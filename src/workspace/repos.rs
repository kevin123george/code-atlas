//! The workspace at runtime: which repo is current (the dock's `Git`: Commit panel, Git view,
//! status bar), the Repos popup (Alt+O; `o` in the Git view) with every root's branch,
//! changes, ahead/behind and last commit, adding / removing roots, recent workspaces, and fetch /
//! pull of all repos, each behind a question (never by itself, never forced).
//!
//! Current repo: it follows the editor (opening or switching to a file of another root makes
//! that root current); Enter / `g` / `c` in the popup pick one by hand until the editor moves to
//! a file of another root again. Each root keeps its own `Git` (ticks, commit message) while
//! another one is current; the editor file's own repo still gets its gutter marks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::status::{RepoState, Statuses};
use super::{Recent, Workspace};
use crate::app::App;
use crate::git::branches::{run_remote, RemoteOp};
use crate::git::Git;
use crate::project::{canonical, fuzzy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    List,
    /// `/`: typing filters the repos by name.
    Filter,
    /// `a`: a folder to add as a root.
    Add,
    /// `w`: recent workspaces.
    Workspaces,
    /// A question (fetch / pull): y or n.
    Confirm,
}

/// A remote action over some repos.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Fetch,
    Pull,
}

/// A confirmed-to-be plan: which repos get what, and which are skipped (and why).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub op: Op,
    pub runs: Vec<(String, PathBuf, RemoteOp)>,
    pub skipped: Vec<(String, String)>,
    pub question: String,
}

pub struct Popup {
    pub mode: Mode,
    pub sel: usize,
    pub scroll: usize,
    pub filter: String,
    pub input: String,
    /// `x`: only repos with changes.
    pub only_changed: bool,
    pub plan: Option<Plan>,
    pub recents: Vec<Recent>,
    pub wsel: usize,
    // Areas from the last draw (mouse).
    pub rect: Rect,
    pub list_rect: Rect,
    /// Root index of each drawn list row.
    pub row_roots: Vec<usize>,
}

struct RemoteRun {
    rx: Receiver<(String, PathBuf, Result<String, String>)>,
    op: Op,
    total: usize,
    results: Vec<(String, Result<String, String>)>,
}

/// `App.workspace`.
pub struct WsState {
    pub status: Statuses,
    pub popup: Option<Popup>,
    /// Root index whose `Git` is `dock.git`.
    pub current: usize,
    /// The active doc's path at the last tick, and its root (the current repo follows changes).
    doc_seen: Option<Option<PathBuf>>,
    doc_root: Option<usize>,
    /// The other roots' `Git`s, by root folder.
    pub stash: HashMap<PathBuf, Git>,
    remote: Option<RemoteRun>,
    /// The last fetch / pull result per root folder ("fetched", or the error).
    pub ops: HashMap<PathBuf, Result<String, String>>,
    /// What runs on the remotes now ("Fetching 3/14").
    pub busy: Option<String>,
    /// `workspaces.toml` (None in tests).
    pub store: Option<PathBuf>,
}

impl Default for WsState {
    fn default() -> Self {
        WsState {
            status: Statuses::default(),
            popup: None,
            current: 0,
            doc_seen: None,
            doc_root: None,
            stash: HashMap::new(),
            remote: None,
            ops: HashMap::new(),
            busy: None,
            store: if cfg!(test) { None } else { super::recent_path() },
        }
    }
}

/// Gutter marks for a doc: the current repo's, or (a file of another root) that root's `Git`.
pub fn marks<'a>(git: &'a Git, st: &'a WsState, path: Option<&Path>) -> &'a [Option<crate::git::diff::Mark>] {
    let own = git.marks(path);
    if !own.is_empty() || path.is_none() {
        return own;
    }
    st.stash.values().map(|g| g.marks(path)).find(|m| !m.is_empty()).unwrap_or(own)
}

/// The root indices the popup lists (filter, only-changed), in workspace order.
pub fn visible(ws: &Workspace, status: &Statuses, filter: &str, only_changed: bool) -> Vec<usize> {
    let f = filter.trim();
    (0..ws.roots.len())
        .filter(|&i| f.is_empty() || fuzzy::score(f, &ws.roots[i].label).is_some())
        .filter(|&i| !only_changed || status.get(&ws.roots[i].path).is_some_and(RepoState::dirty))
        .collect()
}

fn repos(n: usize) -> String {
    if n == 1 { "1 repo".into() } else { format!("{n} repos") }
}

/// Plans fetch / pull for `targets` (label, root folder, status): fetch needs a remote, pull a
/// branch with an upstream. Pull is `--ff-only`: never a merge commit, never forced.
pub fn plan(op: Op, targets: &[(String, PathBuf, Option<&RepoState>)]) -> Result<Plan, String> {
    let mut runs = Vec::new();
    let mut skipped = Vec::new();
    for (label, root, st) in targets {
        let Some(st) = st.filter(|s| s.is_repo()) else {
            skipped.push((label.clone(), "not a git repository".to_string()));
            continue;
        };
        let top = st.top.clone().unwrap_or_else(|| root.clone());
        match op {
            Op::Fetch if st.remotes == 0 => skipped.push((label.clone(), "no remote".into())),
            Op::Fetch => runs.push((label.clone(), top, RemoteOp { args: vec!["fetch".into(), "--all".into()], question: String::new(), done: "fetched".into() })),
            Op::Pull if st.remotes == 0 => skipped.push((label.clone(), "no remote".into())),
            Op::Pull if !st.upstream => skipped.push((label.clone(), "no upstream branch".into())),
            Op::Pull => runs.push((label.clone(), top, RemoteOp { args: vec!["pull".into(), "--ff-only".into()], question: String::new(), done: "pulled".into() })),
        }
    }
    if runs.is_empty() {
        let why = skipped.first().map(|(l, w)| format!(" ({l}: {w})")).unwrap_or_default();
        return Err(format!("Nothing to {}: no repo has {}{why}", if op == Op::Fetch { "fetch" } else { "pull" }, if op == Op::Fetch { "a remote" } else { "an upstream branch" }));
    }
    let skip = if skipped.is_empty() { String::new() } else { format!(", {} skipped", skipped.len()) };
    let what = if runs.len() == 1 { runs[0].0.clone() } else { repos(runs.len()) };
    let question = match op {
        Op::Fetch => format!("Fetch {what} from their remotes{skip}? Nothing local changes"),
        Op::Pull => {
            let behind: usize = targets.iter().filter_map(|t| t.2).map(|s| s.behind).sum();
            format!("Pull {what}, fast-forward only{skip} ({behind} commits behind as of the last fetch; never forced)?")
        }
    };
    Ok(Plan { op, runs, skipped, question })
}

/// The status-bar summary of a finished fetch / pull.
pub fn summary(op: &Op, results: &[(String, Result<String, String>)], skipped: usize) -> String {
    let ok = results.iter().filter(|r| r.1.is_ok()).count();
    let verb = if *op == Op::Fetch { "Fetched" } else { "Pulled" };
    let mut s = format!("{verb} {ok} of {}", repos(results.len()));
    if skipped > 0 {
        s.push_str(&format!(" · {skipped} skipped"));
    }
    let failed: Vec<String> = results
        .iter()
        .filter_map(|(l, r)| r.as_ref().err().map(|e| format!("{l}: {}", e.lines().last().unwrap_or(e))))
        .collect();
    if !failed.is_empty() {
        s = format!("Couldn't {} {} · {s} · {}", if *op == Op::Fetch { "fetch" } else { "pull" }, repos(failed.len()), failed.join(" · "));
    }
    s
}

// ---- glue: the workspace's side of `App` -----------------------------------------------------

impl App {
    /// Right after `App::new`: the first root's `Git` and the recent list.
    pub fn workspace_init(&mut self) {
        let ws = &self.project.ws;
        if ws.multi() {
            let r = &ws.roots[0];
            self.dock.git = Git::new(&r.path);
            self.dock.git.label = Some(r.label.clone());
        }
        self.remember_workspace();
    }

    fn remember_workspace(&self) {
        if let Some(p) = &self.workspace.store {
            let _ = super::remember(p, &self.project.ws);
        }
    }

    /// The current repo's folder (the dock's `Git`), for `toplevel` fallbacks.
    pub fn repo_dir(&self) -> PathBuf {
        let ws = &self.project.ws;
        ws.roots.get(self.workspace.current).map(|r| r.path.clone()).unwrap_or_else(|| self.project.root.clone())
    }

    /// Every loop turn: statuses, the current repo following the editor, fetch / pull results.
    /// True = redraw.
    pub fn workspace_tick(&mut self) -> bool {
        let mut redraw = false;
        let multi = self.project.ws.multi();
        let popup = self.workspace.popup.is_some();
        if multi || popup {
            let roots: Vec<PathBuf> = self.project.ws.roots.iter().map(|r| r.path.clone()).collect();
            redraw |= self.workspace.status.poll(&roots, popup);
        }
        if multi {
            let path = self.docs[self.active].path.clone();
            if self.workspace.doc_seen.as_ref() != Some(&path) {
                let root = path.as_deref().map(canonical).and_then(|p| self.project.ws.root_of(&p));
                self.workspace.doc_seen = Some(path);
                if root != self.workspace.doc_root {
                    self.workspace.doc_root = root;
                    if let Some(i) = root.filter(|i| *i != self.workspace.current) {
                        self.switch_repo(i, false);
                        redraw = true;
                    }
                }
            }
            // The editor's file lives in another repo than the current one: keep its gutter.
            if let Some(i) = self.workspace.doc_root.filter(|i| *i != self.workspace.current) {
                if let Some(r) = self.project.ws.roots.get(i) {
                    let g = self.workspace.stash.entry(r.path.clone()).or_insert_with(|| Git::new(&r.path));
                    let _ = g.poll(&self.docs, self.active);
                    redraw |= g.take_changed();
                }
            }
        }
        redraw | self.remote_tick()
    }

    /// Makes root `i` the current repo (Commit panel, Git view, status bar).
    pub fn switch_repo(&mut self, i: usize, say: bool) {
        let Some(r) = self.project.ws.roots.get(i).cloned() else { return };
        if i != self.workspace.current || self.dock.git.project_dir() != r.path.as_path() {
            let mut new = self.workspace.stash.remove(&r.path).unwrap_or_else(|| Git::new(&r.path));
            new.label = self.project.ws.multi().then(|| r.label.clone());
            new.refresh();
            let old = std::mem::replace(&mut self.dock.git, new);
            let old_dir = old.project_dir().to_path_buf();
            if self.project.ws.roots.iter().any(|x| x.path == old_dir) {
                self.workspace.stash.insert(old_dir, old);
            }
            self.workspace.current = i;
            if self.git_view.open {
                let tab = self.git_view.tab;
                self.open_git_view(None);
                self.git_view.tab = tab;
            }
        }
        if say {
            let b = self.workspace.status.get(&r.path).and_then(|s| s.branch.clone()).map(|b| format!(" ⎇ {b}")).unwrap_or_default();
            self.message = Some(format!("Current repo: {}{b} · Commit panel and Git view show it", r.label));
        }
    }

    /// Replaces the workspace (a recent one, or after adding / removing a root).
    pub fn set_workspace(&mut self, ws: Workspace) {
        ws.apply_excludes();
        let keep = self.dock.git.project_dir().to_path_buf();
        self.project.set_workspace(ws);
        let ws = &self.project.ws;
        self.workspace.stash.retain(|p, _| ws.roots.iter().any(|r| r.path == *p));
        self.workspace.ops.retain(|p, _| ws.roots.iter().any(|r| r.path == *p));
        self.workspace.doc_seen = None;
        self.workspace.doc_root = None;
        self.workspace.status.refresh();
        match ws.roots.iter().position(|r| r.path == keep) {
            Some(i) => {
                self.workspace.current = i;
                self.dock.git.label = ws.multi().then(|| ws.roots[i].label.clone());
            }
            None => {
                self.workspace.current = usize::MAX;
                self.switch_repo(0, false);
            }
        }
        self.remember_workspace();
    }

    /// Adds a folder as a root (runtime). Returns the message.
    pub fn add_root(&mut self, dir: &Path) -> String {
        let mut ws = self.project.ws.clone();
        match ws.add_root(dir, None, None) {
            Ok(i) => {
                let label = ws.roots[i].label.clone();
                if ws.roots.len() == 2 && ws.file.is_none() {
                    ws.name = ws.default_name();
                }
                self.set_workspace(ws);
                self.project.sidebar = true;
                format!("Added {label} · {} roots now", self.project.ws.roots.len())
            }
            Err(e) => format!("Couldn't add: {e}"),
        }
    }

    /// Takes root `i` out of the workspace (its files stay as they are; open tabs stay open).
    pub fn remove_root(&mut self, i: usize) -> String {
        let mut ws = self.project.ws.clone();
        if ws.roots.len() <= 1 {
            return "The only root can't be removed (a adds another one first)".into();
        }
        if i >= ws.roots.len() {
            return String::new();
        }
        let r = ws.roots.remove(i);
        if ws.file.is_none() {
            ws.name = ws.default_name();
        }
        self.set_workspace(ws);
        format!("Removed {} from the workspace (nothing on disk changed; a adds a folder back)", r.label)
    }

    pub fn open_repos(&mut self) {
        self.project.close_popup();
        self.lsp.popup = None;
        self.workspace.status.refresh();
        let sel = self.workspace.current.min(self.project.ws.roots.len().saturating_sub(1));
        self.workspace.popup = Some(Popup {
            mode: Mode::List,
            sel,
            scroll: 0,
            filter: String::new(),
            input: String::new(),
            only_changed: false,
            plan: None,
            recents: Vec::new(),
            wsel: 0,
            rect: Rect::default(),
            list_rect: Rect::default(),
            row_roots: Vec::new(),
        });
        self.message = Some(REPOS_HINT.into());
    }

    fn visible_roots(&self) -> Vec<usize> {
        let Some(p) = &self.workspace.popup else { return Vec::new() };
        visible(&self.project.ws, &self.workspace.status, &p.filter, p.only_changed)
    }

    /// The root index of the selected row.
    pub fn repos_selected(&self) -> Option<usize> {
        let p = self.workspace.popup.as_ref()?;
        self.visible_roots().get(p.sel).copied()
    }

    /// Alt+O anywhere (the terminal keeps its keys), then every key while the popup is open.
    pub fn workspace_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let opener = alt && !ctrl && matches!(k.code, KeyCode::Char('o') | KeyCode::Char('O'));
        if opener {
            if self.workspace.popup.is_some() {
                self.workspace.popup = None;
                self.message = None;
            } else {
                self.open_repos();
            }
            return true;
        }
        let Some(mode) = self.workspace.popup.as_ref().map(|p| p.mode) else { return false };
        // Quitting still works.
        if ctrl && !alt && matches!(k.code, KeyCode::Char('q')) {
            self.workspace.popup = None;
            return false;
        }
        match mode {
            Mode::List => self.repos_list_key(k),
            Mode::Filter | Mode::Add => self.repos_input_key(k, mode),
            Mode::Workspaces => self.repos_recent_key(k),
            Mode::Confirm => self.repos_confirm_key(k),
        }
        true
    }

    fn repos_list_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let n = self.visible_roots().len();
        let page = self.workspace.popup.as_ref().map_or(10, |p| p.list_rect.height.max(2) as isize - 1);
        let step = |p: &mut Popup, d: isize| {
            if n > 0 {
                p.sel = (p.sel as isize + d).clamp(0, n as isize - 1) as usize;
            }
        };
        let sel = self.repos_selected();
        let mut msg: Option<String> = None;
        let Some(p) = self.workspace.popup.as_mut() else { return };
        match k.code {
            KeyCode::Esc => {
                self.workspace.popup = None;
                self.message = None;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl => step(p, -1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => step(p, 1),
            KeyCode::PageUp => step(p, -page),
            KeyCode::PageDown => step(p, page),
            KeyCode::Home => step(p, isize::MIN / 2),
            KeyCode::End => step(p, isize::MAX / 2),
            KeyCode::Char('/') if !ctrl => {
                p.mode = Mode::Filter;
                msg = Some("Filter repos: type a name · Enter keep · Esc clear".into());
            }
            KeyCode::Char('x') if !ctrl => {
                p.only_changed = !p.only_changed;
                p.sel = 0;
                msg = Some(if p.only_changed { "Only repos with changes · x shows all" } else { "All repos" }.into());
            }
            KeyCode::Char('a') if !ctrl => {
                p.mode = Mode::Add;
                p.input.clear();
                msg = Some("Add a folder as a root: type its path (~/ works, relative to the workspace home) · Enter add · Esc cancel".into());
            }
            KeyCode::Char('w') if !ctrl => {
                p.recents = super::load_recent(self.workspace.store.as_deref());
                if p.recents.is_empty() {
                    msg = Some("No recent workspaces yet (catlas -w lists them; they're saved when one opens)".into());
                } else {
                    p.mode = Mode::Workspaces;
                    p.wsel = 0;
                    msg = Some("Recent workspaces: Enter open · Esc back".into());
                }
            }
            KeyCode::Char('r') | KeyCode::F(5) if !ctrl => {
                self.workspace.status.refresh();
                msg = Some("Refreshing every repo…".into());
            }
            KeyCode::Char(c @ ('F' | 'U' | 'f' | 'u')) if !ctrl => {
                let op = if c.eq_ignore_ascii_case(&'f') { Op::Fetch } else { Op::Pull };
                msg = Some(self.ask_plan(op, c.is_ascii_uppercase(), sel));
            }
            KeyCode::Char('d') | KeyCode::Delete if !ctrl => {
                if let Some(i) = sel {
                    msg = Some(self.remove_root(i));
                    if let Some(p) = self.workspace.popup.as_mut() {
                        p.sel = p.sel.min(self.project.ws.roots.len().saturating_sub(1));
                    }
                }
            }
            KeyCode::Enter | KeyCode::Char('g') | KeyCode::Char('c') | KeyCode::Char('l') if !ctrl => {
                let Some(i) = sel else { return };
                let what = k.code;
                self.workspace.popup = None;
                self.switch_repo(i, true);
                match what {
                    KeyCode::Char('g') => self.open_git_view(None),
                    KeyCode::Char('l') => {
                        self.open_git_view(None);
                        self.git_view.tab = crate::git::view::Tab::Log;
                    }
                    KeyCode::Char('c') => {
                        // The Commit panel of that repo (Alt+0 again hides it).
                        self.close_git_view();
                        let d = &mut self.dock;
                        d.open = true;
                        d.focused = true;
                        d.tab = crate::dock::Tab::Commit;
                        d.git.panel.field = crate::git::Field::List;
                        d.git.refresh();
                        self.project.focus_tree = false;
                        self.agents.focus = false;
                    }
                    _ => {
                        // Show the repo in the tree.
                        let path = self.project.ws.roots[i].path.clone();
                        if self.project.sidebar {
                            self.project.tree.reveal(&path);
                        }
                    }
                }
                return;
            }
            _ => {}
        }
        if let Some(m) = msg {
            self.message = Some(m);
        }
    }

    fn ask_plan(&mut self, op: Op, all: bool, sel: Option<usize>) -> String {
        if self.workspace.remote.is_some() {
            return "A fetch / pull is still running".into();
        }
        let ws = &self.project.ws;
        let idx: Vec<usize> = if all { (0..ws.roots.len()).collect() } else { sel.into_iter().collect() };
        let targets: Vec<(String, PathBuf, Option<&RepoState>)> = idx.iter().map(|&i| (ws.roots[i].label.clone(), ws.roots[i].path.clone(), self.workspace.status.get(&ws.roots[i].path))).collect();
        if targets.iter().any(|t| t.2.is_none()) {
            return "Still reading the repos… (try again in a moment)".into();
        }
        match plan(op, &targets) {
            Ok(pl) => {
                let q = format!("{}  y = yes · n = no", pl.question);
                if let Some(p) = self.workspace.popup.as_mut() {
                    p.plan = Some(pl);
                    p.mode = Mode::Confirm;
                }
                q
            }
            Err(e) => e,
        }
    }

    fn repos_confirm_key(&mut self, k: KeyEvent) {
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let yes = plain && matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter);
        let no = k.code == KeyCode::Esc || (plain && matches!(k.code, KeyCode::Char('n') | KeyCode::Char('N')));
        let Some(p) = self.workspace.popup.as_mut() else { return };
        if !yes && !no {
            self.message = Some(format!("{}  y = yes · n = no", p.plan.as_ref().map_or("", |x| x.question.as_str())));
            return;
        }
        p.mode = Mode::List;
        let Some(plan) = p.plan.take() else { return };
        if no {
            self.message = Some("Cancelled".into());
            return;
        }
        self.start_remote(plan);
    }

    fn start_remote(&mut self, plan: Plan) {
        let (tx, rx) = channel();
        let total = plan.runs.len();
        let next = Arc::new(AtomicUsize::new(0));
        let runs = Arc::new(plan.runs.clone());
        for _ in 0..4.min(total) {
            let (tx, next, runs) = (tx.clone(), next.clone(), runs.clone());
            std::thread::spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((label, top, op)) = runs.get(i) else { break };
                let r = run_remote(top, op);
                if tx.send((label.clone(), top.clone(), r)).is_err() {
                    break;
                }
            });
        }
        // Only this run's results show in the popup.
        self.workspace.ops.clear();
        for (label, why) in &plan.skipped {
            if let Some(i) = self.project.ws.label_index(label) {
                let p = self.project.ws.roots[i].path.clone();
                self.workspace.ops.insert(p, Err(format!("skipped: {why}")));
            }
        }
        let verb = if plan.op == Op::Fetch { "Fetching" } else { "Pulling" };
        self.workspace.busy = Some(format!("{verb} 0/{total}…"));
        self.message = self.workspace.busy.clone();
        self.workspace.remote = Some(RemoteRun { rx, op: plan.op, total, results: Vec::new() });
    }

    fn remote_tick(&mut self) -> bool {
        let Some(run) = self.workspace.remote.as_mut() else { return false };
        let mut got = false;
        while let Ok((label, top, r)) = run.rx.try_recv() {
            got = true;
            let root = self.project.ws.label_index(&label).map(|i| self.project.ws.roots[i].path.clone()).unwrap_or(top);
            self.workspace.ops.insert(root, r.clone().map(|m| m.to_string()));
            run.results.push((label, r));
        }
        if !got {
            return false;
        }
        let verb = if run.op == Op::Fetch { "Fetching" } else { "Pulling" };
        if run.results.len() < run.total {
            self.workspace.busy = Some(format!("{verb} {}/{}…", run.results.len(), run.total));
            self.message = self.workspace.busy.clone();
            return true;
        }
        let run = self.workspace.remote.take().unwrap();
        let skipped = self.workspace.ops.values().filter(|r| r.as_ref().is_err_and(|e| e.starts_with("skipped"))).count();
        self.workspace.busy = None;
        self.message = Some(summary(&run.op, &run.results, skipped));
        self.workspace.status.refresh();
        self.dock.git.refresh();
        for g in self.workspace.stash.values_mut() {
            g.refresh();
        }
        self.git_view.review.refresh();
        self.git_view.log.loaded = false;
        if run.op == Op::Pull {
            self.reload_open_docs();
        }
        true
    }

    fn repos_input_key(&mut self, k: KeyEvent, mode: Mode) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(p) = self.workspace.popup.as_mut() else { return };
        let text = if mode == Mode::Filter { &mut p.filter } else { &mut p.input };
        match k.code {
            KeyCode::Esc => {
                if mode == Mode::Filter {
                    p.filter.clear();
                }
                p.mode = Mode::List;
                p.sel = 0;
                self.message = Some(REPOS_HINT.into());
            }
            KeyCode::Enter if mode == Mode::Filter => {
                p.mode = Mode::List;
                self.message = Some(REPOS_HINT.into());
            }
            KeyCode::Enter => {
                let input = p.input.trim().to_string();
                if input.is_empty() {
                    p.mode = Mode::List;
                    return;
                }
                let dir = super::expand(&input, &self.project.ws.home);
                let m = self.add_root(&dir);
                if let Some(p) = self.workspace.popup.as_mut() {
                    if !m.starts_with("Couldn't") {
                        p.mode = Mode::List;
                        p.input.clear();
                        p.filter.clear();
                        p.sel = self.project.ws.roots.len().saturating_sub(1);
                    }
                }
                self.message = Some(m);
            }
            KeyCode::Backspace if ctrl => crate::workspace::repos::pop_word(text),
            KeyCode::Char('h') if ctrl => crate::workspace::repos::pop_word(text),
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if ctrl => text.clear(),
            KeyCode::Down if mode == Mode::Filter => {
                p.mode = Mode::List;
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                text.push(c);
                if mode == Mode::Filter {
                    p.sel = 0;
                }
            }
            _ => {}
        }
    }

    fn repos_recent_key(&mut self, k: KeyEvent) {
        let Some(p) = self.workspace.popup.as_mut() else { return };
        let n = p.recents.len();
        match k.code {
            KeyCode::Esc => {
                p.mode = Mode::List;
                self.message = Some(REPOS_HINT.into());
            }
            KeyCode::Up | KeyCode::Char('k') => p.wsel = p.wsel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.wsel = (p.wsel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter => {
                let Some(r) = p.recents.get(p.wsel).cloned() else { return };
                match r.open() {
                    Ok(ws) => {
                        let name = ws.name.clone();
                        let n = ws.roots.len();
                        self.set_workspace(ws);
                        if let Some(p) = self.workspace.popup.as_mut() {
                            p.mode = Mode::List;
                            p.sel = 0;
                            p.filter.clear();
                        }
                        self.project.sidebar = true;
                        self.message = Some(format!("Workspace {name}: {n} root{} (agents and the terminal stay in {})", if n == 1 { "" } else { "s" }, self.project.root.display()));
                    }
                    Err(e) => self.message = Some(format!("Couldn't open: {e}")),
                }
            }
            _ => {}
        }
    }

    pub fn workspace_paste(&mut self, text: &str) -> bool {
        let Some(p) = self.workspace.popup.as_mut() else { return false };
        let line = text.lines().next().unwrap_or("");
        match p.mode {
            Mode::Add => p.input.push_str(line),
            Mode::Filter => p.filter.push_str(line),
            _ => {}
        }
        true
    }

    pub fn workspace_mouse(&mut self, m: MouseEvent) -> bool {
        let Some(p) = self.workspace.popup.as_mut() else { return false };
        let inside = |r: Rect| m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom();
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if p.mode == Mode::List => {
                let d: isize = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                let max = self.visible_roots().len().saturating_sub(1);
                let p = self.workspace.popup.as_mut().unwrap();
                p.sel = (p.sel as isize + d).clamp(0, max as isize) as usize;
            }
            MouseEventKind::Down(MouseButton::Left) if p.mode == Mode::List && inside(p.list_rect) => {
                let row = (m.row - p.list_rect.y) as usize;
                if let Some(root) = p.row_roots.get(row).copied() {
                    let vis = self.visible_roots();
                    if let Some(k) = vis.iter().position(|r| *r == root) {
                        let p = self.workspace.popup.as_mut().unwrap();
                        if p.sel == k {
                            self.workspace.popup = None;
                            self.switch_repo(root, true);
                        } else {
                            p.sel = k;
                        }
                    }
                }
            }
            MouseEventKind::Down(_) if !inside(p.rect) => {
                self.workspace.popup = None;
                self.message = None;
            }
            _ => {}
        }
        true
    }
}

pub const REPOS_HINT: &str = "Repos: Enter make current · g changes · l log · c commit · F/U fetch/pull all · a add · d remove · w workspaces · / filter · Esc";

pub fn pop_word(s: &mut String) {
    while s.ends_with(|c: char| !c.is_alphanumeric()) {
        s.pop();
    }
    while s.ends_with(char::is_alphanumeric) {
        s.pop();
    }
}
