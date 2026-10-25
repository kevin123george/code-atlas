//! Drawing a terminal's vt100 screen into a ratatui buffer.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use super::Terminal;
use crate::ui::theme::*;

/// ANSI colours 0–15 tuned to the Ledger Terminal theme.
const ANSI: [Color; 16] = [
    Color::Rgb(0x21, 0x26, 0x36),
    CRITICAL,
    GOOD,
    WARNING,
    Color::Rgb(0x5b, 0x9b, 0xf0),
    Color::Rgb(0xf2, 0x8a, 0xb5),
    ACCENT,
    Color::Rgb(0xc8, 0xce, 0xde),
    MUTED,
    Color::Rgb(0xff, 0x8a, 0x80),
    Color::Rgb(0x5e, 0xe6, 0xa8),
    Color::Rgb(0xff, 0xc9, 0x4d),
    Color::Rgb(0x82, 0xb4, 0xff),
    Color::Rgb(0xf7, 0xa8, 0xc8),
    Color::Rgb(0x7e, 0xea, 0xf8),
    TEXT,
];

pub fn color(c: vt100::Color, default: Color) -> Color {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) if i < 16 => ANSI[i as usize],
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Draws the screen into `area`; returns the cursor position when it should show.
pub fn draw(buf: &mut Buffer, t: &Terminal, area: Rect, focused: bool) -> Option<Position> {
    buf.set_style(area, Style::new().bg(PAPER).fg(TEXT));
    let s = t.screen();
    let (rows, cols) = s.size();
    for r in 0..rows.min(area.height) {
        for c in 0..cols.min(area.width) {
            let Some(cell) = s.cell(r, c) else { continue };
            let (x, y) = (area.x + c, area.y + r);
            if cell.is_wide_continuation() {
                buf[(x, y)].set_symbol("");
                continue;
            }
            let (mut fg, mut bg) = (color(cell.fgcolor(), TEXT), color(cell.bgcolor(), PAPER));
            if cell.inverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut style = Style::new().fg(fg).bg(bg);
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let out = &mut buf[(x, y)];
            out.set_style(style);
            if cell.has_contents() {
                out.set_symbol(&cell.contents());
            } else {
                out.set_char(' ');
            }
        }
    }
    let scrolled = s.scrollback() > 0;
    if scrolled {
        let tag = format!(" ↑ {} lines back · any key returns ", s.scrollback());
        let w = (tag.chars().count() as u16).min(area.width);
        buf.set_stringn(area.right() - w, area.y, &tag, w as usize, Style::new().fg(PAPER).bg(WARNING));
    }
    if let Some(e) = &t.exited {
        let note = format!(" {e} · Enter starts a new shell · Alt+F12 hide ");
        let y = area.bottom() - 1;
        buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(SURFACE_2));
        buf.set_stringn(area.x, y, &note, area.width as usize, Style::new().fg(WARNING).bg(SURFACE_2));
        return None;
    }
    let (cr, cc) = s.cursor_position();
    (focused && !scrolled && !s.hide_cursor() && cr < area.height && cc < area.width)
        .then(|| Position::new(area.x + cc, area.y + cr))
}
