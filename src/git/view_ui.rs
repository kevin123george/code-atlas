//! Drawing the Git view: title bar with tabs, the Changes review (file list + document,
//! unified or side by side, syntax colours and word highlights), the Log (graph, refs, ✳ for
//! Claude's commits), commit details, the branches popup and the footer (keys, a question or an
//! input line). Readable at 80 columns: the file list moves above the document there.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Clear, Widget};
use unicode_width::UnicodeWidthStr;

use super::graph::Cell;
use super::patch::{LineKind, Status};
use super::review::{Review, Row, Scope, Section};
use super::ui::{kind_color, ADD_BG, BLUE, DEL_BG};
use super::view::{GitView, Tab};
use crate::app::App;
use crate::syntax::Token;
use crate::ui::theme::*;

/// Word-level highlights: the diff backgrounds, brighter.
pub const ADD_HI: Color = Color::Rgb(0x1c, 0x52, 0x3e);
pub const DEL_HI: Color = Color::Rgb(0x66, 0x26, 0x28);
/// Lane colours of the log graph (Ledger Terminal series).
const LANES: [Color; 7] = [S_BLUE, S_ORANGE, S_AQUA, S_MAGENTA, S_VIOLET, S_YELLOW, S_TEAL];

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, max: u16, style: Style) -> u16 {
    if max == 0 {
        return x;
    }
    buf.set_stringn(x, y, s, max as usize, style).0
}

/// Keeps the end of a path that doesn't fit: `…/git/review.rs`.
fn fit_path(p: &str, w: usize) -> String {
    if p.width() <= w || w < 4 {
        return p.to_string();
    }
    let chars: Vec<char> = p.chars().collect();
    let keep: String = chars[chars.len().saturating_sub(w - 1)..].iter().collect();
    format!("…{keep}")
}

fn status_color(s: Status) -> Color {
    match s {
        Status::Modified => BLUE,
        Status::Added | Status::Untracked => GOOD,
        Status::Deleted => CRITICAL,
    }
}

/// Draws the view over `area` when open. Returns the cursor for an input line.
pub fn draw(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let focused = app.git_view_focused();
    let running = app.agents.running_count();
    let branch = app.dock.git.branch.clone();
    let v = &mut app.git_view;
    if !v.open {
        v.rect = Rect::default();
        v.list_rect = Rect::default();
        v.doc_rect = Rect::default();
        return None;
    }
    Clear.render(area, buf);
    buf.set_style(area, Style::new().bg(PAPER).fg(TEXT));
    v.rect = area;
    if area.height < 4 || area.width < 24 {
        put(buf, area.x, area.y, "Git: too small", area.width, Style::new().fg(MUTED));
        return None;
    }
    title_bar(buf, v, Rect::new(area.x, area.y, area.width, 1), focused, branch.as_deref(), running);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    let cursor = footer(buf, v, Rect::new(area.x, area.bottom() - 1, area.width, 1));
    match v.tab {
        Tab::Changes => changes(buf, v, body),
        Tab::Log => {
            v.list_rect = Rect::default();
            if let Some(d) = &mut v.details {
                v.doc_rect = body;
                let sbs = v.sbs_pref.unwrap_or(body.width >= 150) && body.width >= 60;
                document(buf, d, body, sbs, &v.agent_marks);
            } else {
                v.doc_rect = body;
                log(buf, v, body);
            }
        }
    }
    if v.branches.is_some() {
        branches(buf, v, body);
    }
    cursor
}

fn title_bar(buf: &mut Buffer, v: &GitView, r: Rect, focused: bool, branch: Option<&str>, running: usize) {
    buf.set_style(r, Style::new().bg(SURFACE_1));
    let mut x = put(buf, r.x, r.y, " Git ", r.width, Style::new().fg(ACCENT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
    for (tab, label) in [(Tab::Changes, " 1 Changes "), (Tab::Log, " 2 Log ")] {
        let style = match (tab == v.tab, focused) {
            (true, true) => Style::new().fg(PAPER).bg(ACCENT).add_modifier(Modifier::BOLD),
            (true, false) => Style::new().fg(TEXT).bg(SURFACE_2).add_modifier(Modifier::BOLD),
            _ => Style::new().fg(TEXT_2).bg(SURFACE_1),
        };
        x = put(buf, x, r.y, label, r.right().saturating_sub(x), style);
        x += 1;
    }
    if let Some(b) = branch {
        x = put(buf, x, r.y, &format!("⎇ {b} "), r.right().saturating_sub(x), Style::new().fg(TEXT_2).bg(SURFACE_1));
    }
    // Right side: what is shown, live state.
    let mut right: Vec<(String, Style)> = Vec::new();
    if running > 0 {
        right.push((format!("{} agent working ", crate::agents::ui::spinner()), Style::new().fg(ACCENT).bg(SURFACE_1)));
    }
    if let Some(b) = &v.remote_busy {
        right.push((format!("{b} "), Style::new().fg(WARNING).bg(SURFACE_1)));
    }
    let mut short_scope = None;
    match v.tab {
        Tab::Changes => {
            let rv = &v.review;
            let scope_style = Style::new().fg(if matches!(rv.scope, Scope::Run { .. }) { ACCENT } else { TEXT_2 }).bg(SURFACE_1);
            short_scope = Some((right.len(), format!("{} ", v.scope_short()), scope_style));
            right.push((format!("{} ", v.scope_label()), scope_style));
            if rv.loaded {
                let (f, a, d) = rv.totals();
                right.push((format!("{f} file{} ", if f == 1 { "" } else { "s" }), Style::new().fg(TEXT_2).bg(SURFACE_1)));
                right.push((format!("+{a} "), Style::new().fg(GOOD).bg(SURFACE_1)));
                right.push((format!("−{d} "), Style::new().fg(CRITICAL).bg(SURFACE_1)));
            }
            let live = if rv.busy() { "◌ live " } else { "● live " };
            if !matches!(rv.scope, Scope::Commit { .. }) {
                right.push((live.into(), Style::new().fg(GOOD).bg(SURFACE_1)));
            }
        }
        Tab::Log => {
            let l = &v.log;
            let what = match (&v.details, &l.path) {
                (Some(d), _) => match &d.scope {
                    Scope::Commit { sha } => format!("commit {} ", sha.get(..7).unwrap_or(sha)),
                    _ => String::new(),
                },
                (None, Some(p)) => format!("history of {p} · {} ", l.shown.len()),
                (None, None) if l.loading && !l.loaded => "loading… ".into(),
                (None, None) => format!("{} of {} commits ", l.shown.len(), l.commits.len()),
            };
            right.push((what, Style::new().fg(TEXT_2).bg(SURFACE_1)));
        }
    }
    let fits = |right: &[(String, Style)]| x + right.iter().map(|(s, _)| s.width() as u16).sum::<u16>() < r.right();
    if !fits(&right) {
        // Narrow: the short scope, then without "live".
        if let Some((i, s, st)) = short_scope {
            right[i] = (s, st);
        }
        if !fits(&right) && right.last().is_some_and(|l| l.0.contains("live")) {
            right.pop();
        }
    }
    let total: u16 = right.iter().map(|(s, _)| s.width() as u16).sum();
    // Drop pieces from the left of the right side until it fits.
    let mut skip = 0;
    let mut w = total;
    while x + w + 1 > r.right() && skip < right.len() {
        w -= right[skip].0.width() as u16;
        skip += 1;
    }
    let mut rx = r.right() - w;
    for (s, st) in right.iter().skip(skip) {
        rx = put(buf, rx, r.y, s, r.right().saturating_sub(rx), *st);
    }
}

fn footer(buf: &mut Buffer, v: &GitView, r: Rect) -> Option<Position> {
    buf.set_style(r, Style::new().bg(SURFACE_1));
    if let Some(c) = &v.confirm {
        let x = put(buf, r.x, r.y, " ? ", r.width, Style::new().fg(PAPER).bg(WARNING).add_modifier(Modifier::BOLD));
        let x = put(buf, x + 1, r.y, &c.question, r.right().saturating_sub(x + 1), Style::new().fg(WARNING).bg(SURFACE_1).add_modifier(Modifier::BOLD));
        put(buf, x + 1, r.y, " y yes · n no", r.right().saturating_sub(x + 1), Style::new().fg(TEXT_2).bg(SURFACE_1));
        return None;
    }
    if let Some(inp) = &v.input {
        let label = format!(" {}: ", inp.kind.label());
        let x = put(buf, r.x, r.y, &label, r.width, Style::new().fg(ACCENT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
        let room = r.right().saturating_sub(x + 1) as usize;
        let n = inp.text.chars().count();
        let shown: String = inp.text.chars().skip(n.saturating_sub(room)).collect();
        let end = put(buf, x, r.y, &shown, room as u16, Style::new().fg(TEXT).bg(SURFACE_1));
        return Some(Position::new(end.min(r.right().saturating_sub(1)), r.y));
    }
    let wide = r.width >= 120;
    let keys: &[(&str, &str)] = match v.tab {
        Tab::Changes if wide => &[("F7", "change"), ("Tab", "file"), ("s", "stage"), ("r/R", "revert hunk/file"), ("u", "undo"), ("v", "split"), ("a", "agent runs"), ("t", "agent's files"), ("Enter", "open"), ("b", "blame"), ("h", "history"), ("2", "log"), ("B", "branches"), ("P/U/F", "push/pull/fetch"), ("Esc", "close")],
        Tab::Changes => &[("F7", "change"), ("Tab", "file"), ("s", "stage"), ("r", "revert"), ("u", "undo"), ("a", "runs"), ("2", "log"), ("Esc", "")],
        Tab::Log if v.details.is_some() => &[("F7", "change"), ("Tab", "file"), ("v", "split"), ("Enter", "open file"), ("y", "copy hash"), ("Esc", "back")],
        Tab::Log if wide => &[("Enter", "details"), ("/", "filter"), ("h", "this file's history"), ("y", "copy hash"), ("1", "changes"), ("B", "branches"), ("P/U/F", "push/pull/fetch"), ("Esc", "close")],
        Tab::Log => &[("Enter", "details"), ("/", "filter"), ("h", "file"), ("1", "changes"), ("B", "branches"), ("Esc", "")],
    };
    let mut x = r.x + 1;
    for (k, what) in keys {
        let need = (k.width() + what.width() + 3) as u16;
        if x + need > r.right() {
            break;
        }
        x = put(buf, x, r.y, k, r.right() - x, Style::new().fg(ACCENT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
        if !what.is_empty() {
            x = put(buf, x + 1, r.y, what, r.right() - x - 1, Style::new().fg(TEXT_2).bg(SURFACE_1));
        }
        x += 2;
    }
    None
}

fn changes(buf: &mut Buffer, v: &mut GitView, body: Rect) {
    let n = v.review.model.files.len();
    let wide = body.width >= 110;
    let (list, doc) = if n == 0 {
        (Rect::default(), body)
    } else if wide {
        let lw = (body.width / 4).clamp(28, 44);
        for y in body.y..body.bottom() {
            buf[(body.x + lw, y)].set_char('│').set_fg(GRID);
        }
        (Rect::new(body.x, body.y, lw, body.height), Rect::new(body.x + lw + 1, body.y, body.width - lw - 1, body.height))
    } else {
        let lh = (n as u16).min((body.height / 4).max(2)).min(body.height.saturating_sub(4));
        let sep = body.y + lh;
        for x in body.x..body.right() {
            buf[(x, sep)].set_char('─').set_fg(GRID);
        }
        (Rect::new(body.x, body.y, body.width, lh), Rect::new(body.x, sep + 1, body.width, body.bottom().saturating_sub(sep + 1)))
    };
    v.list_rect = list;
    v.doc_rect = doc;
    if !v.review.loaded {
        put(buf, doc.x + 2, doc.y + 1, "Looking at the changes…", doc.width.saturating_sub(4), Style::new().fg(MUTED));
        return;
    }
    if let Some(e) = &v.review.model.error {
        put(buf, doc.x + 2, doc.y + 1, &format!("git: {e}"), doc.width.saturating_sub(4), Style::new().fg(CRITICAL));
        return;
    }
    if n == 0 {
        let msg = match &v.review.scope {
            Scope::Worktree => "No changes: the working tree matches HEAD. This view updates by itself while an agent works.",
            Scope::Run { only_touched: true, .. } => "Nothing changed in the files this run's tools edited (t shows all changes since it started).",
            Scope::Run { .. } => "Nothing changed since this run started.",
            Scope::Commit { .. } => "This commit changes no files.",
        };
        put(buf, doc.x + 2, doc.y + 1, msg, doc.width.saturating_sub(4), Style::new().fg(MUTED));
        if matches!(v.review.scope, Scope::Worktree) {
            put(buf, doc.x + 2, doc.y + 3, "a: review an agent run · 2: log · B: branches", doc.width.saturating_sub(4), Style::new().fg(MUTED));
        }
        return;
    }
    let sbs = v.sbs_for(doc.width);
    document(buf, &mut v.review, doc, sbs, &v.agent_marks);
    file_list(buf, v, list);
}

fn file_list(buf: &mut Buffer, v: &mut GitView, r: Rect) {
    if r.height == 0 {
        return;
    }
    let cur = v.review.current_file();
    let files = &v.review.model.files;
    let h = r.height as usize;
    if let Some(c) = cur {
        if c < v.list_scroll {
            v.list_scroll = c;
        } else if c >= v.list_scroll + h {
            v.list_scroll = c + 1 - h;
        }
    }
    v.list_scroll = v.list_scroll.min(files.len().saturating_sub(h));
    for (row, (i, e)) in files.iter().enumerate().skip(v.list_scroll).take(h).enumerate() {
        let y = r.y + row as u16;
        let sel = Some(i) == cur;
        let bg = if sel { SELECTION } else { PAPER };
        buf.set_style(Rect::new(r.x, y, r.width, 1), Style::new().bg(bg));
        let stat = format!(" +{} −{} ", e.diff.added(), e.diff.deleted());
        let sw = stat.width() as u16;
        let mut x = r.x + 1;
        let tag = match e.section {
            Section::Staged => "●",
            _ => " ",
        };
        x = put(buf, x, y, tag, 1, Style::new().fg(GOOD).bg(bg));
        x = put(buf, x, y, &e.diff.status.letter().to_string(), 1, Style::new().fg(status_color(e.diff.status)).bg(bg).add_modifier(Modifier::BOLD));
        x += 1;
        let agent = v.agent_marks.contains_key(&e.diff.path);
        if agent {
            x = put(buf, x, y, "✳", 1, Style::new().fg(ACCENT).bg(bg));
        }
        let room = r.right().saturating_sub(x + sw) as usize;
        let style = Style::new().fg(if sel { TEXT } else { status_color(e.diff.status) }).bg(bg);
        let style = if e.diff.status == Status::Deleted { style.add_modifier(Modifier::CROSSED_OUT) } else { style };
        put(buf, x, y, &fit_path(&e.diff.path, room), room as u16, style);
        if r.width > sw + 8 {
            let sx = r.right() - sw;
            let ax = put(buf, sx, y, &format!(" +{}", e.diff.added()), sw, Style::new().fg(GOOD).bg(bg));
            put(buf, ax, y, &format!(" −{} ", e.diff.deleted()), r.right().saturating_sub(ax), Style::new().fg(CRITICAL).bg(bg));
        }
    }
}

/// Draws a review document in `r`, `sbs` = side by side.
pub fn document(buf: &mut Buffer, rv: &mut Review, r: Rect, sbs: bool, marks: &std::collections::HashMap<String, String>) {
    rv.set_sbs(sbs);
    let h = r.height as usize;
    rv.view_h = h;
    if rv.cursor < rv.scroll {
        rv.scroll = rv.cursor;
    } else if h > 0 && rv.cursor >= rv.scroll + h {
        rv.scroll = rv.cursor + 1 - h;
    }
    if !rv.loaded {
        put(buf, r.x + 2, r.y + 1, "Loading…", r.width.saturating_sub(4), Style::new().fg(MUTED));
        return;
    }
    if let Some(e) = &rv.model.error {
        put(buf, r.x + 2, r.y + 1, &format!("git: {e}"), r.width.saturating_sub(4), Style::new().fg(CRITICAL));
        return;
    }
    let num_w = rv
        .model
        .files
        .iter()
        .flat_map(|f| f.diff.hunks.iter())
        .map(|h| (h.old_start + h.old_len).max(h.new_start + h.new_len))
        .max()
        .unwrap_or(1)
        .to_string()
        .len()
        .max(3);
    let total_hunks = rv.rows.iter().filter(|x| matches!(x, Row::Hunk(..))).count();
    let mut hunk_no = rv.rows[..rv.scroll.min(rv.rows.len())].iter().filter(|x| matches!(x, Row::Hunk(..))).count();
    for (row_i, row) in rv.rows.iter().enumerate().skip(rv.scroll).take(h) {
        let y = r.y + (row_i - rv.scroll) as u16;
        let cur = row_i == rv.cursor;
        let line_rect = Rect::new(r.x, y, r.width, 1);
        match row {
            Row::Text(i) => {
                let t = &rv.model.header[*i];
                let st = if *i == 0 { Style::new().fg(ACCENT).add_modifier(Modifier::BOLD) } else if *i == 1 { Style::new().fg(TEXT_2) } else { Style::new().fg(TEXT) };
                let bg = if cur { SELECTION } else { PAPER };
                buf.set_style(line_rect, Style::new().bg(bg));
                put(buf, r.x + 1, y, t, r.width.saturating_sub(2), st.bg(bg));
            }
            Row::Section(s) => {
                let n = rv.model.files.iter().filter(|f| f.section == *s).count();
                let label = match s {
                    Section::Staged => format!("STAGED · {n} file{} (s unstages, Ctrl+K commits)", if n == 1 { "" } else { "s" }),
                    _ => format!("CHANGES · {n} file{} not staged", if n == 1 { "" } else { "s" }),
                };
                let bg = if cur { SELECTION } else { PAPER };
                buf.set_style(line_rect, Style::new().bg(bg));
                put(buf, r.x + 1, y, &label, r.width.saturating_sub(2), Style::new().fg(if *s == Section::Staged { GOOD } else { TEXT_2 }).bg(bg).add_modifier(Modifier::BOLD));
            }
            Row::File(f) => {
                let e = &rv.model.files[*f];
                let bg = if cur { SELECTION } else { SURFACE_2 };
                buf.set_style(line_rect, Style::new().bg(bg));
                let mut x = put(buf, r.x, y, if cur { "▌" } else { " " }, 1, Style::new().fg(ACCENT).bg(bg));
                x = put(buf, x, y, &format!("{} ", e.diff.status.letter()), r.right().saturating_sub(x), Style::new().fg(status_color(e.diff.status)).bg(bg).add_modifier(Modifier::BOLD));
                let stat = format!("  +{} −{}", e.diff.added(), e.diff.deleted());
                let mut tail = String::new();
                if e.section == Section::Staged {
                    tail.push_str("  staged");
                }
                if let Some(m) = marks.get(&e.diff.path) {
                    tail.push_str(&format!("  ✳ {m}"));
                }
                let room = r.right().saturating_sub(x + stat.width() as u16 + 1) as usize;
                x = put(buf, x, y, &fit_path(&e.diff.path, room), room as u16, Style::new().fg(TEXT).bg(bg).add_modifier(Modifier::BOLD));
                let ax = put(buf, x, y, &format!("  +{}", e.diff.added()), r.right().saturating_sub(x), Style::new().fg(GOOD).bg(bg));
                let dx = put(buf, ax, y, &format!(" −{}", e.diff.deleted()), r.right().saturating_sub(ax), Style::new().fg(CRITICAL).bg(bg));
                if !tail.is_empty() {
                    put(buf, dx, y, &tail, r.right().saturating_sub(dx), Style::new().fg(ACCENT).bg(bg));
                }
                let _ = kind_color;
            }
            Row::Note(f) => {
                let e = &rv.model.files[*f];
                let bg = if cur { SELECTION } else { PAPER };
                buf.set_style(line_rect, Style::new().bg(bg));
                put(buf, r.x + 3, y, e.note.as_deref().unwrap_or(""), r.width.saturating_sub(4), Style::new().fg(WARNING).bg(bg));
            }
            Row::Hunk(f, hi) => {
                hunk_no += 1;
                let hk = &rv.model.files[*f].diff.hunks[*hi];
                let bg = if cur { SELECTION } else { SURFACE_1 };
                buf.set_style(line_rect, Style::new().bg(bg));
                let x = put(buf, r.x, y, if cur { "▌" } else { " " }, 1, Style::new().fg(ACCENT).bg(bg));
                let right = format!(" +{} −{} · {hunk_no}/{total_hunks} ", hk.added(), hk.deleted());
                let room = r.right().saturating_sub(x + right.width() as u16);
                put(buf, x, y, &hk.header, room, Style::new().fg(ACCENT).bg(bg));
                if r.width > right.width() as u16 + 20 {
                    put(buf, r.right() - right.width() as u16, y, &right, right.width() as u16, Style::new().fg(MUTED).bg(bg));
                }
            }
            Row::Line { f, h: hi, a, b } => {
                let e = &rv.model.files[*f];
                let hk = &e.diff.hunks[*hi];
                let side = |i: Option<usize>, old: bool| -> Option<Side<'_>> {
                    let i = i?;
                    let l = &hk.lines[i];
                    let num = if old { l.old } else { l.new };
                    let toks = match (l.kind, num) {
                        (LineKind::Deleted, Some(n)) => e.old_tok.get(&n),
                        (_, Some(n)) => e.new_tok.get(&n).or_else(|| l.old.and_then(|o| e.old_tok.get(&o))),
                        _ => None,
                    };
                    Some(Side { kind: l.kind, text: &l.text, words: &e.words[*hi][i], toks, old: l.old, new: l.new })
                };
                if !sbs {
                    let s = side(a.or(*b), a.is_some() && b.is_none()).unwrap();
                    draw_unified(buf, Rect::new(r.x, y, r.width, 1), &s, num_w, cur);
                } else {
                    let half = (r.width - 1) / 2;
                    let left = Rect::new(r.x, y, half, 1);
                    let right = Rect::new(r.x + half + 1, y, r.width - half - 1, 1);
                    draw_half(buf, left, side(*a, true).as_ref(), true, num_w, cur);
                    buf[(r.x + half, y)].set_char('│').set_fg(GRID).set_bg(PAPER);
                    draw_half(buf, right, side(*b, false).as_ref(), false, num_w, cur);
                }
            }
            Row::Blank => {
                if cur {
                    buf.set_style(line_rect, Style::new().bg(SURFACE_1));
                }
            }
        }
    }
}

struct Side<'a> {
    kind: LineKind,
    text: &'a str,
    words: &'a [(usize, usize)],
    toks: Option<&'a Vec<Option<Token>>>,
    old: Option<usize>,
    new: Option<usize>,
}

fn colors(kind: LineKind) -> (Color, Color, &'static str, Color) {
    match kind {
        LineKind::Added => (ADD_BG, ADD_HI, "+", GOOD),
        LineKind::Deleted => (DEL_BG, DEL_HI, "−", CRITICAL),
        LineKind::Context => (PAPER, PAPER, " ", TEXT_2),
    }
}

fn num(n: Option<usize>, w: usize) -> String {
    n.map(|n| format!("{n:>w$}")).unwrap_or_else(|| " ".repeat(w))
}

fn draw_unified(buf: &mut Buffer, r: Rect, s: &Side, num_w: usize, cur: bool) {
    let (bg, hi, sign, sign_fg) = colors(s.kind);
    buf.set_style(r, Style::new().bg(bg));
    let gbg = if cur { SELECTION } else { bg };
    let mut x = put(buf, r.x, r.y, if cur { "▌" } else { " " }, 1, Style::new().fg(ACCENT).bg(gbg));
    x = put(buf, x, r.y, &format!("{} {} ", num(s.old, num_w), num(s.new, num_w)), r.right().saturating_sub(x), Style::new().fg(if cur { TEXT_2 } else { MUTED }).bg(gbg));
    x = put(buf, x, r.y, sign, 1, Style::new().fg(sign_fg).bg(bg).add_modifier(Modifier::BOLD));
    code(buf, x + 1, r.y, r.right().saturating_sub(x + 1), s, bg, hi);
}

fn draw_half(buf: &mut Buffer, r: Rect, s: Option<&Side>, old: bool, num_w: usize, cur: bool) {
    let Some(s) = s else {
        // The other side has lines here: a calm filler.
        buf.set_style(r, Style::new().bg(SURFACE_1));
        if cur {
            put(buf, r.x, r.y, "▌", 1, Style::new().fg(ACCENT).bg(SELECTION));
        }
        return;
    };
    let (bg, hi, sign, sign_fg) = colors(s.kind);
    buf.set_style(r, Style::new().bg(bg));
    let gbg = if cur { SELECTION } else { bg };
    let mut x = put(buf, r.x, r.y, if cur { "▌" } else { " " }, 1, Style::new().fg(ACCENT).bg(gbg));
    x = put(buf, x, r.y, &format!("{} ", num(if old { s.old } else { s.new }, num_w)), r.right().saturating_sub(x), Style::new().fg(if cur { TEXT_2 } else { MUTED }).bg(gbg));
    x = put(buf, x, r.y, sign, 1, Style::new().fg(sign_fg).bg(bg).add_modifier(Modifier::BOLD));
    code(buf, x + 1, r.y, r.right().saturating_sub(x + 1), s, bg, hi);
}

/// One line of code: syntax colours, word highlights, tabs as 4 spaces, `›` when cut.
fn code(buf: &mut Buffer, x0: u16, y: u16, w: u16, s: &Side, bg: Color, hi: Color) {
    let base = if s.kind == LineKind::Context { TEXT_2 } else { TEXT };
    let mut x = x0;
    let end = x0 + w;
    let text = s.text.strip_suffix('\r').unwrap_or(s.text);
    for (i, c) in text.chars().enumerate() {
        let in_word = s.words.iter().any(|&(a, b)| i >= a && i < b);
        let cell_bg = if in_word { hi } else { bg };
        let mut st = match s.toks.and_then(|t| t.get(i).copied().flatten()) {
            Some(t) => token_style(t).bg(cell_bg),
            None => Style::new().fg(base).bg(cell_bg),
        };
        if s.kind == LineKind::Context {
            // Context stays a step back from the changes.
            st = st.add_modifier(Modifier::DIM);
        }
        let cw = if c == '\t' { 4 } else { unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) as u16 };
        if cw == 0 {
            continue;
        }
        if x + cw > end {
            if end > x0 {
                buf[(end - 1, y)].set_char('›').set_style(Style::new().fg(MUTED).bg(bg));
            }
            return;
        }
        if c == '\t' {
            for k in 0..4 {
                buf[(x + k, y)].set_char(' ').set_style(st);
            }
        } else if c.is_control() {
            buf[(x, y)].set_char('?').set_style(st.fg(CRITICAL));
        } else {
            buf[(x, y)].set_char(c).set_style(st);
        }
        x += cw;
    }
}

fn log(buf: &mut Buffer, v: &mut GitView, r: Rect) {
    let l = &mut v.log;
    let h = r.height as usize;
    l.view_h = h;
    if let Some(e) = &l.error {
        put(buf, r.x + 2, r.y + 1, &format!("git: {e}"), r.width.saturating_sub(4), Style::new().fg(CRITICAL));
        return;
    }
    if !l.loaded {
        put(buf, r.x + 2, r.y + 1, "Loading the log…", r.width.saturating_sub(4), Style::new().fg(MUTED));
        return;
    }
    if l.shown.is_empty() {
        let msg = if l.commits.is_empty() { "No commits yet." } else { "No commit matches the filter (/ to change it)." };
        put(buf, r.x + 2, r.y + 1, msg, r.width.saturating_sub(4), Style::new().fg(MUTED));
        return;
    }
    if l.sel < l.scroll {
        l.scroll = l.sel;
    } else if h > 0 && l.sel >= l.scroll + h {
        l.scroll = l.sel + 1 - h;
    }
    let graph = l.show_graph();
    let gw = if graph { l.graph.iter().skip(l.scroll).take(h).map(Vec::len).max().unwrap_or(1) as u16 } else { 1 };
    let now = super::log::now();
    let wide = r.width >= 100;
    for (row, &ci) in l.shown.iter().enumerate().skip(l.scroll).take(h) {
        let y = r.y + (row - l.scroll) as u16;
        let c = &l.commits[ci];
        let sel = row == l.sel;
        let bg = if sel { SELECTION } else { PAPER };
        buf.set_style(Rect::new(r.x, y, r.width, 1), Style::new().bg(bg));
        let mut x = put(buf, r.x, y, if sel { "▌" } else { " " }, 1, Style::new().fg(ACCENT).bg(bg));
        if graph {
            for (k, cell) in l.graph[ci].iter().enumerate() {
                draw_graph_cell(buf, x + k as u16, y, *cell, bg);
            }
        } else {
            buf[(x, y)].set_char('●').set_fg(LANES[0]).set_bg(bg);
        }
        x += gw + 1;
        x = put(buf, x, y, c.short(), r.right().saturating_sub(x), Style::new().fg(if sel { TEXT_2 } else { MUTED }).bg(bg));
        x += 1;
        // Right: author and age.
        let age = super::log::age(now, c.time);
        let author: String = c.author.chars().take(14).collect();
        let tail = if wide { format!(" {author:<14} {age:>4} ") } else { format!(" {age:>4} ") };
        let tw = tail.width() as u16;
        let limit = r.right().saturating_sub(tw);
        for rf in &c.refs {
            if x + 4 >= limit {
                break;
            }
            let (label, color) = if let Some(b) = rf.strip_prefix("HEAD -> ") {
                (b.to_string(), ACCENT)
            } else if let Some(t) = rf.strip_prefix("tag: ") {
                (t.to_string(), S_YELLOW)
            } else if rf.contains('/') {
                (rf.clone(), S_VIOLET)
            } else if rf == "HEAD" {
                ("HEAD".into(), ACCENT)
            } else {
                (rf.clone(), S_AQUA)
            };
            x = put(buf, x, y, &format!("[{label}]"), limit.saturating_sub(x), Style::new().fg(color).bg(bg).add_modifier(Modifier::BOLD));
            x += 1;
        }
        if c.claude && x + 2 < limit {
            x = put(buf, x, y, "✳ ", 2, Style::new().fg(ACCENT).bg(bg).add_modifier(Modifier::BOLD));
        }
        put(buf, x, y, &c.subject, limit.saturating_sub(x), Style::new().fg(TEXT).bg(bg));
        put(buf, limit, y, &tail, tw, Style::new().fg(if c.claude { ACCENT } else { TEXT_2 }).bg(bg));
    }
}

fn draw_graph_cell(buf: &mut Buffer, x: u16, y: u16, c: Cell, bg: Color) {
    if c.ch == ' ' {
        return;
    }
    let color = LANES[c.color as usize % LANES.len()];
    let style = if c.ch == '●' { Style::new().fg(color).bg(bg).add_modifier(Modifier::BOLD) } else { Style::new().fg(color).bg(bg) };
    buf[(x, y)].set_char(c.ch).set_style(style);
}

fn branches(buf: &mut Buffer, v: &GitView, body: Rect) {
    let Some(b) = &v.branches else { return };
    let w = body.width.saturating_sub(4).min(70);
    let h = ((b.list.len() as u16) + 3).min(body.height.saturating_sub(2)).max(5);
    let r = Rect::new(body.x + (body.width - w) / 2, body.y + 1, w, h);
    Clear.render(r, buf);
    buf.set_style(r, Style::new().bg(SURFACE_1).fg(TEXT));
    put(buf, r.x + 1, r.y, " Branches ", w, Style::new().fg(ACCENT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
    let hint = " Enter switch · n new · r rename · d delete · Esc ";
    if w as usize > hint.width() + 12 {
        put(buf, r.right() - hint.width() as u16 - 1, r.y, hint, hint.width() as u16, Style::new().fg(MUTED).bg(SURFACE_1));
    }
    if let Some(e) = &b.error {
        put(buf, r.x + 2, r.y + 1, e, w - 4, Style::new().fg(CRITICAL).bg(SURFACE_1));
        return;
    }
    let rows = (h - 2) as usize;
    let first = b.sel.saturating_sub(rows.saturating_sub(1));
    for (i, br) in b.list.iter().enumerate().skip(first).take(rows) {
        let y = r.y + 1 + (i - first) as u16;
        let sel = i == b.sel;
        let bg = if sel { SELECTION } else { SURFACE_1 };
        buf.set_style(Rect::new(r.x, y, w, 1), Style::new().bg(bg));
        let mut x = put(buf, r.x + 1, y, if br.current { "● " } else { "  " }, 2, Style::new().fg(GOOD).bg(bg));
        x = put(buf, x, y, &br.name, 24, Style::new().fg(if br.current { GOOD } else { TEXT }).bg(bg).add_modifier(Modifier::BOLD));
        x = x.max(r.x + 27);
        let info = format!("{} {} {}", br.track, br.age, br.subject);
        put(buf, x, y, info.trim(), r.right().saturating_sub(x + 1), Style::new().fg(TEXT_2).bg(bg));
    }
}

#[cfg(test)]
pub fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    draw(&mut buf, app, area);
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::run;

    fn wait(app: &mut App, until: impl Fn(&App) -> bool) {
        let t = std::time::Instant::now();
        while !until(app) {
            assert!(t.elapsed() < std::time::Duration::from_secs(20), "timed out");
            let _ = app.dock_tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The view at 80 columns (list above the document) and 200 (list left, side by side),
    /// then the log with its graph and the ✳ on Claude's commit.
    #[test]
    fn draws_review_and_log_at_80_and_200_columns() {
        let dir = crate::git::tests::scratch_repo("view-ui");
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        run(&dir, &["commit", "-qam", "by claude\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();
        std::env::set_var("CATLAS_CLAUDE", "true");
        let mut app = App::new(vec![dir.clone(), dir.join("a.txt")]).unwrap();
        app.agents.runs.clear();
        wait(&mut app, |a| a.dock.git.root.is_some());
        app.open_git_view(None);
        wait(&mut app, |a| a.git_view.review.loaded && a.git_view.review.model.files.len() == 2);
        let s = screen(&mut app, 80, 20).join("\n");
        for want in [" Git  1 Changes   2 Log", "working tree 2 files +2 −0", "  M a.txt", "  ? new.txt", "@@ -1,3 +1,4 @@", "       4 + four", "F7 change"] {
            assert!(s.contains(want), "missing {want:?} in\n{s}");
        }
        assert!(s.lines().nth(3).unwrap().starts_with("────"), "list above the document at 80 columns:\n{s}");
        let wide = screen(&mut app, 200, 20).join("\n");
        assert!(wide.contains(" +0 │") || wide.contains("+1 −0 │"), "list left of the document:\n{wide}");
        assert!(app.git_view.review.sbs, "side by side at 200 columns");
        app.git_view.tab = crate::git::view::Tab::Log;
        wait(&mut app, |a| a.git_view.log.loaded);
        let s = screen(&mut app, 80, 20).join("\n");
        assert!(s.contains("[main] ✳ by claude"), "{s}");
        assert!(s.contains("first"), "{s}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
