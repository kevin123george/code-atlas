//! Conversions between Code Atlas positions (char offsets in a rope) and LSP ones
//! (line + UTF-16 code units), file URIs, and applying `TextEdit`s.

use std::path::{Path, PathBuf};

use lsp_types::{Position, TextEdit};
use ropey::Rope;

use crate::doc::Doc;

pub fn path_to_uri(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) }
    });
    let mut s = String::from("file://");
    for b in abs.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// Char offset → LSP position (UTF-16 columns).
pub fn to_lsp(rope: &Rope, pos: usize) -> Position {
    let pos = pos.min(rope.len_chars());
    let line = rope.char_to_line(pos);
    let start = rope.line_to_char(line);
    let col = rope.char_to_utf16_cu(pos) - rope.char_to_utf16_cu(start);
    Position::new(line as u32, col as u32)
}

/// LSP position → char offset, clamped to the line (servers may point past the end).
pub fn from_lsp(rope: &Rope, p: Position) -> usize {
    let line = p.line as usize;
    if line >= rope.len_lines() {
        return rope.len_chars();
    }
    let start = rope.line_to_char(line);
    let line_text = rope.line(line);
    let mut len = line_text.len_chars();
    while len > 0 && matches!(line_text.char(len - 1), '\n' | '\r') {
        len -= 1;
    }
    let start16 = rope.char_to_utf16_cu(start);
    let end16 = rope.char_to_utf16_cu(start + len);
    rope.utf16_cu_to_char((start16 + p.character as usize).min(end16))
}

/// Applies `edits` (all against the current text, as LSP sends them) as one undo step,
/// keeping the cursor next to the text it was at.
pub fn apply_edits(doc: &mut Doc, edits: &[TextEdit]) {
    if edits.is_empty() {
        return;
    }
    let mut ranges: Vec<(usize, usize, String, usize)> = edits
        .iter()
        .enumerate()
        .map(|(i, e)| (from_lsp(&doc.rope, e.range.start), from_lsp(&doc.rope, e.range.end), e.new_text.replace("\r\n", "\n"), i))
        .collect();
    // Last first, so earlier offsets stay valid; inserts at one spot end up in the given order.
    ranges.sort_by(|a, b| b.0.cmp(&a.0).then(b.3.cmp(&a.3)));
    let eol = doc.eol();
    doc.edit_with(|rope, cursor, anchor| {
        *anchor = None;
        for (s, e, text, _) in ranges {
            let (s, e) = (s.min(e), e.max(s));
            let text = if eol == "\r\n" { text.replace('\n', "\r\n") } else { text };
            rope.remove(s..e);
            rope.insert(s, &text);
            let n = text.chars().count();
            if *cursor >= e {
                *cursor = *cursor + n - (e - s);
            } else if *cursor > s {
                *cursor = s + n.min(*cursor - s);
            }
        }
    });
}

/// Snippet syntax (`${1:name}`, `$0`, `\$`) → plain text.
pub fn strip_snippet(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '$' if chars.peek().is_some_and(|n| n.is_ascii_digit()) => {
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
            }
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
                if chars.peek() == Some(&':') {
                    chars.next();
                }
                // Copy the placeholder text up to the matching brace (placeholders may nest).
                let mut depth = 1;
                let mut inner = String::new();
                for n in chars.by_ref() {
                    match n {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    inner.push(n);
                }
                // `${1|a,b|}` choices: take the first.
                if let Some(choice) = inner.strip_prefix('|') {
                    inner = choice.split([',', '|']).next().unwrap_or("").to_string();
                }
                out.push_str(&strip_snippet(&inner));
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Range;

    #[test]
    fn uris_round_trip() {
        let u = path_to_uri(Path::new("/tmp/a b/ü.rs"));
        assert_eq!(u, "file:///tmp/a%20b/%C3%BC.rs");
        assert_eq!(uri_to_path(&u).unwrap(), PathBuf::from("/tmp/a b/ü.rs"));
        assert_eq!(uri_to_path("https://x"), None);
    }

    #[test]
    fn utf16_positions() {
        let r = Rope::from_str("a😀b\nxyz\n");
        // 😀 is one char but two UTF-16 units.
        assert_eq!(to_lsp(&r, 2), Position::new(0, 3));
        assert_eq!(from_lsp(&r, Position::new(0, 3)), 2);
        assert_eq!(from_lsp(&r, Position::new(1, 99)), 7, "clamped to the line end");
        assert_eq!(from_lsp(&r, Position::new(9, 0)), r.len_chars());
    }

    fn edit(l1: u32, c1: u32, l2: u32, c2: u32, t: &str) -> TextEdit {
        TextEdit { range: Range::new(Position::new(l1, c1), Position::new(l2, c2)), new_text: t.into() }
    }

    #[test]
    fn applies_edits_in_one_step() {
        let mut d = Doc::from_text("let a=1;\nlet b=a;\n");
        d.move_to(15, false); // before "a;" on line 2
        apply_edits(&mut d, &[edit(0, 4, 0, 5, "alpha"), edit(1, 6, 1, 7, "alpha"), edit(0, 5, 0, 6, " = ")]);
        assert_eq!(d.rope.to_string(), "let alpha = 1;\nlet b=alpha;\n");
        assert_eq!(d.cursor, 21);
        assert!(d.undo());
        assert_eq!(d.rope.to_string(), "let a=1;\nlet b=a;\n");
    }

    #[test]
    fn strips_snippets() {
        assert_eq!(strip_snippet("foo(${1:a}, ${2:b})$0"), "foo(a, b)");
        assert_eq!(strip_snippet("vec![$0]"), "vec![]");
        assert_eq!(strip_snippet("${1|one,two|}"), "one");
        assert_eq!(strip_snippet("cost \\$5"), "cost $5");
    }
}
