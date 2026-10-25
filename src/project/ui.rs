//! Drawing the sidebar tree and the popups (finder, recent files, find in files).

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Clear, Widget};

use super::{PopupKind, Project, SIDEBAR_W};
use crate::app::App;
use crate::ui::theme::*;

/// Draws the sidebar on the left of `area` (when shown and there's room) and returns what's left
/// for the editor.
pub fn draw_sidebar(buf: &mut Buffer, app: &mut App, area: Rect) -> Rect {
    let p = &mut app.project;
    if !p.sidebar || area.width < SIDEBAR_W + 20 || area.height < 2 {
        p.sidebar_rect = Rect::default();
        return area;
    }
    let side = Rect::new(area.x, area.y, SIDEBAR_W, area.height);
    p.sidebar_rect = side;
    let inner_w = SIDEBAR_W - 1; // last column is the divider
    buf.set_style(side, Style::new().bg(SURFACE_1).fg(TEXT));
    for y in side.y..side.bottom() {
        buf[(side.right() - 1, y)].set_char('│').set_fg(GRID).set_bg(PAPER);
    }
    let focused = p.focus_tree;
    let head = Style::new().fg(if focused { ACCENT } else { TEXT_2 }).bg(SURFACE_1).add_modifier(Modifier::BOLD);
    buf.set_stringn(side.x, side.y, " Project", inner_w as usize, head);

    let active = app.docs[app.active].path.as_deref().map(super::canonical);
    let git = &app.dock.git;
    let rows_h = side.height.saturating_sub(1) as usize;
    let t = &mut p.tree;
    if t.follow {
        t.keep_visible(rows_h);
        t.follow = false;
    }
    t.scroll = t.scroll.min(t.rows.len().saturating_sub(1));
    for row in 0..rows_h {
        let i = t.scroll + row;
        let Some(n) = t.rows.get(i) else { break };
        let y = side.y + 1 + row as u16;
        let sel = i == t.selected;
        let bg = match (sel, focused) {
            (true, true) => SELECTION,
            (true, false) => SURFACE_2,
            _ => SURFACE_1,
        };
        buf.set_style(Rect::new(side.x, y, inner_w, 1), Style::new().bg(bg));
        let indent = 1 + 2 * n.depth;
        let (arrow, name_style) = if n.is_dir {
            (if n.expanded { "▾ " } else { "▸ " }, Style::new().fg(TEXT).bg(bg).add_modifier(Modifier::BOLD))
        } else if active.as_deref() == Some(n.path.as_path()) {
            ("  ", Style::new().fg(ACCENT).bg(bg))
        } else {
            ("  ", Style::new().fg(TEXT_2).bg(bg))
        };
        // Git status colours (M5): modified blue, new green.
        let name_style = match crate::git::ui::tree_color(git, &n.path, n.is_dir) {
            Some(c) if active.as_deref() != Some(n.path.as_path()) => name_style.fg(c),
            _ => name_style,
        };
        let max = inner_w as usize;
        if indent + 2 >= max {
            continue;
        }
        buf.set_string(side.x + indent as u16, y, arrow, Style::new().fg(MUTED).bg(bg));
        let room = max - indent - 2;
        buf.set_stringn(side.x + (indent + 2) as u16, y, ellipsize(&n.name, room), room, name_style);
    }
    Rect::new(area.x + SIDEBAR_W, area.y, area.width - SIDEBAR_W, area.height)
}

fn ellipsize(s: &str, room: usize) -> String {
    if s.chars().count() <= room {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(room.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Draws the open popup centred in `area`; returns where the cursor goes (the input line).
pub fn draw_popup(buf: &mut Buffer, p: &mut Project, area: Rect) -> Option<Position> {
    let kind = p.popup.as_ref()?.kind;
    let w = area.width.saturating_sub(4).min(if kind == PopupKind::Search { 110 } else { 90 });
    let h = area.height.saturating_sub(2).min(24);
    if w < 20 || h < 6 {
        return None;
    }
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    p.popup_rect = r;
    Clear.render(r, buf);
    let status = status_line(p);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BASELINE).bg(SURFACE_1))
        .style(Style::new().bg(SURFACE_1).fg(TEXT))
        .title(format!(" {} ", kind.title()))
        .title_style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
        .title_bottom(" ↑↓ select · Enter open · Esc close ")
        .render(r, buf);
    // Bottom hint in muted, over the border colour.
    for x in r.x + 1..r.right() - 1 {
        let c = &mut buf[(x, r.bottom() - 1)];
        if c.symbol() != "─" {
            c.set_fg(MUTED);
        }
    }

    let inner = Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 2);
    let popup = p.popup.as_mut()?;
    // Input line.
    buf.set_string(inner.x, inner.y, "› ", Style::new().fg(ACCENT).bg(SURFACE_1));
    let room = inner.width.saturating_sub(3) as usize;
    let shown: String = {
        let n = popup.input.chars().count();
        popup.input.chars().skip(n.saturating_sub(room)).collect()
    };
    let (cx, _) = buf.set_stringn(inner.x + 2, inner.y, &shown, room, Style::new().fg(TEXT).bg(SURFACE_1));
    let cursor = Position::new(cx.min(inner.right() - 1), inner.y);
    // Status (counts) right-aligned on the input line, and a divider.
    let sw = status.chars().count() as u16;
    if sw + 8 < inner.width && sw > 0 && cx + 2 < inner.right() - sw {
        buf.set_string(inner.right() - sw, inner.y, &status, Style::new().fg(MUTED).bg(SURFACE_1));
    }
    for x in r.x + 1..r.right() - 1 {
        buf[(x, inner.y + 1)].set_char('─').set_fg(GRID).set_bg(SURFACE_1);
    }

    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    p.list_rect = list;
    let len = p.popup_len();
    let popup = p.popup.as_mut()?;
    let lh = list.height as usize;
    if popup.selected < popup.scroll {
        popup.scroll = popup.selected;
    } else if lh > 0 && popup.selected >= popup.scroll + lh {
        popup.scroll = popup.selected + 1 - lh;
    }
    let (selected, scroll) = (popup.selected, popup.scroll);
    if len == 0 {
        let msg = empty_text(p, kind);
        buf.set_stringn(list.x, list.y, msg, list.width as usize, Style::new().fg(MUTED).bg(SURFACE_1));
        return Some(cursor);
    }
    for row in 0..lh {
        let i = scroll + row;
        if i >= len {
            break;
        }
        let y = list.y + row as u16;
        let bg = if i == selected { SELECTION } else { SURFACE_1 };
        // Selected row spans the whole inner width of the box.
        buf.set_style(Rect::new(r.x + 1, y, r.width - 2, 1), Style::new().bg(bg));
        let row_rect = Rect::new(list.x, y, list.width, 1);
        if kind == PopupKind::Search {
            draw_hit(buf, p, i, row_rect, bg);
        } else {
            draw_match(buf, p, i, row_rect, bg);
        }
    }
    Some(cursor)
}

fn status_line(p: &Project) -> String {
    let Some(pop) = &p.popup else { return String::new() };
    match pop.kind {
        PopupKind::Search => {
            if pop.input.chars().count() < 2 {
                String::new()
            } else {
                let more = if p.hits.len() >= super::search::MAX_HITS { "+" } else { "" };
                let busy = if p.searching() { " · searching…" } else { "" };
                let n = p.hits.len();
                let f = p.hit_files;
                format!("{n}{more} match{} in {f} file{}{busy}", if n == 1 { "" } else { "es" }, if f == 1 { "" } else { "s" })
            }
        }
        _ => {
            let busy = if p.indexing() { " · indexing…" } else { "" };
            format!("{} of {} files{}", p.matches.len(), if pop.kind == PopupKind::Recent { p.recent.len() } else { p.files.len() }, busy)
        }
    }
}

fn empty_text(p: &Project, kind: PopupKind) -> &'static str {
    let input = p.popup.as_ref().map_or("", |x| x.input.as_str());
    match kind {
        PopupKind::Search if input.chars().count() < 2 => "Type at least 2 characters to search the project",
        PopupKind::Search if p.searching() => "Searching…",
        PopupKind::Search => "Nothing found",
        PopupKind::Recent => "No recent files yet",
        PopupKind::Files if input.trim().is_empty() => "Type part of a file name (name:42 jumps to a line)",
        PopupKind::Files if p.indexing() => "Indexing…",
        PopupKind::Files => "No matching files",
    }
}

/// A finder row: the file name (bold), then its folder (muted); matched chars in the accent.
fn draw_match(buf: &mut Buffer, p: &Project, i: usize, r: Rect, bg: ratatui::style::Color) {
    let m = &p.matches[i];
    let chars: Vec<char> = m.path.chars().collect();
    let name_start = chars.iter().rposition(|&c| c == '/').map_or(0, |k| k + 1);
    let hit = |k: usize| m.pos.binary_search(&k).is_ok();
    let mut x = r.x;
    let end = r.right();
    for (k, &c) in chars.iter().enumerate().skip(name_start) {
        let st = if hit(k) { Style::new().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) };
        x = put(buf, x, r.y, c, end, st.bg(bg));
    }
    if name_start > 0 {
        x = put(buf, x, r.y, ' ', end, Style::new().bg(bg));
        x = put(buf, x, r.y, ' ', end, Style::new().bg(bg));
        for (k, &c) in chars.iter().enumerate().take(name_start - 1) {
            let st = if hit(k) { Style::new().fg(ACCENT) } else { Style::new().fg(MUTED) };
            x = put(buf, x, r.y, c, end, st.bg(bg));
        }
    }
}

/// A find-in-files row: `path:line` then the line, with the match in the accent.
fn draw_hit(buf: &mut Buffer, p: &Project, i: usize, r: Rect, bg: ratatui::style::Color) {
    let h = &p.hits[i];
    let end = r.right();
    let loc = format!("{}:{}", h.path, h.line + 1);
    // Keep the location to at most 45% of the row, cutting its front.
    let max_loc = (r.width as usize * 45 / 100).max(10);
    let n = loc.chars().count();
    let loc: String = if n > max_loc { format!("…{}", loc.chars().skip(n - max_loc + 1).collect::<String>()) } else { loc };
    let (mut x, _) = buf.set_stringn(r.x, r.y, &loc, r.width as usize, Style::new().fg(TEXT_2).bg(bg));
    x = put(buf, x, r.y, ' ', end, Style::new().bg(bg));
    x = put(buf, x, r.y, ' ', end, Style::new().bg(bg));
    let len = h.end - h.start;
    let lead = h.text.chars().take_while(|c| *c == ' ').count().min(h.text_start);
    for (k, c) in h.text.chars().enumerate().skip(lead) {
        let on = k >= h.text_start && k < h.text_start + len;
        let st = if on { Style::new().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT) };
        x = put(buf, x, r.y, c, end, st.bg(bg));
        if x >= end {
            break;
        }
    }
}

/// Writes one char at `x` (if it fits before `end`) and returns the next x.
fn put(buf: &mut Buffer, x: u16, y: u16, c: char, end: u16, st: Style) -> u16 {
    if x >= end {
        return x;
    }
    let mut tmp = [0u8; 4];
    let (nx, _) = buf.set_stringn(x, y, c.encode_utf8(&mut tmp), (end - x) as usize, st);
    nx
}
