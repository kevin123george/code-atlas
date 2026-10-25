//! Drawing the agents panel: header, run list, transcript of the selected run, staff, options
//! and the prompt box, top to bottom in a strip on the right.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use super::run::{fmt_ms, short_id, State};
use super::staff::Status;
use super::transcript::{self, ellipsize};
use super::{Area, PANEL_W};
#[cfg(test)]
use super::Agents;
use crate::app::App;
use crate::ui::theme::*;

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner() -> &'static str {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    SPIN[(ms / 120) as usize % SPIN.len()]
}

fn state_style(s: State) -> (&'static str, Color) {
    match s {
        State::Running => ("", ACCENT),
        State::Done => ("✓", GOOD),
        State::Failed => ("✗", CRITICAL),
        State::Stopped => ("■", WARNING),
    }
}

/// A divider row with a label: `── label ──────`.
fn divider(buf: &mut Buffer, x: u16, y: u16, w: u16, label: &str, label_style: Style) {
    for i in 0..w {
        buf[(x + i, y)].set_char('─').set_fg(GRID).set_bg(SURFACE_1);
    }
    if !label.is_empty() && w > 6 {
        let text = format!(" {} ", ellipsize(label, w as usize - 5));
        buf.set_stringn(x + 2, y, &text, w as usize - 2, label_style.bg(SURFACE_1));
    }
}

/// Draws the panel on the right of `area` (when shown and there's room). Returns what's left for
/// the editor and where the cursor goes when the prompt box has focus.
pub fn draw_panel(buf: &mut Buffer, app: &mut App, area: Rect) -> (Rect, Option<Position>) {
    let a = &mut app.agents;
    if !a.visible || area.width < PANEL_W + 30 || area.height < 8 {
        a.rect = Rect::default();
        return (area, None);
    }
    let r = Rect::new(area.right() - PANEL_W, area.y, PANEL_W, area.height);
    a.rect = r;
    buf.set_style(r, Style::new().bg(SURFACE_1).fg(TEXT));
    for y in r.y..r.bottom() {
        buf[(r.x, y)].set_char('│').set_fg(GRID).set_bg(PAPER);
    }
    // Content: one column of padding after the divider.
    let (x, w) = (r.x + 2, PANEL_W - 3);
    let mut y = r.y;
    let spin = spinner();

    // Header.
    let head = Style::new().fg(if a.focus { ACCENT } else { TEXT_2 }).bg(SURFACE_1).add_modifier(Modifier::BOLD);
    buf.set_string(r.x + 1, y, " Agents", head);
    let n = a.running_count();
    let right = if n > 0 { format!("{spin} {n} running") } else { "Alt+9".to_string() };
    let rw = right.width() as u16;
    buf.set_string(r.right() - rw - 1, y, &right, Style::new().fg(if n > 0 { ACCENT } else { MUTED }).bg(SURFACE_1));
    y += 1;

    // Height budget: list, transcript (rest), staff, options + prompt.
    let shown = a.shown();
    let (prompt_rows, cursor_rc) = a.input.layout(w as usize - 2);
    let prompt_h = (prompt_rows.len() as u16).clamp(1, 6);
    let staff_rows = match &a.staff.status {
        Status::Ok(m) if !m.is_empty() => m.len() as u16,
        _ => 1,
    };
    let avail = r.bottom() - y;
    let list_h = ((shown.len() + 1) as u16).min((avail / 5).max(2)).min(8);
    let mut staff_h = staff_rows.min(6);
    let fixed = |s: u16| list_h + 1 + 1 + s + 1 + prompt_h;
    if avail < fixed(staff_h) + 4 {
        staff_h = 0;
    }
    let trans_h = avail.saturating_sub(fixed(staff_h));

    // Run list ("+ New agent" first, then this project's runs, newest first).
    let list = Rect::new(r.x + 1, y, PANEL_W - 1, list_h);
    a.runs_rect = list;
    let sel_row = a.selected.and_then(|id| shown.iter().position(|&i| a.runs[i].id == id)).map_or(0, |p| p + 1);
    if sel_row < a.runs_scroll {
        a.runs_scroll = sel_row;
    } else if sel_row >= a.runs_scroll + list_h as usize {
        a.runs_scroll = sel_row + 1 - list_h as usize;
    }
    for row in 0..list_h as usize {
        let i = a.runs_scroll + row;
        if i > shown.len() {
            break;
        }
        let yy = y + row as u16;
        let selected = i == sel_row;
        let bg = match (selected, a.focus && a.area == Area::Runs) {
            (true, true) => SELECTION,
            (true, false) => SURFACE_2,
            _ => SURFACE_1,
        };
        buf.set_style(Rect::new(r.x + 1, yy, PANEL_W - 1, 1), Style::new().bg(bg));
        if i == 0 {
            buf.set_string(x, yy, "+ New agent", Style::new().fg(if selected { ACCENT } else { TEXT_2 }).bg(bg));
            continue;
        }
        let run = &a.runs[shown[i - 1]];
        let (icon, c) = state_style(run.state);
        let icon = if run.running() { spin } else { icon };
        let tail = format!(" {:>7} {:>6}", run.state.label(), fmt_ms(run.elapsed_ms()));
        let room = (w as usize).saturating_sub(2 + tail.width());
        buf.set_string(x, yy, icon, Style::new().fg(c).bg(bg));
        buf.set_stringn(x + 2, yy, ellipsize(&run.title, room), room, Style::new().fg(TEXT).bg(bg));
        buf.set_string(x + w - tail.width() as u16, yy, &tail, Style::new().fg(c).bg(bg));
    }
    y += list_h;

    // Transcript of the selected run, with its mode / agent / session on the divider.
    let sel = a.selected.and_then(|id| a.run_index(id));
    let meta = match sel.map(|i| &a.runs[i]) {
        Some(run) => {
            let mode = run.mode_used.clone().or(run.mode.clone()).unwrap_or_else(|| "default".into());
            let mut m = format!("mode {mode}");
            if let Some(ag) = &run.agent {
                m.push_str(&format!(" · {ag}"));
            }
            if let Some(s) = &run.session_id {
                m.push_str(&format!(" · {}", short_id(s)));
            }
            if run.cost > 0.0 {
                m.push_str(&format!(" · ${:.2}", run.cost));
            }
            m
        }
        None => "new agent".into(),
    };
    let focus_t = a.focus && a.area == Area::Transcript;
    divider(buf, r.x + 1, y, PANEL_W - 1, &meta, Style::new().fg(if focus_t { ACCENT } else { TEXT_2 }));
    y += 1;
    let tr = Rect::new(x, y, w, trans_h);
    a.transcript_rect = tr;
    match sel.map(|i| &a.runs[i]) {
        Some(run) => {
            let lines = transcript::render(&run.entries, w as usize, spin);
            let h = trans_h as usize;
            a.max_scroll = lines.len().saturating_sub(h);
            if a.follow {
                a.scroll = a.max_scroll;
            }
            a.scroll = a.scroll.min(a.max_scroll);
            for (row, l) in lines.iter().skip(a.scroll).take(h).enumerate() {
                let mut cx = x;
                for (t, st) in &l.spans {
                    if cx >= x + w {
                        break;
                    }
                    let (nx, _) = buf.set_stringn(cx, y + row as u16, t, (x + w - cx) as usize, st.bg(SURFACE_1));
                    cx = nx;
                }
            }
            if run.running() && lines.len() < h {
                let el = format!("{spin} working · {}", fmt_ms(run.elapsed_ms()));
                buf.set_stringn(x, y + lines.len() as u16, el, w as usize, Style::new().fg(ACCENT).bg(SURFACE_1));
            } else if run.running() && trans_h > 0 {
                // Keep the elapsed time in view on the last row's right.
                let el = format!(" {spin} {} ", fmt_ms(run.elapsed_ms()));
                let ew = el.width() as u16;
                buf.set_string(x + w - ew, y + trans_h - 1, el, Style::new().fg(ACCENT).bg(SURFACE_2));
            }
            if a.scroll < a.max_scroll && trans_h > 0 {
                let more = format!(" ↓ {} more ", a.max_scroll - a.scroll);
                buf.set_string(x + w - more.width() as u16, y, more, Style::new().fg(MUTED).bg(SURFACE_2));
            }
        }
        None => {
            a.max_scroll = 0;
            let help = [
                "Type a task below and press Enter: it runs",
                "`claude -p` in the project folder.",
                "",
                "Alt+M permission mode · Alt+A agent type",
                "Alt+I add the current file / selection",
                "Tab moves between list, transcript, prompt",
            ];
            for (i, t) in help.iter().enumerate().take(trans_h as usize) {
                buf.set_stringn(x, y + i as u16, t, w as usize, Style::new().fg(MUTED).bg(SURFACE_1));
            }
        }
    }
    y += trans_h;

    // Staff.
    let label = match (&a.staff.status, a.staff.at) {
        (_, _) if a.staff.busy() && a.staff.at.is_none() => format!("Aa staff ({}) · checking…", a.staff.source),
        (Status::Ok(_), Some(t)) => format!("Aa staff ({}) · {}s ago", a.staff.source, t.elapsed().as_secs()),
        (Status::Unreachable, _) => format!("Aa staff · {} unreachable", a.staff.source),
        _ => "Aa staff".into(),
    };
    let lstyle = Style::new().fg(if matches!(a.staff.status, Status::Unreachable) { MUTED } else { TEXT_2 });
    divider(buf, r.x + 1, y, PANEL_W - 1, &label, lstyle);
    y += 1;
    if staff_h > 0 {
        match &a.staff.status {
            Status::Ok(members) if !members.is_empty() => {
                for (i, m) in members.iter().take(staff_h as usize).enumerate() {
                    let yy = y + i as u16;
                    let c = match (m.live, m.state.as_str()) {
                        (false, _) => MUTED,
                        (true, "done" | "idle") => GOOD,
                        (true, "blocked") => WARNING,
                        _ => ACCENT,
                    };
                    buf.set_string(x, yy, if m.live { "●" } else { "○" }, Style::new().fg(c).bg(SURFACE_1));
                    buf.set_stringn(x + 2, yy, ellipsize(&m.name, 16), 16, Style::new().fg(if m.live { TEXT } else { TEXT_2 }).bg(SURFACE_1));
                    let tail = format!("{:<8} {}", m.state, if m.live { "live" } else { "stopped" });
                    buf.set_stringn(x + 19, yy, &tail, (w - 19) as usize, Style::new().fg(c).bg(SURFACE_1));
                    let id = format!(" {}", m.id);
                    if 19 + tail.width() + id.width() < w as usize {
                        buf.set_string(x + w - id.width() as u16, yy, id, Style::new().fg(MUTED).bg(SURFACE_1));
                    }
                }
            }
            Status::Ok(_) => {
                buf.set_stringn(x, y, "no staff sessions", w as usize, Style::new().fg(MUTED).bg(SURFACE_1));
            }
            Status::Unreachable => {
                buf.set_stringn(x, y, "laptop unreachable · retries every 60 s", w as usize, Style::new().fg(MUTED).bg(SURFACE_1));
            }
            Status::Never => {
                buf.set_stringn(x, y, "checking…", w as usize, Style::new().fg(MUTED).bg(SURFACE_1));
            }
        }
        y += staff_h;
    }

    // Options for the next send, then the prompt box.
    let agent = a.agent().unwrap_or_else(|| "no agent".into());
    let opts = format!("{} · {} · file {}", a.mode_label(), agent, if a.include_ctx { "on" } else { "off" });
    let focus_p = a.focus && a.area == Area::Prompt;
    divider(buf, r.x + 1, y, PANEL_W - 1, &opts, Style::new().fg(if focus_p { ACCENT } else { TEXT_2 }));
    y += 1;
    let pr = Rect::new(r.x + 1, y, PANEL_W - 1, prompt_h.min(r.bottom() - y));
    a.prompt_rect = pr;
    buf.set_style(pr, Style::new().bg(SURFACE_2));
    buf.set_string(x, y, "›", Style::new().fg(if focus_p { ACCENT } else { MUTED }).bg(SURFACE_2).add_modifier(Modifier::BOLD));
    let first = cursor_rc.0.saturating_sub(pr.height as usize - 1);
    if a.input.text.is_empty() {
        let hint = match sel.map(|i| &a.runs[i]) {
            None => "Describe a task for a new agent…".to_string(),
            Some(run) if run.running() => "Working… Ctrl+C stops it · Ctrl+N new agent".to_string(),
            Some(run) if run.session_id.is_none() => "No session to resume · Ctrl+N new agent".to_string(),
            Some(run) => format!("Follow-up to “{}”…", ellipsize(&run.title, 22)),
        };
        buf.set_stringn(x + 2, y, hint, w as usize - 2, Style::new().fg(MUTED).bg(SURFACE_2));
    } else {
        for (i, row) in prompt_rows.iter().skip(first).take(pr.height as usize).enumerate() {
            buf.set_stringn(x + 2, y + i as u16, row, w as usize - 2, Style::new().fg(TEXT).bg(SURFACE_2));
        }
    }
    let cursor = focus_p.then(|| Position::new(x + 2 + cursor_rc.1 as u16, y + (cursor_rc.0 - first) as u16));
    (Rect::new(area.x, area.y, area.width - PANEL_W, area.height), cursor)
}

/// Shared with tests: renders the panel of `app` into a plain-text screen.
#[cfg(test)]
pub fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    draw_panel(&mut buf, app, area);
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_shows_runs_transcript_staff_and_prompt() {
        let d = std::env::temp_dir().join(format!("catlas-agents-ui-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut app = App::new(vec![d.clone()]).unwrap();
        let a: &mut Agents = &mut app.agents;
        a.runs.clear();
        a.visible = true;
        a.focus = true;
        let mut run = super::super::run::Run::new(7, crate::project::canonical(&d), "create hello.txt containing hi");
        run.begin_turn(&super::super::run::Turn { shown: "create hello.txt containing hi".into(), mode: Some("acceptEdits".into()), ..Default::default() });
        for l in include_str!("testdata/write-run.jsonl").lines() {
            run.feed_line(l);
        }
        run.proc = None;
        a.runs.push(run);
        a.selected = Some(7);
        a.staff.status = Status::Ok(super::super::staff::parse("Aa · Ops  85360144  done  live  /home/astra/home-lab\n"));
        let s = screen(&mut app, 100, 30).join("\n");
        for want in ["Agents", "+ New agent", "create hello.txt", "done", "mode acceptEdits", "⏺ Write ", "✓ done in 7.4s", "Aa · Ops", "default · no agent · file off", "Follow-up to"] {
            assert!(s.contains(want), "missing {want:?} in\n{s}");
        }
        std::fs::remove_dir_all(d).unwrap();
    }
}
