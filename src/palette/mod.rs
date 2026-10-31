//! Find Action (decision 0016, roadmap M7): IDEA's Ctrl+Shift+A. A popup listing every action of
//! the editor ([`actions`]), searched with the project's fuzzy scorer, recently used first, each
//! with its key; Enter runs it exactly as its key would. Terminals without the kitty protocol
//! (macOS Terminal.app) can't send Ctrl+Shift+A (it arrives as Ctrl+A, select all), so
//! **Ctrl+R** opens it too (IDEA's Replace isn't built here; Ctrl+O is the Repos popup, decision
//! 0013), everywhere except the terminal pane (the shell's history search) and Vim normal / visual
//! mode in the editor (redo there).
//!
//! Hooks elsewhere: `App.palette`, `control_key` in `App::handle` (src/control/mod.rs hands keys
//! here first), `control::ui::draw` draws it, `hints` context `Palette`.

pub mod actions;
pub mod ui;
#[cfg(test)]
pub mod tests;

use std::path::PathBuf;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::App;
use actions::{Action, Run};

/// Recent actions kept (in the config folder's `recent-actions`, one id per line).
const RECENT_MAX: usize = 30;

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub input: String,
    pub selected: usize,
    pub scroll: usize,
    /// Action ids, most recent first.
    pub recent: Vec<String>,
    store: Option<PathBuf>,
    /// Screen areas from the last draw (mouse).
    pub rect: Rect,
    pub list_rect: Rect,
}

/// One row of the list: the action and the matched character positions in its name.
pub struct Hit {
    pub action: &'static Action,
    pub pos: Vec<usize>,
}

impl Palette {
    /// The saved recent list (tests never read or write the user's file).
    pub fn load() -> Palette {
        let store = if cfg!(test) { None } else { crate::lsp::config::config_dir().map(|d| d.join("recent-actions")) };
        let recent = store
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).take(RECENT_MAX).collect())
            .unwrap_or_default();
        Palette { recent, store, ..Palette::default() }
    }

    pub fn show(&mut self) {
        self.open = true;
        self.input.clear();
        self.selected = 0;
        self.scroll = 0;
    }

    /// Remembers `id` as the most recent action (and saves the list).
    pub fn note(&mut self, id: &str) {
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(RECENT_MAX);
        if let Some(p) = &self.store {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, self.recent.join("\n") + "\n");
        }
    }

    /// The list for the current input: without one, recent actions first, then all in registry
    /// order; with one, by fuzzy score (name first, then group and alias words), recent ones a
    /// little higher.
    pub fn hits(&self) -> Vec<Hit> {
        rank(&self.input, &self.recent)
    }
}

pub fn rank(query: &str, recent: &[String]) -> Vec<Hit> {
    use crate::project::fuzzy::score;
    let all = actions::all();
    let recency = |a: &Action| recent.iter().position(|r| r == a.id);
    if query.trim().is_empty() {
        let mut out: Vec<Hit> = recent.iter().filter_map(|id| all.iter().find(|a| a.id == id)).map(|a| Hit { action: a, pos: Vec::new() }).collect();
        for a in &all {
            if recency(a).is_none() {
                out.push(Hit { action: a, pos: Vec::new() });
            }
        }
        return out;
    }
    let mut scored: Vec<(i32, usize, Hit)> = Vec::new();
    for (i, a) in all.iter().enumerate() {
        let words = format!("{} {} {} {}", a.name, a.group, a.alias, a.key.unwrap_or(""));
        let one = |q: &str| {
            let by_name = score(q, a.name).map(|(s, pos)| (s + 20, pos));
            let by_words = score(q, &words).map(|(s, _)| (s, Vec::new()));
            by_name.into_iter().chain(by_words).max_by_key(|(s, _)| *s)
        };
        // The whole query, or each word on its own (any order: "light theme").
        let parts: Vec<&str> = query.split_whitespace().collect();
        let split = (parts.len() > 1).then(|| parts.iter().map(|p| one(p)).collect::<Option<Vec<_>>>()).flatten().map(|v| {
            let mut pos: Vec<usize> = v.iter().flat_map(|(_, p)| p.clone()).collect();
            pos.sort_unstable();
            pos.dedup();
            (v.iter().map(|(s, _)| *s).sum::<i32>() / parts.len() as i32, pos)
        });
        let Some((s, pos)) = one(query).into_iter().chain(split).max_by_key(|(s, _)| *s) else { continue };
        let bonus = recency(a).map_or(0, |r| 24 - 2 * r.min(12) as i32);
        scored.push((s + bonus, i, Hit { action: a, pos }));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, h)| h).collect()
}

/// Whether `k` opens Find Action: Ctrl+Shift+A (kitty protocol) or Ctrl+R (every terminal).
pub fn is_opener(k: &KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    ctrl && !alt && (matches!(k.code, KeyCode::Char('r') | KeyCode::Char('R')) || (shift && matches!(k.code, KeyCode::Char('A') | KeyCode::Char('a'))))
}

impl App {
    pub fn open_palette(&mut self) {
        self.project.close_popup();
        self.palette.show();
        self.message = Some("Find Action · type to search · Enter runs · Esc closes".into());
    }

    /// Keys while Find Action is open (it takes them all).
    pub fn palette_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let n = self.palette.hits().len();
        let p = &mut self.palette;
        let page = p.list_rect.height.max(2) as usize - 1;
        match k.code {
            _ if is_opener(&k) => p.open = false,
            KeyCode::Esc => p.open = false,
            KeyCode::Enter => {
                let hits = self.palette.hits();
                match hits.get(self.palette.selected) {
                    Some(h) => {
                        let a = h.action;
                        self.run_palette_action(a);
                    }
                    None => self.message = Some("No action matches · Backspace / Ctrl+U to search again".into()),
                }
                return;
            }
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down => p.selected = (p.selected + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => p.selected = p.selected.saturating_sub(page),
            KeyCode::PageDown => p.selected = (p.selected + page).min(n.saturating_sub(1)),
            KeyCode::Home if ctrl => p.selected = 0,
            KeyCode::End if ctrl => p.selected = n.saturating_sub(1),
            KeyCode::Char('o') if alt && !ctrl => {
                p.open = false;
                self.open_control(crate::control::Page::Overview);
                return;
            }
            KeyCode::Char('u') if ctrl => {
                p.input.clear();
                p.selected = 0;
            }
            KeyCode::Backspace if ctrl || alt => {
                delete_word(&mut p.input);
                p.selected = 0;
            }
            KeyCode::Char('h') if ctrl => {
                delete_word(&mut p.input);
                p.selected = 0;
            }
            KeyCode::Backspace => {
                p.input.pop();
                p.selected = 0;
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                p.input.push(c);
                p.selected = 0;
            }
            _ => {}
        }
        if !self.palette.open && self.message.as_deref().is_some_and(|m| m.starts_with("Find Action")) {
            self.message = None;
        }
    }

    pub fn palette_mouse(&mut self, m: MouseEvent) {
        let p = &mut self.palette;
        let r = p.list_rect;
        let inside = m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        match m.kind {
            MouseEventKind::ScrollDown => p.selected += 1,
            MouseEventKind::ScrollUp => p.selected = p.selected.saturating_sub(1),
            MouseEventKind::Down(MouseButton::Left) if inside => {
                let i = p.scroll + (m.row - r.y) as usize;
                if let Some(h) = self.palette.hits().get(i) {
                    let a = h.action;
                    self.run_palette_action(a);
                }
                return;
            }
            MouseEventKind::Down(_) => {
                let pr = p.rect;
                if !(m.column >= pr.x && m.column < pr.right() && m.row >= pr.y && m.row < pr.bottom()) {
                    p.open = false;
                }
            }
            _ => {}
        }
        let n = self.palette.hits().len();
        self.palette.selected = self.palette.selected.min(n.saturating_sub(1));
    }

    /// Runs an action as its key would (closing the palette and the Control Center first, so the
    /// key reaches what had the focus before), and remembers it as recent.
    pub fn run_palette_action(&mut self, a: &'static Action) {
        self.palette.open = false;
        self.palette.note(a.id);
        self.message = None;
        match a.run {
            Run::Keys(labels) => {
                self.control.open = false;
                self.press(labels);
            }
            Run::Editor(labels) => {
                self.control.open = false;
                self.focus_editor();
                self.press(labels);
            }
            Run::Do(f) => f(self),
        }
    }

    /// Presses each label's first key sequence (hint-table syntax) through `App::handle`.
    pub fn press(&mut self, labels: &[&str]) {
        for l in labels {
            let Some(seq) = crate::hints::parse(l).and_then(|alts| alts.into_iter().next()) else { continue };
            for e in seq {
                self.handle(Event::Key(e));
            }
        }
    }

    /// Gives the editor the keys: the tree, panels, dock, Git view and popups let go.
    pub fn focus_editor(&mut self) {
        self.project.close_popup();
        self.project.focus_tree = false;
        self.agents.focus = false;
        self.dock.focused = false;
        if self.git_view.open {
            self.close_git_view();
        }
        self.lsp.popup = None;
        self.prompt = None;
    }
}

fn delete_word(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
    while s.chars().last().is_some_and(|c| c != ' ') {
        s.pop();
    }
}
