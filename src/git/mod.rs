//! Git (M5): status of the project's repository, changed-line gutter marks, the Commit panel and
//! the diff view. Everything runs the `git` CLI on short-lived threads; [`Git::poll`] drains the
//! results every loop turn. Never pushes, never forces anything.

pub mod blame;
pub mod branches;
pub mod diff;
pub mod graph;
pub mod log;
pub mod patch;
pub mod review;
pub mod status;
pub mod ui;
pub mod view;
pub mod view_ui;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ropey::Rope;

use crate::doc::Doc;
use diff::{DiffLine, LineKind, Mark};
use status::{Entry, Kind};

/// Re-read `git status` this often (and right after a commit).
const STATUS_EVERY: Duration = Duration::from_secs(2);
/// Outside a repository, look again this rarely (someone may run `git init`).
const NO_REPO_EVERY: Duration = Duration::from_secs(10);

/// Runs `git -C dir args…`; stdout on success, stderr as the error.
pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    run_with(dir, args, &[], None)
}

/// [`run`] with extra environment variables and (optionally) bytes on stdin.
pub fn run_with(dir: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)], stdin: Option<&[u8]>) -> Result<String, String> {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        // Status shouldn't take locks another git process (or the user) is waiting for.
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Never ask for a password or passphrase over the editor (push/pull/fetch).
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(data), Some(mut input)) = (stdin, child.stdin.take()) {
        // A thread, so a big input can't deadlock against a full stdout pipe.
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = input.write_all(&data);
        });
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(if err.is_empty() { format!("git {} failed", args.first().unwrap_or(&"")) } else { err.to_string() })
    }
}

/// `git apply <args> -` with `patch` on stdin (`--cached` stages, `-R` reverses).
pub fn apply(root: &Path, patch: &str, args: &[&str]) -> Result<(), String> {
    let mut a = vec!["apply", "--whitespace=nowarn"];
    a.extend_from_slice(args);
    a.push("-");
    run_with(root, &a, &[], Some(patch.as_bytes())).map(|_| ())
}

/// A tree object of the working tree as it is now (tracked + untracked, minus ignored files),
/// made with a throwaway copy of the index: the user's index, stash and refs are untouched.
/// Its objects are unreferenced, so `git gc` may prune them after its grace period (2 weeks).
pub fn snapshot(root: &Path) -> Result<String, String> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tmp = std::env::temp_dir().join(format!("catlas-snap-{}-{}.index", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    // Starting from a copy keeps git's stat cache: only changed files are hashed again.
    let index = run(root, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
    if std::fs::copy(index.trim(), &tmp).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    let env = [("GIT_INDEX_FILE", tmp.as_os_str())];
    let r = run_with(root, &["add", "-A", "--", "."], &env, None).and_then(|_| run_with(root, &["write-tree"], &env, None));
    let _ = std::fs::remove_file(&tmp);
    r.map(|t| t.trim().to_string())
}

/// The repository root (canonical) for a folder inside it.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    run(dir, &["rev-parse", "--show-toplevel"]).ok().map(|t| crate::project::canonical(Path::new(t.trim())))
}

pub(crate) fn job<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

struct StatusResult {
    root: Option<PathBuf>,
    branch: Option<String>,
    head: Option<String>,
    entries: Vec<Entry>,
}

fn read_status(project: &Path) -> StatusResult {
    let none = StatusResult { root: None, branch: None, head: None, entries: Vec::new() };
    let Ok(top) = run(project, &["rev-parse", "--show-toplevel"]) else { return none };
    let root = crate::project::canonical(Path::new(top.trim()));
    let Ok(out) = run(&root, &["status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all"]) else {
        return StatusResult { root: Some(root), ..none };
    };
    let st = status::parse(&out);
    let head = run(&root, &["rev-parse", "HEAD"]).ok().map(|s| s.trim().to_string());
    let branch = st.branch.or_else(|| head.as_ref().map(|h| format!("{}…", &h[..h.len().min(8)])));
    StatusResult { root: Some(root), branch, head, entries: st.entries }
}

/// The file at HEAD, or None (not in HEAD, binary, no commits).
fn read_head(root: &Path, rel: &str) -> Option<Arc<str>> {
    let text = run(root, &["show", &format!("HEAD:{rel}")]).ok()?;
    (!text.contains('\0')).then(|| Arc::from(text))
}

/// Gutter state of one open file.
#[derive(Default)]
struct Gutter {
    marks: Vec<Option<Mark>>,
    /// The text the marks (or the running job) were computed from.
    snapshot: Option<Rope>,
    job: Option<Receiver<Vec<Option<Mark>>>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    List,
    Message,
}

/// The Commit panel's own state.
pub struct Panel {
    pub cursor: usize,
    pub scroll: usize,
    pub checked: HashSet<String>,
    /// Paths seen before, so new changes get their default tick only once.
    known: HashSet<String>,
    pub message: String,
    pub field: Field,
}

/// The unified diff of one changed file (Enter in the Commit panel).
pub struct DiffView {
    pub path: String,
    pub kind: Kind,
    pub rows: Vec<DiffLine>,
    pub added: usize,
    pub deleted: usize,
    pub scroll: usize,
    /// Rows from the last draw (paging).
    pub view_h: usize,
    pub note: Option<String>,
}

impl DiffView {
    pub fn hunks(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows.iter().enumerate().filter(|(_, r)| r.kind == LineKind::Hunk).map(|(i, _)| i)
    }

    /// The file line at the top of the view (for F4: jump to the source).
    pub fn line_at_top(&self) -> usize {
        self.rows[self.scroll.min(self.rows.len().saturating_sub(1))..]
            .iter()
            .find_map(|r| r.new)
            .unwrap_or(1)
    }
}

pub struct Git {
    project: PathBuf,
    /// Repository root (canonical); None outside a repository.
    pub root: Option<PathBuf>,
    pub branch: Option<String>,
    head: Option<String>,
    pub entries: Vec<Entry>,
    /// Absolute path → kind, for the tree colours; folders holding changes.
    by_path: HashMap<PathBuf, Kind>,
    dirs: HashSet<PathBuf>,
    status_job: Option<Receiver<StatusResult>>,
    last_status: Option<Instant>,
    force_status: bool,

    /// HEAD text per absolute path (None = not in HEAD).
    bases: HashMap<PathBuf, Option<Arc<str>>>,
    base_jobs: HashMap<PathBuf, Receiver<Option<Arc<str>>>>,
    /// Keyed by the doc's path as given (what `ui` looks up while drawing).
    gutters: HashMap<PathBuf, Gutter>,
    canon: HashMap<PathBuf, PathBuf>,

    pub panel: Panel,
    pub diff: Option<DiffView>,
    diff_job: Option<Receiver<DiffView>>,
    commit_job: Option<Receiver<Result<String, String>>>,
    changed: bool,
}

impl Git {
    pub fn new(project: &Path) -> Self {
        Git {
            project: project.to_path_buf(),
            root: None,
            branch: None,
            head: None,
            entries: Vec::new(),
            by_path: HashMap::new(),
            dirs: HashSet::new(),
            status_job: None,
            last_status: None,
            force_status: true,
            bases: HashMap::new(),
            base_jobs: HashMap::new(),
            gutters: HashMap::new(),
            canon: HashMap::new(),
            panel: Panel {
                cursor: 0,
                scroll: 0,
                checked: HashSet::new(),
                known: HashSet::new(),
                message: String::new(),
                field: Field::List,
            },
            diff: None,
            diff_job: None,
            commit_job: None,
            changed: false,
        }
    }

    /// Whether a job delivered something since the last call (the screen needs a redraw).
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn committing(&self) -> bool {
        self.commit_job.is_some()
    }

    pub fn refresh(&mut self) {
        self.force_status = true;
    }

    /// Gutter marks for a doc path (as stored in `Doc::path`), indexed by line.
    pub fn marks(&self, path: Option<&Path>) -> &[Option<Mark>] {
        path.and_then(|p| self.gutters.get(p)).map(|g| g.marks.as_slice()).unwrap_or(&[])
    }

    /// Git status of a tree row (absolute path). Folders report Modified when they hold changes.
    pub fn kind_of(&self, path: &Path, is_dir: bool) -> Option<Kind> {
        if is_dir {
            return self.dirs.contains(path).then_some(Kind::Modified);
        }
        self.by_path.get(path).copied()
    }

    fn rel(&self, abs: &Path) -> Option<String> {
        let r = abs.strip_prefix(self.root.as_ref()?).ok()?;
        Some(r.to_string_lossy().replace('\\', "/"))
    }

    /// Drains finished jobs and starts due ones. `docs[active]` gets its gutter refreshed.
    /// Returns a message for the status bar (commit results).
    pub fn poll(&mut self, docs: &[Doc], active: usize) -> Option<String> {
        let mut msg = None;
        if let Some(rx) = &self.status_job {
            if let Ok(r) = rx.try_recv() {
                self.status_job = None;
                self.changed = true;
                self.apply_status(r);
            }
        }
        let every = if self.root.is_some() { STATUS_EVERY } else { NO_REPO_EVERY };
        let due = self.force_status || self.last_status.is_none_or(|t| t.elapsed() >= every);
        if self.status_job.is_none() && self.commit_job.is_none() && due {
            self.force_status = false;
            self.last_status = Some(Instant::now());
            let project = self.project.clone();
            self.status_job = Some(job(move || read_status(&project)));
        }

        if let Some(rx) = &self.commit_job {
            if let Ok(r) = rx.try_recv() {
                self.commit_job = None;
                self.force_status = true;
                msg = Some(match r {
                    Ok(m) => {
                        self.panel.message.clear();
                        self.panel.field = Field::List;
                        m
                    }
                    Err(e) => format!("Couldn't commit: {e}"),
                });
            }
        }
        if let Some(rx) = &self.diff_job {
            if let Ok(v) = rx.try_recv() {
                self.diff_job = None;
                self.diff = Some(v);
                self.changed = true;
            }
        }

        let finished: Vec<PathBuf> =
            self.base_jobs.iter().filter_map(|(p, rx)| rx.try_recv().ok().map(|b| (p.clone(), b))).map(|(p, b)| {
                self.bases.insert(p.clone(), b);
                p
            }).collect();
        for p in finished {
            self.base_jobs.remove(&p);
        }

        if let Some(d) = docs.get(active) {
            self.update_gutter(d);
        }
        msg
    }

    fn apply_status(&mut self, r: StatusResult) {
        if r.head != self.head || r.root != self.root {
            // A commit, checkout or reset: HEAD texts are stale.
            self.bases.clear();
            self.base_jobs.clear();
            for g in self.gutters.values_mut() {
                g.snapshot = None;
            }
        }
        self.root = r.root;
        self.branch = r.branch;
        self.head = r.head;
        self.entries = r.entries;
        self.by_path.clear();
        self.dirs.clear();
        if let Some(root) = &self.root {
            for e in &self.entries {
                let abs = root.join(&e.path);
                if e.kind() != Kind::Ignored {
                    let mut dir = abs.parent();
                    while let Some(d) = dir {
                        if !d.starts_with(root) || !self.dirs.insert(d.to_path_buf()) {
                            break;
                        }
                        dir = d.parent();
                    }
                }
                self.by_path.insert(abs, e.kind());
            }
        }
        // Default ticks like IDEA: changed tracked files are in, unversioned files are out.
        let p = &mut self.panel;
        let now: HashSet<String> = self.entries.iter().map(|e| e.path.clone()).collect();
        for e in &self.entries {
            if p.known.insert(e.path.clone()) && e.kind() != Kind::Untracked {
                p.checked.insert(e.path.clone());
            }
        }
        p.known.retain(|k| now.contains(k));
        p.checked.retain(|k| now.contains(k));
        p.cursor = p.cursor.min(self.entries.len().saturating_sub(1));
    }

    fn update_gutter(&mut self, d: &Doc) {
        let Some(path) = d.path.clone() else { return };
        if self.root.is_none() {
            return;
        }
        let abs = self.canon.entry(path.clone()).or_insert_with(|| crate::project::canonical(&path)).clone();
        let Some(rel) = self.rel(&abs) else { return };
        let base = match self.bases.get(&abs) {
            Some(b) => b.clone(),
            None => {
                if !self.base_jobs.contains_key(&abs) {
                    let root = self.root.clone().unwrap();
                    self.base_jobs.insert(abs, job(move || read_head(&root, &rel)));
                }
                return;
            }
        };
        let g = self.gutters.entry(path).or_default();
        if let Some(rx) = &g.job {
            match rx.try_recv() {
                Ok(m) => {
                    g.marks = m;
                    g.job = None;
                    self.changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(_) => g.job = None,
            }
        }
        if g.snapshot.as_ref().is_some_and(|s| *s == d.rope) {
            return;
        }
        let rope = d.rope.clone();
        g.snapshot = Some(rope.clone());
        match base {
            None => {
                self.changed |= !g.marks.is_empty();
                g.marks.clear();
            }
            Some(base) => g.job = Some(job(move || diff::gutter_marks(&base, &rope.to_string()))),
        }
    }

    // ---- Commit panel actions ----------------------------------------------------------------

    pub fn toggle(&mut self, i: usize) {
        if let Some(e) = self.entries.get(i) {
            if !self.panel.checked.remove(&e.path) {
                self.panel.checked.insert(e.path.clone());
            }
        }
    }

    pub fn toggle_all(&mut self) {
        if self.panel.checked.len() == self.entries.len() {
            self.panel.checked.clear();
        } else {
            self.panel.checked = self.entries.iter().map(|e| e.path.clone()).collect();
        }
    }

    /// `git add` the ticked files, then `git commit` only those. Returns an error to show, if any.
    pub fn commit(&mut self) -> Result<(), String> {
        let Some(root) = self.root.clone() else { return Err("Not a git repository".into()) };
        if self.commit_job.is_some() {
            return Err("A commit is already running".into());
        }
        let msg = self.panel.message.trim().to_string();
        if msg.is_empty() {
            return Err("Write a commit message first".into());
        }
        let mut paths: Vec<String> = Vec::new();
        // Ticked files with staged hunks *and* other changes: only their staged part goes in.
        let mut partial: Vec<String> = Vec::new();
        for e in self.entries.iter().filter(|e| self.panel.checked.contains(&e.path)) {
            if e.kind() == Kind::Conflict {
                return Err(format!("{} has conflicts; resolve them first", e.path));
            }
            if e.partial() {
                partial.push(e.path.clone());
                continue;
            }
            paths.push(e.path.clone());
            paths.extend(e.orig.clone());
        }
        if paths.is_empty() && partial.is_empty() {
            return Err("Tick at least one file (Space)".into());
        }
        if !partial.is_empty() {
            // The commit then takes the index as it is, so nothing unticked may be staged.
            if let Some(e) = self.entries.iter().find(|e| e.staged() && !self.panel.checked.contains(&e.path)) {
                return Err(format!("{} has staged changes but isn't ticked: tick it, or unstage it in the review (Ctrl+L)", e.path));
            }
        }
        let n = self.panel.checked.len();
        self.commit_job = Some(job(move || commit(&root, &paths, &partial, &msg, n)));
        Ok(())
    }

    /// Opens the diff view for the entry at `i` (in the background).
    pub fn open_diff(&mut self, i: usize) {
        let (Some(root), Some(e)) = (self.root.clone(), self.entries.get(i).cloned()) else { return };
        self.diff_job = Some(job(move || build_diff(&root, &e)));
    }

    pub fn step_diff(&mut self, delta: isize) {
        let Some(d) = &self.diff else { return };
        let Some(i) = self.entries.iter().position(|e| e.path == d.path) else { return };
        let j = i as isize + delta;
        if j >= 0 && (j as usize) < self.entries.len() {
            self.panel.cursor = j as usize;
            self.open_diff(j as usize);
        }
    }
}

/// `git add` the whole-file paths, then commit. With `partial` files (staged hunks plus other
/// changes) the commit is the index as it stands (only their staged hunks); otherwise only
/// `paths` are committed, whatever else is staged.
fn commit(root: &Path, paths: &[String], partial: &[String], msg: &str, n: usize) -> Result<String, String> {
    if !paths.is_empty() {
        let mut add = vec!["add", "-A", "--"];
        add.extend(paths.iter().map(String::as_str));
        run(root, &add)?;
    }
    let mut ci = vec!["commit", "-q", "-m", msg];
    if partial.is_empty() {
        ci.push("--");
        ci.extend(paths.iter().map(String::as_str));
    }
    run(root, &ci)?;
    let sha = run(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let subject = msg.lines().next().unwrap_or("");
    let note = match partial.len() {
        0 => String::new(),
        1 => format!(" (staged hunks of {})", partial[0]),
        k => format!(" (staged hunks of {k} files)"),
    };
    Ok(format!("Committed {n} file(s) as {}: {subject}{note}", sha.trim()))
}

fn build_diff(root: &Path, e: &Entry) -> DiffView {
    let kind = e.kind();
    let head_path = e.orig.as_deref().unwrap_or(&e.path);
    let old = match kind {
        Kind::Untracked | Kind::Added => String::new(),
        _ => run(root, &["show", &format!("HEAD:{head_path}")]).unwrap_or_default(),
    };
    let new = match kind {
        Kind::Deleted => String::new(),
        _ => std::fs::read(root.join(&e.path)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default(),
    };
    let mut v = DiffView { path: e.path.clone(), kind, rows: Vec::new(), added: 0, deleted: 0, scroll: 0, view_h: 10, note: None };
    if old.contains('\0') || new.contains('\0') {
        v.note = Some("Binary file: no text diff".into());
        return v;
    }
    let (rows, added, deleted) = diff::unified(&old, &new, 3);
    if rows.is_empty() {
        v.note = Some(if kind == Kind::Conflict { "Conflicted file: open it to resolve" } else { "No text changes (mode or whitespace at the end only)" }.into());
    }
    if let Some(o) = &e.orig {
        v.note = Some(format!("Renamed from {o}"));
    }
    v.rows = rows;
    v.added = added;
    v.deleted = deleted;
    v
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A scratch repository under the temp dir with one commit (a.txt, b.txt).
    pub fn scratch_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("catlas-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = crate::project::canonical(&dir);
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "Test"], &["config", "user.email", "t@example.com"], &["config", "commit.gpgsign", "false"]] {
            run(&dir, args).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.join("b.txt"), "bee\n").unwrap();
        run(&dir, &["add", "."]).unwrap();
        run(&dir, &["commit", "-q", "-m", "first"]).unwrap();
        dir
    }

    fn wait(g: &mut Git, docs: &[Doc], until: impl Fn(&Git) -> bool) -> Option<String> {
        let t = Instant::now();
        let mut msg = None;
        while t.elapsed() < Duration::from_secs(10) {
            if let Some(m) = g.poll(docs, 0) {
                msg = Some(m);
            }
            if until(g) {
                return msg;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for git");
    }

    #[test]
    fn status_gutter_and_commit_in_a_scratch_repo() {
        let dir = scratch_repo("flow");
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();
        let docs = vec![Doc::open(&dir.join("a.txt")).unwrap()];
        let mut g = Git::new(&dir);
        wait(&mut g, &docs, |g| g.entries.len() == 2);
        assert_eq!(g.branch.as_deref(), Some("main"));
        assert_eq!(g.kind_of(&dir.join("a.txt"), false), Some(Kind::Modified));
        assert_eq!(g.kind_of(&dir.join("new.txt"), false), Some(Kind::Untracked));
        // Tracked changes are ticked by default, unversioned ones aren't.
        assert!(g.panel.checked.contains("a.txt") && !g.panel.checked.contains("new.txt"));

        wait(&mut g, &docs, |g| !g.marks(docs[0].path.as_deref()).is_empty());
        let m = g.marks(docs[0].path.as_deref()).to_vec();
        assert_eq!(&m[..4], &[None, Some(Mark::Modified), None, Some(Mark::Added)]);

        g.open_diff(0);
        wait(&mut g, &docs, |g| g.diff.is_some());
        let v = g.diff.as_ref().unwrap();
        assert_eq!((v.path.as_str(), v.added, v.deleted), ("a.txt", 2, 1));

        assert!(g.commit().is_err(), "no message yet");
        g.panel.message = "Second commit\n\nbody".into();
        g.commit().unwrap();
        let msg = wait(&mut g, &docs, |g| !g.committing() && g.entries.len() == 1).unwrap();
        assert!(msg.starts_with("Committed 1 file(s)"), "{msg}");
        let log = run(&dir, &["log", "--format=%s", "-n", "2"]).unwrap();
        assert_eq!(log, "Second commit\nfirst\n");
        // new.txt stayed out of the commit.
        assert_eq!(g.entries[0].path, "new.txt");
        // HEAD moved: the gutter is clean again.
        wait(&mut g, &docs, |g| g.marks(docs[0].path.as_deref()).iter().all(Option::is_none));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn outside_a_repo_nothing_happens() {
        let dir = std::env::temp_dir().join(format!("catlas-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut g = Git::new(&dir);
        let docs = vec![Doc::empty()];
        g.poll(&docs, 0);
        let t = Instant::now();
        while g.status_job.is_some() && t.elapsed() < Duration::from_secs(5) {
            g.poll(&docs, 0);
            std::thread::sleep(Duration::from_millis(10));
        }
        // temp dirs are not inside a repository on our machines; if one is, skip.
        if g.root.is_none() {
            assert!(g.branch.is_none() && g.entries.is_empty());
            assert!(g.commit().is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
