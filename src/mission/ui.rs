//! Drawing mission control (a full-screen view under the tab bar), the ask-an-agent popup and
//! the status-bar ticker. Terminal-native and calm: Ledger Terminal tokens, one colour per agent,
//! a card of three rows per agent, the keys in the footer.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Clear, Widget};
use unicode_width::UnicodeWidthStr;

use super::presence::{self, GLYPH};
use super::{repo_name, InputKind, View, PRESETS};
use crate::agents::run::{fmt_ms, now_ms, Activity, Run, State};
use crate::agents::transcript::{self, ellipsize};
use crate::agents::ui::spinner;
use crate::app::App;
use crate::ui::theme::*;

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Sparkline: 24 buckets of 5 s (the last two minutes).
const SPARK_N: usize = 24;
const SPARK_MS: u64 = 5_000;

/// `▁▂▅▇` from counts (a dot for no activity at all).
pub fn sparkline(v: &[u32]) -> String {
    let max = v.iter().copied().max().unwrap_or(0);
    v.iter()
        .map(|&n| match (n, max) {
            (0, _) => '·',
            (_, 0) => '·',
            _ => BARS[((n as usize * BARS.len()).div_ceil(max as usize)).clamp(1, BARS.len()) - 1],
        })
        .collect()
}

pub fn tokens(n: u64) -> String {
    match n {
        0..=999 => format!("{n} tok"),
        1_000..=999_999 => format!("{:.1}k tok", n as f64 / 1e3),
        _ => format!("{:.1}M tok", n as f64 / 1e6),
    }
}

fn activity_color(a: &Activity) -> Color {
    match a {
        a if a.working() => ACCENT,
        Activity::Waiting | Activity::Landed(_) => GOOD,
        Activity::Attention(_) | Activity::Stopped => WARNING,
        Activity::Failed => CRITICAL,
        _ => TEXT_2,
    }
}

fn state_glyph(r: &Run, spin: &'static str) -> (&'static str, Color) {
    match (r.state, r.activity()) {
        (_, Activity::Landed(l)) if l == "discarded" => ("○", MUTED),
        (_, Activity::Landed(_)) => ("◆", GOOD),
        (State::Running, _) => (spin, ACCENT),
        (State::Done, Activity::Attention(_)) => ("!", WARNING),
        (State::Done, _) => ("✓", GOOD),
        (State::Failed, _) => ("✗", CRITICAL),
        (State::Stopped, _) => ("■", WARNING),
    }
}

/// Writes `s` at (x, y) within `max` columns; returns the next x.
fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, max: u16, st: Style) -> u16 {
    if max == 0 {
        return x;
    }
    buf.set_stringn(x, y, s, max as usize, st).0
}

/// Writes `s` right-aligned so it ends at `right`; returns where it starts.
fn put_right(buf: &mut Buffer, right: u16, y: u16, s: &str, st: Style) -> u16 {
    let x = right.saturating_sub(s.width() as u16);
    buf.set_string(x, y, s, st);
    x
}

/// Mission control and the ask popup, over everything but the status bar. Returns the cursor
/// for an input line.
pub fn draw(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let mut cursor = None;
    if app.mission.open && area.width >= 40 && area.height >= 8 {
        cursor = draw_view(buf, app, area);
    } else {
        app.mission.rect = Rect::default();
        app.mission.cards.clear();
    }
    if app.mission.ask.is_some() {
        cursor = draw_ask(buf, app, area).or(cursor);
    }
    cursor
}

fn draw_view(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    Clear.render(area, buf);
    buf.set_style(area, Style::new().bg(PAPER).fg(TEXT));
    app.mission.rect = area;
    let spin = spinner();
    let runs = app.mission_runs();
    let (x, w) = (area.x + 1, area.width.saturating_sub(2));
    let right = x + w;

    // Title bar.
    let y = area.y;
    buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(SURFACE_2));
    let mut cx = put(buf, area.x, y, " ◈ MISSION CONTROL ", w, Style::new().fg(ACCENT).bg(SURFACE_2).add_modifier(Modifier::BOLD));
    let all: Vec<&Run> = runs.iter().map(|&i| &app.agents.runs[i]).collect();
    let working = all.iter().filter(|r| r.running()).count() + app.mission.pending.len();
    let attention = all.iter().filter(|r| matches!(r.activity(), Activity::Attention(_) | Activity::Failed)).count();
    let cost: f64 = all.iter().map(|r| r.cost).sum();
    let toks: u64 = all.iter().map(|r| r.tokens).sum();
    let n = all.len() + app.mission.pending.len();
    let mut stats = format!(" {n} agent{}", if n == 1 { "" } else { "s" });
    if working > 0 {
        stats.push_str(&format!(" · {spin} {working} working"));
    }
    if attention > 0 {
        stats.push_str(&format!(" · {attention} need{} you", if attention == 1 { "s" } else { "" }));
    }
    if cost > 0.0 {
        stats.push_str(&format!(" · ${cost:.2} · {}", tokens(toks)));
    }
    cx = put(buf, cx, y, &stats, right.saturating_sub(cx), Style::new().fg(TEXT_2).bg(SURFACE_2));
    let repo = format!("{}  Ctrl+O close ", repo_name(&app.dock.git.root.clone().unwrap_or_else(|| app.project.root.clone())));
    if cx + repo.width() as u16 + 2 < area.right() {
        put_right(buf, area.right(), y, &repo, Style::new().fg(MUTED).bg(SURFACE_2));
    }
    for i in 0..area.width {
        buf[(area.x + i, y + 1)].set_char('─').set_fg(GRID).set_bg(PAPER);
    }

    // A question too long for one row gets two.
    let footer_h = match &app.mission.confirm {
        Some(c) if c.question.width() as u16 > area.width.saturating_sub(2) => 2,
        _ => 1,
    };
    let body = Rect::new(area.x, y + 2, area.width, area.height.saturating_sub(2 + footer_h));
    let footer_y = area.bottom() - footer_h;
    app.mission.cards.clear();
    match app.mission.view {
        View::Transcript if app.mission.sel.is_some_and(|id| app.agents.run_index(id).is_some()) => draw_transcript(buf, app, body, spin),
        _ => draw_cards(buf, app, body, &runs, spin),
    }
    draw_footer(buf, app, Rect::new(area.x, footer_y, area.width, footer_h))
}

fn draw_cards(buf: &mut Buffer, app: &mut App, body: Rect, runs: &[usize], spin: &'static str) {
    let (x, w) = (body.x + 1, body.width.saturating_sub(2));
    let pending = &app.mission.pending;
    if runs.is_empty() && pending.is_empty() {
        let help = [
            "No agents yet.",
            "",
            "n          start an agent in its own git worktree (branch + folder <repo>-wt/<task>)",
            "Ctrl+U     on code in the editor: ask an agent to explain / fix / write tests",
            "Alt+9      the agents panel (agents in place, the Aa staff)",
            "",
            "Each agent gets a card: what it is doing, time, cost, files changed. Review it (d),",
            "then land it (l: merge its branch, always asked, never pushed) or discard it (x).",
        ];
        for (i, t) in help.iter().enumerate().take(body.height as usize) {
            put(buf, x + 2, body.y + 1 + i as u16, t, w.saturating_sub(2), Style::new().fg(if i == 0 { TEXT } else { MUTED }).bg(PAPER));
        }
        return;
    }
    const CARD_H: u16 = 4;
    let per_page = (body.height / CARD_H).max(1) as usize;
    let sel_pos = app.mission.sel.and_then(|s| runs.iter().position(|&i| app.agents.runs[i].id == s)).map_or(0, |p| p + pending.len());
    if sel_pos < app.mission.scroll {
        app.mission.scroll = sel_pos;
    } else if sel_pos >= app.mission.scroll + per_page {
        app.mission.scroll = sel_pos + 1 - per_page;
    }
    let mut yy = body.y;
    let now = now_ms();
    let total = pending.len() + runs.len();
    for idx in app.mission.scroll..total {
        if yy + 3 > body.bottom() {
            break;
        }
        let card = Rect::new(body.x, yy, body.width, 3);
        if idx < pending.len() {
            let p = &app.mission.pending[idx];
            let c = presence::color(p.id);
            buf.set_style(card, Style::new().bg(SURFACE_1));
            for r in 0..3 {
                buf[(body.x, yy + r)].set_char('▌').set_fg(c).set_bg(PAPER);
            }
            let mut cx = put(buf, x + 1, yy, &format!("#{} ", p.id), w, Style::new().fg(c).bg(SURFACE_1).add_modifier(Modifier::BOLD));
            cx = put(buf, cx, yy, &format!("{spin} "), 2, Style::new().fg(ACCENT).bg(SURFACE_1));
            let tail = "creating worktree…";
            let room = (x + w).saturating_sub(cx).saturating_sub(tail.width() as u16 + 2);
            put(buf, cx, yy, &ellipsize(&p.title, room as usize), room, Style::new().fg(TEXT).bg(SURFACE_1).add_modifier(Modifier::BOLD));
            put_right(buf, x + w, yy, tail, Style::new().fg(ACCENT).bg(SURFACE_1));
            put(buf, x + 4, yy + 1, &format!("{} · new worktree from HEAD", repo_name(&p.repo)), w.saturating_sub(4), Style::new().fg(TEXT_2).bg(SURFACE_1));
            yy += CARD_H;
            continue;
        }
        let r = &app.agents.runs[runs[idx - pending.len()]];
        let selected = app.mission.sel == Some(r.id);
        let bg = if selected { SELECTION } else { SURFACE_1 };
        let c = presence::color(r.id);
        app.mission.cards.push((card, r.id));
        buf.set_style(card, Style::new().bg(bg));
        for row in 0..3 {
            buf[(body.x, yy + row)].set_char(if selected { '▐' } else { '▌' }).set_fg(c).set_bg(PAPER);
        }
        let act = r.activity();
        let (g, gc) = state_glyph(r, spin);
        // Row 1: id, state, title · activity, elapsed.
        let mut cx = put(buf, x + 1, yy, &format!("#{} ", r.id), w, Style::new().fg(c).bg(bg).add_modifier(Modifier::BOLD));
        cx = put(buf, cx, yy, &format!("{g} "), 2, Style::new().fg(gc).bg(bg).add_modifier(Modifier::BOLD));
        let tail = format!("{}   {:>6}", act.label(), fmt_ms(r.elapsed_ms()));
        let tail_w = (tail.width() as u16).min(w / 2);
        let room = (x + w).saturating_sub(cx).saturating_sub(tail_w + 2);
        put(buf, cx, yy, &ellipsize(&r.title, room as usize), room, Style::new().fg(TEXT).bg(bg).add_modifier(Modifier::BOLD));
        let tail = ellipsize(&tail, tail_w as usize);
        let tx = put_right(buf, x + w, yy, &tail, Style::new().fg(activity_color(&act)).bg(bg));
        // The elapsed part in a quieter colour.
        let el = format!("{:>6}", fmt_ms(r.elapsed_ms()));
        if tail.ends_with(&el) {
            put_right(buf, x + w, yy, &el, Style::new().fg(TEXT_2).bg(bg));
        }
        let _ = tx;
        // Row 2: where it works · mode, then money and changes on the right.
        let mut where_ = match &r.worktree {
            Some(wt) => format!("⎇ {} ← {}", wt.branch, wt.base_label()),
            None => "in place".into(),
        };
        where_.push_str(&format!(" · {}", repo_name(r.home())));
        if let Some(m) = r.mode_used.clone().or(r.mode.clone()) {
            where_.push_str(&format!(" · {m}"));
        }
        let mut money = Vec::new();
        if r.cost > 0.0 {
            money.push(format!("${:.2}", r.cost));
        }
        if r.tokens > 0 {
            money.push(tokens(r.tokens));
        }
        if let Some(s) = app.mission.live.get(&r.id).filter(|_| r.running()).or(r.stat.as_ref()) {
            money.push(s.clone());
        }
        let money = money.join(" · ");
        let mw = (money.width() as u16).min(w / 2);
        let room = w.saturating_sub(4 + mw + 2);
        put(buf, x + 4, yy + 1, &ellipsize(&where_, room as usize), room, Style::new().fg(if r.worktree.is_some() { S_BLUE } else { TEXT_2 }).bg(bg));
        put_right(buf, x + w, yy + 1, &ellipsize(&money, mw as usize), Style::new().fg(TEXT_2).bg(bg));
        // Row 3: the last tool line, the sparkline.
        let spark = sparkline(&r.sparkline(now, SPARK_MS, SPARK_N));
        let show_spark = w > 60;
        let room = w.saturating_sub(4 + if show_spark { SPARK_N as u16 + 2 } else { 0 });
        let last = r.last_line().unwrap_or_default();
        put(buf, x + 4, yy + 2, &ellipsize(&last, room as usize), room, Style::new().fg(MUTED).bg(bg));
        if show_spark {
            put_right(buf, x + w, yy + 2, &spark, Style::new().fg(if r.running() { c } else { BASELINE }).bg(bg));
        }
        yy += CARD_H;
    }
    if app.mission.scroll > 0 {
        put_right(buf, body.right() - 1, body.y, &format!(" ↑ {} more ", app.mission.scroll), Style::new().fg(MUTED).bg(SURFACE_2));
    }
    let shown = ((yy - body.y) / CARD_H) as usize;
    let below = total.saturating_sub(app.mission.scroll + shown);
    if below > 0 && body.height > 1 {
        put_right(buf, body.right() - 1, body.bottom() - 1, &format!(" ↓ {below} more "), Style::new().fg(MUTED).bg(SURFACE_2));
    }
}

fn draw_transcript(buf: &mut Buffer, app: &mut App, body: Rect, spin: &'static str) {
    let Some(i) = app.mission.sel.and_then(|id| app.agents.run_index(id)) else { return };
    let r = &app.agents.runs[i];
    let (x, w) = (body.x + 2, body.width.saturating_sub(4));
    let c = presence::color(r.id);
    let (g, gc) = state_glyph(r, spin);
    let mut cx = put(buf, x, body.y, &format!("#{} ", r.id), w, Style::new().fg(c).add_modifier(Modifier::BOLD));
    cx = put(buf, cx, body.y, &format!("{g} "), 2, Style::new().fg(gc));
    let act = r.activity().label();
    let room = (x + w).saturating_sub(cx).saturating_sub(act.width() as u16 + 2);
    put(buf, cx, body.y, &ellipsize(&r.title, room as usize), room, Style::new().fg(TEXT).add_modifier(Modifier::BOLD));
    put_right(buf, x + w, body.y, &act, Style::new().fg(activity_color(&r.activity())));
    let mut meta = vec![match &r.worktree {
        Some(wt) => format!("⎇ {} · {}", wt.branch, wt.path.display()),
        None => format!("in place · {}", r.cwd.display()),
    }];
    if r.cost > 0.0 {
        meta.push(format!("${:.2} · {}", r.cost, tokens(r.tokens)));
    }
    if let Some(s) = &r.stat {
        meta.push(s.clone());
    }
    put(buf, x, body.y + 1, &ellipsize(&meta.join(" · "), w as usize), w, Style::new().fg(MUTED));
    let top = body.y + 3;
    let h = body.bottom().saturating_sub(top) as usize;
    let lines = transcript::render(&r.entries, w as usize, spin);
    let m = &mut app.mission;
    m.t_max = lines.len().saturating_sub(h);
    if m.t_follow {
        m.t_scroll = m.t_max;
    }
    m.t_scroll = m.t_scroll.min(m.t_max);
    for (row, l) in lines.iter().skip(m.t_scroll).take(h).enumerate() {
        let mut cx = x;
        for (t, st) in &l.spans {
            if cx >= x + w {
                break;
            }
            cx = buf.set_stringn(cx, top + row as u16, t, (x + w - cx) as usize, *st).0;
        }
    }
    if m.t_scroll < m.t_max {
        put_right(buf, x + w, top, &format!(" ↓ {} more ", m.t_max - m.t_scroll), Style::new().fg(MUTED).bg(SURFACE_2));
    }
}

fn draw_footer(buf: &mut Buffer, app: &mut App, r: Rect) -> Option<Position> {
    buf.set_style(r, Style::new().bg(SURFACE_2).fg(TEXT_2));
    let m = &app.mission;
    let w = r.width.saturating_sub(2);
    if let Some(c) = &m.confirm {
        for (i, l) in transcript::wrap(&c.question, w as usize).iter().take(r.height as usize).enumerate() {
            put(buf, r.x + 1, r.y + i as u16, l, w, Style::new().fg(WARNING).bg(SURFACE_2).add_modifier(Modifier::BOLD));
        }
        return None;
    }
    let r = Rect::new(r.x, r.y, r.width, 1);
    if let Some(inp) = &m.input {
        let label = match &inp.kind {
            InputKind::FollowUp(id) => format!(" follow-up #{id} › "),
            InputKind::New { worktree, repo } => {
                let repos = app.mission_repos();
                let name = repos.get(*repo).map(|p| repo_name(p)).unwrap_or_default();
                format!(" new agent · {name} · {} › ", if *worktree { "worktree" } else { "in place" })
            }
        };
        let lx = put(buf, r.x, r.y, &label, w, Style::new().fg(ACCENT).bg(SURFACE_2).add_modifier(Modifier::BOLD));
        let room = r.right().saturating_sub(lx + 1) as usize;
        let n = inp.text.chars().count();
        let shown: String = inp.text.chars().skip(n.saturating_sub(room.saturating_sub(1))).collect();
        let cx = put(buf, lx, r.y, &shown, room as u16, Style::new().fg(TEXT).bg(SURFACE_2));
        if inp.text.is_empty() {
            let hint = match inp.kind {
                InputKind::New { .. } => "what should it do? · Tab worktree/in place · Ctrl+R repo · Esc",
                InputKind::FollowUp(_) => "what next? · Enter send · Esc",
            };
            put(buf, lx, r.y, hint, room as u16, Style::new().fg(MUTED).bg(SURFACE_2));
        }
        return Some(Position::new(cx.min(r.right() - 1), r.y));
    }
    if let Some(b) = &m.busy {
        put(buf, r.x + 1, r.y, &format!("{} {b}…", spinner()), w, Style::new().fg(ACCENT).bg(SURFACE_2));
        return None;
    }
    let keys = match m.view {
        View::Cards => "↑↓ agent · Enter transcript · n new · f follow-up · d review · l land · w remove worktree · x discard · s stop · Del forget · Esc close",
        View::Transcript => "↑↓ PgUp PgDn scroll · End follow · f follow-up · d review · l land · x discard · s stop · Esc back",
    };
    put(buf, r.x + 1, r.y, keys, w, Style::new().fg(TEXT_2).bg(SURFACE_2));
    None
}

fn draw_ask(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let a = app.mission.ask.as_ref()?;
    let w = area.width.saturating_sub(4).min(76);
    let h = 8u16;
    if w < 30 || area.height < h + 2 {
        return None;
    }
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + 2, w, h);
    Clear.render(r, buf);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BASELINE).bg(SURFACE_1))
        .style(Style::new().bg(SURFACE_1).fg(TEXT))
        .title(format!(" {GLYPH} Ask an agent · {} ", ellipsize(&a.label, w as usize - 22)))
        .title_style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
        .title_bottom(" Enter start · ↑↓ pick · Tab where · Esc ")
        .render(r, buf);
    for x in r.x + 1..r.right() - 1 {
        let c = &mut buf[(x, r.bottom() - 1)];
        if c.symbol() != "─" {
            c.set_fg(MUTED);
        }
    }
    let inner = Rect::new(r.x + 2, r.y + 1, r.width - 4, r.height - 2);
    put(buf, inner.x, inner.y, "› ", inner.width, Style::new().fg(ACCENT).bg(SURFACE_1));
    let room = inner.width.saturating_sub(3) as usize;
    let n = a.text.chars().count();
    let shown: String = a.text.chars().skip(n.saturating_sub(room)).collect();
    let cx = put(buf, inner.x + 2, inner.y, &shown, room as u16, Style::new().fg(TEXT).bg(SURFACE_1));
    if a.text.is_empty() {
        put(buf, inner.x + 2, inner.y, "type a task, or pick one below", room as u16, Style::new().fg(MUTED).bg(SURFACE_1));
    }
    for (i, (name, _, _)) in PRESETS.iter().enumerate() {
        let y = inner.y + 2 + i as u16;
        let on = a.text.is_empty() && a.sel == i;
        let bg = if on { SELECTION } else { SURFACE_1 };
        buf.set_style(Rect::new(inner.x, y, inner.width, 1), Style::new().bg(bg));
        put(buf, inner.x + 2, y, name, 14, Style::new().fg(if on { ACCENT } else { TEXT_2 }).bg(bg));
        let where_ = if PRESETS[i].2 { "in a new worktree, then you review + land" } else { "in place, reads only, answer in the agents panel" };
        put(buf, inner.x + 16, y, where_, inner.width.saturating_sub(16), Style::new().fg(MUTED).bg(bg));
    }
    let where_ = if a.worktree { "▸ runs in a new worktree (Tab: in place)" } else { "▸ runs in place (Tab: new worktree)" };
    put(buf, inner.x, inner.y + 5, where_, inner.width, Style::new().fg(if a.worktree { S_BLUE } else { TEXT_2 }).bg(SURFACE_1));
    Some(Position::new(cx.min(inner.right() - 1), inner.y))
}

/// The ticker left of `right_w` columns already used on the status bar's right. Returns the
/// width it took (0 = nothing to show or no room).
pub fn draw_ticker(buf: &mut Buffer, app: &App, area: Rect, right_w: u16) -> u16 {
    let Some((text, color)) = presence::ticker(&app.agents, app.mission.pending.len()) else { return 0 };
    let text = format!(" {text} ");
    let tw = text.width() as u16;
    if area.width < right_w + tw + 30 {
        return 0;
    }
    let x = area.right() - right_w - tw;
    buf.set_string(x, area.y, &text, Style::new().fg(color).bg(SURFACE_2));
    tw
}

#[cfg(test)]
pub fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    draw(&mut buf, app, area);
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}
