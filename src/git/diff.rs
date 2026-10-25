//! Line diffs (the `similar` crate): gutter marks for the editor and the unified diff view.

use std::time::Duration;

use similar::{DiffOp, TextDiff};

/// Give up looking for the smallest diff after this long (big files stay responsive).
const TIMEOUT: Duration = Duration::from_millis(300);

/// A gutter mark on one line of the current text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// New line (green bar).
    Added,
    /// Changed line (blue bar).
    Modified,
    /// Lines were deleted just below this line (red triangle at the bottom).
    DeletedBelow,
    /// Lines were deleted just above this line: only at the top of the file.
    DeletedAbove,
}

fn line_diff<'a>(old: &'a str, new: &'a str) -> TextDiff<'a, 'a, str> {
    TextDiff::configure().timeout(TIMEOUT).diff_lines(old, new)
}

/// One entry per line of `new` (plus one for the empty line after a final newline).
pub fn gutter_marks(old: &str, new: &str) -> Vec<Option<Mark>> {
    let lines = new.split('\n').count();
    let mut marks = vec![None; lines];
    for op in line_diff(old, new).ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => {
                for m in marks.iter_mut().skip(new_index).take(new_len) {
                    *m = Some(Mark::Added);
                }
            }
            DiffOp::Replace { new_index, new_len, .. } => {
                for m in marks.iter_mut().skip(new_index).take(new_len) {
                    *m = Some(Mark::Modified);
                }
            }
            DiffOp::Delete { new_index, .. } => {
                let (at, mark) = if new_index == 0 { (0, Mark::DeletedAbove) } else { (new_index - 1, Mark::DeletedBelow) };
                if let Some(m) = marks.get_mut(at) {
                    if m.is_none() {
                        *m = Some(mark);
                    }
                }
            }
        }
    }
    marks
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Hunk,
    Context,
    Added,
    Deleted,
}

/// One row of the unified diff view. Line numbers are 1-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
}

/// Unified diff rows with `context` lines around each change; `(rows, added, deleted)`.
pub fn unified(old: &str, new: &str, context: usize) -> (Vec<DiffLine>, usize, usize) {
    let diff = line_diff(old, new);
    let (mut added, mut deleted) = (0, 0);
    let mut rows = Vec::new();
    let clean = |s: &str| s.trim_end_matches(['\n', '\r']).to_string();
    for group in diff.grouped_ops(context) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else { continue };
        let (o0, n0) = (first.old_range().start, first.new_range().start);
        let (o1, n1) = (last.old_range().end, last.new_range().end);
        rows.push(DiffLine {
            kind: LineKind::Hunk,
            old: None,
            new: Some(n0 + 1),
            text: format!("@@ -{},{} +{},{} @@", hunk_start(o0, o1), o1 - o0, hunk_start(n0, n1), n1 - n0),
        });
        for op in &group {
            for change in diff.iter_changes(op) {
                let (kind, old, new) = match change.tag() {
                    similar::ChangeTag::Equal => (LineKind::Context, change.old_index(), change.new_index()),
                    similar::ChangeTag::Delete => {
                        deleted += 1;
                        (LineKind::Deleted, change.old_index(), None)
                    }
                    similar::ChangeTag::Insert => {
                        added += 1;
                        (LineKind::Added, None, change.new_index())
                    }
                };
                rows.push(DiffLine { kind, old: old.map(|i| i + 1), new: new.map(|i| i + 1), text: clean(change.value()) });
            }
        }
    }
    (rows, added, deleted)
}

/// Git's hunk header start: 1-based, or the line before for an empty range.
fn hunk_start(start: usize, end: usize) -> usize {
    if end > start {
        start + 1
    } else {
        start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Mark::*;

    #[test]
    fn unchanged_text_has_no_marks() {
        assert!(gutter_marks("a\nb\n", "a\nb\n").iter().all(Option::is_none));
    }

    #[test]
    fn added_modified_deleted() {
        let old = "a\nb\nc\nd\n";
        // b changed, x added after c, d deleted.
        let new = "a\nB\nc\nx\n";
        let m = gutter_marks(old, new);
        assert_eq!(m[0], None);
        assert_eq!(m[1], Some(Modified));
        assert_eq!(m[2], None);
        assert_eq!(m[3], Some(Modified)); // d -> x is a replace
        let m = gutter_marks("a\nb\nc\n", "a\nb\nnew\nc\n");
        assert_eq!(m, vec![None, None, Some(Added), None, None]);
        let m = gutter_marks("a\nb\nc\n", "a\nc\n");
        assert_eq!(m, vec![Some(DeletedBelow), None, None]);
        let m = gutter_marks("a\nb\n", "b\n");
        assert_eq!(m, vec![Some(DeletedAbove), None]);
    }

    #[test]
    fn appending_without_final_newline() {
        let m = gutter_marks("a\n", "a\nb");
        assert_eq!(m, vec![None, Some(Added)]);
    }

    #[test]
    fn unified_rows_and_counts() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = "1\n2\n3\n4\nfive\n6\n7\n8\n9\nten\n";
        let (rows, added, deleted) = unified(old, new, 1);
        assert_eq!((added, deleted), (2, 1));
        let hunks: Vec<&str> = rows.iter().filter(|r| r.kind == LineKind::Hunk).map(|r| r.text.as_str()).collect();
        assert_eq!(hunks, vec!["@@ -4,3 +4,3 @@", "@@ -9,1 +9,2 @@"]);
        let del = rows.iter().find(|r| r.kind == LineKind::Deleted).unwrap();
        assert_eq!((del.old, del.new, del.text.as_str()), (Some(5), None, "5"));
        let add = rows.iter().find(|r| r.kind == LineKind::Added).unwrap();
        assert_eq!((add.old, add.new, add.text.as_str()), (None, Some(5), "five"));
    }

    #[test]
    fn new_file_is_all_added() {
        let (rows, added, deleted) = unified("", "x\ny\n", 3);
        assert_eq!((added, deleted), (2, 0));
        assert_eq!(rows[0].text, "@@ -0,0 +1,2 @@");
    }
}
