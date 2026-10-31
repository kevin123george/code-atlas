//! Find Action's popup: input line, then one row per action (name with the matched letters in
//! the accent colour, its group, a live value for toggles, its key on the right).

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::control::widgets::{self, put};
use crate::ui::theme::*;

pub fn draw(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let hits = app.palette.hits();
    let kitty = app.hints.kitty;
    let bottom = "↑↓ select · Enter run · Alt+O control center · Esc close";
    let (r, inner) = widgets::popup(buf, area, 96, 22, "Find Action", bottom)?;
    let p = &mut app.palette;
    p.rect = r;
    // Input line + count, then a divider.
    buf.set_string(inner.x, inner.y, "› ", Style::new().fg(ACCENT).bg(SURFACE_1));
    let room = inner.width.saturating_sub(14);
    let n = p.input.chars().count();
    let shown: String = p.input.chars().skip(n.saturating_sub(room as usize)).collect();
    let cx = put(buf, inner.x + 2, inner.y, &shown, room, Style::new().fg(TEXT).bg(SURFACE_1));
    let count = format!("{} actions", hits.len());
    put(buf, inner.right() - count.width() as u16, inner.y, &count, 12, Style::new().fg(MUTED).bg(SURFACE_1));
    if p.input.is_empty() {
        put(buf, inner.x + 2, inner.y, "type an action: theme, vim, commit, restart…", room, Style::new().fg(MUTED).bg(SURFACE_1));
    }
    for x in r.x + 1..r.right() - 1 {
        buf[(x, inner.y + 1)].set_char('─').set_fg(GRID).set_bg(SURFACE_1);
    }
    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    p.list_rect = list;
    let lh = list.height as usize;
    p.selected = p.selected.min(hits.len().saturating_sub(1));
    if p.selected < p.scroll {
        p.scroll = p.selected;
    } else if lh > 0 && p.selected >= p.scroll + lh {
        p.scroll = p.selected + 1 - lh;
    }
    if hits.is_empty() {
        put(buf, list.x, list.y, "No action matches", list.width, Style::new().fg(MUTED).bg(SURFACE_1));
    }
    let recent = p.recent.clone();
    let (sel, scroll) = (p.selected, p.scroll);
    for (row, h) in hits.iter().enumerate().skip(scroll).take(lh) {
        let y = list.y + (row - scroll) as u16;
        let selected = row == sel;
        let bg = if selected { SELECTION } else { SURFACE_1 };
        buf.set_style(Rect::new(list.x, y, list.width, 1), Style::new().bg(bg));
        let a = h.action;
        let key = a.shown_key(kitty).unwrap_or("");
        let state = a.state.map(|f| f(app)).unwrap_or_default();
        let kw = key.width() as u16;
        let right_w = kw + if state.is_empty() { 0 } else { state.width() as u16 + 3 };
        let name_room = list.width.saturating_sub(right_w + 2);
        // Name, matched letters in the accent colour.
        let mut x = list.x + 1;
        let style = Style::new().fg(TEXT).bg(bg);
        for (i, c) in a.name.chars().enumerate() {
            let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1) as u16;
            if x + w > list.x + 1 + name_room {
                break;
            }
            let st = if h.pos.contains(&i) { style.fg(ACCENT).add_modifier(Modifier::BOLD) } else { style };
            buf.set_string(x, y, c.to_string(), st);
            x += w;
        }
        let tag = if recent.iter().any(|r| r == a.id) && app.palette.input.is_empty() { "recent".to_string() } else { a.group.to_string() };
        let tag_room = (list.x + 1 + name_room).saturating_sub(x + 2);
        if tag_room > 4 {
            put(buf, x + 2, y, &tag, tag_room, Style::new().fg(MUTED).bg(bg));
        }
        let mut rx = list.right().saturating_sub(right_w + 1);
        if !state.is_empty() {
            put(buf, rx, y, &state, state.width() as u16, Style::new().fg(GOOD).bg(bg));
            rx += state.width() as u16 + 3;
        }
        put(buf, rx, y, key, kw, Style::new().fg(if selected { ACCENT } else { TEXT_2 }).bg(bg));
    }
    Some(Position::new(cx.min(inner.right() - 1), inner.y))
}
