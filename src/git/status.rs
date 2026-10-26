//! `git status --porcelain=v1 -z --branch` parsing.

/// What happened to a file, as the panel, tree and colours see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
    Ignored,
}

impl Kind {
    pub fn letter(self) -> char {
        match self {
            Kind::Modified => 'M',
            Kind::Added => 'A',
            Kind::Deleted => 'D',
            Kind::Renamed => 'R',
            Kind::Untracked => '?',
            Kind::Conflict => 'U',
            Kind::Ignored => '!',
        }
    }
}

/// One changed path, relative to the repository root (forward slashes, as git prints it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    /// The old path of a rename or copy.
    pub orig: Option<String>,
    /// Index (staged) and work-tree status letters.
    pub x: char,
    pub y: char,
}

impl Entry {
    /// Something of this path is in the index (staged).
    pub fn staged(&self) -> bool {
        !matches!(self.x, ' ' | '?' | '!')
    }

    /// Staged changes *and* more changes in the working tree (e.g. some hunks staged).
    pub fn partial(&self) -> bool {
        self.staged() && self.kind() != Kind::Conflict && matches!(self.y, 'M' | 'D' | 'T')
    }

    pub fn kind(&self) -> Kind {
        let (x, y) = (self.x, self.y);
        match (x, y) {
            ('?', _) => Kind::Untracked,
            ('!', _) => Kind::Ignored,
            ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U') => Kind::Conflict,
            _ if x == 'R' || y == 'R' || x == 'C' => Kind::Renamed,
            ('A', _) => Kind::Added,
            _ if x == 'D' || y == 'D' => Kind::Deleted,
            _ => Kind::Modified,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// None when detached (the caller shows the short commit instead).
    pub branch: Option<String>,
    pub entries: Vec<Entry>,
}

/// Parses `git status --porcelain=v1 -z --branch` output.
pub fn parse(out: &str) -> Status {
    let mut st = Status::default();
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        if let Some(head) = f.strip_prefix("## ") {
            st.branch = parse_branch(head);
            continue;
        }
        if f.len() < 4 {
            continue;
        }
        let mut chars = f.chars();
        let x = chars.next().unwrap_or(' ');
        let y = chars.next().unwrap_or(' ');
        let path = f[3..].to_string();
        // Renames and copies are followed by their source path as its own field.
        let orig = if x == 'R' || x == 'C' || y == 'R' || y == 'C' { fields.next().map(str::to_string) } else { None };
        st.entries.push(Entry { path, orig, x, y });
    }
    st.entries.sort_by(|a, b| a.path.cmp(&b.path));
    st
}

fn parse_branch(head: &str) -> Option<String> {
    for prefix in ["No commits yet on ", "Initial commit on "] {
        if let Some(b) = head.strip_prefix(prefix) {
            return Some(b.trim().to_string());
        }
    }
    if head.starts_with("HEAD (no branch)") {
        return None;
    }
    let name = head.split("...").next().unwrap_or(head);
    let name = name.split(' ').next().unwrap_or(name);
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_entries() {
        let out = "## main...origin/main [ahead 1]\0 M src/app.rs\0A  new.rs\0?? notes.txt\0R  b.rs\0a.rs\0 D gone.rs\0UU both.rs\0";
        let st = parse(out);
        assert_eq!(st.branch.as_deref(), Some("main"));
        let kinds: Vec<(&str, Kind)> = st.entries.iter().map(|e| (e.path.as_str(), e.kind())).collect();
        assert_eq!(
            kinds,
            vec![
                ("b.rs", Kind::Renamed),
                ("both.rs", Kind::Conflict),
                ("gone.rs", Kind::Deleted),
                ("new.rs", Kind::Added),
                ("notes.txt", Kind::Untracked),
                ("src/app.rs", Kind::Modified),
            ]
        );
        assert_eq!(st.entries[0].orig.as_deref(), Some("a.rs"));
    }

    #[test]
    fn branch_header_variants() {
        assert_eq!(parse("## No commits yet on main\0").branch.as_deref(), Some("main"));
        assert_eq!(parse("## feature/x\0").branch.as_deref(), Some("feature/x"));
        assert_eq!(parse("## HEAD (no branch)\0").branch, None);
    }

    #[test]
    fn paths_with_spaces_and_staged_and_unstaged() {
        let st = parse("## main\0MM my file.rs\0AM added.rs\0");
        assert_eq!(st.entries[0].path, "added.rs");
        assert_eq!(st.entries[0].kind(), Kind::Added);
        assert_eq!(st.entries[1].path, "my file.rs");
        assert_eq!(st.entries[1].kind(), Kind::Modified);
        assert!(st.entries[1].partial() && st.entries[0].partial());
        let st = parse("## main\0M  staged.rs\0 M wt.rs\0?? new.rs\0");
        assert!(st.entries.iter().all(|e| !e.partial()));
        assert_eq!(st.entries.iter().map(Entry::staged).collect::<Vec<_>>(), vec![false, true, false]);
    }
}
