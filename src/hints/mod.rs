//! Shortcut helper: a small see-through panel in the bottom-right corner of the editor area with
//! the most useful keys for what has the focus (decision 0008). Alt+/ toggles it (saved in
//! `settings.toml`, `src/config.rs`); `catlas --no-hints` starts without it.
//!
//! - `table.rs`: the rows per context, as data (the single source of truth; tested against the
//!   real key handlers in `tests.rs`).
//! - `ui.rs`: placement and the "glass" drawing (dims the cells under it, no background fill).
//! - Key Promoter tips: a mouse action with a key, or a fallback key where the real one works,
//!   shows that key at the top of the panel for 3 s.
//!
//! Hooks elsewhere: `App.hints`, `hints_key` first in `App::handle` (keys), `hints_mouse` after
//! the mouse handlers, `ui::draw` calls `hints::ui::draw` after the editor and before popups,
//! `main.rs` sets `kitty` (whether the kitty keyboard protocol is on) and reads `--no-hints`.

pub mod table;
pub mod ui;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::App;
use crate::config::Settings;
use table::{Ctx, Hint};

/// How long a Key Promoter tip stays, and how soon the same tip may come back.
pub const TIP_FOR: Duration = Duration::from_secs(3);
pub const TIP_AGAIN: Duration = Duration::from_secs(30);

pub struct Hints {
    pub on: bool,
    /// The kitty keyboard protocol is on (Ctrl+Shift+letter etc. arrive): show the real keys.
    pub kitty: bool,
    /// The settings file the toggle saves to (`None` = don't save: `--no-hints`, tests).
    pub store: Option<PathBuf>,
    pub tip: Option<(Hint, Instant)>,
    tipped: HashMap<&'static str, Instant>,
    /// Where the panel was drawn last (empty when hidden).
    pub rect: Rect,
}

impl Hints {
    /// The saved choice (on by default; tests never read or write the user's file).
    pub fn load() -> Hints {
        let store = if cfg!(test) { None } else { crate::config::settings_path() };
        let on = Settings::load(store.as_deref()).hints;
        Hints { on, kitty: false, store, tip: None, tipped: HashMap::new(), rect: Rect::default() }
    }

    /// Alt+/: flips the helper and saves the choice. Returns the status message.
    pub fn toggle(&mut self) -> String {
        self.on = !self.on;
        self.tip = None;
        let saved = match &self.store {
            Some(p) => Settings::save_hints(p, self.on).err().map(|e| format!(" (couldn't save: {e})")),
            None => None,
        };
        let msg = if self.on { "Shortcut hints on · Alt+/ hides them" } else { "Shortcut hints off · Alt+/ shows them" };
        format!("{msg}{}", saved.unwrap_or_default())
    }

    /// Shows `h` as a tip (unless the panel is off or the same tip was shown recently).
    pub fn promote(&mut self, h: Hint, now: Instant) {
        if !self.on {
            return;
        }
        let key = h.key;
        if self.tipped.get(key).is_some_and(|t| now.duration_since(*t) < TIP_AGAIN) {
            return;
        }
        self.tipped.insert(key, now);
        self.tip = Some((h, now));
    }

    pub fn current_tip(&self, now: Instant) -> Option<Hint> {
        self.tip.filter(|(_, t)| now.duration_since(*t) < TIP_FOR).map(|(h, _)| h)
    }
}

/// What has the focus, in the order `App::handle` hands keys around.
pub fn context(app: &App) -> Ctx {
    use crate::agents::Area;
    use crate::lsp::popup::Popup;
    use crate::project::PopupKind;
    use crate::vim::Mode;
    let dock = &app.dock;
    if dock.git.diff.is_some() {
        return Ctx::Diff;
    }
    if dock.open && dock.focused {
        return match dock.tab {
            crate::dock::Tab::Terminal => Ctx::Terminal,
            crate::dock::Tab::Commit if dock.git.panel.field == crate::git::Field::Message => Ctx::CommitMessage,
            crate::dock::Tab::Commit => Ctx::Commit,
        };
    }
    if app.prompt.is_some() {
        return Ctx::Prompt;
    }
    if let Some(p) = &app.project.popup {
        return match p.kind {
            PopupKind::Files => Ctx::GotoFile,
            PopupKind::Recent => Ctx::Recent,
            PopupKind::Search => Ctx::FindInFiles,
        };
    }
    if app.agents.visible && app.agents.focus {
        return match app.agents.area {
            Area::Prompt => Ctx::AgentsPrompt,
            Area::Runs => Ctx::AgentsRuns,
            Area::Transcript => Ctx::AgentsTranscript,
        };
    }
    if app.project.focus_tree && app.project.sidebar {
        return Ctx::Tree;
    }
    match &app.lsp.popup {
        Some(Popup::Completion { .. }) => return Ctx::Completion,
        Some(Popup::Picker(_)) | Some(Popup::Actions { .. }) => return Ctx::List,
        _ => {}
    }
    if let Some(v) = &app.vim {
        if v.cmdline().is_some() {
            return Ctx::VimCommand;
        }
        return match v.mode {
            Mode::Normal => Ctx::VimNormal,
            Mode::Insert => Ctx::VimInsert,
            Mode::Visual | Mode::VisualLine => Ctx::VimVisual,
        };
    }
    if app.doc().selection().is_some() {
        return Ctx::Selection;
    }
    Ctx::Idea
}

/// The panel title: the context, with the Vim mode or the prompt's name where it helps.
pub fn title(app: &App, c: Ctx) -> String {
    match c {
        Ctx::VimVisual => format!("vim · {}", app.vim.as_ref().map(|v| v.mode.label()).unwrap_or("VISUAL")),
        Ctx::Prompt => match &app.prompt {
            Some(p) => format!("prompt · {}", p.kind.label().split('[').next().unwrap_or("").trim()),
            None => table::title(c).into(),
        },
        _ => table::title(c).into(),
    }
}

// ---- key labels → key events (the table's syntax, see table.rs) ------------------------------

fn ev(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE }
}

fn named(name: &str) -> Option<KeyCode> {
    Some(match name {
        "↑" => KeyCode::Up,
        "↓" => KeyCode::Down,
        "←" => KeyCode::Left,
        "→" => KeyCode::Right,
        "PgUp" => KeyCode::PageUp,
        "PgDn" => KeyCode::PageDown,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Tab" => KeyCode::Tab,
        "Space" => KeyCode::Char(' '),
        "Del" => KeyCode::Delete,
        "Backspace" => KeyCode::Backspace,
        _ => {
            let n: u8 = name.strip_prefix('F')?.parse().ok()?;
            return (1..=12).contains(&n).then_some(KeyCode::F(n));
        }
    })
}

/// One key with modifiers, the way crossterm reports it (Shift+letter = upper case + SHIFT,
/// Shift+Tab = BackTab).
fn chord(mods: KeyModifiers, key: &str) -> Option<KeyEvent> {
    let mut code = named(key).or_else(|| {
        let mut cs = key.chars();
        let c = cs.next()?;
        cs.next().is_none().then_some(KeyCode::Char(c))
    })?;
    let mut mods = mods;
    if let KeyCode::Char(c) = code {
        if c.is_ascii_uppercase() && !mods.contains(KeyModifiers::SHIFT) && mods.is_empty() {
            mods |= KeyModifiers::SHIFT; // a bare "V" (Vim) is Shift+v
        }
        // Labels write letters in capitals ("Alt+Z"); the key itself is a lower-case letter
        // unless Shift is held.
        if c.is_ascii_alphabetic() && !mods.is_empty() {
            let shifted = mods.contains(KeyModifiers::SHIFT);
            code = KeyCode::Char(if shifted { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() });
        }
    }
    if code == KeyCode::Tab && mods.contains(KeyModifiers::SHIFT) {
        code = KeyCode::BackTab;
    }
    Some(ev(code, mods))
}

/// Splits "Ctrl+Alt+↑" into modifiers and the key; `None` when there are no modifiers.
fn split_mods(s: &str) -> Option<(KeyModifiers, &str)> {
    let mut mods = KeyModifiers::NONE;
    let mut rest = s;
    loop {
        let (m, r) = if let Some(r) = rest.strip_prefix("Ctrl+") {
            (KeyModifiers::CONTROL, r)
        } else if let Some(r) = rest.strip_prefix("Alt+") {
            (KeyModifiers::ALT, r)
        } else if let Some(r) = rest.strip_prefix("Shift+") {
            (KeyModifiers::SHIFT, r)
        } else {
            break;
        };
        if r.is_empty() {
            break; // "Ctrl++" style: the '+' is the key (not used yet)
        }
        mods |= m;
        rest = r;
    }
    (!mods.is_empty()).then_some((mods, rest))
}

/// A table label → its alternatives, each a sequence of key presses. `None` = a label the
/// syntax doesn't cover (the table test fails on it).
pub fn parse(label: &str) -> Option<Vec<Vec<KeyEvent>>> {
    // Split on '/' between keys: not the '/' key itself ("Ctrl+/", a lone "/").
    let chars: Vec<char> = label.chars().collect();
    let mut parts = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let is_sep = c == '/' && !cur.is_empty() && !cur.ends_with('+') && i + 1 < chars.len();
        if is_sep {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    let mut out = Vec::new();
    let mut first_mods = None;
    for (i, p) in parts.iter().enumerate() {
        if let Some((mods, key)) = split_mods(p) {
            if i == 0 {
                first_mods = Some(mods);
            }
            out.push(vec![chord(mods, key)?]);
        } else if let (Some(mods), true) = (first_mods.filter(|_| i > 0), named(p).is_some() || p.chars().count() == 1) {
            out.push(vec![chord(mods, p)?]);
        } else if named(p).is_some() {
            out.push(vec![chord(KeyModifiers::NONE, p)?]);
        } else if !p.is_empty() {
            out.push(p.chars().map(|c| chord(KeyModifiers::NONE, &c.to_string())).collect::<Option<Vec<_>>>()?);
        } else {
            return None;
        }
    }
    Some(out)
}

/// Whether `k` is one of the label's keys (single presses only).
pub fn matches(label: &str, k: &KeyEvent) -> bool {
    let norm = |e: &KeyEvent| (e.code, e.modifiers);
    parse(label).is_some_and(|alts| alts.iter().any(|seq| seq.len() == 1 && norm(&seq[0]) == norm(k)))
}

/// The helper's toggle: Alt+/ (also Alt+? and Alt+Shift+7, where '/' sits on a German keyboard).
pub fn is_toggle(k: &KeyEvent) -> bool {
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    alt && !ctrl && (matches!(k.code, KeyCode::Char('/') | KeyCode::Char('?')) || (shift && k.code == KeyCode::Char('7')))
}

// ---- glue: the helper's side of `App` --------------------------------------------------------

/// What a mouse handler changed, for the Key Promoter.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Before {
    active: usize,
    docs: usize,
    tree: bool,
    agents: bool,
    dock: (bool, bool, crate::dock::Tab),
}

impl App {
    /// Alt+/ toggles the helper (in every context, the terminal too). Other keys only feed the
    /// Key Promoter. Returns true when the key was the toggle.
    pub fn hints_key(&mut self, k: KeyEvent) -> bool {
        if is_toggle(&k) {
            self.message = Some(self.hints.toggle());
            return true;
        }
        if self.hints.kitty && self.hints.on {
            let ctx = context(self);
            let editorish = matches!(
                ctx,
                Ctx::Idea | Ctx::Selection | Ctx::VimNormal | Ctx::VimInsert | Ctx::VimVisual | Ctx::Tree | Ctx::GotoFile | Ctx::Recent | Ctx::FindInFiles
            );
            if editorish {
                if let Some(h) = table::FALLBACKS.iter().find(|h| h.legacy.is_some_and(|l| matches(l, &k))) {
                    self.hints.promote(*h, Instant::now());
                }
            }
        }
        false
    }

    pub fn hints_before(&self) -> Before {
        Before {
            active: self.active,
            docs: self.docs.len(),
            tree: self.project.focus_tree,
            agents: self.agents.focus,
            dock: (self.dock.open, self.dock.focused, self.dock.tab),
        }
    }

    /// After the mouse handlers: a click or scroll that has a key shows it as a tip.
    pub fn hints_mouse(&mut self, m: MouseEvent, b: Before) {
        let inside = |r: Rect| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        let opened = self.active != b.active || self.docs.len() != b.docs;
        let tip = match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let dock = (self.dock.open, self.dock.focused, self.dock.tab);
                if m.row == 0 && opened {
                    Some(table::TAB_SWITCH)
                } else if self.project.sidebar && inside(self.project.sidebar_rect) {
                    if opened {
                        Some(table::TREE_OPEN)
                    } else {
                        (!b.tree && self.project.focus_tree).then_some(table::TREE_FOCUS)
                    }
                } else if self.dock.open && inside(self.dock.rect) && dock != b.dock && self.dock.focused {
                    Some(if self.dock.tab == crate::dock::Tab::Terminal { table::DOCK_TERMINAL } else { table::DOCK_COMMIT })
                } else if self.agents.visible && inside(self.agents.rect) && !b.agents && self.agents.focus {
                    Some(table::AGENTS_FOCUS)
                } else {
                    None
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let l = self.layout;
                let in_text = m.column >= l.text_x && m.row >= l.text_y && m.row < l.text_y + l.text_h;
                let elsewhere = (self.project.sidebar && inside(self.project.sidebar_rect))
                    || (self.dock.open && inside(self.dock.rect))
                    || (self.agents.visible && inside(self.agents.rect))
                    || self.project.popup.is_some()
                    || self.dock.git.diff.is_some();
                (in_text && !elsewhere).then_some(table::SCROLL)
            }
            _ => None,
        };
        if let Some(h) = tip {
            self.hints.promote(h, Instant::now());
        }
    }
}
