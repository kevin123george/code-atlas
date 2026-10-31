//! Git status of every root, read in the background (a few threads, one `git status` + one
//! `git log -1` per repo; `git diff HEAD --numstat` too while the Repos popup shows the +/−).
//! Feeds the tree badges (branch, changed files, ahead/behind), the tree colours of the repos
//! that aren't the current one, and the Repos popup. Never takes locks (`GIT_OPTIONAL_LOCKS=0`
//! comes with `git::run`), never touches a remote.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::git::run;
use crate::git::status::{self, Kind};

/// Re-read every root this often (the current repo also has the dock's own 2 s status).
pub const EVERY: Duration = Duration::from_secs(10);
/// Parallel git processes at most.
const THREADS: usize = 4;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoState {
    /// Repository root (canonical); None = not a git repository.
    pub top: Option<PathBuf>,
    /// Branch, or the short commit when detached.
    pub branch: Option<String>,
    pub upstream: bool,
    pub ahead: usize,
    pub behind: usize,
    /// Changed paths (untracked included, ignored not).
    pub changed: usize,
    pub untracked: usize,
    pub conflicts: usize,
    /// Lines added / deleted vs HEAD (tracked files), when read with detail.
    pub stat: Option<(usize, usize)>,
    /// Last commit: short hash, subject, commit time (unix).
    pub last: Option<(String, String, i64)>,
    pub remotes: usize,
    /// Absolute path → kind, and the folders holding changes (tree colours).
    pub kinds: HashMap<PathBuf, Kind>,
    pub dirs: HashSet<PathBuf>,
    pub error: Option<String>,
}

impl RepoState {
    pub fn is_repo(&self) -> bool {
        self.top.is_some()
    }

    pub fn dirty(&self) -> bool {
        self.changed > 0
    }

    /// Git status of a tree row of this repo (as `Git::kind_of`).
    pub fn kind_of(&self, path: &Path, is_dir: bool) -> Option<Kind> {
        if is_dir {
            return self.dirs.contains(path).then_some(Kind::Modified);
        }
        self.kinds.get(path).copied()
    }
}

/// `main...origin/main [ahead 1, behind 2]` → (upstream?, ahead, behind).
pub fn parse_track(head: &str) -> (bool, usize, usize) {
    let upstream = head.contains("...");
    let (mut ahead, mut behind) = (0, 0);
    if let Some(i) = head.find('[') {
        let inner = head[i + 1..].trim_end_matches(']');
        for part in inner.split(',') {
            let mut it = part.split_whitespace();
            match (it.next(), it.next().and_then(|n| n.parse().ok())) {
                (Some("ahead"), Some(n)) => ahead = n,
                (Some("behind"), Some(n)) => behind = n,
                _ => {}
            }
        }
    }
    (upstream, ahead, behind)
}

/// Sums `git diff --numstat` output (binary files count as 0).
pub fn sum_numstat(out: &str) -> (usize, usize) {
    out.lines().fold((0, 0), |(a, d), l| {
        let mut it = l.split('\t');
        let x = it.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        let y = it.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        (a + x, d + y)
    })
}

/// Reads one root (blocking; runs on a worker thread).
pub fn read(dir: &Path, detail: bool) -> RepoState {
    let mut st = RepoState::default();
    let top = match run(dir, &["rev-parse", "--show-toplevel"]) {
        Ok(t) => crate::project::canonical(Path::new(t.trim())),
        Err(_) => return st,
    };
    st.top = Some(top.clone());
    match run(&top, &["status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all"]) {
        Ok(out) => {
            if let Some(head) = out.split('\0').next().and_then(|f| f.strip_prefix("## ")) {
                (st.upstream, st.ahead, st.behind) = parse_track(head);
            }
            let parsed = status::parse(&out);
            st.branch = parsed.branch;
            for e in parsed.entries.iter().filter(|e| e.kind() != Kind::Ignored) {
                st.changed += 1;
                match e.kind() {
                    Kind::Untracked => st.untracked += 1,
                    Kind::Conflict => st.conflicts += 1,
                    _ => {}
                }
                let abs = top.join(&e.path);
                let mut d = abs.parent();
                while let Some(x) = d {
                    if !x.starts_with(&top) || !st.dirs.insert(x.to_path_buf()) {
                        break;
                    }
                    d = x.parent();
                }
                st.kinds.insert(abs, e.kind());
            }
        }
        Err(e) => st.error = Some(e),
    }
    if let Ok(out) = run(&top, &["log", "-1", "--format=%h%x1f%s%x1f%ct"]) {
        let mut it = out.trim_end().splitn(3, '\x1f');
        if let (Some(h), Some(s), Some(t)) = (it.next(), it.next(), it.next()) {
            st.last = Some((h.to_string(), s.to_string(), t.trim().parse().unwrap_or(0)));
        }
    }
    if st.branch.is_none() {
        st.branch = st.last.as_ref().map(|(h, _, _)| format!("{h}…"));
    }
    st.remotes = run(&top, &["remote"]).map(|o| o.lines().filter(|l| !l.is_empty()).count()).unwrap_or(0);
    if detail && st.last.is_some() {
        st.stat = run(&top, &["diff", "HEAD", "--numstat"]).ok().map(|o| sum_numstat(&o));
    }
    st
}

/// The status of every root, refreshed in the background.
#[derive(Default)]
pub struct Statuses {
    /// Per root folder.
    pub repos: HashMap<PathBuf, RepoState>,
    job: Option<Receiver<(PathBuf, RepoState)>>,
    /// Results still to come from the running job.
    pending: Arc<AtomicUsize>,
    last: Option<Instant>,
    due: bool,
}

impl Statuses {
    pub fn get(&self, root: &Path) -> Option<&RepoState> {
        self.repos.get(root)
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Read everything again at the next poll.
    pub fn refresh(&mut self) {
        self.due = true;
    }

    /// Drains results and starts a new round when due. Returns true when something arrived.
    pub fn poll(&mut self, roots: &[PathBuf], detail: bool) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.job {
            while let Ok((root, st)) = rx.try_recv() {
                if self.repos.get(&root) != Some(&st) {
                    self.repos.insert(root, st);
                    changed = true;
                }
            }
            if self.pending.load(Ordering::Relaxed) == 0 {
                while let Ok((root, st)) = rx.try_recv() {
                    self.repos.insert(root, st);
                    changed = true;
                }
                self.job = None;
            }
        }
        let due = self.due || self.last.is_none_or(|t| t.elapsed() >= EVERY);
        if self.job.is_none() && due && !roots.is_empty() {
            self.due = false;
            self.last = Some(Instant::now());
            self.repos.retain(|k, _| roots.contains(k));
            self.start(roots.to_vec(), detail);
        }
        changed
    }

    fn start(&mut self, roots: Vec<PathBuf>, detail: bool) {
        let (tx, rx) = channel();
        let pending = Arc::new(AtomicUsize::new(roots.len()));
        let next = Arc::new(AtomicUsize::new(0));
        let roots = Arc::new(roots);
        for _ in 0..THREADS.min(roots.len()) {
            let (tx, pending, next, roots) = (tx.clone(), pending.clone(), next.clone(), roots.clone());
            std::thread::spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(root) = roots.get(i) else { break };
                let st = read(root, detail);
                let gone = tx.send((root.clone(), st)).is_err();
                pending.fetch_sub(1, Ordering::Relaxed);
                if gone {
                    break;
                }
            });
        }
        self.pending = pending;
        self.job = Some(rx);
    }

    /// Waits for the running round (tests).
    #[cfg(test)]
    pub fn settle(&mut self, roots: &[PathBuf]) {
        let t = Instant::now();
        while self.job.is_some() || self.repos.len() < roots.len() {
            self.poll(roots, true);
            assert!(t.elapsed() < Duration::from_secs(10), "status never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
