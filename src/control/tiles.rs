//! What the Control Center's tiles show, as data: rows with a tone and an action, built from the
//! `App` for every draw (cheap: no file system, no processes; background results come from
//! `Control`'s caches). `ui.rs` draws them, `mod.rs` acts on Enter.

use std::path::PathBuf;

use crate::agents::run::{Entry, State as RunState};
use crate::agents::staff::Status as StaffStatus;
use crate::app::App;
use crate::control::hooks;
use crate::control::procs::fmt_kb;
use crate::control::settings::{self, FIELDS};
use crate::control::widgets::{sparkline, Tone};
use crate::lsp::status::State as Srv;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileId {
    Workspace,
    Agents,
    Servers,
    Index,
    Editor,
    Settings,
}

pub const ORDER: [TileId; 6] = [TileId::Workspace, TileId::Agents, TileId::Servers, TileId::Index, TileId::Editor, TileId::Settings];

/// What Enter (or Space) does on a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    None,
    /// Show a line in the status bar.
    Say(String),
    GitView(PathBuf),
    /// Mission Control (track C) or the agents panel, on a run.
    Mission(Option<u64>),
    NewAgent,
    Staff,
    Restart(String),
    Problems,
    Tab(usize),
    SaveAll,
    Setting(&'static str),
    SettingsPage,
    OpenSettings,
    OpenLanguages,
    Tree,
    FindAction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub left: String,
    pub left_tone: Tone,
    pub right: String,
    pub tone: Tone,
    pub act: Act,
    /// A section label (not selectable).
    pub head: bool,
}

fn row(left: impl Into<String>, right: impl Into<String>, tone: Tone, act: Act) -> Row {
    Row { left: left.into(), left_tone: Tone::Normal, right: right.into(), tone, act, head: false }
}

fn head(label: &str) -> Row {
    Row { left: label.into(), left_tone: Tone::Muted, right: String::new(), tone: Tone::Muted, act: Act::None, head: true }
}

pub struct Tile {
    pub id: TileId,
    pub glyph: &'static str,
    pub title: &'static str,
    pub summary: String,
    pub tone: Tone,
    pub rows: Vec<Row>,
}

impl Tile {
    pub fn selectable(&self) -> Vec<usize> {
        (0..self.rows.len()).filter(|&i| !self.rows[i].head).collect()
    }
}

pub fn build(app: &App) -> Vec<Tile> {
    ORDER.iter().map(|id| build_one(app, *id)).collect()
}

pub fn build_one(app: &App, id: TileId) -> Tile {
    match id {
        TileId::Workspace => workspace(app),
        TileId::Agents => agents(app),
        TileId::Servers => servers(app),
        TileId::Index => index(app),
        TileId::Editor => editor(app),
        TileId::Settings => settings_tile(app),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn workspace(app: &App) -> Tile {
    let roots = hooks::roots(app);
    let mut rows = Vec::new();
    let mut dirty = 0;
    let mut repos = 0;
    for root in &roots {
        let name = hooks::label(app, root);
        match hooks::repo(app, root) {
            None => rows.push(row(name, "reading git…", Tone::Muted, Act::None)),
            Some(r) => match r.branch.clone() {
                None => rows.push(row(name, "not a git repository", Tone::Muted, Act::Tree)),
                Some(b) => {
                    repos += 1;
                    let mut parts = Vec::new();
                    if r.ahead > 0 {
                        parts.push(format!("↑{}", r.ahead));
                    }
                    if r.behind > 0 {
                        parts.push(format!("↓{}", r.behind));
                    }
                    if r.conflicts > 0 {
                        parts.push(plural(r.conflicts, "conflict", "conflicts"));
                    }
                    if r.changed > 0 {
                        parts.push(format!("{} changed", r.changed));
                    }
                    if r.untracked > 0 {
                        parts.push(format!("{} new", r.untracked));
                    }
                    let clean = parts.is_empty();
                    if !clean {
                        dirty += 1;
                    }
                    let tone = if r.conflicts > 0 { Tone::Bad } else if clean { Tone::Good } else { Tone::Warn };
                    let right = if clean { "clean".to_string() } else { parts.join(" · ") };
                    rows.push(row(format!("⎇ {b}  {name}"), right, tone, Act::GitView(root.clone())));
                    if let Some(u) = r.upstream.as_ref().filter(|_| roots.len() == 1) {
                        let mut x = row(format!("   ↳ {u}"), if r.ahead + r.behind == 0 { "in sync" } else { "" }, Tone::Muted, Act::GitView(root.clone()));
                        x.left_tone = Tone::Muted;
                        rows.push(x);
                    }
                }
            },
        }
    }
    rows.push(row("project folder", hooks::short_path(&app.project.root), Tone::Muted, Act::Tree));
    let loaded = roots.iter().all(|r| hooks::repo(app, r).is_some());
    let summary = if repos == 0 && loaded {
        "no git repository".to_string()
    } else {
        format!("{} · {}", plural(roots.len(), "repo", "repos"), if dirty == 0 { "clean".into() } else { format!("{dirty} with changes") })
    };
    Tile { id: TileId::Workspace, glyph: "◇", title: "WORKSPACE", summary, tone: if dirty == 0 { Tone::Good } else { Tone::Warn }, rows }
}

/// Runs that need a look: failed, or finished with refused tool calls.
pub fn needs_attention(r: &crate::agents::run::Run) -> bool {
    if r.state == RunState::Failed {
        return true;
    }
    !r.running() && r.entries.iter().rev().find_map(|e| if let Entry::Done { denials, .. } = e { Some(*denials) } else { None }).unwrap_or(0) > 0
}

fn agents(app: &App) -> Tile {
    let a = &app.agents;
    let c = &app.control;
    let midnight = hooks::local_midnight(c.utc_offset);
    let running = a.runs.iter().filter(|r| r.running()).count();
    let finished = a.runs.iter().filter(|r| !r.running() && r.updated >= midnight).count();
    let cost: f64 = a.runs.iter().filter(|r| r.updated >= midnight).map(|r| r.cost).sum::<f64>() + 0.0; // an empty sum is -0.0
    let attention: Vec<u64> = a.runs.iter().filter(|r| needs_attention(r)).map(|r| r.id).collect();
    let mut rows = vec![row(
        format!("● {running} running · {finished} done today"),
        format!("${cost:.2} today"),
        if running > 0 { Tone::Accent } else { Tone::Muted },
        Act::Mission(None),
    )];
    if !attention.is_empty() || !a.changed_on_disk.is_empty() {
        let n = attention.len() + a.changed_on_disk.len();
        let what = if attention.is_empty() { "files changed on disk".to_string() } else { "failed or refused tools".into() };
        let mut x = row(format!("▲ {} need attention", n), what, Tone::Warn, Act::Mission(attention.first().copied()));
        x.left_tone = Tone::Warn;
        rows.push(x);
    }
    let shown = a.shown();
    if shown.is_empty() {
        rows.push(row("+ new agent", "Alt+9", Tone::Muted, Act::NewAgent));
    }
    for &i in shown.iter().take(6) {
        let r = &a.runs[i];
        let (right, tone) = match r.state {
            RunState::Running => (format!("running {}", crate::agents::run::fmt_ms(r.elapsed_ms())), Tone::Accent),
            RunState::Done if needs_attention(r) => ("done · refused tools".to_string(), Tone::Warn),
            RunState::Done => (format!("done ${:.2}", r.cost), Tone::Good),
            RunState::Failed => ("failed".to_string(), Tone::Bad),
            RunState::Stopped => ("stopped".to_string(), Tone::Muted),
        };
        rows.push(row(format!("#{} {}", r.id, r.title), right, tone, Act::Mission(Some(r.id))));
    }
    rows.push(head("AA STAFF"));
    match &a.staff.status {
        StaffStatus::Never => rows.push(row(if a.staff.busy() { "checking the laptop…" } else { "not checked yet" }, "Enter checks", Tone::Muted, Act::Staff)),
        StaffStatus::Unreachable => rows.push(row("laptop unreachable (ssh)", "Enter retries", Tone::Warn, Act::Staff)),
        StaffStatus::Ok(members) if members.is_empty() => rows.push(row("no staff sessions", "", Tone::Muted, Act::Staff)),
        StaffStatus::Ok(members) => {
            for m in members {
                let tone = if !m.live {
                    Tone::Muted
                } else if m.state == "blocked" {
                    Tone::Warn
                } else {
                    Tone::Good
                };
                rows.push(row(m.name.clone(), format!("{} · {}", if m.live { "live" } else { "stopped" }, m.state), tone, Act::Staff));
            }
        }
    }
    let summary = if running > 0 { format!("{running} running · ${cost:.2}") } else { format!("${cost:.2} today") };
    let tone = if !attention.is_empty() { Tone::Warn } else if running > 0 { Tone::Accent } else { Tone::Muted };
    Tile { id: TileId::Agents, glyph: "◆", title: "AGENTS", summary, tone, rows }
}

fn servers(app: &App) -> Tile {
    let c = &app.control;
    let list = app.lsp_rows(c.installed.as_ref());
    let mem = |pid: Option<u32>| pid.and_then(|p| c.proc_kb(p)).map(|kb| format!(" · {}", fmt_kb(kb))).unwrap_or_default();
    let mut rows = Vec::new();
    let (mut running, mut crashed, mut missing) = (0, 0, 0);
    for r in &list {
        let server = r.server.clone().unwrap_or_default();
        let (dot, tone, act) = match r.state {
            Srv::Running => {
                running += 1;
                ("●", Tone::Good, Act::Restart(server.clone()))
            }
            Srv::Starting => {
                running += 1;
                ("◌", Tone::Accent, Act::Restart(server.clone()))
            }
            Srv::Crashed => {
                crashed += 1;
                ("✕", Tone::Bad, Act::Restart(server.clone()))
            }
            Srv::Ready => ("○", Tone::Muted, Act::Say(format!("{server} is installed · it starts when a {} file opens", r.name))),
            Srv::Missing => {
                missing += 1;
                ("·", Tone::Muted, Act::Say(format!("{} has no server on PATH ({server}) · catlas --health shows what to install", r.name)))
            }
            Srv::Unconfigured => ("·", Tone::Muted, Act::Say(format!("{}: highlighting only (no server configured in languages.toml)", r.name))),
            Srv::Unknown => ("·", Tone::Muted, Act::None),
        };
        let extra = if r.extra.is_empty() { String::new() } else { format!(" {}", r.extra.join(" ")) };
        let mut x = row(format!("{dot} {:<11}{server}{extra}", r.name), format!("{}{}", r.state.label(), mem(r.pid)), tone, act);
        if matches!(r.state, Srv::Missing | Srv::Unconfigured | Srv::Unknown) && !r.open {
            x.left_tone = Tone::Muted;
        }
        rows.push(x);
    }
    let summary = match (running, crashed) {
        (_, c) if c > 0 => format!("{running} running · {c} crashed"),
        (0, _) => format!("none running · {missing} missing"),
        _ => format!("{running} running"),
    };
    let tone = if crashed > 0 { Tone::Bad } else if running > 0 { Tone::Good } else { Tone::Muted };
    Tile { id: TileId::Servers, glyph: "◈", title: "LANGUAGE SERVERS", summary, tone, rows }
}

fn index(app: &App) -> Tile {
    let ix = hooks::index_status(app);
    let mut rows = vec![row(
        "project files",
        if ix.listing { format!("listing… {}", ix.files) } else { format!("{}", ix.files) },
        if ix.listing { Tone::Accent } else { Tone::Normal },
        Act::Say("The file list behind Go to file (Ctrl+P) and Search Everywhere (Ctrl+T), kept in memory".into()),
    )];
    match &ix.persistent {
        Some(p) => rows.push(row("persistent index", p.clone(), Tone::Good, Act::None)),
        None => rows.push(row("persistent index", "not in this build", Tone::Muted, Act::Say("The persistent index for huge codebases comes with track B (decision 0014)".into()))),
    }
    rows.push(row("open files", format!("{}", app.docs.iter().filter(|d| d.path.is_some()).count()), Tone::Normal, Act::FindAction));
    let summary = if ix.listing { "listing…".to_string() } else { format!("{} files", ix.files) };
    Tile { id: TileId::Index, glyph: "▤", title: "INDEX", summary, tone: if ix.listing { Tone::Accent } else { Tone::Muted }, rows }
}

fn editor(app: &App) -> Tile {
    let c = &app.control;
    let (mut errors, mut warnings) = (0, 0);
    for v in app.lsp.diagnostics.values() {
        for d in v {
            match d.severity {
                1 => errors += 1,
                2 => warnings += 1,
                _ => {}
            }
        }
    }
    let ptone = if errors > 0 { Tone::Bad } else if warnings > 0 { Tone::Warn } else { Tone::Good };
    let mut rows = vec![row("problems", if errors + warnings == 0 { "none".to_string() } else { format!("{} · {}", plural(errors, "error", "errors"), plural(warnings, "warning", "warnings")) }, ptone, Act::Problems)];
    let unsaved: Vec<usize> = (0..app.docs.len()).filter(|&i| app.docs[i].dirty()).collect();
    rows.push(row("unsaved", if unsaved.is_empty() { "none".to_string() } else { format!("{} · Enter saves all", plural(unsaved.len(), "file", "files")) }, if unsaved.is_empty() { Tone::Good } else { Tone::Warn }, if unsaved.is_empty() { Act::None } else { Act::SaveAll }));
    for &i in unsaved.iter().take(4) {
        let mut x = row(format!("   ● {}", app.docs[i].name()), "", Tone::Warn, Act::Tab(i));
        x.left_tone = Tone::Warn;
        rows.push(x);
    }
    rows.push(row("tabs", format!("{} open", app.docs.len()), Tone::Normal, Act::FindAction));
    rows.push(head("PROCESSES"));
    match &c.usage {
        None => rows.push(row("catlas", "measuring…", Tone::Muted, Act::None)),
        Some(u) => {
            let spark = sparkline(&c.cpu, 12);
            rows.push(row(format!("catlas  {spark}"), format!("{} · {:.0}%", fmt_kb(u.me_kb), u.me_cpu), Tone::Normal, Act::None));
            for g in &u.groups {
                let n = if g.count > 1 { format!(" ×{}", g.count) } else { String::new() };
                rows.push(row(format!("{}{n}", g.label), format!("{} · {:.0}%", fmt_kb(g.rss_kb), g.cpu), Tone::Muted, Act::None));
            }
            rows.push(row("total", format!("{} · {:.0}%", fmt_kb(u.total_kb()), u.total_cpu()), Tone::Accent, Act::None));
        }
    }
    let summary = if errors + warnings + unsaved.len() == 0 {
        "all clear".to_string()
    } else {
        let mut s = Vec::new();
        if errors > 0 {
            s.push(plural(errors, "error", "errors"));
        }
        if !unsaved.is_empty() {
            s.push(format!("{} unsaved", unsaved.len()));
        }
        if s.is_empty() {
            s.push(plural(warnings, "warning", "warnings"));
        }
        s.join(" · ")
    };
    let tone = if errors > 0 { Tone::Bad } else if !unsaved.is_empty() || warnings > 0 { Tone::Warn } else { Tone::Good };
    Tile { id: TileId::Editor, glyph: "▣", title: "EDITOR", summary, tone, rows }
}

fn settings_tile(app: &App) -> Tile {
    let s = &app.control.settings;
    let mut rows: Vec<Row> = FIELDS
        .iter()
        .map(|f| {
            let v = settings::value(s, f.key);
            let tone = match v {
                toml::Value::Boolean(false) => Tone::Muted,
                _ => Tone::Accent,
            };
            row(f.label, settings::shown(&v), tone, Act::Setting(f.key))
        })
        .collect();
    rows.push(row("All settings…", "2", Tone::Muted, Act::SettingsPage));
    rows.push(row("Open settings.toml", "e", Tone::Muted, Act::OpenSettings));
    rows.push(row("Open languages.toml", "l", Tone::Muted, Act::OpenLanguages));
    let problems = app.control.file_problems.len();
    let (summary, tone) = if problems > 0 { (plural(problems, "problem", "problems"), Tone::Warn) } else { (s.theme.name().to_string(), Tone::Muted) };
    Tile { id: TileId::Settings, glyph: "≡", title: "SETTINGS", summary, tone, rows }
}
