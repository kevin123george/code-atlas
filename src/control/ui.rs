//! Drawing the Control Center: a title bar, a strip of status chips, the tiles (3 columns from
//! 170 columns, 2 from 110, else one column that scrolls to the selected tile, so it reads at
//! 80 columns), a key legend; or the settings screen (with a preview of settings.toml when
//! there is room). Find Action draws on top (`palette::ui`).

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use super::settings::{self, FIELDS};
use super::tiles::{self, Tile, TileId};
use super::widgets::{self, put, Tone};
use super::Page;
use crate::app::App;
use crate::ui::theme::*;

/// Draws whatever of the Control Center / Find Action is open. `None` = neither (the editor's
/// cursor stays); `Some(cursor)` otherwise.
pub fn draw(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Option<Position>> {
    if !app.control.open && !app.palette.open {
        return None;
    }
    if app.control.open && area.height >= 6 && area.width >= 30 {
        let r = Rect::new(area.x, area.y, area.width, area.height - 1);
        draw_center(buf, app, r);
    }
    if app.palette.open {
        return Some(crate::palette::ui::draw(buf, app, area));
    }
    Some(None)
}

/// How many tile columns fit.
pub fn columns(width: u16) -> usize {
    if width >= 170 {
        3
    } else if width >= 110 {
        2
    } else {
        1
    }
}

fn draw_center(buf: &mut Buffer, app: &mut App, r: Rect) {
    buf.set_style(r, Style::new().bg(PAPER).fg(TEXT));
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            buf[(x, y)].set_symbol(" ");
        }
    }
    title_bar(buf, app, Rect::new(r.x, r.y, r.width, 1));
    let legend = Rect::new(r.x, r.bottom() - 1, r.width, 1);
    let body = Rect::new(r.x + 1, r.y + 3, r.width.saturating_sub(2), r.height.saturating_sub(5));
    match app.control.page {
        Page::Overview => {
            strip(buf, app, Rect::new(r.x + 1, r.y + 1, r.width.saturating_sub(2), 1));
            overview(buf, app, body);
            widgets::keys(buf, legend, &[("Tab", "tile"), ("↑↓", "row"), ("Enter", "act"), ("r", "restart / refresh"), ("m", "agents"), ("2", "settings"), ("Ctrl+R", "find action"), ("Esc", "close")], PAPER);
        }
        Page::Settings => {
            settings_page(buf, app, Rect::new(r.x + 1, r.y + 2, r.width.saturating_sub(2), r.height.saturating_sub(4)));
            widgets::keys(buf, legend, &[("↑↓", "select"), ("Enter/Space", "change"), ("←→", "choice"), ("e", "settings.toml"), ("l", "languages.toml"), ("Esc", "back")], PAPER);
        }
    }
    if app.hints.on {
        crate::hints::ui::draw(buf, app, body);
    }
}

fn title_bar(buf: &mut Buffer, app: &App, r: Rect) {
    buf.set_style(r, Style::new().bg(SURFACE_2));
    let mut x = put(buf, r.x, r.y, " ◈ CONTROL CENTER ", r.width, Style::new().fg(ACCENT).bg(SURFACE_2).add_modifier(Modifier::BOLD));
    let name = app.project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let branch = app.dock.git.branch.as_deref().map(|b| format!("  ⎇ {b}")).unwrap_or_default();
    // Tabs on the right.
    let tabs = [(Page::Overview, " 1 Overview "), (Page::Settings, " 2 Settings ")];
    let tabs_w: u16 = tabs.iter().map(|(_, t)| t.width() as u16).sum::<u16>() + 1;
    let room = r.right().saturating_sub(x + tabs_w + 1);
    x = put(buf, x, r.y, &format!("· {name}{branch}"), room, Style::new().fg(TEXT_2).bg(SURFACE_2));
    let _ = x;
    let mut tx = r.right().saturating_sub(tabs_w);
    for (p, t) in tabs {
        let st = if app.control.page == p { Style::new().fg(PAPER).bg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_2).bg(SURFACE_2) };
        if tx + t.width() as u16 <= r.right() {
            buf.set_string(tx, r.y, t, st);
        }
        tx += t.width() as u16;
    }
}

/// The status chips under the title: agents, servers, problems, unsaved, memory, theme.
fn strip(buf: &mut Buffer, app: &App, r: Rect) {
    let ts = tiles::build(app);
    let right = r.right();
    let mut x = r.x;
    let t = |id: TileId| ts.iter().find(|t| t.id == id).expect("every tile is built");
    for (glyph, id, label) in [("◆", TileId::Agents, "agents"), ("◈", TileId::Servers, "servers"), ("▣", TileId::Editor, "editor"), ("◇", TileId::Workspace, "git")] {
        let tile = t(id);
        x = widgets::chip(buf, x, r.y, glyph, &format!("{label} {}", tile.summary), tile.tone, right);
    }
    if let Some(u) = &app.control.usage {
        x = widgets::chip(buf, x, r.y, "◷", &format!("{} · {:.0}% cpu", super::procs::fmt_kb(u.total_kb()), u.total_cpu()), Tone::Muted, right);
    }
    let _ = widgets::chip(buf, x, r.y, "◐", app.control.settings.theme.name(), Tone::Muted, right);
}

fn overview(buf: &mut Buffer, app: &mut App, body: Rect) {
    let ts = tiles::build(app);
    let cols = columns(body.width + 2);
    let c = &mut app.control;
    c.tile = c.tile.min(ts.len() - 1);
    // Keep each tile's selected row on a selectable row.
    for (i, t) in ts.iter().enumerate() {
        let sel = t.selectable();
        if !sel.contains(&c.rows[i]) {
            c.rows[i] = sel.iter().copied().find(|&r| r >= c.rows[i]).or(sel.last().copied()).unwrap_or(0);
        }
    }
    c.tile_rects.clear();
    c.row_rects.clear();
    if body.height < 3 {
        return;
    }
    if cols == 1 {
        // One column: natural heights, scrolled so the selected tile is in view.
        let hs: Vec<u16> = ts.iter().map(|t| (t.rows.len() as u16 + 2).clamp(3, 14)).collect();
        let top: Vec<u16> = hs.iter().scan(0u16, |acc, h| {
            let y = *acc;
            *acc += h;
            Some(y)
        }).collect();
        let sel_top = top[c.tile];
        let sel_bottom = sel_top + hs[c.tile];
        let off = sel_bottom.saturating_sub(body.height).min(sel_top);
        for (i, t) in ts.iter().enumerate() {
            let (y0, h) = (top[i], hs[i]);
            if y0 + h <= off || y0 >= off + body.height {
                continue;
            }
            let mut tmp = Buffer::empty(Rect::new(0, 0, body.width, h));
            let rows = draw_tile(&mut tmp, Rect::new(0, 0, body.width, h), t, i == c.tile, c.rows[i]);
            for ty in 0..h {
                let sy = y0 + ty;
                if sy < off || sy >= off + body.height {
                    continue;
                }
                let y = body.y + sy - off;
                for x in 0..body.width {
                    buf[(body.x + x, y)] = tmp[(x, ty)].clone();
                }
            }
            let vis_top = y0.max(off);
            let vis_bottom = (y0 + h).min(off + body.height);
            c.tile_rects.push(Rect::new(body.x, body.y + vis_top - off, body.width, vis_bottom - vis_top));
            for (ri, rr) in rows {
                let sy = y0 + rr.y;
                if sy >= off && sy < off + body.height {
                    c.row_rects.push((i, ri, Rect::new(body.x + rr.x, body.y + sy - off, rr.width, 1)));
                }
            }
        }
        return;
    }
    let n_rows = ts.len().div_ceil(cols) as u16;
    let gap = 1u16;
    let tw = (body.width - gap * (cols as u16 - 1)) / cols as u16;
    let th = (body.height.saturating_sub(n_rows - 1)) / n_rows;
    for (i, t) in ts.iter().enumerate() {
        let (gx, gy) = ((i % cols) as u16, (i / cols) as u16);
        let w = if gx as usize == cols - 1 { body.width - gx * (tw + gap) } else { tw };
        let h = if gy == n_rows - 1 { body.height - gy * (th + 1) } else { th };
        let rect = Rect::new(body.x + gx * (tw + gap), body.y + gy * (th + 1), w, h);
        let rows = draw_tile(buf, rect, t, i == c.tile, c.rows[i]);
        c.tile_rects.push(rect);
        for (ri, rr) in rows {
            c.row_rects.push((i, ri, rr));
        }
    }
}

/// Draws one tile; returns the drawn rows (row index, area).
fn draw_tile(buf: &mut Buffer, rect: Rect, t: &Tile, focused: bool, sel: usize) -> Vec<(usize, Rect)> {
    let inner = widgets::tile(buf, rect, t.glyph, t.title, &t.summary, t.tone, focused);
    let ih = inner.height as usize;
    if ih == 0 {
        return Vec::new();
    }
    let off = if sel >= ih { sel + 1 - ih } else { 0 };
    let mut out = Vec::new();
    for (i, row) in t.rows.iter().enumerate().skip(off).take(ih) {
        let rr = Rect::new(inner.x, inner.y + (i - off) as u16, inner.width, 1);
        if row.head {
            widgets::section(buf, rr, &row.left);
        } else {
            widgets::row(buf, rr, &row.left, row.left_tone, &row.right, row.tone, focused && i == sel, focused);
        }
        out.push((i, rr));
    }
    if t.rows.len() > off + ih {
        let more = format!(" +{} ", t.rows.len() - off - ih);
        put(buf, rect.right().saturating_sub(more.width() as u16 + 2), rect.bottom() - 1, &more, 8, Style::new().fg(MUTED).bg(SURFACE_1));
    }
    out
}

fn settings_page(buf: &mut Buffer, app: &mut App, body: Rect) {
    let c = &mut app.control;
    let preview = body.width >= 140;
    let list_w = if preview { (body.width * 3 / 5).min(110) } else { body.width };
    let list = Rect::new(body.x, body.y, list_w, body.height);
    let path = c.store.as_ref().map(|p| super::hooks::short_path(p)).unwrap_or_else(|| "settings.toml (not saved in this run)".into());
    let state = if !c.file_exists { "not there yet: defaults (a change writes it)".to_string() } else if c.file_problems.is_empty() { "loaded".into() } else { format!("{} problem(s)", c.file_problems.len()) };
    let tone = if c.file_problems.is_empty() { Tone::Muted } else { Tone::Warn };
    let inner = widgets::tile(buf, list, "≡", "SETTINGS", &state, tone, true);
    if inner.height == 0 {
        return;
    }
    let mut y = inner.y;
    put(buf, inner.x + 1, y, &path, inner.width.saturating_sub(2), Style::new().fg(TEXT_2).bg(SURFACE_1));
    y += 1;
    for p in c.file_problems.iter().take(3) {
        if y >= inner.bottom() {
            break;
        }
        put(buf, inner.x + 1, y, &format!("▲ {p}"), inner.width.saturating_sub(2), Style::new().fg(WARNING).bg(SURFACE_1));
        y += 1;
    }
    y += 1;
    c.setting_rects.clear();
    let n = FIELDS.len() + 3;
    c.setting = c.setting.min(n - 1);
    let label_w = 20u16;
    let value_w = 16u16;
    for (i, f) in FIELDS.iter().enumerate() {
        if y >= inner.bottom() {
            break;
        }
        let rr = Rect::new(inner.x, y, inner.width, 1);
        let selected = c.setting == i;
        let bg = if selected { SELECTION } else { SURFACE_1 };
        buf.set_style(rr, Style::new().bg(bg));
        buf.set_string(rr.x, y, if selected { "›" } else { " " }, Style::new().fg(ACCENT).bg(bg));
        put(buf, rr.x + 2, y, f.label, label_w, Style::new().fg(TEXT).bg(bg));
        let v = settings::value(&c.settings, f.key);
        let vt = match v {
            toml::Value::Boolean(false) => MUTED,
            _ => ACCENT,
        };
        let shown = match f.kind {
            settings::Kind::Choice(_) => format!("‹ {} ›", settings::shown(&v)),
            settings::Kind::Bool => format!("[{}] {}", if v.as_bool() == Some(true) { "x" } else { " " }, settings::shown(&v)),
        };
        let vx = rr.x + 2 + label_w;
        put(buf, vx, y, &shown, value_w + 6, Style::new().fg(vt).bg(bg).add_modifier(Modifier::BOLD));
        let hx = vx + value_w + 8;
        if hx + 10 < rr.right() {
            put(buf, hx, y, &format!("{} = …  {}", f.key, f.help), rr.right() - hx - 1, Style::new().fg(MUTED).bg(bg));
        }
        c.setting_rects.push((i, rr));
        y += 1;
    }
    y += 1;
    for (j, (label, key)) in [("Open settings.toml", "e"), ("Open languages.toml", "l"), ("Back to the overview", "Esc")].iter().enumerate() {
        if y >= inner.bottom() {
            break;
        }
        let i = FIELDS.len() + j;
        let rr = Rect::new(inner.x, y, inner.width, 1);
        widgets::row(buf, rr, label, Tone::Normal, key, Tone::Accent, c.setting == i, true);
        c.setting_rects.push((i, rr));
        y += 1;
    }
    if preview {
        let pr = Rect::new(list.right() + 1, body.y, body.right().saturating_sub(list.right() + 1), body.height);
        let pin = widgets::tile(buf, pr, "▤", "settings.toml", if c.file_exists { "on disk" } else { "not written yet" }, Tone::Muted, false);
        let text = if c.file_exists { c.file_text.clone() } else { settings::template() };
        for (i, line) in text.lines().take(pin.height as usize).enumerate() {
            let st = if line.trim_start().starts_with('#') { Style::new().fg(MUTED).add_modifier(Modifier::ITALIC) } else { Style::new().fg(TEXT_2) };
            put(buf, pin.x + 1, pin.y + i as u16, line, pin.width.saturating_sub(2), st.bg(SURFACE_1));
        }
    }
}
