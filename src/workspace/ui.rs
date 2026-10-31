//! Drawing for workspaces: the Repos popup (overview of every root), the badges on the tree's
//! root rows, tree colours of repos that aren't current, and the current repo in the status bar.

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Clear, Widget};

use super::repos::{visible, Mode};
use super::status::RepoState;
use crate::app::App;
use crate::git::ui::{kind_color, BLUE};
use crate::ui::theme::*;

/// Tree colour of a row: the current repo's live status, else the background status of the
/// repo the row lies in (several roots).
pub fn tree_color(git: &crate::git::Git, status: &super::status::Statuses, multi: bool, path: &Path, is_dir: bool) -> Option<Color> {
    if !multi || git.root.as_ref().is_some_and(|r| path.starts_with(r)) {
        return crate::git::ui::tree_color(git, path, is_dir);
    }
    let st = status.repos.values().filter(|s| s.top.as_ref().is_some_and(|t| path.starts_with(t))).max_by_key(|s| s.top.as_ref().map_or(0, |t| t.as_os_str().len()))?;
    st.kind_of(path, is_dir).map(kind_color)
}

/// Badge parts of a root row: (text, colour), drawn right-aligned. Changes, ahead, behind first;
/// the branch when there's room.
pub fn badge(st: Option<&RepoState>) -> Vec<(String, Color)> {
    let Some(st) = st else { return vec![("…".into(), MUTED)] };
    if !st.is_repo() {
        return vec![("no git".into(), MUTED)];
    }
    let mut v = Vec::new();
    if let Some(b) = &st.branch {
        v.push((format!("⎇{b}"), MUTED));
    }
    if st.conflicts > 0 {
        v.push((format!("!{}", st.conflicts), CRITICAL));
    }
    if st.changed > 0 {
        v.push((format!("●{}", st.changed), BLUE));
    }
    if st.ahead > 0 {
        v.push((format!("↑{}", st.ahead), GOOD));
    }
    if st.behind > 0 {
        v.push((format!("↓{}", st.behind), WARNING));
    }
    v
}

fn w(s: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(s) as u16
}

/// Draws a root row's badge into `x0..end` on row `y`, right-aligned; returns where the name has
/// to stop. The branch goes first when space runs out (cut to fit), then the rest.
pub fn draw_badge(buf: &mut Buffer, x0: u16, end: u16, y: u16, parts: &[(String, Color)], bg: Color, name_w: u16) -> u16 {
    let room = end.saturating_sub(x0);
    let mut parts: Vec<(String, Color)> = parts.to_vec();
    let total = |p: &[(String, Color)]| p.iter().map(|(s, _)| w(s) + 1).sum::<u16>();
    // Name keeps at least 6 columns (or its own width when shorter).
    let keep = name_w.min(6) + 1;
    while total(&parts) + keep > room && !parts.is_empty() {
        let over = total(&parts) + keep - room;
        let b = &mut parts[0];
        if b.0.starts_with('⎇') && w(&b.0) > over + 3 {
            let n = b.0.chars().count() - over as usize - 1;
            b.0 = format!("{}…", b.0.chars().take(n).collect::<String>());
        } else {
            parts.remove(0);
        }
    }
    let mut x = end.saturating_sub(total(&parts));
    let stop = x;
    for (s, c) in &parts {
        x += 1;
        buf.set_stringn(x, y, s, (end - x) as usize, Style::new().fg(*c).bg(bg));
        x += w(s);
    }
    stop
}

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, end: u16, st: Style) -> u16 {
    if x >= end {
        return x;
    }
    buf.set_stringn(x, y, s, (end - x) as usize, st).0
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        format!("{s:<n$}")
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

/// The Repos popup (Alt+O): one row per root, a summary line, the keys. Returns the cursor for
/// the filter / add input.
pub fn draw_popup(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let p = app.workspace.popup.as_ref()?;
    let ws = &app.project.ws;
    let status = &app.workspace.status;
    let vis = visible(ws, status, &p.filter, p.only_changed);
    let wanted = vis.len().max(1) as u16 + 6;
    let wd = area.width.saturating_sub(4).min(124);
    let h = area.height.saturating_sub(2).min(wanted.max(10));
    if wd < 30 || h < 7 {
        return None;
    }
    let r = Rect::new(area.x + (area.width - wd) / 2, area.y + (area.height - h) / 2, wd, h);
    Clear.render(r, buf);
    let title = format!(" Repos · {} · {} ", ws.roots.len(), ws.name);
    let bottom = match p.mode {
        Mode::List => " Enter current · g/l/c changes/log/commit · F/U fetch/pull all · f/u one · a add · d remove · w recent · / filter · x changed · Esc ",
        Mode::Filter => " type to filter · Enter keep · Esc clear ",
        Mode::Add => " a folder (~/ ok) · Enter add · Esc cancel ",
        Mode::Workspaces => " ↑↓ · Enter open · Esc back ",
        Mode::Confirm => " y / Enter yes · n / Esc no ",
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BASELINE).bg(SURFACE_1))
        .style(Style::new().bg(SURFACE_1).fg(TEXT))
        .title(title)
        .title_style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
        .title_bottom(bottom)
        .render(r, buf);
    for x in r.x + 1..r.right() - 1 {
        let c = &mut buf[(x, r.bottom() - 1)];
        if c.symbol() != "─" {
            c.set_fg(MUTED);
        }
    }
    let inner = Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 2);
    let end = inner.right();
    let mut cursor = None;

    // Line 1: the overview, or the input / question.
    let y = inner.y;
    match p.mode {
        Mode::Filter | Mode::Add => {
            let (label, text) = if p.mode == Mode::Filter { ("Filter: ", &p.filter) } else { ("Add folder: ", &p.input) };
            let x = put(buf, inner.x, y, label, end, Style::new().fg(ACCENT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
            let x = put(buf, x, y, text, end, Style::new().fg(TEXT).bg(SURFACE_2));
            cursor = Some(Position::new(x.min(end - 1), y));
        }
        Mode::Confirm => {
            let q = p.plan.as_ref().map_or(String::new(), |pl| pl.question.clone());
            put(buf, inner.x, y, &format!("{q}  y / n"), end, Style::new().fg(WARNING).bg(SURFACE_1).add_modifier(Modifier::BOLD));
        }
        _ => {
            let all: Vec<&RepoState> = ws.roots.iter().filter_map(|r| status.get(&r.path)).collect();
            let dirty = all.iter().filter(|s| s.dirty()).count();
            let behind = all.iter().filter(|s| s.behind > 0).count();
            let ahead = all.iter().filter(|s| s.ahead > 0).count();
            let (a, d) = all.iter().filter_map(|s| s.stat).fold((0, 0), |(a, d), (x, y)| (a + x, d + y));
            let mut x = put(buf, inner.x, y, &format!("{} with changes", dirty), end, Style::new().fg(if dirty > 0 { BLUE } else { TEXT_2 }).bg(SURFACE_1).add_modifier(Modifier::BOLD));
            x = put(buf, x, y, &format!("  +{a}"), end, Style::new().fg(GOOD).bg(SURFACE_1));
            x = put(buf, x, y, &format!(" −{d}"), end, Style::new().fg(CRITICAL).bg(SURFACE_1));
            x = put(buf, x, y, &format!("  · {ahead} ahead · {behind} behind"), end, Style::new().fg(TEXT_2).bg(SURFACE_1));
            if let Some(b) = &app.workspace.busy {
                x = put(buf, x, y, &format!("  · {b}"), end, Style::new().fg(WARNING).bg(SURFACE_1));
            } else if status.busy() {
                x = put(buf, x, y, "  · reading…", end, Style::new().fg(MUTED).bg(SURFACE_1));
            }
            if p.only_changed {
                x = put(buf, x, y, "  · only changed (x)", end, Style::new().fg(ACCENT).bg(SURFACE_1));
            }
            if !p.filter.is_empty() {
                put(buf, x, y, &format!("  · filter “{}”", p.filter), end, Style::new().fg(ACCENT).bg(SURFACE_1));
            }
        }
    }
    for x in r.x + 1..r.right() - 1 {
        buf[(x, inner.y + 1)].set_char('─').set_fg(GRID).set_bg(SURFACE_1);
    }
    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));

    if p.mode == Mode::Workspaces {
        let now = crate::git::log::now();
        for (k, rc) in p.recents.iter().enumerate().take(list.height as usize) {
            let y = list.y + k as u16;
            let bg = if k == p.wsel { SELECTION } else { SURFACE_1 };
            buf.set_style(Rect::new(r.x + 1, y, r.width - 2, 1), Style::new().bg(bg));
            let x = put(buf, list.x, y, &cut(&rc.name, 28), end, Style::new().fg(TEXT).bg(bg).add_modifier(Modifier::BOLD));
            let labels: Vec<&str> = rc.roots.iter().map(|x| x.label.as_str()).collect();
            let x = put(buf, x + 1, y, &format!("{:>2} roots  ", rc.roots.len()), end, Style::new().fg(TEXT_2).bg(bg));
            let x = put(buf, x, y, &format!("{} ago  ", crate::git::log::age(now, rc.opened as i64)), end, Style::new().fg(MUTED).bg(bg));
            put(buf, x, y, &labels.join(", "), end, Style::new().fg(MUTED).bg(bg));
        }
        if cursor.is_none() {
            cursor = Some(Position::new(list.x, list.y + p.wsel.min(list.height.saturating_sub(1) as usize) as u16));
        }
        let pm = app.workspace.popup.as_mut()?;
        pm.rect = r;
        pm.list_rect = list;
        return cursor;
    }

    // The repo rows.
    let lh = list.height as usize;
    let pm = app.workspace.popup.as_mut()?;
    pm.sel = pm.sel.min(vis.len().saturating_sub(1));
    if pm.sel < pm.scroll {
        pm.scroll = pm.sel;
    } else if lh > 0 && pm.sel >= pm.scroll + lh {
        pm.scroll = pm.sel + 1 - lh;
    }
    let (sel, scroll) = (pm.sel, pm.scroll);
    pm.rect = r;
    pm.list_rect = list;
    pm.row_roots.clear();
    let ws = &app.project.ws;
    let status = &app.workspace.status;
    let now = crate::git::log::now();
    let lw = ws.roots.iter().map(|r| r.label.chars().count()).max().unwrap_or(4).clamp(4, 22);
    let mut rows = Vec::new();
    if vis.is_empty() {
        put(buf, list.x, list.y, if ws.roots.is_empty() { "No roots" } else { "No repo matches (Esc clears the filter · x shows all)" }, end, Style::new().fg(MUTED).bg(SURFACE_1));
    }
    for (row, &i) in vis.iter().enumerate().skip(scroll).take(lh) {
        rows.push(i);
        let root = &ws.roots[i];
        let y = list.y + (row - scroll) as u16;
        let bg = if row == sel { SELECTION } else { SURFACE_1 };
        buf.set_style(Rect::new(r.x + 1, y, r.width - 2, 1), Style::new().bg(bg));
        let cur = i == app.workspace.current;
        let mut x = put(buf, list.x, y, if cur { "▸ " } else { "  " }, end, Style::new().fg(ACCENT).bg(bg).add_modifier(Modifier::BOLD));
        x = put(buf, x, y, &cut(&root.label, lw), end, Style::new().fg(root.color).bg(bg).add_modifier(Modifier::BOLD));
        x += 2;
        let st = status.get(&root.path);
        let Some(st) = st else {
            put(buf, x, y, "reading…", end, Style::new().fg(MUTED).bg(bg));
            continue;
        };
        if !st.is_repo() {
            put(buf, x, y, "not a git repository", end, Style::new().fg(MUTED).bg(bg));
            continue;
        }
        x = put(buf, x, y, &cut(&format!("⎇ {}", st.branch.as_deref().unwrap_or("?")), 22), end, Style::new().fg(TEXT_2).bg(bg));
        x += 1;
        let sync = if st.remotes == 0 {
            ("no remote".to_string(), MUTED)
        } else if !st.upstream {
            ("no upstream".to_string(), MUTED)
        } else if st.ahead == 0 && st.behind == 0 {
            ("✓ in sync".to_string(), TEXT_2)
        } else {
            (format!("↑{} ↓{}", st.ahead, st.behind), if st.behind > 0 { WARNING } else { GOOD })
        };
        x = put(buf, x, y, &cut(&sync.0, 11), end, Style::new().fg(sync.1).bg(bg));
        x += 1;
        if st.changed == 0 {
            x = put(buf, x, y, &cut("clean", 20), end, Style::new().fg(MUTED).bg(bg));
        } else {
            let files = format!("{} file{}", st.changed, if st.changed == 1 { "" } else { "s" });
            let x0 = x;
            x = put(buf, x, y, &files, end, Style::new().fg(BLUE).bg(bg).add_modifier(Modifier::BOLD));
            if let Some((a, d)) = st.stat {
                x = put(buf, x, y, &format!(" +{a}"), end, Style::new().fg(GOOD).bg(bg));
                x = put(buf, x, y, &format!(" −{d}"), end, Style::new().fg(CRITICAL).bg(bg));
            }
            x = x.max(x0 + 20);
        }
        x += 1;
        // Last column: the last fetch / pull result, else the last commit.
        match app.workspace.ops.get(&root.path) {
            Some(Ok(m)) => {
                put(buf, x, y, &format!("✓ {m}"), end, Style::new().fg(GOOD).bg(bg));
            }
            Some(Err(e)) => {
                put(buf, x, y, &format!("✗ {}", e.lines().last().unwrap_or(e)), end, Style::new().fg(if e.starts_with("skipped") { MUTED } else { CRITICAL }).bg(bg));
            }
            None => {
                if let Some((h, s, t)) = &st.last {
                    let x = put(buf, x, y, &format!("{h} "), end, Style::new().fg(S_YELLOW).bg(bg));
                    let age = format!(" · {}", crate::git::log::age(now, *t));
                    let room = end.saturating_sub(x).saturating_sub(w(&age)) as usize;
                    let x = put(buf, x, y, &cut(s, room).trim_end().to_string(), end, Style::new().fg(TEXT_2).bg(bg));
                    put(buf, x, y, &age, end, Style::new().fg(MUTED).bg(bg));
                }
            }
        }
    }
    if let Some(pm) = app.workspace.popup.as_mut() {
        pm.row_roots = rows;
    }
    cursor.or_else(|| (!vis.is_empty()).then(|| Position::new(list.x, list.y + (sel - scroll) as u16)))
}
