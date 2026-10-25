//! Popups next to the cursor: completion list, hover text, quick-fix (code action) list,
//! signature help (above the cursor line); and the centred pickers (usages, file structure,
//! workspace symbols, implementations, problems).

use std::path::PathBuf;

use lsp_types::{CompletionItem, CompletionItemKind, CompletionTextEdit, InsertTextFormat, TextEdit};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use serde_json::Value;

use super::snippet::{self, Snippet};
use crate::ui::theme::*;

pub struct Item {
    pub label: String,
    pub detail: String,
    pub kind: &'static str,
    /// The text to insert with its tab stops (plain text = no stops).
    pub insert: Snippet,
    /// A function or method whose text has no `(`: `($0)` is added unless a `(` follows already.
    pub auto_parens: bool,
    filter: String,
    sort: String,
    /// The server's own replace range (its start), if it gave one.
    pub edit_start: Option<lsp_types::Position>,
    /// Ends of the server's insert and replace ranges (text after the cursor they cover).
    pub edit_end: Option<lsp_types::Position>,
    pub replace_end: Option<lsp_types::Position>,
    pub extra: Vec<TextEdit>,
}

impl Item {
    pub fn from_lsp(c: CompletionItem) -> Item {
        let snippet = c.insert_text_format == Some(InsertTextFormat::SNIPPET);
        let (insert, edit_start, edit_end, replace_end) = match &c.text_edit {
            Some(CompletionTextEdit::Edit(e)) => (e.new_text.clone(), Some(e.range.start), Some(e.range.end), Some(e.range.end)),
            Some(CompletionTextEdit::InsertAndReplace(e)) => (e.new_text.clone(), Some(e.insert.start), Some(e.insert.end), Some(e.replace.end)),
            None => (c.insert_text.clone().unwrap_or_else(|| c.label.clone()), None, None, None),
        };
        let insert = if snippet { snippet::parse(&insert) } else { Snippet { text: insert, stops: Vec::new() } };
        let callable = matches!(c.kind, Some(CompletionItemKind::FUNCTION) | Some(CompletionItemKind::METHOD) | Some(CompletionItemKind::CONSTRUCTOR));
        let auto_parens = callable && !insert.text.contains('(') && !insert.text.contains('!');
        let detail = c.label_details.as_ref().and_then(|d| d.description.clone().or(d.detail.clone())).or(c.detail.clone()).unwrap_or_default();
        Item {
            // jdtls sends filter texts like "apiHit() : void": filter on the name part only, or
            // "api" would match "snapshot() : Map<String,Object>" through the type.
            filter: {
                let f = c.filter_text.clone().unwrap_or_else(|| c.label.clone());
                let name: String = f.chars().take_while(|&ch| ch.is_alphanumeric() || ch == '_' || ch == '$').collect();
                if name.is_empty() { f } else { name }
            },
            sort: c.sort_text.clone().unwrap_or_else(|| c.label.clone()),
            kind: kind_name(c.kind),
            detail: detail.lines().next().unwrap_or("").to_string(),
            label: c.label,
            insert,
            auto_parens,
            edit_start,
            edit_end,
            replace_end,
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
        /// The cursor when the list was asked for (server ranges are relative to it).
        req: lsp_types::Position,
    },
    Hover { lines: Vec<String>, scroll: usize },
    /// Code actions with the server each came from.
    Actions { items: Vec<(usize, Value)>, sel: usize },
    Picker(Picker),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickKind {
    Usages,
    Structure,
    Symbols,
    Locations,
    Problems,
}

#[derive(Clone, Debug)]
pub struct PickItem {
    pub kind: &'static str,
    pub label: String,
    pub detail: String,
    pub path: PathBuf,
    pub pos: lsp_types::Position,
    /// Nesting (file structure).
    pub depth: usize,
}

/// A centred list with a filter line: Enter opens the selected place.
pub struct Picker {
    pub kind: PickKind,
    pub title: String,
    pub query: String,
    pub items: Vec<PickItem>,
    pub shown: Vec<usize>,
    pub sel: usize,
    /// Workspace symbols: the server asked, and the number of the newest request.
    pub server: usize,
    pub seq: u64,
}

impl Picker {
    pub fn new(kind: PickKind, title: String, items: Vec<PickItem>) -> Picker {
        let mut p = Picker { kind, title, query: String::new(), shown: Vec::new(), items, sel: 0, server: 0, seq: 0 };
        p.refilter();
        p
    }

    /// Local fuzzy filter (workspace symbols are filtered by the server instead).
    pub fn refilter(&mut self) {
        let q = self.query.clone();
        self.shown = (0..self.items.len())
            .filter(|&i| self.kind == PickKind::Symbols || q.is_empty() || fuzzy(&q, &self.items[i].label) || fuzzy(&q, &self.items[i].detail))
            .collect();
        if !q.is_empty() && self.kind != PickKind::Symbols {
            let lower = q.to_lowercase();
            self.shown.sort_by_key(|&i| !self.items[i].label.to_lowercase().contains(&lower));
        }
        self.sel = self.sel.min(self.shown.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&PickItem> {
        self.shown.get(self.sel).map(|&i| &self.items[i])
    }
}

/// Signature help: the signature, the active parameter's char range in it, and a doc line.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub label: String,
    pub active: Option<(usize, usize)>,
    pub doc: String,
    /// Line the help was asked on (it closes when the cursor leaves it).
    pub line: usize,
    pub more: usize,
}

impl Popup {
    pub fn completion(items: Vec<Item>, start: usize, incomplete: bool, prefix: &str, req: lsp_types::Position) -> Popup {
        let mut p = Popup::Completion { items, shown: Vec::new(), sel: 0, start, incomplete, prefix: String::new(), req };
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
            Popup::Picker(p) => (&mut p.sel, p.shown.len()),
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

/// Resets the cells under a popup, so the text's styles (diagnostic underlines) don't show through.
fn clear(buf: &mut Buffer, area: Rect) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].reset();
        }
    }
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
        Popup::Picker(_) => return,
        Popup::Actions { items, sel, .. } => items.iter().enumerate().take(15).map(|(i, (_, a))| {
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
    clear(buf, area);
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

/// Draws a picker centred on `screen`; returns where the cursor goes (end of the filter text).
pub fn draw_picker(buf: &mut Buffer, p: &Picker, screen: Rect) -> Position {
    let w = screen.width.saturating_sub(4).min(110).max(20);
    let rows = p.shown.len().clamp(1, 15) as u16;
    let h = (rows + 3).min(screen.height.saturating_sub(2));
    let area = Rect::new(screen.x + (screen.width - w) / 2, screen.y + 2.min(screen.height.saturating_sub(h)), w, h);
    let base = Style::new().bg(SURFACE_1).fg(TEXT);
    clear(buf, area);
    buf.set_style(area, base);
    for y in area.y..area.bottom() {
        buf.set_string(area.x, y, " ".repeat(w as usize), base);
    }
    let count = if p.shown.len() == p.items.len() { format!("{}", p.items.len()) } else { format!("{} of {}", p.shown.len(), p.items.len()) };
    let title = format!(" {} ", p.title);
    buf.set_stringn(area.x + 1, area.y, &title, w as usize - 2, base.fg(ACCENT).add_modifier(Modifier::BOLD));
    let cw = count.chars().count() as u16;
    if title.chars().count() as u16 + cw + 4 < w {
        buf.set_string(area.right() - cw - 2, area.y, &count, base.fg(MUTED));
    }
    let prompt = "› ";
    buf.set_string(area.x + 1, area.y + 1, prompt, base.fg(ACCENT));
    let qx = area.x + 1 + prompt.chars().count() as u16;
    buf.set_stringn(qx, area.y + 1, &p.query, (w as usize).saturating_sub(5), base.fg(TEXT));
    let cursor = Position::new((qx + p.query.chars().count() as u16).min(area.right() - 2), area.y + 1);
    let list_h = h.saturating_sub(3) as usize;
    if p.shown.is_empty() {
        let msg = if p.kind == PickKind::Symbols && p.query.is_empty() { "type to search symbols" } else { "nothing found" };
        buf.set_string(area.x + 2, area.y + 2, msg, base.fg(MUTED));
        return cursor;
    }
    let top = p.sel.saturating_sub(list_h.saturating_sub(1));
    for (row, (i, &k)) in p.shown.iter().enumerate().skip(top).take(list_h).enumerate() {
        let it = &p.items[k];
        let y = area.y + 2 + row as u16;
        let style = if i == p.sel { base.bg(SELECTION) } else { base };
        buf.set_string(area.x, y, " ".repeat(w as usize), style);
        let kind_style = match it.kind {
            "err" => style.fg(CRITICAL),
            "warn" => style.fg(WARNING),
            "info" => style.fg(S_BLUE),
            _ => style.fg(S_VIOLET),
        };
        buf.set_stringn(area.x + 1, y, it.kind, 4, kind_style);
        let indent = if p.query.is_empty() { it.depth.min(8) * 2 } else { 0 };
        let room = (w as usize).saturating_sub(8 + indent);
        let detail_w = it.detail.chars().count();
        let label_room = if detail_w + 2 < room / 2 { room - detail_w - 2 } else { room.saturating_sub(room / 2) };
        let label_style = if i == p.sel { style.add_modifier(Modifier::BOLD) } else { style };
        buf.set_stringn(area.x + 6 + indent as u16, y, &it.label, label_room, label_style);
        let dw = detail_w.min(room.saturating_sub(label_room.min(it.label.chars().count()) + 2));
        if dw > 0 {
            // Keep the end of a long path (file name and line).
            let d: String = it.detail.chars().skip(detail_w - dw).collect();
            buf.set_string(area.right() - 1 - dw as u16, y, d, style.fg(MUTED));
        }
    }
    cursor
}

/// Draws signature help on the line above the cursor `at` (below when at the top).
pub fn draw_signature(buf: &mut Buffer, sig: &Signature, at: Position, screen: Rect) {
    let more = if sig.more > 0 { format!("  (+{} more)", sig.more) } else { String::new() };
    let lines = if sig.doc.is_empty() { 1 } else { 2 };
    let len = sig.label.chars().count() + more.chars().count();
    let w = ((len.max(sig.doc.chars().count().min(80)) + 2) as u16).min(screen.width.saturating_sub(2)).max(10);
    let x = at.x.saturating_sub(2).min(screen.right().saturating_sub(w));
    let y = if at.y >= screen.y + 1 + lines { at.y - lines } else { at.y + 1 };
    let base = Style::new().bg(SURFACE_2).fg(TEXT_2);
    let area = Rect::new(x, y, w, lines.min(screen.bottom().saturating_sub(y)));
    clear(buf, area);
    buf.set_style(area, base);
    buf.set_string(x, y, " ".repeat(w as usize), base);
    let mut cx = x + 1;
    for (i, c) in sig.label.chars().enumerate() {
        if cx >= area.right() - 1 {
            break;
        }
        let on = sig.active.is_some_and(|(s, e)| i >= s && i < e);
        let st = if on { base.fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { base };
        buf[(cx, y)].set_char(c).set_style(st);
        cx += 1;
    }
    if !more.is_empty() {
        buf.set_stringn(cx, y, &more, area.right().saturating_sub(cx + 1) as usize, base.fg(MUTED));
    }
    if lines == 2 && area.height == 2 {
        buf.set_string(x, y + 1, " ".repeat(w as usize), base);
        buf.set_stringn(x + 1, y + 1, &sig.doc, w as usize - 2, base.fg(MUTED));
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
        let mut p = Popup::completion(vec![item("set_goto_line"), item("goto_line"), item("other")], 0, false, "", Default::default());
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
        let it = Item::from_lsp(c);
        assert_eq!(it.insert.text, "push(value)");
        assert_eq!(it.insert.stops[0], snippet::Stop { index: 1, start: 5, end: 10 });
        assert!(!it.auto_parens);
    }

    #[test]
    fn plain_functions_get_parentheses() {
        let f = CompletionItem { label: "print".into(), kind: Some(CompletionItemKind::FUNCTION), ..Default::default() };
        let it = Item::from_lsp(f);
        assert!(it.auto_parens && it.insert.stops.is_empty());
        let v = CompletionItem { label: "x".into(), kind: Some(CompletionItemKind::VARIABLE), ..Default::default() };
        assert!(!Item::from_lsp(v).auto_parens);
        let m = CompletionItem { label: "println!".into(), kind: Some(CompletionItemKind::FUNCTION), ..Default::default() };
        assert!(!Item::from_lsp(m).auto_parens, "macros are left alone");
    }

    #[test]
    fn filter_text_uses_the_name_part() {
        let c = CompletionItem { label: "snapshot".into(), filter_text: Some("snapshot() : Map<String,Object>".into()), ..Default::default() };
        let mut p = Popup::completion(vec![Item::from_lsp(c), item("apiHit")], 0, false, "", Default::default());
        p.filter("api");
        let Popup::Completion { shown, items, .. } = &p else { unreachable!() };
        assert_eq!(shown.iter().map(|&i| items[i].label.as_str()).collect::<Vec<_>>(), ["apiHit"]);
    }

    fn pick(label: &str, detail: &str) -> PickItem {
        PickItem { kind: "fn", label: label.into(), detail: detail.into(), path: PathBuf::new(), pos: Default::default(), depth: 0 }
    }

    #[test]
    fn pickers_filter_locally_except_workspace_symbols() {
        let items = vec![pick("save_file", "doc.rs"), pick("load", "app.rs"), pick("file_size", "doc.rs")];
        let mut p = Picker::new(PickKind::Structure, "t".into(), items.clone());
        assert_eq!(p.shown, [0, 1, 2]);
        p.query = "file".into();
        p.refilter();
        assert_eq!(p.shown, [0, 2]);
        p.query = "app".into();
        p.refilter();
        assert_eq!(p.shown, [1], "detail matches too");
        assert_eq!(p.selected().unwrap().label, "load");
        let mut s = Picker::new(PickKind::Symbols, "t".into(), items);
        s.query = "zzz".into();
        s.refilter();
        assert_eq!(s.shown.len(), 3);
    }

    #[test]
    fn hover_text_is_wrapped_without_fences() {
        let l = hover_lines("```rust\npub fn line(&self) -> usize\n```\n\nThe line the cursor is on, counting from zero.", 20);
        assert_eq!(l[0], "pub fn line(&self)");
        assert_eq!(l[1], "-> usize");
        assert!(l.iter().all(|x| x.chars().count() <= 20));
    }
}
