//! The bottom dock (M5): a tool-window area under the editor with tabs (Terminal, Commit), like
//! IntelliJ's bottom tool windows. Owns the terminal (`src/term/`) and git state (`src/git/`).
//!
//! Hooks elsewhere: `App.dock`, `dock_key`/`dock_paste`/`dock_mouse` first in `App::handle`,
//! `dock_tick` (redraw when it says so) in the main loop, `split`/`draw`/`draw_over_editor` in `ui::draw`,
//! gutter marks in `ui::draw_text`, the branch in the status bar and tree colours in the sidebar.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

use crate::app::App;
use crate::git::{Field, Git};
use crate::project::Target;
use crate::term::Terminal;
use crate::ui::theme::*;

/// Dock height in rows, its tab row included.
pub const DEFAULT_H: u16 = 12;
pub const MIN_H: u16 = 4;
/// The editor keeps at least this many rows above the dock.
pub const MIN_EDITOR_H: u16 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Terminal,
    Commit,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Terminal => "Terminal",
            Tab::Commit => "Commit",
        }
    }
}

pub struct Dock {
    pub open: bool,
    pub focused: bool,
    pub tab: Tab,
    pub height: u16,
    pub term: Option<Terminal>,
    pub git: Git,
    root: PathBuf,
    /// Screen areas from the last draw (mouse, terminal size).
    pub rect: Rect,
    body: Rect,
    diff_rect: Rect,
    list_rect: Rect,
    msg_rect: Rect,
    tab_spans: Vec<(u16, u16, Tab)>,
}

/// Splits the editor area into (editor, dock) for a dock of `want` rows. No dock when there is
/// no room for both.
pub fn split_rows(area: Rect, want: u16) -> (Rect, Option<Rect>) {
    if area.height < MIN_EDITOR_H + MIN_H {
        return (area, None);
    }
    let h = want.clamp(MIN_H, area.height - MIN_EDITOR_H);
    (
        Rect::new(area.x, area.y, area.width, area.height - h),
        Some(Rect::new(area.x, area.bottom() - h, area.width, h)),
    )
}

pub fn split(area: Rect, dock: &Dock) -> (Rect, Option<Rect>) {
    if dock.open {
        split_rows(area, dock.height)
    } else {
        (area, None)
    }
}

impl Dock {
    pub fn new(root: &Path) -> Self {
        Dock {
            open: false,
            focused: false,
            tab: Tab::Terminal,
            height: DEFAULT_H,
            term: None,
            git: Git::new(root),
            root: root.to_path_buf(),
            rect: Rect::default(),
            body: Rect::default(),
            diff_rect: Rect::default(),
            list_rect: Rect::default(),
            msg_rect: Rect::default(),
            tab_spans: Vec::new(),
        }
    }

    /// Keys go to the dock (or the diff view) rather than the editor.
    pub fn has_focus(&self) -> bool {
        self.focused || self.git.diff.is_some()
    }

    /// Starts a shell when there is none (or the last one exited). Returns an error to show.
    fn ensure_term(&mut self) -> Option<String> {
        if self.term.as_ref().is_some_and(|t| t.exited.is_none()) {
            return None;
        }
        let (rows, cols) = if self.body.height > 0 {
            (self.body.height, self.body.width)
        } else {
            (self.height - 1, crossterm::terminal::size().map(|s| s.0).unwrap_or(80))
        };
        match Terminal::spawn(&self.root, rows, cols) {
            Ok(t) => {
                self.term = Some(t);
                None
            }
            Err(e) => Some(format!("Couldn't start the terminal: {e:#}")),
        }
    }

    fn resize_by(&mut self, d: i16) {
        self.height = (self.height as i16 + d).clamp(MIN_H as i16, 60) as u16;
    }
}

const TERM_HINT: &str = "Terminal: keys go to the shell · Ctrl+Alt+↑ editor · Alt+F12 hide · Shift+PgUp/PgDn scroll";
const COMMIT_HINT: &str = "Commit: Space tick · Enter diff · Tab message · Ctrl+Enter / Alt+Enter commit · Esc editor";

// ---- glue: the dock's side of `App` ---------------------------------------------------------

impl App {
    /// Called every loop turn next to `tick()`; true when the screen needs a redraw (shell output,
    /// git results). The loop polls every 30 ms, so shell output shows within a frame or two.
    pub fn dock_tick(&mut self) -> bool {
        let mut changed = false;
        if let Some(t) = &mut self.dock.term {
            changed |= t.poll();
        }
        if let Some(m) = self.dock.git.poll(&self.docs, self.active) {
            self.message = Some(m);
            changed = true;
        }
        changed |= self.git_view_tick();
        changed | self.dock.git.take_changed()
    }

    fn toggle_terminal(&mut self) {
        let d = &mut self.dock;
        if d.open && d.focused && d.tab == Tab::Terminal {
            d.open = false;
            d.focused = false;
            self.message = None;
            return;
        }
        d.open = true;
        d.focused = true;
        d.tab = Tab::Terminal;
        d.git.diff = None;
        self.project.focus_tree = false;
        self.agents.focus = false;
        self.message = Some(d.ensure_term().unwrap_or_else(|| TERM_HINT.into()));
    }

    fn show_commit(&mut self, field: Field, toggle: bool) {
        let d = &mut self.dock;
        if toggle && d.open && d.focused && d.tab == Tab::Commit {
            d.open = false;
            d.focused = false;
            self.message = None;
            return;
        }
        d.open = true;
        d.focused = true;
        d.tab = Tab::Commit;
        d.git.panel.field = field;
        d.git.refresh();
        self.project.focus_tree = false;
        self.agents.focus = false;
        self.message = Some(COMMIT_HINT.into());
    }

    fn try_commit(&mut self) {
        self.message = Some(match self.dock.git.commit() {
            Ok(()) => "Committing…".into(),
            Err(e) => format!("Couldn't commit: {e}"),
        });
    }

    /// Dock keys: the tool-window keys from anywhere, then the focused dock tab or the diff view.
    /// Returns false when the key is not the dock's.
    pub fn dock_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ch = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        if alt && !ctrl && k.code == KeyCode::F(12) {
            self.toggle_terminal();
            return true;
        }
        if alt && !ctrl && ch('0') {
            self.dock.git.diff = None;
            self.show_commit(Field::List, true);
            return true;
        }
        if ctrl && !alt && !shift && ch('k') {
            let d = &self.dock;
            if d.focused && d.tab == Tab::Commit && d.git.panel.field == Field::Message && !d.git.panel.message.trim().is_empty() {
                self.try_commit();
            } else {
                self.dock.git.diff = None;
                self.show_commit(Field::Message, false);
            }
            return true;
        }
        // Other tool windows (Alt+1 project, Alt+9 agents) take the focus.
        if alt && !ctrl && (ch('1') || ch('9')) {
            self.dock.focused = false;
            self.dock.git.diff = None;
            return false;
        }
        if self.dock.git.diff.is_some() {
            return self.diff_key(k);
        }
        if !self.dock.focused {
            return false;
        }
        if ctrl && alt && k.code == KeyCode::Up {
            self.dock.focused = false;
            self.message = None;
            return true;
        }
        if shift && !ctrl && !alt && k.code == KeyCode::Esc {
            self.dock.open = false;
            self.dock.focused = false;
            return true;
        }
        if ctrl && shift && !alt && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            self.dock.resize_by(if k.code == KeyCode::Up { 2 } else { -2 });
            return true;
        }
        match self.dock.tab {
            Tab::Terminal => {
                self.terminal_key(k);
                true
            }
            Tab::Commit => self.commit_key(k),
        }
    }

    fn terminal_key(&mut self, k: KeyEvent) {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let d = &mut self.dock;
        let alive = d.term.as_ref().is_some_and(|t| t.exited.is_none());
        if !alive {
            match k.code {
                KeyCode::Enter => self.message = Some(d.ensure_term().unwrap_or_else(|| TERM_HINT.into())),
                KeyCode::Esc => d.focused = false,
                _ => {}
            }
            return;
        }
        let t = d.term.as_mut().unwrap();
        let page = (d.body.height.max(2) - 1) as isize;
        match k.code {
            KeyCode::PageUp if shift => t.scroll(page),
            KeyCode::PageDown if shift => t.scroll(-page),
            _ => {
                if let Some(bytes) = crate::term::keys::encode(&k, t.screen().application_cursor()) {
                    t.scroll_to_bottom();
                    t.write(&bytes);
                }
            }
        }
    }

    fn commit_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let git = &mut self.dock.git;
        // Ctrl+Enter (kitty protocol), Alt+Enter and Ctrl+J (what Ctrl+Enter sends elsewhere).
        if (k.code == KeyCode::Enter && (ctrl || alt)) || (ctrl && k.code == KeyCode::Char('j')) {
            self.try_commit();
            return true;
        }
        match k.code {
            KeyCode::Tab | KeyCode::BackTab => {
                git.panel.field = if git.panel.field == Field::List { Field::Message } else { Field::List };
                return true;
            }
            KeyCode::Esc => {
                if git.panel.field == Field::Message {
                    git.panel.field = Field::List;
                } else {
                    self.dock.focused = false;
                    self.message = None;
                }
                return true;
            }
            KeyCode::F(5) => {
                git.refresh();
                return true;
            }
            _ => {}
        }
        let len = git.entries.len();
        let p = &mut git.panel;
        let page = self.dock.list_rect.height.saturating_sub(1).max(1) as isize;
        let mut step = |d: isize| {
            if len > 0 {
                p.cursor = (p.cursor as isize + d).clamp(0, len as isize - 1) as usize;
            }
        };
        match p.field {
            Field::List => match k.code {
                KeyCode::Up => step(-1),
                KeyCode::Down => step(1),
                KeyCode::PageUp => step(-page),
                KeyCode::PageDown => step(page),
                KeyCode::Home => step(isize::MIN / 2),
                KeyCode::End => step(isize::MAX / 2),
                KeyCode::Char(' ') if !ctrl && !alt => {
                    let i = p.cursor;
                    git.toggle(i);
                }
                KeyCode::Char('a') if ctrl => git.toggle_all(),
                KeyCode::Enter | KeyCode::F(4) => {
                    let i = p.cursor;
                    git.open_diff(i);
                }
                // Typing starts the message, as in IDEA's commit window.
                KeyCode::Char(c) if !ctrl && !alt => {
                    p.field = Field::Message;
                    p.message.push(c);
                }
                _ if ctrl || alt => return false,
                _ => {}
            },
            Field::Message => match k.code {
                KeyCode::Enter => p.message.push('\n'),
                KeyCode::Backspace if ctrl || alt => pop_word(&mut p.message),
                KeyCode::Char('h') if ctrl => pop_word(&mut p.message),
                KeyCode::Backspace => {
                    p.message.pop();
                }
                KeyCode::Char('u') if ctrl => {
                    let keep = p.message.rfind('\n').map(|i| i + 1).unwrap_or(0);
                    p.message.truncate(keep);
                }
                KeyCode::Char(c) if !ctrl && !alt => p.message.push(c),
                _ if ctrl || alt => return false,
                _ => {}
            },
        }
        true
    }

    fn diff_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let git = &mut self.dock.git;
        let Some(v) = &mut git.diff else { return false };
        let page = v.view_h.saturating_sub(2).max(1);
        let max = v.rows.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc => {
                git.diff = None;
                self.dock.open = true;
                self.dock.focused = true;
                self.dock.tab = Tab::Commit;
            }
            KeyCode::Up if alt => git.step_diff(-1),
            KeyCode::Down if alt => git.step_diff(1),
            KeyCode::Up => v.scroll = v.scroll.saturating_sub(1),
            KeyCode::Down => v.scroll = (v.scroll + 1).min(max),
            KeyCode::PageUp => v.scroll = v.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => v.scroll = (v.scroll + page).min(max),
            KeyCode::Home => v.scroll = 0,
            KeyCode::End => v.scroll = max,
            KeyCode::F(7) | KeyCode::Char('n') | KeyCode::Char('p') => {
                let back = (k.code == KeyCode::F(7) && shift) || k.code == KeyCode::Char('p');
                let hunks: Vec<usize> = v.hunks().collect();
                let next = if back {
                    hunks.iter().rev().find(|&&h| h < v.scroll).copied()
                } else {
                    hunks.iter().find(|&&h| h > v.scroll).copied()
                };
                match next {
                    Some(h) => v.scroll = h,
                    None => self.message = Some(if back { "First change" } else { "Last change" }.into()),
                }
            }
            KeyCode::F(4) | KeyCode::Enter => {
                let line = v.line_at_top();
                let path = git.root.clone().map(|r| r.join(&v.path));
                git.diff = None;
                self.dock.focused = false;
                if let Some(path) = path {
                    self.open_target(Target { path, at: Some((line.saturating_sub(1), 0, 0)) });
                }
            }
            // Quit and save still work; other editor keys would act on the hidden file.
            KeyCode::Char('q') | KeyCode::Char('s') if ctrl => {
                git.diff = None;
                return false;
            }
            _ => {}
        }
        true
    }

    pub fn dock_paste(&mut self, text: &str) -> bool {
        if self.dock.git.diff.is_some() {
            return true;
        }
        if !self.dock.focused {
            return false;
        }
        match self.dock.tab {
            Tab::Terminal => {
                if let Some(t) = &mut self.dock.term {
                    t.paste(text);
                }
            }
            Tab::Commit => {
                let p = &mut self.dock.git.panel;
                p.field = Field::Message;
                p.message.push_str(&text.replace("\r\n", "\n"));
            }
        }
        true
    }

    pub fn dock_mouse(&mut self, m: MouseEvent) -> bool {
        let inside = |r: Rect| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        let wheel = match m.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => 0,
        };
        let d = &mut self.dock;
        if let Some(v) = &mut d.git.diff {
            if inside(d.diff_rect) {
                if wheel != 0 {
                    v.scroll = (v.scroll as isize + wheel).clamp(0, v.rows.len().saturating_sub(1) as isize) as usize;
                }
                return true;
            }
        }
        if !d.open || !inside(d.rect) {
            if matches!(m.kind, MouseEventKind::Down(_)) {
                d.focused = false;
            }
            return false;
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            d.focused = true;
            self.project.focus_tree = false;
            self.agents.focus = false;
            if m.row == d.rect.y {
                if let Some(&(_, _, tab)) = d.tab_spans.iter().find(|(a, b, _)| m.column >= *a && m.column < *b) {
                    d.tab = tab;
                    if tab == Tab::Terminal {
                        if let Some(e) = d.ensure_term() {
                            self.message = Some(e);
                        }
                    } else {
                        d.git.refresh();
                    }
                }
                return true;
            }
        }
        match d.tab {
            Tab::Terminal => {
                if let Some(t) = &mut d.term {
                    if wheel != 0 {
                        t.scroll(-wheel);
                    }
                }
            }
            Tab::Commit => {
                let (l, msg) = (d.list_rect, d.msg_rect);
                let git = &mut d.git;
                let len = git.entries.len();
                if wheel != 0 && len > 0 {
                    git.panel.cursor = (git.panel.cursor as isize + wheel).clamp(0, len as isize - 1) as usize;
                }
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    if inside(msg) {
                        git.panel.field = Field::Message;
                    } else if inside(l) && m.row > l.y {
                        git.panel.field = Field::List;
                        let i = git.panel.scroll + (m.row - l.y - 1) as usize;
                        if i < len {
                            git.panel.cursor = i;
                            // The [x] box toggles.
                            if m.column < l.x + 5 {
                                git.toggle(i);
                            }
                        }
                    }
                }
            }
        }
        true
    }
}

fn pop_word(s: &mut String) {
    while s.ends_with(|c: char| !c.is_alphanumeric() && c != '\n') {
        s.pop();
    }
    while s.ends_with(char::is_alphanumeric) {
        s.pop();
    }
}

/// Draws the dock into `area` (from [`split`]); returns the cursor when the dock has the focus.
pub fn draw(buf: &mut Buffer, app: &mut App, area: Rect) -> Option<Position> {
    let d = &mut app.dock;
    d.rect = area;
    // Tab row: doubles as the divider from the editor.
    let head = Rect::new(area.x, area.y, area.width, 1);
    buf.set_style(head, Style::new().bg(SURFACE_1).fg(TEXT_2));
    d.tab_spans.clear();
    let mut x = area.x;
    for tab in [Tab::Terminal, Tab::Commit] {
        let mut title = format!(" {} ", tab.title());
        if tab == Tab::Commit && !d.git.entries.is_empty() {
            title = format!(" {} {} ", tab.title(), d.git.entries.len());
        }
        let w = title.chars().count() as u16;
        if x + w >= area.right() {
            break;
        }
        let active = tab == d.tab;
        let style = match (active, d.focused) {
            (true, true) => Style::new().fg(PAPER).bg(ACCENT).add_modifier(Modifier::BOLD),
            (true, false) => Style::new().fg(TEXT).bg(SURFACE_2).add_modifier(Modifier::BOLD),
            _ => Style::new().fg(TEXT_2).bg(SURFACE_1),
        };
        buf.set_string(x, area.y, &title, style);
        d.tab_spans.push((x, x + w, tab));
        x += w + 1;
    }
    let hint = match d.tab {
        Tab::Terminal => " Ctrl+Alt+↑ editor · Alt+F12 hide ",
        Tab::Commit => " Space tick · Enter diff · Tab message · Ctrl+Enter commit ",
    };
    let hw = hint.chars().count() as u16;
    if x + hw + 2 < area.right() {
        buf.set_string(area.right() - hw, area.y, hint, Style::new().fg(MUTED).bg(SURFACE_1));
    }

    let body = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    d.body = body;
    match d.tab {
        Tab::Terminal => match &mut d.term {
            Some(t) => {
                t.resize(body.height, body.width);
                crate::term::ui::draw(buf, t, body, d.focused)
            }
            None => {
                buf.set_style(body, Style::new().bg(PAPER));
                buf.set_stringn(body.x + 1, body.y, "No shell running: Enter starts one", body.width.saturating_sub(2) as usize, Style::new().fg(MUTED));
                None
            }
        },
        Tab::Commit => {
            let wide = body.width >= 70;
            let (l, m) = if wide {
                let lw = body.width * 3 / 5;
                (Rect::new(body.x, body.y, lw, body.height), Rect::new(body.x + lw + 1, body.y, body.width - lw - 1, body.height))
            } else {
                let mh = 3.min(body.height / 2);
                (Rect::new(body.x, body.y, body.width, body.height - mh), Rect::new(body.x, body.bottom() - mh, body.width, mh))
            };
            d.list_rect = l;
            d.msg_rect = m;
            crate::git::ui::draw_panel(buf, &mut d.git, body, d.focused)
        }
    }
}

/// Draws what the dock puts over the editor (the diff view).
pub fn draw_over_editor(buf: &mut Buffer, app: &mut App, area: Rect) {
    app.dock.diff_rect = if app.dock.git.diff.is_some() { area } else { Rect::default() };
    crate::git::ui::draw_diff(buf, &mut app.dock.git, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_gives_the_dock_its_rows() {
        let area = Rect::new(0, 1, 100, 30);
        let (ed, dock) = split_rows(area, DEFAULT_H);
        let dock = dock.unwrap();
        assert_eq!((ed.y, ed.height), (1, 18));
        assert_eq!((dock.y, dock.height), (19, 12));
        assert_eq!(ed.height + dock.height, area.height);
    }

    #[test]
    fn split_keeps_the_editor_usable() {
        let (ed, dock) = split_rows(Rect::new(0, 0, 80, 10), 40);
        assert_eq!((ed.height, dock.unwrap().height), (MIN_EDITOR_H, 7));
        let (ed, dock) = split_rows(Rect::new(0, 0, 80, 10), 1);
        assert_eq!(dock.unwrap().height, MIN_H);
        assert_eq!(ed.height, 6);
        // Too small for both: no dock.
        let (ed, dock) = split_rows(Rect::new(0, 0, 80, 6), 12);
        assert!(dock.is_none());
        assert_eq!(ed.height, 6);
    }

    #[test]
    fn resize_is_clamped() {
        let mut d = Dock::new(&std::env::temp_dir());
        d.resize_by(-100);
        assert_eq!(d.height, MIN_H);
        d.resize_by(100);
        assert_eq!(d.height, 60);
    }
}
