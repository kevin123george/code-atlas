//! The review: every change as one scrollable document (file headers, hunks, lines), for the
//! working tree (unstaged + staged, refreshed by itself while an agent edits), for an agent run
//! (everything since the run's snapshot) or for one commit (Log → Enter).
//!
//! The model is built on a background thread ([`build`]): `git diff` parsed into files and
//! hunks, untracked files as all-added, word-level ranges for changed line pairs and syntax
//! tokens for the shown lines. A fingerprint (the raw diff output, or the snapshot tree) makes
//! an unchanged refresh cost one `git diff` and nothing else.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ropey::Rope;

use super::patch::{self, FileDiff, LineKind, Status, DIFF_ARGS};
use super::{job, run};
use crate::syntax::{Syntax, Token};

/// Look for changes this often (working tree; an agent run's snapshot every other time).
pub const REFRESH_EVERY: Duration = Duration::from_millis(1000);
/// Untracked files bigger than this are listed, not read.
const MAX_UNTRACKED_BYTES: u64 = 512 * 1024;
/// Untracked files read at most per refresh.
const MAX_UNTRACKED_READ: usize = 200;
/// Files that get syntax colours per refresh, and the biggest text highlighted.
const MAX_HIGHLIGHT_FILES: usize = 40;
const MAX_HIGHLIGHT_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    /// Working tree vs index (unstaged) and index vs HEAD (staged).
    Worktree,
    /// Everything since an agent run started: its snapshot tree vs a snapshot taken now.
    Run { id: u64, title: String, base: String, since: u64, touched: Vec<String>, only_touched: bool },
    /// One commit vs its first parent.
    Commit { sha: String },
}

impl Scope {
    pub fn read_only(&self) -> bool {
        matches!(self, Scope::Commit { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// Working tree vs index.
    Unstaged,
    /// Index vs HEAD.
    Staged,
    /// A run's or a commit's range.
    Range,
}

/// One file of the model with what drawing needs besides the diff.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub diff: FileDiff,
    pub section: Section,
    /// Word ranges per hunk per line (char offsets; empty = whole line or none).
    pub words: Vec<Vec<Vec<(usize, usize)>>>,
    /// Syntax tokens per line number (1-based), old and new side.
    pub old_tok: HashMap<usize, Vec<Option<Token>>>,
    pub new_tok: HashMap<usize, Vec<Option<Token>>>,
    /// Shown instead of hunks (binary, too big, mode change).
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub files: Vec<FileEntry>,
    /// Commit details: the message block above the files.
    pub header: Vec<String>,
    pub fingerprint: String,
    pub error: Option<String>,
}

/// One row of the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// Line `i` of the commit message block.
    Text(usize),
    /// "CHANGES · 3 files" / "STAGED · 1 file" (only when both exist).
    Section(Section),
    File(usize),
    Note(usize),
    Hunk(usize, usize),
    /// A line pair: `a` the old/left line, `b` the new/right line (indices into the hunk's
    /// lines). Unified rows have one of them, or both pointing at the same context line.
    Line { f: usize, h: usize, a: Option<usize>, b: Option<usize> },
    Blank,
}

impl Row {
    pub fn file(&self) -> Option<usize> {
        match *self {
            Row::File(f) | Row::Note(f) | Row::Hunk(f, _) | Row::Line { f, .. } => Some(f),
            _ => None,
        }
    }

    pub fn hunk(&self) -> Option<(usize, usize)> {
        match *self {
            Row::Hunk(f, h) | Row::Line { f, h, .. } => Some((f, h)),
            _ => None,
        }
    }
}

/// Builds the rows for a model, unified or side by side.
pub fn rows(m: &Model, sbs: bool) -> Vec<Row> {
    let mut rows: Vec<Row> = (0..m.header.len()).map(Row::Text).collect();
    if !m.header.is_empty() {
        rows.push(Row::Blank);
    }
    let both = m.files.iter().any(|f| f.section == Section::Staged) && m.files.iter().any(|f| f.section == Section::Unstaged);
    let mut section = None;
    for (fi, f) in m.files.iter().enumerate() {
        if both && section != Some(f.section) {
            if section.is_some() {
                rows.push(Row::Blank);
            }
            rows.push(Row::Section(f.section));
            section = Some(f.section);
        }
        rows.push(Row::File(fi));
        if f.note.is_some() {
            rows.push(Row::Note(fi));
        }
        for (hi, h) in f.diff.hunks.iter().enumerate() {
            rows.push(Row::Hunk(fi, hi));
            if !sbs {
                for (li, l) in h.lines.iter().enumerate() {
                    let (a, b) = match l.kind {
                        LineKind::Context => (Some(li), Some(li)),
                        LineKind::Deleted => (Some(li), None),
                        LineKind::Added => (None, Some(li)),
                    };
                    rows.push(Row::Line { f: fi, h: hi, a, b });
                }
                continue;
            }
            let mut i = 0;
            while i < h.lines.len() {
                if h.lines[i].kind == LineKind::Context {
                    rows.push(Row::Line { f: fi, h: hi, a: Some(i), b: Some(i) });
                    i += 1;
                    continue;
                }
                let d0 = i;
                while i < h.lines.len() && h.lines[i].kind == LineKind::Deleted {
                    i += 1;
                }
                let a0 = i;
                while i < h.lines.len() && h.lines[i].kind == LineKind::Added {
                    i += 1;
                }
                let (nd, na) = (a0 - d0, i - a0);
                for k in 0..nd.max(na) {
                    rows.push(Row::Line { f: fi, h: hi, a: (k < nd).then_some(d0 + k), b: (k < na).then_some(a0 + k) });
                }
            }
        }
        rows.push(Row::Blank);
    }
    rows
}

// ---- building the model (background thread) ------------------------------------------------

fn git_diff(root: &Path, extra: &[&str], paths: &[String]) -> Result<String, String> {
    let mut a = vec!["-c", "core.quotePath=false", "diff"];
    a.extend_from_slice(DIFF_ARGS);
    a.extend_from_slice(extra);
    if !paths.is_empty() {
        a.push("--");
        a.extend(paths.iter().map(String::as_str));
    }
    run(root, &a)
}

/// `git show <rev>:<path>` as text (None: not there, or binary).
fn show(root: &Path, rev: &str, path: &str) -> Option<String> {
    let t = run(root, &["show", &format!("{rev}:{path}")]).ok()?;
    (!t.contains('\0')).then_some(t)
}

/// Where a side's full text comes from (for syntax colours).
#[derive(Clone)]
enum Src {
    Disk,
    Rev(String),
    None,
}

fn text_of(root: &Path, src: &Src, path: &str) -> Option<String> {
    match src {
        Src::Disk => {
            let p = root.join(path);
            if std::fs::metadata(&p).ok()?.len() as usize > MAX_HIGHLIGHT_BYTES {
                return None;
            }
            std::fs::read_to_string(p).ok()
        }
        Src::Rev(r) => show(root, r, path).filter(|t| t.len() <= MAX_HIGHLIGHT_BYTES),
        Src::None => None,
    }
}

/// Tokens of the `wanted` lines (1-based) of `text`, highlighted as a whole file.
fn highlight(path: &str, text: &str, wanted: &BTreeSet<usize>) -> HashMap<usize, Vec<Option<Token>>> {
    let mut out = HashMap::new();
    if wanted.is_empty() {
        return out;
    }
    let rope = Rope::from_str(text);
    let mut syn = Syntax::detect(Some(Path::new(path)), &rope);
    if !syn.lang.highlights() {
        return out;
    }
    syn.update(&rope, 1);
    let total = rope.len_lines();
    let lines = syn.visible(&rope, 0, total);
    for &n in wanted {
        if let Some(t) = lines.get(n - 1) {
            out.insert(n, t.clone());
        }
    }
    out
}

fn entry(diff: FileDiff, section: Section) -> FileEntry {
    let words = diff
        .hunks
        .iter()
        .map(|h| {
            let mut w = vec![Vec::new(); h.lines.len()];
            for (d, a) in patch::change_pairs(&h.lines) {
                if let Some((o, n)) = patch::word_ranges(&h.lines[d].text, &h.lines[a].text) {
                    w[d] = o;
                    w[a] = n;
                }
            }
            w
        })
        .collect();
    let note = if diff.binary {
        Some("Binary file: no text diff".to_string())
    } else if diff.mode_only {
        Some("File mode changed (no text changes)".to_string())
    } else {
        None
    };
    FileEntry { diff, section, words, old_tok: HashMap::new(), new_tok: HashMap::new(), note }
}

fn add_tokens(root: &Path, e: &mut FileEntry, old: &Src, new: &Src) {
    let (mut ow, mut nw) = (BTreeSet::new(), BTreeSet::new());
    for h in &e.diff.hunks {
        for l in &h.lines {
            match l.kind {
                LineKind::Deleted => {
                    ow.extend(l.old);
                }
                _ => {
                    nw.extend(l.new);
                }
            }
        }
    }
    if !nw.is_empty() {
        if let Some(t) = text_of(root, new, &e.diff.path) {
            e.new_tok = highlight(&e.diff.path, &t, &nw);
        }
    }
    if !ow.is_empty() {
        if let Some(t) = text_of(root, old, &e.diff.path) {
            e.old_tok = highlight(&e.diff.path, &t, &ow);
        }
    }
}

fn untracked_files(root: &Path) -> Vec<String> {
    run(root, &["-c", "core.quotePath=false", "ls-files", "--others", "--exclude-standard", "-z"])
        .map(|o| o.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Builds the model for `scope`; `None` when the fingerprint says nothing changed.
pub fn build(root: &Path, scope: &Scope, old_fp: &str) -> Option<Model> {
    match build_inner(root, scope, old_fp) {
        Ok(m) => m,
        Err(e) => Some(Model { error: Some(e.clone()), fingerprint: format!("error {e}"), ..Model::default() }),
    }
}

fn build_inner(root: &Path, scope: &Scope, old_fp: &str) -> Result<Option<Model>, String> {
    let mut m = Model::default();
    let mut highlight_jobs: Vec<(usize, Src, Src)> = Vec::new();
    match scope {
        Scope::Worktree => {
            let unstaged = git_diff(root, &[], &[])?;
            let staged = git_diff(root, &["--cached"], &[])?;
            let untracked = untracked_files(root);
            let mut fp = format!("{unstaged}\0{staged}\0");
            let metas: Vec<Option<std::fs::Metadata>> = untracked.iter().map(|p| std::fs::metadata(root.join(p)).ok()).collect();
            for (p, meta) in untracked.iter().zip(&metas) {
                let (len, mt) = meta.as_ref().map_or((0, 0), |m| (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())));
                fp.push_str(&format!("{p}:{len}:{mt}\0"));
            }
            if fp == old_fp {
                return Ok(None);
            }
            m.fingerprint = fp;
            let mut files: Vec<FileEntry> = patch::parse(&unstaged).into_iter().map(|d| entry(d, Section::Unstaged)).collect();
            for (i, (p, meta)) in untracked.iter().zip(&metas).enumerate() {
                let big = meta.as_ref().is_none_or(|m| m.len() > MAX_UNTRACKED_BYTES);
                let mut e = if big || i >= MAX_UNTRACKED_READ {
                    let mut e = entry(patch::untracked(p, ""), Section::Unstaged);
                    e.note = Some(if big { "New file, too big to show".into() } else { "New file (not read: too many new files)".into() });
                    e
                } else {
                    match std::fs::read(root.join(p)) {
                        Ok(b) if b.contains(&0) || std::str::from_utf8(&b).is_err() => {
                            let mut e = entry(patch::untracked(p, ""), Section::Unstaged);
                            e.diff.binary = true;
                            e.note = Some("New binary file".into());
                            e
                        }
                        Ok(b) => {
                            let t = String::from_utf8(b).unwrap_or_default();
                            let mut e = entry(patch::untracked(p, &t), Section::Unstaged);
                            if t.is_empty() {
                                e.note = Some("New empty file".into());
                            }
                            e
                        }
                        Err(_) => continue,
                    }
                };
                e.diff.status = Status::Untracked;
                files.push(e);
            }
            files.sort_by(|a, b| a.diff.path.cmp(&b.diff.path));
            let n = files.len();
            for i in 0..n {
                highlight_jobs.push((i, Src::Rev(String::new()), Src::Disk));
            }
            let mut st: Vec<FileEntry> = patch::parse(&staged).into_iter().map(|d| entry(d, Section::Staged)).collect();
            for i in 0..st.len() {
                highlight_jobs.push((n + i, Src::Rev("HEAD".into()), Src::Rev(String::new())));
            }
            files.append(&mut st);
            m.files = files;
        }
        Scope::Run { base, touched, only_touched, .. } => {
            let now = super::snapshot(root)?;
            let fp = format!("{now} {only_touched}");
            if fp == old_fp {
                return Ok(None);
            }
            m.fingerprint = fp;
            let paths: &[String] = if *only_touched { touched } else { &[] };
            if *only_touched && touched.is_empty() {
                m.files = Vec::new();
            } else {
                let out = git_diff(root, &[base.as_str(), now.as_str()], paths)?;
                m.files = patch::parse(&out).into_iter().map(|d| entry(d, Section::Range)).collect();
            }
            for i in 0..m.files.len() {
                highlight_jobs.push((i, Src::Rev(base.clone()), Src::Disk));
            }
        }
        Scope::Commit { sha } => {
            let fp = format!("commit {sha}");
            if fp == old_fp {
                return Ok(None);
            }
            m.fingerprint = fp;
            let head = run(root, &["show", "-s", "--date=format:%Y-%m-%d %H:%M", "--format=%H%n%an <%ae>%n%ad%n%D%n%B", sha])?;
            let mut lines = head.lines();
            let full = lines.next().unwrap_or("").to_string();
            let who = lines.next().unwrap_or("").to_string();
            let when = lines.next().unwrap_or("").to_string();
            let refs = lines.next().unwrap_or("").to_string();
            m.header.push(format!("commit {full}{}", if refs.is_empty() { String::new() } else { format!("  ({refs})") }));
            m.header.push(format!("{who} · {when}"));
            m.header.push(String::new());
            let body: Vec<String> = lines.map(|l| format!("    {l}")).collect();
            let keep = body.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
            m.header.extend(body.into_iter().take(keep));
            let mut a = vec!["-c", "core.quotePath=false", "show", "--format=", "--diff-merges=first-parent"];
            a.extend_from_slice(DIFF_ARGS);
            a.push(sha);
            let out = run(root, &a)?;
            m.files = patch::parse(&out).into_iter().map(|d| entry(d, Section::Range)).collect();
            let parent = run(root, &["rev-parse", "--verify", "-q", &format!("{sha}^1")]).ok().map(|s| s.trim().to_string());
            for i in 0..m.files.len() {
                highlight_jobs.push((i, parent.clone().map_or(Src::None, Src::Rev), Src::Rev(sha.clone())));
            }
        }
    }
    for (i, old, new) in highlight_jobs.into_iter().take(MAX_HIGHLIGHT_FILES) {
        if let Some(e) = m.files.get_mut(i) {
            if !e.diff.binary {
                add_tokens(root, e, &old, &new);
            }
        }
    }
    Ok(Some(m))
}

// ---- the review state ------------------------------------------------------------------------

/// Where the cursor was, to find it again after a refresh or a layout switch.
#[derive(Clone, Debug, PartialEq)]
struct Anchor {
    path: String,
    section: Section,
    /// On a hunk header: that hunk.
    hunk: Option<usize>,
    /// New-side (else old-side) line number of the cursor line; 0 = the file header.
    line: usize,
    old_side: bool,
}

/// An undoable revert: the file's bytes before and right after it.
#[derive(Clone, Debug)]
pub struct Undo {
    pub path: String,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}

pub struct Review {
    pub scope: Scope,
    pub model: Model,
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub scroll: usize,
    /// Rows the last layout was built for (side by side or not).
    pub sbs: bool,
    /// The document's height at the last draw (paging, keeping the cursor in view).
    pub view_h: usize,
    pub loaded: bool,
    job: Option<Receiver<Option<Model>>>,
    last: Option<Instant>,
    force: bool,
    /// Commit details: the log row to go back to.
    pub from_log: bool,
}

impl Review {
    pub fn new(scope: Scope) -> Review {
        Review { scope, model: Model::default(), rows: Vec::new(), cursor: 0, scroll: 0, sbs: false, view_h: 10, loaded: false, job: None, last: None, force: true, from_log: false }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Asks for a rebuild at the next tick (after an action).
    pub fn refresh(&mut self) {
        self.force = true;
    }

    /// Switches what is reviewed; the old rows go.
    pub fn set_scope(&mut self, scope: Scope) {
        *self = Review { sbs: self.sbs, view_h: self.view_h, ..Review::new(scope) };
    }

    /// Starts a refresh when due and takes a finished one. Returns true when the rows changed.
    pub fn tick(&mut self, root: &Path) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(Some(m)) => {
                    self.job = None;
                    self.set_model(m);
                    changed = true;
                }
                Ok(None) => {
                    self.job = None;
                    if !self.loaded {
                        self.loaded = true;
                        changed = true;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => self.job = None,
            }
        }
        let every = match self.scope {
            Scope::Worktree => REFRESH_EVERY,
            Scope::Run { .. } => REFRESH_EVERY * 2,
            Scope::Commit { .. } => Duration::from_secs(3600 * 24),
        };
        let due = self.force || self.last.is_none_or(|t| t.elapsed() >= every);
        if self.job.is_none() && due {
            self.force = false;
            self.last = Some(Instant::now());
            let (root, scope, fp) = (root.to_path_buf(), self.scope.clone(), self.model.fingerprint.clone());
            self.job = Some(job(move || build(&root, &scope, &fp)));
        }
        changed
    }

    /// Waits for the current refresh (tests).
    #[cfg(test)]
    pub fn wait(&mut self, root: &Path) {
        self.refresh();
        self.tick(root);
        let t = Instant::now();
        while self.job.is_some() {
            assert!(t.elapsed() < Duration::from_secs(20), "review build timed out");
            std::thread::sleep(Duration::from_millis(5));
            self.tick(root);
        }
    }

    fn anchor(&self) -> Option<Anchor> {
        let row = self.rows.get(self.cursor)?;
        let f = row.file()?;
        let e = self.model.files.get(f)?;
        let hunk = match *row {
            Row::Hunk(_, h) => Some(h),
            _ => None,
        };
        let (line, old_side) = match *row {
            Row::Line { h, a, b, .. } => {
                let hunk = &e.diff.hunks[h];
                match (b.and_then(|i| hunk.lines[i].new), a.and_then(|i| hunk.lines[i].old)) {
                    (Some(n), _) => (n, false),
                    (None, Some(o)) => (o, true),
                    _ => (0, false),
                }
            }
            Row::Hunk(_, h) => (e.diff.hunks[h].new_start.max(1), false),
            _ => (0, false),
        };
        Some(Anchor { path: e.diff.path.clone(), section: e.section, hunk, line, old_side })
    }

    fn find(&self, a: &Anchor) -> Option<usize> {
        let fi = self.model.files.iter().position(|e| e.diff.path == a.path && e.section == a.section).or_else(|| self.model.files.iter().position(|e| e.diff.path == a.path))?;
        let header = self.rows.iter().position(|r| *r == Row::File(fi))?;
        if let Some(h) = a.hunk.filter(|&h| h < self.model.files[fi].diff.hunks.len()) {
            return self.rows.iter().position(|r| *r == Row::Hunk(fi, h));
        }
        if a.line == 0 {
            return Some(header);
        }
        let e = &self.model.files[fi];
        let mut best: Option<(usize, usize)> = None;
        for (ri, r) in self.rows.iter().enumerate().skip(header) {
            if r.file() != Some(fi) {
                if ri > header + 1 {
                    break;
                }
                continue;
            }
            if let Row::Line { h, a: la, b: lb, .. } = *r {
                let hunk = &e.diff.hunks[h];
                let n = if a.old_side { la.and_then(|i| hunk.lines[i].old) } else { lb.and_then(|i| hunk.lines[i].new) };
                if let Some(n) = n {
                    let d = n.abs_diff(a.line);
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, ri));
                    }
                }
            }
        }
        Some(best.map_or(header, |b| b.1))
    }

    fn set_model(&mut self, m: Model) {
        let anchor = self.anchor();
        let first = !self.loaded;
        self.model = m;
        self.loaded = true;
        self.relayout(self.sbs, anchor);
        if first {
            // Start on the first hunk: that's what one wants to read (a commit's message stays
            // in view above it).
            self.cursor = self.rows.iter().position(|r| matches!(r, Row::Hunk(..))).or_else(|| self.rows.iter().position(|r| matches!(r, Row::File(_)))).unwrap_or(0);
            self.scroll = if self.model.header.is_empty() { self.cursor.saturating_sub(2) } else { 0 };
        }
    }

    fn relayout(&mut self, sbs: bool, anchor: Option<Anchor>) {
        let top_off = self.cursor.saturating_sub(self.scroll);
        self.sbs = sbs;
        self.rows = rows(&self.model, sbs);
        self.cursor = anchor.and_then(|a| self.find(&a)).unwrap_or(self.cursor).min(self.rows.len().saturating_sub(1));
        self.scroll = self.cursor.saturating_sub(top_off);
    }

    /// Rebuilds the rows side by side or unified, keeping the cursor on its line.
    pub fn set_sbs(&mut self, sbs: bool) {
        if sbs != self.sbs {
            let a = self.anchor();
            self.relayout(sbs, a);
        }
    }

    // ---- moving ----------------------------------------------------------------------------

    pub fn move_by(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = (self.cursor as isize + d).clamp(0, self.rows.len() as isize - 1) as usize;
    }

    pub fn move_to(&mut self, row: usize) {
        self.cursor = row.min(self.rows.len().saturating_sub(1));
        // Show a little above it.
        if self.cursor < self.scroll + 1 || self.cursor >= self.scroll + self.view_h.max(2) - 1 {
            self.scroll = self.cursor.saturating_sub(2);
        }
    }

    /// Next / previous hunk (crossing files); false at the end.
    pub fn next_hunk(&mut self, back: bool) -> bool {
        let found = if back {
            let cur_hunk_start = (0..=self.cursor.min(self.rows.len().saturating_sub(1))).rev().find(|&i| matches!(self.rows[i], Row::Hunk(..)));
            let before = match cur_hunk_start {
                Some(s) if s < self.cursor => Some(s),
                Some(s) => (0..s).rev().find(|&i| matches!(self.rows[i], Row::Hunk(..))),
                None => None,
            };
            before
        } else {
            (self.cursor + 1..self.rows.len()).find(|&i| matches!(self.rows[i], Row::Hunk(..)))
        };
        match found {
            Some(i) => {
                self.move_to(i);
                true
            }
            None => false,
        }
    }

    /// Next / previous file header.
    pub fn next_file(&mut self, back: bool) -> bool {
        let cur = self.rows.get(self.cursor).and_then(Row::file);
        let found = if back {
            let start = cur.and_then(|f| self.rows.iter().position(|r| *r == Row::File(f)));
            match start {
                Some(s) if s < self.cursor => Some(s),
                Some(s) => (0..s).rev().find(|&i| matches!(self.rows[i], Row::File(_))),
                None => (0..self.cursor).rev().find(|&i| matches!(self.rows[i], Row::File(_))),
            }
        } else {
            (self.cursor + 1..self.rows.len()).find(|&i| matches!(self.rows[i], Row::File(_)))
        };
        match found {
            Some(i) => {
                self.move_to(i);
                true
            }
            None => false,
        }
    }

    /// Puts the cursor on file `f`'s header.
    pub fn goto_file(&mut self, f: usize) {
        if let Some(i) = self.rows.iter().position(|r| *r == Row::File(f)) {
            self.move_to(i);
            self.scroll = i;
        }
    }

    /// The file under the cursor (the last one above it on blank/section rows).
    pub fn current_file(&self) -> Option<usize> {
        (0..=self.cursor.min(self.rows.len().saturating_sub(1))).rev().find_map(|i| self.rows[i].file())
    }

    /// Where F4 opens: path and 1-based line.
    pub fn source_at_cursor(&self) -> Option<(String, usize)> {
        let f = self.current_file()?;
        let e = &self.model.files[f];
        let line = match self.rows.get(self.cursor)? {
            Row::Line { h, a, b, .. } => {
                let hunk = &e.diff.hunks[*h];
                b.and_then(|i| hunk.lines[i].new)
                    .or_else(|| a.and_then(|i| hunk.lines[..=i].iter().rev().find_map(|l| l.new).map(|n| n + 1)))
                    .unwrap_or(hunk.new_start.max(1))
            }
            Row::Hunk(_, h) => e.diff.hunks[*h].first_new_line(),
            _ => e.diff.hunks.first().map_or(1, |h| h.first_new_line()),
        };
        Some((e.diff.path.clone(), line))
    }

    /// Totals for the title: files, +, −.
    pub fn totals(&self) -> (usize, usize, usize) {
        let mut seen = std::collections::HashSet::new();
        let (mut a, mut d) = (0, 0);
        for e in &self.model.files {
            seen.insert(e.diff.path.as_str());
            a += e.diff.added();
            d += e.diff.deleted();
        }
        (seen.len(), a, d)
    }

    /// "hunk 2/7" for the cursor.
    #[cfg(test)]
    pub fn hunk_position(&self) -> Option<(usize, usize)> {
        let total = self.rows.iter().filter(|r| matches!(r, Row::Hunk(..))).count();
        let cur = self.rows.get(self.cursor)?.hunk()?;
        let idx = self.rows.iter().filter(|r| matches!(r, Row::Hunk(..))).position(|r| r.hunk() == Some(cur))?;
        Some((idx + 1, total))
    }
}

// ---- actions ---------------------------------------------------------------------------------

/// What `s` would do at the cursor.
#[derive(Clone, Debug, PartialEq)]
pub enum StageOp {
    StageHunk { patch: String },
    UnstageHunk { patch: String },
    StageFile { path: String },
    UnstageFile { path: String },
}

/// The stage / unstage for the cursor (`whole`: the file, not the hunk). Err = why not.
pub fn stage_op(r: &Review, whole: bool) -> Result<(StageOp, String), String> {
    if !matches!(r.scope, Scope::Worktree) {
        return Err("Staging works in the working-tree review: press a until it says \"working tree\"".into());
    }
    let f = r.current_file().ok_or("No file here")?;
    let e = &r.model.files[f];
    let path = e.diff.path.clone();
    let hunk = r.rows.get(r.cursor).and_then(Row::hunk).map(|(_, h)| h);
    let whole = whole || hunk.is_none() || e.diff.status == Status::Untracked || e.diff.binary;
    Ok(match (e.section, whole) {
        (Section::Unstaged, true) => (StageOp::StageFile { path: path.clone() }, format!("Staged {path}")),
        (Section::Staged, true) => (StageOp::UnstageFile { path: path.clone() }, format!("Unstaged {path}")),
        (Section::Unstaged, false) => (StageOp::StageHunk { patch: e.diff.hunk_patch(hunk.unwrap()) }, format!("Staged a hunk of {path}")),
        (Section::Staged, false) => (StageOp::UnstageHunk { patch: e.diff.hunk_patch(hunk.unwrap()) }, format!("Unstaged a hunk of {path}")),
        (Section::Range, _) => return Err("Not stageable here".into()),
    })
}

pub fn do_stage(root: &Path, op: &StageOp) -> Result<(), String> {
    match op {
        StageOp::StageHunk { patch } => super::apply(root, patch, &["--cached"]),
        StageOp::UnstageHunk { patch } => super::apply(root, patch, &["--cached", "-R"]),
        StageOp::StageFile { path } => run(root, &["add", "-A", "--", path]).map(|_| ()),
        StageOp::UnstageFile { path } => run(root, &["restore", "--staged", "--", path]).or_else(|_| run(root, &["rm", "-q", "--cached", "--", path])).map(|_| ()),
    }
}

/// What `r` / `R` would revert at the cursor.
#[derive(Clone, Debug, PartialEq)]
pub enum RevertOp {
    /// Reverse-apply a patch to the working tree.
    Patch { path: String, patch: String },
    /// Put back the index version (`git checkout -- path`).
    FromIndex { path: String },
    /// Write a revision's version (a run's snapshot), or delete when it had none.
    FromRev { path: String, rev: String },
    /// Delete an untracked file.
    Delete { path: String },
}

impl RevertOp {
    pub fn path(&self) -> &str {
        match self {
            RevertOp::Patch { path, .. } | RevertOp::FromIndex { path } | RevertOp::FromRev { path, .. } | RevertOp::Delete { path } => path,
        }
    }
}

/// The revert for the cursor and the question to ask first. Err = why not.
pub fn revert_op(r: &Review, whole: bool) -> Result<(RevertOp, String), String> {
    if r.scope.read_only() {
        return Err("A commit can't be reverted from here".into());
    }
    let f = r.current_file().ok_or("No file here")?;
    let e = &r.model.files[f];
    let path = e.diff.path.clone();
    if e.section == Section::Staged {
        return Err(format!("That change of {path} is staged: unstage it first (s), then revert"));
    }
    let hunk = r.rows.get(r.cursor).and_then(Row::hunk).map(|(_, h)| h);
    if e.diff.status == Status::Untracked {
        return Ok((RevertOp::Delete { path: path.clone() }, format!("Delete the new file {path}? (u undoes)")));
    }
    let whole = whole || hunk.is_none() || e.diff.binary || e.diff.mode_only;
    if !whole {
        let h = &e.diff.hunks[hunk.unwrap()];
        let q = format!("Revert this hunk of {path} (+{} −{})? (u undoes)", h.added(), h.deleted());
        return Ok((RevertOp::Patch { path, patch: e.diff.hunk_patch(hunk.unwrap()) }, q));
    }
    let q = format!("Revert all changes to {path} (+{} −{})? (u undoes)", e.diff.added(), e.diff.deleted());
    match &r.scope {
        Scope::Run { base, .. } => Ok((RevertOp::FromRev { path, rev: base.clone() }, q.replace("all changes to", "everything since the run started in"))),
        _ => Ok((RevertOp::FromIndex { path }, q)),
    }
}

/// Carries out a revert; returns the undo record.
pub fn do_revert(root: &Path, op: &RevertOp) -> Result<Undo, String> {
    let path = op.path().to_string();
    let abs = root.join(&path);
    let before = std::fs::read(&abs).ok();
    match op {
        RevertOp::Patch { patch, .. } => super::apply(root, patch, &["-R"])?,
        RevertOp::FromIndex { .. } => {
            run(root, &["checkout", "-q", "--", &path])?;
        }
        RevertOp::FromRev { rev, .. } => {
            // Only the working tree: `git checkout <rev> -- path` would stage it too.
            match run(root, &["cat-file", "-e", &format!("{rev}:{path}")]) {
                Ok(_) => {
                    let bytes = blob_bytes(root, rev, &path)?;
                    if let Some(d) = abs.parent() {
                        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(&abs, bytes).map_err(|e| e.to_string())?;
                }
                Err(_) => std::fs::remove_file(&abs).map_err(|e| e.to_string())?,
            }
        }
        RevertOp::Delete { .. } => std::fs::remove_file(&abs).map_err(|e| e.to_string())?,
    }
    let after = std::fs::read(&abs).ok();
    Ok(Undo { path, before, after })
}

fn blob_bytes(root: &Path, rev: &str, path: &str) -> Result<Vec<u8>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "blob", &format!("{rev}:{path}")])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Puts a reverted file back as it was, unless it changed since the revert.
pub fn undo(root: &Path, u: &Undo) -> Result<(), String> {
    let abs: PathBuf = root.join(&u.path);
    let now = std::fs::read(&abs).ok();
    if now != u.after {
        return Err(format!("{} changed since the revert; not undone", u.path));
    }
    match &u.before {
        Some(b) => {
            if let Some(d) = abs.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(&abs, b).map_err(|e| e.to_string())
        }
        None => std::fs::remove_file(&abs).map_err(|e| e.to_string()),
    }
}

/// Counts changes between a run's snapshot and now: "3 files +42 −7".
pub fn run_stat(root: &Path, base: &str) -> Result<(usize, usize, usize), String> {
    let now = super::snapshot(root)?;
    let out = run(root, &["diff", "--numstat", "--no-renames", base, &now])?;
    Ok(patch::numstat(&out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::scratch_repo;

    fn lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    fn setup(name: &str) -> PathBuf {
        let dir = scratch_repo(name);
        std::fs::write(dir.join("a.txt"), lines(30)).unwrap();
        run(&dir, &["commit", "-qam", "thirty"]).unwrap();
        dir
    }

    #[test]
    fn worktree_model_has_unstaged_untracked_and_staged_sections() {
        let dir = setup("review-model");
        std::fs::write(dir.join("a.txt"), lines(30).replace("line 2\n", "line two\n").replace("line 28\n", "line 28\nmore\n")).unwrap();
        std::fs::write(dir.join("new.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("b.txt"), "bee\nstaged\n").unwrap();
        run(&dir, &["add", "b.txt"]).unwrap();
        let mut r = Review::new(Scope::Worktree);
        r.wait(&dir);
        let files: Vec<(&str, Section, Status)> = r.model.files.iter().map(|e| (e.diff.path.as_str(), e.section, e.diff.status)).collect();
        assert_eq!(files, vec![("a.txt", Section::Unstaged, Status::Modified), ("new.rs", Section::Unstaged, Status::Untracked), ("b.txt", Section::Staged, Status::Modified)]);
        assert_eq!(r.totals(), (3, 4, 1));
        // Word ranges on the changed pair, syntax tokens on the new Rust file.
        assert_eq!(r.model.files[0].words[0][2], vec![(5, 8)], "\"two\" vs \"2\"");
        assert!(r.model.files[1].new_tok.get(&1).is_some_and(|t| t.iter().any(Option::is_some)));
        // Sections show because both exist; the cursor starts on the first hunk.
        assert_eq!(r.rows[0], Row::Section(Section::Unstaged));
        assert!(matches!(r.rows[r.cursor], Row::Hunk(0, 0)));
        assert_eq!(r.hunk_position(), Some((1, 4)));
        // An unchanged tree doesn't rebuild.
        let fp = r.model.fingerprint.clone();
        assert!(build(&dir, &Scope::Worktree, &fp).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn side_by_side_pairs_changes_and_keeps_the_cursor_line() {
        let dir = setup("review-sbs");
        std::fs::write(dir.join("a.txt"), lines(30).replace("line 5\n", "five\nfive b\n")).unwrap();
        let mut r = Review::new(Scope::Worktree);
        r.wait(&dir);
        // Unified: 3 context, 1 deleted, 2 added, 3 context.
        let unified = r.rows.iter().filter(|x| matches!(x, Row::Line { .. })).count();
        assert_eq!(unified, 9);
        assert!(matches!(r.rows[r.cursor], Row::Hunk(0, 0)));
        r.move_by(6); // on "five b" (second added line)
        assert_eq!(r.source_at_cursor(), Some(("a.txt".into(), 6)));
        r.set_sbs(true);
        let sbs: Vec<&Row> = r.rows.iter().filter(|x| matches!(x, Row::Line { .. })).collect();
        assert_eq!(sbs.len(), 8, "the deleted line and the first added line share a row");
        assert!(sbs.iter().any(|x| matches!(x, Row::Line { a: Some(3), b: Some(4), .. })));
        assert!(sbs.iter().any(|x| matches!(x, Row::Line { a: None, b: Some(5), .. })));
        assert_eq!(r.source_at_cursor(), Some(("a.txt".into(), 6)), "same line after the switch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stage_unstage_and_revert_hunks_with_undo() {
        let dir = setup("review-actions");
        let changed = lines(30).replace("line 2\n", "line two\n").replace("line 28\n", "line 28\nmore\n");
        std::fs::write(dir.join("a.txt"), &changed).unwrap();
        let mut r = Review::new(Scope::Worktree);
        r.wait(&dir);
        // Stage the second hunk.
        assert!(r.next_hunk(false));
        let (op, msg) = stage_op(&r, false).unwrap();
        assert_eq!(msg, "Staged a hunk of a.txt");
        do_stage(&dir, &op).unwrap();
        r.wait(&dir);
        let staged: Vec<_> = r.model.files.iter().filter(|e| e.section == Section::Staged).collect();
        assert_eq!(staged.len(), 1);
        assert!(staged[0].diff.hunks[0].lines.iter().any(|l| l.text == "more"));
        let cached = run(&dir, &["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(cached, "a.txt\n");
        // A staged hunk can't be reverted, only unstaged.
        let row = r.rows.iter().position(|x| matches!(x, Row::Hunk(f, _) if r.model.files[*f].section == Section::Staged)).unwrap();
        r.move_to(row);
        assert!(revert_op(&r, false).unwrap_err().contains("unstage it first"));
        let (op, _) = stage_op(&r, false).unwrap();
        assert!(matches!(op, StageOp::UnstageHunk { .. }));
        do_stage(&dir, &op).unwrap();
        r.wait(&dir);
        assert!(r.model.files.iter().all(|e| e.section == Section::Unstaged));
        // Revert the first hunk; undo brings it back.
        r.move_to(0);
        r.next_hunk(false);
        let (op, q) = revert_op(&r, false).unwrap();
        assert!(q.starts_with("Revert this hunk of a.txt (+1 −1)?"), "{q}");
        let u = do_revert(&dir, &op).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), lines(30).replace("line 28\n", "line 28\nmore\n"));
        undo(&dir, &u).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), changed);
        // Undo refuses when the file moved on since.
        let u = do_revert(&dir, &op).unwrap();
        std::fs::write(dir.join("a.txt"), "edited again\n").unwrap();
        assert!(undo(&dir, &u).unwrap_err().contains("changed since the revert"));
        // Untracked files: staged whole, reverted = deleted (and undone = back).
        std::fs::write(dir.join("n.txt"), "new\n").unwrap();
        r.wait(&dir);
        let f = r.model.files.iter().position(|e| e.diff.path == "n.txt").unwrap();
        r.goto_file(f);
        let (op, q) = revert_op(&r, true).unwrap();
        assert!(q.starts_with("Delete the new file n.txt"));
        let u = do_revert(&dir, &op).unwrap();
        assert!(!dir.join("n.txt").exists());
        undo(&dir, &u).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("n.txt")).unwrap(), "new\n");
        r.wait(&dir);
        let f = r.model.files.iter().position(|e| e.diff.path == "n.txt").unwrap();
        r.goto_file(f);
        let (op, _) = stage_op(&r, false).unwrap();
        assert_eq!(op, StageOp::StageFile { path: "n.txt".into() });
        do_stage(&dir, &op).unwrap();
        assert!(run(&dir, &["diff", "--cached", "--name-only"]).unwrap().contains("n.txt"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_scope_shows_everything_since_the_snapshot_without_touching_the_index() {
        let dir = setup("review-run");
        std::fs::write(dir.join("a.txt"), lines(30).replace("line 1\n", "user edit\n")).unwrap();
        let index_before = std::fs::read(dir.join(".git/index")).unwrap();
        let base = crate::git::snapshot(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index_before, "the user's index is untouched");
        assert!(run(&dir, &["stash", "list"]).unwrap().is_empty());
        // The "agent" edits a file, adds one, and even commits.
        std::fs::write(dir.join("a.txt"), lines(30).replace("line 1\n", "user edit\n").replace("line 20\n", "agent edit\n")).unwrap();
        std::fs::write(dir.join("made.txt"), "by the agent\n").unwrap();
        run(&dir, &["add", "made.txt"]).unwrap();
        run(&dir, &["commit", "-qm", "agent commit"]).unwrap();
        let scope = Scope::Run { id: 1, title: "t".into(), base: base.clone(), since: 0, touched: vec!["made.txt".into()], only_touched: false };
        let mut r = Review::new(scope.clone());
        r.wait(&dir);
        let files: Vec<&str> = r.model.files.iter().map(|e| e.diff.path.as_str()).collect();
        assert_eq!(files, vec!["a.txt", "made.txt"]);
        let a = &r.model.files[0];
        assert_eq!((a.diff.added(), a.diff.deleted()), (1, 1), "the user's earlier edit is not the run's");
        assert_eq!(run_stat(&dir, &base).unwrap(), (2, 2, 1));
        // Only the files the agent's tools touched.
        let Scope::Run { touched, .. } = scope else { unreachable!() };
        let mut r = Review::new(Scope::Run { id: 1, title: "t".into(), base: base.clone(), since: 0, touched, only_touched: true });
        r.wait(&dir);
        assert_eq!(r.model.files.len(), 1);
        // Revert the whole file to the snapshot (a deletion here): working tree only.
        r.goto_file(0);
        let (op, _) = revert_op(&r, true).unwrap();
        assert!(matches!(op, RevertOp::FromRev { .. }));
        do_revert(&dir, &op).unwrap();
        assert!(!dir.join("made.txt").exists());
        assert!(run(&dir, &["ls-files", "made.txt"]).unwrap().contains("made.txt"), "the index keeps it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commit_scope_has_the_message_and_the_files() {
        let dir = setup("review-commit");
        let sha = run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let mut r = Review::new(Scope::Commit { sha });
        r.wait(&dir);
        assert!(r.model.header[0].starts_with("commit "));
        assert!(r.model.header.iter().any(|l| l.trim() == "thirty"));
        assert_eq!(r.model.files.len(), 1);
        assert!(stage_op(&r, false).is_err() && revert_op(&r, false).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
