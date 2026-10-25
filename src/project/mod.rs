//! Project view (M2): the file tree sidebar (Alt+1), the file finder (Ctrl+Shift+N / Ctrl+P),
//! find in files (Ctrl+Shift+F / Alt+F) and recent files (Ctrl+E).
//!
//! `App` keeps one [`Project`]; the hooks in `app.rs` hand it keys, mouse events and ticks first,
//! and it answers with an [`Outcome`]. Opening files goes through [`App::open_target`].

pub mod fuzzy;
pub mod search;
pub mod tree;
pub mod ui;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::App;
use crate::doc::Doc;
use search::{Hit, Job};
use tree::Tree;

pub const SIDEBAR_W: u16 = 28;
const RECENT_MAX: usize = 50;
/// Shown in the finder at most.
const FINDER_MAX: usize = 500;
/// Wait this long after the last keystroke before searching the files.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

/// A file to open, optionally at a place in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    /// 0-based line and char column, and the length to select there.
    pub at: Option<(usize, usize, usize)>,
}

pub enum Outcome {
    /// The project view used the event.
    Handled,
    Open(Target),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PopupKind {
    Files,
    Recent,
    Search,
}

impl PopupKind {
    pub fn title(self) -> &'static str {
        match self {
            PopupKind::Files => "Find File",
            PopupKind::Recent => "Recent Files",
            PopupKind::Search => "Find in Files",
        }
    }
}

pub struct Popup {
    pub kind: PopupKind,
    pub input: String,
    pub selected: usize,
    pub scroll: usize,
}

/// One finder row: a path and the char positions the query matched.
pub struct Match {
    pub path: String,
    pub pos: Vec<usize>,
}

pub struct Project {
    pub root: PathBuf,
    pub tree: Tree,
    pub sidebar: bool,
    pub focus_tree: bool,
    pub popup: Option<Popup>,
    /// Most recent first, absolute.
    pub recent: Vec<PathBuf>,

    /// All project files (relative), filled in the background.
    pub files: Vec<String>,
    index: Option<Job<Vec<String>>>,
    /// Finder / recent rows for the current input.
    pub matches: Vec<Match>,
    matches_stale: bool,

    pub hits: Vec<Hit>,
    pub hit_files: usize,
    search: Option<Job<Vec<Hit>>>,
    pub searched: String,
    search_due: Option<Instant>,
    /// Last search popup input, kept when it's closed (IDEA remembers it too).
    last_search: String,

    /// Screen areas from the last draw (mouse).
    pub sidebar_rect: Rect,
    pub list_rect: Rect,
    pub popup_rect: Rect,
}

/// Splits command-line arguments: the first folder becomes the project root (the current folder
/// when none is given); the rest are files to open. Returns (root, files, opened as a project).
pub fn split_args(paths: Vec<PathBuf>) -> (PathBuf, Vec<PathBuf>, bool) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let dir = paths.iter().position(|p| p.is_dir());
    let as_project = dir.is_some() || paths.is_empty();
    let root = dir.map(|i| paths[i].clone()).unwrap_or(cwd);
    let files = paths.into_iter().filter(|p| !p.is_dir()).collect();
    (canonical(&root), files, as_project)
}

/// Canonical form for comparing paths (the path itself when it doesn't exist yet).
pub fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
        }
    })
}

/// `name:42` or `name:42:7` → ("name", line, col), 1-based as typed.
pub fn split_line_suffix(q: &str) -> (&str, Option<(usize, usize)>) {
    let mut parts = q.rsplitn(3, ':');
    let last = parts.next().unwrap_or("");
    let mid = parts.next();
    let first = parts.next();
    match (first, mid, last.parse::<usize>()) {
        (Some(f), Some(m), Ok(c)) if m.parse::<usize>().is_ok() => (f, Some((m.parse().unwrap(), c))),
        (_, Some(_), Ok(l)) => (&q[..q.len() - last.len() - 1], Some((l, 1))),
        _ => (q, None),
    }
}

impl Project {
    pub fn new(root: PathBuf, show_sidebar: bool) -> Self {
        Project {
            tree: Tree::new(&root),
            index: Some(search::index_files(root.clone())),
            root,
            sidebar: show_sidebar,
            focus_tree: false,
            popup: None,
            recent: Vec::new(),
            files: Vec::new(),
            matches: Vec::new(),
            matches_stale: true,
            hits: Vec::new(),
            hit_files: 0,
            search: None,
            searched: String::new(),
            search_due: None,
            last_search: String::new(),
            sidebar_rect: Rect::default(),
            list_rect: Rect::default(),
            popup_rect: Rect::default(),
        }
    }

    pub fn indexing(&self) -> bool {
        self.index.is_some()
    }

    pub fn searching(&self) -> bool {
        self.search.is_some() || self.search_due.is_some()
    }

    pub fn rel(&self, p: &Path) -> String {
        search::rel(&self.root, p)
    }

    pub fn note_opened(&mut self, p: &Path) {
        let p = canonical(p);
        self.recent.retain(|r| *r != p);
        self.recent.insert(0, p);
        self.recent.truncate(RECENT_MAX);
        self.matches_stale = true;
    }

    /// Drains the background jobs; called every loop turn (at least every 250 ms).
    pub fn poll(&mut self) {
        if let Some(job) = &self.index {
            let before = self.files.len();
            while let Ok(batch) = job.rx.try_recv() {
                self.files.extend(batch);
            }
            let done = job.done();
            if done {
                while let Ok(batch) = job.rx.try_recv() {
                    self.files.extend(batch);
                }
                self.files.sort_unstable();
                self.index = None;
            }
            if self.files.len() != before || done {
                self.matches_stale = true;
            }
        }
        if self.search_due.is_some_and(|t| t.elapsed() >= SEARCH_DEBOUNCE) {
            self.search_due = None;
            self.start_search();
        }
        if let Some(job) = self.search.take() {
            let done = job.done();
            while let Ok(batch) = job.rx.try_recv() {
                self.push_hits(batch);
            }
            if !done {
                self.search = Some(job);
            }
        }
        if self.matches_stale && self.popup.as_ref().is_some_and(|p| p.kind != PopupKind::Search) {
            self.refilter();
        }
    }

    fn push_hits(&mut self, batch: Vec<Hit>) {
        let room = search::MAX_HITS.saturating_sub(self.hits.len());
        if room > 0 && !batch.is_empty() {
            self.hit_files += 1;
            self.hits.extend(batch.into_iter().take(room));
        }
    }

    fn start_search(&mut self) {
        let q = self.popup.as_ref().map(|p| p.input.clone()).unwrap_or_default();
        self.hits.clear();
        self.hit_files = 0;
        self.searched = q.clone();
        if let Some(p) = &mut self.popup {
            p.selected = 0;
            p.scroll = 0;
        }
        // Dropping the old job cancels it.
        self.search = (q.chars().count() >= 2).then(|| search::find_in_files(self.root.clone(), q));
    }

    /// Recomputes the finder / recent rows for the current input.
    fn refilter(&mut self) {
        self.matches_stale = false;
        let Some(p) = &self.popup else { return };
        let (q, _) = split_line_suffix(p.input.trim());
        let recent: Vec<String> = self.recent.iter().map(|r| self.rel(r)).collect();
        let mut scored: Vec<(i32, Match)> = Vec::new();
        let pool: Box<dyn Iterator<Item = &String>> = match p.kind {
            PopupKind::Recent => Box::new(recent.iter()),
            _ if q.is_empty() => Box::new(recent.iter().filter(|r| !r.starts_with('/'))),
            _ => Box::new(self.files.iter()),
        };
        for path in pool {
            if let Some((s, pos)) = fuzzy::score(q, path) {
                scored.push((s, Match { path: path.clone(), pos }));
            }
        }
        if !q.is_empty() {
            // Stable sort: equal scores keep the recent / alphabetical order.
            scored.sort_by(|a, b| b.0.cmp(&a.0));
        }
        scored.truncate(FINDER_MAX);
        self.matches = scored.into_iter().map(|(_, m)| m).collect();
        if let Some(p) = &mut self.popup {
            p.selected = p.selected.min(self.matches.len().saturating_sub(1));
        }
    }

    pub fn open_popup(&mut self, kind: PopupKind, seed: Option<String>) {
        if let Some(p) = &self.popup {
            if p.kind == PopupKind::Search {
                self.last_search = p.input.clone();
            }
        }
        let input = match kind {
            PopupKind::Search => seed.unwrap_or_else(|| self.last_search.clone()),
            _ => String::new(),
        };
        let research = kind == PopupKind::Search && input != self.searched;
        self.popup = Some(Popup { kind, input, selected: 0, scroll: 0 });
        if research {
            self.search_due = Some(Instant::now().checked_sub(SEARCH_DEBOUNCE).unwrap_or_else(Instant::now));
        }
        self.matches_stale = true;
        self.refilter_if_list();
        // Opening Recent Files puts the previous file first, like IDEA's Ctrl+E.
        if kind == PopupKind::Recent && self.matches.len() > 1 {
            if let Some(p) = &mut self.popup {
                p.selected = 1;
            }
        }
    }

    fn refilter_if_list(&mut self) {
        if self.popup.as_ref().is_some_and(|p| p.kind != PopupKind::Search) {
            self.refilter();
        }
    }

    pub fn close_popup(&mut self) {
        if let Some(p) = self.popup.take() {
            if p.kind == PopupKind::Search {
                self.last_search = p.input;
            }
        }
    }

    pub fn popup_len(&self) -> usize {
        match &self.popup {
            Some(p) if p.kind == PopupKind::Search => self.hits.len(),
            Some(_) => self.matches.len(),
            None => 0,
        }
    }

    fn input_changed(&mut self) {
        let Some(p) = &mut self.popup else { return };
        p.selected = 0;
        p.scroll = 0;
        if p.kind == PopupKind::Search {
            self.search_due = Some(Instant::now());
        } else {
            self.refilter();
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(p) = &mut self.popup {
            p.input.push_str(text.lines().next().unwrap_or(""));
            self.input_changed();
        }
    }

    /// The selected popup row as a file to open.
    fn popup_target(&self) -> Option<Target> {
        let p = self.popup.as_ref()?;
        match p.kind {
            PopupKind::Search => {
                let h = self.hits.get(p.selected)?;
                Some(Target { path: self.root.join(&h.path), at: Some((h.line, h.start, h.end - h.start)) })
            }
            _ => {
                let m = self.matches.get(p.selected)?;
                let at = split_line_suffix(p.input.trim()).1.map(|(l, c)| (l.saturating_sub(1), c.saturating_sub(1), 0));
                Some(Target { path: self.root.join(&m.path), at })
            }
        }
    }

    /// Global project keys, then the open popup or the focused tree. `None` = not ours.
    pub fn key(&mut self, k: KeyEvent, selected_text: Option<String>, current_file: Option<PathBuf>) -> Option<Outcome> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let ch = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        // Global keys (also while a popup is open: they switch it).
        if alt && !ctrl && ch('1') {
            self.close_popup();
            if !self.sidebar {
                self.sidebar = true;
                self.focus_tree = true;
            } else if self.focus_tree {
                self.sidebar = false;
                self.focus_tree = false;
            } else {
                self.focus_tree = true;
            }
            return Some(Outcome::Handled);
        }
        if (ctrl && shift && ch('n')) || (ctrl && !shift && !alt && ch('p')) {
            self.open_popup(PopupKind::Files, None);
            return Some(Outcome::Handled);
        }
        if (ctrl && shift && ch('f')) || (alt && !ctrl && ch('f')) {
            let seed = selected_text.filter(|s| !s.is_empty() && !s.contains('\n'));
            self.open_popup(PopupKind::Search, seed);
            return Some(Outcome::Handled);
        }
        // Alt+F1: select the current file in the project view (IDEA's "Select In", straight to it).
        if alt && k.code == KeyCode::F(1) {
            self.close_popup();
            if let Some(path) = current_file {
                self.sidebar = true;
                self.focus_tree = self.tree.reveal(&canonical(&path));
            }
            return Some(Outcome::Handled);
        }
        if ctrl && !shift && !alt && ch('e') {
            self.open_popup(PopupKind::Recent, None);
            return Some(Outcome::Handled);
        }
        if self.popup.is_some() {
            return Some(self.popup_key(k));
        }
        if self.focus_tree && self.sidebar {
            return self.tree_key(k);
        }
        None
    }

    fn popup_key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let len = self.popup_len();
        let Some(p) = &mut self.popup else { return Outcome::Handled };
        let step = |p: &mut Popup, d: isize| {
            if len > 0 {
                p.selected = (p.selected as isize + d).clamp(0, len as isize - 1) as usize;
            }
        };
        match k.code {
            KeyCode::Esc => self.close_popup(),
            KeyCode::Enter => {
                if let Some(t) = self.popup_target() {
                    self.close_popup();
                    return Outcome::Open(t);
                }
            }
            KeyCode::Up => step(p, -1),
            KeyCode::Down => step(p, 1),
            KeyCode::PageUp => step(p, -10),
            KeyCode::PageDown => step(p, 10),
            KeyCode::Char('n') if k.modifiers == KeyModifiers::CONTROL => step(p, 1),
            KeyCode::Char('h') if ctrl => {
                pop_word(&mut p.input);
                self.input_changed();
            }
            KeyCode::Backspace if ctrl => {
                pop_word(&mut p.input);
                self.input_changed();
            }
            KeyCode::Backspace => {
                p.input.pop();
                self.input_changed();
            }
            KeyCode::Char('u') if k.modifiers == KeyModifiers::CONTROL => {
                p.input.clear();
                self.input_changed();
            }
            KeyCode::Char(c) if !ctrl => {
                p.input.push(c);
                self.input_changed();
            }
            _ => {}
        }
        Outcome::Handled
    }

    fn tree_key(&mut self, k: KeyEvent) -> Option<Outcome> {
        let t = &mut self.tree;
        let page = self.sidebar_rect.height.saturating_sub(2).max(1) as isize;
        match k.code {
            KeyCode::Esc => self.focus_tree = false,
            KeyCode::Up => t.move_by(-1),
            KeyCode::Down => t.move_by(1),
            KeyCode::PageUp => t.move_by(-page),
            KeyCode::PageDown => t.move_by(page),
            KeyCode::Home => t.move_by(isize::MIN / 2),
            KeyCode::End => t.move_by(isize::MAX / 2),
            KeyCode::Left => t.left(),
            KeyCode::Right => t.right(),
            KeyCode::Enter | KeyCode::F(4) => {
                let n = t.selected_node()?.clone();
                if n.is_dir {
                    t.toggle(t.selected);
                } else {
                    self.focus_tree = false;
                    return Some(Outcome::Open(Target { path: n.path, at: None }));
                }
            }
            // Other keys with modifiers (Ctrl+S, Ctrl+Q, Alt+←…) still reach the editor.
            _ if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => return None,
            _ => {}
        }
        Some(Outcome::Handled)
    }

    /// Mouse in the sidebar or a popup. `None` = not ours.
    pub fn mouse(&mut self, m: MouseEvent) -> Option<Outcome> {
        let inside = |r: Rect| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        if self.popup.is_some() {
            let l = self.list_rect;
            match m.kind {
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                    let len = self.popup_len();
                    let p = self.popup.as_mut()?;
                    let d: isize = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                    p.selected = (p.selected as isize + d).clamp(0, len.saturating_sub(1) as isize) as usize;
                }
                MouseEventKind::Down(MouseButton::Left) if inside(l) => {
                    let len = self.popup_len();
                    let p = self.popup.as_mut()?;
                    let i = p.scroll + (m.row - l.y) as usize;
                    if i < len {
                        p.selected = i;
                        let t = self.popup_target()?;
                        self.close_popup();
                        return Some(Outcome::Open(t));
                    }
                }
                MouseEventKind::Down(_) if !inside(self.popup_rect) => self.close_popup(),
                _ => {}
            }
            return Some(Outcome::Handled);
        }
        let s = self.sidebar_rect;
        if !self.sidebar || !inside(s) {
            if matches!(m.kind, MouseEventKind::Down(_)) {
                self.focus_tree = false;
            }
            return None;
        }
        let rows_y = s.y + 1;
        match m.kind {
            MouseEventKind::ScrollDown => {
                let max = self.tree.rows.len().saturating_sub(1);
                self.tree.scroll = (self.tree.scroll + 3).min(max);
                self.tree.follow = false;
            }
            MouseEventKind::ScrollUp => {
                self.tree.scroll = self.tree.scroll.saturating_sub(3);
                self.tree.follow = false;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus_tree = true;
                if m.row < rows_y {
                    return Some(Outcome::Handled);
                }
                let i = self.tree.scroll + (m.row - rows_y) as usize;
                let Some(n) = self.tree.rows.get(i).cloned() else { return Some(Outcome::Handled) };
                self.tree.selected = i;
                if n.is_dir {
                    self.tree.toggle(i);
                } else {
                    self.focus_tree = false;
                    return Some(Outcome::Open(Target { path: n.path, at: None }));
                }
            }
            _ => {}
        }
        Some(Outcome::Handled)
    }
}

fn pop_word(s: &mut String) {
    while s.ends_with(|c: char| !c.is_alphanumeric()) {
        s.pop();
    }
    while s.ends_with(char::is_alphanumeric) {
        s.pop();
    }
}

// ---- glue: the project view's side of `App` ------------------------------------------------

impl App {
    /// Called once per loop turn before drawing.
    pub fn tick(&mut self) {
        self.project.poll();
    }

    /// Runs a project-view key; returns false when the project view didn't want it.
    pub fn project_key(&mut self, k: KeyEvent) -> bool {
        let d = &self.docs[self.active];
        let (sel, file) = (d.selected_text(), d.path.clone());
        match self.project.key(k, sel, file) {
            None => false,
            Some(o) => {
                self.project_outcome(o);
                true
            }
        }
    }

    pub fn project_mouse(&mut self, m: MouseEvent) -> bool {
        match self.project.mouse(m) {
            None => false,
            Some(o) => {
                self.project_outcome(o);
                true
            }
        }
    }

    fn project_outcome(&mut self, o: Outcome) {
        match o {
            Outcome::Handled => {
                if self.project.focus_tree {
                    self.message = Some("Project: ↑↓ move · →← unfold/fold · Enter open · Esc editor · Alt+1 hide".into());
                }
            }
            Outcome::Open(t) => self.open_target(t),
        }
    }

    /// Opens `t.path` in a tab (or switches to its tab) and goes to `t.at`.
    pub fn open_target(&mut self, t: Target) {
        let want = canonical(&t.path);
        let existing = self.docs.iter().position(|d| d.path.as_deref().is_some_and(|p| canonical(p) == want));
        let i = match existing {
            Some(i) => i,
            None => match Doc::open(&t.path) {
                Ok(d) => {
                    // A fresh, empty untitled tab gets replaced instead of piling up.
                    let cur = &self.docs[self.active];
                    if self.docs.len() == 1 && cur.path.is_none() && !cur.dirty() && cur.rope.len_chars() == 0 {
                        self.docs[0] = d;
                        0
                    } else {
                        self.docs.push(d);
                        self.docs.len() - 1
                    }
                }
                Err(e) => {
                    self.message = Some(format!("Couldn't open: {e:#}"));
                    return;
                }
            },
        };
        self.active = i;
        let d = &mut self.docs[i];
        if let Some((line, col, len)) = t.at {
            d.goto_line(line, col);
            if len > 0 {
                let s = d.cursor;
                d.select(s, (s + len).min(d.line_end(d.line())));
            }
        }
        self.project.note_opened(&want);
        self.project.focus_tree = false;
        self.message = Some(self.project.rel(&want));
        self.follow = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_suffix() {
        assert_eq!(split_line_suffix("app.rs"), ("app.rs", None));
        assert_eq!(split_line_suffix("app.rs:42"), ("app.rs", Some((42, 1))));
        assert_eq!(split_line_suffix("app.rs:42:7"), ("app.rs", Some((42, 7))));
        assert_eq!(split_line_suffix("a:b"), ("a:b", None));
    }

    #[test]
    fn args_split_into_root_and_files() {
        let root = tree::tests::sample();
        let (r, files, proj) = split_args(vec![root.join("README.md"), root.clone()]);
        assert_eq!(r, canonical(&root));
        assert_eq!(files, vec![root.join("README.md")]);
        assert!(proj);
        let (_, files, proj) = split_args(vec![root.join("README.md")]);
        assert!(!proj && files.len() == 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recent_moves_to_front_without_duplicates() {
        let root = tree::tests::sample();
        let mut p = Project::new(canonical(&root), true);
        p.note_opened(&root.join("README.md"));
        p.note_opened(&root.join("src/main.rs"));
        p.note_opened(&root.join("README.md"));
        let rel: Vec<String> = p.recent.iter().map(|r| p.rel(r)).collect();
        assert_eq!(rel, vec!["README.md", "src/main.rs"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
