//! LSP client: one server process per (language server, project root), started when a file of
//! that language is open; which servers comes from `languages.toml` ([`config`]). A file has a
//! main server (all requests) and optionally extra ones (`also`: their diagnostics and quick fixes
//! too). Full-text sync (didOpen / didChange after a short pause in typing / didSave / didClose),
//! diagnostics, completion with snippets, signature help, go to definition / implementation,
//! usages, file structure, workspace symbols, hover, rename, format and quick fixes. JSON-RPC runs
//! over stdio in background threads; [`App::lsp_tick`] handles what arrived, once per loop turn.
//!
//! Keys (IntelliJ): Ctrl+Space complete (Ctrl+J on a Mac; it also opens by itself while typing
//! and after the server's trigger characters, see [`auto`]; Tab / Shift+Tab walk a snippet's
//! placeholders), Ctrl+B go to definition, Ctrl+Alt+B implementations, Ctrl+Alt+←
//! back, Alt+F7 usages, Ctrl+F12 file structure, Ctrl+Alt+Shift+N (Alt+N) symbol in the
//! workspace, F2 / Shift+F2 next / previous problem, Alt+6 the file's problems, F1 quick
//! documentation (IDEA's Ctrl+Q is quit here), Shift+F6 rename, Ctrl+Alt+L reformat, Alt+Enter
//! quick fixes; signature help opens by itself after `(` and `,`.

pub mod auto;
pub mod config;
pub mod health;
pub mod jsonrpc;
pub mod popup;
pub mod server;
pub mod snippet;
pub mod text;
pub mod words;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lsp_types::{
    DocumentChangeOperation, DocumentChanges, GotoDefinitionResponse, Hover, HoverContents,
    MarkedString, OneOf, Position, Range, TextEdit, WorkspaceEdit,
};
use serde_json::{json, Value};

use crate::app::{App, Prompt, PromptKind};
use crate::doc::Doc;
use crate::syntax::Lang;
use jsonrpc::Incoming;
use popup::{PickItem, PickKind, Picker, Popup, Signature};
use server::{Event, Pending, Server};
use text::{apply_edits, from_lsp, path_to_uri, to_lsp, uri_to_path};

/// Pause in typing before the new text goes to the server (requests send it at once).
const CHANGE_DELAY: Duration = Duration::from_millis(150);

struct OpenDoc {
    /// Main server first, then the `also` ones.
    servers: Vec<usize>,
    version: i32,
    sent_rev: u64,
    seen_rev: u64,
    seen_at: Instant,
}

pub struct Diag {
    pub start: Position,
    pub end: Position,
    /// 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
    raw: Value,
    server: usize,
}

/// Tab stops of an inserted snippet (absolute char offsets), walked with Tab / Shift+Tab.
struct SnippetSession {
    doc: usize,
    stops: Vec<(usize, usize)>,
    cur: usize,
    /// Document length after the last step, to see edits.
    len: usize,
}

pub struct Lsp {
    servers: Vec<Server>,
    next_server: usize,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    /// Languages with no server on PATH, and servers that died (not restarted).
    no_server: HashSet<Lang>,
    dead: HashSet<(String, PathBuf)>,
    canon: HashMap<PathBuf, PathBuf>,
    open: HashMap<PathBuf, OpenDoc>,
    pub diagnostics: HashMap<PathBuf, Vec<Diag>>,
    pub popup: Option<Popup>,
    /// Work-done progress per token: (server, title, latest text).
    progress: HashMap<String, (String, String, String)>,
    back: Vec<(PathBuf, usize)>,
    pub signature: Option<Signature>,
    /// Revision the signature help was last asked for.
    sig_rev: u64,
    snippet: Option<SnippetSession>,
    /// Code-action answers still expected (several servers may answer).
    actions_waiting: usize,
    seq: u64,
    /// Completion opens by itself while typing (`autocomplete` in `settings.toml`).
    pub auto: bool,
    /// An automatic completion request waiting for a pause in typing.
    auto_at: Option<auto::AutoReq>,
    /// Number of the newest completion request (older answers are dropped).
    comp_seq: u64,
    /// (doc, word start) whose popup was closed with Esc: it doesn't open by itself again.
    auto_off: Option<(usize, usize)>,
}

impl Default for Lsp {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Lsp {
            servers: Vec::new(),
            next_server: 1,
            tx,
            rx,
            no_server: HashSet::new(),
            dead: HashSet::new(),
            canon: HashMap::new(),
            open: HashMap::new(),
            diagnostics: HashMap::new(),
            popup: None,
            progress: HashMap::new(),
            back: Vec::new(),
            signature: None,
            sig_rev: 0,
            snippet: None,
            actions_waiting: 0,
            seq: 0,
            auto: true,
            auto_at: None,
            comp_seq: 0,
            auto_off: None,
        }
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        if self.servers.is_empty() {
            return;
        }
        for s in &mut self.servers {
            s.shutdown();
        }
        std::thread::sleep(Duration::from_millis(150));
        for s in &mut self.servers {
            s.kill();
        }
    }
}

impl Lsp {
    /// The canonical path used as key (and in URIs).
    fn key(&mut self, path: &Path) -> PathBuf {
        if let Some(k) = self.canon.get(path) {
            return k.clone();
        }
        let k = uri_to_path(&path_to_uri(path)).unwrap_or_else(|| path.to_path_buf());
        if path.exists() {
            self.canon.insert(path.to_path_buf(), k.clone());
        }
        k
    }

    pub fn picker_open(&self) -> bool {
        matches!(self.popup, Some(Popup::Picker(_)))
    }

    fn server(&mut self, id: usize) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.id == id)
    }
}

pub(crate) fn initialize_params(root: &Path, init_options: &Value) -> Value {
    let uri = path_to_uri(root);
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "Code Atlas", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": uri,
        "rootPath": root.display().to_string(),
        "workspaceFolders": [{ "uri": uri, "name": name }],
        "initializationOptions": if init_options.is_null() { json!({}) } else { init_options.clone() },
        "capabilities": {
            "general": { "positionEncodings": ["utf-16"] },
            "textDocument": {
                "synchronization": { "didSave": true, "dynamicRegistration": false },
                "completion": {
                    "completionItem": { "snippetSupport": true, "documentationFormat": ["plaintext", "markdown"], "labelDetailsSupport": true },
                    "contextSupport": true
                },
                "signatureHelp": {
                    "signatureInformation": {
                        "documentationFormat": ["plaintext", "markdown"],
                        "parameterInformation": { "labelOffsetSupport": true },
                        "activeParameterSupport": true
                    },
                    "contextSupport": true
                },
                "references": {},
                "implementation": { "linkSupport": true },
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "diagnostic": { "dynamicRegistration": false, "relatedDocumentSupport": false },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "definition": { "linkSupport": true },
                "rename": { "prepareSupport": false },
                "formatting": {},
                "publishDiagnostics": { "relatedInformation": false },
                "codeAction": {
                    "codeActionLiteralSupport": { "codeActionKind": { "valueSet": [
                        "", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"
                    ] } },
                    "resolveSupport": { "properties": ["edit"] },
                    "dataSupport": true,
                    "isPreferredSupport": true
                }
            },
            "workspace": {
                "applyEdit": true,
                "workspaceEdit": { "documentChanges": true },
                "configuration": true,
                "workspaceFolders": true,
                "symbol": {},
                "diagnostics": { "refreshSupport": true }
            },
            "window": { "workDoneProgress": true }
        }
    })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Word characters of a language: `$` belongs to names in JS/TS/Java/Kotlin, but starts a
/// variable in shell scripts (`$TARGET` completes `TARGET`).
fn is_word_in(lang: Lang, c: char) -> bool {
    c.is_alphanumeric() || c == '_' || (c == '$' && matches!(lang, Lang::TypeScript | Lang::Tsx | Lang::JavaScript | Lang::Java | Lang::Kotlin))
}

fn word_start(d: &Doc) -> usize {
    let mut s = d.cursor;
    while s > 0 && is_word_in(d.syntax.lang, d.rope.char(s - 1)) {
        s -= 1;
    }
    s
}

fn word_at(d: &Doc) -> String {
    let mut e = d.cursor;
    while e < d.rope.len_chars() && is_word_in(d.syntax.lang, d.rope.char(e)) {
        e += 1;
    }
    d.rope.slice(word_start(d)..e).to_string()
}

impl App {
    // ---- event loop ------------------------------------------------------------------------

    /// Handles server messages and sends text changes. Returns whether the screen may have changed.
    pub fn lsp_tick(&mut self) -> bool {
        let mut changed = false;
        while let Ok((id, msg)) = self.lsp.rx.try_recv() {
            changed = true;
            match msg {
                Some(m) => self.on_message(id, m),
                None => self.on_exit(id),
            }
        }
        self.lsp_sync(None);
        changed |= self.auto_tick(Instant::now());
        changed
    }

    /// Opens, changes and closes documents on their servers. `now` = doc index to send at once.
    fn lsp_sync(&mut self, now: Option<usize>) {
        let t = Instant::now();
        let mut live = HashSet::new();
        for i in 0..self.docs.len() {
            let Some(path) = self.docs[i].path.clone() else { continue };
            let lang = self.docs[i].syntax.lang;
            if !config::get().has_servers(lang.key()) || self.lsp.no_server.contains(&lang) {
                continue;
            }
            let key = self.lsp.key(&path);
            live.insert(key.clone());
            let rev = self.docs[i].rev();
            let uri = path_to_uri(&key);
            if let Some(od) = self.lsp.open.get_mut(&key) {
                if od.seen_rev != rev {
                    od.seen_rev = rev;
                    od.seen_at = t;
                }
                if od.sent_rev != rev && (now == Some(i) || t - od.seen_at >= CHANGE_DELAY) {
                    od.version += 1;
                    od.sent_rev = rev;
                    let (sids, version) = (od.servers.clone(), od.version);
                    let text = self.docs[i].rope.to_string();
                    for sid in sids {
                        if let Some(s) = self.lsp.server(sid) {
                            s.notify("textDocument/didChange", json!({
                                "textDocument": { "uri": uri, "version": version },
                                "contentChanges": [{ "text": text }]
                            }));
                        }
                        self.pull_diags(sid, &key);
                    }
                }
                continue;
            }
            let sids = self.servers_for(lang, &key);
            if sids.is_empty() {
                continue;
            }
            let text = self.docs[i].rope.to_string();
            let lang_id = lang.lsp_id(Some(&key));
            for &sid in &sids {
                if let Some(s) = self.lsp.server(sid) {
                    s.notify("textDocument/didOpen", json!({
                        "textDocument": { "uri": uri, "languageId": lang_id, "version": 1, "text": text }
                    }));
                }
            }
            self.lsp.open.insert(key.clone(), OpenDoc { servers: sids.clone(), version: 1, sent_rev: rev, seen_rev: rev, seen_at: t });
            for sid in sids {
                self.pull_diags(sid, &key);
            }
        }
        let gone: Vec<PathBuf> = self.lsp.open.keys().filter(|k| !live.contains(*k)).cloned().collect();
        for k in gone {
            let od = self.lsp.open.remove(&k).unwrap();
            self.lsp.diagnostics.remove(&k);
            for sid in od.servers {
                if let Some(s) = self.lsp.server(sid) {
                    s.notify("textDocument/didClose", json!({ "textDocument": { "uri": path_to_uri(&k) } }));
                }
            }
        }
    }

    /// The running servers for a file (main first), starting them if needed.
    fn servers_for(&mut self, lang: Lang, key: &Path) -> Vec<usize> {
        let cfg = config::get();
        let roots = cfg.langs.get(lang.key()).map(|l| l.roots.clone()).unwrap_or_default();
        let root = server::find_root(key, &roots);
        // A server that failed here before is skipped, so the next one in the list gets a go.
        let dead = &self.lsp.dead;
        let sel = cfg.select(lang.key(), |s| if dead.contains(&(s.name.clone(), root.clone())) { None } else { server::which(&s.command) });
        let main_missing = sel.main.is_none();
        let picked: Vec<_> = sel.main.into_iter().chain(sel.also).collect();
        if picked.is_empty() {
            self.lsp.no_server.insert(lang);
            if self.message.as_deref().is_none_or(|m| !m.starts_with("Couldn't")) {
                self.message = Some(format!("No language server for {} on PATH ({}): highlighting only · catlas --health", lang.name(), sel.tried.join(", ")));
            }
            return Vec::new();
        }
        if main_missing && self.message.as_deref().is_none_or(|m| !m.starts_with("Couldn't")) {
            self.message = Some(format!("No main language server for {} ({}); running {} only", lang.name(), sel.tried.join(", "), picked[0].0.name));
        }
        let mut ids = Vec::new();
        for (cfg, exe) in picked {
            if let Some(s) = self.lsp.servers.iter().find(|s| s.name == cfg.name && s.root == root) {
                ids.push(s.id);
                continue;
            }
            let id = self.lsp.next_server;
            self.lsp.next_server += 1;
            match Server::start(id, &exe, &cfg, &root, self.lsp.tx.clone()) {
                Ok(s) => {
                    self.lsp.progress.insert(format!("start-{id}"), (s.name.clone(), "starting".into(), String::new()));
                    self.lsp.servers.push(s);
                    ids.push(id);
                }
                Err(e) => {
                    self.lsp.dead.insert((cfg.name.clone(), root.clone()));
                    self.message = Some(format!("Couldn't start {}: {e}", cfg.command));
                }
            }
        }
        ids
    }

    /// A server that refused to start: stop it, keep the error message.
    fn on_exit_quiet(&mut self, id: usize) {
        let msg = self.message.take();
        self.on_exit(id);
        self.message = msg;
    }

    /// Asks a pull-diagnostics server for a file's problems (no-op for push servers).
    fn pull_diags(&mut self, sid: usize, key: &Path) {
        let uri = path_to_uri(key);
        if let Some(s) = self.lsp.server(sid).filter(|s| s.ready && s.pulls_diagnostics()) {
            s.request("textDocument/diagnostic", json!({ "textDocument": { "uri": uri } }), Pending::PullDiags { path: key.to_path_buf() });
        }
    }

    /// Pulls diagnostics for every open file of server `sid`.
    fn pull_all(&mut self, sid: usize) {
        let keys: Vec<PathBuf> = self.lsp.open.iter().filter(|(_, o)| o.servers.contains(&sid)).map(|(k, _)| k.clone()).collect();
        for k in keys {
            self.pull_diags(sid, &k);
        }
    }

    /// New diagnostics of server `id` for `path` (pushed or pulled).
    fn set_diags(&mut self, id: usize, path: PathBuf, list: Vec<Value>) {
        let name = self.lsp.server(id).map(|s| s.name.clone()).unwrap_or_default();
        let diags = list
            .into_iter()
            .filter_map(|raw| {
                let d: lsp_types::Diagnostic = serde_json::from_value(raw.clone()).ok()?;
                let severity = match d.severity {
                    Some(lsp_types::DiagnosticSeverity::ERROR) | None => 1,
                    Some(lsp_types::DiagnosticSeverity::WARNING) => 2,
                    Some(lsp_types::DiagnosticSeverity::INFORMATION) => 3,
                    _ => 4,
                };
                let message = match &d.source {
                    Some(src) if src != &name && !d.message.contains(src.as_str()) => format!("{}  [{src}]", d.message),
                    _ => d.message,
                };
                Some(Diag { start: d.range.start, end: d.range.end, severity, message, raw, server: id })
            })
            .collect::<Vec<_>>();
        let all = self.lsp.diagnostics.entry(path).or_default();
        all.retain(|g| g.server != id);
        all.extend(diags);
        all.sort_by_key(|g| (g.start.line, g.start.character));
    }

    fn on_exit(&mut self, id: usize) {
        self.lsp.progress.remove(&format!("start-{id}"));
        let Some(i) = self.lsp.servers.iter().position(|s| s.id == id) else { return };
        let mut s = self.lsp.servers.remove(i);
        s.kill();
        let tail = s.stderr_tail.lock().unwrap().clone();
        self.lsp.dead.insert((s.name.clone(), s.root.clone()));
        for od in self.lsp.open.values_mut() {
            od.servers.retain(|&s| s != id);
        }
        self.lsp.open.retain(|_, od| !od.servers.is_empty());
        for v in self.lsp.diagnostics.values_mut() {
            v.retain(|g| g.server != id);
        }
        self.lsp.progress.retain(|_, p| p.0 != s.name);
        self.message = Some(format!("Couldn't keep {} running: {}", s.name, if tail.is_empty() { "it exited".into() } else { tail }));
    }

    fn on_message(&mut self, id: usize, m: Incoming) {
        match m {
            Incoming::Response { id: rid, result } => {
                let Some(s) = self.lsp.server(id) else { return };
                let name = s.name.clone();
                let Some(p) = s.pending.remove(&rid) else { return };
                match result {
                    Ok(v) => self.on_response(id, p, v),
                    Err(e) => {
                        if let Pending::CodeActions = p {
                            self.lsp.actions_waiting = self.lsp.actions_waiting.saturating_sub(1);
                        }
                        if let Pending::Initialize = p {
                            self.message = Some(format!("Couldn't start {name}: {e}"));
                            self.on_exit_quiet(id);
                        } else if !matches!(p, Pending::Ignore | Pending::Completion { .. } | Pending::Signature { .. } | Pending::Symbols { .. } | Pending::CodeActions | Pending::PullDiags { .. })
                            && !e.contains("content modified")
                            && !e.contains("cancel")
                        {
                            self.message = Some(format!("Couldn't ask {name}: {e}"));
                        }
                    }
                }
            }
            Incoming::Request { id: rid, method, params } => {
                let result = match method.as_str() {
                    "workspace/configuration" => {
                        let items = params.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                        let cfg = self.lsp.server(id).map(|s| s.cfg.clone()).unwrap_or_default();
                        Value::Array(items.iter().map(|it| cfg.section(it.get("section").and_then(Value::as_str))).collect())
                    }
                    "workspace/applyEdit" => {
                        let edit = params.get("edit").cloned().and_then(|e| serde_json::from_value::<WorkspaceEdit>(e).ok());
                        let n = edit.map_or(0, |e| self.apply_workspace_edit(e));
                        json!({ "applied": n > 0 })
                    }
                    "workspace/workspaceFolders" => Value::Null,
                    "workspace/diagnostic/refresh" => {
                        self.pull_all(id);
                        Value::Null
                    }
                    _ => Value::Null,
                };
                if let Some(s) = self.lsp.server(id) {
                    s.reply(rid, result);
                }
            }
            Incoming::Notification { method, params } => self.on_notification(id, &method, params),
        }
    }

    fn on_notification(&mut self, id: usize, method: &str, params: Value) {
        let name = self.lsp.server(id).map(|s| s.name.clone()).unwrap_or_default();
        match method {
            "textDocument/publishDiagnostics" => {
                let Some(path) = params.get("uri").and_then(Value::as_str).and_then(uri_to_path) else { return };
                let list = params.get("diagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
                self.set_diags(id, path, list);
            }
            "$/progress" => {
                let token = params.get("token").map(|t| t.to_string()).unwrap_or_default();
                let v = params.get("value").cloned().unwrap_or(Value::Null);
                let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let pct = v.get("percentage").and_then(Value::as_u64).map(|p| format!(" {p}%")).unwrap_or_default();
                self.lsp.progress.retain(|k, p| !(k.starts_with("start-") && p.0 == name));
                match s("kind").as_str() {
                    "begin" => {
                        self.lsp.progress.insert(token, (name, s("title"), format!("{}{pct}", s("message"))));
                    }
                    "report" => {
                        if let Some(p) = self.lsp.progress.get_mut(&token) {
                            p.2 = format!("{}{pct}", s("message"));
                        }
                    }
                    _ => {
                        self.lsp.progress.remove(&token);
                    }
                }
            }
            "window/showMessage" => {
                let kind = params.get("type").and_then(Value::as_u64).unwrap_or(4);
                // Unprompted server news waits for a free status bar: it must not hide the
                // editor's own feedback ("Saved …", "Not found …").
                if kind <= 2 && self.message.is_none() {
                    let msg = params.get("message").and_then(Value::as_str).unwrap_or("").lines().next().unwrap_or("").to_string();
                    self.message = Some(format!("{name}: {msg}"));
                }
            }
            _ => {}
        }
    }

    fn on_response(&mut self, sid: usize, p: Pending, v: Value) {
        match p {
            Pending::Initialize => {
                let caps = v.get("capabilities").cloned().unwrap_or(Value::Null);
                if let Some(s) = self.lsp.server(sid) {
                    s.initialized(caps);
                    let key = format!("start-{sid}");
                    self.lsp.progress.remove(&key);
                }
                self.pull_all(sid);
            }
            Pending::Completion { path, start, req, seq, auto } => self.on_completion(path, start, req, seq, auto, v),
            Pending::Definition { what, word } => {
                let locs: Vec<(String, Position)> = match serde_json::from_value::<Option<GotoDefinitionResponse>>(v).ok().flatten() {
                    Some(GotoDefinitionResponse::Scalar(l)) => vec![(l.uri.to_string(), l.range.start)],
                    Some(GotoDefinitionResponse::Array(a)) => a.into_iter().map(|l| (l.uri.to_string(), l.range.start)).collect(),
                    Some(GotoDefinitionResponse::Link(a)) => a.into_iter().map(|l| (l.target_uri.to_string(), l.target_selection_range.start)).collect(),
                    None => Vec::new(),
                };
                let locs: Vec<(PathBuf, Position)> = locs.into_iter().filter_map(|(u, p)| uri_to_path(&u).map(|path| (path, p))).collect();
                match locs.len() {
                    0 => self.message = Some(format!("No {what} found here")),
                    1 => self.jump_to(&locs[0].0, locs[0].1),
                    n => {
                        let items = self.location_items(locs);
                        let title = if what == "definition" { format!("Choose declaration of {word}") } else { format!("Implementations of {word}") };
                        self.lsp.popup = Some(Popup::Picker(Picker::new(PickKind::Locations, format!("{title} ({n})"), items)));
                    }
                }
            }
            Pending::References { word } => {
                let locs = serde_json::from_value::<Option<Vec<lsp_types::Location>>>(v).ok().flatten().unwrap_or_default();
                let locs: Vec<(PathBuf, Position)> = locs.into_iter().filter_map(|l| uri_to_path(l.uri.as_str()).map(|p| (p, l.range.start))).collect();
                if locs.is_empty() {
                    self.message = Some(format!("No usages of {word} found"));
                } else {
                    let n = locs.len();
                    let items = self.location_items(locs);
                    let files = items.iter().map(|i| &i.path).collect::<HashSet<_>>().len();
                    self.lsp.popup = Some(Popup::Picker(Picker::new(PickKind::Usages, format!("Usages of {word} · {n} in {files} file(s)"), items)));
                }
            }
            Pending::Structure { path } => {
                let mut items = Vec::new();
                for s in v.as_array().cloned().unwrap_or_default() {
                    flatten_symbol(&s, &path, 0, &mut items);
                }
                if items.is_empty() {
                    self.message = Some("No symbols in this file".into());
                    return;
                }
                let here = (self.doc().path.clone().map(|p| self.lsp.key(&p)) == Some(path.clone())).then(|| to_lsp(&self.doc().rope, self.doc().cursor));
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut p = Picker::new(PickKind::Structure, format!("Structure of {name}"), items);
                if let Some(c) = here {
                    // The innermost symbol that starts before the cursor.
                    p.sel = p.items.iter().enumerate().filter(|(_, it)| (it.pos.line, it.pos.character) <= (c.line, c.character)).max_by_key(|(_, it)| (it.pos.line, it.pos.character)).map_or(0, |(i, _)| i);
                }
                self.lsp.popup = Some(Popup::Picker(p));
            }
            Pending::Symbols { seq } => {
                let root = self.project.root.clone();
                let Some(Popup::Picker(p)) = &mut self.lsp.popup else { return };
                if p.kind != PickKind::Symbols || p.seq != seq {
                    return;
                }
                let mut items = Vec::new();
                for s in v.as_array().cloned().unwrap_or_default() {
                    let Some(path) = s.pointer("/location/uri").and_then(Value::as_str).and_then(uri_to_path) else { continue };
                    let pos = s.pointer("/location/range/start").and_then(|p| serde_json::from_value(p.clone()).ok()).unwrap_or_default();
                    let container = s.get("containerName").and_then(Value::as_str).unwrap_or("");
                    let file = rel(&root, &path);
                    let detail = if container.is_empty() { format!("{file}:{}", pos_line(pos)) } else { format!("{container} · {file}:{}", pos_line(pos)) };
                    items.push(PickItem {
                        kind: symbol_kind(s.get("kind").and_then(Value::as_u64).unwrap_or(0)),
                        label: s.get("name").and_then(Value::as_str).unwrap_or("?").to_string(),
                        detail,
                        path,
                        pos,
                        depth: 0,
                    });
                }
                items.truncate(500);
                p.items = items;
                p.sel = 0;
                p.refilter();
            }
            Pending::PullDiags { path } => {
                // "unchanged" reports keep what we have.
                if v.get("kind").and_then(Value::as_str) == Some("full") {
                    let items = v.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                    self.set_diags(sid, path, items);
                }
            }
            Pending::Signature { path, line } => {
                let here = self.doc().path.clone().map(|p| self.lsp.key(&p));
                if here.as_ref() != Some(&path) || self.doc().line() != line {
                    return;
                }
                self.lsp.signature = serde_json::from_value::<Option<lsp_types::SignatureHelp>>(v).ok().flatten().and_then(|h| signature(h, line));
            }
            Pending::Hover => {
                let text = match serde_json::from_value::<Option<Hover>>(v).ok().flatten().map(|h| h.contents) {
                    Some(HoverContents::Markup(m)) => m.value,
                    Some(HoverContents::Scalar(s)) => marked(s),
                    Some(HoverContents::Array(a)) => a.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
                    None => String::new(),
                };
                let lines = popup::hover_lines(&text, 78);
                if lines.is_empty() {
                    self.message = Some("No documentation here".into());
                } else {
                    self.lsp.popup = Some(Popup::Hover { lines, scroll: 0 });
                }
            }
            Pending::Rename => match serde_json::from_value::<Option<WorkspaceEdit>>(v).ok().flatten() {
                Some(e) => {
                    let n = self.apply_workspace_edit(e);
                    self.message = Some(format!("Renamed in {n} file(s); unsaved tabs are marked ●"));
                }
                None => self.message = Some("Nothing to rename here".into()),
            },
            Pending::Format { path } => {
                let Some(edits) = serde_json::from_value::<Option<Vec<TextEdit>>>(v).ok().flatten() else {
                    // rust-analyzer answers null when rustfmt is missing or fails.
                    self.message = Some("Server didn't format this file (formatter missing or a syntax error?)".into());
                    return;
                };
                if let Some(i) = self.doc_index(&path) {
                    let before = self.docs[i].rope.clone();
                    apply_edits(&mut self.docs[i], &edits);
                    self.message = Some(if self.docs[i].rope == before { "Already formatted".into() } else { "Reformatted".into() });
                    self.follow = true;
                }
            }
            Pending::CodeActions => {
                self.lsp.actions_waiting = self.lsp.actions_waiting.saturating_sub(1);
                let new: Vec<(usize, Value)> = v.as_array().cloned().unwrap_or_default().into_iter().map(|a| (sid, a)).collect();
                let mut items = match self.lsp.popup.take() {
                    Some(Popup::Actions { items, .. }) => items,
                    other => {
                        self.lsp.popup = other;
                        Vec::new()
                    }
                };
                items.extend(new);
                // Preferred fixes first, as IntelliJ shows them (stable: main server's first).
                items.sort_by_key(|(_, a)| !a.get("isPreferred").and_then(Value::as_bool).unwrap_or(false));
                if !items.is_empty() {
                    self.lsp.popup = Some(Popup::Actions { items, sel: 0 });
                } else if self.lsp.actions_waiting == 0 && self.lsp.popup.is_none() {
                    self.message = Some("No quick fixes here".into());
                }
            }
            Pending::ResolveAction => self.run_action(sid, v, false),
            Pending::Ignore => {}
        }
    }

    // ---- requests --------------------------------------------------------------------------

    /// Server and URI of the active doc, after sending it its latest text.
    fn active_server(&mut self) -> Option<(usize, String, PathBuf)> {
        let path = self.doc().path.clone()?;
        let lang = self.doc().syntax.lang;
        if !config::get().has_servers(lang.key()) {
            self.message = Some(format!("No code smarts for {} files", lang.name()));
            return None;
        }
        self.lsp_sync(Some(self.active));
        let key = self.lsp.key(&path);
        let Some(od) = self.lsp.open.get(&key) else {
            if !self.lsp.no_server.contains(&lang) && self.message.is_none() {
                self.message = Some(format!("No language server running for {}", lang.name()));
            }
            return None;
        };
        let sid = od.servers[0];
        if self.lsp.server(sid).is_some_and(|s| !s.ready) {
            self.message = Some("Language server is still starting…".into());
        }
        Some((sid, path_to_uri(&key), key))
    }

    /// False (with a message, except for automatic requests) when the started server says it
    /// can't do `method`.
    fn can(&mut self, sid: usize, method: &str) -> bool {
        let (cap, what) = match method {
            "textDocument/definition" => ("definitionProvider", "go to definition"),
            "textDocument/implementation" => ("implementationProvider", "go to implementation"),
            "textDocument/references" => ("referencesProvider", "find usages"),
            "textDocument/hover" => ("hoverProvider", "quick documentation"),
            "textDocument/rename" => ("renameProvider", "rename"),
            "textDocument/formatting" => ("documentFormattingProvider", "reformat"),
            "textDocument/documentSymbol" => ("documentSymbolProvider", "file structure"),
            "workspace/symbol" => ("workspaceSymbolProvider", "symbol search"),
            "textDocument/signatureHelp" => ("signatureHelpProvider", ""),
            "textDocument/completion" => ("completionProvider", ""),
            _ => return true,
        };
        let Some(s) = self.lsp.server(sid) else { return false };
        if !s.ready {
            return true;
        }
        let ok = s.caps.get(cap).is_some_and(|c| !c.is_null() && c != &json!(false));
        if !ok && !what.is_empty() {
            let name = s.name.clone();
            self.message = Some(format!("{name} has no {what}"));
        }
        ok
    }

    fn request_here(&mut self, method: &str, mut extra: Value, p: Pending) {
        let Some((sid, uri, _)) = self.active_server() else { return };
        if !self.can(sid, method) {
            return;
        }
        let pos = to_lsp(&self.doc().rope, self.doc().cursor);
        let mut params = json!({ "textDocument": { "uri": uri }, "position": pos });
        if let (Some(o), Some(e)) = (params.as_object_mut(), extra.as_object_mut()) {
            o.append(e);
        }
        if let Some(s) = self.lsp.server(sid) {
            s.request(method, params, p);
        }
    }

    pub fn lsp_rename(&mut self, new_name: String) {
        if new_name.trim().is_empty() {
            return;
        }
        self.request_here("textDocument/rename", json!({ "newName": new_name.trim() }), Pending::Rename);
    }

    fn request_format(&mut self) {
        let Some((sid, uri, key)) = self.active_server() else { return };
        if !self.can(sid, "textDocument/formatting") {
            return;
        }
        let params = json!({ "textDocument": { "uri": uri }, "options": { "tabSize": crate::doc::TAB, "insertSpaces": true } });
        if let Some(s) = self.lsp.server(sid) {
            s.request("textDocument/formatting", params, Pending::Format { path: key });
        }
    }

    fn request_actions(&mut self) {
        let Some((sid, uri, key)) = self.active_server() else { return };
        let d = self.doc();
        let (s, e) = d.selection().unwrap_or((d.cursor, d.cursor));
        let range = Range::new(to_lsp(&d.rope, s), to_lsp(&d.rope, e));
        let line = d.line() as u32;
        let diags: Vec<(usize, Value)> = self
            .lsp
            .diagnostics
            .get(&key)
            .map(|v| v.iter().filter(|g| g.start.line <= line && g.end.line >= line).map(|g| (g.server, g.raw.clone())).collect())
            .unwrap_or_default();
        let sids = self.lsp.open.get(&key).map(|o| o.servers.clone()).unwrap_or_else(|| vec![sid]);
        self.lsp.actions_waiting = 0;
        for sid in sids {
            // Each server gets its own diagnostics back.
            let mine: Vec<Value> = diags.iter().filter(|(s, _)| *s == sid).map(|(_, r)| r.clone()).collect();
            let params = json!({ "textDocument": { "uri": uri }, "range": range, "context": { "diagnostics": mine } });
            if let Some(s) = self.lsp.server(sid) {
                if s.caps.get("codeActionProvider").is_some_and(|c| !c.is_null() && c != &json!(false)) || !s.ready {
                    s.request("textDocument/codeAction", params, Pending::CodeActions);
                    self.lsp.actions_waiting += 1;
                }
            }
        }
        if self.lsp.actions_waiting == 0 {
            self.message = Some("No quick fixes here".into());
        }
    }

    pub fn lsp_did_save(&mut self) {
        let Some(path) = self.doc().path.clone() else { return };
        self.lsp_sync(Some(self.active));
        let key = self.lsp.key(&path);
        for sid in self.lsp.open.get(&key).map(|o| o.servers.clone()).unwrap_or_default() {
            if let Some(s) = self.lsp.server(sid) {
                s.notify("textDocument/didSave", json!({ "textDocument": { "uri": path_to_uri(&key) } }));
            }
            self.pull_diags(sid, &key);
        }
    }

    // ---- results ---------------------------------------------------------------------------

    fn doc_index(&mut self, key: &Path) -> Option<usize> {
        (0..self.docs.len()).find(|&i| self.docs[i].path.clone().is_some_and(|p| self.lsp.key(&p) == key))
    }

    /// The tab showing `path`, opening it if needed.
    fn open_tab(&mut self, path: &Path) -> Option<usize> {
        let key = self.lsp.key(path);
        if let Some(i) = self.doc_index(&key) {
            return Some(i);
        }
        match Doc::open(path) {
            Ok(d) => {
                self.docs.push(d);
                Some(self.docs.len() - 1)
            }
            Err(e) => {
                self.message = Some(format!("Couldn't open {}: {e:#}", path.display()));
                None
            }
        }
    }

    fn jump_to(&mut self, path: &Path, pos: Position) {
        if let Some(p) = self.doc().path.clone() {
            let cursor = self.doc().cursor;
            self.lsp.back.push((p, cursor));
        }
        let Some(i) = self.open_tab(path) else { return };
        self.active = i;
        let d = &mut self.docs[i];
        let c = from_lsp(&d.rope, pos);
        d.move_to(c, false);
        d.scroll_line = d.line().saturating_sub(d.view_h / 3);
        self.follow = true;
    }

    fn go_back(&mut self) {
        let Some((path, cursor)) = self.lsp.back.pop() else {
            self.message = Some("Nowhere to go back to".into());
            return;
        };
        if let Some(i) = self.open_tab(&path) {
            self.active = i;
            self.docs[i].move_to(cursor, false);
        }
    }

    /// Applies a WorkspaceEdit to open tabs (opening files as needed). Returns files changed.
    fn apply_workspace_edit(&mut self, we: WorkspaceEdit) -> usize {
        let mut per_file: Vec<(String, Vec<TextEdit>)> = Vec::new();
        if let Some(changes) = we.changes {
            per_file.extend(changes.into_iter().map(|(u, e)| (u.as_str().to_string(), e)));
        }
        let mut skipped = 0;
        let doc_edit = |e: lsp_types::TextDocumentEdit, out: &mut Vec<(String, Vec<TextEdit>)>| {
            let edits = e.edits.into_iter().map(|x| match x {
                OneOf::Left(t) => t,
                OneOf::Right(a) => a.text_edit,
            });
            out.push((e.text_document.uri.as_str().to_string(), edits.collect()));
        };
        match we.document_changes {
            Some(DocumentChanges::Edits(v)) => v.into_iter().for_each(|e| doc_edit(e, &mut per_file)),
            Some(DocumentChanges::Operations(v)) => {
                for op in v {
                    match op {
                        DocumentChangeOperation::Edit(e) => doc_edit(e, &mut per_file),
                        DocumentChangeOperation::Op(_) => skipped += 1,
                    }
                }
            }
            None => {}
        }
        let mut n = 0;
        for (uri, edits) in per_file {
            let Some(path) = uri_to_path(&uri) else { continue };
            if let Some(i) = self.open_tab(&path) {
                apply_edits(&mut self.docs[i], &edits);
                n += 1;
            }
        }
        if skipped > 0 {
            self.message = Some(format!("Skipped {skipped} file create/rename/delete step(s): not supported yet"));
        }
        self.follow = true;
        n
    }

    /// Runs a chosen code action: its edit, then its command; resolves it first if it has neither.
    fn run_action(&mut self, sid: usize, a: Value, may_resolve: bool) {
        // A bare Command (title + command string).
        if a.get("command").is_some_and(Value::is_string) {
            let params = json!({ "command": a["command"], "arguments": a.get("arguments").cloned().unwrap_or(json!([])) });
            if let Some(s) = self.lsp.server(sid) {
                s.request("workspace/executeCommand", params, Pending::Ignore);
            }
            return;
        }
        let edit = a.get("edit").cloned().and_then(|e| serde_json::from_value::<WorkspaceEdit>(e).ok());
        let command = a.get("command").filter(|c| c.is_object()).cloned();
        if edit.is_none() && command.is_none() && may_resolve {
            if let Some(s) = self.lsp.server(sid) {
                s.request("codeAction/resolve", a, Pending::ResolveAction);
            }
            return;
        }
        if let Some(e) = edit {
            self.apply_workspace_edit(e);
        }
        if let Some(c) = command {
            let params = json!({ "command": c["command"], "arguments": c.get("arguments").cloned().unwrap_or(json!([])) });
            if let Some(s) = self.lsp.server(sid) {
                s.request("workspace/executeCommand", params, Pending::Ignore);
            }
        }
    }

    /// Enter inserts (keeps what follows the cursor), Tab replaces the rest of the word, as in IDEA.
    fn accept_completion(&mut self, replace: bool) {
        let Some(Popup::Completion { items, shown, sel, start, req, .. }) = self.lsp.popup.take() else { return };
        let Some(&k) = shown.get(sel) else { return };
        let it = &items[k];
        let d = &mut self.docs[self.active];
        let from = it.edit_start.map(|p| from_lsp(&d.rope, p)).filter(|&s| s <= d.cursor).unwrap_or(start);
        // Text after the cursor the server's range covers (a closing quote, the rest of a word).
        let end = if replace { it.replace_end } else { it.edit_end };
        let tail = end.filter(|e| e.line == req.line).map_or(0, |e| e.character.saturating_sub(req.character) as usize);
        let line_end = d.line_end(d.line());
        let to = (d.cursor + tail).min(line_end.max(d.cursor));
        let mut snip = it.insert.clone();
        let next = (d.cursor < d.rope.len_chars()).then(|| d.rope.char(d.cursor));
        if it.auto_parens && next != Some('(') {
            snip.text.push_str("()");
            let n = snip.text.chars().count();
            snip.stops = vec![snippet::Stop { index: 0, start: n - 1, end: n - 1 }];
        }
        // Extra edits (imports) above the word move it down.
        let shift: isize = it
            .extra
            .iter()
            .filter_map(|e| {
                let (s, t) = (from_lsp(&d.rope, e.range.start), from_lsp(&d.rope, e.range.end));
                (t <= from).then(|| e.new_text.chars().count() as isize - (t - s) as isize)
            })
            .sum();
        let mut edits = vec![TextEdit { range: Range::new(to_lsp(&d.rope, from), to_lsp(&d.rope, to)), new_text: snip.text.clone() }];
        edits.extend(it.extra.iter().cloned());
        apply_edits(d, &edits);
        let base = (from as isize + shift).max(0) as usize;
        let stops: Vec<(usize, usize)> = snip.stops.iter().map(|s| (base + s.start, base + s.end)).collect();
        match stops.last() {
            Some(&(s, _)) => d.move_to(s, false),
            None => d.move_to(base + snip.text.chars().count(), false),
        }
        self.lsp.snippet = None;
        if stops.len() > 1 {
            let len = d.rope.len_chars();
            self.lsp.snippet = Some(SnippetSession { doc: self.active, stops, cur: 0, len });
            self.snippet_select();
        }
        if snip.text.contains('(') {
            self.request_signature(None);
        }
    }

    // ---- snippets --------------------------------------------------------------------------

    /// Selects the current tab stop's placeholder (or puts the cursor there).
    fn snippet_select(&mut self) {
        let Some(s) = &self.lsp.snippet else { return };
        let (a, b) = s.stops[s.cur];
        let d = &mut self.docs[s.doc];
        d.move_to(a, false);
        if b > a {
            d.move_to(b, true);
        }
    }

    /// Tab / Shift+Tab: next / previous stop; reaching the last one (`$0`) ends the session.
    fn snippet_jump(&mut self, forward: bool) {
        let Some(s) = &mut self.lsp.snippet else { return };
        if forward {
            s.cur += 1;
        } else {
            s.cur = s.cur.saturating_sub(1);
        }
        let last = s.cur + 1 >= s.stops.len();
        self.snippet_select();
        if last {
            self.lsp.snippet = None;
        }
    }

    /// After a key: moves the stops along with typing inside the current one; an edit anywhere
    /// else ends the session.
    fn snippet_track(&mut self) {
        let Some(s) = &mut self.lsp.snippet else { return };
        if s.doc != self.active || s.doc >= self.docs.len() {
            self.lsp.snippet = None;
            return;
        }
        let d = &self.docs[s.doc];
        let len = d.rope.len_chars();
        if len == s.len {
            return;
        }
        let delta = len as isize - s.len as isize;
        let (cs, ce) = s.stops[s.cur];
        let new_ce = ce as isize + delta;
        if new_ce < cs as isize || d.cursor < cs || d.cursor as isize > new_ce {
            self.lsp.snippet = None;
            return;
        }
        let mut out = Vec::new();
        let mut cur = 0;
        for (j, &(a, b)) in s.stops.iter().enumerate() {
            if j == s.cur {
                cur = out.len();
                out.push((cs, new_ce as usize));
            } else if a >= cs && b <= ce && a < ce {
                // Nested in the placeholder that was typed over: gone.
            } else if a >= ce {
                out.push(((a as isize + delta) as usize, (b as isize + delta) as usize));
            } else {
                out.push((a, b));
            }
        }
        s.stops = out;
        s.cur = cur;
        s.len = len;
    }

    // ---- navigation pickers ------------------------------------------------------------------

    fn rel_path(&self, p: &Path) -> String {
        rel(&self.project.root, p)
    }

    /// Picker rows for locations: the line's text, and `file:line`.
    fn location_items(&mut self, mut locs: Vec<(PathBuf, Position)>) -> Vec<PickItem> {
        locs.sort_by(|a, b| a.0.cmp(&b.0).then((a.1.line, a.1.character).cmp(&(b.1.line, b.1.character))));
        locs.dedup();
        let mut files: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let mut out = Vec::new();
        for (path, pos) in locs.into_iter().take(1000) {
            let text = match self.doc_index(&path) {
                Some(i) => {
                    let r = &self.docs[i].rope;
                    if (pos.line as usize) < r.len_lines() { r.line(pos.line as usize).to_string() } else { String::new() }
                }
                None => {
                    let lines = files.entry(path.clone()).or_insert_with(|| {
                        std::fs::read_to_string(&path).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default()
                    });
                    lines.get(pos.line as usize).cloned().unwrap_or_default()
                }
            };
            let label: String = text.trim().chars().take(160).collect();
            let detail = format!("{}:{}", self.rel_path(&path), pos_line(pos));
            out.push(PickItem { kind: "", label, detail, path, pos, depth: 0 });
        }
        out
    }

    fn request_structure(&mut self) {
        let Some((sid, uri, key)) = self.active_server() else { return };
        if !self.can(sid, "textDocument/documentSymbol") {
            return;
        }
        if let Some(s) = self.lsp.server(sid) {
            s.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": uri } }), Pending::Structure { path: key });
        }
    }

    fn open_symbols(&mut self) {
        let Some((sid, _, _)) = self.active_server() else { return };
        if !self.can(sid, "workspace/symbol") {
            return;
        }
        let mut p = Picker::new(PickKind::Symbols, "Go to symbol in the project".into(), Vec::new());
        p.server = sid;
        p.query = word_at(self.doc());
        self.lsp.popup = Some(Popup::Picker(p));
        self.request_symbols();
    }

    fn request_symbols(&mut self) {
        self.lsp.seq += 1;
        let seq = self.lsp.seq;
        let Some(Popup::Picker(p)) = &mut self.lsp.popup else { return };
        p.seq = seq;
        let (sid, query) = (p.server, p.query.clone());
        if let Some(s) = self.lsp.server(sid) {
            s.request("workspace/symbol", json!({ "query": query }), Pending::Symbols { seq });
        }
    }

    /// Problems of the active file: (start char, severity, message), in file order.
    fn problems(&mut self) -> Vec<(usize, u8, String, Position)> {
        let Some(path) = self.doc().path.clone() else { return Vec::new() };
        let key = self.lsp.key(&path);
        let rope = &self.docs[self.active].rope;
        let mut v: Vec<_> = self
            .lsp
            .diagnostics
            .get(&key)
            .map(|v| v.iter().filter(|g| g.severity < 4).map(|g| (from_lsp(rope, g.start), g.severity, g.message.clone(), g.start)).collect())
            .unwrap_or_default();
        v.sort_by_key(|p| p.0);
        v
    }

    /// F2 / Shift+F2: the next / previous error or warning (infos when there are none), wrapping.
    fn goto_problem(&mut self, forward: bool) {
        let all = self.problems();
        let worst = all.iter().map(|p| p.1).min().unwrap_or(4).max(2);
        let list: Vec<_> = all.into_iter().filter(|p| p.1 <= worst).collect();
        if list.is_empty() {
            self.message = Some("No problems in this file".into());
            return;
        }
        let c = self.doc().cursor;
        let pick = if forward {
            list.iter().find(|p| p.0 > c).or(list.first())
        } else {
            list.iter().rev().find(|p| p.0 < c).or(list.last())
        };
        if let Some(&(at, _, ref msg, _)) = pick {
            let n = list.iter().position(|p| p.0 == at).unwrap_or(0) + 1;
            let msg = format!("{n}/{} {}", list.len(), msg.lines().next().unwrap_or(""));
            self.docs[self.active].move_to(at, false);
            self.message = Some(msg);
        }
    }

    fn open_problems(&mut self) {
        let Some(path) = self.doc().path.clone() else { return };
        let key = self.lsp.key(&path);
        let items: Vec<PickItem> = self
            .problems()
            .into_iter()
            .map(|(_, sev, msg, pos)| PickItem {
                kind: match sev {
                    1 => "err",
                    2 => "warn",
                    _ => "info",
                },
                label: msg.lines().next().unwrap_or("").to_string(),
                detail: format!("Ln {}", pos_line(pos)),
                path: key.clone(),
                pos,
                depth: 0,
            })
            .collect();
        if items.is_empty() {
            self.message = Some("No problems in this file".into());
            return;
        }
        let n = items.len();
        self.lsp.popup = Some(Popup::Picker(Picker::new(PickKind::Problems, format!("Problems in {} ({n})", self.doc().name()), items)));
    }

    /// Keys while a picker is open. Always handled.
    fn picker_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let Some(Popup::Picker(p)) = &mut self.lsp.popup else { return };
        let mut requery = false;
        match k.code {
            KeyCode::Esc => self.lsp.popup = None,
            KeyCode::Up => p.sel = p.sel.checked_sub(1).unwrap_or(p.shown.len().saturating_sub(1)),
            KeyCode::Down => p.sel = if p.sel + 1 >= p.shown.len() { 0 } else { p.sel + 1 },
            KeyCode::PageUp => p.sel = p.sel.saturating_sub(10),
            KeyCode::PageDown => p.sel = (p.sel + 10).min(p.shown.len().saturating_sub(1)),
            KeyCode::Home if ctrl => p.sel = 0,
            KeyCode::End if ctrl => p.sel = p.shown.len().saturating_sub(1),
            KeyCode::Enter | KeyCode::F(4) => {
                let target = p.selected().map(|it| (it.path.clone(), it.pos));
                self.lsp.popup = None;
                if let Some((path, pos)) = target {
                    self.jump_to(&path, pos);
                }
            }
            KeyCode::Backspace | KeyCode::Char('h') if ctrl || k.code == KeyCode::Backspace => {
                if ctrl || alt {
                    let t = p.query.trim_end_matches(|c: char| is_word(c)).len();
                    p.query.truncate(if t == p.query.len() { p.query.trim_end().len().saturating_sub(1) } else { t });
                } else {
                    p.query.pop();
                }
                requery = true;
            }
            KeyCode::Char('u') if ctrl => {
                p.query.clear();
                requery = true;
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                p.query.push(c);
                requery = true;
            }
            _ => {}
        }
        if requery {
            if let Some(Popup::Picker(p)) = &mut self.lsp.popup {
                if p.kind == PickKind::Symbols {
                    self.request_symbols();
                } else {
                    p.sel = 0;
                    p.refilter();
                }
            }
        }
    }

    // ---- signature help ----------------------------------------------------------------------

    fn request_signature(&mut self, trigger: Option<char>) {
        let Some(path) = self.doc().path.clone() else { return };
        let path = self.lsp.key(&path);
        let line = self.doc().line();
        self.lsp.sig_rev = self.doc().rev();
        let ctx = match trigger {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string(), "isRetrigger": self.lsp.signature.is_some() }),
            None => json!({ "triggerKind": if self.lsp.signature.is_some() { 3 } else { 1 }, "isRetrigger": self.lsp.signature.is_some() }),
        };
        self.request_here("textDocument/signatureHelp", json!({ "context": ctx }), Pending::Signature { path, line });
    }

    /// The server's signature-help trigger characters (default `(` and `,`).
    fn sig_triggers(&mut self) -> Vec<String> {
        let Some(path) = self.doc().path.clone() else { return Vec::new() };
        let key = self.lsp.key(&path);
        let Some(sid) = self.lsp.open.get(&key).map(|o| o.servers[0]) else { return Vec::new() };
        let Some(s) = self.lsp.server(sid) else { return Vec::new() };
        let Some(p) = s.caps.get("signatureHelpProvider").filter(|p| p.is_object()) else { return Vec::new() };
        let mut v: Vec<String> = ["triggerCharacters", "retriggerCharacters"]
            .iter()
            .flat_map(|k| p.get(*k).and_then(Value::as_array).cloned().unwrap_or_default())
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect();
        if v.is_empty() {
            v = vec!["(".into(), ",".into()];
        }
        v
    }

    // ---- screen ----------------------------------------------------------------------------

    /// Diagnostics of the active doc as (start, end, severity) char ranges; hints left out.
    pub fn diag_spans(&mut self) -> Vec<(usize, usize, u8)> {
        let Some(path) = self.doc().path.clone() else { return Vec::new() };
        let key = self.lsp.key(&path);
        let rope = &self.docs[self.active].rope;
        self.lsp.diagnostics.get(&key).map_or_else(Vec::new, |v| {
            v.iter()
                .filter(|g| g.severity < 4)
                .map(|g| {
                    let s = from_lsp(rope, g.start);
                    let e = from_lsp(rope, g.end).max(s + 1).min(rope.len_chars().max(s + 1));
                    (s, e, g.severity)
                })
                .collect()
        })
    }

    /// For the status bar: the diagnostic under the cursor, else server progress.
    pub fn lsp_hint(&mut self) -> Option<(String, u8)> {
        if let Some(path) = self.doc().path.clone() {
            let key = self.lsp.key(&path);
            let d = &self.docs[self.active];
            let line = d.line() as u32;
            let col = to_lsp(&d.rope, d.cursor).character;
            if let Some(v) = self.lsp.diagnostics.get(&key) {
                let on_line = |g: &&Diag| g.severity < 4 && g.start.line <= line && g.end.line >= line;
                let at = |g: &&Diag| (g.start.line < line || g.start.character <= col) && (g.end.line > line || g.end.character >= col);
                let best = v.iter().filter(on_line).filter(at).min_by_key(|g| g.severity).or_else(|| v.iter().filter(on_line).min_by_key(|g| g.severity));
                if let Some(g) = best {
                    return Some((g.message.lines().next().unwrap_or("").to_string(), g.severity));
                }
            }
        }
        self.lsp.progress.values().next().map(|(server, title, text)| (format!("{server}: {title} {text}").trim_end().to_string(), 0))
    }

    // ---- keys ------------------------------------------------------------------------------

    /// Code-smarts keys and popup navigation, before the normal key map. True = handled.
    pub fn smart_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        if matches!(self.lsp.popup, Some(Popup::Picker(_))) {
            self.picker_key(k);
            return true;
        }
        if let Some(p) = &mut self.lsp.popup {
            let list = !matches!(p, Popup::Hover { .. });
            match k.code {
                KeyCode::Esc => {
                    if matches!(p, Popup::Completion { .. }) {
                        self.completion_dismissed();
                    }
                    self.lsp.popup = None;
                    return true;
                }
                KeyCode::Up => {
                    p.move_sel(-1);
                    return true;
                }
                KeyCode::Down => {
                    p.move_sel(1);
                    return true;
                }
                KeyCode::PageUp => {
                    p.move_sel(-8);
                    return true;
                }
                KeyCode::PageDown => {
                    p.move_sel(8);
                    return true;
                }
                // Enter on an item that is typed out already is a new line (and closes the list).
                KeyCode::Enter if !alt && !ctrl && completion_adds_nothing(p, self.docs[self.active].cursor, &self.docs[self.active].rope) => {
                    self.lsp.popup = None;
                    return false;
                }
                KeyCode::Enter | KeyCode::Tab if list && !alt => {
                    let replace = k.code == KeyCode::Tab;
                    match self.lsp.popup.take() {
                        Some(Popup::Actions { items, sel }) => {
                            if let Some((server, a)) = items.into_iter().nth(sel) {
                                self.run_action(server, a, true);
                            }
                        }
                        other => {
                            self.lsp.popup = other;
                            self.accept_completion(replace);
                        }
                    }
                    return true;
                }
                _ => {
                    if !matches!(p, Popup::Completion { .. }) {
                        self.lsp.popup = None;
                    }
                }
            }
        }
        if k.code == KeyCode::Esc {
            // Closes signature help and ends a snippet, then does what Esc always does (Vim!).
            if self.lsp.auto_at.is_some() {
                self.completion_dismissed();
            }
            self.lsp.signature = None;
            self.lsp.snippet = None;
            return false;
        }
        if self.lsp.snippet.is_some() && !ctrl && !alt {
            match k.code {
                KeyCode::Tab if !shift => {
                    self.snippet_jump(true);
                    return true;
                }
                KeyCode::BackTab | KeyCode::Tab => {
                    self.snippet_jump(false);
                    return true;
                }
                _ => {}
            }
        }
        // Vim normal/visual mode keeps Ctrl+B (page up) and gets Vim's Ctrl+] for go to definition.
        let vim_cmd = self.vim.as_ref().is_some_and(|v| !matches!(v.mode, crate::vim::Mode::Insert));
        let def = |word: String| Pending::Definition { what: "definition", word };
        if auto::is_complete_key(&k, vim_cmd) {
            self.request_completion(None, false);
            return true;
        }
        match k.code {
            KeyCode::Char(']') | KeyCode::Char('5') if ctrl && vim_cmd => {
                let w = word_at(self.doc());
                self.request_here("textDocument/definition", json!({}), def(w))
            }
            KeyCode::Char('b') | KeyCode::Char('B') if ctrl && alt => {
                let word = word_at(self.doc());
                self.request_here("textDocument/implementation", json!({}), Pending::Definition { what: "implementation", word })
            }
            KeyCode::Char('b') if ctrl && !alt && !vim_cmd => {
                let w = word_at(self.doc());
                self.request_here("textDocument/definition", json!({}), def(w))
            }
            KeyCode::Left if ctrl && alt => self.go_back(),
            KeyCode::F(1) => self.request_here("textDocument/hover", json!({}), Pending::Hover),
            KeyCode::F(2) if !ctrl && !alt => self.goto_problem(!shift),
            KeyCode::F(7) if alt && !ctrl => {
                let word = word_at(self.doc());
                self.request_here("textDocument/references", json!({ "context": { "includeDeclaration": false } }), Pending::References { word })
            }
            KeyCode::F(12) if ctrl && !alt => self.request_structure(),
            // Ctrl+Alt+Shift+N (IDEA "Symbol…"); Alt+N where the terminal can't send it.
            KeyCode::Char('n') | KeyCode::Char('N') if alt && (ctrl || !shift) => self.open_symbols(),
            KeyCode::Char('6') if alt && !ctrl => self.open_problems(),
            KeyCode::F(6) if shift => {
                if self.active_server().is_some() {
                    let input = word_at(self.doc());
                    self.prompt = Some(Prompt { kind: PromptKind::Rename, input });
                }
            }
            KeyCode::Char('l') | KeyCode::Char('L') if ctrl && alt => self.request_format(),
            KeyCode::Enter if alt => self.request_actions(),
            _ => return false,
        }
        true
    }

    /// After a normal key: keep the completion list in step with typing, completion as you type
    /// ([`auto`]), signature help after `(`/`,`, snippet stops following the typing.
    pub fn smart_after_key(&mut self, k: KeyEvent) {
        self.snippet_track();
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if let Some(Popup::Completion { start, prefix, incomplete, .. }) = &self.lsp.popup {
            let (start, incomplete) = (*start, *incomplete);
            let d = self.doc();
            let typed = (d.cursor >= start && d.rope.char_to_line(d.cursor) == d.rope.char_to_line(start))
                .then(|| d.rope.slice(start..d.cursor).to_string())
                .filter(|t| t.chars().all(|c| is_word_in(d.syntax.lang, c)));
            match typed {
                None => self.lsp.popup = None,
                Some(t) if t != *prefix => {
                    if let Some(p) = &mut self.lsp.popup {
                        p.filter(&t);
                    }
                    let empty = matches!(&self.lsp.popup, Some(Popup::Completion { shown, .. }) if shown.is_empty());
                    if incomplete {
                        self.request_completion(None, true);
                    } else if empty {
                        // Nothing left to offer for this word: don't ask again while it's typed.
                        self.lsp.auto_off = Some((self.active, start));
                        self.lsp.popup = None;
                    }
                }
                Some(_) => {}
            }
        }
        let typing = self.vim.as_ref().is_none_or(|v| matches!(v.mode, crate::vim::Mode::Insert));
        let lang = self.doc().syntax.lang;
        let smart = config::get().has_servers(lang.key()) && !self.lsp.no_server.contains(&lang);
        // Signature help follows the cursor: closes off its line, refreshes after edits.
        if let Some(sig) = &self.lsp.signature {
            if sig.line != self.doc().line() || !typing {
                self.lsp.signature = None;
            }
        }
        let mut sig_asked = false;
        self.auto_after_key(k, typing);
        if let (KeyCode::Char(c), true, true, true) = (k.code, plain, typing, smart) {
            if self.sig_triggers().iter().any(|t| t.starts_with(c)) {
                self.request_signature(Some(c));
                sig_asked = true;
            } else if c == ')' && self.lsp.signature.is_some() {
                self.request_signature(None);
                sig_asked = true;
            }
        }
        if !sig_asked && self.lsp.signature.is_some() && self.doc().rev() != self.lsp.sig_rev {
            self.request_signature(None);
        }
    }

    /// Draws signature help, the completion/hover/actions popup and pickers. Returns the cursor
    /// position a picker wants (its filter line).
    pub fn draw_lsp_popups(&self, buf: &mut ratatui::buffer::Buffer, cursor: Option<ratatui::layout::Position>, prompt: bool, screen: ratatui::layout::Rect) -> Option<ratatui::layout::Position> {
        if let (Some(sig), Some(at), false) = (&self.lsp.signature, cursor, prompt) {
            if !matches!(self.lsp.popup, Some(Popup::Picker(_))) {
                popup::draw_signature(buf, sig, at, screen);
            }
        }
        match &self.lsp.popup {
            Some(Popup::Picker(p)) => Some(popup::draw_picker(buf, p, screen)),
            Some(p) => {
                if let (Some(at), false) = (cursor, prompt) {
                    popup::draw(buf, p, at, screen);
                }
                None
            }
            None => None,
        }
    }
}

/// Whether the selected completion item would insert only what is typed already.
fn completion_adds_nothing(p: &Popup, cursor: usize, rope: &ropey::Rope) -> bool {
    let Popup::Completion { items, shown, sel, start, .. } = p else { return false };
    let Some(it) = shown.get(*sel).map(|&i| &items[i]) else { return false };
    cursor >= *start && it.adds_nothing(&rope.slice(*start..cursor).to_string())
}

/// `p` relative to the project root when inside it.
fn rel(root: &Path, p: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    p.strip_prefix(&root).unwrap_or(p).display().to_string()
}

fn pos_line(p: Position) -> u32 {
    p.line + 1
}

/// Short tag for an LSP `SymbolKind`.
fn symbol_kind(k: u64) -> &'static str {
    match k {
        1 => "file",
        2 => "mod",
        3 => "ns",
        4 => "pkg",
        5 => "C",
        6 => "me",
        7 | 8 => "fd",
        9 => "new",
        10 => "E",
        11 => "I",
        12 => "fn",
        13 => "v",
        14 => "c",
        22 => "e",
        23 => "S",
        24 => "ev",
        25 => "op",
        26 => "T",
        20 => "key",
        _ => "·",
    }
}

/// DocumentSymbol (nested) or SymbolInformation (flat) → picker rows.
fn flatten_symbol(s: &Value, path: &Path, depth: usize, out: &mut Vec<PickItem>) {
    let name = s.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
    let kind = symbol_kind(s.get("kind").and_then(Value::as_u64).unwrap_or(0));
    let start = s.pointer("/selectionRange/start").or_else(|| s.pointer("/location/range/start")).or_else(|| s.pointer("/range/start"));
    let pos: Position = start.and_then(|p| serde_json::from_value(p.clone()).ok()).unwrap_or_default();
    let detail = s.get("detail").and_then(Value::as_str).or_else(|| s.get("containerName").and_then(Value::as_str)).unwrap_or("");
    let detail: String = detail.lines().next().unwrap_or("").chars().take(60).collect();
    out.push(PickItem { kind, label: name, detail: if detail.is_empty() { format!("Ln {}", pos_line(pos)) } else { format!("{detail} · Ln {}", pos_line(pos)) }, path: path.to_path_buf(), pos, depth });
    for c in s.get("children").and_then(Value::as_array).into_iter().flatten() {
        flatten_symbol(c, path, depth + 1, out);
    }
}

/// The active signature with its active parameter as a char range of the label.
fn signature(h: lsp_types::SignatureHelp, line: usize) -> Option<Signature> {
    let n = h.signatures.len();
    let i = (h.active_signature.unwrap_or(0) as usize).min(n.checked_sub(1)?);
    let sig = h.signatures.into_iter().nth(i)?;
    let active_param = sig.active_parameter.or(h.active_parameter).unwrap_or(0) as usize;
    let label = sig.label.replace('\n', " ");
    let param = sig.parameters.as_ref().and_then(|ps| ps.get(active_param));
    let active = param.and_then(|p| match &p.label {
        lsp_types::ParameterLabel::LabelOffsets([a, b]) => {
            // UTF-16 offsets → char offsets.
            let mut u = 0usize;
            let (mut ca, mut cb) = (None, None);
            for (ci, ch) in label.chars().chain(std::iter::once(' ')).enumerate() {
                if u >= *a as usize && ca.is_none() {
                    ca = Some(ci);
                }
                if u >= *b as usize && cb.is_none() {
                    cb = Some(ci);
                }
                u += ch.len_utf16();
            }
            Some((ca?, cb?))
        }
        lsp_types::ParameterLabel::Simple(name) => {
            let open = label.find('(').unwrap_or(0);
            let at = label[open..].find(name.as_str())? + open;
            let s = label[..at].chars().count();
            Some((s, s + name.chars().count()))
        }
    });
    let doc_of = |d: &lsp_types::Documentation| match d {
        lsp_types::Documentation::String(s) => s.clone(),
        lsp_types::Documentation::MarkupContent(m) => m.value.clone(),
    };
    let doc = param
        .and_then(|p| p.documentation.as_ref().map(doc_of))
        .or_else(|| sig.documentation.as_ref().map(doc_of))
        .unwrap_or_default();
    let doc = doc.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("```")).unwrap_or("").to_string();
    Some(Signature { label, active, doc, line, more: n - 1 })
}

fn marked(s: MarkedString) -> String {
    match s {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(l) => l.value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_marks_the_active_parameter() {
        let h: lsp_types::SignatureHelp = serde_json::from_value(json!({
            "signatures": [{
                "label": "fn add(a: i32, b: i32) -> i32",
                "documentation": "Adds two numbers.",
                "parameters": [{ "label": [7, 13] }, { "label": [15, 21] }]
            }],
            "activeParameter": 1
        }))
        .unwrap();
        let s = signature(h, 3).unwrap();
        assert_eq!(&s.label.chars().collect::<Vec<_>>()[s.active.unwrap().0..s.active.unwrap().1].iter().collect::<String>(), "b: i32");
        assert_eq!((s.doc.as_str(), s.line, s.more), ("Adds two numbers.", 3, 0));
        // String labels are looked up after the "(", so a parameter named like the function works.
        let h: lsp_types::SignatureHelp = serde_json::from_value(json!({
            "signatures": [{ "label": "x(x, y)", "parameters": [{ "label": "x" }, { "label": "y" }], "activeParameter": 0 },
                           { "label": "x()" }],
            "activeSignature": 0
        }))
        .unwrap();
        let s = signature(h, 0).unwrap();
        assert_eq!((s.active, s.more), (Some((2, 3)), 1));
        let empty: lsp_types::SignatureHelp = serde_json::from_value(json!({ "signatures": [] })).unwrap();
        assert!(signature(empty, 0).is_none());
    }

    #[test]
    fn symbols_flatten_with_depth() {
        let v = json!([{ "name": "App", "kind": 23, "range": {"start": {"line": 1, "character": 0}, "end": {"line": 9, "character": 1}},
                         "selectionRange": {"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 14}},
                         "children": [{ "name": "new", "kind": 6, "detail": "fn() -> App",
                                        "range": {"start": {"line": 2, "character": 4}, "end": {"line": 3, "character": 5}},
                                        "selectionRange": {"start": {"line": 2, "character": 11}, "end": {"line": 2, "character": 14}} }] },
                       { "name": "flat", "kind": 12, "containerName": "m",
                         "location": { "uri": "file:///x.rs", "range": {"start": {"line": 20, "character": 0}, "end": {"line": 20, "character": 1}} } }]);
        let mut out = Vec::new();
        for s in v.as_array().unwrap() {
            flatten_symbol(s, Path::new("/x.rs"), 0, &mut out);
        }
        let rows: Vec<(&str, &str, usize, u32)> = out.iter().map(|i| (i.kind, i.label.as_str(), i.depth, i.pos.line)).collect();
        assert_eq!(rows, [("S", "App", 0, 1), ("me", "new", 1, 2), ("fn", "flat", 0, 20)]);
        assert_eq!(out[1].detail, "fn() -> App · Ln 3");
        assert_eq!(out[2].detail, "m · Ln 21");
    }
}
