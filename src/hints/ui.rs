//! Drawing the shortcut helper as "glass": no background fill. The cells under the panel keep
//! their characters and backgrounds but are repainted in `theme::GHOST` (just above the paper),
//! and the hint text is drawn on top with a foreground colour only. Each text run gets one blank
//! cell on either side (and the column after the edge is blank) so dimmed code never touches
//! it; the rest of the gap between key and description and the padding stay see-through.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use super::table::{self, Hint};
use crate::app::App;
use crate::ui::theme::*;

pub const MIN_W: u16 = 30;
pub const MAX_W: u16 = 38;
/// The editor keeps at least this many columns next to the panel (else the panel hides).
const ROOM: u16 = 12;

/// One line of the panel.
pub struct Line {
    pub key: String,
    pub does: String,
    pub tip: bool,
}

/// The lines for the current context: an optional tip first, then the table rows (8 at most).
pub fn lines(rows: &[Hint], kitty: bool, tip: Option<Hint>) -> Vec<Line> {
    let mut out: Vec<Line> = tip.map(|t| Line { key: t.shown(kitty).into(), does: t.does.into(), tip: true }).into_iter().collect();
    for r in rows.iter().take(8 - out.len()) {
        out.push(Line { key: r.shown(kitty).into(), does: r.does.into(), tip: false });
    }
    out
}

/// Width of the panel for these lines and title, and the key column's width.
pub fn size(lines: &[Line], title: &str) -> (u16, u16, u16) {
    let rows = lines.iter().filter(|l| !l.tip);
    let key_w = rows.clone().map(|l| l.key.width()).max().unwrap_or(0) as u16;
    let does_w = rows.map(|l| l.does.width()).max().unwrap_or(0) as u16;
    // "│ " + key + gap + text + margin; the title row has the toggle at its right end; a tip
    // row is "│ Tip: key  text ".
    let title_w = (2 + title.width() + 2 + table::TOGGLE.width() + 1) as u16;
    let tip_w = lines.iter().filter(|l| l.tip).map(|l| 2 + 5 + l.key.width() + 2 + l.does.width() + 1).max().unwrap_or(0) as u16;
    let w = (2 + key_w + 2 + does_w + 1).max(title_w).max(tip_w).clamp(MIN_W, MAX_W);
    (w, 1 + lines.len() as u16, key_w)
}

/// Where the panel goes in the editor `area`: the bottom-right corner, or the top-right corner
/// when the cursor's line (`cursor_row`, a screen row) would be under it. `None` = no room (or
/// the cursor is under both corners).
pub fn place(area: Rect, w: u16, h: u16, cursor_row: Option<u16>) -> Option<Rect> {
    if area.width < w + ROOM || area.height < h + 2 {
        return None;
    }
    let x = area.right() - w - 1;
    let under = |r: Rect| cursor_row.is_some_and(|y| y >= r.y && y < r.bottom());
    let bottom = Rect::new(x, area.bottom() - h, w, h);
    if !under(bottom) {
        return Some(bottom);
    }
    let top = Rect::new(x, area.y, w, h);
    (!under(top)).then_some(top)
}

/// Repaints the cells of `r` in the ghost colour, keeping characters and backgrounds.
pub fn dim(buf: &mut Buffer, r: Rect) {
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            let c = &mut buf[(x, y)];
            c.set_fg(GHOST);
            c.modifier = Modifier::empty();
            c.underline_color = Color::Reset;
        }
    }
}

/// Writes `s` from `x` (foreground only, so the background shows through), at most to `right`,
/// with one blank cell on each side so dimmed code never touches the text. Returns the end.
fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, right: u16, style: Style) -> u16 {
    if x >= right {
        return x;
    }
    let (nx, _) = buf.set_stringn(x, y, s, (right - x) as usize, style);
    for bx in [x.saturating_sub(1), nx] {
        if bx < right {
            buf[(bx, y)].set_symbol(" ");
        }
    }
    nx
}

/// Draws the helper into the editor `area` (from `ui::draw`, after the text, before popups).
pub fn draw(buf: &mut Buffer, app: &mut App, area: Rect) {
    app.hints.rect = Rect::default();
    if !app.hints.on {
        return;
    }
    let ctx = super::context(app);
    let title = super::title(app, ctx);
    let tip = app.hints.current_tip(Instant::now());
    let lines = lines(table::rows(ctx), app.hints.kitty, tip);
    let (w, h, key_w) = size(&lines, &title);
    // The editor cursor's line (when the editor text is what's under the panel).
    let d = app.doc();
    let l = app.layout;
    let cursor_row = (app.dock.git.diff.is_none() && d.line() >= d.scroll_line && d.line() < d.scroll_line + l.text_h as usize)
        .then(|| l.text_y + (d.line() - d.scroll_line) as u16);
    let Some(r) = place(area, w, h, cursor_row) else { return };
    app.hints.rect = r;
    dim(buf, r);

    // Left edge: a thin rounded line in the baseline colour, then a clear column.
    buf[(r.x, r.y)].set_symbol("╭").set_fg(BASELINE);
    for y in r.y + 1..r.bottom() {
        buf[(r.x, y)].set_symbol("│").set_fg(BASELINE);
    }
    for y in r.y..r.bottom() {
        buf[(r.x + 1, y)].set_symbol(" ");
    }
    let right = r.right();
    let x0 = r.x + 2;
    put(buf, x0, r.y, &title, right, Style::new().fg(TEXT_2).add_modifier(Modifier::BOLD));
    let tw = table::TOGGLE.width() as u16;
    put(buf, right - tw - 1, r.y, table::TOGGLE, right, Style::new().fg(MUTED));
    for (i, line) in lines.iter().enumerate() {
        let y = r.y + 1 + i as u16;
        let mut x = x0;
        if line.tip {
            x = put(buf, x, y, "Tip: ", right, Style::new().fg(WARNING).add_modifier(Modifier::BOLD));
            x = put(buf, x, y, &line.key, right, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD));
            put(buf, x + 2, y, &line.does, right - 1, Style::new().fg(TEXT));
        } else {
            put(buf, x, y, &line.key, right, Style::new().fg(ACCENT));
            put(buf, x0 + key_w + 2, y, &line.does, right - 1, Style::new().fg(TEXT_2));
        }
    }
}
