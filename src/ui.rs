//! Drawing: tab bar, line-number gutter and text, status bar. Colours are the
//! Cashflow Atlas "Ledger Terminal" dark tokens (finance-tracker `index.html`).

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use crate::app::{App, Layout};
use crate::doc::char_width;

pub mod theme {
    use ratatui::style::Color;
    pub const PAPER: Color = Color::Rgb(0x09, 0x0b, 0x10);
    pub const SURFACE_1: Color = Color::Rgb(0x10, 0x13, 0x1b);
    pub const SURFACE_2: Color = Color::Rgb(0x16, 0x1a, 0x24);
    pub const TEXT: Color = Color::Rgb(0xe8, 0xec, 0xf5);
    pub const TEXT_2: Color = Color::Rgb(0x9a, 0xa4, 0xb8);
    pub const MUTED: Color = Color::Rgb(0x5c, 0x65, 0x77);
    pub const GRID: Color = Color::Rgb(0x21, 0x26, 0x36);
    pub const BASELINE: Color = Color::Rgb(0x2b, 0x31, 0x43);
    pub const ACCENT: Color = Color::Rgb(0x2c, 0xe0, 0xf5);
    pub const GOOD: Color = Color::Rgb(0x2f, 0xd1, 0x8f);
    pub const CRITICAL: Color = Color::Rgb(0xff, 0x6a, 0x60);
    pub const WARNING: Color = Color::Rgb(0xfa, 0xb2, 0x19);
    /// Selection: the accent mixed into the surface.
    pub const SELECTION: Color = Color::Rgb(0x14, 0x3a, 0x4a);
}

use theme::*;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.height < 3 || area.width < 10 {
        return;
    }
    let buf = f.buffer_mut();
    buf.set_style(area, Style::new().bg(PAPER).fg(TEXT));

    draw_tabs(buf, app, Rect::new(area.x, area.y, area.width, 1));

    let text_area = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    let cursor = draw_text(buf, app, text_area);

    let status = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let prompt_cursor = draw_status(buf, app, status);

    if let Some(p) = prompt_cursor.or(cursor) {
        f.set_cursor_position(p);
    }
}

fn draw_tabs(buf: &mut Buffer, app: &mut App, area: Rect) {
    buf.set_style(area, Style::new().bg(SURFACE_1));
    let brand = " ◈ Code Atlas ";
    buf.set_string(area.x, area.y, brand, Style::new().fg(ACCENT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
    let mut x = area.x + brand.chars().count() as u16;
    app.tab_spans.clear();
    for (i, d) in app.docs.iter().enumerate() {
        let title = format!(" {}{} ", d.name(), if d.dirty() { " ●" } else { "" });
        let w = title.chars().count() as u16;
        let style = if i == app.active {
            Style::new().fg(TEXT).bg(SURFACE_2).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(TEXT_2).bg(SURFACE_1)
        };
        if x + w > area.right() {
            break;
        }
        buf.set_string(x, area.y, &title, style);
        if i == app.active {
            // Accent underline effect on the active tab's first cell.
            buf[(x, area.y)].set_fg(ACCENT).set_symbol("▎");
        }
        app.tab_spans.push((x, x + w));
        x += w;
    }
}

fn draw_text(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let follow = app.follow;
    let d = &mut app.docs[app.active];
    let total = d.visible_lines();
    let gutter_w = (total.to_string().len().max(3) + 2) as u16;
    let text_x = area.x + gutter_w;
    let text_w = area.width.saturating_sub(gutter_w) as usize;
    let h = area.height as usize;
    d.view_h = h;
    if follow {
        d.ensure_cursor_visible(h, text_w);
    }
    app.layout = Layout { text_x, text_y: area.y, text_h: area.height };

    let cur_line = d.line();
    let sel = d.selection();
    for row in 0..h {
        let line = d.scroll_line + row;
        let y = area.y + row as u16;
        if line >= total {
            buf.set_string(area.x, y, format!("{:>w$} ", "~", w = gutter_w as usize - 2), Style::new().fg(BASELINE));
            continue;
        }
        let is_cur = line == cur_line;
        let line_bg = if is_cur { SURFACE_1 } else { PAPER };
        buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(line_bg));
        let num_style = if is_cur { Style::new().fg(ACCENT) } else { Style::new().fg(MUTED) };
        buf.set_string(area.x, y, format!("{:>w$}  ", line + 1, w = gutter_w as usize - 2), num_style.bg(line_bg));

        let start = d.line_start(line);
        let len = d.line_len(line);
        let mut col = 0usize;
        for (i, c) in d.rope.line(line).chars().take(len).enumerate() {
            let w = char_width(c, col);
            let pos = start + i;
            let selected = sel.is_some_and(|(s, e)| pos >= s && pos < e);
            let style = Style::new().fg(TEXT).bg(if selected { SELECTION } else { line_bg });
            for k in 0..w {
                let vc = col + k;
                if vc < d.scroll_col || vc >= d.scroll_col + text_w {
                    continue;
                }
                let x = text_x + (vc - d.scroll_col) as u16;
                let cell = &mut buf[(x, y)];
                cell.set_style(style);
                if k == 0 && c != '\t' && !c.is_control() {
                    cell.set_char(c);
                } else if c.is_control() && c != '\t' {
                    cell.set_char('?').set_fg(CRITICAL);
                } else if c == '\t' {
                    cell.set_char(if k == 0 { '→' } else { ' ' }).set_fg(if k == 0 { GRID } else { TEXT });
                } else {
                    // Second cell of a wide character.
                    cell.set_symbol("");
                }
            }
            col += w;
        }
        // Show a selected line break as one selected cell.
        if sel.is_some_and(|(s, e)| start + len >= s && start + len < e && line + 1 < total) {
            let vc = col;
            if vc >= d.scroll_col && vc < d.scroll_col + text_w {
                buf[(text_x + (vc - d.scroll_col) as u16, y)].set_bg(SELECTION);
            }
        }
    }

    let line = d.line();
    let col = d.display_col(line, d.col());
    let visible = line >= d.scroll_line && line < d.scroll_line + h && col >= d.scroll_col && col < d.scroll_col + text_w;
    visible.then(|| Position::new(text_x + (col - d.scroll_col) as u16, area.y + (line - d.scroll_line) as u16))
}

fn draw_status(buf: &mut Buffer, app: &App, area: Rect) -> Option<Position> {
    buf.set_style(area, Style::new().bg(SURFACE_2).fg(TEXT_2));
    let d = app.doc();
    let right = format!(
        " Ln {}, Col {}{} · {} · UTF-8 · IDEA ",
        d.line() + 1,
        d.col() + 1,
        d.selection().map(|(s, e)| format!(" ({} chars)", e - s)).unwrap_or_default(),
        if d.crlf { "CRLF" } else { "LF" },
    );
    let rw = right.chars().count() as u16;
    if rw < area.width {
        buf.set_string(area.right() - rw, area.y, &right, Style::new().fg(TEXT_2).bg(SURFACE_2));
    }
    if let Some(p) = &app.prompt {
        let label = format!(" {}: ", p.kind.label());
        buf.set_string(area.x, area.y, &label, Style::new().fg(ACCENT).bg(SURFACE_2).add_modifier(Modifier::BOLD));
        let x = area.x + label.chars().count() as u16;
        buf.set_string(x, area.y, &p.input, Style::new().fg(TEXT).bg(SURFACE_2));
        return Some(Position::new((x + p.input.chars().count() as u16).min(area.right() - 1), area.y));
    }
    let (text, color) = match &app.message {
        Some(m) if m.starts_with("Couldn't") || m.starts_with("Not found") => (m.clone(), CRITICAL),
        Some(m) if m.contains("not saved") || m.contains("Not saved") => (m.clone(), WARNING),
        Some(m) if m.starts_with("Saved") => (m.clone(), GOOD),
        Some(m) => (m.clone(), TEXT_2),
        None => (d.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "untitled".into()), MUTED),
    };
    let max = area.width.saturating_sub(rw + 2) as usize;
    let text: String = text.chars().take(max).collect();
    buf.set_string(area.x + 1, area.y, text, Style::new().fg(color).bg(SURFACE_2));
    None
}
