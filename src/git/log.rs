//! The Log tab's data: `git log` of all branches (or one file's history) with a commit graph,
//! refs, relative dates and a mark on commits Claude co-authored; a text filter.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use super::graph::{self, Cell};
use super::{job, run};

/// Commits loaded at most (the graph needs them in one go).
pub const LIMIT: usize = 1500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    /// Unix seconds (author date).
    pub time: i64,
    /// Decorations: `HEAD -> main`, `origin/main`, `tag: v1`.
    pub refs: Vec<String>,
    pub subject: String,
    /// A `Co-Authored-By: Claude …` trailer (or Claude as the author).
    pub claude: bool,
}

impl Commit {
    pub fn short(&self) -> &str {
        self.sha.get(..7).unwrap_or(&self.sha)
    }
}

const FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1f%(trailers:key=Co-authored-by,valueonly,separator=%x1d)%x1e";

/// Whether a commit's co-author trailers (or author) name Claude.
pub fn is_claude(trailers: &str, author: &str, email: &str) -> bool {
    let t = trailers.to_ascii_lowercase();
    t.split('\x1d').any(|v| {
        let v = v.trim();
        v.starts_with("claude") || v.contains("noreply@anthropic.com")
    }) || author.to_ascii_lowercase().starts_with("claude")
        || email.eq_ignore_ascii_case("noreply@anthropic.com")
}

/// Parses `git log` output in [`FORMAT`].
pub fn parse(out: &str) -> Vec<Commit> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            if rec.is_empty() {
                return None;
            }
            let f: Vec<&str> = rec.split('\x1f').collect();
            if f.len() < 7 {
                return None;
            }
            let refs = f[5].split(", ").filter(|r| !r.is_empty()).map(str::to_string).collect();
            let trailers = f.get(7).copied().unwrap_or("");
            Some(Commit {
                sha: f[0].to_string(),
                parents: f[1].split_whitespace().map(str::to_string).collect(),
                author: f[2].to_string(),
                email: f[3].to_string(),
                time: f[4].trim().parse().unwrap_or(0),
                refs,
                subject: f[6].to_string(),
                claude: is_claude(trailers, f[2], f[3]),
            })
        })
        .collect()
}

/// "now", "5m", "3h", "2d", "3w", "4mo", "2y".
pub fn age(now: i64, t: i64) -> String {
    let s = (now - t).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=1_209_599 => format!("{}d", s / 86_400),
        1_209_600..=5_183_999 => format!("{}w", s / 604_800),
        5_184_000..=31_535_999 => format!("{}mo", s / 2_592_000),
        _ => format!("{}y", s / 31_536_000),
    }
}

pub fn now() -> i64 {
    super::super::agents::run::now_secs() as i64
}

/// The parts of a filter: words (all must match subject, hash, author or refs), `author:x`,
/// `path:p` (history of that path; needs a reload).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub words: Vec<String>,
    pub author: Option<String>,
    pub path: Option<String>,
}

pub fn parse_filter(s: &str) -> Filter {
    let mut f = Filter::default();
    for w in s.split_whitespace() {
        if let Some(a) = w.strip_prefix("author:").or_else(|| w.strip_prefix('@')) {
            f.author = Some(a.to_lowercase());
        } else if let Some(p) = w.strip_prefix("path:") {
            f.path = Some(p.to_string());
        } else {
            f.words.push(w.to_lowercase());
        }
    }
    f
}

impl Filter {
    pub fn matches(&self, c: &Commit) -> bool {
        if let Some(a) = &self.author {
            if !c.author.to_lowercase().contains(a) && !c.email.to_lowercase().contains(a) && !(a == "claude" && c.claude) {
                return false;
            }
        }
        self.words.iter().all(|w| {
            c.subject.to_lowercase().contains(w) || c.sha.starts_with(w.as_str()) || c.author.to_lowercase().contains(w) || c.refs.iter().any(|r| r.to_lowercase().contains(w))
        })
    }
}

struct Loaded {
    path: Option<String>,
    result: Result<Vec<Commit>, String>,
}

pub struct Log {
    pub commits: Vec<Commit>,
    /// Graph cells per commit (empty for a file's history: no graph there).
    pub graph: Vec<Vec<Cell>>,
    /// Indices of the commits the filter lets through.
    pub shown: Vec<usize>,
    /// Selected row in `shown`, first row drawn.
    pub sel: usize,
    pub scroll: usize,
    pub filter: String,
    /// The path whose history is loaded (None = all branches).
    pub path: Option<String>,
    pub error: Option<String>,
    pub loading: bool,
    pub loaded: bool,
    job: Option<Receiver<Loaded>>,
    /// Rows from the last draw (paging).
    pub view_h: usize,
}

impl Default for Log {
    fn default() -> Self {
        Log { commits: Vec::new(), graph: Vec::new(), shown: Vec::new(), sel: 0, scroll: 0, filter: String::new(), path: None, error: None, loading: false, loaded: false, job: None, view_h: 10 }
    }
}

fn load(root: &Path, path: Option<&str>) -> Result<Vec<Commit>, String> {
    let limit = format!("-n{LIMIT}");
    let mut args = vec!["-c", "core.quotePath=false", "log", "--date-order", &limit, FORMAT];
    match path {
        // One file: follow renames, no graph (parents would be rewritten anyway).
        Some(p) => args.extend(["--follow", "--", p]),
        None => args.extend(["--branches", "--tags", "--remotes", "HEAD"]),
    }
    match run(root, &args) {
        Ok(out) => Ok(parse(&out)),
        Err(e) if e.contains("does not have any commits") || e.contains("bad default revision") || e.contains("unknown revision") => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

impl Log {
    /// (Re)loads in the background; `path` = a file's history.
    pub fn reload(&mut self, root: &Path, path: Option<String>) {
        let root: PathBuf = root.to_path_buf();
        self.loading = true;
        self.job = Some(job(move || {
            let result = load(&root, path.as_deref());
            Loaded { path, result }
        }));
    }

    /// Takes a finished load. Returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.job else { return false };
        let Ok(l) = rx.try_recv() else { return false };
        self.job = None;
        self.loading = false;
        self.loaded = true;
        let keep = self.selected().map(|c| c.sha.clone());
        self.path = l.path;
        match l.result {
            Ok(cs) => {
                self.error = None;
                self.graph = if self.path.is_none() {
                    graph::layout(&cs.iter().map(|c| (c.sha.clone(), c.parents.clone())).collect::<Vec<_>>())
                } else {
                    Vec::new()
                };
                self.commits = cs;
            }
            Err(e) => {
                self.error = Some(e);
                self.commits.clear();
                self.graph.clear();
            }
        }
        self.apply_filter();
        if let Some(sha) = keep {
            self.select_sha(&sha);
        }
        true
    }

    /// Re-runs the text filter over the loaded commits.
    pub fn apply_filter(&mut self) {
        let f = parse_filter(&self.filter);
        self.shown = (0..self.commits.len()).filter(|&i| f.matches(&self.commits[i])).collect();
        self.sel = self.sel.min(self.shown.len().saturating_sub(1));
    }

    /// The graph is drawn only for the unfiltered list of all branches.
    pub fn show_graph(&self) -> bool {
        self.path.is_none() && self.shown.len() == self.commits.len() && !self.graph.is_empty()
    }

    pub fn selected(&self) -> Option<&Commit> {
        self.shown.get(self.sel).map(|&i| &self.commits[i])
    }

    pub fn select_sha(&mut self, sha: &str) -> bool {
        match self.shown.iter().position(|&i| self.commits[i].sha.starts_with(sha)) {
            Some(p) => {
                self.sel = p;
                true
            }
            None => false,
        }
    }

    pub fn step(&mut self, d: isize) {
        if !self.shown.is_empty() {
            self.sel = (self.sel as isize + d).clamp(0, self.shown.len() as isize - 1) as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records_refs_and_claude_trailers() {
        let out = "\naaaaaaaaaa\x1fbbbbbbbbbb cccccccccc\x1fKevin\x1fk@example.com\x1f1700000000\x1fHEAD -> main, origin/main, tag: v1\x1fMerge it\x1fClaude Opus 5.5 <noreply@anthropic.com>\x1e\nbbbbbbbbbb\x1f\x1fKevin\x1fk@example.com\x1f1690000000\x1f\x1fFirst\x1f\x1e\n";
        let cs = parse(out);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].parents.len(), 2);
        assert_eq!(cs[0].refs, vec!["HEAD -> main", "origin/main", "tag: v1"]);
        assert!(cs[0].claude && !cs[1].claude);
        assert_eq!(cs[0].short(), "aaaaaaa");
        assert!(cs[1].parents.is_empty());
        assert!(is_claude("Someone <a@b>\x1dclaude <x@y>", "Kevin", "k@x"));
        assert!(!is_claude("Someone <a@b>", "Kevin", "k@x"));
    }

    #[test]
    fn ages() {
        let n = 2_000_000_000;
        assert_eq!(age(n, n - 10), "now");
        assert_eq!(age(n, n - 300), "5m");
        assert_eq!(age(n, n - 7200), "2h");
        assert_eq!(age(n, n - 3 * 86_400), "3d");
        assert_eq!(age(n, n - 21 * 86_400), "3w");
        assert_eq!(age(n, n - 90 * 86_400), "3mo");
        assert_eq!(age(n, n - 800 * 86_400), "2y");
    }

    #[test]
    fn filters() {
        let c = |subject: &str, author: &str, claude: bool| Commit { sha: "abc123".into(), parents: vec![], author: author.into(), email: String::new(), time: 0, refs: vec!["main".into()], subject: subject.into(), claude };
        let f = parse_filter("Parser author:kev path:src/a.rs");
        assert_eq!(f, Filter { words: vec!["parser".into()], author: Some("kev".into()), path: Some("src/a.rs".into()) });
        assert!(f.matches(&c("Fix the parser", "Kevin", false)));
        assert!(!f.matches(&c("Fix the lexer", "Kevin", false)));
        assert!(parse_filter("@claude").matches(&c("x", "Kevin", true)));
        assert!(parse_filter("abc1").matches(&c("x", "Kevin", false)));
    }

    #[test]
    fn loads_a_real_history_with_a_graph() {
        let dir = crate::git::tests::scratch_repo("log");
        run(&dir, &["checkout", "-qb", "side"]).unwrap();
        std::fs::write(dir.join("b.txt"), "side\n").unwrap();
        run(&dir, &["commit", "-qam", "on side"]).unwrap();
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        std::fs::write(dir.join("a.txt"), "main\n").unwrap();
        run(&dir, &["commit", "-qam", "on main\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"]).unwrap();
        run(&dir, &["merge", "-q", "--no-edit", "side"]).unwrap();
        let mut log = Log::default();
        log.reload(&dir, None);
        let t = std::time::Instant::now();
        while !log.poll() {
            assert!(t.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let subjects: Vec<&str> = log.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects.len(), 4, "{subjects:?}");
        assert!(subjects[0].starts_with("Merge branch 'side'"));
        let rows: Vec<String> = log.graph.iter().map(|r| r.iter().map(|c| c.ch).collect()).collect();
        assert_eq!(rows[0], "●─╮");
        assert_eq!(rows.last().unwrap(), "●─╯");
        assert!(log.commits.iter().find(|c| c.subject == "on main").unwrap().claude);
        assert!(log.show_graph());
        log.filter = "side".into();
        log.apply_filter();
        assert!(!log.show_graph());
        assert!(log.shown.len() >= 2);
        // One file's history.
        log.reload(&dir, Some("b.txt".into()));
        while !log.poll() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        log.filter.clear();
        log.apply_filter();
        assert_eq!(log.commits.len(), 2, "first + on side");
        assert!(log.graph.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
