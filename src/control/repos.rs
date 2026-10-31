//! Repository state for the Control Center's workspace tile: branch, upstream, ahead / behind,
//! changed and untracked files, one `git status` per root on a background thread. Several roots
//! once multi-repo workspaces land (track A, decision 0013): see `control::hooks::roots`.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Repo {
    pub root: PathBuf,
    /// The folder's name (what the tile shows).
    pub name: String,
    /// `None` outside a git repository.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    /// Changed (staged or not) and untracked files.
    pub changed: usize,
    pub untracked: usize,
    pub conflicts: usize,
}

/// The `## …` line of `git status --porcelain=v1 --branch`: (branch, upstream, ahead, behind).
pub fn parse_header(line: &str) -> (Option<String>, Option<String>, usize, usize) {
    let h = line.trim_start_matches("## ").trim();
    if let Some(b) = h.strip_prefix("No commits yet on ").or_else(|| h.strip_prefix("Initial commit on ")) {
        return (Some(b.to_string()), None, 0, 0);
    }
    if h.starts_with("HEAD (no branch)") {
        return (Some("detached HEAD".into()), None, 0, 0);
    }
    let (names, counts) = match h.split_once(" [") {
        Some((n, c)) => (n, c.trim_end_matches(']')),
        None => (h, ""),
    };
    let (branch, upstream) = match names.split_once("...") {
        Some((b, u)) => (b.to_string(), Some(u.to_string())),
        None => (names.to_string(), None),
    };
    let mut ahead = 0;
    let mut behind = 0;
    for part in counts.split(", ") {
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    (Some(branch), upstream, ahead, behind)
}

/// Reads one root (blocking).
pub fn read(root: &Path) -> Repo {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
    let mut r = Repo { root: root.to_path_buf(), name, ..Repo::default() };
    let Ok(out) = crate::git::run(root, &["status", "--porcelain=v1", "--branch", "--untracked-files=normal"]) else { return r };
    for (i, line) in out.lines().enumerate() {
        if i == 0 && line.starts_with("## ") {
            (r.branch, r.upstream, r.ahead, r.behind) = parse_header(line);
        } else if line.starts_with("??") {
            r.untracked += 1;
        } else if line.len() >= 2 {
            let xy = &line[..2];
            if matches!(xy, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
                r.conflicts += 1;
            }
            r.changed += 1;
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        assert_eq!(parse_header("## main...forgejo/main [ahead 2, behind 1]"), (Some("main".into()), Some("forgejo/main".into()), 2, 1));
        assert_eq!(parse_header("## main...origin/main"), (Some("main".into()), Some("origin/main".into()), 0, 0));
        assert_eq!(parse_header("## feature"), (Some("feature".into()), None, 0, 0));
        assert_eq!(parse_header("## main...origin/main [behind 7]"), (Some("main".into()), Some("origin/main".into()), 0, 7));
        assert_eq!(parse_header("## No commits yet on main"), (Some("main".into()), None, 0, 0));
        assert_eq!(parse_header("## HEAD (no branch)").0.as_deref(), Some("detached HEAD"));
        assert_eq!(parse_header("## main...origin/main [gone]").2, 0);
    }

    #[test]
    fn reads_a_scratch_repo() {
        let d = crate::git::tests::scratch_repo("control-repo");
        std::fs::write(d.join("new.txt"), "x").unwrap();
        std::fs::write(d.join("a.txt"), "changed\n").unwrap();
        let r = read(&d);
        assert_eq!(r.branch.as_deref(), Some("main"));
        assert_eq!((r.changed, r.untracked, r.ahead), (1, 1, 0), "{r:?}");
        let none = read(&std::env::temp_dir().join("surely-not-a-repo-xyz"));
        assert_eq!(none.branch, None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
