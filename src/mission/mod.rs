//! Mission control (decision 0015): agent-native editing. A full-screen view (Ctrl+O, Alt+7) with
//! a live card per agent: repo, worktree / branch, what it is doing, time, cost, tokens, files
//! changed, its last tool line and an activity sparkline. From there: transcript, follow-up,
//! stop, review (the Git view on its worktree), **land** (merge its branch into the base branch,
//! always asked, never pushed or forced), remove its worktree, discard.
//!
//! Agents that change code get their own git worktree (`<repo>-wt/<slug>` on branch `<slug>`,
//! `worktree.rs`), so several can work at once. **Ctrl+U** (Alt+A) on a selection or the current
//! file asks an agent about that code: explain (in place), fix, write tests or free text (in a
//! worktree). Presence in the editor (`presence.rs`): `✳` on files agents are writing, line
//! numbers in the agent's colour for lines it changed, a ticker in the status bar. Finished or
//! stuck agents ring / notify (`notify.rs`).
//!
//! Hooks elsewhere: `App.mission`; `mission_key` / `mission_paste` / `mission_mouse` first in
//! `App::handle`; `app.mission_tick()` in the main loop; `ui::draw` calls `mission::ui::draw`
//! (over everything but the status bar), `draw_ticker`, the tab / tree / line-number markers.
//! Runs themselves are the agents panel's (`src/agents/`): one list, two views.

pub mod notify;
pub mod presence;
pub mod ui;
pub mod worktree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::agents::run::{now_secs, Baseline, Run, State, Turn};
use crate::app::App;
use crate::project::canonical;
use worktree::{How, Worktree};

/// Live "files +/−" of working agents are counted this often while mission control is open.
const LIVE_STAT_EVERY: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Cards,
    Transcript,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// A new agent: in a worktree or in place, in `repos()[repo]`.
    New { worktree: bool, repo: usize },
    FollowUp(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineInput {
    pub kind: InputKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Land(u64),
    Remove(u64),
    Discard(u64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    pub question: String,
    pub act: Act,
    /// Landing a branch whose base moved: `b` rebases first.
    pub can_rebase: bool,
}

/// The ask-an-agent presets: (name, what the agent is told, in a worktree by default).
pub const PRESETS: [(&str, &str, bool); 3] = [
    ("explain", "Explain this code: what it does, how it fits into the project, and anything surprising or risky. Don't change any files.", false),
    ("fix", "Find and fix the problem in this code (bugs, errors, edge cases). Keep the change small, then say what you changed and why.", true),
    ("write tests", "Write tests for this code in the project's existing test style, and run them if you can. Say what they cover.", true),
];

/// Ctrl+U: a one-line prompt about the selection or the current file.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    /// `src/x.rs:3-9`.
    pub label: String,
    /// What goes into the prompt (path, lines, the selected text).
    pub block: String,
    /// Where the agent runs (in place) or which repository gets the worktree.
    pub cwd: PathBuf,
    /// The preset under the cursor (used when nothing is typed).
    pub sel: usize,
    pub text: String,
    pub worktree: bool,
}

impl Ask {
    pub fn set_sel(&mut self, sel: usize) {
        self.sel = sel;
        self.worktree = PRESETS[sel].2;
    }
}

/// An agent whose worktree is being made (a thread); it starts when that is done.
pub struct Pending {
    pub id: u64,
    pub title: String,
    pub repo: PathBuf,
    turn: Turn,
    show_in_panel: bool,
    rx: Receiver<Result<(Worktree, String), String>>,
}

enum Job {
    PlanLand(u64),
    AskDiscard(u64),
    Land(u64),
    Remove(u64),
    Discard(u64),
}

type JobResult = Result<(String, bool), String>;

pub struct Mission {
    pub open: bool,
    pub view: View,
    /// Selected card (a run id; pending agents can't be selected).
    pub sel: Option<u64>,
    pub input: Option<LineInput>,
    pub confirm: Option<Confirm>,
    pub ask: Option<Ask>,
    pub pending: Vec<Pending>,
    job: Option<(Job, Receiver<JobResult>)>,
    /// What runs in the background right now ("Landing fix-x …").
    pub busy: Option<String>,
    /// Live "files +/−" per working run, and when it was counted.
    pub live: HashMap<u64, String>,
    live_jobs: HashMap<u64, (Instant, Option<Receiver<Option<String>>>)>,
    /// Last seen state per run (notifications on a change).
    states: HashMap<u64, State>,
    /// Unix seconds when the editor started: "this session".
    pub session_start: u64,
    pub lines: presence::AgentLines,
    // Transcript view scroll.
    pub t_scroll: usize,
    pub t_follow: bool,
    pub t_max: usize,
    /// First card shown.
    pub scroll: usize,
    // Areas from the last draw (mouse): each card's rows and run id.
    pub cards: Vec<(Rect, u64)>,
    pub rect: Rect,
    /// Notifications sent (tests count them; the terminal gets them via `notify::send`).
    pub notified: Vec<String>,
}

impl Default for Mission {
    fn default() -> Self {
        Mission {
            open: false,
            view: View::Cards,
            sel: None,
            input: None,
            confirm: None,
            ask: None,
            pending: Vec::new(),
            job: None,
            busy: None,
            live: HashMap::new(),
            live_jobs: HashMap::new(),
            states: HashMap::new(),
            session_start: now_secs(),
            lines: presence::AgentLines::default(),
            t_scroll: 0,
            t_follow: true,
            t_max: 0,
            scroll: 0,
            cards: Vec::new(),
            rect: Rect::default(),
            notified: Vec::new(),
        }
    }
}

impl Mission {
    /// Everything the keys can change, for the shortcut-helper binding test.
    #[cfg(test)]
    pub fn fingerprint(&self) -> String {
        format!(
            "{} {:?} {:?} {:?} {:?} {:?} {} {} {} {} {}",
            self.open,
            self.view,
            self.sel,
            self.input,
            self.confirm,
            self.ask,
            self.pending.len(),
            self.busy.is_some(),
            self.t_scroll,
            self.t_follow,
            self.scroll
        )
    }
}

/// A short repository name for cards: the folder's name.
pub fn repo_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

// ---- glue: mission control's side of `App` ---------------------------------------------------

impl App {
    /// The repository the editor shows (its git root, else the project folder).
    fn mission_repo(&self) -> PathBuf {
        self.dock.git.root.clone().unwrap_or_else(|| canonical(&self.project.root))
    }

    /// Repositories a new agent can work in: the editor's first, then those of earlier runs.
    /// (Track A's multi-repo workspaces add their roots here: one repo per agent.)
    pub fn mission_repos(&self) -> Vec<PathBuf> {
        let mut v = vec![self.mission_repo()];
        for r in self.agents.runs.iter().rev() {
            let h = canonical(r.home());
            if !v.contains(&h) && h.is_dir() {
                v.push(h);
            }
        }
        v
    }

    /// The cards, newest first: runs of the editor's project or repo, and any running agent.
    pub fn mission_runs(&self) -> Vec<usize> {
        let homes = [canonical(&self.project.root), self.mission_repo()];
        let mut v: Vec<usize> = (0..self.agents.runs.len())
            .filter(|&i| {
                let r = &self.agents.runs[i];
                r.running() || homes.contains(&canonical(r.home()))
            })
            .collect();
        v.sort_by_key(|&i| std::cmp::Reverse(self.agents.runs[i].id));
        v
    }

    fn mission_selected(&self) -> Option<&Run> {
        self.mission.sel.and_then(|id| self.agents.runs.iter().find(|r| r.id == id))
    }

    pub fn open_mission(&mut self) {
        let m = &mut self.mission;
        m.open = true;
        m.view = View::Cards;
        m.input = None;
        m.confirm = None;
        let runs = self.mission_runs();
        let ids: Vec<u64> = runs.iter().map(|&i| self.agents.runs[i].id).collect();
        if self.mission.sel.is_none_or(|s| !ids.contains(&s)) {
            self.mission.sel = ids.first().copied();
        }
        self.agents.probe_cli();
        self.dock.focused = false;
        self.dock.git.diff = None;
        self.agents.focus = false;
        self.project.focus_tree = false;
        self.project.close_popup();
        self.lsp.popup = None;
        self.message = Some(if ids.is_empty() { "Mission control · n starts an agent in its own worktree · Esc closes".into() } else { mission_hint(View::Cards) });
    }

    pub fn close_mission(&mut self) {
        let m = &mut self.mission;
        m.open = false;
        m.input = None;
        m.confirm = None;
        self.message = None;
    }

    /// The editor (not a tool window, popup or prompt) has the keys.
    fn editor_has_focus(&self) -> bool {
        !self.dock.has_focus() && self.prompt.is_none() && self.project.popup.is_none() && !self.project.focus_tree && !self.agents.focus && !self.git_view_focused() && !self.lsp.picker_open()
    }

    /// Keys: the ask prompt and mission control take every key while open; Ctrl+O / Alt+7 open
    /// mission control (not in the terminal, a prompt or a popup); Ctrl+U / Alt+A ask an agent
    /// from the editor. false = not ours.
    pub fn mission_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let ch = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        if self.mission.ask.is_some() {
            self.ask_key(k);
            return true;
        }
        let opener = (ctrl && !alt && ch('o')) || (alt && !ctrl && ch('7'));
        if self.mission.open {
            if ctrl && !alt && ch('q') {
                self.close_mission();
                return false; // quitting goes on to the editor
            }
            if opener && self.mission.input.is_none() && self.mission.confirm.is_none() {
                self.close_mission();
                return true;
            }
            self.mission_open_key(k);
            return true;
        }
        if opener && !self.dock.has_focus() && self.prompt.is_none() && self.project.popup.is_none() {
            self.open_mission();
            return true;
        }
        if ((ctrl && !alt && ch('u')) || (alt && !ctrl && ch('a'))) && self.editor_has_focus() {
            self.open_ask();
            return true;
        }
        false
    }

    pub fn mission_paste(&mut self, text: &str) -> bool {
        let line = text.lines().next().unwrap_or("");
        if let Some(a) = &mut self.mission.ask {
            a.text.push_str(line);
            return true;
        }
        if let Some(i) = &mut self.mission.input {
            i.text.push_str(line);
            return true;
        }
        self.mission.open
    }

    pub fn mission_mouse(&mut self, m: MouseEvent) -> bool {
        if self.mission.ask.is_some() {
            return true;
        }
        if !self.mission.open {
            return false;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.mission.cards.iter().find(|(r, _)| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom()).map(|c| c.1);
                if let Some(id) = hit {
                    self.mission.sel = Some(id);
                }
            }
            MouseEventKind::ScrollDown => self.mission_step(1),
            MouseEventKind::ScrollUp => self.mission_step(-1),
            _ => {}
        }
        true
    }

    fn mission_step(&mut self, d: isize) {
        let ids: Vec<u64> = self.mission_runs().iter().map(|&i| self.agents.runs[i].id).collect();
        if ids.is_empty() {
            return;
        }
        let cur = self.mission.sel.and_then(|s| ids.iter().position(|x| *x == s)).unwrap_or(0) as isize;
        let next = (cur + d).clamp(0, ids.len() as isize - 1) as usize;
        self.mission.sel = Some(ids[next]);
        self.mission.t_follow = true;
    }

    fn mission_open_key(&mut self, k: KeyEvent) {
        if self.mission.confirm.is_some() {
            return self.mission_confirm_key(k);
        }
        if self.mission.input.is_some() {
            return self.mission_input_key(k);
        }
        if self.mission.busy.is_some() && !matches!(k.code, KeyCode::Esc | KeyCode::Up | KeyCode::Down) {
            self.message = Some(format!("{} … one moment", self.mission.busy.clone().unwrap_or_default()));
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let plain = !ctrl && !alt;
        let view = self.mission.view;
        match k.code {
            KeyCode::Esc if view == View::Transcript => {
                self.mission.view = View::Cards;
                self.message = Some(mission_hint(View::Cards));
            }
            KeyCode::Esc => self.close_mission(),
            KeyCode::Up | KeyCode::Char('k') if plain && view == View::Transcript => self.mission_scroll(-1),
            KeyCode::Down | KeyCode::Char('j') if plain && view == View::Transcript => self.mission_scroll(1),
            KeyCode::PageUp if view == View::Transcript => self.mission_scroll(-20),
            KeyCode::PageDown if view == View::Transcript => self.mission_scroll(20),
            KeyCode::Home if view == View::Transcript => {
                self.mission.t_follow = false;
                self.mission.t_scroll = 0;
            }
            KeyCode::End if view == View::Transcript => self.mission.t_follow = true,
            KeyCode::Up | KeyCode::Char('k') if plain => self.mission_step(-1),
            KeyCode::Down | KeyCode::Char('j') if plain => self.mission_step(1),
            KeyCode::Home => self.mission_step(-10_000),
            KeyCode::End => self.mission_step(10_000),
            KeyCode::Enter if view == View::Cards => {
                if self.mission.sel.is_some() {
                    self.mission.view = View::Transcript;
                    self.mission.t_follow = true;
                    self.message = Some(mission_hint(View::Transcript));
                } else {
                    self.message = Some("No agent yet: n starts one".into());
                }
            }
            KeyCode::Char('n') if plain => {
                self.mission.input = Some(LineInput { kind: InputKind::New { worktree: true, repo: 0 }, text: String::new() });
                self.message = Some("New agent · type the task · Enter start · Tab worktree / in place · Ctrl+R repository · Esc cancel".into());
            }
            KeyCode::Char('f') if plain => self.mission_follow_up(),
            KeyCode::Char('s') if plain => self.mission_stop(),
            KeyCode::Char('c') if ctrl && !alt => self.mission_stop(),
            KeyCode::Char('d') if plain || (ctrl && !alt) => self.mission_review(),
            KeyCode::Char('l') if plain => self.mission_ask_land(),
            KeyCode::Char('w') if plain => self.mission_ask_remove(),
            KeyCode::Char('x') if plain => self.mission_ask_discard(),
            KeyCode::Delete => self.mission_forget(),
            _ => {}
        }
    }

    fn mission_scroll(&mut self, d: isize) {
        let m = &mut self.mission;
        let cur = if m.t_follow { m.t_max } else { m.t_scroll.min(m.t_max) };
        m.t_scroll = (cur as isize + d).clamp(0, m.t_max as isize) as usize;
        m.t_follow = m.t_scroll >= m.t_max;
    }

    fn mission_follow_up(&mut self) {
        let Some(r) = self.mission_selected() else {
            self.message = Some("No agent selected".into());
            return;
        };
        let why = if r.running() {
            Some("It is still working: wait, or s stops it")
        } else if r.session_id.is_none() {
            Some("That run has no session to resume: n starts a new agent")
        } else if r.landed.is_some() {
            Some("Its work has landed or was discarded: n starts a new agent")
        } else {
            None
        };
        if let Some(w) = why {
            self.message = Some(w.into());
            return;
        }
        let id = r.id;
        self.mission.input = Some(LineInput { kind: InputKind::FollowUp(id), text: String::new() });
        self.message = Some(format!("Follow-up to #{id} · Enter send · Esc cancel"));
    }

    fn mission_stop(&mut self) {
        let Some(id) = self.mission.sel else { return };
        let Some(i) = self.agents.run_index(id) else { return };
        if !self.agents.runs[i].running() {
            self.message = Some(format!("#{id} isn't running"));
            return;
        }
        self.agents.runs[i].stop();
        self.message = Some(format!("Stopping #{id}…"));
    }

    fn mission_review(&mut self) {
        let Some(id) = self.mission.sel else {
            self.message = Some("No agent selected".into());
            return;
        };
        self.close_mission();
        self.review_run(id);
        if self.git_view.open {
            self.message = Some(format!("Review of #{id} · Esc closes · Ctrl+O back to mission control"));
        }
    }

    /// The selected run's worktree, or a message why there is none.
    fn mission_worktree(&mut self, what: &str) -> Option<(u64, Worktree)> {
        let Some(r) = self.mission_selected() else {
            self.message = Some("No agent selected".into());
            return None;
        };
        let id = r.id;
        let Some(w) = r.worktree.clone() else {
            self.message = Some(format!("#{id} worked in place: nothing to {what} (d reviews its changes; commit them in the Commit panel)"));
            return None;
        };
        if r.running() {
            self.message = Some(format!("#{id} is still working: stop it first (s)"));
            return None;
        }
        if let Some(l) = &r.landed {
            if what != "remove" || !w.path.exists() {
                self.message = Some(format!("#{id}: {l}"));
                return None;
            }
        }
        Some((id, w))
    }

    fn start_job(&mut self, job: Job, busy: String, f: impl FnOnce() -> JobResult + Send + 'static) {
        self.mission.busy = Some(busy);
        self.mission.job = Some((job, crate::git::job(f)));
    }

    fn mission_ask_land(&mut self) {
        let Some((id, w)) = self.mission_worktree("land") else { return };
        self.start_job(Job::PlanLand(id), format!("Checking {}", w.branch), move || {
            let p = worktree::plan_land(&w)?;
            Ok((p.question(&w), !p.ff()))
        });
    }

    fn mission_ask_remove(&mut self) {
        let Some((id, w)) = self.mission_worktree("remove") else { return };
        if !w.path.exists() {
            self.message = Some(format!("{} is already gone", w.short()));
            return;
        }
        self.mission.confirm = Some(Confirm {
            question: format!("Remove worktree {} and its branch {} (only if merged)? Refused if it has uncommitted files. (y yes · n no)", w.short(), w.branch),
            act: Act::Remove(id),
            can_rebase: false,
        });
    }

    fn mission_ask_discard(&mut self) {
        let Some((id, w)) = self.mission_worktree("discard") else { return };
        self.start_job(Job::AskDiscard(id), format!("Checking {}", w.branch), move || Ok((worktree::discard_question(&w), false)));
    }

    /// Del: takes a finished run off the cards (and the agents panel); its Claude session stays.
    fn mission_forget(&mut self) {
        let Some(r) = self.mission_selected() else { return };
        let id = r.id;
        if r.running() {
            self.message = Some("Stop it first (s)".into());
            return;
        }
        if r.worktree.as_ref().is_some_and(|w| w.path.exists()) {
            self.message = Some("Its worktree is still there: land (l), remove (w) or discard (x) it first".into());
            return;
        }
        self.mission_step(1);
        if self.mission.sel == Some(id) {
            self.mission_step(-1);
        }
        if self.mission.sel == Some(id) {
            self.mission.sel = None;
        }
        if let Some(i) = self.agents.run_index(id) {
            self.agents.runs.remove(i);
            self.agents.save();
        }
        self.message = Some(format!("Removed #{id} from the list (its Claude session is kept)"));
    }

    fn mission_confirm_key(&mut self, k: KeyEvent) {
        let Some(c) = self.mission.confirm.clone() else { return };
        let how = match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => How::Merge,
            KeyCode::Char('b') if c.can_rebase => How::Rebase,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mission.confirm = None;
                self.message = Some("Nothing changed".into());
                return;
            }
            _ => return,
        };
        self.mission.confirm = None;
        let run = |app: &App, id: u64| app.agents.runs.iter().find(|r| r.id == id).and_then(|r| r.worktree.clone().map(|w| (w, r.title.clone())));
        match c.act {
            Act::Land(id) => {
                let Some((w, title)) = run(self, id) else { return };
                let label = format!("Landing {} in {}", w.branch, w.base_label());
                self.start_job(Job::Land(id), label, move || worktree::land(&w, &title, id, how).map(|m| (m, false)));
            }
            Act::Remove(id) => {
                let Some((w, _)) = run(self, id) else { return };
                self.start_job(Job::Remove(id), format!("Removing {}", w.short()), move || worktree::remove(&w).map(|m| (m, false)));
            }
            Act::Discard(id) => {
                let Some((w, _)) = run(self, id) else { return };
                self.start_job(Job::Discard(id), format!("Discarding {}", w.short()), move || worktree::discard(&w).map(|m| (m, false)));
            }
        }
    }

    fn mission_input_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let n_repos = self.mission_repos().len();
        let Some(inp) = &mut self.mission.input else { return };
        match k.code {
            KeyCode::Esc => {
                self.mission.input = None;
                self.message = Some(mission_hint(self.mission.view));
            }
            KeyCode::Enter => self.mission_submit(),
            KeyCode::Tab | KeyCode::BackTab => {
                if let InputKind::New { worktree, .. } = &mut inp.kind {
                    *worktree = !*worktree;
                    let w = *worktree;
                    self.message = Some(if w { "In a new worktree (its own branch; land it when it's good)".into() } else { "In place: it edits your checkout directly".into() });
                }
            }
            KeyCode::Char('r') if ctrl && !alt => {
                if let InputKind::New { repo, .. } = &mut inp.kind {
                    *repo = (*repo + 1) % n_repos.max(1);
                    let r = *repo;
                    let repos = self.mission_repos();
                    self.message = Some(match repos.get(r) {
                        Some(p) if n_repos > 1 => format!("Repository: {} ({})", repo_name(p), p.display()),
                        Some(p) => format!("Only one repository so far: {}", p.display()),
                        None => "No repository".into(),
                    });
                }
            }
            KeyCode::Char('u') if ctrl => inp.text.clear(),
            KeyCode::Backspace if ctrl || alt => {
                let t = inp.text.trim_end().to_string();
                let cut = t.rfind(' ').map_or(0, |i| i + 1);
                inp.text.truncate(cut);
            }
            KeyCode::Backspace => {
                inp.text.pop();
            }
            KeyCode::Char(c) if !ctrl && !alt => inp.text.push(c),
            _ => {}
        }
    }

    fn mission_submit(&mut self) {
        let Some(inp) = self.mission.input.clone() else { return };
        let text = inp.text.trim().to_string();
        if text.is_empty() {
            self.message = Some("Type what the agent should do first".into());
            return;
        }
        self.mission.input = None;
        match inp.kind {
            InputKind::FollowUp(id) => {
                let turn = Turn { prompt: text.clone(), shown: text, ..Turn::default() };
                let mode = self.agents.runs.iter().find(|r| r.id == id).and_then(|r| r.mode.clone());
                let turn = Turn { mode, ..turn };
                self.message = Some(match self.agents.follow_up(id, &turn) {
                    Ok(()) => {
                        self.mission.t_follow = true;
                        format!("Follow-up sent to #{id}")
                    }
                    Err(e) => e,
                });
            }
            InputKind::New { worktree, repo } => {
                let repos = self.mission_repos();
                let cwd = repos.get(repo).cloned().unwrap_or_else(|| self.mission_repo());
                self.launch(cwd, text.clone(), text, None, worktree, false);
            }
        }
    }

    fn open_ask(&mut self) {
        let d = &self.docs[self.active];
        let Some(path) = d.path.clone() else {
            self.message = Some("Save the file first: an agent needs a path to look at".into());
            return;
        };
        let sel = self.visual_range().or(d.selection());
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.project.root.clone());
        let repo = crate::git::toplevel(&dir);
        let proj = canonical(&self.project.root);
        let file = canonical(&path);
        // In place: the project folder when the file is in it, else the file's repository.
        let cwd = if file.starts_with(&proj) { proj } else { repo.clone().unwrap_or(canonical(&dir)) };
        let root = repo.unwrap_or_else(|| cwd.clone());
        let Some((label, block)) = crate::agents::file_context_sel(d, sel, &root) else { return };
        let mut a = Ask { label, block, cwd, sel: 0, text: String::new(), worktree: false };
        a.set_sel(0);
        self.mission.ask = Some(a);
        self.agents.probe_cli();
        self.message = Some("Ask an agent · type a task or pick one · Enter start · Tab worktree / in place · Esc cancel".into());
    }

    fn ask_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let Some(a) = &mut self.mission.ask else { return };
        match k.code {
            KeyCode::Esc => {
                self.mission.ask = None;
                self.message = None;
            }
            KeyCode::Up if a.text.is_empty() => a.set_sel((a.sel + PRESETS.len() - 1) % PRESETS.len()),
            KeyCode::Down if a.text.is_empty() => a.set_sel((a.sel + 1) % PRESETS.len()),
            KeyCode::Tab | KeyCode::BackTab => a.worktree = !a.worktree,
            KeyCode::Char('u') if ctrl => a.text.clear(),
            KeyCode::Backspace => {
                a.text.pop();
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                if a.text.is_empty() {
                    a.worktree = true; // a typed task is a code task unless Tab says otherwise
                }
                a.text.push(c);
            }
            KeyCode::Enter => {
                let a = self.mission.ask.take().unwrap();
                let typed = a.text.trim().to_string();
                let (shown, task) = if typed.is_empty() {
                    let (name, task, _) = PRESETS[a.sel];
                    (format!("{name} {}", a.label), task.to_string())
                } else {
                    (typed.clone(), typed)
                };
                let prompt = format!("{task}\n\n{}", a.block);
                let in_panel = !a.worktree;
                self.launch(a.cwd, shown, prompt, Some(a.label), a.worktree, in_panel);
            }
            _ => {}
        }
    }

    /// Starts an agent: in a new worktree of `cwd`'s repository (made on a thread, then the
    /// agent starts there with acceptEdits when the CLI offers it) or in place (the agents
    /// panel's mode). `show_in_panel`: select it in the agents panel (answers to read).
    pub fn launch(&mut self, cwd: PathBuf, shown: String, prompt: String, ctx: Option<String>, in_worktree: bool, show_in_panel: bool) {
        let id = self.agents.new_id();
        let title = shown.lines().next().unwrap_or("agent").to_string();
        if in_worktree {
            // Edits go to an isolated branch and land only after a question: acceptEdits is the
            // useful mode there (`-p` can't ask for permissions). Never bypassPermissions.
            self.agents.probe_cli();
            let turn = Turn { prompt, shown, ctx, mode: None, agent: None };
            let (c, t) = (cwd.clone(), title.clone());
            let rx = crate::git::job(move || {
                let w = worktree::create(&c, &t)?;
                let tree = worktree::tree_of(&w.path, "HEAD")?;
                Ok((w, tree))
            });
            self.mission.pending.push(Pending { id, title: title.clone(), repo: cwd, turn, show_in_panel, rx });
            self.mission.sel = Some(id);
            self.message = Some(format!("Agent #{id}: creating a worktree for “{}” · Ctrl+O mission control", crate::agents::transcript::ellipsize(&title, 40)));
        } else {
            let turn = Turn { prompt, shown, ctx, mode: if show_in_panel { None } else { self.agents.mode() }, agent: None };
            let run = Run::new(id, cwd, &title);
            self.agents.start_new(run, &turn);
            self.after_start(id, show_in_panel);
        }
    }

    fn after_start(&mut self, id: u64, show_in_panel: bool) {
        self.mission.sel = Some(id);
        self.mission.t_follow = true;
        if show_in_panel {
            self.agents.visible = true;
            self.agents.select(Some(id));
            self.message = Some(format!("Agent #{id} started · its answer shows in the agents panel (Alt+9) · Ctrl+O mission control"));
        } else {
            let wt = self.agents.runs.iter().find(|r| r.id == id).and_then(|r| r.worktree.as_ref().map(Worktree::short));
            self.message = Some(match wt {
                Some(w) => format!("Agent #{id} started in {w} · Ctrl+O mission control"),
                None => format!("Agent #{id} started in place · Ctrl+O mission control"),
            });
        }
    }

    /// Once per loop turn: worktrees made, jobs done, live counts, presence lines and
    /// notifications. True = redraw.
    pub fn mission_tick(&mut self) -> bool {
        let mut redraw = false;
        // Worktrees that are ready: start their agents.
        // They wait for `claude --help` too: the mode (acceptEdits) is picked from its answer.
        let mut ready = Vec::new();
        let probing = self.agents.probing();
        self.mission.pending.retain(|p| match if probing { Err(std::sync::mpsc::TryRecvError::Empty) } else { p.rx.try_recv() } {
            Ok(r) => {
                ready.push((p.id, p.title.clone(), p.turn.clone(), p.show_in_panel, r));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for (id, title, turn, show, r) in ready {
            redraw = true;
            match r {
                Ok((w, tree)) => {
                    let turn = Turn { mode: self.agents.offered_mode("acceptEdits"), ..turn };
                    let mut run = Run::new(id, w.path.clone(), &title);
                    run.base = Some(Baseline { head: Some(w.base_head.clone()), tree, at: now_secs() });
                    run.worktree = Some(w);
                    self.agents.start_new(run, &turn);
                    self.after_start(id, show);
                }
                Err(e) => self.message = Some(format!("Couldn't create a worktree for #{id}: {e}")),
            }
        }
        // Background git jobs.
        if let Some((_, rx)) = &self.mission.job {
            if let Ok(res) = rx.try_recv() {
                let (job, _) = self.mission.job.take().unwrap();
                self.mission.busy = None;
                self.job_done(job, res);
                redraw = true;
            }
        }
        // Notifications: a run that stopped working.
        let mut done = Vec::new();
        for r in &self.agents.runs {
            let before = self.mission.states.insert(r.id, r.state);
            if before == Some(State::Running) && r.state != State::Running {
                done.push(r.id);
            }
        }
        for id in done {
            self.notify_done(id);
            redraw = true;
        }
        redraw |= self.live_stats();
        let since = self.mission.session_start;
        redraw |= self.mission.lines.tick(&self.agents, &self.docs[self.active], since);
        if self.mission.open && self.agents.running_count() > 0 {
            redraw = true; // spinners, elapsed times, sparklines
        }
        redraw
    }

    fn notify_done(&mut self, id: u64) {
        let Some(r) = self.agents.runs.iter().find(|r| r.id == id) else { return };
        let act = r.activity();
        let what = match r.state {
            State::Done if matches!(act, crate::agents::run::Activity::Attention(_)) => act.label(),
            s => s.label().to_string(),
        };
        self.mission.live.remove(&id);
        let next = if r.worktree.is_some() { " · Ctrl+O: review (d) and land (l)" } else { "" };
        let text = format!("Agent #{id} “{}” {what}{next}", crate::agents::transcript::ellipsize(&r.title, 40));
        notify::send(&format!("Code Atlas: agent #{id} {what} · {}", crate::agents::transcript::ellipsize(&r.title, 60)));
        self.mission.notified.push(text.clone());
        self.message = Some(text);
    }

    /// "N files +a −d" of working agents, counted every few seconds while mission control is open.
    fn live_stats(&mut self) -> bool {
        let mut redraw = false;
        let now = Instant::now();
        for (id, (_, rx)) in self.mission.live_jobs.iter_mut() {
            if let Some(r) = rx {
                if let Ok(s) = r.try_recv() {
                    *rx = None;
                    if let Some(s) = s {
                        self.mission.live.insert(*id, s);
                        redraw = true;
                    }
                }
            }
        }
        if !self.mission.open {
            return redraw;
        }
        for r in self.agents.runs.iter().filter(|r| r.running()) {
            let Some(base) = r.base.clone() else { continue };
            let e = self.mission.live_jobs.entry(r.id).or_insert((now - LIVE_STAT_EVERY, None));
            if e.1.is_some() || now.duration_since(e.0) < LIVE_STAT_EVERY {
                continue;
            }
            let cwd = r.cwd.clone();
            e.0 = now;
            e.1 = Some(crate::git::job(move || {
                let root = crate::git::toplevel(&cwd)?;
                let (f, a, d) = crate::git::review::run_stat(&root, &base.tree).ok()?;
                Some(crate::git::patch::stat_label(f, a, d))
            }));
        }
        redraw
    }

    fn job_done(&mut self, job: Job, res: JobResult) {
        match (job, res) {
            (Job::PlanLand(id), Ok((q, can_rebase))) => self.mission.confirm = Some(Confirm { question: q, act: Act::Land(id), can_rebase }),
            (Job::PlanLand(id), Err(e)) => self.message = Some(format!("Can't land #{id}: {e}")),
            (Job::AskDiscard(id), Ok((q, _))) => self.mission.confirm = Some(Confirm { question: q, act: Act::Discard(id), can_rebase: false }),
            (Job::Land(id), Ok((msg, _))) => {
                let base = self.agents.runs.iter().find(|r| r.id == id).and_then(|r| r.worktree.as_ref().map(Worktree::base_label)).unwrap_or_default();
                self.set_landed(id, format!("landed in {base}"));
                self.reload_clean_docs();
                self.dock.git.refresh();
                let w = self.agents.runs.iter().find(|r| r.id == id).and_then(|r| r.worktree.clone());
                if let Some(w) = w {
                    self.mission.confirm = Some(Confirm {
                        question: format!("{msg}. Remove its worktree {} and branch {} now? (y yes · n keep them)", w.short(), w.branch),
                        act: Act::Remove(id),
                        can_rebase: false,
                    });
                }
                self.message = Some(msg);
            }
            (Job::Remove(_), Ok((msg, _))) => self.message = Some(msg),
            (Job::Discard(id), Ok((msg, _))) => {
                self.set_landed(id, "discarded".into());
                self.message = Some(msg);
            }
            (Job::Land(id), Err(e)) => self.message = Some(format!("Couldn't land #{id}: {e}")),
            (Job::Remove(id), Err(e)) => self.message = Some(format!("Couldn't remove #{id}'s worktree: {e}")),
            (Job::Discard(id), Err(e)) => self.message = Some(format!("Couldn't discard #{id}: {e}")),
            (Job::AskDiscard(id), Err(e)) => self.message = Some(format!("Can't discard #{id}: {e}")),
        }
    }

    fn set_landed(&mut self, id: u64, what: String) {
        if let Some(i) = self.agents.run_index(id) {
            self.agents.runs[i].landed = Some(what);
            self.agents.save();
        }
    }

    /// After landing: open files without unsaved edits take the new disk version.
    fn reload_clean_docs(&mut self) {
        for d in &mut self.docs {
            let Some(p) = d.path.clone() else { continue };
            let disk = std::fs::read_to_string(&p).ok();
            if crate::agents::decide(d.dirty(), disk.as_deref(), &d.rope.to_string()) == crate::agents::Reload::Reload {
                let _ = crate::agents::reload_doc(d);
            }
        }
    }
}

pub fn mission_hint(v: View) -> String {
    match v {
        View::Cards => "Mission control · ↑↓ agent · Enter transcript · n new · f follow-up · d review · l land · x discard · Esc close".into(),
        View::Transcript => "Transcript · ↑↓ PgUp PgDn scroll · End follow · f follow-up · d review · l land · Esc back".into(),
    }
}

#[cfg(test)]
mod tests;
