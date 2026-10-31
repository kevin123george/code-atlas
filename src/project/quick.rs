//! Search Everywhere (quick search, decision 0009): one popup that finds file names and file
//! contents at the same time, and the ways it works with the project tree.
//!
//! - The popup is `PopupKind::Quick`. File names are ranked with the finder's scorer
//!   (`Project::refilter`), contents come from the find-in-files job (`Project::start_search`),
//!   so typing never waits for the disk and a new query drops the old search.
//! - Rows: a **Files** section (at most [`ALL_FILES`] in the "All" tab), then a **Text** section
//!   with `path:line  snippet` rows as they stream in. Tab / Shift+Tab switch All → Files → Text.
//! - Scope: started from the focused tree, the search covers the selected folder only (shown
//!   in the title); Backspace on an empty input, or the key again, widens it to the project.
//! - Tree follow: while a popup row is selected, the tree (when shown) expands down to that file
//!   and selects it without taking the focus; Esc puts the tree back as it was, opening a file
//!   keeps it. Alt+F1 in the popup reveals the row in the tree and focuses it.
//! - Speed search (in the focused tree): typing filters/jumps by name (`Tree::speed_*`); when
//!   no open row matches, the best file name from the index is revealed.

use std::path::{Path, PathBuf};

use super::search::Hit;
use super::tree::Node;
use super::{fuzzy, split_line_suffix, PopupKind, Project, Target};

/// File-name rows shown in the "All" tab (the "Files" tab shows them all).
pub const ALL_FILES: usize = 8;

/// The popup's tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    All,
    Files,
    Text,
}

impl Mode {
    pub const TABS: [Mode; 3] = [Mode::All, Mode::Files, Mode::Text];

    pub fn label(self) -> &'static str {
        match self {
            Mode::All => "All",
            Mode::Files => "Files",
            Mode::Text => "Text",
        }
    }

    /// Tab: All → Files → Text → All (Shift+Tab backwards).
    pub fn next(self, back: bool) -> Mode {
        let i = Mode::TABS.iter().position(|m| *m == self).unwrap_or(0);
        Mode::TABS[(i + if back { 2 } else { 1 }) % 3]
    }
}

/// State of the quick-search popup that outlives one keystroke.
#[derive(Default)]
pub struct Quick {
    pub mode: Mode,
    /// Folder the search is limited to, relative to the root (`None` = the whole project).
    pub scope: Option<String>,
}

/// One selectable row of the popup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    /// Index into `Project::matches`.
    File(usize),
    /// Index into `Project::hits`.
    Hit(usize),
}

/// A drawn row: a section header or an item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Header(Section),
    Item(Item),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Files,
    Text,
}

/// Whether `path` (relative) lies in the folder `scope` (relative).
pub fn in_scope(path: &str, scope: &str) -> bool {
    path.strip_prefix(scope).is_some_and(|r| r.starts_with('/'))
}

/// The selectable rows for `mode`: file names first, then content hits.
pub fn items(mode: Mode, files: usize, hits: usize) -> Vec<Item> {
    let files = match mode {
        Mode::All => files.min(ALL_FILES),
        Mode::Files => files,
        Mode::Text => 0,
    };
    let hits = if mode == Mode::Files { 0 } else { hits };
    (0..files).map(Item::File).chain((0..hits).map(Item::Hit)).collect()
}

/// The rows as drawn: a header before each section that has something to say. The Text header
/// also shows while a search runs or found nothing, so it's clear content is searched too.
pub fn rows(mode: Mode, files: usize, hits: usize, text_header: bool) -> Vec<Row> {
    let its = items(mode, files, hits);
    let mut out = Vec::with_capacity(its.len() + 2);
    let nfiles = its.iter().filter(|i| matches!(i, Item::File(_))).count();
    if nfiles > 0 {
        out.push(Row::Header(Section::Files));
    }
    for (k, it) in its.iter().enumerate() {
        if k == nfiles && mode != Mode::Files {
            out.push(Row::Header(Section::Text));
        }
        out.push(Row::Item(*it));
    }
    if nfiles == its.len() && mode != Mode::Files && text_header {
        out.push(Row::Header(Section::Text));
    }
    out
}

/// The best file for a tree speed search that matched no open row: the index file whose *name*
/// scores best (shorter paths win ties).
pub fn best_by_name<'a>(q: &str, files: impl Iterator<Item = &'a String>) -> Option<&'a String> {
    let mut best: Option<(i32, usize, &String)> = None;
    for f in files {
        let name = f.rsplit('/').next().unwrap_or(f);
        if let Some((s, _)) = fuzzy::score(q, name) {
            let better = match best {
                None => true,
                Some((bs, bl, _)) => s > bs || (s == bs && f.len() < bl),
            };
            if better {
                best = Some((s, f.len(), f));
            }
        }
    }
    best.map(|(_, _, f)| f)
}

/// The tree as it was when a popup opened, put back when it's cancelled.
pub struct TreeSnap {
    rows: Vec<Node>,
    selected: usize,
    scroll: usize,
}

impl Project {
    pub fn quick_items(&self) -> Vec<Item> {
        items(self.quick.mode, self.matches.len(), self.hits.len())
    }

    /// Whether the Text section has a header even with no hits (a query long enough to search).
    pub fn quick_text_header(&self) -> bool {
        self.popup.as_ref().is_some_and(|p| p.input.trim().chars().count() >= 2)
    }

    pub fn quick_rows(&self) -> Vec<Row> {
        rows(self.quick.mode, self.matches.len(), self.hits.len(), self.quick_text_header())
    }

    /// The selected quick-search row as a file to open.
    pub(super) fn quick_target(&self) -> Option<Target> {
        let p = self.popup.as_ref()?;
        match *self.quick_items().get(p.selected)? {
            Item::File(i) => {
                let m = self.matches.get(i)?;
                let at = split_line_suffix(p.input.trim()).1.map(|(l, c)| (l.saturating_sub(1), c.saturating_sub(1), 0));
                Some(Target { path: self.root.join(&m.path), at })
            }
            Item::Hit(i) => Some(hit_target(&self.root, self.hits.get(i)?)),
        }
    }

    /// Opens the quick-search popup, scoped to `scope` (a folder relative to the root) or the
    /// whole project.
    pub fn open_quick(&mut self, seed: Option<String>, scope: Option<String>) {
        self.quick.scope = scope.filter(|s| !s.is_empty());
        self.quick.mode = Mode::All;
        self.open_popup(PopupKind::Quick, seed);
    }

    /// The folder to scope a search started from the focused tree to: the selected folder, or
    /// the selected file's folder (`None` at the root).
    pub fn tree_scope(&self) -> Option<String> {
        let n = self.tree.selected_node()?;
        let dir = if n.is_dir { n.path.clone() } else { n.path.parent()?.to_path_buf() };
        let rel = self.rel(&dir);
        (!rel.is_empty() && !Path::new(&rel).is_absolute()).then_some(rel)
    }

    /// Drops the scope: search the whole project again.
    pub(super) fn widen_quick(&mut self) {
        if self.quick.scope.take().is_some() {
            self.matches_stale = true;
            self.input_changed();
            self.search_due = Some(std::time::Instant::now().checked_sub(super::SEARCH_DEBOUNCE).unwrap_or_else(std::time::Instant::now));
        }
    }

    pub(super) fn snap_tree(&mut self) {
        if self.tree_snap.is_none() {
            let t = &self.tree;
            self.tree_snap = Some(TreeSnap { rows: t.rows.clone(), selected: t.selected, scroll: t.scroll });
            self.followed = None;
        }
    }

    /// Ends tree following: `restore` puts the tree back as it was before the popup.
    pub(super) fn end_follow(&mut self, restore: bool) {
        let snap = self.tree_snap.take();
        if let (true, Some(_), Some(s)) = (restore, self.followed.take(), snap) {
            self.tree.rows = s.rows;
            self.tree.selected = s.selected;
            self.tree.scroll = s.scroll;
            self.tree.follow = true;
        }
        self.followed = None;
    }

    /// The tree follows the popup's selected row (IDEA "autoscroll to source"): it expands the
    /// folders down to the file and selects it, without taking the focus. Each step starts from
    /// the tree as it was before the popup, so browsing doesn't leave folders open behind it.
    pub fn follow_tree(&mut self) {
        if !self.sidebar || self.popup.is_none() || self.tree_snap.is_none() {
            return;
        }
        let want: Option<PathBuf> = self.popup_target().map(|t| t.path).or_else(|| {
            // Nothing selected: show the scope folder, if any.
            let scoped = self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Quick);
            self.quick.scope.as_ref().filter(|_| scoped).map(|s| self.root.join(s))
        });
        if want == self.followed {
            return;
        }
        if let Some(s) = &self.tree_snap {
            self.tree.rows = s.rows.clone();
            self.tree.selected = s.selected;
            self.tree.scroll = s.scroll;
            self.tree.follow = true;
        }
        if let Some(path) = &want {
            self.tree.reveal(path);
        }
        self.followed = want;
    }

    /// Alt+F1 in a popup: reveal the selected row in the tree and focus it (the popup closes).
    pub(super) fn reveal_selected(&mut self) {
        let Some(t) = self.popup_target() else { return };
        self.close_popup_keep();
        self.sidebar = true;
        self.focus_tree = self.tree.reveal(&t.path);
    }

    // ---- tree speed search ------------------------------------------------------------------

    /// A character typed in the focused tree: narrows the speed search and jumps to the best
    /// open row whose name matches; with none, reveals the best file from the index.
    pub(super) fn speed_type(&mut self, c: char) {
        self.tree.speed.push(c);
        self.speed_refresh();
    }

    pub(super) fn speed_back(&mut self) {
        self.tree.speed.pop();
        if !self.tree.speed.is_empty() {
            self.speed_refresh();
        }
    }

    fn speed_refresh(&mut self) {
        if self.tree.speed_jump() {
            return;
        }
        let q = self.tree.speed.clone();
        if let Some(f) = best_by_name(&q, self.files.iter()).cloned() {
            let path = self.root.join(f);
            self.tree.reveal(&path);
        }
    }
}

/// Where a content hit opens: its line and column, with the match selected.
pub fn hit_target(root: &Path, h: &Hit) -> Target {
    Target { path: root.join(&h.path), at: Some((h.line, h.start, h.end - h.start)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{canonical, tree};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};

    fn key(p: &mut Project, code: KeyCode, m: KeyModifiers) -> Option<super::super::Outcome> {
        p.key(KeyEvent::new(code, m), None, None)
    }

    fn typ(p: &mut Project, s: &str) {
        for c in s.chars() {
            key(p, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    fn ready(root: &Path) -> Project {
        let mut p = Project::new(canonical(root), true);
        let t = Instant::now();
        while p.indexing() {
            assert!(t.elapsed() < Duration::from_secs(10));
            p.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        p
    }

    /// Polls until the content search is done.
    fn settle(p: &mut Project) {
        let t = Instant::now();
        loop {
            p.poll();
            if !p.searching() {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn keys_work_on_a_mac_without_setup() {
        use crate::project::is_quick_key;
        let k = |c, m| KeyEvent::new(c, m);
        assert!(is_quick_key(&k(KeyCode::Char('t'), KeyModifiers::CONTROL)), "Ctrl+T: every terminal");
        assert!(is_quick_key(&k(KeyCode::Char('s'), KeyModifiers::ALT)));
        assert!(is_quick_key(&k(KeyCode::Char(' '), KeyModifiers::ALT)));
        assert!(is_quick_key(&k(KeyCode::Char('\u{a0}'), KeyModifiers::NONE)), "Option+Space without Option as Meta");
        assert!(!is_quick_key(&k(KeyCode::Char(' '), KeyModifiers::CONTROL)), "Ctrl+Space is completion");
        assert!(!is_quick_key(&k(KeyCode::Char('s'), KeyModifiers::CONTROL)), "Ctrl+S saves");
        assert!(!is_quick_key(&k(KeyCode::Char('ß'), KeyModifiers::NONE)), "a German key, typed for real");
    }

    #[test]
    fn modes_cycle_both_ways() {
        assert_eq!(Mode::All.next(false), Mode::Files);
        assert_eq!(Mode::Text.next(false), Mode::All);
        assert_eq!(Mode::All.next(true), Mode::Text);
    }

    #[test]
    fn items_put_files_first_and_cap_them_in_all() {
        let it = items(Mode::All, 20, 3);
        assert_eq!(it.len(), ALL_FILES + 3);
        assert_eq!(it[0], Item::File(0));
        assert_eq!(it[ALL_FILES], Item::Hit(0));
        assert_eq!(items(Mode::Files, 20, 3).len(), 20);
        assert_eq!(items(Mode::Text, 20, 3), vec![Item::Hit(0), Item::Hit(1), Item::Hit(2)]);
    }

    #[test]
    fn rows_have_section_headers() {
        use Row::*;
        let r = rows(Mode::All, 2, 1, true);
        assert_eq!(r, vec![Header(Section::Files), Item(super::Item::File(0)), Item(super::Item::File(1)), Header(Section::Text), Item(super::Item::Hit(0))]);
        // Text header while searching / nothing found yet; none for a one-letter query.
        assert_eq!(rows(Mode::All, 1, 0, true).last(), Some(&Header(Section::Text)));
        assert_eq!(rows(Mode::All, 1, 0, false).len(), 2);
        assert_eq!(rows(Mode::Text, 5, 0, true), vec![Header(Section::Text)]);
        assert!(!rows(Mode::Files, 1, 4, true).contains(&Header(Section::Text)));
    }

    #[test]
    fn scope_is_a_folder_prefix() {
        assert!(in_scope("src/main.rs", "src"));
        assert!(in_scope("src/nested/deep.rs", "src/nested"));
        assert!(!in_scope("srcx/main.rs", "src"));
        assert!(!in_scope("src", "src"));
    }

    #[test]
    fn best_by_name_scores_names_not_folders() {
        let files: Vec<String> = ["deep/main/x.rs", "src/main.rs", "very/long/path/main.rs"].iter().map(|s| s.to_string()).collect();
        assert_eq!(best_by_name("main", files.iter()).map(String::as_str), Some("src/main.rs"));
        assert_eq!(best_by_name("zzz", files.iter()), None);
    }

    #[test]
    fn mixes_file_names_and_contents() {
        let root = tree::tests::sample();
        let mut p = ready(&root);
        key(&mut p, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(p.popup.as_ref().map(|x| x.kind), Some(PopupKind::Quick));
        typ(&mut p, "main");
        settle(&mut p);
        // "main" is a file name (src/main.rs) and text in it (fn main).
        let its = p.quick_items();
        assert_eq!(its[0], Item::File(0));
        assert_eq!(p.matches[0].path, "src/main.rs");
        let hit = its.iter().find_map(|i| match i { Item::Hit(h) => Some(*h), _ => None }).expect("a content hit");
        assert_eq!(p.hits[hit].path, "src/main.rs");
        assert_eq!(p.hits[hit].line, 0);
        // Enter on the first row opens the file; on the hit, at the match.
        assert_eq!(p.popup_target().unwrap().path, p.root.join("src/main.rs"));
        let n = its.iter().position(|i| matches!(i, Item::Hit(_))).unwrap();
        p.popup.as_mut().unwrap().selected = n;
        assert_eq!(p.popup_target().unwrap().at, Some((0, 3, 4)));
        // Tab narrows to text only, then files only... and back.
        key(&mut p, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(p.quick.mode, Mode::Files);
        assert!(p.quick_items().iter().all(|i| matches!(i, Item::File(_))));
        key(&mut p, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(p.quick.mode, Mode::Text);
        assert!(p.quick_items().iter().all(|i| matches!(i, Item::Hit(_))));
        key(&mut p, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(p.quick.mode, Mode::Files);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn line_suffix_and_ignored_files() {
        let root = tree::tests::sample();
        let mut p = ready(&root);
        p.open_quick(None, None);
        typ(&mut p, "deep:1:4");
        settle(&mut p);
        assert_eq!(p.matches[0].path, "src/nested/deep.rs");
        assert_eq!(p.popup_target().unwrap().at, Some((0, 3, 0)));
        // Ignored places stay out of both sections.
        key(&mut p, KeyCode::Char('u'), KeyModifiers::CONTROL);
        typ(&mut p, "hello");
        settle(&mut p);
        let paths: Vec<&str> = p.hits.iter().map(|h| h.path.as_str()).collect();
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|x| !x.contains("secret") && !x.ends_with(".log") && !x.starts_with("target")), "{paths:?}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scoped_search_from_the_tree() {
        let root = tree::tests::sample();
        let mut p = ready(&root);
        p.focus_tree = true;
        assert!(p.tree.reveal(&p.root.join("src/nested/deep.rs")));
        assert_eq!(p.tree_scope().as_deref(), Some("src/nested"));
        p.tree.selected = 0;
        assert_eq!(p.tree_scope(), None, "the root is the whole project");
        p.tree.reveal(&p.root.join("src"));
        key(&mut p, KeyCode::Char(' '), KeyModifiers::ALT);
        assert_eq!(p.quick.scope.as_deref(), Some("src"));
        typ(&mut p, "hello");
        settle(&mut p);
        let mut paths: Vec<&str> = p.hits.iter().map(|h| h.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["src/lib.rs", "src/main.rs", "src/nested/deep.rs"], "smart case: lowercase finds Hello too, inside src only, paths from the root");
        assert!(p.matches.iter().all(|m| m.path.starts_with("src/")));
        // Backspace on an empty input widens to the whole project.
        key(&mut p, KeyCode::Char('u'), KeyModifiers::CONTROL);
        key(&mut p, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(p.quick.scope, None);
        typ(&mut p, "hello");
        settle(&mut p);
        assert!(p.hits.iter().any(|h| h.path == "README.md"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tree_follows_the_selection_and_esc_restores_it() {
        let root = tree::tests::sample();
        let mut p = ready(&root);
        let before = p.tree.rows.len();
        p.open_quick(None, None);
        typ(&mut p, "deep");
        settle(&mut p);
        let n = p.tree.selected_node().unwrap();
        assert_eq!(n.name, "deep.rs", "the tree shows the selected result");
        assert!(!p.focus_tree, "without taking the focus");
        assert!(p.tree.rows.len() > before);
        key(&mut p, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(p.tree.rows.len(), before, "Esc puts the tree back");
        assert_eq!(p.tree.selected, 0);
        // Enter keeps it (the opened file stays shown).
        p.open_quick(None, None);
        typ(&mut p, "deep");
        settle(&mut p);
        assert!(matches!(key(&mut p, KeyCode::Enter, KeyModifiers::NONE), Some(super::super::Outcome::Open(_))));
        assert_eq!(p.tree.selected_node().unwrap().name, "deep.rs");
        // Alt+F1 reveals and focuses.
        p.focus_tree = false;
        p.open_quick(None, None);
        typ(&mut p, "lib");
        settle(&mut p);
        key(&mut p, KeyCode::F(1), KeyModifiers::ALT);
        assert!(p.popup.is_none() && p.focus_tree);
        assert_eq!(p.tree.selected_node().unwrap().name, "lib.rs");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tree_doesnt_follow_when_hidden() {
        let root = tree::tests::sample();
        let mut p = ready(&root);
        p.sidebar = false;
        let before = p.tree.rows.len();
        p.open_quick(None, None);
        typ(&mut p, "deep");
        settle(&mut p);
        assert_eq!(p.tree.rows.len(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn speed_search_jumps_highlights_and_reveals() {
        let root = tree::tests::sample();
        let mut p = ready(&root);
        p.focus_tree = true;
        typ(&mut p, "rea");
        assert_eq!(p.tree.speed, "rea");
        assert_eq!(p.tree.selected_node().unwrap().name, "README.md");
        assert!(p.tree.speed_match(p.tree.selected).is_some());
        // Not among the open rows: the best file name from the index is revealed.
        key(&mut p, KeyCode::Esc, KeyModifiers::NONE);
        assert!(p.tree.speed.is_empty() && p.focus_tree, "Esc clears the search first");
        typ(&mut p, "deep");
        assert_eq!(p.tree.selected_node().unwrap().name, "deep.rs");
        // Backspace narrows back; ↓ goes to the next match.
        key(&mut p, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(p.tree.speed, "dee");
        key(&mut p, KeyCode::Esc, KeyModifiers::NONE);
        typ(&mut p, "rs");
        let first = p.tree.selected;
        key(&mut p, KeyCode::Down, KeyModifiers::NONE);
        assert_ne!(p.tree.selected, first);
        assert!(p.tree.speed_match(p.tree.selected).is_some(), "↓ lands on a match");
        // Enter opens the file and clears the search.
        let out = key(&mut p, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(out, Some(super::super::Outcome::Open(_))));
        assert!(p.tree.speed.is_empty());
        key(&mut p, KeyCode::Esc, KeyModifiers::NONE);
        std::fs::remove_dir_all(root).unwrap();
    }
}
