//! The project tree as a flat list of visible rows: expanding a folder inserts its children after
//! it, collapsing removes them. Folders are listed lazily, with `.gitignore` respected.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Always hidden, ignored or not.
pub const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];
const SKIP_FILES: &[&str] = &[".DS_Store"];

pub fn skip_name(name: &str, is_dir: bool) -> bool {
    // `.git` is a file in worktrees and submodules, so it goes whatever it is.
    name == ".git" || if is_dir { SKIP_DIRS.contains(&name) } else { SKIP_FILES.contains(&name) }
}

/// A walker over `dir` that respects `.gitignore` (also outside git repos), shows dotfiles and
/// skips [`SKIP_DIRS`].
pub fn walker(dir: &Path) -> WalkBuilder {
    let mut w = WalkBuilder::new(dir);
    w.hidden(false).require_git(false).follow_links(false).filter_entry(|e| {
        let is_dir = e.file_type().is_some_and(|t| t.is_dir());
        !skip_name(&e.file_name().to_string_lossy(), is_dir)
    });
    w
}

#[derive(Debug, Clone)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

pub struct Tree {
    pub rows: Vec<Node>,
    pub selected: usize,
    pub scroll: usize,
    /// Keep the selected row in view at the next draw (keyboard moves do, the wheel doesn't).
    pub follow: bool,
}

/// The children of `dir`: folders first, then files, each sorted case-insensitively.
pub fn list_dir(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out: Vec<(String, PathBuf, bool)> = walker(dir)
        .max_depth(Some(1))
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            (e.file_name().to_string_lossy().into_owned(), e.into_path(), is_dir)
        })
        .collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())).then_with(|| a.0.cmp(&b.0)));
    out
}

impl Tree {
    /// A tree with `root` as its single, expanded top row.
    pub fn new(root: &Path) -> Self {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        let mut t = Tree {
            rows: vec![Node { path: root.to_path_buf(), name, is_dir: true, depth: 0, expanded: false }],
            selected: 0,
            scroll: 0,
            follow: true,
        };
        t.expand(0);
        t
    }

    pub fn selected_node(&self) -> Option<&Node> {
        self.rows.get(self.selected)
    }

    /// End (exclusive) of the rows below `i` that are its descendants.
    fn subtree_end(&self, i: usize) -> usize {
        let d = self.rows[i].depth;
        let mut e = i + 1;
        while e < self.rows.len() && self.rows[e].depth > d {
            e += 1;
        }
        e
    }

    /// Lists and shows the children of folder row `i` (re-read each time, so new files appear).
    pub fn expand(&mut self, i: usize) {
        let Some(n) = self.rows.get(i) else { return };
        if !n.is_dir || n.expanded {
            return;
        }
        let depth = n.depth + 1;
        let kids: Vec<Node> = list_dir(&n.path)
            .into_iter()
            .map(|(name, path, is_dir)| Node { path, name, is_dir, depth, expanded: false })
            .collect();
        self.rows[i].expanded = true;
        self.rows.splice(i + 1..i + 1, kids);
    }

    pub fn collapse(&mut self, i: usize) {
        if !self.rows.get(i).is_some_and(|n| n.expanded) {
            return;
        }
        let end = self.subtree_end(i);
        self.rows.drain(i + 1..end);
        self.rows[i].expanded = false;
        if self.selected > i && self.selected < end {
            self.selected = i;
        } else if self.selected >= end {
            self.selected -= end - i - 1;
        }
    }

    pub fn toggle(&mut self, i: usize) {
        if self.rows.get(i).is_some_and(|n| n.expanded) {
            self.collapse(i);
        } else {
            self.expand(i);
        }
    }

    pub fn parent(&self, i: usize) -> Option<usize> {
        let d = self.rows.get(i)?.depth;
        (0..i).rev().find(|&k| self.rows[k].depth < d)
    }

    pub fn move_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.follow = true;
    }

    /// → : open a closed folder, or step into an open one.
    pub fn right(&mut self) {
        let i = self.selected;
        match self.rows.get(i) {
            Some(n) if n.is_dir && !n.expanded => self.expand(i),
            Some(n) if n.is_dir && i + 1 < self.rows.len() && self.rows[i + 1].depth > n.depth => self.selected = i + 1,
            _ => {}
        }
        self.follow = true;
    }

    /// ← : close an open folder, or step out to the parent.
    pub fn left(&mut self) {
        let i = self.selected;
        if self.rows.get(i).is_some_and(|n| n.expanded) {
            self.collapse(i);
        } else if let Some(p) = self.parent(i) {
            self.selected = p;
        }
        self.follow = true;
    }

    /// Expands the folders down to `path` (inside the root) and selects it.
    pub fn reveal(&mut self, path: &Path) -> bool {
        let mut i = 0;
        loop {
            let n = &self.rows[i];
            if n.path == path {
                self.selected = i;
                self.follow = true;
                return true;
            }
            if !n.is_dir || !path.starts_with(&n.path) {
                return false;
            }
            self.expand(i);
            let d = self.rows[i].depth + 1;
            let end = self.subtree_end(i);
            match (i + 1..end).find(|&k| self.rows[k].depth == d && path.starts_with(&self.rows[k].path)) {
                Some(k) => i = k,
                None => return false,
            }
        }
    }

    /// Scroll so the selection is visible in `h` rows.
    pub fn keep_visible(&mut self, h: usize) {
        if h == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + h {
            self.scroll = self.selected + 1 - h;
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::fs;

    /// A throwaway project folder: `src/{main.rs,lib.rs}`, `docs/a.md`, `README.md`,
    /// `.gitignore` (ignores `*.log` and `secret/`), plus things that must stay hidden.
    pub fn sample() -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("catlas-test-{}-{n}", std::process::id()));
        for d in ["src/nested", "docs", "target/debug", "node_modules/x", ".git", "secret"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for (f, body) in [
            ("src/main.rs", "fn main() {\n    println!(\"hello\");\n}\n"),
            ("src/lib.rs", "pub fn Hello() {}\n"),
            ("src/nested/deep.rs", "// hello again\n"),
            ("docs/a.md", "# Docs\n"),
            ("README.md", "Hello world\n"),
            (".gitignore", "*.log\nsecret/\n"),
            ("debug.log", "hello log\n"),
            ("secret/key.txt", "hello secret\n"),
            ("target/debug/out.rs", "hello target\n"),
            ("node_modules/x/i.js", "hello nm\n"),
            (".git/config", "hello git\n"),
            ("src/.git", "gitdir: elsewhere\n"),
        ] {
            fs::write(root.join(f), body).unwrap();
        }
        root
    }

    fn names(t: &Tree) -> Vec<String> {
        t.rows.iter().map(|n| format!("{}{}", "  ".repeat(n.depth), n.name)).collect()
    }

    #[test]
    fn lists_folders_first_and_hides_ignored() {
        let root = sample();
        let t = Tree::new(&root);
        let got: Vec<String> = names(&t).into_iter().skip(1).collect();
        assert_eq!(got, vec!["  docs", "  src", "  .gitignore", "  README.md"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_collapse_and_moves() {
        let root = sample();
        let mut t = Tree::new(&root);
        t.selected = 2; // src
        t.right(); // expand
        assert_eq!(names(&t)[3..6], ["    nested", "    lib.rs", "    main.rs"]);
        t.right(); // step into
        assert_eq!(t.selected, 3);
        t.move_by(2); // main.rs
        assert_eq!(t.selected_node().unwrap().name, "main.rs");
        t.left(); // to parent src
        assert_eq!(t.selected, 2);
        t.move_by(4); // .gitignore (below the open src)
        assert_eq!(t.selected_node().unwrap().name, ".gitignore");
        t.collapse(2);
        // Selection follows its row when rows above disappear.
        assert_eq!(t.selected_node().unwrap().name, ".gitignore");
        assert_eq!(t.rows.len(), 5);
        t.move_by(-100);
        assert_eq!(t.selected, 0);
        t.move_by(100);
        assert_eq!(t.selected, 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collapse_with_selection_inside_selects_folder() {
        let root = sample();
        let mut t = Tree::new(&root);
        t.expand(2);
        t.selected = 4;
        t.collapse(2);
        assert_eq!(t.selected, 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reveal_expands_down_to_a_file() {
        let root = sample();
        let mut t = Tree::new(&root);
        assert!(t.reveal(&root.join("src/nested/deep.rs")));
        assert_eq!(t.selected_node().unwrap().name, "deep.rs");
        assert_eq!(t.selected_node().unwrap().depth, 3);
        assert!(!t.reveal(&root.join("target/debug/out.rs")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keep_visible_scrolls() {
        let root = sample();
        let mut t = Tree::new(&root);
        t.selected = 4;
        t.keep_visible(2);
        assert_eq!(t.scroll, 3);
        t.selected = 0;
        t.keep_visible(2);
        assert_eq!(t.scroll, 0);
        fs::remove_dir_all(root).unwrap();
    }
}
