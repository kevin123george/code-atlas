//! Popups next to the cursor: completion list, hover text, quick-fix (code action) list.

use lsp_types::{CompletionItem, CompletionItemKind, CompletionTextEdit, InsertTextFormat, TextEdit};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use serde_json::Value;

use super::text::strip_snippet;
use crate::ui::theme::*;

pub struct Item {
    pub label: String,
    pub detail: String,
    pub kind: &'static str,
    pub insert: String,
    filter: String,
    sort: String,
    /// The server's own replace range (its start), if it gave one.
    pub edit_start: Option<lsp_types::Position>,
    pub extra: Vec<TextEdit>,
}

impl Item {
    pub fn from_lsp(c: CompletionItem) -> Item {
        let snippet = c.insert_text_format == Some(InsertTextFormat::SNIPPET);
        let (mut insert, edit_start) = match &c.text_edit {
            Some(CompletionTextEdit::Edit(e)) => (e.new_text.clone(), Some(e.range.start)),
            Some(CompletionTextEdit::InsertAndReplace(e)) => (e.new_text.clone(), Some(e.insert.start)),
            None => (c.insert_text.clone().unwrap_or_else(|| c.label.clone()), None),
        };
        if snippet {
            insert = strip_snippet(&insert);
        }
        let detail = c.label_details.as_ref().and_then(|d| d.description.clone().or(d.detail.clone())).or(c.detail.clone()).unwrap_or_default();
        Item {
            filter: c.filter_text.clone().unwrap_or_else(|| c.label.clone()),
            sort: c.sort_text.clone().unwrap_or_else(|| c.label.clone()),
            kind: kind_name(c.kind),
            detail: detail.lines().next().unwrap_or("").to_string(),
            label: c.label,
            insert,
            edit_start,
            extra: c.additional_text_edits.unwrap_or_default(),
        }
    }
}

fn kind_name(k: Option<CompletionItemKind>) -> &'static str {
    match k {
        Some(CompletionItemKind::FUNCTION) => "fn",
        Some(CompletionItemKind::METHOD) => "me",
        Some(CompletionItemKind::FIELD) | Some(CompletionItemKind::PROPERTY) => "fd",
        Some(CompletionItemKind::VARIABLE) => "v",
        Some(CompletionItemKind::CLASS) | Some(CompletionItemKind::STRUCT) => "C",
        Some(CompletionItemKind::INTERFACE) => "I",
        Some(CompletionItemKind::ENUM) => "E",
        Some(CompletionItemKind::ENUM_MEMBER) => "e",
        Some(CompletionItemKind::MODULE) => "m",
        Some(CompletionItemKind::KEYWORD) => "kw",
        Some(CompletionItemKind::CONSTANT) => "c",
        Some(CompletionItemKind::SNIPPET) => "sn",
        Some(CompletionItemKind::TYPE_PARAMETER) => "T",
        _ => "·",
    }
}

/// Case-insensitive subsequence match: "gtln" matches "goto_line".
pub fn fuzzy(pattern: &str, text: &str) -> bool {
    let mut t = text.chars().flat_map(char::to_lowercase);
    pattern.chars().flat_map(char::to_lowercase).all(|p| t.any(|c| c == p))
}

pub enum Popup {
    Completion {
        items: Vec<Item>,
        /// Indexes into `items` that match what's typed, best first.
        shown: Vec<usize>,
        sel: usize,
        /// Char offset where the completed word starts.
        start: usize,
        incomplete: bool,
        prefix: String,
    },
    Hover { lines: Vec<String>, scroll: usize },
    Actions { items: Vec<Value>, sel: usize, server: usize },
}

impl Popup {
    pub fn completion(items: Vec<Item>, start: usize, incomplete: bool, prefix: &str) -> Popup {
        let mut p = Popup::Completion { items, shown: Vec::new(), sel: 0, start, incomplete, prefix: String::new() };
        p.filter(prefix);
        p
    }

    pub fn filter(&mut self, typed: &str) {
        if let Popup::Completion { items, shown, sel, prefix, .. } = self {
            *prefix = typed.to_string();
            let lower = typed.to_lowercase();
            let mut v: Vec<usize> = (0..items.len()).filter(|&i| fuzzy(typed, &items[i].filter)).collect();
            v.sort_by(|&a, &b| {
                let pa = !items[a].filter.to_lowercase().starts_with(&lower);
                let pb = !items[b].filter.to_lowercase().starts_with(&lower);
                pa.cmp(&pb).then_with(|| items[a].sort.cmp(&items[b].sort))
            });
            *shown = v;
            *sel = 0;
        }
    }

    pub fn move_sel(&mut self, delta: isize) {
        let (sel, n) = match self {
            Popup::Completion { sel, shown, .. } => (sel, shown.len()),
            Popup::Actions { sel, items, .. } => (sel, items.len()),
            Popup::Hover { scroll, lines } => {
                *scroll = (*scroll as isize + delta).clamp(0, lines.len().saturating_sub(1) as isize) as usize;
                return;
            }
        };
        if n > 0 {
            *sel = (*sel as isize + delta).rem_euclid(n as isize) as usize;
        }
    }
}

/// Hover markdown → plain lines (code fences dropped, wrapped to `w`).
pub fn hover_lines(md: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in md.lines() {
        let l = line.trim_end();
        if l.starts_with("```") {
            continue;
        }
        if l == "---" || l == "***" {
            out.push("─".repeat(w.min(40)));
            continue;
        }
        let mut cur = String::new();
        for word in l.split(' ') {
            if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > w {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        out.push(cur);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    out
}

/// Draws the popup at the text cursor `at` (screen position), inside `screen`.
pub fn draw(buf: &mut Buffer, popup: &Popup, at: Position, screen: Rect) {
    let rows: Vec<(String, String, String, bool)> = match popup {
        Popup::Completion { items, shown, sel, .. } => {
            let top = sel.saturating_sub(9);
            shown.iter().enumerate().skip(top).take(10).map(|(i, &k)| {
                let it = &items[k];
                (it.kind.to_string(), it.label.clone(), it.detail.clone(), i == *sel)
            }).collect()
        }
        Popup::Actions { items, sel, .. } => items.iter().enumerate().take(15).map(|(i, a)| {
            let title = a.get("title").and_then(Value::as_str).unwrap_or("?").to_string();
            let kind = a.get("kind").and_then(Value::as_str).unwrap_or("");
            let mark = if kind.starts_with("quickfix") { "fix" } else if kind.is_empty() { "·" } else { "ref" };
            (mark.to_string(), title, String::new(), i == *sel)
        }).collect(),
        Popup::Hover { lines, scroll } => lines.iter().skip(*scroll).take(15).map(|l| (String::new(), l.clone(), String::new(), false)).collect(),
    };
    if rows.is_empty() {
        return;
    }
    let hover = matches!(popup, Popup::Hover { .. });
    let kind_w = if hover { 0 } else { 4 };
    let label_w = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0);
    let detail_w = rows.iter().map(|r| r.2.chars().count()).max().unwrap_or(0);
    let want = kind_w + label_w + if detail_w > 0 { detail_w + 2 } else { 0 } + 2;
    let w = (want as u16).min(screen.width.saturating_sub(2)).min(if hover { 82 } else { 72 }).max(12);
    let h = rows.len() as u16;
    let x = at.x.min(screen.right().saturating_sub(w));
    let below = at.y + 1 + h <= screen.bottom().saturating_sub(1);
    let y = if below { at.y + 1 } else { at.y.saturating_sub(h).max(screen.y + 1) };
    let area = Rect::new(x, y, w, h.min(screen.bottom().saturating_sub(y + 1)));
    let base = Style::new().bg(SURFACE_2).fg(TEXT);
    buf.set_style(area, base);
    for (i, (kind, label, detail, selected)) in rows.iter().enumerate() {
        let yy = area.y + i as u16;
        if yy >= area.bottom() {
            break;
        }
        let row_style = if *selected { base.bg(SELECTION) } else { base };
        buf.set_string(area.x, yy, " ".repeat(area.width as usize), row_style);
        let mut cx = area.x + 1;
        if !hover {
            buf.set_stringn(cx, yy, kind, 3, row_style.fg(S_VIOLET));
            cx += kind_w as u16;
        }
        let room = area.right().saturating_sub(cx + 1) as usize;
        let style = if *selected { row_style.add_modifier(Modifier::BOLD) } else { row_style };
        buf.set_stringn(cx, yy, label, room, style);
        let dw = detail.chars().count();
        let used = label.chars().count() + 2;
        if dw > 0 && used + dw <= room {
            buf.set_stringn(area.right() - 1 - dw as u16, yy, detail, dw, row_style.fg(MUTED));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Item {
        Item::from_lsp(CompletionItem { label: label.into(), ..Default::default() })
    }

    #[test]
    fn filters_fuzzy_with_prefix_matches_first() {
        let mut p = Popup::completion(vec![item("set_goto_line"), item("goto_line"), item("other")], 0, false, "");
        p.filter("gtl");
        let Popup::Completion { shown, items, .. } = &p else { unreachable!() };
        let labels: Vec<&str> = shown.iter().map(|&i| items[i].label.as_str()).collect();
        assert_eq!(labels, ["goto_line", "set_goto_line"]);
    }

    #[test]
    fn snippet_items_insert_plain_text() {
        let c = CompletionItem {
            label: "push(…)".into(),
            insert_text: Some("push(${1:value})$0".into()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        };
        assert_eq!(Item::from_lsp(c).insert, "push(value)");
    }

    #[test]
    fn hover_text_is_wrapped_without_fences() {
        let l = hover_lines("```rust\npub fn line(&self) -> usize\n```\n\nThe line the cursor is on, counting from zero.", 20);
        assert_eq!(l[0], "pub fn line(&self)");
        assert_eq!(l[1], "-> usize");
        assert!(l.iter().all(|x| x.chars().count() <= 20));
    }
}
