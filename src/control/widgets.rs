//! Drawing pieces the Control Center and Find Action are made of: a tile (rounded panel with a
//! title and a summary), a list row (left text, right value, tone, selection), a status chip, a
//! key legend, a sparkline and a centred popup box. All colours come from `ui::theme` (themes are
//! applied to the finished frame), so restyling them (track E, decision 0017) happens here.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Clear, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::theme::*;

/// How a value reads: its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Good,
    Warn,
    Bad,
    Muted,
    Accent,
}

impl Tone {
    pub fn color(self) -> ratatui::style::Color {
        match self {
            Tone::Normal => TEXT,
            Tone::Good => GOOD,
            Tone::Warn => WARNING,
            Tone::Bad => CRITICAL,
            Tone::Muted => MUTED,
            Tone::Accent => ACCENT,
        }
    }
}

/// Writes `text` from `x` with at most `max` columns (cut with … when longer). Returns the
/// column after it.
pub fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, max: u16, style: Style) -> u16 {
    if max == 0 {
        return x;
    }
    let s = fit(text, max as usize);
    let (end, _) = buf.set_stringn(x, y, &s, max as usize, style);
    end
}

/// `s` cut to `w` columns (with … when it was longer).
pub fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    if w > 0 {
        out.push('…');
    }
    out
}

/// A tile: rounded border (accent when focused), ` GLYPH TITLE ` on the top left, `summary`
/// on the top right, SURFACE_1 inside. Returns the inner area.
pub fn tile(buf: &mut Buffer, r: Rect, glyph: &str, title: &str, summary: &str, summary_tone: Tone, focused: bool) -> Rect {
    if r.width < 6 || r.height < 2 {
        return Rect::default();
    }
    Clear.render(r, buf);
    let border = if focused { ACCENT } else { BASELINE };
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(border).bg(SURFACE_1)).style(Style::new().bg(SURFACE_1).fg(TEXT)).render(r, buf);
    let x = r.x + 2;
    let title_style = Style::new().fg(if focused { ACCENT } else { TEXT_2 }).bg(SURFACE_1).add_modifier(Modifier::BOLD);
    let max = r.width.saturating_sub(4);
    let mut end = put(buf, x, r.y, &format!(" {glyph} "), max, Style::new().fg(ACCENT).bg(SURFACE_1));
    end = put(buf, end, r.y, &format!("{title} "), (r.x + 2 + max).saturating_sub(end), title_style);
    let sw = summary.width() as u16 + 2;
    if !summary.is_empty() && end + sw + 2 <= r.right() - 1 {
        put(buf, r.right() - 1 - sw, r.y, &format!(" {summary} "), sw, Style::new().fg(summary_tone.color()).bg(SURFACE_1));
    }
    Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height.saturating_sub(2))
}

/// A list row: `›` marker and SELECTION background when selected (accent marker only when the
/// list has the focus), left text, value right-aligned in its tone.
pub fn row(buf: &mut Buffer, r: Rect, left: &str, left_tone: Tone, right: &str, tone: Tone, selected: bool, focused: bool) {
    if r.width < 4 {
        return;
    }
    let bg = if selected && focused { SELECTION } else { SURFACE_1 };
    buf.set_style(r, Style::new().bg(bg));
    let mark = if selected { "›" } else { " " };
    buf.set_string(r.x, r.y, mark, Style::new().fg(if focused { ACCENT } else { MUTED }).bg(bg));
    let inner = r.width.saturating_sub(3);
    let rw = (right.width() as u16).min(inner.saturating_sub(4) * 2 / 3);
    let lw = inner.saturating_sub(rw + if rw > 0 { 1 } else { 0 });
    put(buf, r.x + 2, r.y, left, lw, Style::new().fg(left_tone.color()).bg(bg));
    if rw > 0 {
        put(buf, r.x + 2 + inner - rw, r.y, right, rw, Style::new().fg(tone.color()).bg(bg));
    }
}

/// A section label inside a tile ("AA STAFF"), with a hairline after it.
pub fn section(buf: &mut Buffer, r: Rect, label: &str) {
    buf.set_style(r, Style::new().bg(SURFACE_1));
    let end = put(buf, r.x + 1, r.y, label, r.width.saturating_sub(2), Style::new().fg(MUTED).bg(SURFACE_1).add_modifier(Modifier::BOLD));
    for x in end + 1..r.right().saturating_sub(1) {
        buf[(x, r.y)].set_char('─').set_fg(GRID);
    }
}

/// A status chip: `glyph text` in its tone. Returns the column after it (plus a gap).
pub fn chip(buf: &mut Buffer, x: u16, y: u16, glyph: &str, text: &str, tone: Tone, right: u16) -> u16 {
    let s = format!("{glyph} {text}");
    let w = s.width() as u16;
    if x + w > right {
        return x;
    }
    buf.set_string(x, y, glyph, Style::new().fg(tone.color()));
    buf.set_string(x + glyph.width() as u16 + 1, y, text, Style::new().fg(if tone == Tone::Muted { MUTED } else { TEXT_2 }));
    x + w + 3
}

/// A key legend: `key what · key what …`, keys in the accent colour, cut where it doesn't fit.
pub fn keys(buf: &mut Buffer, r: Rect, items: &[(&str, &str)], bg: ratatui::style::Color) {
    buf.set_style(r, Style::new().bg(bg));
    let mut x = r.x + 1;
    for (i, (k, what)) in items.iter().enumerate() {
        let need = (k.width() + 1 + what.width() + if i > 0 { 3 } else { 0 }) as u16;
        if x + need > r.right() {
            break;
        }
        if i > 0 {
            buf.set_string(x, r.y, " · ", Style::new().fg(GRID).bg(bg));
            x += 3;
        }
        buf.set_string(x, r.y, k, Style::new().fg(ACCENT).bg(bg));
        x += k.width() as u16 + 1;
        buf.set_string(x, r.y, what, Style::new().fg(MUTED).bg(bg));
        x += what.width() as u16;
    }
}

/// `▁▂▃▅▇` for values 0..=max (the newest last).
pub fn sparkline(values: &[f32], width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let vs = &values[values.len().saturating_sub(width)..];
    let max = vs.iter().cloned().fold(1.0f32, f32::max);
    vs.iter().map(|v| BARS[((v / max) * 7.0).round().clamp(0.0, 7.0) as usize]).collect()
}

/// A centred popup box like the project popups (rounded, SURFACE_1, accent title, muted bottom
/// hint). Returns (the box, its inner area).
pub fn popup(buf: &mut Buffer, area: Rect, w: u16, h: u16, title: &str, bottom: &str) -> Option<(Rect, Rect)> {
    let w = w.min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(2));
    if w < 20 || h < 6 {
        return None;
    }
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 3, w, h);
    Clear.render(r, buf);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BASELINE).bg(SURFACE_1))
        .style(Style::new().bg(SURFACE_1).fg(TEXT))
        .title(format!(" {title} "))
        .title_style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
        .title_bottom(format!(" {bottom} "))
        .render(r, buf);
    for x in r.x + 1..r.right() - 1 {
        let c = &mut buf[(x, r.bottom() - 1)];
        if c.symbol() != "─" {
            c.set_fg(MUTED);
        }
    }
    Some((r, Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_and_sparkline() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(sparkline(&[0.0, 5.0, 10.0], 10), "▁▅█");
        assert_eq!(sparkline(&[1.0, 2.0, 3.0, 4.0], 2).chars().count(), 2);
    }

    #[test]
    fn tile_and_row_draw_inside_their_box() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 5));
        let inner = tile(&mut buf, Rect::new(0, 0, 30, 5), "◆", "AGENTS", "2 running", Tone::Good, true);
        assert_eq!(inner, Rect::new(1, 1, 28, 3));
        let top: String = (0..30).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(top.starts_with("╭─ ◆ AGENTS") && top.contains("2 running"), "{top}");
        assert_eq!(buf[(0, 0)].fg, ACCENT);
        row(&mut buf, Rect::new(1, 1, 28, 1), "#3 fix it", Tone::Normal, "done", Tone::Good, true, true);
        let line: String = (1..29).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(line.starts_with("› #3 fix it") && line.trim_end().ends_with("done"), "{line}");
        assert_eq!(buf[(5, 1)].bg, SELECTION);
    }
}
