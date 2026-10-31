//! Control Center (decision 0016): one full-screen place to see and steer the whole editor.
//! Tiles for the workspace (repos, branch, ahead / behind), agents (running, cost today, what
//! needs attention, Aa staff), language servers (running / crashed / missing, restart), the
//! index, the editor (problems, unsaved files, memory / CPU of catlas and its children) and
//! settings (toggles that change live and are saved to `settings.toml`), plus a settings screen.
//! Opens with **Alt+O** (Option+O; `ø` where Option isn't Meta), from Find Action, or the
//! Control Center action; Esc closes it.
//!
//! - `tiles.rs` what the tiles show (rows, tones, actions) · `ui.rs` layout and drawing ·
//!   `widgets.rs` reusable pieces · `theme.rs` the palette registry · `settings.rs` the schema ·
//!   `procs.rs` / `repos.rs` background samples · `hooks.rs` where tracks A/B/C plug in.
//!
//! Hooks elsewhere: `App.control` / `App.palette`, `control_key` first in `App::handle` (after the
//! helper's Alt+/), `control_paste` / `control_mouse` first too, `control::ui::draw` + `theme::apply`
//! at the end of `ui::draw`, `app.control_tick()` in the main loop, `vim` from the settings in
//! `main.rs`, `theme::syntax_fg` in `ui::theme::token_style`.

pub mod hooks;
pub mod procs;
pub mod repos;
pub mod settings;
pub mod theme;
pub mod tiles;
pub mod ui;
pub mod widgets;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::App;
use crate::config::Settings;
use crate::palette::actions::{act_do, with_alias, with_key, with_state, Action};
use theme::Theme;
use tiles::{Act, TileId};

/// How often the background samples refresh while the Control Center is open.
const REPOS_EVERY: Duration = Duration::from_secs(3);
const PROCS_EVERY: Duration = Duration::from_secs(2);
const PROBE_EVERY: Duration = Duration::from_secs(20);
/// How often `settings.toml` is looked at for edits made elsewhere (one `stat`).
const FILE_EVERY: Duration = Duration::from_secs(2);
/// CPU samples kept for the sparkline.
const CPU_KEEP: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Overview,
    Settings,
}

pub struct Control {
    pub open: bool,
    pub page: Page,
    /// Selected tile (index into `tiles::ORDER`) and the selected row in each.
    pub tile: usize,
    pub rows: [usize; 6],
    /// Selected row on the settings screen.
    pub setting: usize,
    pub settings: Settings,
    /// `settings.toml` (None in tests: nothing is written).
    pub store: Option<PathBuf>,
    /// Problems in the file (`settings::check`).
    pub file_problems: Vec<String>,
    pub file_exists: bool,
    /// The file as last read (the settings screen previews it).
    pub file_text: String,
    mtime: Option<SystemTime>,
    checked: Option<Instant>,
    pub repos: Vec<repos::Repo>,
    repos_job: Option<Receiver<Vec<repos::Repo>>>,
    repos_at: Option<Instant>,
    pub installed: Option<HashMap<String, bool>>,
    probe_job: Option<Receiver<HashMap<String, bool>>>,
    probe_at: Option<Instant>,
    pub usage: Option<procs::Usage>,
    pub procs: Vec<procs::Proc>,
    pub cpu: Vec<f32>,
    pub utc_offset: i64,
    procs_job: Option<Receiver<procs::Sample>>,
    procs_at: Option<Instant>,
    /// Screen areas from the last draw (mouse): tiles, and each drawn row (tile, row, area).
    pub tile_rects: Vec<Rect>,
    pub row_rects: Vec<(usize, usize, Rect)>,
    pub setting_rects: Vec<(usize, Rect)>,
}

impl Control {
    pub fn new(settings: Settings) -> Control {
        let store = if cfg!(test) { None } else { crate::config::settings_path() };
        theme::set_active(settings.theme);
        let mut c = Control {
            open: false,
            page: Page::Overview,
            tile: 0,
            rows: [0; 6],
            setting: 0,
            settings,
            store,
            file_problems: Vec::new(),
            file_exists: false,
            file_text: String::new(),
            mtime: None,
            checked: None,
            repos: Vec::new(),
            repos_job: None,
            repos_at: None,
            installed: None,
            probe_job: None,
            probe_at: None,
            usage: None,
            procs: Vec::new(),
            cpu: Vec::new(),
            utc_offset: 0,
            procs_job: None,
            procs_at: None,
            tile_rects: Vec::new(),
            row_rects: Vec::new(),
            setting_rects: Vec::new(),
        };
        c.read_file_state();
        c
    }

    /// Looks at the settings file: problems, whether it exists, its mtime. Returns the text.
    fn read_file_state(&mut self) -> Option<String> {
        let p = self.store.as_ref()?;
        self.mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let text = std::fs::read_to_string(p).ok();
        self.file_exists = text.is_some();
        self.file_problems = text.as_deref().map(settings::check).unwrap_or_default();
        self.file_text = text.clone().unwrap_or_default();
        text
    }

    /// Resident memory of one process from the last sample.
    pub fn proc_kb(&self, pid: u32) -> Option<u64> {
        self.procs.iter().find(|p| p.pid == pid).map(|p| p.rss_kb)
    }
}

// ---- actions this module registers (Find Action) -------------------------------------------

fn on_off(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}

pub const ACTIONS: &[Action] = &[
    with_alias(with_key(act_do("control.open", "View", "Control Center", |a| a.open_control(Page::Overview)), "Alt+O"), "dashboard overview status mission"),
    with_alias(act_do("control.settings", "Settings", "Settings", |a| a.open_control(Page::Settings)), "preferences options config"),
    with_state(with_alias(with_key(act_do("settings.vim", "Settings", "Vim mode", |a| a.toggle_setting("vim")), "Ctrl+Alt+V"), "idea keys toggle"), |a| on_off(a.vim.is_some())),
    with_state(with_alias(with_key(act_do("settings.hints", "Settings", "Shortcut helper", |a| a.toggle_setting("hints")), "Alt+/"), "hints keys toggle"), |a| on_off(a.hints.on)),
    with_state(with_alias(act_do("settings.autocomplete", "Settings", "Autocomplete as you type", |a| a.toggle_setting("autocomplete")), "completion toggle"), |a| on_off(a.lsp.auto)),
    with_state(with_alias(act_do("theme.next", "Settings", "Next theme", |a| a.set_theme(a.control.settings.theme.next())), "colours colors appearance"), |a| a.control.settings.theme.key().to_string()),
    with_alias(act_do("theme.dark", "Settings", "Theme: Ledger Terminal dark", |a| a.set_theme_key("ledger-dark")), "colours colors"),
    with_alias(act_do("theme.light", "Settings", "Theme: Ledger Terminal light", |a| a.set_theme_key("ledger-light")), "colours colors bright day"),
    with_alias(act_do("theme.contrast", "Settings", "Theme: High contrast", |a| a.set_theme_key("high-contrast")), "colours colors accessibility"),
    with_alias(act_do("settings.open-file", "Settings", "Open settings.toml", |a| a.open_settings_file()), "edit config"),
    with_alias(act_do("settings.open-languages", "Settings", "Open languages.toml", |a| a.open_languages_file()), "language servers lsp config"),
    with_alias(act_do("lsp.status", "Code", "Language servers", |a| a.open_control_at(TileId::Servers)), "lsp health status"),
    with_alias(act_do("lsp.restart", "Code", "Restart language server", |a| a.restart_current_server()), "lsp reload"),
    with_alias(act_do("editor.processes", "View", "Memory and CPU", |a| a.open_control_at(TileId::Editor)), "processes ram usage"),
];

// ---- glue: the Control Center's side of `App` ----------------------------------------------

/// Alt+O, or `ø` (what Option+O types on a Mac without "Option as Meta").
pub fn is_opener(k: &KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    alt && !ctrl && matches!(k.code, KeyCode::Char('o'))
}

fn is_oslash(k: &KeyEvent) -> bool {
    k.code == KeyCode::Char('ø') && !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

impl App {
    /// Find Action and the Control Center first: when open they take every key; otherwise their
    /// openers (not in the terminal pane, whose keys are the shell's). false = not ours.
    pub fn control_key(&mut self, k: KeyEvent) -> bool {
        if self.palette.open {
            self.palette_key(k);
            return true;
        }
        if self.control.open {
            return self.center_key(k);
        }
        let in_terminal = self.dock.open && self.dock.focused && self.dock.tab == crate::dock::Tab::Terminal;
        if in_terminal {
            return false;
        }
        // Vim normal / visual mode keeps Ctrl+R as redo.
        let vim_redo = self.vim.as_ref().is_some_and(|v| v.mode != crate::vim::Mode::Insert) && self.editor_has_keys();
        if crate::palette::is_opener(&k) && !(vim_redo && !k.modifiers.contains(KeyModifiers::SHIFT)) {
            self.open_palette();
            return true;
        }
        if is_opener(&k) || (is_oslash(&k) && self.editor_has_keys()) {
            self.open_control(Page::Overview);
            return true;
        }
        false
    }

    /// The editor text has the keys (no prompt, popup, panel, dock or Git view): where `ø`
    /// opens the Control Center.
    fn editor_has_keys(&self) -> bool {
        self.prompt.is_none() && self.project.popup.is_none() && !self.project.focus_tree && !(self.agents.visible && self.agents.focus) && !self.dock.has_focus() && !self.git_view.open && self.lsp.popup.is_none()
    }

    pub fn open_control(&mut self, page: Page) {
        self.project.close_popup();
        self.palette.open = false;
        self.control.open = true;
        self.control.page = page;
        self.control_refresh(false);
        self.message = Some(match page {
            Page::Overview => "Control Center · Tab tiles · ↑↓ rows · Enter acts · 2 settings · Ctrl+R actions · Esc closes".into(),
            Page::Settings => "Settings · ↑↓ · Enter / Space changes (saved to settings.toml) · e open the file · Esc back".into(),
        });
    }

    pub fn open_control_at(&mut self, t: TileId) {
        self.open_control(Page::Overview);
        self.control.tile = tiles::ORDER.iter().position(|x| *x == t).unwrap_or(0);
        self.control.rows[self.control.tile] = 0;
    }

    pub fn close_control(&mut self) {
        self.control.open = false;
        self.message = None;
    }

    /// Starts the background samples that are due (`force`: all of them now).
    fn control_refresh(&mut self, force: bool) {
        let due = |at: Option<Instant>, every: Duration| force || at.is_none_or(|t| t.elapsed() >= every);
        let own = hooks::roots(self).into_iter().any(|r| self.workspace.status.get(&r).is_none());
        let c = &mut self.control;
        if own && c.repos_job.is_none() && due(c.repos_at, REPOS_EVERY) {
            let roots = hooks::roots(self);
            let c = &mut self.control;
            c.repos_job = Some(crate::git::job(move || roots.iter().map(|r| repos::read(r)).collect()));
            c.repos_at = Some(Instant::now());
        }
        let c = &mut self.control;
        if c.procs_job.is_none() && due(c.procs_at, PROCS_EVERY) {
            c.procs_job = Some(crate::git::job(procs::sample));
            c.procs_at = Some(Instant::now());
        }
        if c.probe_job.is_none() && due(c.probe_at, PROBE_EVERY) {
            c.probe_job = Some(crate::lsp::status::probe());
            c.probe_at = Some(Instant::now());
        }
        // Aa staff over SSH (never from unit tests).
        if !cfg!(test) {
            if force {
                self.agents.staff.refresh();
            } else {
                self.agents.staff.maybe_refresh();
            }
        }
    }

    /// Once per loop turn: background results in, samples refreshed while open, settings.toml
    /// edits applied. Returns whether the screen needs a redraw.
    pub fn control_tick(&mut self) -> bool {
        let mut redraw = false;
        if self.control.open {
            self.control_refresh(false);
        }
        let c = &mut self.control;
        if let Some(rx) = &c.repos_job {
            if let Ok(r) = rx.try_recv() {
                c.repos = r;
                c.repos_job = None;
                redraw = true;
            }
        }
        if let Some(rx) = &c.probe_job {
            if let Ok(m) = rx.try_recv() {
                c.installed = Some(m);
                c.probe_job = None;
                redraw = true;
            }
        }
        if let Some(rx) = &c.procs_job {
            if let Ok(s) = rx.try_recv() {
                c.procs_job = None;
                c.utc_offset = s.utc_offset;
                c.procs = s.procs;
                redraw = true;
                let known = self.known_children();
                let known: Vec<(u32, &str)> = known.iter().map(|(p, l)| (*p, *l)).collect();
                let c = &mut self.control;
                let u = procs::summarize(&c.procs, std::process::id(), &known);
                c.cpu.push(u.total_cpu());
                if c.cpu.len() > CPU_KEEP {
                    c.cpu.remove(0);
                }
                c.usage = Some(u);
            }
        }
        // settings.toml changed elsewhere (edited in a tab, another catlas): apply it.
        let c = &mut self.control;
        if c.store.is_some() && c.checked.is_none_or(|t| t.elapsed() >= FILE_EVERY) {
            c.checked = Some(Instant::now());
            let m = c.store.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            if m != c.mtime {
                if let Some(text) = c.read_file_state() {
                    let new = Settings::from_text(&text);
                    self.apply_settings(new);
                    redraw = true;
                }
            }
        }
        redraw
    }

    /// (pid, label) of the processes catlas started.
    fn known_children(&self) -> Vec<(u32, &'static str)> {
        let mut v: Vec<(u32, &'static str)> = self.lsp_pids().into_iter().map(|p| (p, "language servers")).collect();
        if let Some(p) = self.dock.term.as_ref().and_then(|t| t.pid()) {
            v.push((p, "terminal"));
        }
        for r in &self.agents.runs {
            if let Some(p) = &r.proc {
                v.push((p.pid, "agents"));
            }
        }
        v
    }

    /// Makes `new` the live settings (helper, completion, Vim, theme), without saving.
    fn apply_settings(&mut self, new: Settings) {
        let old = std::mem::replace(&mut self.control.settings, new.clone());
        if new.hints != old.hints {
            self.hints.on = new.hints;
        }
        if new.autocomplete != old.autocomplete {
            self.lsp.auto = new.autocomplete;
        }
        if new.vim != old.vim && new.vim != self.vim.is_some() {
            self.set_vim(new.vim);
        }
        theme::set_active(new.theme);
    }

    /// Sets one setting live and saves it (just that line of settings.toml).
    pub fn set_setting(&mut self, key: &str, v: toml::Value) {
        let new = settings::with(self.control.settings.clone(), key, &v);
        self.apply_settings(new);
        let label = settings::field(key).map_or(key, |f| f.label);
        let saved = match &self.control.store {
            Some(p) => match crate::config::save_key(p, key, v.clone()) {
                Ok(()) => " · saved".to_string(),
                Err(e) => format!(" · couldn't save: {e}"),
            },
            None => String::new(),
        };
        self.control.read_file_state();
        self.message = Some(format!("{label}: {}{saved}", settings::shown(&v)));
    }

    /// Flips a bool setting from what is live now (Vim: whether it is on in this run).
    pub fn toggle_setting(&mut self, key: &str) {
        let now = match key {
            "vim" => self.vim.is_some(),
            "hints" => self.hints.on,
            "autocomplete" => self.lsp.auto,
            _ => settings::value(&self.control.settings, key).as_bool().unwrap_or(false),
        };
        // Keep the stored value in step with the live one first, so the change is a real flip.
        self.control.settings = settings::with(self.control.settings.clone(), key, &toml::Value::Boolean(now));
        self.set_setting(key, toml::Value::Boolean(!now));
    }

    pub fn set_theme(&mut self, t: Theme) {
        self.set_setting("theme", toml::Value::String(t.key().into()));
        self.message = Some(format!("Theme: {}{}", t.name(), self.message.as_deref().and_then(|m| m.find(" · ").map(|i| &m[i..])).unwrap_or("")));
    }

    pub fn set_theme_key(&mut self, key: &str) {
        if let Some(t) = Theme::from_key(key) {
            self.set_theme(t);
        }
    }

    /// Opens settings.toml in a tab (written from the template first when there is none).
    pub fn open_settings_file(&mut self) {
        let Some(p) = self.control.store.clone().or_else(crate::config::settings_path) else {
            self.message = Some("No config folder (HOME isn't set)".into());
            return;
        };
        if !p.exists() {
            let mut text = settings::template();
            // Keep what is live (e.g. the theme picked from the palette).
            for f in settings::FIELDS {
                text = crate::config::set_key_in(&text, f.key, &settings::value(&self.control.settings, f.key));
            }
            if cfg!(test) || std::fs::create_dir_all(p.parent().unwrap_or(&p)).and_then(|_| std::fs::write(&p, text)).is_err() {
                self.message = Some(format!("Couldn't write {}", p.display()));
                return;
            }
        }
        self.open_config_file(p);
    }

    /// Opens the user's languages.toml (created from the commented example when missing).
    pub fn open_languages_file(&mut self) {
        let Some(dir) = crate::lsp::config::config_dir() else { return };
        let p = dir.join("languages.toml");
        if !p.exists() {
            let text = include_str!("../../docs/languages.example.toml");
            if cfg!(test) || std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&p, text)).is_err() {
                self.message = Some(format!("Couldn't write {}", p.display()));
                return;
            }
        }
        self.open_config_file(p);
    }

    fn open_config_file(&mut self, p: PathBuf) {
        self.control.open = false;
        self.focus_editor();
        self.open_target(crate::project::Target { path: p.clone(), at: None });
        self.message = Some(format!("{} · saving applies it", hooks::short_path(&p)));
    }

    pub fn restart_current_server(&mut self) {
        let lang = self.doc().syntax.lang.key().to_string();
        let rows = self.lsp_rows(self.control.installed.as_ref());
        match rows.iter().find(|r| r.lang == lang).and_then(|r| r.server.clone()) {
            Some(s) => self.message = Some(self.lsp_restart(&s)),
            None => self.message = Some(format!("No language server for {}", self.doc().syntax.lang.name())),
        }
    }

    /// Saves every changed file that has a name.
    pub fn save_all(&mut self) {
        let active = self.active;
        let (mut saved, mut failed) = (0, None);
        for i in 0..self.docs.len() {
            if !self.docs[i].dirty() || self.docs[i].path.is_none() {
                continue;
            }
            match self.docs[i].save() {
                Ok(()) => {
                    saved += 1;
                    self.active = i;
                    self.lsp_did_save();
                }
                Err(e) => failed = Some(format!("Couldn't save {}: {e:#}", self.docs[i].name())),
            }
        }
        self.active = active;
        let untitled = self.docs.iter().filter(|d| d.dirty() && d.path.is_none()).count();
        self.message = Some(failed.unwrap_or_else(|| {
            let extra = if untitled > 0 { format!(" · {untitled} untitled left (Ctrl+S names them)") } else { String::new() };
            format!("Saved {saved} file(s){extra}")
        }));
    }

    /// Opens the Git view and presses one of its keys there (`1` changes, `2` log, `B`
    /// branches, `b` blame, `P` / `U` / `F` push / pull / fetch, which ask first).
    pub fn git_view_tab(&mut self, key: char) {
        self.control.open = false;
        if !self.git_view.open {
            self.open_git_view(None);
        }
        if !self.git_view.open {
            return; // not a repository: open_git_view said so
        }
        self.focus_editor_keep_view();
        let mods = if key.is_ascii_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
        self.handle(crossterm::event::Event::Key(KeyEvent::new(KeyCode::Char(key), mods)));
    }

    fn focus_editor_keep_view(&mut self) {
        self.project.close_popup();
        self.project.focus_tree = false;
        self.agents.focus = false;
        self.dock.focused = false;
    }

    pub fn agents_new(&mut self) {
        hooks::mission_control(self, None);
        self.agents.selected = None;
        self.agents.area = crate::agents::Area::Prompt;
    }

    pub fn review_last_run(&mut self) {
        self.control.open = false;
        match self.agents.shown().first().map(|&i| self.agents.runs[i].id) {
            Some(id) => self.review_run(id),
            None => self.message = Some("No agent run in this project yet · Alt+9 starts one".into()),
        }
    }

    pub fn refresh_staff(&mut self) {
        if !cfg!(test) {
            self.agents.staff.refresh();
        }
        self.message = Some("Checking the Aa staff on the laptop…".into());
    }

    // ---- keys ------------------------------------------------------------------------------

    fn center_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        if ctrl && !alt && matches!(k.code, KeyCode::Char('q')) {
            self.close_control();
            return false; // quit as usual
        }
        if crate::palette::is_opener(&k) {
            self.open_palette();
            return true;
        }
        if is_opener(&k) || is_oslash(&k) {
            self.close_control();
            return true;
        }
        match k.code {
            KeyCode::Char('1') if !ctrl && !alt => self.open_control(Page::Overview),
            KeyCode::Char('2') if !ctrl && !alt => self.open_control(Page::Settings),
            KeyCode::Char('e') if !ctrl && !alt => self.open_settings_file(),
            KeyCode::Char('l') if !ctrl && !alt => self.open_languages_file(),
            KeyCode::F(5) => {
                self.control_refresh(true);
                self.message = Some("Refreshing…".into());
            }
            _ => match self.control.page {
                Page::Overview => self.overview_key(k, ctrl, alt, shift),
                Page::Settings => self.settings_key(k, ctrl, alt),
            },
        }
        true
    }

    fn overview_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) {
        let n = tiles::ORDER.len();
        let ts = tiles::build(self);
        let c = &mut self.control;
        let sel = |_: &Control, t: usize| ts[t].selectable();
        match k.code {
            KeyCode::Esc => {
                self.close_control();
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Right | KeyCode::Left => {
                let back = matches!(k.code, KeyCode::BackTab | KeyCode::Left) || shift;
                c.tile = if back { (c.tile + n - 1) % n } else { (c.tile + 1) % n };
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
                let rows = sel(c, c.tile);
                let pos = rows.iter().position(|&r| r == c.rows[c.tile]).unwrap_or(0);
                let last = rows.len().saturating_sub(1);
                match k.code {
                    KeyCode::Up if pos == 0 => {
                        c.tile = (c.tile + n - 1) % n;
                        c.rows[c.tile] = sel(c, c.tile).last().copied().unwrap_or(0);
                    }
                    KeyCode::Down if pos >= last => {
                        c.tile = (c.tile + 1) % n;
                        c.rows[c.tile] = sel(c, c.tile).first().copied().unwrap_or(0);
                    }
                    KeyCode::Up => c.rows[c.tile] = rows[pos - 1],
                    KeyCode::Down => c.rows[c.tile] = rows[pos + 1],
                    KeyCode::PageUp => c.rows[c.tile] = rows[pos.saturating_sub(5)],
                    KeyCode::PageDown => c.rows[c.tile] = rows[(pos + 5).min(last)],
                    KeyCode::Home => c.rows[c.tile] = rows.first().copied().unwrap_or(0),
                    _ => c.rows[c.tile] = rows.last().copied().unwrap_or(0),
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let t = c.tile;
                let act = ts[t].rows.get(c.rows[t]).map(|r| r.act.clone()).unwrap_or(Act::None);
                self.do_act(act);
            }
            KeyCode::Char('r') if !ctrl && !alt => {
                let t = c.tile;
                match ts[t].rows.get(c.rows[t]).map(|r| r.act.clone()) {
                    Some(Act::Restart(s)) => self.message = Some(self.lsp_restart(&s)),
                    _ => {
                        self.control_refresh(true);
                        self.message = Some("Refreshing… (r on a language server restarts it)".into());
                    }
                }
            }
            KeyCode::Char('m') if !ctrl && !alt => hooks::mission_control(self, None),
            _ => {}
        }
    }

    fn settings_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool) {
        let n = settings::FIELDS.len() + 3; // + open settings.toml, open languages.toml, back
        let c = &mut self.control;
        match k.code {
            KeyCode::Esc => self.open_control(Page::Overview),
            KeyCode::Up => c.setting = c.setting.saturating_sub(1),
            KeyCode::Down => c.setting = (c.setting + 1).min(n - 1),
            KeyCode::Home => c.setting = 0,
            KeyCode::End => c.setting = n - 1,
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Left if !ctrl && !alt => {
                let i = c.setting;
                let back = k.code == KeyCode::Left;
                match settings::FIELDS.get(i) {
                    Some(f) => {
                        let v = match f.key {
                            "vim" | "hints" | "autocomplete" => {
                                self.toggle_setting(f.key);
                                return;
                            }
                            _ => settings::cycle(f, &settings::value(&self.control.settings, f.key), back),
                        };
                        if f.key == "theme" {
                            self.set_theme_key(v.as_str().unwrap_or(""));
                        } else {
                            self.set_setting(f.key, v);
                        }
                    }
                    None if matches!(k.code, KeyCode::Left | KeyCode::Right) => {}
                    None => match i - settings::FIELDS.len() {
                        0 => self.open_settings_file(),
                        1 => self.open_languages_file(),
                        _ => self.open_control(Page::Overview),
                    },
                }
            }
            _ => {}
        }
    }

    /// What a tile row's Enter does.
    pub fn do_act(&mut self, act: Act) {
        match act {
            Act::None => self.message = Some("Nothing to do on this row".into()),
            Act::Say(s) => self.message = Some(s),
            Act::GitView(root) => hooks::git_view_on(self, &root),
            Act::Mission(run) => hooks::mission_control(self, run),
            Act::NewAgent => self.agents_new(),
            Act::Staff => self.refresh_staff(),
            Act::Restart(s) => self.message = Some(self.lsp_restart(&s)),
            Act::Problems => {
                self.close_control();
                self.focus_editor();
                self.press(&["Alt+6"]);
            }
            Act::Tab(i) => {
                self.close_control();
                self.focus_editor();
                self.active = i.min(self.docs.len() - 1);
            }
            Act::SaveAll => self.save_all(),
            Act::Setting(key) => match key {
                "theme" => self.set_theme(self.control.settings.theme.next()),
                k => self.toggle_setting(k),
            },
            Act::SettingsPage => self.open_control(Page::Settings),
            Act::OpenSettings => self.open_settings_file(),
            Act::OpenLanguages => self.open_languages_file(),
            Act::Tree => {
                self.close_control();
                self.press(&["Alt+1"]);
            }
            Act::FindAction => self.open_palette(),
        }
    }

    pub fn control_paste(&mut self, text: &str) -> bool {
        if self.palette.open {
            self.palette.input.push_str(text.lines().next().unwrap_or(""));
            self.palette.selected = 0;
            return true;
        }
        self.control.open
    }

    pub fn control_mouse(&mut self, m: MouseEvent) -> bool {
        if self.palette.open {
            self.palette_mouse(m);
            return true;
        }
        if !self.control.open {
            return false;
        }
        let at = |r: &Rect| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.control.page == Page::Settings {
                    if let Some((i, _)) = self.control.setting_rects.iter().find(|(_, r)| at(r)) {
                        self.control.setting = *i;
                    }
                } else if let Some((t, r, _)) = self.control.row_rects.iter().find(|(_, _, r)| at(r)).copied() {
                    let again = self.control.tile == t && self.control.rows[t] == r;
                    self.control.tile = t;
                    self.control.rows[t] = r;
                    if again {
                        let ts = tiles::build_one(self, tiles::ORDER[t]);
                        if let Some(row) = ts.rows.get(r) {
                            let act = row.act.clone();
                            self.do_act(act);
                        }
                    }
                } else if let Some(t) = self.control.tile_rects.iter().position(at) {
                    self.control.tile = t;
                }
            }
            MouseEventKind::ScrollDown => self.overview_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), false, false, false),
            MouseEventKind::ScrollUp => self.overview_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), false, false, false),
            _ => {}
        }
        true
    }
}
