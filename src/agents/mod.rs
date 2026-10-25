//! Agents panel (M6): a right-side tool window (Alt+9) to run Claude Code agents on the open
//! project, follow them as a transcript, send follow-ups, stop them, and watch the Aa staff.
//!
//! Each turn is a `claude -p --output-format stream-json --verbose` child in the project root
//! (follow-ups add `--resume <session_id>`), read on a background thread (`run.rs`). `App` keeps
//! one [`Agents`]; the hooks in `app.rs` / `ui.rs` / `main.rs` hand it keys, mouse, paste, ticks and
//! a strip of the screen. Runs are saved in `~/.config/code-atlas/agents.json`.

pub mod cli;
pub mod input;
pub mod run;
pub mod staff;
pub mod stream;
pub mod transcript;
pub mod ui;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::doc::Doc;
use crate::project::canonical;
use input::Input;
use run::{Run, Turn};
#[cfg(test)]
use run::State;

pub const PANEL_W: u16 = 45;
/// Runs kept in the file (all projects together).
const MAX_RUNS: usize = 100;
/// Files bigger than this aren't compared or reloaded automatically.
const MAX_RELOAD_BYTES: u64 = 8 << 20;
/// Lines of a selection put into a prompt at most.
const MAX_CTX_LINES: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Area {
    Prompt,
    Runs,
    Transcript,
}

#[derive(Serialize, Deserialize, Default)]
struct Store {
    next_id: u64,
    runs: Vec<Run>,
}

pub struct Agents {
    pub visible: bool,
    pub focus: bool,
    pub area: Area,
    /// All saved runs (every project), oldest first; the panel shows this project's.
    pub runs: Vec<Run>,
    next_id: u64,
    /// Selected run id; `None` = "+ New agent".
    pub selected: Option<u64>,
    pub input: Input,
    /// Permission modes offered: index 0 = the CLI default (no flag), then `claude --help`'s.
    pub modes: Vec<String>,
    pub mode_idx: usize,
    pub has_agent_flag: bool,
    /// Agent types: index 0 = none.
    pub agent_names: Vec<String>,
    pub agent_idx: usize,
    /// Add the current file / selected lines to the prompt.
    pub include_ctx: bool,
    /// Transcript scroll (first shown line) and whether it sticks to the end.
    pub scroll: usize,
    pub follow: bool,
    /// Last scroll position that still fills the view (set while drawing).
    pub max_scroll: usize,
    pub staff: staff::Staff,
    /// Open files an agent changed while they had unsaved edits.
    pub changed_on_disk: Vec<PathBuf>,
    reload_armed: bool,
    root: PathBuf,
    store: Option<PathBuf>,
    program: String,
    cli: Option<Receiver<cli::CliInfo>>,
    // Screen areas from the last draw (mouse).
    pub rect: Rect,
    pub runs_rect: Rect,
    pub transcript_rect: Rect,
    pub prompt_rect: Rect,
    /// First run shown in the list (scroll).
    pub runs_scroll: usize,
}

/// Where runs are saved: `$CATLAS_CONFIG_DIR`, else `$XDG_CONFIG_HOME/code-atlas`, else
/// `~/.config/code-atlas`.
fn store_path() -> Option<PathBuf> {
    let dir = if let Some(d) = std::env::var_os("CATLAS_CONFIG_DIR") {
        PathBuf::from(d)
    } else if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        PathBuf::from(x).join("code-atlas")
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config/code-atlas")
    };
    Some(dir.join("agents.json"))
}

impl Agents {
    pub fn new(root: PathBuf) -> Agents {
        let store = store_path();
        Self::with_store(root, store)
    }

    fn with_store(root: PathBuf, store: Option<PathBuf>) -> Agents {
        let Store { next_id, mut runs } = store
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        runs.iter_mut().for_each(Run::after_load);
        let next_id = next_id.max(runs.iter().map(|r| r.id + 1).max().unwrap_or(1));
        let program = cli::find_claude();
        let mut agent_names = vec![String::new()];
        agent_names.extend(cli::list_agents(&cli::agent_dirs(&root)));
        Agents {
            visible: false,
            focus: false,
            area: Area::Prompt,
            runs,
            next_id,
            selected: None,
            input: Input::default(),
            modes: vec![String::new()],
            mode_idx: 0,
            has_agent_flag: false,
            agent_names,
            agent_idx: 0,
            include_ctx: false,
            scroll: 0,
            follow: true,
            max_scroll: 0,
            staff: staff::Staff::default(),
            changed_on_disk: Vec::new(),
            reload_armed: false,
            cli: None,
            program,
            root,
            store,
            rect: Rect::default(),
            runs_rect: Rect::default(),
            transcript_rect: Rect::default(),
            prompt_rect: Rect::default(),
            runs_scroll: 0,
        }
    }

    pub fn save(&self) {
        let Some(p) = &self.store else { return };
        let mut keep: Vec<&Run> = self.runs.iter().collect();
        let n = keep.len().saturating_sub(MAX_RUNS);
        keep.drain(..n);
        #[derive(Serialize)]
        struct Out<'a> {
            next_id: u64,
            runs: Vec<&'a Run>,
        }
        let Ok(json) = serde_json::to_string_pretty(&Out { next_id: self.next_id, runs: keep }) else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // Write a temp file and rename it, so a crash can't leave half a file.
        let tmp = p.with_extension(format!("json.{}", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, p);
        }
    }

    /// This project's runs, newest first (what the list shows).
    pub fn shown(&self) -> Vec<usize> {
        let root = canonical(&self.root);
        let mut v: Vec<usize> = (0..self.runs.len()).filter(|&i| canonical(&self.runs[i].cwd) == root).collect();
        v.reverse();
        v
    }

    pub fn run_index(&self, id: u64) -> Option<usize> {
        self.runs.iter().position(|r| r.id == id)
    }

    pub fn selected_run(&self) -> Option<&Run> {
        self.selected.and_then(|id| self.run_index(id)).map(|i| &self.runs[i])
    }

    pub fn running_count(&self) -> usize {
        self.runs.iter().filter(|r| r.running()).count()
    }

    pub fn mode(&self) -> Option<String> {
        Some(self.modes[self.mode_idx].clone()).filter(|m| !m.is_empty())
    }

    pub fn agent(&self) -> Option<String> {
        Some(self.agent_names[self.agent_idx].clone()).filter(|a| !a.is_empty())
    }

    pub fn mode_label(&self) -> &str {
        if self.mode_idx == 0 { "default" } else { &self.modes[self.mode_idx] }
    }

    fn select(&mut self, id: Option<u64>) {
        self.selected = id;
        self.follow = true;
        // Follow-ups default to the run's own mode and agent.
        if let Some(r) = self.selected_run() {
            let (m, a) = (r.mode.clone().unwrap_or_default(), r.agent.clone().unwrap_or_default());
            if let Some(i) = self.modes.iter().position(|x| *x == m) {
                self.mode_idx = i;
            }
            if let Some(i) = self.agent_names.iter().position(|x| *x == a) {
                self.agent_idx = i;
            }
        }
    }

    /// Moves the list selection: row 0 is "+ New agent", then the runs.
    fn step_selection(&mut self, d: isize) {
        let shown = self.shown();
        let cur = self.selected.and_then(|id| shown.iter().position(|&i| self.runs[i].id == id)).map_or(0, |p| p + 1);
        let next = (cur as isize + d).clamp(0, shown.len() as isize) as usize;
        let id = if next == 0 { None } else { Some(self.runs[shown[next - 1]].id) };
        self.select(id);
    }

    pub fn toggle(&mut self) {
        if !self.visible {
            self.visible = true;
            self.focus = true;
            self.area = Area::Prompt;
        } else if self.focus {
            self.visible = false;
            self.focus = false;
        } else {
            self.focus = true;
        }
        if self.visible && self.cli.is_none() && self.modes.len() == 1 {
            self.cli = Some(cli::probe(self.program.clone()));
        }
    }

    /// Starts a new run or a follow-up in the selected one. Returns a status message.
    fn send(&mut self, ctx: Option<(String, String)>) -> Option<String> {
        if self.input.is_blank() {
            return None;
        }
        let typed = self.input.text.trim_end().to_string();
        let (prompt, ctx_label) = match ctx.filter(|_| self.include_ctx) {
            Some((label, block)) => (format!("{typed}\n\n{block}"), Some(label)),
            None => (typed.clone(), None),
        };
        let turn = Turn { prompt, shown: typed.clone(), ctx: ctx_label, mode: self.mode(), agent: self.agent().filter(|_| self.has_agent_flag) };
        let program = self.program.clone();
        let i = match self.selected.and_then(|id| self.run_index(id)) {
            Some(i) => {
                let r = &self.runs[i];
                if r.running() {
                    return Some("This agent is still working. Ctrl+C stops it, Ctrl+N starts another.".into());
                }
                if r.session_id.is_none() {
                    return Some("That run has no session to resume. Ctrl+N starts a new agent.".into());
                }
                i
            }
            None => {
                let id = self.next_id;
                self.next_id += 1;
                self.runs.push(Run::new(id, self.root.clone(), &typed));
                self.runs.len() - 1
            }
        };
        self.input.clear();
        self.runs[i].start(&program, &turn);
        let id = self.runs[i].id;
        self.selected = Some(id);
        self.follow = true;
        self.save();
        None
    }

    fn stop_selected(&mut self) -> Option<String> {
        let i = self.selected.and_then(|id| self.run_index(id))?;
        if !self.runs[i].running() {
            return Some("The selected agent isn't running.".into());
        }
        self.runs[i].stop();
        Some("Stopping the agent…".into())
    }

    fn remove_selected(&mut self) -> Option<String> {
        let i = self.selected.and_then(|id| self.run_index(id))?;
        if self.runs[i].running() {
            return Some("Stop the agent first (Ctrl+C).".into());
        }
        self.step_selection(1);
        if self.selected == Some(self.runs[i].id) {
            self.step_selection(-1);
        }
        let r = self.runs.remove(i);
        self.save();
        Some(format!("Removed \"{}\" from the list (its Claude session is kept).", transcript::ellipsize(&r.title, 40)))
    }

    /// Panel keys; `ctx` gives the current file context for "include file". `None` = not ours.
    fn key(&mut self, k: KeyEvent, ctx: impl FnOnce() -> Option<(String, String)>) -> Option<Option<String>> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ch = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        // Keys anywhere in the panel.
        match k.code {
            KeyCode::Esc => {
                self.focus = false;
                return Some(None);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let back = k.code == KeyCode::BackTab || shift;
                self.area = match (self.area, back) {
                    (Area::Prompt, false) | (Area::Transcript, true) => Area::Runs,
                    (Area::Runs, false) | (Area::Prompt, true) => Area::Transcript,
                    (Area::Transcript, false) | (Area::Runs, true) => Area::Prompt,
                };
                return Some(None);
            }
            _ if ctrl && !alt && ch('c') => return Some(self.stop_selected()),
            _ if ctrl && !alt && ch('n') => {
                self.select(None);
                self.area = Area::Prompt;
                return Some(None);
            }
            _ if alt && !ctrl && ch('m') => {
                self.mode_idx = (self.mode_idx + 1) % self.modes.len();
                let note = if self.modes.len() == 1 { " (checking claude --help…)" } else { "" };
                return Some(Some(format!("Permission mode for the next send: {}{note}", self.mode_label())));
            }
            _ if alt && !ctrl && ch('a') => {
                if !self.has_agent_flag {
                    return Some(Some("This claude has no --agent flag (or claude --help hasn't answered yet).".into()));
                }
                self.agent_idx = (self.agent_idx + 1) % self.agent_names.len();
                let a = self.agent().unwrap_or_else(|| "none".into());
                return Some(Some(format!("Agent type for the next send: {a}")));
            }
            _ if alt && !ctrl && ch('i') => {
                self.include_ctx = !self.include_ctx;
                return Some(Some(format!("Include current file/selection: {}", if self.include_ctx { "on" } else { "off" })));
            }
            KeyCode::PageUp => {
                self.scroll_by(-(self.transcript_rect.height.max(2) as isize - 1));
                return Some(None);
            }
            KeyCode::PageDown => {
                self.scroll_by(self.transcript_rect.height.max(2) as isize - 1);
                return Some(None);
            }
            _ => {}
        }
        match self.area {
            Area::Prompt => {
                let newline = (k.code == KeyCode::Enter && (alt || shift)) || (ctrl && ch('j'));
                match k.code {
                    _ if newline => self.input.insert('\n'),
                    KeyCode::Enter => return Some(self.send(ctx())),
                    KeyCode::Backspace if ctrl || alt => self.input.delete_word_left(),
                    KeyCode::Char('h') if ctrl => self.input.delete_word_left(),
                    KeyCode::Char('u') if ctrl => self.input.clear(),
                    KeyCode::Backspace => self.input.backspace(),
                    KeyCode::Delete => self.input.delete(),
                    KeyCode::Left => self.input.left(),
                    KeyCode::Right => self.input.right(),
                    KeyCode::Home => self.input.home(),
                    KeyCode::End => self.input.end(),
                    KeyCode::Up => {
                        if !self.input.vertical(false) {
                            self.area = Area::Runs;
                        }
                    }
                    KeyCode::Down => {
                        self.input.vertical(true);
                    }
                    KeyCode::Char(c) if !ctrl && !alt => self.input.insert(c),
                    _ if ctrl || alt => return None,
                    _ => {}
                }
            }
            Area::Runs => match k.code {
                KeyCode::Up => self.step_selection(-1),
                KeyCode::Down => self.step_selection(1),
                KeyCode::Home => self.step_selection(-10_000),
                KeyCode::End => self.step_selection(10_000),
                KeyCode::Enter => self.area = Area::Prompt,
                KeyCode::Delete => return Some(self.remove_selected()),
                _ if ctrl || alt => return None,
                _ => {}
            },
            Area::Transcript => match k.code {
                KeyCode::Up => self.scroll_by(-1),
                KeyCode::Down => self.scroll_by(1),
                KeyCode::Home => self.scroll_by(-(self.max_scroll as isize)),
                KeyCode::End => self.follow = true,
                KeyCode::Enter => self.area = Area::Prompt,
                _ if ctrl || alt => return None,
                _ => {}
            },
        }
        Some(None)
    }

    /// Scrolls the transcript; reaching the end sticks to it again.
    pub fn scroll_by(&mut self, d: isize) {
        let cur = if self.follow { self.max_scroll } else { self.scroll.min(self.max_scroll) };
        self.scroll = (cur as isize + d).clamp(0, self.max_scroll as isize) as usize;
        self.follow = self.scroll >= self.max_scroll;
    }

    fn mouse(&mut self, m: MouseEvent) -> bool {
        let inside = |r: Rect| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        if !self.visible || !inside(self.rect) {
            if matches!(m.kind, MouseEventKind::Down(_)) {
                self.focus = false;
            }
            return false;
        }
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d: isize = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                if inside(self.runs_rect) {
                    self.step_selection(d.signum());
                } else {
                    self.scroll_by(d);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = true;
                if inside(self.runs_rect) {
                    self.area = Area::Runs;
                    let row = self.runs_scroll + (m.row - self.runs_rect.y) as usize;
                    let shown = self.shown();
                    if row == 0 {
                        self.select(None);
                        self.area = Area::Prompt;
                    } else if let Some(&i) = shown.get(row - 1) {
                        let id = self.runs[i].id;
                        self.select(Some(id));
                    }
                } else if inside(self.transcript_rect) {
                    self.area = Area::Transcript;
                } else if inside(self.prompt_rect) {
                    self.area = Area::Prompt;
                }
            }
            _ => {}
        }
        true
    }
}

impl Drop for Agents {
    fn drop(&mut self) {
        // Runs end their agents when dropped (`Run::drop`); save them as stopped.
        let any = self.runs.iter().any(Run::running);
        for r in &mut self.runs {
            if r.running() {
                r.after_load();
            }
        }
        if any {
            self.save();
        }
    }
}

// ---- reload: files an agent changed -------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum Reload {
    /// The buffer matches the disk.
    Same,
    /// No unsaved edits: take the disk version.
    Reload,
    /// Unsaved edits: keep them, flag "changed on disk".
    Conflict,
}

/// What to do with an open document whose file may have changed. `disk = None`: unreadable or
/// gone (treated as a change, never reloaded).
pub fn decide(dirty: bool, disk: Option<&str>, buffer: &str) -> Reload {
    match disk {
        Some(d) if d == buffer => Reload::Same,
        Some(_) if !dirty => Reload::Reload,
        _ => Reload::Conflict,
    }
}

fn read_small(p: &Path) -> Option<String> {
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > MAX_RELOAD_BYTES {
        return None;
    }
    std::fs::read_to_string(p).ok()
}

/// Replaces `d` with the file's current content, keeping the cursor line and scroll.
pub fn reload_doc(d: &mut Doc) -> anyhow::Result<()> {
    let path = d.path.clone().ok_or_else(|| anyhow::anyhow!("no file"))?;
    let mut fresh = Doc::open(&path)?;
    let len = fresh.rope.len_chars();
    fresh.cursor = d.cursor.min(len);
    fresh.scroll_line = d.scroll_line.min(fresh.visible_lines().saturating_sub(1));
    fresh.scroll_col = d.scroll_col;
    fresh.view_h = d.view_h;
    *d = fresh;
    Ok(())
}

// ---- glue: the panel's side of `App` -------------------------------------------------------

impl App {
    /// Global agent keys (Alt+9, Ctrl+Alt+Y) and the focused panel. false = not ours.
    pub fn agents_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let ch = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        if alt && !ctrl && ch('9') {
            self.project.close_popup();
            self.agents.toggle();
            if self.agents.focus {
                self.project.focus_tree = false;
            }
            self.message = Some(self.agents_hint());
            return true;
        }
        // Ctrl+Alt+Y (IDEA: Reload All from Disk): reload the current file, asking once if unsaved.
        if ctrl && alt && ch('y') {
            self.reload_current();
            return true;
        }
        self.agents.reload_armed = false;
        if !self.agents.focus || !self.agents.visible || self.project.popup.is_some() {
            return false;
        }
        let d = &self.docs[self.active];
        let root = self.project.root.clone();
        let ctx = || file_context(d, &root);
        match self.agents.key(k, ctx) {
            None => false,
            Some(msg) => {
                self.message = msg.or_else(|| self.agents.focus.then(|| self.agents_hint()));
                true
            }
        }
    }

    fn agents_hint(&self) -> String {
        if !self.agents.focus {
            return String::new();
        }
        match self.agents.area {
            Area::Prompt => "Enter send · Alt+Enter newline · Alt+M mode · Alt+A agent · Alt+I file · Tab list · Esc editor".into(),
            Area::Runs => "↑↓ pick a run · Enter write a follow-up · Ctrl+N new · Ctrl+C stop · Del remove · Esc editor".into(),
            Area::Transcript => "↑↓ PgUp PgDn scroll · End follow · Tab prompt · Esc editor".into(),
        }
    }

    pub fn agents_mouse(&mut self, m: MouseEvent) -> bool {
        let used = self.agents.mouse(m);
        if used && self.agents.focus {
            self.project.focus_tree = false;
        }
        used
    }

    /// A bracketed paste goes into the prompt box when the panel has focus there.
    pub fn agents_paste(&mut self, text: &str) -> bool {
        if self.agents.visible && self.agents.focus && self.agents.area == Area::Prompt && self.project.popup.is_none() {
            self.agents.input.insert_str(text);
            return true;
        }
        false
    }

    /// Called once per loop turn: streams agent output in, reloads changed files, refreshes staff.
    pub fn agents_tick(&mut self) {
        let a = &mut self.agents;
        if self.project.focus_tree || self.project.popup.is_some() {
            a.focus = false;
        }
        if let Some(rx) = &a.cli {
            if let Ok(info) = rx.try_recv() {
                a.modes.truncate(1);
                a.modes.extend(info.modes);
                a.has_agent_flag = info.has_agent;
                a.cli = None;
            }
        }
        let mut files = Vec::new();
        let mut save = false;
        let mut finished = Vec::new();
        for r in &mut a.runs {
            let ch = r.poll();
            files.extend(ch.files);
            save |= ch.session || ch.finished;
            if ch.finished {
                finished.push((r.id, r.title.clone(), r.state));
            }
        }
        if save {
            a.save();
        }
        if a.visible {
            a.staff.maybe_refresh();
        }
        a.staff.poll();
        for (id, title, state) in &finished {
            if !a.visible || a.selected != Some(*id) {
                self.message = Some(format!("Agent \"{}\" {} · Alt+9 shows it", transcript::ellipsize(title, 40), state.label()));
            }
        }
        // Edited files, then (after a turn, for changes made by shell commands) every open file.
        for f in files {
            self.check_on_disk(&canonical(&f), false);
        }
        if !finished.is_empty() {
            let open: Vec<PathBuf> = self.docs.iter().filter_map(|d| d.path.clone()).map(|p| canonical(&p)).collect();
            for p in open {
                self.check_on_disk(&p, true);
            }
        }
        // A flagged file whose edits were saved or undone gets checked again.
        let flagged: Vec<PathBuf> = self.agents.changed_on_disk.clone();
        for p in flagged {
            if self.docs.iter().any(|d| d.path.as_deref().is_some_and(|x| canonical(x) == p) && !d.dirty()) {
                self.agents.changed_on_disk.retain(|x| *x != p);
                self.check_on_disk(&p, true);
            }
        }
        let cur = self.docs[self.active].path.as_deref().map(canonical);
        if self.message.is_none() && cur.is_some_and(|c| self.agents.changed_on_disk.contains(&c)) {
            self.message = Some(format!("{} changed on disk · your unsaved edits are kept · Ctrl+Alt+Y reloads", self.docs[self.active].name()));
        }
    }

    /// Compares an open file with the disk: reload it, or flag it when it has unsaved edits.
    /// `quiet`: no message when nothing changed.
    fn check_on_disk(&mut self, p: &Path, quiet: bool) {
        let Some(i) = self.docs.iter().position(|d| d.path.as_deref().is_some_and(|x| canonical(x) == p)) else { return };
        let disk = read_small(p);
        if disk.is_none() && quiet {
            return; // deleted or huge: leave it
        }
        let d = &mut self.docs[i];
        match decide(d.dirty(), disk.as_deref(), &d.rope.to_string()) {
            Reload::Same => {}
            Reload::Reload => {
                let name = d.name();
                self.message = Some(match reload_doc(d) {
                    Ok(()) => format!("Reloaded {name} (changed by an agent)"),
                    Err(e) => format!("Couldn't reload {name}: {e:#}"),
                });
            }
            Reload::Conflict => {
                if !self.agents.changed_on_disk.iter().any(|x| x == p) {
                    self.agents.changed_on_disk.push(p.to_path_buf());
                }
                self.message = Some(format!("{} changed on disk · your unsaved edits are kept · Ctrl+Alt+Y reloads", d.name()));
            }
        }
    }

    fn reload_current(&mut self) {
        let armed = std::mem::take(&mut self.agents.reload_armed);
        let d = &mut self.docs[self.active];
        let Some(p) = d.path.clone() else {
            self.message = Some("This tab has no file.".into());
            return;
        };
        let disk = read_small(&p);
        if disk.as_deref() == Some(&d.rope.to_string()) {
            self.message = Some(format!("{} is the same as on disk", d.name()));
            return;
        }
        if d.dirty() && !armed {
            self.agents.reload_armed = true;
            self.message = Some(format!("{} has unsaved edits. Ctrl+Alt+Y again discards them and reloads.", d.name()));
            return;
        }
        let name = d.name();
        self.message = Some(match reload_doc(d) {
            Ok(()) => format!("Reloaded {name} from disk"),
            Err(e) => format!("Couldn't reload {name}: {e:#}"),
        });
        let c = canonical(&p);
        self.agents.changed_on_disk.retain(|x| *x != c);
    }
}

/// The current file as prompt context: a label for the transcript and the text added to the
/// prompt (path, selected line numbers and those lines).
pub fn file_context(d: &Doc, root: &Path) -> Option<(String, String)> {
    let path = d.path.as_ref()?;
    let rel = crate::project::search::rel(root, &canonical(path));
    match d.selection() {
        Some((s, e)) if e > s => {
            let first = d.rope.char_to_line(s);
            // A selection ending at a line start doesn't include that line.
            let mut last = d.rope.char_to_line(e);
            if last > first && d.line_start(last) == e {
                last -= 1;
            }
            let label = format!("{rel}:{}-{}", first + 1, last + 1);
            let mut text = String::new();
            for l in first..=last.min(first + MAX_CTX_LINES - 1) {
                text.push_str(&format!("{:>5}  {}", l + 1, d.rope.line(l)));
                if !text.ends_with('\n') {
                    text.push('\n');
                }
            }
            if last >= first + MAX_CTX_LINES {
                text.push_str("  …\n");
            }
            Some((label, format!("Current file: {rel} (lines {}-{} selected):\n```\n{text}```", first + 1, last + 1)))
        }
        _ => {
            let line = d.line() + 1;
            Some((format!("{rel}:{line}"), format!("Current file: {rel} (cursor on line {line}).")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_decisions() {
        assert_eq!(decide(false, Some("a"), "a"), Reload::Same);
        assert_eq!(decide(true, Some("a"), "a"), Reload::Same);
        assert_eq!(decide(false, Some("b"), "a"), Reload::Reload);
        assert_eq!(decide(true, Some("b"), "a"), Reload::Conflict);
        assert_eq!(decide(false, None, "a"), Reload::Conflict);
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("catlas-agents-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reload_keeps_the_cursor_and_takes_the_disk_text() {
        let d = scratch("reload");
        let f = d.join("a.txt");
        std::fs::write(&f, "one\ntwo\nthree\n").unwrap();
        let mut doc = Doc::open(&f).unwrap();
        doc.goto_line(1, 2);
        std::fs::write(&f, "one\nTWO\nthree\nfour\n").unwrap();
        reload_doc(&mut doc).unwrap();
        assert_eq!(doc.rope.to_string(), "one\nTWO\nthree\nfour\n");
        assert_eq!((doc.line(), doc.col()), (1, 2));
        assert!(!doc.dirty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn app_reloads_clean_docs_and_flags_dirty_ones() {
        let d = scratch("app");
        let (a, b) = (d.join("a.txt"), d.join("b.txt"));
        std::fs::write(&a, "a\n").unwrap();
        std::fs::write(&b, "b\n").unwrap();
        let mut app = App::new(vec![d.clone(), a.clone(), b.clone()]).unwrap();
        app.agents.store = None;
        app.docs[1].type_char('x'); // b.txt has unsaved edits
        std::fs::write(&a, "a changed\n").unwrap();
        std::fs::write(&b, "b changed\n").unwrap();
        app.check_on_disk(&canonical(&a), false);
        app.check_on_disk(&canonical(&b), false);
        assert_eq!(app.docs[0].rope.to_string(), "a changed\n");
        assert_eq!(app.docs[1].rope.to_string(), "xb\n");
        assert_eq!(app.agents.changed_on_disk, vec![canonical(&b)]);
        // Undoing the edit makes it clean again: the next tick reloads it.
        app.docs[1].undo();
        app.agents_tick();
        assert_eq!(app.docs[1].rope.to_string(), "b changed\n");
        assert!(app.agents.changed_on_disk.is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn context_names_the_selected_lines() {
        let mut doc = Doc::open(Path::new("/tmp/catlas-ctx/src/x.rs")).unwrap();
        doc.insert_str("fn a() {}\nfn b() {}\nfn c() {}\n");
        let root = Path::new("/tmp/catlas-ctx");
        doc.select(doc.line_start(1), doc.line_start(3));
        let (label, block) = file_context(&doc, root).unwrap();
        assert_eq!(label, "src/x.rs:2-3");
        assert!(block.contains("lines 2-3 selected") && block.contains("    2  fn b() {}\n    3  fn c() {}\n```"), "{block}");
        doc.move_to(0, false);
        assert_eq!(file_context(&doc, root).unwrap().0, "src/x.rs:1");
        assert!(file_context(&Doc::empty(), root).is_none());
    }

    #[test]
    fn runs_persist_and_reload_as_stopped() {
        let d = scratch("store");
        let store = Some(d.join("agents.json"));
        let mut a = Agents::with_store(d.clone(), store.clone());
        let mut r = Run::new(a.next_id, d.clone(), "hello");
        a.next_id += 1;
        r.begin_turn(&Turn { prompt: "hello".into(), shown: "hello".into(), ..Turn::default() });
        r.session_id = Some("sid-1".into());
        a.runs.push(r);
        a.runs.push(Run::new(a.next_id, PathBuf::from("/elsewhere"), "other project"));
        a.next_id += 1;
        a.save();
        drop(a);
        let b = Agents::with_store(d.clone(), store);
        assert_eq!(b.runs.len(), 2);
        assert_eq!(b.runs[0].state, State::Stopped);
        assert_eq!(b.runs[0].session_id.as_deref(), Some("sid-1"));
        assert_eq!(b.shown(), vec![0], "only this project's runs are listed");
        assert!(b.next_id >= 3);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn list_selection_and_follow_up_rules() {
        let d = scratch("sel");
        let mut a = Agents::with_store(d.clone(), None);
        for (i, t) in ["first", "second"].iter().enumerate() {
            let mut r = Run::new(i as u64 + 1, d.clone(), t);
            r.mode = Some("plan".into());
            a.runs.push(r);
        }
        a.modes = vec![String::new(), "acceptEdits".into(), "plan".into()];
        a.step_selection(1);
        assert_eq!(a.selected, Some(2), "newest first, under '+ New agent'");
        assert_eq!(a.mode_label(), "plan", "choosers follow the selected run");
        a.step_selection(5);
        assert_eq!(a.selected, Some(1));
        a.step_selection(-5);
        assert_eq!(a.selected, None);
        // A follow-up to a run without a session is refused, without starting anything.
        a.select(Some(1));
        a.input.insert_str("again");
        let msg = a.send(None).unwrap();
        assert!(msg.contains("no session"), "{msg}");
        assert_eq!(a.input.text, "again");
        a.runs[0].state = State::Running;
        a.runs[0].session_id = Some("s".into());
        assert!(a.send(None).unwrap().contains("still working"));
        a.runs[0].state = State::Done;
        std::fs::remove_dir_all(d).unwrap();
    }
}
