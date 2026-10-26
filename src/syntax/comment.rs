//! Ctrl+/ "Comment with Line Comment", IntelliJ style: if every non-blank line in the selection
//! (or the cursor line) is commented, uncomment them; otherwise add `token ` at the smallest
//! indentation of those lines. Without a selection the cursor then moves one line down.
//! Languages with only block comments (XML, HTML, CSS, Markdown) get each line wrapped instead:
//! `<!-- line -->` (IntelliJ does the same for XML), see [`plan_wrap`].

use crate::doc::Doc;

/// One line's change: at char column `col`, remove `remove` chars and insert `insert`.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    pub col: usize,
    pub remove: usize,
    pub insert: String,
}

fn indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// What toggling does to each of `lines` (text without line breaks).
pub fn plan(lines: &[&str], token: &str) -> Vec<Option<Change>> {
    let blank = |l: &str| l.trim().is_empty();
    let commented = |l: &str| l.trim_start().starts_with(token);
    let any_text = lines.iter().any(|l| !blank(l));
    if any_text && lines.iter().filter(|l| !blank(l)).all(|l| commented(l)) {
        return lines
            .iter()
            .map(|l| {
                if blank(l) {
                    return None;
                }
                let col = indent(l);
                let after: String = l.chars().skip(col + token.chars().count()).take(1).collect();
                Some(Change { col, remove: token.chars().count() + usize::from(after == " "), insert: String::new() })
            })
            .collect();
    }
    let target = |l: &str| !any_text || !blank(l);
    let col = lines.iter().filter(|l| target(l)).map(|l| indent(l)).min().unwrap_or(0);
    lines
        .iter()
        .map(|l| target(l).then(|| Change { col: col.min(l.chars().count()), remove: 0, insert: format!("{token} ") }))
        .collect()
}

/// Whether a line (without its break) is wrapped as a whole: `<!-- x -->`.
fn wrapped(line: &str, open: &str, close: &str) -> bool {
    let t = line.trim();
    t.len() >= open.len() + close.len() && t.starts_with(open) && t.ends_with(close)
}

/// Block-comment toggling, one line at a time: if every non-blank line is wrapped in
/// `open … close`, unwrap them (with one space inside each end, if there); otherwise wrap each
/// non-blank line that isn't wrapped yet (wrapping it again would nest comments, which XML
/// doesn't allow). The opening goes at the smallest indentation, the closing at the line end.
/// Each line's changes come right to left.
pub fn plan_wrap(lines: &[&str], open: &str, close: &str) -> Vec<Vec<Change>> {
    let blank = |l: &str| l.trim().is_empty();
    let any_text = lines.iter().any(|l| !blank(l));
    let (ol, cl) = (open.chars().count(), close.chars().count());
    if any_text && lines.iter().filter(|l| !blank(l)).all(|l| wrapped(l, open, close)) {
        return lines
            .iter()
            .map(|l| {
                if blank(l) {
                    return Vec::new();
                }
                let chars: Vec<char> = l.chars().collect();
                let start = indent(l);
                let end = chars.len() - l.chars().rev().take_while(|c| c.is_whitespace()).count();
                let inner_start = start + ol;
                let mut close_at = end - cl;
                let mut close_len = cl;
                if close_at > inner_start && chars[close_at - 1] == ' ' {
                    close_at -= 1;
                    close_len += 1;
                }
                let open_len = ol + usize::from(inner_start < close_at && chars[inner_start] == ' ');
                vec![
                    Change { col: close_at, remove: close_len, insert: String::new() },
                    Change { col: start, remove: open_len, insert: String::new() },
                ]
            })
            .collect();
    }
    let target = |l: &str| if any_text { !blank(l) && !wrapped(l, open, close) } else { true };
    let col = lines.iter().filter(|l| target(l)).map(|l| indent(l)).min().unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            if !target(l) {
                return Vec::new();
            }
            let end = l.chars().count() - l.chars().rev().take_while(|c| c.is_whitespace()).count();
            vec![
                Change { col: end.max(col), remove: 0, insert: format!(" {close}") },
                Change { col: col.min(l.chars().count()), remove: 0, insert: format!("{open} ") },
            ]
        })
        .collect()
}

/// Toggles line comments on the selected lines (or the cursor line) as one undo step.
pub fn toggle(doc: &mut Doc, token: &str) {
    apply(doc, |lines| plan(lines, token).into_iter().map(|c| c.into_iter().collect()).collect());
}

/// Toggles `open … close` around each selected line (or the cursor line) as one undo step.
pub fn toggle_wrap(doc: &mut Doc, open: &str, close: &str) {
    apply(doc, |lines| plan_wrap(lines, open, close));
}

/// Runs a plan over the selected lines (or the cursor line); each line's changes right to left.
fn apply(doc: &mut Doc, plan: impl FnOnce(&[&str]) -> Vec<Vec<Change>>) {
    let (a, b) = doc.line_span();
    let had_selection = doc.selection().is_some();
    let texts: Vec<String> = (a..=b).map(|l| doc.rope.slice(doc.line_start(l)..doc.line_end(l)).to_string()).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let changes = plan(&refs);
    let starts: Vec<usize> = (a..=b).map(|l| doc.line_start(l)).collect();
    doc.edit_with(|rope, cursor, anchor| {
        // Bottom-up, so the line starts we computed stay valid.
        for (i, line) in changes.iter().enumerate().rev() {
            for ch in line {
                let p = starts[i] + ch.col;
                if ch.remove > 0 {
                    rope.remove(p..p + ch.remove);
                    let fix = |x: usize| if x >= p + ch.remove { x - ch.remove } else if x > p { p } else { x };
                    *cursor = fix(*cursor);
                    *anchor = anchor.map(fix);
                }
                if !ch.insert.is_empty() {
                    rope.insert(p, &ch.insert);
                    let n = ch.insert.chars().count();
                    let fix = |x: usize| if x >= p { x + n } else { x };
                    *cursor = fix(*cursor);
                    *anchor = anchor.map(fix);
                }
            }
        }
    });
    if !had_selection && b + 1 < doc.visible_lines() {
        doc.vertical(1, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, sel: Option<(usize, usize)>, token: &str) -> (String, usize) {
        let mut d = Doc::from_text(text);
        match sel {
            Some((s, e)) => d.select(s, e),
            None => d.move_to(0, false),
        }
        toggle(&mut d, token);
        (d.rope.to_string(), d.cursor)
    }

    #[test]
    fn comments_and_uncomments_one_line() {
        let (t, c) = run("let a = 1;\nlet b = 2;\n", None, "//");
        assert_eq!(t, "// let a = 1;\nlet b = 2;\n");
        assert_eq!(c, 14 + 3, "cursor moves to the next line, same column");
        let (t, _) = run("// let a = 1;\n", None, "//");
        assert_eq!(t, "let a = 1;\n");
        let (t, _) = run("    #x\n", None, "#");
        assert_eq!(t, "    x\n", "no space after the token is fine too");
    }

    #[test]
    fn multi_line_selection_uses_the_smallest_indent_and_skips_blank_lines() {
        let text = "fn a() {\n    x();\n\n    y();\n}\n";
        let (t, _) = run(text, Some((9, 26)), "//"); // lines 2-4
        assert_eq!(t, "fn a() {\n    // x();\n\n    // y();\n}\n");
        // Toggling again restores it.
        let mut d = Doc::from_text(&t);
        d.select(9, 32);
        toggle(&mut d, "//");
        assert_eq!(d.rope.to_string(), text);
        assert!(d.undo());
        assert_eq!(d.rope.to_string(), t, "one undo step");
    }

    #[test]
    fn mixed_lines_get_commented() {
        let p = plan(&["# done", "todo"], "#");
        assert_eq!(p[0], Some(Change { col: 0, remove: 0, insert: "# ".into() }));
        assert_eq!(p[1], Some(Change { col: 0, remove: 0, insert: "# ".into() }));
    }

    #[test]
    fn a_blank_line_alone_gets_a_comment() {
        assert_eq!(plan(&[""], "//"), vec![Some(Change { col: 0, remove: 0, insert: "// ".into() })]);
    }

    fn run_wrap(text: &str, sel: Option<(usize, usize)>) -> String {
        let mut d = Doc::from_text(text);
        match sel {
            Some((s, e)) => d.select(s, e),
            None => d.move_to(0, false),
        }
        toggle_wrap(&mut d, "<!--", "-->");
        d.rope.to_string()
    }

    #[test]
    fn xml_lines_get_wrapped_and_unwrapped() {
        assert_eq!(run_wrap("<a x=\"1\"/>\n<b/>\n", None), "<!-- <a x=\"1\"/> -->\n<b/>\n");
        assert_eq!(run_wrap("<!-- <a/> -->\n", None), "<a/>\n");
        assert_eq!(run_wrap("  <!--<a/>-->  \n", None), "  <a/>  \n", "no spaces inside, trailing blanks kept");
        // Several lines: each wrapped at the smallest indent; blank lines left alone.
        let text = "<r>\n    <a/>\n\n  <b>t</b>\n</r>\n";
        let sel = Some((4, 24)); // lines 2-4
        let wrapped = run_wrap(text, sel);
        assert_eq!(wrapped, "<r>\n  <!--   <a/> -->\n\n  <!-- <b>t</b> -->\n</r>\n");
        let mut d = Doc::from_text(&wrapped);
        d.select(4, d.line_end(3));
        toggle_wrap(&mut d, "<!--", "-->");
        assert_eq!(d.rope.to_string(), text, "toggling again restores it");
        assert!(d.undo());
        assert_eq!(d.rope.to_string(), wrapped, "one undo step");
        // A line already commented isn't wrapped again (XML comments don't nest).
        let p = plan_wrap(&["<!-- a -->", "<b/>"], "<!--", "-->");
        assert!(p[0].is_empty());
        assert_eq!(p[1].len(), 2);
        // CSS uses the same per-line wrapping.
        let mut c = Doc::from_text("a { color: red; }\n");
        toggle_wrap(&mut c, "/*", "*/");
        assert_eq!(c.rope.to_string(), "/* a { color: red; } */\n");
    }

    #[test]
    fn selection_ending_at_line_start_leaves_that_line() {
        let (t, _) = run("a\nb\nc\n", Some((0, 4)), "#");
        assert_eq!(t, "# a\n# b\nc\n");
    }
}
