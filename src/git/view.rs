//! The Git view (Ctrl+L, Alt+8): a full-height tool window over the editor area with two tabs,
//! **Changes** (the review: working tree, or one agent run) and **Log** (graph, commit details),
//! plus the branches popup, push / pull / fetch behind a confirmation, and blame in the gutter.
//! Keys are single letters (it has no text to type into, like `tig` or `lazygit`), so they work
//! in any terminal; the opening key Ctrl+L needs no Option-as-Meta on a Mac.
//!
//! Hooks elsewhere: `App.git_view`; `git_view_key` / `git_view_mouse` in `App::handle` (after
//! the dock, before the agents panel); `git_view_tick` from `dock_tick`; `view_ui::draw` and the
//! blame gutter in `ui.rs`; Ctrl+D in the agents panel calls `review_run`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::blame::Blames;
use super::branches::{self, Branches, RemoteOp};
use super::log::Log;
use super::review::{self, Review, RevertOp, Scope, Undo};
use super::{job, run};
use crate::app::App;
use crate::project::{canonical, Target};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Changes,
    Log,
}

/// What a confirmed question does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Revert(RevertOp),
    Remote(RemoteOp),
    Switch(String),
    DeleteBranch(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    pub question: String,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// The Log filter (words, `author:`, `path:`).
    Filter,
    NewBranch,
    Rename(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub kind: InputKind,
    pub text: String,
}

impl InputKind {
    pub fn label(&self) -> String {
        match self {
            InputKind::Filter => "Filter (words, author:name, path:file)".into(),
            InputKind::NewBranch => "New branch from HEAD".into(),
            InputKind::Rename(b) => format!("Rename {b} to"),
        }
    }
}

pub struct GitView {
    pub open: bool,
    pub tab: Tab,
    pub review: Review,
    /// Commit details over the Log list (Enter on a commit).
    pub details: Option<Review>,
    pub log: Log,
    pub branches: Option<Branches>,
    pub confirm: Option<Confirm>,
    pub input: Option<Input>,
    /// Reverts that `u` can take back, newest last.
    pub undo: Vec<Undo>,
    pub blames: Blames,
    remote_job: Option<Receiver<Result<String, String>>>,
    /// What runs on the remote right now ("Pushing main …").
    pub remote_busy: Option<String>,
    /// Side by side: `None` = by width (wide terminals), else the user's choice.
    pub sbs_pref: Option<bool>,
    /// Repository root while open.
    pub root: Option<PathBuf>,
    /// The editor's file when the view opened (b = blame it, h = its history), relative.
    pub origin: Option<String>,
    origin_abs: Option<PathBuf>,
    /// Relative path → the newest agent run that edited it (shown on file headers).
    pub agent_marks: HashMap<String, String>,
    /// A commit to select (and show) once the log has loaded.
    pending_sha: Option<String>,
    // Areas from the last draw (mouse).
    pub rect: Rect,
    pub list_rect: Rect,
    pub doc_rect: Rect,
    pub list_scroll: usize,
}

impl Default for GitView {
    fn default() -> Self {
        GitView {
            open: false,
            tab: Tab::Changes,
            review: Review::new(Scope::Worktree),
            details: None,
            log: Log::default(),
            branches: None,
            confirm: None,
            input: None,
            undo: Vec::new(),
            blames: Blames::default(),
            remote_job: None,
            remote_busy: None,
            sbs_pref: None,
            root: None,
            origin: None,
            origin_abs: None,
            agent_marks: HashMap::new(),
            pending_sha: None,
            rect: Rect::default(),
            list_rect: Rect::default(),
            doc_rect: Rect::default(),
            list_scroll: 0,
        }
    }
}

impl GitView {
    /// The review the keys act on: commit details on the Log tab, else the Changes review.
    pub fn active_review(&mut self) -> Option<&mut Review> {
        match self.tab {
            Tab::Changes => Some(&mut self.review),
            Tab::Log => self.details.as_mut(),
        }
    }

    /// Side by side for a document this wide?
    pub fn sbs_for(&self, width: u16) -> bool {
        width >= 60 && self.sbs_pref.unwrap_or(width >= 150)
    }

    /// The scope in a few characters (narrow title bars).
    pub fn scope_short(&self) -> String {
        match &self.review.scope {
            Scope::Worktree => "worktree".into(),
            Scope::Run { id, .. } => format!("run #{id}"),
            Scope::Commit { sha } => sha.get(..7).unwrap_or(sha).to_string(),
        }
    }

    pub fn scope_label(&self) -> String {
        match &self.review.scope {
            Scope::Worktree => "working tree".into(),
            Scope::Run { id, title, only_touched, .. } => {
                format!("agent run #{id} “{}”{}", crate::agents::transcript::ellipsize(title, 28), if *only_touched { " · its edited files" } else { "" })
            }
            Scope::Commit { sha } => format!("commit {}", sha.get(..7).unwrap_or(sha)),
        }
    }
}

/// `p` relative to the repository root (forward slashes). A file that is gone (an agent deleted
/// it) is resolved through its folder, so /var vs /private/var still match on a Mac.
fn rel_in(root: &Path, p: &Path) -> Option<String> {
    let abs = if p.exists() { canonical(p) } else { canonical(p.parent()?).join(p.file_name()?) };
    abs.strip_prefix(root).ok().map(|x| x.to_string_lossy().replace('\\', "/"))
}

// ---- glue: the view's side of `App` ----------------------------------------------------------

impl App {
    /// The Git view gets the keys: open, and nothing else (tree, panel, dock, popup) has focus.
    pub fn git_view_focused(&self) -> bool {
        self.git_view.open && !self.dock.focused && !self.agents.focus && !self.project.focus_tree && self.project.popup.is_none() && self.prompt.is_none() && self.dock.git.diff.is_none()
    }

    fn git_root(&self) -> Option<PathBuf> {
        if let Some(r) = self.dock.git.root.clone() {
            return Some(r);
        }
        super::toplevel(&self.project.root)
    }

    /// Opens (or focuses) the view; `scope` replaces the review's scope when given.
    pub fn open_git_view(&mut self, scope: Option<Scope>) {
        let Some(root) = self.git_root() else {
            self.message = Some("Not a git repository (git init to start one)".into());
            return;
        };
        let v = &mut self.git_view;
        if v.root.as_ref() != Some(&root) {
            v.review.set_scope(Scope::Worktree);
            v.log = Log::default();
            v.details = None;
        }
        v.root = Some(root.clone());
        if !v.open {
            // Remember the editor's file (for b / h), whatever the view shows.
            let d = &self.docs[self.active];
            v.origin_abs = d.path.as_deref().map(canonical);
            v.origin = v.origin_abs.as_ref().and_then(|p| p.strip_prefix(&root).ok()).map(|p| p.to_string_lossy().replace('\\', "/"));
        }
        v.open = true;
        if let Some(s) = scope {
            if v.review.scope != s {
                v.review.set_scope(s);
            }
            v.tab = Tab::Changes;
        }
        v.review.refresh();
        self.dock.focused = false;
        self.dock.git.diff = None;
        self.agents.focus = false;
        self.project.focus_tree = false;
        self.project.close_popup();
        self.lsp.popup = None;
        self.update_agent_marks();
        self.message = Some(self.git_view_hint());
    }

    pub fn close_git_view(&mut self) {
        let v = &mut self.git_view;
        v.open = false;
        v.confirm = None;
        v.input = None;
        v.branches = None;
        self.message = None;
    }

    fn git_view_hint(&self) -> String {
        let v = &self.git_view;
        // The view's footer lists the keys; the status bar says what is shown.
        match v.tab {
            Tab::Changes => format!("Review: {}", v.scope_label()),
            Tab::Log if v.details.is_some() => "Commit details · Esc back to the log".into(),
            Tab::Log => match &v.log.path {
                Some(p) => format!("Log: history of {p}"),
                None => "Log: all branches".into(),
            },
        }
    }

    /// Ctrl+D in the agents panel: review what the selected run changed.
    pub fn review_run(&mut self, id: u64) {
        let Some(r) = self.agents.runs.iter().find(|r| r.id == id) else { return };
        let Some(base) = r.base.clone() else {
            self.message = Some("This run has no snapshot to compare with (started before reviews existed, or outside a git repository)".into());
            return;
        };
        let Some(root) = self.git_root() else {
            self.message = Some("Not a git repository".into());
            return;
        };
        let touched = r.touched.iter().filter_map(|p| rel_in(&root, p)).collect();
        let scope = Scope::Run { id, title: r.title.clone(), base: base.tree, since: base.at, touched, only_touched: false };
        self.open_git_view(Some(scope));
    }

    /// This project's runs that can be reviewed, newest first.
    fn reviewable_runs(&self) -> Vec<u64> {
        let root = canonical(&self.project.root);
        let mut v: Vec<&crate::agents::run::Run> = self.agents.runs.iter().filter(|r| r.base.is_some() && canonical(&r.cwd) == root).collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.id));
        v.into_iter().map(|r| r.id).collect()
    }

    fn update_agent_marks(&mut self) {
        let Some(root) = self.git_view.root.clone() else { return };
        let proj = canonical(&self.project.root);
        let mut marks = HashMap::new();
        let mut runs: Vec<&crate::agents::run::Run> = self.agents.runs.iter().filter(|r| canonical(&r.cwd) == proj).collect();
        runs.sort_by_key(|r| r.id);
        for r in runs {
            for p in &r.touched {
                if let Some(rel) = rel_in(&root, p) {
                    marks.insert(rel, format!("#{} {}", r.id, crate::agents::transcript::ellipsize(&r.title, 24)));
                }
            }
        }
        self.git_view.agent_marks = marks;
    }

    /// Called from `dock_tick`: refreshes, loads and finished remote jobs. True = redraw.
    pub fn git_view_tick(&mut self) -> bool {
        let mut redraw = false;
        let root = self.git_view.root.clone().or_else(|| self.dock.git.root.clone());
        if let Some(root) = &root {
            redraw |= self.git_view.blames.tick(root, &self.docs[self.active]);
        }
        let v = &mut self.git_view;
        if let Some(rx) = &v.remote_job {
            if let Ok(r) = rx.try_recv() {
                v.remote_job = None;
                v.remote_busy = None;
                self.message = Some(match r {
                    Ok(m) => m,
                    Err(e) => format!("Couldn't {}", e.lines().last().unwrap_or(&e)),
                });
                v.log.loaded = false;
                v.review.refresh();
                self.dock.git.refresh();
                // A pull may have changed open files.
                self.reload_open_docs();
                redraw = true;
            }
        }
        let v = &mut self.git_view;
        if !v.open {
            return redraw;
        }
        let Some(root) = v.root.clone() else { return redraw };
        match v.tab {
            Tab::Changes => {
                if v.review.tick(&root) {
                    redraw = true;
                    self.update_agent_marks();
                }
            }
            Tab::Log => {
                if !v.log.loaded && !v.log.loading {
                    let path = v.log.path.clone();
                    v.log.reload(&root, path);
                }
                if v.log.poll() {
                    redraw = true;
                    if let Some(sha) = v.pending_sha.take() {
                        if !v.log.select_sha(&sha) {
                            self.message = Some(format!("{} isn't in the loaded log", sha.get(..7).unwrap_or(&sha)));
                        }
                    }
                }
                let v = &mut self.git_view;
                if let Some(d) = &mut v.details {
                    redraw |= d.tick(&root);
                }
            }
        }
        redraw
    }

    /// Keys for the Git view: Alt+8 / Ctrl+L anywhere (not in the terminal), then everything
    /// while it has the focus. false = not ours.
    pub fn git_view_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ch = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        let opener = (alt && !ctrl && ch('8')) || (ctrl && !alt && !shift && ch('l'));
        if opener && self.prompt.is_none() {
            if self.git_view_focused() {
                self.close_git_view();
            } else if !self.git_view.open && ctrl && self.blame_jump() {
                // Ctrl+L on a blamed line: that line's commit.
            } else {
                self.open_git_view(None);
            }
            return true;
        }
        if !self.git_view_focused() {
            return false;
        }
        // Tool windows and quitting pass on.
        if (alt && !ctrl && (ch('1') || ch('9') || ch('0'))) || (ctrl && !alt && ch('q')) {
            if ch('q') {
                self.close_git_view();
            }
            return false;
        }
        if self.git_view.confirm.is_some() {
            self.confirm_key(k);
            return true;
        }
        if self.git_view.input.is_some() {
            self.input_key(k);
            return true;
        }
        if self.git_view.branches.is_some() {
            self.branches_key(k);
            return true;
        }
        // Keys every tab has.
        match k.code {
            KeyCode::Char('1') if !ctrl && !alt => {
                self.git_view.tab = Tab::Changes;
                self.git_view.review.refresh();
                self.message = Some(self.git_view_hint());
                return true;
            }
            KeyCode::Char('2') if !ctrl && !alt => {
                self.git_view.tab = Tab::Log;
                self.message = Some(self.git_view_hint());
                return true;
            }
            KeyCode::Char('B') if !ctrl && !alt => {
                if let Some(root) = self.git_view.root.clone() {
                    let b = Branches::open(&root);
                    self.message = b.error.clone().or(Some("Branches · Enter switch · n new · r rename · d delete (merged only) · Esc close".into()));
                    self.git_view.branches = Some(b);
                }
                return true;
            }
            KeyCode::Char(c @ ('P' | 'U' | 'F')) if !ctrl && !alt => {
                self.ask_remote(c);
                return true;
            }
            KeyCode::Char('b') if !ctrl && !alt => {
                self.toggle_blame();
                return true;
            }
            KeyCode::Char('h') if !ctrl && !alt => {
                self.file_history();
                return true;
            }
            KeyCode::F(5) => {
                self.git_view.review.refresh();
                self.git_view.log.loaded = false;
                self.message = Some("Refreshing…".into());
                return true;
            }
            _ => {}
        }
        match self.git_view.tab {
            Tab::Changes => self.review_key(k, false),
            Tab::Log if self.git_view.details.is_some() => self.review_key(k, true),
            Tab::Log => self.log_key(k),
        }
        true
    }

    /// Keys on a review document (the Changes tab, or commit details when `details`).
    fn review_key(&mut self, k: KeyEvent, details: bool) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let Some(r) = self.git_view.active_review() else { return };
        let page = r.view_h.saturating_sub(2).max(1) as isize;
        let mut msg: Option<String> = None;
        match k.code {
            KeyCode::Esc => {
                if details {
                    self.git_view.details = None;
                    self.message = Some(self.git_view_hint());
                } else {
                    self.close_git_view();
                }
                return;
            }
            KeyCode::Up if alt => {
                r.next_file(true);
            }
            KeyCode::Down if alt => {
                r.next_file(false);
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => r.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => r.move_by(1),
            KeyCode::PageUp => r.move_by(-page),
            KeyCode::PageDown | KeyCode::Char(' ') => r.move_by(page),
            KeyCode::Home | KeyCode::Char('g') => r.move_to(0),
            KeyCode::End | KeyCode::Char('G') => r.move_to(usize::MAX),
            KeyCode::F(7) | KeyCode::Char('n') | KeyCode::Char('p') | KeyCode::Char('N') => {
                let back = (k.code == KeyCode::F(7) && shift) || matches!(k.code, KeyCode::Char('p') | KeyCode::Char('N'));
                if !r.next_hunk(back) {
                    msg = Some(if back { "First change" } else { "Last change" }.into());
                }
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Char(']') | KeyCode::Char('[') => {
                let back = matches!(k.code, KeyCode::BackTab | KeyCode::Char('['));
                if !r.next_file(back) {
                    msg = Some(if back { "First file" } else { "Last file" }.into());
                }
            }
            KeyCode::Enter | KeyCode::F(4) => {
                self.open_review_source();
                return;
            }
            KeyCode::Char('v') if !ctrl => {
                let w = self.git_view.doc_rect.width;
                let now = self.git_view.sbs_for(w);
                self.git_view.sbs_pref = Some(!now);
                msg = Some(if !now && w < 60 { "Too narrow for side by side (60+ columns)".into() } else if !now { "Side by side".into() } else { "Unified".into() });
            }
            KeyCode::Char('y') | KeyCode::Char('c') if details && !ctrl => {
                if let Scope::Commit { sha } = &r.scope {
                    crate::app::set_system_clipboard(sha);
                    msg = Some(format!("Copied {sha}"));
                }
            }
            _ if details => {}
            KeyCode::Char('s') | KeyCode::Char('S') if !ctrl && !alt => self.stage_at_cursor(k.code == KeyCode::Char('S')),
            KeyCode::Char('r') | KeyCode::Char('R') if !ctrl && !alt => self.ask_revert(k.code == KeyCode::Char('R')),
            KeyCode::Char('u') if !ctrl && !alt => self.undo_revert(),
            KeyCode::Char('z') if ctrl => self.undo_revert(),
            KeyCode::Char('a') if !ctrl && !alt => self.cycle_scope(),
            KeyCode::Char('t') if !ctrl && !alt => {
                if let Scope::Run { only_touched, .. } = &mut r.scope {
                    *only_touched = !*only_touched;
                    let s = r.scope.clone();
                    r.set_scope(s);
                    msg = Some(self.git_view_hint());
                } else {
                    msg = Some("t works in an agent run's review (a picks one)".into());
                }
            }
            _ => {}
        }
        if let Some(m) = msg {
            self.message = Some(m);
        }
    }

    fn open_review_source(&mut self) {
        let Some(root) = self.git_view.root.clone() else { return };
        let Some((path, line)) = self.git_view.active_review().and_then(|r| r.source_at_cursor()) else { return };
        let abs = root.join(&path);
        if !abs.exists() {
            self.message = Some(format!("{path} isn't in the working tree"));
            return;
        }
        self.close_git_view();
        self.open_target(Target { path: abs, at: Some((line.saturating_sub(1), 0, 0)) });
    }

    fn stage_at_cursor(&mut self, whole: bool) {
        let Some(root) = self.git_view.root.clone() else { return };
        let r = &mut self.git_view.review;
        if !r.loaded {
            self.message = Some("Still reading the changes…".into());
            return;
        }
        let res = review::stage_op(r, whole).and_then(|(op, msg)| review::do_stage(&root, &op).map(|_| msg));
        self.message = Some(match res {
            Ok(m) => {
                r.refresh();
                self.dock.git.refresh();
                format!("{m} · Ctrl+K commits the staged hunks")
            }
            Err(e) => format!("Couldn't stage: {e}"),
        });
    }

    fn ask_revert(&mut self, whole: bool) {
        let r = &self.git_view.review;
        if !r.loaded {
            self.message = Some("Still reading the changes…".into());
            return;
        }
        match review::revert_op(r, whole) {
            Ok((op, q)) => {
                self.message = Some(format!("{q}  y = yes · n = no"));
                self.git_view.confirm = Some(Confirm { question: q, action: Action::Revert(op) });
            }
            Err(e) => self.message = Some(e),
        }
    }

    /// A doc of `rel` with unsaved edits (a revert under it would be lost or clash).
    fn dirty_doc(&self, root: &Path, rel: &str) -> Option<String> {
        let abs = root.join(rel);
        self.docs.iter().find(|d| d.dirty() && d.path.as_deref().is_some_and(|p| canonical(p) == abs)).map(|d| d.name())
    }

    /// Reloads open files git changed on disk (clean ones; dirty ones keep their edits).
    fn reload_open_docs(&mut self) {
        for d in &mut self.docs {
            let Some(p) = d.path.clone() else { continue };
            let disk = std::fs::read_to_string(&p).ok();
            if crate::agents::decide(d.dirty(), disk.as_deref(), &d.rope.to_string()) == crate::agents::Reload::Reload {
                let _ = crate::agents::reload_doc(d);
            }
        }
    }

    fn undo_revert(&mut self) {
        let Some(root) = self.git_view.root.clone() else { return };
        let Some(u) = self.git_view.undo.last().cloned() else {
            self.message = Some("Nothing to undo (u takes back reverts made here)".into());
            return;
        };
        if let Some(name) = self.dirty_doc(&root, &u.path) {
            self.message = Some(format!("{name} has unsaved edits: save or undo them first"));
            return;
        }
        self.message = Some(match review::undo(&root, &u) {
            Ok(()) => {
                self.git_view.undo.pop();
                self.reload_open_docs();
                format!("Undone: {} is back as it was", u.path)
            }
            Err(e) => {
                self.git_view.undo.pop();
                format!("Couldn't undo: {e}")
            }
        });
        self.git_view.review.refresh();
        self.dock.git.refresh();
    }

    fn cycle_scope(&mut self) {
        let runs = self.reviewable_runs();
        let next = match &self.git_view.review.scope {
            Scope::Run { id, .. } => runs.iter().position(|r| r == id).and_then(|i| runs.get(i + 1)).copied(),
            _ => runs.first().copied(),
        };
        match next {
            Some(id) => self.review_run(id),
            None => {
                let had_runs = !runs.is_empty();
                self.git_view.review.set_scope(Scope::Worktree);
                self.message = Some(if had_runs {
                    "Review · working tree (a: agent runs)".into()
                } else {
                    "No agent runs to review yet: runs started in the agents panel (Alt+9) are snapshotted".into()
                });
            }
        }
    }

    fn ask_remote(&mut self, c: char) {
        let Some(root) = self.git_view.root.clone() else { return };
        if self.git_view.remote_busy.is_some() {
            self.message = Some("A remote action is still running".into());
            return;
        }
        let info = branches::remote_info(&root);
        let op = match c {
            'P' => branches::push_op(&info),
            'U' => branches::pull_op(&info),
            _ => branches::fetch_op(&info),
        };
        match op {
            Ok(op) => {
                self.message = Some(format!("{}  y = yes · n = no", op.question));
                self.git_view.confirm = Some(Confirm { question: op.question.clone(), action: Action::Remote(op) });
            }
            Err(e) => self.message = Some(e),
        }
    }

    /// y / Enter: do it; n / Esc: don't; other keys keep the question (a stray key neither
    /// confirms nor silently drops it).
    fn confirm_key(&mut self, k: KeyEvent) {
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let yes = plain && matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter);
        let no = k.code == KeyCode::Esc || (plain && matches!(k.code, KeyCode::Char('n') | KeyCode::Char('N')));
        if !yes && !no {
            self.message = Some(format!("{}  y = yes · n = no", self.git_view.confirm.as_ref().map_or("", |c| c.question.as_str())));
            return;
        }
        let Some(c) = self.git_view.confirm.take() else { return };
        if no {
            self.message = Some("Cancelled".into());
            return;
        }
        let Some(root) = self.git_view.root.clone() else { return };
        match c.action {
            Action::Revert(op) => {
                if let Some(name) = self.dirty_doc(&root, op.path()) {
                    self.message = Some(format!("{name} has unsaved edits: save or undo them first"));
                    return;
                }
                self.message = Some(match review::do_revert(&root, &op) {
                    Ok(u) => {
                        self.git_view.undo.push(u);
                        self.reload_open_docs();
                        format!("Reverted in {} · u undoes", op.path())
                    }
                    Err(e) => format!("Couldn't revert: {e}"),
                });
                self.git_view.review.refresh();
                self.dock.git.refresh();
            }
            Action::Remote(op) => {
                let label = match op.args[0].as_str() {
                    "push" => "Pushing…",
                    "pull" => "Pulling…",
                    _ => "Fetching…",
                };
                self.git_view.remote_busy = Some(label.into());
                self.message = Some(label.into());
                self.git_view.remote_job = Some(job(move || branches::run_remote(&root, &op)));
            }
            Action::Switch(name) => {
                self.message = Some(match run(&root, &["switch", "-q", &name]) {
                    Ok(_) => {
                        self.reload_open_docs();
                        format!("Switched to {name}")
                    }
                    Err(e) => format!("Couldn't switch: {}", e.lines().next().unwrap_or(&e)),
                });
                self.after_branch_change(&root);
            }
            Action::DeleteBranch(name) => {
                self.message = Some(match run(&root, &["branch", "-d", &name]) {
                    Ok(_) => format!("Deleted branch {name}"),
                    Err(e) => format!("Couldn't delete: {}", e.lines().next().unwrap_or(&e)),
                });
                self.after_branch_change(&root);
            }
        }
    }

    fn after_branch_change(&mut self, root: &Path) {
        let v = &mut self.git_view;
        if v.branches.is_some() {
            let sel = v.branches.as_ref().map_or(0, |b| b.sel);
            let mut b = Branches::open(root);
            b.sel = sel.min(b.list.len().saturating_sub(1));
            v.branches = Some(b);
        }
        v.log.loaded = false;
        v.review.refresh();
        self.dock.git.refresh();
    }

    fn branches_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let Some(b) = self.git_view.branches.as_mut() else { return };
        match k.code {
            KeyCode::Esc => {
                self.git_view.branches = None;
                self.message = Some(self.git_view_hint());
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => b.step(-1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => b.step(1),
            KeyCode::Home => b.step(-10_000),
            KeyCode::End => b.step(10_000),
            KeyCode::Enter => {
                let Some(br) = b.selected().cloned() else { return };
                if br.current {
                    self.message = Some(format!("Already on {}", br.name));
                    return;
                }
                let changed = self.dock.git.entries.iter().filter(|e| e.kind() != super::status::Kind::Untracked).count();
                if changed == 0 {
                    self.git_view.confirm = Some(Confirm { question: format!("Switch to {}?", br.name), action: Action::Switch(br.name.clone()) });
                    self.message = Some(format!("Switch to {}?  y = yes · n = no", br.name));
                } else {
                    let q = format!("Switch to {} with {changed} changed file(s)? They come along; git refuses if they'd be overwritten", br.name);
                    self.message = Some(format!("{q}  y = yes · n = no"));
                    self.git_view.confirm = Some(Confirm { question: q, action: Action::Switch(br.name) });
                }
            }
            KeyCode::Char('n') if !ctrl => {
                self.git_view.input = Some(Input { kind: InputKind::NewBranch, text: String::new() });
                self.message = Some("New branch from HEAD: type a name, Enter creates and switches, Esc cancels".into());
            }
            KeyCode::Char('r') if !ctrl => {
                if let Some(br) = b.selected().cloned() {
                    self.git_view.input = Some(Input { kind: InputKind::Rename(br.name.clone()), text: br.name });
                    self.message = Some("Rename: Enter renames, Esc cancels".into());
                }
            }
            KeyCode::Char('d') if !ctrl => {
                let Some(br) = b.selected().cloned() else { return };
                if br.current {
                    self.message = Some("Can't delete the branch you're on".into());
                    return;
                }
                let q = format!("Delete branch {}? (only if it is merged: git branch -d)", br.name);
                self.message = Some(format!("{q}  y = yes · n = no"));
                self.git_view.confirm = Some(Confirm { question: q, action: Action::DeleteBranch(br.name) });
            }
            _ => {}
        }
    }

    fn input_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let Some(root) = self.git_view.root.clone() else { return };
        let Some(inp) = self.git_view.input.as_mut() else { return };
        match k.code {
            KeyCode::Esc => {
                let was = self.git_view.input.take().unwrap();
                if was.kind == InputKind::Filter {
                    // Leave the filter as typed; Esc only ends editing it.
                    self.message = Some(self.git_view_hint());
                } else {
                    self.message = Some("Cancelled".into());
                }
                return;
            }
            KeyCode::Enter => {
                let inp = self.git_view.input.take().unwrap();
                self.submit_input(&root, inp);
                return;
            }
            KeyCode::Backspace if ctrl || alt => {
                while inp.text.ends_with(' ') {
                    inp.text.pop();
                }
                while inp.text.ends_with(|c: char| !c.is_whitespace()) {
                    inp.text.pop();
                }
            }
            KeyCode::Char('h') if ctrl => {
                inp.text.pop();
            }
            KeyCode::Backspace => {
                inp.text.pop();
            }
            KeyCode::Char('u') if ctrl => inp.text.clear(),
            KeyCode::Char(c) if !ctrl && !alt => inp.text.push(c),
            _ => return,
        }
        if inp.kind == InputKind::Filter {
            let words = inp.text.clone();
            let log = &mut self.git_view.log;
            log.filter = words;
            log.apply_filter();
        }
    }

    fn submit_input(&mut self, root: &Path, inp: Input) {
        match inp.kind {
            InputKind::Filter => {
                let f = super::log::parse_filter(&inp.text);
                let log = &mut self.git_view.log;
                log.filter = inp.text;
                if f.path != log.path {
                    log.reload(root, f.path);
                }
                log.apply_filter();
                self.message = Some(format!("{} of {} commits", log.shown.len(), log.commits.len()));
            }
            InputKind::NewBranch => {
                let name = inp.text.trim().to_string();
                self.message = Some(match branches::valid_name(root, &name).and_then(|_| run(root, &["switch", "-q", "-c", &name])) {
                    Ok(_) => format!("Created and switched to {name}"),
                    Err(e) => format!("Couldn't create the branch: {}", e.lines().next().unwrap_or(&e)),
                });
                self.after_branch_change(root);
            }
            InputKind::Rename(old) => {
                let name = inp.text.trim().to_string();
                if name == old {
                    self.message = Some("Same name: nothing to do".into());
                    return;
                }
                self.message = Some(match branches::valid_name(root, &name).and_then(|_| run(root, &["branch", "-m", &old, &name])) {
                    Ok(_) => format!("Renamed {old} to {name}"),
                    Err(e) => format!("Couldn't rename: {}", e.lines().next().unwrap_or(&e)),
                });
                self.after_branch_change(root);
            }
        }
    }

    fn log_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let log = &mut self.git_view.log;
        let page = log.view_h.saturating_sub(1).max(1) as isize;
        match k.code {
            KeyCode::Esc => self.close_git_view(),
            KeyCode::Up | KeyCode::Char('k') if !ctrl => log.step(-1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => log.step(1),
            KeyCode::PageUp => log.step(-page),
            KeyCode::PageDown | KeyCode::Char(' ') => log.step(page),
            KeyCode::Home | KeyCode::Char('g') => log.step(-1_000_000),
            KeyCode::End | KeyCode::Char('G') => log.step(1_000_000),
            KeyCode::Enter | KeyCode::F(4) => {
                if let Some(c) = log.selected() {
                    let mut r = Review::new(Scope::Commit { sha: c.sha.clone() });
                    r.from_log = true;
                    self.git_view.details = Some(r);
                    self.message = Some(self.git_view_hint());
                }
            }
            KeyCode::Char('/') => {
                let text = log.filter.clone();
                self.git_view.input = Some(Input { kind: InputKind::Filter, text });
                self.message = Some("Filter: words match subject / hash / author / branch; author:name; path:file (Enter). Esc ends".into());
            }
            KeyCode::Char('y') | KeyCode::Char('c') if !ctrl => {
                if let Some(c) = log.selected() {
                    let sha = c.sha.clone();
                    crate::app::set_system_clipboard(&sha);
                    self.message = Some(format!("Copied {sha}"));
                }
            }
            _ => {}
        }
    }

    /// `b`: blame for the editor's file (from the view), closing the view to show it.
    fn toggle_blame(&mut self) {
        let Some(abs) = self.git_view.origin_abs.clone() else {
            self.message = Some("Open a file first: b blames the editor's file".into());
            return;
        };
        let Some(d) = self.docs.iter().find(|d| d.path.as_deref().is_some_and(|p| canonical(p) == abs)) else { return };
        let Some(path) = d.path.clone() else { return };
        let on = self.git_view.blames.toggle(&path);
        self.close_git_view();
        self.message = Some(if on { "Blame on: who changed each line, when (✳ = with Claude) · Ctrl+L on a line shows its commit · Ctrl+L, b turns it off".into() } else { "Blame off".into() });
    }

    /// `h`: the Log tab with the editor file's history (again: all branches).
    fn file_history(&mut self) {
        let Some(root) = self.git_view.root.clone() else { return };
        let v = &mut self.git_view;
        let Some(rel) = v.origin.clone() else {
            self.message = Some("Open a file first: h shows the editor file's history".into());
            return;
        };
        v.tab = Tab::Log;
        v.details = None;
        let path = if v.log.path.as_deref() == Some(rel.as_str()) { None } else { Some(rel.clone()) };
        v.log.filter = path.as_ref().map(|p| format!("path:{p}")).unwrap_or_default();
        v.log.reload(&root, path.clone());
        self.message = Some(match path {
            Some(p) => format!("History of {p} (h again: all branches)"),
            None => "Log · all branches".into(),
        });
    }

    /// Ctrl+L with blame on: opens the commit of the cursor line. false = not applicable.
    fn blame_jump(&mut self) -> bool {
        let d = &self.docs[self.active];
        let Some(b) = self.git_view.blames.get(d.path.as_deref()) else { return false };
        let Some(info) = b.at(d.line()) else { return false };
        if info.uncommitted() {
            self.message = Some("This line isn't committed yet".into());
            return true;
        }
        let sha = info.sha.clone();
        self.open_git_view(None);
        if !self.git_view.open {
            return true;
        }
        let v = &mut self.git_view;
        v.tab = Tab::Log;
        if !v.log.select_sha(&sha) {
            v.pending_sha = Some(sha.clone());
            if !v.log.loading && v.log.path.is_some() {
                // A file history is loaded: go back to all branches to find it.
                v.log.filter.clear();
                let root = v.root.clone().unwrap();
                v.log.reload(&root, None);
            }
        }
        let mut r = Review::new(Scope::Commit { sha });
        r.from_log = true;
        v.details = Some(r);
        self.message = Some(self.git_view_hint());
        true
    }

    /// A paste goes into the input line; otherwise the view swallows it (the editor under it
    /// is hidden, text must not land there unseen).
    pub fn git_view_paste(&mut self, text: &str) -> bool {
        if !self.git_view_focused() {
            return false;
        }
        if let Some(inp) = &mut self.git_view.input {
            inp.text.push_str(text.lines().next().unwrap_or(""));
            if inp.kind == InputKind::Filter {
                self.git_view.log.filter = inp.text.clone();
                self.git_view.log.apply_filter();
            }
        }
        true
    }

    pub fn git_view_mouse(&mut self, m: MouseEvent) -> bool {
        let v = &mut self.git_view;
        if !v.open {
            return false;
        }
        let inside = |r: Rect| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        if !inside(v.rect) {
            return false;
        }
        let wheel = match m.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => 0,
        };
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            self.dock.focused = false;
            self.agents.focus = false;
            self.project.focus_tree = false;
        }
        let (list, doc) = (v.list_rect, v.doc_rect);
        match v.tab {
            Tab::Changes if inside(list) && list.height > 0 => {
                if wheel != 0 {
                    v.list_scroll = (v.list_scroll as isize + wheel).max(0) as usize;
                } else if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    let i = v.list_scroll + (m.row - list.y) as usize;
                    if i < v.review.model.files.len() {
                        v.review.goto_file(i);
                    }
                }
            }
            Tab::Log if v.details.is_none() && inside(doc) => {
                if wheel != 0 {
                    v.log.step(wheel);
                } else if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    let i = v.log.scroll + (m.row - doc.y) as usize;
                    if i < v.log.shown.len() {
                        v.log.sel = i;
                    }
                }
            }
            _ => {
                if let Some(r) = v.active_review() {
                    if wheel != 0 {
                        r.scroll = (r.scroll as isize + wheel).clamp(0, r.rows.len().saturating_sub(1) as isize) as usize;
                        r.cursor = r.cursor.clamp(r.scroll, r.scroll + r.view_h.saturating_sub(1));
                    } else if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                        if inside(doc) {
                            let i = r.scroll + (m.row - doc.y) as usize;
                            if i < r.rows.len() {
                                r.cursor = i;
                            }
                        }
                    }
                }
            }
        }
        true
    }
}
