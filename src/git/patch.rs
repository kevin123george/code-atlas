//! `git diff` output as files and hunks, and patches made from them: one hunk (stage, unstage,
//! revert with `git apply`) or a whole file. Also word-level change ranges for paired lines.
//!
//! Diffs are always asked for with `--no-color --no-ext-diff --no-textconv --no-renames
//! --src-prefix=a/ --dst-prefix=b/` and `core.quotePath=false` ([`DIFF_ARGS`]), so the output
//! is plain unified diff that `git apply` takes back, whatever the user's config says.

/// Flags for every `git diff` whose hunks may become patches.
pub const DIFF_ARGS: &[&str] = &["--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--src-prefix=a/", "--dst-prefix=b/", "-U3"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// The text without the prefix character and the line break (a `\r` of CRLF files stays).
    pub text: String,
    /// "\ No newline at end of file" followed this line.
    pub no_eol: bool,
    /// Old / new line numbers (1-based) where the side has the line.
    pub old: Option<usize>,
    pub new: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    /// The whole `@@ … @@ context` line.
    pub header: String,
    pub lines: Vec<Line>,
}

impl Hunk {
    pub fn added(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == LineKind::Added).count()
    }

    pub fn deleted(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == LineKind::Deleted).count()
    }

    /// First changed line on the new side (where F4 should land), 1-based: an added line, or
    /// for a deletion the line that now stands where it was.
    pub fn first_new_line(&self) -> usize {
        let mut last_new = 0;
        for (i, l) in self.lines.iter().enumerate() {
            if l.kind == LineKind::Context {
                last_new = l.new.unwrap_or(last_new);
                continue;
            }
            return self.lines[i..].iter().find_map(|x| x.new).unwrap_or(last_new + 1).max(1);
        }
        self.new_start.max(1)
    }
}

/// What happened to a file in this diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Modified,
    Added,
    Deleted,
    /// Not known to git (made up by the caller from the file's text).
    Untracked,
}

impl Status {
    pub fn letter(self) -> char {
        match self {
            Status::Modified => 'M',
            Status::Added => 'A',
            Status::Deleted => 'D',
            Status::Untracked => '?',
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// Path relative to the repository root (forward slashes).
    pub path: String,
    pub status: Status,
    /// Header lines from `diff --git` up to the first hunk, as git printed them (for patches).
    pub head: Vec<String>,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// Only the file mode changed (no hunks, not binary).
    pub mode_only: bool,
}

impl FileDiff {
    pub fn added(&self) -> usize {
        self.hunks.iter().map(Hunk::added).sum()
    }

    pub fn deleted(&self) -> usize {
        self.hunks.iter().map(Hunk::deleted).sum()
    }

    /// The patch for hunk `i` alone (applies with `git apply [--cached] [-R]`).
    pub fn hunk_patch(&self, i: usize) -> String {
        let mut p = self.head_text();
        if let Some(h) = self.hunks.get(i) {
            push_hunk(&mut p, h);
        }
        p
    }

    /// The patch for the whole file.
    #[cfg(test)]
    pub fn patch(&self) -> String {
        let mut p = self.head_text();
        for h in &self.hunks {
            push_hunk(&mut p, h);
        }
        p
    }

    fn head_text(&self) -> String {
        let mut p = String::new();
        for l in &self.head {
            p.push_str(l);
            p.push('\n');
        }
        p
    }
}

fn push_hunk(p: &mut String, h: &Hunk) {
    p.push_str(&h.header);
    p.push('\n');
    for l in &h.lines {
        p.push(match l.kind {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Deleted => '-',
        });
        p.push_str(&l.text);
        p.push('\n');
        if l.no_eol {
            p.push_str("\\ No newline at end of file\n");
        }
    }
}

/// `@@ -a[,b] +c[,d] @@` → (a, b, c, d).
fn parse_range(header: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let end = rest.find(" @@")?;
    let (old, new) = rest[..end].split_once(" +")?;
    let one = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (a, b) = one(old)?;
    let (c, d) = one(new)?;
    Some((a, b, c, d))
}

/// Undoes git's C-style quoting of a path (`"a\tb"`, octal escapes for bytes).
pub fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return s.to_string() };
    let mut bytes = Vec::new();
    let mut it = inner.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'a') => bytes.push(7),
            Some(b'b') => bytes.push(8),
            Some(b'f') => bytes.push(12),
            Some(b'v') => bytes.push(11),
            Some(d @ b'0'..=b'7') => {
                let mut v = (d - b'0') as u32;
                for _ in 0..2 {
                    match it.peek() {
                        Some(&o @ b'0'..=b'7') => {
                            v = v * 8 + (o - b'0') as u32;
                            it.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(v as u8);
            }
            Some(o) => bytes.push(o),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The path from a `--- a/x` / `+++ b/x` line (None for /dev/null).
fn side_path(rest: &str, prefix: &str) -> Option<String> {
    let rest = rest.strip_suffix('\t').unwrap_or(rest);
    if rest == "/dev/null" {
        return None;
    }
    let p = unquote(rest);
    Some(p.strip_prefix(prefix).unwrap_or(&p).to_string())
}

/// The path from `diff --git a/x b/x` (same name on both sides: no renames are asked for).
fn git_line_path(rest: &str) -> String {
    if rest.starts_with('"') {
        // "a/x" "b/x"
        if let Some(end) = rest[1..].find("\" ") {
            let a = unquote(&rest[..end + 2]);
            return a.strip_prefix("a/").unwrap_or(&a).to_string();
        }
    }
    let n = rest.chars().count();
    if n >= 5 && (n - 5) % 2 == 0 {
        let l = (n - 5) / 2;
        let p: String = rest.chars().skip(2).take(l).collect();
        return p;
    }
    rest.trim_start_matches("a/").to_string()
}

/// Parses `git diff` output (see [`DIFF_ARGS`]).
pub fn parse(out: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut lines = out.split('\n').peekable();
    // (old, new) line counters inside the current hunk.
    let mut at = (0usize, 0usize);
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(FileDiff {
                path: git_line_path(rest),
                status: Status::Modified,
                head: vec![line.to_string()],
                hunks: Vec::new(),
                binary: false,
                mode_only: false,
            });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if f.hunks.is_empty() && !line.starts_with("@@") {
            // Header lines.
            if line.is_empty() && lines.peek().is_none() {
                break;
            }
            f.head.push(line.to_string());
            if line.starts_with("new file mode") {
                f.status = Status::Added;
            } else if line.starts_with("deleted file mode") {
                f.status = Status::Deleted;
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                f.binary = true;
            } else if let Some(r) = line.strip_prefix("--- ") {
                if let Some(p) = side_path(r, "a/") {
                    f.path = p;
                }
            } else if let Some(r) = line.strip_prefix("+++ ") {
                if let Some(p) = side_path(r, "b/") {
                    f.path = p;
                }
            }
            continue;
        }
        if line.starts_with("@@") {
            if let Some((a, b, c, d)) = parse_range(line) {
                f.hunks.push(Hunk { old_start: a, old_len: b, new_start: c, new_len: d, header: line.to_string(), lines: Vec::new() });
                at = (a.max(1), c.max(1));
                // An empty side starts at 0 in the header but its first line would be 1.
                if b == 0 {
                    at.0 = a + 1;
                }
                if d == 0 {
                    at.1 = c + 1;
                }
            }
            continue;
        }
        let Some(h) = f.hunks.last_mut() else { continue };
        if line.starts_with('\\') {
            if let Some(l) = h.lines.last_mut() {
                l.no_eol = true;
            }
            continue;
        }
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Deleted, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            _ => {
                if line.is_empty() && lines.peek().is_none() {
                    break;
                }
                // An empty context line some tools strip the space from.
                if line.is_empty() {
                    (LineKind::Context, "")
                } else {
                    continue;
                }
            }
        };
        let (old, new) = match kind {
            LineKind::Context => {
                at = (at.0 + 1, at.1 + 1);
                (Some(at.0 - 1), Some(at.1 - 1))
            }
            LineKind::Deleted => {
                at.0 += 1;
                (Some(at.0 - 1), None)
            }
            LineKind::Added => {
                at.1 += 1;
                (None, Some(at.1 - 1))
            }
        };
        h.lines.push(Line { kind, text: text.to_string(), no_eol: false, old, new });
    }
    for f in &mut files {
        f.mode_only = !f.binary && f.hunks.is_empty();
    }
    files
}

/// A made-up diff for a file git doesn't track yet: every line added.
pub fn untracked(path: &str, text: &str) -> FileDiff {
    let mut lines = Vec::new();
    let mut n = 0;
    let body = text.strip_suffix('\n').unwrap_or(text);
    if !text.is_empty() {
        for (i, l) in body.split('\n').enumerate() {
            n = i + 1;
            lines.push(Line { kind: LineKind::Added, text: l.to_string(), no_eol: false, old: None, new: Some(n) });
        }
        if !text.ends_with('\n') {
            if let Some(l) = lines.last_mut() {
                l.no_eol = true;
            }
        }
    }
    let hunks = if n > 0 { vec![Hunk { old_start: 0, old_len: 0, new_start: 1, new_len: n, header: format!("@@ -0,0 +1,{n} @@"), lines }] } else { Vec::new() };
    FileDiff {
        path: path.to_string(),
        status: Status::Untracked,
        head: vec![format!("diff --git a/{path} b/{path}"), "new file mode 100644".into(), "--- /dev/null".into(), format!("+++ b/{path}")],
        hunks,
        binary: false,
        mode_only: false,
    }
}

/// `git diff --numstat` → (files, added, deleted); binary files count as files only.
pub fn numstat(out: &str) -> (usize, usize, usize) {
    let (mut files, mut add, mut del) = (0, 0, 0);
    for l in out.lines() {
        let mut it = l.split('\t');
        let (Some(a), Some(d), Some(_)) = (it.next(), it.next(), it.next()) else { continue };
        files += 1;
        add += a.parse::<usize>().unwrap_or(0);
        del += d.parse::<usize>().unwrap_or(0);
    }
    (files, add, del)
}

/// "3 files +42 −7".
pub fn stat_label(files: usize, add: usize, del: usize) -> String {
    let f = if files == 1 { "1 file".to_string() } else { format!("{files} files") };
    format!("{f} +{add} −{del}")
}

// ---- word-level highlights ---------------------------------------------------------------------

/// Splits a line into words, runs of spaces and single other characters (char offsets).
fn tokens(s: &str) -> Vec<(usize, usize)> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        let class = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 };
        let k = class(c);
        let mut j = i + 1;
        if k != 2 {
            while j < cs.len() && class(cs[j]) == k {
                j += 1;
            }
        }
        out.push((i, j));
        i = j;
    }
    out
}

/// Changed character ranges in `old` and `new` (a changed line pair), or None when the lines
/// have too little in common for word highlights to help.
pub fn word_ranges(old: &str, new: &str) -> Option<(Vec<(usize, usize)>, Vec<(usize, usize)>)> {
    let (oc, nc): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let (ot, nt) = (tokens(old), tokens(new));
    if ot.len() > 400 || nt.len() > 400 {
        return None;
    }
    let ow: Vec<String> = ot.iter().map(|&(a, b)| oc[a..b].iter().collect()).collect();
    let nw: Vec<String> = nt.iter().map(|&(a, b)| nc[a..b].iter().collect()).collect();
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, &ow, &nw);
    let (mut ro, mut rn) = (Vec::new(), Vec::new());
    let mut same = 0usize;
    let push = |v: &mut Vec<(usize, usize)>, a: usize, b: usize| {
        if a >= b {
            return;
        }
        match v.last_mut() {
            Some(last) if last.1 == a => last.1 = b,
            _ => v.push((a, b)),
        }
    };
    for op in ops {
        match op {
            similar::DiffOp::Equal { old_index, len, .. } => {
                same += ot[old_index..old_index + len].iter().filter(|(a, b)| !oc[*a..*b].iter().all(|c| c.is_whitespace())).map(|(a, b)| b - a).sum::<usize>();
            }
            similar::DiffOp::Delete { old_index, old_len, .. } => push(&mut ro, ot[old_index].0, ot[old_index + old_len - 1].1),
            similar::DiffOp::Insert { new_index, new_len, .. } => push(&mut rn, nt[new_index].0, nt[new_index + new_len - 1].1),
            similar::DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                push(&mut ro, ot[old_index].0, ot[old_index + old_len - 1].1);
                push(&mut rn, nt[new_index].0, nt[new_index + new_len - 1].1);
            }
        }
    }
    let longest = oc.iter().filter(|c| !c.is_whitespace()).count().max(nc.iter().filter(|c| !c.is_whitespace()).count());
    // Less than a third in common: the whole line changed, highlights would be noise.
    if longest == 0 || same * 3 < longest {
        return None;
    }
    Some((ro, rn))
}

/// For each run of deleted lines followed by added lines, pairs them up (first with first …)
/// and returns (deleted index, added index) pairs, indices into `lines`.
pub fn change_pairs(lines: &[Line]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Deleted {
            i += 1;
            continue;
        }
        let d0 = i;
        while i < lines.len() && lines[i].kind == LineKind::Deleted {
            i += 1;
        }
        let a0 = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        for k in 0..(a0 - d0).min(i - a0) {
            pairs.push((d0 + k, a0 + k));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::run;
    use std::path::Path;

    const SAMPLE: &str = "diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,4 @@ fn main() {
 one
-two
+TWO
 three
 four
@@ -10,2 +10,3 @@
 ten
 eleven
+twelve
\\ No newline at end of file
diff --git a/new file.txt b/new file.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new file.txt\t
@@ -0,0 +1 @@
+hello
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/img.png b/img.png
index 5555555..6666666 100644
Binary files a/img.png and b/img.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
";

    #[test]
    fn parses_files_hunks_and_line_numbers() {
        let fs = parse(SAMPLE);
        let names: Vec<(&str, Status, bool)> = fs.iter().map(|f| (f.path.as_str(), f.status, f.binary)).collect();
        assert_eq!(
            names,
            vec![
                ("src/a.rs", Status::Modified, false),
                ("new file.txt", Status::Added, false),
                ("gone.txt", Status::Deleted, false),
                ("img.png", Status::Modified, true),
                ("run.sh", Status::Modified, false),
            ]
        );
        assert!(fs[4].mode_only && !fs[3].mode_only);
        let a = &fs[0];
        assert_eq!(a.hunks.len(), 2);
        assert_eq!((a.added(), a.deleted()), (2, 1));
        let h = &a.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (1, 4, 1, 4));
        assert_eq!(h.lines[1], Line { kind: LineKind::Deleted, text: "two".into(), no_eol: false, old: Some(2), new: None });
        assert_eq!(h.lines[2], Line { kind: LineKind::Added, text: "TWO".into(), no_eol: false, old: None, new: Some(2) });
        assert_eq!((h.lines[3].old, h.lines[3].new), (Some(3), Some(3)));
        assert_eq!(h.first_new_line(), 2);
        let h2 = &a.hunks[1];
        assert!(h2.lines[2].no_eol);
        assert_eq!(h2.lines[2].new, Some(12));
        assert_eq!(fs[1].hunks[0].lines[0].new, Some(1));
        assert_eq!(fs[2].hunks[0].lines[0].old, Some(1));
    }

    #[test]
    fn hunk_patch_keeps_the_header_and_no_eol_marker() {
        let fs = parse(SAMPLE);
        let p = fs[0].hunk_patch(1);
        assert_eq!(p, "diff --git a/src/a.rs b/src/a.rs\nindex 1111111..2222222 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,2 +10,3 @@\n ten\n eleven\n+twelve\n\\ No newline at end of file\n");
        // The whole output round-trips file by file.
        let joined: String = fs.iter().map(FileDiff::patch).collect();
        assert_eq!(joined, SAMPLE);
    }

    #[test]
    fn quoted_paths() {
        assert_eq!(unquote("\"tab\\there\""), "tab\there");
        assert_eq!(unquote("\"caf\\303\\251\""), "café");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(git_line_path("a/x y b/x y"), "x y");
        assert_eq!(git_line_path("\"a/t\\tb\" \"b/t\\tb\""), "t\tb");
    }

    #[test]
    fn untracked_files_are_all_added() {
        let f = untracked("n.txt", "a\nb");
        assert_eq!((f.added(), f.deleted()), (2, 0));
        assert!(f.hunks[0].lines[1].no_eol);
        assert_eq!(f.hunks[0].header, "@@ -0,0 +1,2 @@");
        assert!(untracked("e.txt", "").hunks.is_empty());
    }

    #[test]
    fn numstat_and_label() {
        assert_eq!(numstat("3\t1\ta.rs\n-\t-\timg.png\n10\t0\tb.rs\n"), (3, 13, 1));
        assert_eq!(stat_label(1, 2, 0), "1 file +2 −0");
        assert_eq!(stat_label(3, 13, 1), "3 files +13 −1");
    }

    #[test]
    fn word_highlights_mark_only_the_changed_words() {
        let (o, n) = word_ranges("let total = price * count;", "let total = price * amount;").unwrap();
        assert_eq!(o, vec![(20, 25)]);
        assert_eq!(n, vec![(20, 26)]);
        // Nothing in common: no highlights (the whole line is the change).
        assert!(word_ranges("alpha beta", "gamma delta").is_none());
        let lines: Vec<Line> = [LineKind::Context, LineKind::Deleted, LineKind::Deleted, LineKind::Added, LineKind::Context, LineKind::Deleted, LineKind::Added, LineKind::Added]
            .iter()
            .map(|&kind| Line { kind, text: String::new(), no_eol: false, old: None, new: None })
            .collect();
        assert_eq!(change_pairs(&lines), vec![(1, 3), (5, 6)]);
    }

    /// Hunk patches from real `git diff` output stage, unstage and revert exactly one hunk.
    #[test]
    fn hunk_patches_round_trip_through_git_apply() {
        let dir = crate::git::tests::scratch_repo("patch");
        let base: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("a.txt"), &base).unwrap();
        run(&dir, &["commit", "-qam", "thirty lines"]).unwrap();
        let changed = base.replace("line 3\n", "line three\n").replace("line 25\n", "line 25\nextra\n");
        std::fs::write(dir.join("a.txt"), &changed).unwrap();
        let diff = |args: &[&str]| -> Vec<FileDiff> {
            let mut a = vec!["-c", "core.quotePath=false", "diff"];
            a.extend_from_slice(args);
            a.extend_from_slice(DIFF_ARGS);
            parse(&run(&dir, &a).unwrap())
        };
        let fs = diff(&[]);
        assert_eq!(fs[0].hunks.len(), 2);
        // Stage the second hunk only.
        apply(&dir, &fs[0].hunk_patch(1), &["--cached"]);
        let staged = diff(&["--cached"]);
        assert_eq!(staged[0].hunks.len(), 1);
        assert!(staged[0].hunks[0].lines.iter().any(|l| l.text == "extra"));
        let unstaged = diff(&[]);
        assert_eq!(unstaged[0].hunks.len(), 1);
        assert!(unstaged[0].hunks[0].lines.iter().any(|l| l.text == "line three"));
        // Unstage it again.
        apply(&dir, &staged[0].hunk_patch(0), &["--cached", "-R"]);
        assert!(diff(&["--cached"]).is_empty());
        // Revert the first hunk in the working tree: only the second change is left.
        let fs = diff(&[]);
        apply(&dir, &fs[0].hunk_patch(0), &["-R"]);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), base.replace("line 25\n", "line 25\nextra\n"));
        // A file without a final newline keeps it that way.
        std::fs::write(dir.join("b.txt"), "bee\nmore").unwrap();
        let fs = diff(&[]);
        let b = fs.iter().find(|f| f.path == "b.txt").unwrap();
        assert!(b.hunks[0].lines.last().unwrap().no_eol);
        apply(&dir, &b.hunk_patch(0), &["-R"]);
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "bee\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn apply(dir: &Path, patch: &str, args: &[&str]) {
        crate::git::apply(dir, patch, args).unwrap();
    }
}
