//! Language-server status for the Control Center (decision 0016): per configured language,
//! whether its server is running, starting, crashed, installed but idle, or missing (the same
//! picture as `catlas --health`, plus what is live in this editor), and restarting a server.
//! A child module of `lsp`, so it reads `Lsp`'s private state directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use serde_json::json;

use super::config;
use super::server;
use super::text::path_to_uri;
use crate::app::App;
use crate::syntax::Lang;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// Died or failed to start (in this editor run).
    Crashed,
    Running,
    /// Started, `initialize` not answered yet.
    Starting,
    /// A server is installed; it starts when a file of the language opens.
    Ready,
    /// None of the configured servers is on PATH.
    Missing,
    /// Highlighting only (no server configured, e.g. SQL).
    Unconfigured,
    /// Not probed yet.
    Unknown,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Crashed => "crashed",
            State::Running => "running",
            State::Starting => "starting",
            State::Ready => "ready",
            State::Missing => "missing",
            State::Unconfigured => "no server",
            State::Unknown => "…",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LangRow {
    /// The `languages.toml` key ("rust").
    pub lang: String,
    /// What the status bar calls it ("Rust").
    pub name: String,
    /// The server the state is about (the running one, the crashed one, or the first installed).
    pub server: Option<String>,
    pub state: State,
    pub pid: Option<u32>,
    /// A file of this language is open.
    pub open: bool,
    /// Other running `also` servers ("+ruff").
    pub extra: Vec<String>,
}

/// Which configured servers are installed, by name (PATH lookups, off the UI thread).
pub fn probe() -> Receiver<HashMap<String, bool>> {
    crate::git::job(|| {
        let cfg = config::get();
        cfg.servers.iter().map(|(n, s)| (n.clone(), server::which(&s.command).is_some())).collect()
    })
}

impl App {
    /// One row per configured language: crashed first, then running, open-but-missing, ready,
    /// missing, no server; by name within each. `installed` = the last [`probe`] (None: not yet).
    pub fn lsp_rows(&self, installed: Option<&HashMap<String, bool>>) -> Vec<LangRow> {
        let cfg = config::get();
        let mut rows = Vec::new();
        for (key, l) in &cfg.langs {
            let lang = Lang::from_key(key);
            let name = lang.map(|l| l.name().to_string()).unwrap_or_else(|| key.clone());
            let open = self.docs.iter().any(|d| d.path.is_some() && d.syntax.lang.key() == key);
            let names: Vec<&String> = l.servers.iter().chain(&l.also).collect();
            let live: Vec<&server::Server> = self.lsp.servers.iter().filter(|s| names.contains(&&s.name)).collect();
            let main = live.iter().find(|s| l.servers.contains(&s.name)).or(live.first());
            let crashed = names.iter().find(|n| self.lsp.dead.iter().any(|(d, _)| d == **n));
            let installed_first = |list: &[String]| installed.and_then(|m| list.iter().find(|n| m.get(*n).copied().unwrap_or(false)).cloned());
            let (state, server, pid) = if let Some(s) = main {
                (if s.ready { State::Running } else { State::Starting }, Some(s.name.clone()), Some(s.pid()))
            } else if let Some(n) = crashed {
                (State::Crashed, Some((*n).clone()), None)
            } else if l.servers.is_empty() && l.also.is_empty() {
                (State::Unconfigured, None, None)
            } else if lang.is_some_and(|g| self.lsp.no_server.contains(&g)) {
                (State::Missing, l.servers.first().cloned(), None)
            } else if installed.is_none() {
                (State::Unknown, l.servers.first().cloned(), None)
            } else if let Some(n) = installed_first(&l.servers).or_else(|| installed_first(&l.also)) {
                (State::Ready, Some(n), None)
            } else {
                (State::Missing, l.servers.first().cloned(), None)
            };
            let extra = live.iter().filter(|s| Some(&s.name) != server.as_ref()).map(|s| format!("+{}", s.name)).collect();
            rows.push(LangRow { lang: key.clone(), name, server, state, pid, open, extra });
        }
        let rank = |r: &LangRow| match r.state {
            State::Crashed => 0,
            State::Running | State::Starting => 1,
            State::Missing if r.open => 2,
            _ if r.open => 2,
            State::Ready => 3,
            State::Unknown => 4,
            State::Missing => 5,
            State::Unconfigured => 6,
        };
        rows.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));
        rows
    }

    /// Process ids of the running servers.
    pub fn lsp_pids(&self) -> Vec<u32> {
        self.lsp.servers.iter().map(|s| s.pid()).collect()
    }

    /// Restarts every running instance of server `name` (and forgets that it crashed): its
    /// files are opened again on the next sync, which starts it afresh. Returns a status line.
    pub fn lsp_restart(&mut self, name: &str) -> String {
        let ids: Vec<usize> = self.lsp.servers.iter().filter(|s| s.name == name).map(|s| s.id).collect();
        let was_dead = self.lsp.dead.iter().any(|(n, _)| n == name);
        for id in &ids {
            if let Some(i) = self.lsp.servers.iter().position(|s| s.id == *id) {
                let mut s = self.lsp.servers.remove(i);
                s.shutdown();
                s.kill();
            }
            self.lsp.progress.remove(&format!("start-{id}"));
            for v in self.lsp.diagnostics.values_mut() {
                v.retain(|g| g.server != *id);
            }
        }
        self.lsp.progress.retain(|_, p| p.0 != name);
        // Files those servers had: closed on the others too, so the next sync opens them on all.
        let keys: Vec<PathBuf> = self.lsp.open.iter().filter(|(_, od)| od.servers.iter().any(|s| ids.contains(s))).map(|(k, _)| k.clone()).collect();
        for k in keys {
            let od = self.lsp.open.remove(&k).expect("listed above");
            for sid in od.servers.into_iter().filter(|s| !ids.contains(s)) {
                if let Some(s) = self.lsp.server(sid) {
                    s.notify("textDocument/didClose", json!({ "textDocument": { "uri": path_to_uri(&k) } }));
                }
            }
        }
        self.lsp.dead.retain(|(n, _)| n != name);
        let cfg = config::get();
        let langs: Vec<Lang> = cfg.langs.iter().filter(|(_, l)| l.servers.iter().chain(&l.also).any(|s| s == name)).filter_map(|(k, _)| Lang::from_key(k)).collect();
        self.lsp.no_server.retain(|l| !langs.contains(l));
        let msg = self.message.take();
        self.lsp_sync(None);
        let started = self.lsp.servers.iter().any(|s| s.name == name);
        let start_msg = self.message.take().filter(|m| m.starts_with("Couldn't") || m.starts_with("No "));
        self.message = msg;
        match (ids.is_empty() && !was_dead, started, start_msg) {
            (_, _, Some(m)) => m,
            (true, false, None) => format!("{name} isn't running · it starts when one of its files is open"),
            (_, true, None) => format!("Restarted {name}"),
            (false, false, None) => format!("Stopped {name} · it starts again when one of its files is open"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_cover_every_language_and_restart_explains_itself() {
        let dir = std::env::temp_dir().join(format!("catlas-lsp-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("q.sql"), "select 1;\n").unwrap();
        let mut app = App::new(vec![dir.join("q.sql")]).unwrap();
        let installed: HashMap<String, bool> = config::get().servers.keys().map(|k| (k.clone(), k == "gopls")).collect();
        let rows = app.lsp_rows(Some(&installed));
        assert_eq!(rows.len(), config::get().langs.len());
        let row = |k: &str| rows.iter().find(|r| r.lang == k).unwrap().clone();
        assert_eq!(row("go").state, State::Ready);
        assert_eq!(row("go").server.as_deref(), Some("gopls"));
        assert_eq!(row("rust").state, State::Missing);
        let sql = row("sql");
        assert!(sql.open && sql.state == State::Unconfigured, "{sql:?}");
        assert_eq!(rows[0].lang, "sql", "an open file's language comes before idle ones");
        assert!(app.lsp_rows(None).iter().any(|r| r.state == State::Unknown));
        // A crashed server shows, and a restart forgets it.
        app.lsp.dead.insert(("gopls".into(), dir.clone()));
        assert_eq!(app.lsp_rows(Some(&installed))[0].state, State::Crashed);
        let msg = app.lsp_restart("gopls");
        assert!(msg.contains("gopls"), "{msg}");
        assert!(app.lsp.dead.is_empty());
        assert_eq!(app.lsp_restart("nothing-here"), "nothing-here isn't running · it starts when one of its files is open");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
