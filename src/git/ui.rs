//! Drawing for git: gutter marks, the Commit panel (a dock tab), the diff view, the branch in the
//! status bar and the tree colours.

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Clear, Widget};

use super::diff::{LineKind, Mark};
use super::status::Kind;
use super::{Field, Git};
use crate::ui::theme::*;

/// "Modified" blue from the Ledger Terminal series palette (same as M3's `S_BLUE`).
pub const BLUE: Color = Color::Rgb(0x5b, 0x9b, 0xf0);
/// Diff row backgrounds: GOOD / CRITICAL mixed into the paper.
pub const ADD_BG: Color = Color::Rgb(0x0f, 0x2a, 0x22);
pub const DEL_BG: Color = Color::Rgb(0x33, 0x16, 0x18);

pub fn kind_color(k: Kind) -> Color {
    match k {
        Kind::Modified | Kind::Renamed => BLUE,
        Kind::Added | Kind::Untracked => GOOD,
        Kind::Deleted => MUTED,
        Kind::Conflict => CRITICAL,
        Kind::Ignored => BASELINE,
    }
}

/// Colour for a tree row, when git has something to say about it.
pub fn tree_color(git: &Git, path: &Path, is_dir: bool) -> Option<Color> {
    git.kind_of(path, is_dir).map(kind_color)
}

/// Draws a gutter mark at (x, y).
pub fn draw_mark(buf: &mut Buffer, x: u16, y: u16, m: Mark) {
    let (sym, fg) = match m {
        Mark::Added => ("▎", GOOD),
        Mark::Modified => ("▎", BLUE),
        Mark::DeletedBelow => ("▁", CRITICAL),
        Mark::DeletedAbove => ("▔", CRITICAL),
    };
    buf[(x, y)].set_symbol(sym).set_fg(fg);
}

/// Draws the branch just left of the status bar's right part; returns the width used.
pub fn draw_branch(buf: &mut Buffer, git: &Git, area: Rect, right_w: u16) -> u16 {
    let Some(b) = &git.branch else { return 0 };
    let label = format!(" ⎇ {b} ·");
    let w = label.chars().count() as u16;
    if right_w + w + 20 > area.width {
        return 0;
    }
    buf.set_string(area.right() - right_w - w, area.y, &label, Style::new().fg(TEXT_2).bg(SURFACE_2));
    w
}

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, max: u16, style: Style) -> u16 {
    if max == 0 {
        return x;
    }
    buf.set_stringn(x, y, s, max as usize, style).0
}

/// The Commit panel inside the dock body. Returns the cursor when the message field is focused.
pub fn draw_panel(buf: &mut Buffer, git: &mut Git, area: Rect, focused: bool) -> Option<Position> {
    buf.set_style(area, Style::new().bg(PAPER).fg(TEXT));
    if area.height < 2 || area.width < 20 {
        return None;
    }
    if git.root.is_none() {
        put(buf, area.x + 1, area.y, "Not a git repository (git init to start one)", area.width - 2, Style::new().fg(MUTED));
        return None;
    }
    let wide = area.width >= 70;
    let (list, msg) = if wide {
        let lw = area.width * 3 / 5;
        (Rect::new(area.x, area.y, lw, area.height), Rect::new(area.x + lw + 1, area.y, area.width - lw - 1, area.height))
    } else {
        let mh = 3.min(area.height / 2);
        (
            Rect::new(area.x, area.y, area.width, area.height - mh),
            Rect::new(area.x, area.bottom() - mh, area.width, mh),
        )
    };
    if wide {
        for y in area.y..area.bottom() {
            buf[(list.right(), y)].set_char('│').set_fg(GRID);
        }
    }

    // List: a summary row, then one row per change.
    let committing = git.committing();
    let p = &mut git.panel;
    let list_focus = focused && p.field == Field::List;
    let head = format!(" Changes {} · {} ticked", git.entries.len(), p.checked.len());
    put(buf, list.x, list.y, &head, list.width, Style::new().fg(if list_focus { ACCENT } else { TEXT_2 }).add_modifier(Modifier::BOLD));
    let rows_h = list.height.saturating_sub(1) as usize;
    if p.cursor < p.scroll {
        p.scroll = p.cursor;
    } else if rows_h > 0 && p.cursor >= p.scroll + rows_h {
        p.scroll = p.cursor + 1 - rows_h;
    }
    if git.entries.is_empty() {
        put(buf, list.x + 1, list.y + 1, "No changes. Working tree clean.", list.width.saturating_sub(2), Style::new().fg(MUTED));
    }
    for (row, (i, e)) in git.entries.iter().enumerate().skip(p.scroll).take(rows_h).enumerate() {
        let y = list.y + 1 + row as u16;
        let sel = i == p.cursor;
        let bg = match (sel, list_focus) {
            (true, true) => SELECTION,
            (true, false) => SURFACE_2,
            _ => PAPER,
        };
        buf.set_style(Rect::new(list.x, y, list.width, 1), Style::new().bg(bg));
        let tick = if p.checked.contains(&e.path) { "[x]" } else { "[ ]" };
        let k = e.kind();
        let mut x = put(buf, list.x + 1, y, tick, list.width.saturating_sub(1), Style::new().fg(if tick == "[x]" { ACCENT } else { MUTED }).bg(bg));
        x = put(buf, x + 1, y, &k.letter().to_string(), list.right().saturating_sub(x + 1), Style::new().fg(kind_color(k)).bg(bg).add_modifier(Modifier::BOLD));
        let name = match &e.orig {
            Some(o) => format!("{} ← {o}", e.path),
            None => e.path.clone(),
        };
        let style = Style::new().fg(if k == Kind::Deleted { MUTED } else { kind_color(k) }).bg(bg);
        let style = if k == Kind::Deleted { style.add_modifier(Modifier::CROSSED_OUT) } else { style };
        put(buf, x + 1, y, &name, list.right().saturating_sub(x + 2), style);
    }

    // Message field.
    let msg_focus = focused && p.field == Field::Message;
    let title = if committing { " Committing…" } else { " Commit message" };
    put(buf, msg.x, msg.y, title, msg.width, Style::new().fg(if msg_focus { ACCENT } else { TEXT_2 }).add_modifier(Modifier::BOLD));
    let hint = " Ctrl+Enter / Alt+Enter commit ";
    let hw = hint.chars().count() as u16;
    if msg.width > hw + 16 {
        put(buf, msg.right() - hw, msg.y, hint, hw, Style::new().fg(MUTED));
    }
    let body = Rect::new(msg.x, msg.y + 1, msg.width, msg.height.saturating_sub(1));
    buf.set_style(body, Style::new().bg(if msg_focus { SURFACE_1 } else { PAPER }));
    let lines: Vec<&str> = p.message.split('\n').collect();
    let first = lines.len().saturating_sub(body.height as usize);
    if p.message.is_empty() && !msg_focus {
        put(buf, body.x + 1, body.y, "Tab here to write one", body.width.saturating_sub(2), Style::new().fg(MUTED));
    }
    let bg = if msg_focus { SURFACE_1 } else { PAPER };
    let mut cursor = None;
    for (row, line) in lines.iter().enumerate().skip(first) {
        let y = body.y + (row - first) as u16;
        if y >= body.bottom() {
            break;
        }
        // Long lines show their end, where the typing happens.
        let room = body.width.saturating_sub(2) as usize;
        let n = line.chars().count();
        let shown: String = line.chars().skip(n.saturating_sub(room)).collect();
        let end = put(buf, body.x + 1, y, &shown, room as u16, Style::new().fg(TEXT).bg(bg));
        cursor = Some(Position::new(end.min(body.right().saturating_sub(1)), y));
    }
    cursor.filter(|_| msg_focus && body.height > 0)
}

/// The diff view over the editor area.
pub fn draw_diff(buf: &mut Buffer, git: &mut Git, area: Rect) {
    let Some(v) = &mut git.diff else { return };
    // The editor was drawn underneath: wipe it.
    Clear.render(area, buf);
    buf.set_style(area, Style::new().bg(PAPER).fg(TEXT));
    if area.height < 3 || area.width < 20 {
        return;
    }
    // Title bar.
    let bar = Rect::new(area.x, area.y, area.width, 1);
    buf.set_style(bar, Style::new().bg(SURFACE_1));
    let mut x = put(buf, area.x, area.y, " Diff ", area.width, Style::new().fg(ACCENT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
    x = put(buf, x, area.y, &v.path, area.right().saturating_sub(x), Style::new().fg(kind_color(v.kind)).bg(SURFACE_1).add_modifier(Modifier::BOLD));
    x = put(buf, x, area.y, "  HEAD → working tree  ", area.right().saturating_sub(x), Style::new().fg(MUTED).bg(SURFACE_1));
    x = put(buf, x, area.y, &format!("+{}", v.added), area.right().saturating_sub(x), Style::new().fg(GOOD).bg(SURFACE_1));
    x = put(buf, x + 1, area.y, &format!("−{}", v.deleted), area.right().saturating_sub(x + 1), Style::new().fg(CRITICAL).bg(SURFACE_1));
    let hint = if area.width >= 110 { " F7/Shift+F7 hunk · Alt+↑↓ file · F4 source · Esc close " } else { " F7 hunk · F4 source · Esc " };
    let hw = hint.chars().count() as u16;
    if x + hw + 2 <= area.right() {
        put(buf, area.right() - hw, area.y, hint, hw, Style::new().fg(MUTED).bg(SURFACE_1));
    }

    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    v.view_h = body.height as usize;
    let mut y = body.y;
    if let Some(note) = &v.note {
        put(buf, body.x + 1, y, note, body.width.saturating_sub(2), Style::new().fg(WARNING));
        y += 1;
    }
    let max_scroll = v.rows.len().saturating_sub(1);
    v.scroll = v.scroll.min(max_scroll);
    let num_w = v.rows.iter().filter_map(|r| r.old.max(r.new)).max().unwrap_or(1).to_string().len().max(3);
    for r in v.rows.iter().skip(v.scroll) {
        if y >= body.bottom() {
            break;
        }
        let row = Rect::new(body.x, y, body.width, 1);
        let num = |n: Option<usize>| n.map(|n| format!("{n:>num_w$}")).unwrap_or_else(|| " ".repeat(num_w));
        match r.kind {
            LineKind::Hunk => {
                buf.set_style(row, Style::new().bg(SURFACE_1));
                put(buf, body.x + 1, y, &r.text, body.width.saturating_sub(2), Style::new().fg(ACCENT).bg(SURFACE_1));
            }
            k => {
                let (bg, sign, fg) = match k {
                    LineKind::Added => (ADD_BG, "+", GOOD),
                    LineKind::Deleted => (DEL_BG, "−", CRITICAL),
                    _ => (PAPER, " ", TEXT_2),
                };
                buf.set_style(row, Style::new().bg(bg));
                let gutter = format!(" {} {} ", num(r.old), num(r.new));
                let mut x = put(buf, body.x, y, &gutter, body.width, Style::new().fg(MUTED).bg(bg));
                x = put(buf, x, y, sign, body.right().saturating_sub(x), Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD));
                let text = r.text.replace('\t', "    ");
                let tf = if k == LineKind::Context { TEXT_2 } else { TEXT };
                put(buf, x + 1, y, &text, body.right().saturating_sub(x + 1), Style::new().fg(tf).bg(bg));
            }
        }
        y += 1;
    }
}
