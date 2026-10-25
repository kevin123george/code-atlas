//! LSP client: one server process per (language server, project root), started when a file of
//! that language is open. Full-text sync (didOpen / didChange after a short pause in typing /
//! didSave / didClose), diagnostics, completion, go to definition, hover, rename, format and
//! quick fixes. JSON-RPC runs over stdio in background threads; [`App::lsp_tick`] handles what
//! arrived, once per event-loop turn.
//!
//! Keys (IntelliJ): Ctrl+Space complete (also after `.` and `::`), Ctrl+B go to definition,
//! Ctrl+Alt+← back, F1 quick documentation (IDEA's Ctrl+Q is quit here), Shift+F6 rename,
//! Ctrl+Alt+L reformat, Alt+Enter quick fixes.

pub mod jsonrpc;
pub mod popup;
pub mod server;
pub mod text;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lsp_types::{
    CompletionResponse, DocumentChangeOperation, DocumentChanges, GotoDefinitionResponse, Hover, HoverContents,
    MarkedString, OneOf, Position, Range, TextEdit, WorkspaceEdit,
};
use serde_json::{json, Value};

use crate::app::{App, Prompt, PromptKind};
use crate::doc::Doc;
use crate::syntax::Lang;
use jsonrpc::Incoming;
use popup::{Item, Popup};
use server::{Event, Pending, Server};
use text::{apply_edits, from_lsp, path_to_uri, to_lsp, uri_to_path};

/// Pause in typing before the new text goes to the server (requests send it at once).
const CHANGE_DELAY: Duration = Duration::from_millis(150);

struct OpenDoc {
    server: usize,
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

    fn server(&mut self, id: usize) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.id == id)
    }
}

pub(crate) fn initialize_params(root: &Path) -> Value {
    let uri = path_to_uri(root);
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "Code Atlas", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": uri,
        "rootPath": root.display().to_string(),
        "workspaceFolders": [{ "uri": uri, "name": name }],
        "capabilities": {
            "general": { "positionEncodings": ["utf-16"] },
            "textDocument": {
                "synchronization": { "didSave": true, "dynamicRegistration": false },
                "completion": {
                    "completionItem": { "snippetSupport": false, "documentationFormat": ["plaintext", "markdown"], "labelDetailsSupport": true },
                    "contextSupport": true
                },
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
                "workspaceFolders": true
            },
            "window": { "workDoneProgress": true }
        }
    })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn word_start(d: &Doc) -> usize {
    let mut s = d.cursor;
    while s > 0 && is_word(d.rope.char(s - 1)) {
        s -= 1;
    }
    s
}

fn word_at(d: &Doc) -> String {
    let mut e = d.cursor;
    while e < d.rope.len_chars() && is_word(d.rope.char(e)) {
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
        changed
    }

    /// Opens, changes and closes documents on their servers. `now` = doc index to send at once.
    fn lsp_sync(&mut self, now: Option<usize>) {
        let t = Instant::now();
        let mut live = HashSet::new();
        for i in 0..self.docs.len() {
            let Some(path) = self.docs[i].path.clone() else { continue };
            let lang = self.docs[i].syntax.lang;
            if server::specs(lang).is_empty() || self.lsp.no_server.contains(&lang) {
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
                    let (sid, version) = (od.server, od.version);
                    let text = self.docs[i].rope.to_string();
                    if let Some(s) = self.lsp.server(sid) {
                        s.notify("textDocument/didChange", json!({
                            "textDocument": { "uri": uri, "version": version },
                            "contentChanges": [{ "text": text }]
                        }));
                    }
                }
                continue;
            }
            let Some(sid) = self.server_for(lang, &key) else { continue };
            let text = self.docs[i].rope.to_string();
            if let Some(s) = self.lsp.server(sid) {
                s.notify("textDocument/didOpen", json!({
                    "textDocument": { "uri": uri, "languageId": lang.lsp_id(), "version": 1, "text": text }
                }));
            }
            self.lsp.open.insert(key, OpenDoc { server: sid, version: 1, sent_rev: rev, seen_rev: rev, seen_at: t });
        }
        let gone: Vec<PathBuf> = self.lsp.open.keys().filter(|k| !live.contains(*k)).cloned().collect();
        for k in gone {
            let od = self.lsp.open.remove(&k).unwrap();
            self.lsp.diagnostics.remove(&k);
            if let Some(s) = self.lsp.server(od.server) {
                s.notify("textDocument/didClose", json!({ "textDocument": { "uri": path_to_uri(&k) } }));
            }
        }
    }

    /// The running server for a file, starting one if needed.
    fn server_for(&mut self, lang: Lang, key: &Path) -> Option<usize> {
        let specs = server::specs(lang);
        let found = specs.iter().find_map(|s| server::which(s.cmd).map(|exe| (s, exe)));
        let Some((spec, exe)) = found else {
            self.lsp.no_server.insert(lang);
            let names: Vec<&str> = specs.iter().map(|s| s.cmd).collect();
            self.message = Some(format!("No language server for {} on PATH ({}): highlighting only", lang.name(), names.join(", ")));
            return None;
        };
        let root = server::find_root(key, spec.markers);
        if let Some(s) = self.lsp.servers.iter().find(|s| s.name == spec.cmd && s.root == root) {
            return Some(s.id);
        }
        if self.lsp.dead.contains(&(spec.cmd.to_string(), root.clone())) {
            return None;
        }
        let id = self.lsp.next_server;
        self.lsp.next_server += 1;
        match Server::start(id, &exe, spec, &root, self.lsp.tx.clone()) {
            Ok(s) => {
                self.lsp.progress.insert(format!("start-{id}"), (s.name.clone(), "starting".into(), String::new()));
                self.lsp.servers.push(s);
                Some(id)
            }
            Err(e) => {
                self.lsp.dead.insert((spec.cmd.to_string(), root));
                self.message = Some(format!("Couldn't start {}: {e}", spec.cmd));
                None
            }
        }
    }

    fn on_exit(&mut self, id: usize) {
        let Some(i) = self.lsp.servers.iter().position(|s| s.id == id) else { return };
        let mut s = self.lsp.servers.remove(i);
        s.kill();
        let tail = s.stderr_tail.lock().unwrap().clone();
        self.lsp.dead.insert((s.name.clone(), s.root.clone()));
        self.lsp.open.retain(|_, od| od.server != id);
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
                        if let Pending::Initialize = p {
                            self.message = Some(format!("Couldn't start {name}: {e}"));
                        } else if !matches!(p, Pending::Ignore | Pending::Completion { .. }) && !e.contains("content modified") && !e.contains("cancel") {
                            self.message = Some(format!("Couldn't ask {name}: {e}"));
                        }
                    }
                }
            }
            Incoming::Request { id: rid, method, params } => {
                let result = match method.as_str() {
                    "workspace/configuration" => {
                        let n = params.get("items").and_then(Value::as_array).map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; n])
                    }
                    "workspace/applyEdit" => {
                        let edit = params.get("edit").cloned().and_then(|e| serde_json::from_value::<WorkspaceEdit>(e).ok());
                        let n = edit.map_or(0, |e| self.apply_workspace_edit(e));
                        json!({ "applied": n > 0 })
                    }
                    "workspace/workspaceFolders" => Value::Null,
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
                        Some(Diag { start: d.range.start, end: d.range.end, severity, message: d.message, raw })
                    })
                    .collect();
                self.lsp.diagnostics.insert(path, diags);
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
            }
            Pending::Completion { path, start } => {
                let here = self.doc().path.clone().map(|p| self.lsp.key(&p));
                if here.as_ref() != Some(&path) {
                    return;
                }
                let d = self.doc();
                if d.cursor < start || d.rope.char_to_line(d.cursor) != d.rope.char_to_line(start) {
                    return;
                }
                let (items, incomplete) = match serde_json::from_value::<Option<CompletionResponse>>(v).ok().flatten() {
                    Some(CompletionResponse::Array(a)) => (a, false),
                    Some(CompletionResponse::List(l)) => (l.items, l.is_incomplete),
                    None => (Vec::new(), false),
                };
                let prefix = d.rope.slice(start..d.cursor).to_string();
                let items: Vec<Item> = items.into_iter().map(Item::from_lsp).collect();
                let popup = Popup::completion(items, start, incomplete, &prefix);
                let empty = matches!(&popup, Popup::Completion { shown, .. } if shown.is_empty());
                self.lsp.popup = if empty { None } else { Some(popup) };
            }
            Pending::Definition => {
                let loc = match serde_json::from_value::<Option<GotoDefinitionResponse>>(v).ok().flatten() {
                    Some(GotoDefinitionResponse::Scalar(l)) => Some((l.uri, l.range.start)),
                    Some(GotoDefinitionResponse::Array(a)) => a.into_iter().next().map(|l| (l.uri, l.range.start)),
                    Some(GotoDefinitionResponse::Link(a)) => a.into_iter().next().map(|l| (l.target_uri, l.target_selection_range.start)),
                    None => None,
                };
                match loc.and_then(|(u, pos)| uri_to_path(u.as_str()).map(|p| (p, pos))) {
                    Some((path, pos)) => self.jump_to(&path, pos),
                    None => self.message = Some("No definition found here".into()),
                }
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
                let mut items: Vec<Value> = v.as_array().cloned().unwrap_or_default();
                // Preferred fixes first, as IntelliJ shows them.
                items.sort_by_key(|a| !a.get("isPreferred").and_then(Value::as_bool).unwrap_or(false));
                if items.is_empty() {
                    self.message = Some("No quick fixes here".into());
                } else {
                    self.lsp.popup = Some(Popup::Actions { items, sel: 0, server: sid });
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
        if server::specs(lang).is_empty() {
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
        let sid = od.server;
        if self.lsp.server(sid).is_some_and(|s| !s.ready) {
            self.message = Some("Language server is still starting…".into());
        }
        Some((sid, path_to_uri(&key), key))
    }

    fn request_here(&mut self, method: &str, mut extra: Value, p: Pending) {
        let Some((sid, uri, _)) = self.active_server() else { return };
        let pos = to_lsp(&self.doc().rope, self.doc().cursor);
        let mut params = json!({ "textDocument": { "uri": uri }, "position": pos });
        if let (Some(o), Some(e)) = (params.as_object_mut(), extra.as_object_mut()) {
            o.append(e);
        }
        if let Some(s) = self.lsp.server(sid) {
            s.request(method, params, p);
        }
    }

    fn request_completion(&mut self, trigger: Option<char>) {
        let start = word_start(self.doc());
        let Some(path) = self.doc().path.clone() else { return };
        let path = self.lsp.key(&path);
        let ctx = match trigger {
            Some(c) => json!({ "context": { "triggerKind": 2, "triggerCharacter": c.to_string() } }),
            None => json!({ "context": { "triggerKind": 1 } }),
        };
        self.request_here("textDocument/completion", ctx, Pending::Completion { path, start });
    }

    pub fn lsp_rename(&mut self, new_name: String) {
        if new_name.trim().is_empty() {
            return;
        }
        self.request_here("textDocument/rename", json!({ "newName": new_name.trim() }), Pending::Rename);
    }

    fn request_format(&mut self) {
        let Some((sid, uri, key)) = self.active_server() else { return };
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
        let diags: Vec<Value> = self
            .lsp
            .diagnostics
            .get(&key)
            .map(|v| v.iter().filter(|g| g.start.line <= line && g.end.line >= line).map(|g| g.raw.clone()).collect())
            .unwrap_or_default();
        let params = json!({ "textDocument": { "uri": uri }, "range": range, "context": { "diagnostics": diags } });
        if let Some(s) = self.lsp.server(sid) {
            s.request("textDocument/codeAction", params, Pending::CodeActions);
        }
    }

    pub fn lsp_did_save(&mut self) {
        let Some(path) = self.doc().path.clone() else { return };
        self.lsp_sync(Some(self.active));
        let key = self.lsp.key(&path);
        if let Some(sid) = self.lsp.open.get(&key).map(|o| o.server) {
            if let Some(s) = self.lsp.server(sid) {
                s.notify("textDocument/didSave", json!({ "textDocument": { "uri": path_to_uri(&key) } }));
            }
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

    fn accept_completion(&mut self) {
        let Some(Popup::Completion { items, shown, sel, start, .. }) = self.lsp.popup.take() else { return };
        let Some(&k) = shown.get(sel) else { return };
        let it = &items[k];
        let d = &mut self.docs[self.active];
        let from = it.edit_start.map(|p| from_lsp(&d.rope, p)).filter(|&s| s <= d.cursor).unwrap_or(start);
        let mut edits = vec![TextEdit { range: Range::new(to_lsp(&d.rope, from), to_lsp(&d.rope, d.cursor)), new_text: it.insert.clone() }];
        edits.extend(it.extra.iter().cloned());
        apply_edits(d, &edits);
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
        if let Some(p) = &mut self.lsp.popup {
            let list = !matches!(p, Popup::Hover { .. });
            match k.code {
                KeyCode::Esc => {
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
                KeyCode::Enter | KeyCode::Tab if list && !alt => {
                    match self.lsp.popup.take() {
                        Some(Popup::Actions { items, sel, server }) => {
                            if let Some(a) = items.into_iter().nth(sel) {
                                self.run_action(server, a, true);
                            }
                        }
                        other => {
                            self.lsp.popup = other;
                            self.accept_completion();
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
        // Vim normal/visual mode keeps Ctrl+B (page up) and gets Vim's Ctrl+] for go to definition.
        let vim_cmd = self.vim.as_ref().is_some_and(|v| !matches!(v.mode, crate::vim::Mode::Insert));
        match k.code {
            KeyCode::Char(' ') if ctrl => self.request_completion(None),
            KeyCode::Char(']') | KeyCode::Char('5') if ctrl && vim_cmd => {
                self.request_here("textDocument/definition", json!({}), Pending::Definition)
            }
            KeyCode::Char('b') if ctrl && !alt && !vim_cmd => self.request_here("textDocument/definition", json!({}), Pending::Definition),
            KeyCode::Left if ctrl && alt => self.go_back(),
            KeyCode::F(1) => self.request_here("textDocument/hover", json!({}), Pending::Hover),
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

    /// After a normal key: keep the completion list in step with typing, auto-complete after `.`/`::`.
    pub fn smart_after_key(&mut self, k: KeyEvent) {
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if let Some(Popup::Completion { start, prefix, incomplete, .. }) = &self.lsp.popup {
            let (start, incomplete) = (*start, *incomplete);
            let d = self.doc();
            let typed = (d.cursor >= start && d.rope.char_to_line(d.cursor) == d.rope.char_to_line(start))
                .then(|| d.rope.slice(start..d.cursor).to_string())
                .filter(|t| t.chars().all(is_word));
            match typed {
                None => self.lsp.popup = None,
                Some(t) if t != *prefix => {
                    if let Some(p) = &mut self.lsp.popup {
                        p.filter(&t);
                    }
                    let empty = matches!(&self.lsp.popup, Some(Popup::Completion { shown, .. }) if shown.is_empty());
                    if incomplete {
                        self.request_completion(None);
                    } else if empty {
                        self.lsp.popup = None;
                    }
                }
                Some(_) => {}
            }
        }
        let typing = self.vim.as_ref().is_none_or(|v| matches!(v.mode, crate::vim::Mode::Insert));
        if let (KeyCode::Char(c), true, true) = (k.code, plain, typing) {
            let d = self.doc();
            let prev = d.cursor.checked_sub(2).map(|p| d.rope.char(p));
            let lang = d.syntax.lang;
            let trigger = c == '.' || (c == ':' && prev == Some(':') && matches!(lang, Lang::Rust));
            if trigger && self.lsp.popup.is_none() && !server::specs(lang).is_empty() && !self.lsp.no_server.contains(&lang) {
                self.request_completion(Some(c));
            }
        }
    }
}

fn marked(s: MarkedString) -> String {
    match s {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(l) => l.value,
    }
}
