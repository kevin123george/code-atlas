//! Workspaces (decision 0013): several roots (usually git repositories) open at once.
//!
//! - **Opening:** `catlas dir1 dir2 …` (each folder a root), a workspace file (`*.catlas.toml`, or
//!   `<dir>/.code-atlas/workspace.toml` when `catlas <dir>` opens that folder), or a folder that
//!   is no repository itself but holds 2+ git repositories one level down (`catlas ~/Projects`):
//!   those repositories become the roots. `--single` opens such a folder as one root, as before.
//!   One root behaves exactly as Code Atlas always did.
//! - **Paths:** with several roots, project paths are `label/path` (the label is the root's name,
//!   unique) and are shown as `label › path`; [`Workspace::rel`] / [`Workspace::abs`] map them.
//!   With one root they stay plain relative paths.
//! - **Recent workspaces** live in `workspaces.toml` in the config folder (`catlas -w` lists them,
//!   `catlas -w 2` / `-w name` reopens one, `w` in the Repos popup switches at runtime).
//!
//! Submodules: `status.rs` background git status of every root (tree badges, Repos popup),
//! `repos.rs` the Repos popup (Ctrl+O) and the `impl App` glue (current repo, add/remove a root,
//! fetch/pull all), `ui.rs` drawing (popup, tree badges, `repo › path`, status bar).

pub mod repos;
pub mod status;
pub mod ui;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

use crate::project::canonical;
use crate::ui::theme::*;

/// `catlas --single dir`: a folder full of repositories opens as one root (as before 0013).
pub static SINGLE: AtomicBool = AtomicBool::new(false);
/// `catlas -w <n|name>`: the recent workspace to open (taken by [`from_args`]).
pub static PRESET: std::sync::Mutex<Option<Workspace>> = std::sync::Mutex::new(None);
/// The workspace file name inside a folder's `.code-atlas/`.
pub const DIR_FILE: &str = ".code-atlas/workspace.toml";
/// Recent workspaces kept.
const RECENT_MAX: usize = 20;
/// Root colours, in order (the Ledger Terminal series palette).
const PALETTE: [Color; 8] = [S_BLUE, S_AQUA, S_ORANGE, S_VIOLET, S_YELLOW, S_MAGENTA, S_TEAL, GOOD];

#[derive(Clone, Debug, PartialEq)]
pub struct Root {
    /// Canonical folder.
    pub path: PathBuf,
    /// Unique, no `/`; the first segment of this root's project paths.
    pub label: String,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub roots: Vec<Root>,
    /// Extra things to hide everywhere (tree, finder, search): names / `*.ext` globs, or paths
    /// relative to a root (`docs/old`).
    pub excludes: Vec<String>,
    /// The workspace file it came from.
    pub file: Option<PathBuf>,
    /// Where catlas was pointed (agents and the terminal start here): the folder given, the
    /// workspace file's folder, or the first root.
    pub home: PathBuf,
}

fn dir_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// `~/x` → `$HOME/x`; relative paths against `base`.
pub fn expand(s: &str, base: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let p = if s == "~" {
        home
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    };
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// A colour by name (`blue`, `green`, `orange`, `violet`/`purple`, `yellow`, `pink`/`magenta`,
/// `teal`/`cyan`, `aqua`, `red`) or `#rrggbb`.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_lowercase();
    if let Some(hex) = s.strip_prefix('#').filter(|h| h.len() == 6) {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    Some(match s.as_str() {
        "blue" => S_BLUE,
        "green" => GOOD,
        "aqua" => S_AQUA,
        "orange" => S_ORANGE,
        "violet" | "purple" => S_VIOLET,
        "yellow" => S_YELLOW,
        "pink" | "magenta" => S_MAGENTA,
        "teal" | "cyan" => S_TEAL,
        "red" => CRITICAL,
        _ => return None,
    })
}

impl Workspace {
    /// One root: everything as before workspaces.
    pub fn single(root: PathBuf) -> Workspace {
        let label = dir_name(&root);
        Workspace { name: label.clone(), roots: vec![Root { path: root.clone(), label, color: PALETTE[0] }], excludes: Vec::new(), file: None, home: root }
    }

    /// Roots from folders (labels from their names, made unique; colours in palette order).
    /// Folders inside another root, or given twice, are left out.
    pub fn from_dirs(name: Option<String>, home: PathBuf, dirs: Vec<(PathBuf, Option<String>, Option<Color>)>) -> Workspace {
        let mut ws = Workspace { name: String::new(), roots: Vec::new(), excludes: Vec::new(), file: None, home };
        for (d, label, color) in dirs {
            let _ = ws.add_root(&d, label, color);
        }
        ws.name = name.unwrap_or_else(|| ws.default_name());
        ws
    }

    fn default_name(&self) -> String {
        match self.roots.len() {
            0 | 1 => self.roots.first().map(|r| r.label.clone()).unwrap_or_default(),
            n if self.home != self.roots[0].path => format!("{} ({n} roots)", dir_name(&self.home)),
            n => format!("{} + {}", self.roots[0].label, n - 1),
        }
    }

    pub fn multi(&self) -> bool {
        self.roots.len() > 1
    }

    /// Adds a root; returns its index or why not (missing, not a folder, nested, duplicate).
    pub fn add_root(&mut self, dir: &Path, label: Option<String>, color: Option<Color>) -> Result<usize, String> {
        if !dir.is_dir() {
            return Err(format!("{} is not a folder", dir.display()));
        }
        let path = canonical(dir);
        if let Some(r) = self.roots.iter().find(|r| path.starts_with(&r.path) || r.path.starts_with(&path)) {
            return Err(if r.path == path { format!("{} is already a root", r.label) } else { format!("{} overlaps the root {}", path.display(), r.label) });
        }
        let base = label.map(|l| l.replace('/', "-")).filter(|l| !l.trim().is_empty()).unwrap_or_else(|| dir_name(&path));
        let mut label = base.clone();
        let mut n = 2;
        while self.roots.iter().any(|r| r.label == label) {
            label = format!("{base}-{n}");
            n += 1;
        }
        let color = color.unwrap_or(PALETTE[self.roots.len() % PALETTE.len()]);
        self.roots.push(Root { path, label, color });
        Ok(self.roots.len() - 1)
    }

    /// The root holding `p` (canonical), the innermost when they nest.
    pub fn root_of(&self, p: &Path) -> Option<usize> {
        self.roots.iter().enumerate().filter(|(_, r)| p.starts_with(&r.path)).max_by_key(|(_, r)| r.path.as_os_str().len()).map(|(i, _)| i)
    }

    /// `p` as a project path: relative to the root (one root), `label/rel` (several), or the
    /// absolute path when it lies outside every root.
    pub fn rel(&self, p: &Path) -> String {
        if !self.multi() {
            return crate::project::search::rel(&self.roots[0].path, p);
        }
        match self.root_of(p) {
            Some(i) => {
                let r = &self.roots[i];
                let rest = p.strip_prefix(&r.path).map(|x| x.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                if rest.is_empty() { r.label.clone() } else { format!("{}/{rest}", r.label) }
            }
            None => p.to_string_lossy().replace('\\', "/"),
        }
    }

    /// The file for a project path (the inverse of [`Workspace::rel`]).
    pub fn abs(&self, rel: &str) -> PathBuf {
        if Path::new(rel).is_absolute() {
            return PathBuf::from(rel);
        }
        if !self.multi() {
            return self.roots[0].path.join(rel);
        }
        let (head, rest) = rel.split_once('/').unwrap_or((rel, ""));
        match self.roots.iter().find(|r| r.label == head) {
            Some(r) if rest.is_empty() => r.path.clone(),
            Some(r) => r.path.join(rest),
            None => self.home.join(rel),
        }
    }

    /// A project path for people: `label › path` with several roots.
    pub fn show(&self, rel: &str) -> String {
        match rel.split_once('/') {
            Some((head, rest)) if self.multi() && self.label_index(head).is_some() => format!("{head} › {rest}"),
            _ => rel.to_string(),
        }
    }

    pub fn label_index(&self, label: &str) -> Option<usize> {
        self.roots.iter().position(|r| r.label == label)
    }

    /// (prefix, folder) per root for the background walks: no prefix with one root.
    pub fn prefixes(&self) -> Vec<(String, PathBuf)> {
        if !self.multi() {
            return vec![(String::new(), self.roots[0].path.clone())];
        }
        self.roots.iter().map(|r| (r.label.clone(), r.path.clone())).collect()
    }

    /// Search scoping by repo: `@label rest` (a label, or a unique start of one, then a space)
    /// → (that label, rest). Anything else → (None, query).
    pub fn repo_query<'a>(&self, q: &'a str) -> (Option<String>, &'a str) {
        let Some(after) = q.strip_prefix('@') else { return (None, q) };
        let Some((word, rest)) = after.split_once(' ') else { return (None, q) };
        if word.is_empty() || !self.multi() {
            return (None, q);
        }
        let w = word.to_lowercase();
        let exact = self.roots.iter().find(|r| r.label.to_lowercase() == w);
        let starts: Vec<&Root> = self.roots.iter().filter(|r| r.label.to_lowercase().starts_with(&w)).collect();
        match exact.or(if starts.len() == 1 { Some(starts[0]) } else { None }) {
            Some(r) => (Some(r.label.clone()), rest.trim_start()),
            None => (None, q),
        }
    }

    /// Makes [`excluded`] use this workspace's excludes.
    pub fn apply_excludes(&self) {
        let mut list = Vec::new();
        for e in &self.excludes {
            let e = e.trim().trim_end_matches('/');
            if e.is_empty() {
                continue;
            }
            if e.contains('/') {
                list.extend(self.roots.iter().map(|r| Exclude::Path(r.path.join(e))));
            } else {
                list.push(Exclude::Name(e.to_string()));
            }
        }
        HAS_EXCLUDES.store(!list.is_empty(), Ordering::Relaxed);
        if let Ok(mut g) = EXCLUDES.write() {
            *g = list;
        }
    }
}

// ---- excludes (used by the project walker) ---------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Exclude {
    /// A file or folder name, or a `*` glob over names (`*.min.js`).
    Name(String),
    /// A path (absolute: root + the relative path from the file).
    Path(PathBuf),
}

static HAS_EXCLUDES: AtomicBool = AtomicBool::new(false);
static EXCLUDES: RwLock<Vec<Exclude>> = RwLock::new(Vec::new());

/// `*` glob over a name (no `*` = equal).
pub fn glob(pat: &str, name: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

pub fn matches(list: &[Exclude], path: &Path, name: &str) -> bool {
    list.iter().any(|e| match e {
        Exclude::Name(p) => glob(p, name),
        Exclude::Path(p) => path == p.as_path(),
    })
}

/// Whether the walker should skip this entry (the workspace's `exclude` list).
pub fn excluded(path: &Path, name: &str) -> bool {
    if !HAS_EXCLUDES.load(Ordering::Relaxed) {
        return false;
    }
    EXCLUDES.read().map(|l| matches(&l, path, name)).unwrap_or(false)
}

// ---- opening: arguments, workspace files, nested repositories --------------------------------

#[derive(Debug, Default, Deserialize)]
struct FileRoot {
    path: String,
    label: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileFormat {
    name: Option<String>,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    root: Vec<FileRoot>,
    #[serde(default, alias = "excludes")]
    exclude: Vec<String>,
}

/// Whether `p` names a workspace file.
pub fn is_workspace_file(p: &Path) -> bool {
    p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".catlas.toml"))
}

/// Reads a workspace file. Roots are relative to `base` (the file's folder, or the folder that
/// holds `.code-atlas/`). Returns the workspace and notes about roots that were skipped.
pub fn load_file(file: &Path, base: &Path) -> Result<(Workspace, Vec<String>), String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let f: FileFormat = toml::from_str(&text).map_err(|e| format!("{}: {}", file.display(), e.message()))?;
    let mut dirs: Vec<(PathBuf, Option<String>, Option<Color>)> = f.roots.iter().map(|p| (expand(p, base), None, None)).collect();
    dirs.extend(f.root.iter().map(|r| (expand(&r.path, base), r.label.clone(), r.color.as_deref().and_then(parse_color))));
    let mut notes = Vec::new();
    let mut ws = Workspace { name: String::new(), roots: Vec::new(), excludes: f.exclude, file: Some(canonical(file)), home: canonical(base) };
    for (d, label, color) in dirs {
        if let Err(e) = ws.add_root(&d, label, color) {
            notes.push(e);
        }
    }
    if ws.roots.is_empty() {
        return Err(format!("{}: no root folder found{}", file.display(), notes.first().map(|n| format!(" ({n})")).unwrap_or_default()));
    }
    ws.name = f.name.unwrap_or_else(|| ws.default_name());
    Ok((ws, notes))
}

/// Git repositories one level below `dir` (folders holding a `.git`), sorted by name; empty
/// when `dir` is a repository itself.
pub fn nested_repos(dir: &Path) -> Vec<PathBuf> {
    if dir.join(".git").exists() {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            !n.starts_with('.') && !crate::project::tree::skip_name(&n, true)
        })
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    out.sort_by_key(|p| dir_name(p).to_lowercase());
    out
}

/// What the command line opens.
pub struct Opened {
    pub ws: Workspace,
    pub files: Vec<PathBuf>,
    /// Opened as a project (a folder or workspace given, or no arguments): the tree shows.
    pub as_project: bool,
    /// For the status bar ("Workspace: 15 repos …", skipped roots).
    pub note: Option<String>,
}

/// Splits command-line arguments into the workspace and files to open. One folder (or none)
/// gives one root as always, unless it has `.code-atlas/workspace.toml` or holds several
/// repositories; several folders give several roots; a `*.catlas.toml` loads that workspace.
pub fn from_args(paths: Vec<PathBuf>) -> Opened {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let as_project = paths.is_empty() || paths.iter().any(|p| p.is_dir() || is_workspace_file(p));
    let ws_file = paths.iter().find(|p| is_workspace_file(p)).cloned();
    let dirs: Vec<PathBuf> = paths.iter().filter(|p| p.is_dir()).cloned().collect();
    let files: Vec<PathBuf> = paths.into_iter().filter(|p| !p.is_dir() && !is_workspace_file(p)).collect();
    let mut notes: Vec<String> = Vec::new();
    let mut ws = PRESET.lock().ok().and_then(|mut p| p.take());
    if let (None, Some(f)) = (&ws, &ws_file) {
        let base = canonical(f.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(&cwd));
        match load_file(f, &base) {
            Ok((w, n)) => {
                ws = Some(w);
                notes.extend(n);
            }
            Err(e) => notes.push(e),
        }
    }
    let single = SINGLE.load(Ordering::Relaxed);
    let mut ws = match ws {
        Some(mut w) => {
            for d in &dirs {
                if let Err(e) = w.add_root(d, None, None) {
                    notes.push(e);
                }
            }
            w
        }
        None if dirs.len() >= 2 => Workspace::from_dirs(None, canonical(&dirs[0]), dirs.iter().map(|d| (d.clone(), None, None)).collect()),
        None => {
            let dir = canonical(dirs.first().unwrap_or(&cwd));
            let opened_folder = !dirs.is_empty() || files.is_empty();
            let own = dir.join(DIR_FILE);
            let nested = if opened_folder && !single && !own.is_file() { nested_repos(&dir) } else { Vec::new() };
            if opened_folder && !single && own.is_file() {
                match load_file(&own, &dir) {
                    Ok((w, n)) => {
                        notes.extend(n);
                        w
                    }
                    Err(e) => {
                        notes.push(e);
                        Workspace::single(dir)
                    }
                }
            } else if nested.len() >= 2 {
                let n = nested.len();
                let w = Workspace::from_dirs(Some(dir_name(&dir)), dir.clone(), nested.into_iter().map(|d| (d, None, None)).collect());
                notes.insert(0, format!("Workspace: {n} repos in {} · Ctrl+O repos · catlas --single opens it as one folder", dir.display()));
                w
            } else {
                Workspace::single(dir)
            }
        }
    };
    if ws.roots.is_empty() {
        ws = Workspace::single(canonical(&cwd));
    }
    if ws.multi() && notes.is_empty() {
        notes.push(format!("Workspace {}: {} roots · Ctrl+O repos", ws.name, ws.roots.len()));
    }
    Opened { ws, files, as_project, note: (!notes.is_empty()).then(|| notes.join(" · ")) }
}

// ---- recent workspaces -----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentRoot {
    pub path: PathBuf,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub name: String,
    pub home: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub roots: Vec<RecentRoot>,
    #[serde(default)]
    pub opened: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    recent: Vec<Recent>,
}

/// `workspaces.toml` in the config folder.
pub fn recent_path() -> Option<PathBuf> {
    crate::lsp::config::config_dir().map(|d| d.join("workspaces.toml"))
}

pub fn load_recent(path: Option<&Path>) -> Vec<Recent> {
    path.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| toml::from_str::<RecentFile>(&t).ok()).map(|f| f.recent).unwrap_or_default()
}

impl Recent {
    pub fn of(ws: &Workspace) -> Recent {
        Recent {
            name: ws.name.clone(),
            home: ws.home.clone(),
            file: ws.file.clone(),
            roots: ws.roots.iter().map(|r| RecentRoot { path: r.path.clone(), label: r.label.clone() }).collect(),
            opened: crate::agents::run::now_secs(),
        }
    }

    /// Same workspace: same file, or (without one) the same roots.
    fn same(&self, other: &Recent) -> bool {
        match (&self.file, &other.file) {
            (Some(a), Some(b)) => a == b,
            (None, None) => {
                let mut a: Vec<&PathBuf> = self.roots.iter().map(|r| &r.path).collect();
                let mut b: Vec<&PathBuf> = other.roots.iter().map(|r| &r.path).collect();
                a.sort();
                b.sort();
                a == b
            }
            _ => false,
        }
    }

    /// The workspace again: from its file when it has one (labels, colours, excludes), else
    /// from the saved roots (missing folders are skipped).
    pub fn open(&self) -> Result<Workspace, String> {
        if let Some(f) = self.file.as_ref().filter(|f| f.is_file()) {
            let base = if f.ends_with(DIR_FILE) { f.parent().and_then(Path::parent) } else { f.parent() };
            return load_file(f, base.unwrap_or(&self.home)).map(|(w, _)| w);
        }
        let ws = Workspace::from_dirs(Some(self.name.clone()), self.home.clone(), self.roots.iter().map(|r| (r.path.clone(), Some(r.label.clone()), None)).collect());
        if ws.roots.is_empty() {
            return Err(format!("None of the folders of {} exist any more", self.name));
        }
        Ok(ws)
    }
}

/// Puts `ws` first in the recent list at `path` (only workspaces with several roots or a file).
pub fn remember(path: &Path, ws: &Workspace) -> std::io::Result<()> {
    if !ws.multi() && ws.file.is_none() {
        return Ok(());
    }
    let mut list = load_recent(Some(path));
    let me = Recent::of(ws);
    list.retain(|r| !r.same(&me) && !(r.name == me.name && r.home == me.home));
    list.insert(0, me);
    list.truncate(RECENT_MAX);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(&RecentFile { recent: list }).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

/// `catlas -w` / `catlas -w <n|name>`: lists the recent workspaces, or picks one.
pub fn pick_recent(list: &[Recent], arg: &str) -> Option<Recent> {
    if let Ok(n) = arg.parse::<usize>() {
        return list.get(n.checked_sub(1)?).cloned();
    }
    let a = arg.to_lowercase();
    list.iter().find(|r| r.name.to_lowercase() == a).or_else(|| list.iter().find(|r| r.name.to_lowercase().contains(&a))).cloned()
}

pub fn list_recent(list: &[Recent]) -> String {
    if list.is_empty() {
        return "No recent workspaces yet (open several folders, or a folder of repos, to make one)".into();
    }
    let now = crate::git::log::now();
    list.iter()
        .enumerate()
        .map(|(i, r)| {
            let labels: Vec<&str> = r.roots.iter().map(|x| x.label.as_str()).take(6).collect();
            let more = if r.roots.len() > 6 { format!(" +{}", r.roots.len() - 6) } else { String::new() };
            format!("{:>2}  {}  ({} roots: {}{more})  {} ago", i + 1, r.name, r.roots.len(), labels.join(", "), crate::git::log::age(now, r.opened as i64))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
