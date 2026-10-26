//! Language server processes: which server for which language, the project root, and the
//! stdio plumbing (a writer thread, a reader thread, a stderr thread keeping the last line).

use std::collections::HashMap;
use std::io::{BufReader, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::jsonrpc::{self, Incoming};
use super::config::ServerCfg;

/// Walks up from the file for one of `markers`, then for `.git`; else the file's folder.
pub fn find_root(file: &Path, markers: &[String]) -> PathBuf {
    let start = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let has = |dir: &Path, names: &[String]| names.iter().any(|m| dir.join(m).exists());
    let up = |names: &[String]| start.ancestors().find(|d| has(d, names)).map(Path::to_path_buf);
    up(markers).or_else(|| up(&[".git".to_string()])).unwrap_or(start)
}

/// Finds `cmd` on PATH, plus the usual per-user tool folders (the Mac keeps ~/.cargo/bin off PATH).
/// A command with a `/` is taken as a path (`~/` expanded).
pub fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = match cmd.strip_prefix("~/") {
            Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
            None => PathBuf::from(cmd),
        };
        return p.is_file().then_some(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend([home.join(".cargo/bin"), home.join(".local/bin"), home.join(".bun/bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
    dirs.into_iter().map(|d| d.join(cmd)).find(|p| p.is_file())
}

/// `CATLAS_LSP_LOG=/some/file` logs all LSP traffic there (for debugging servers).
fn log_file() -> Option<std::fs::File> {
    let path = std::env::var_os("CATLAS_LSP_LOG")?;
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

fn log(f: &Option<std::fs::File>, dir: &str, bytes: &[u8]) {
    if let Some(mut f) = f.as_ref() {
        let text = String::from_utf8_lossy(bytes);
        let body = text.split_once("\r\n\r\n").map_or(&*text, |(_, b)| b);
        let short: String = body.chars().take(2000).collect();
        let _ = writeln!(f, "{dir} {short}");
    }
}

/// What a request was for, so its response can be handled.
pub enum Pending {
    Initialize,
    /// `seq` = the request's number (only the newest is shown); `auto` = opened by typing.
    Completion { path: PathBuf, start: usize, req: lsp_types::Position, seq: u64, auto: bool },
    /// Definition or implementation (`what`), for the word at the cursor.
    Definition { what: &'static str, word: String },
    References { word: String },
    Structure { path: PathBuf },
    Symbols { seq: u64 },
    Signature { path: PathBuf, line: usize },
    /// Pull diagnostics (`textDocument/diagnostic`, LSP 3.17) for one file.
    PullDiags { path: PathBuf },
    Hover,
    Rename,
    Format { path: PathBuf },
    CodeActions,
    ResolveAction,
    Ignore,
}

pub struct Server {
    pub id: usize,
    pub name: String,
    pub cfg: ServerCfg,
    pub root: PathBuf,
    child: Child,
    out: Sender<Vec<u8>>,
    next_id: u64,
    pub ready: bool,
    pub caps: Value,
    queued: Vec<Value>,
    pub pending: HashMap<u64, Pending>,
    /// Last line the server wrote to stderr (shown if it dies).
    pub stderr_tail: Arc<Mutex<String>>,
}

/// One message from server `id`; `None` when its output closed (it exited).
pub type Event = (usize, Option<Incoming>);

impl Server {
    pub fn start(id: usize, exe: &Path, cfg: &ServerCfg, root: &Path, events: Sender<Event>) -> std::io::Result<Server> {
        // Tools the server runs (cargo, rustfmt, node) usually sit next to it, even when that
        // folder isn't on PATH (the Mac's ~/.cargo/bin).
        let mut path: Vec<PathBuf> = exe.parent().map(Path::to_path_buf).into_iter().collect();
        path.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
        let mut child = Command::new(exe)
            .env("PATH", std::env::join_paths(path).unwrap_or_default())
            .args(&cfg.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let (out, out_rx) = mpsc::channel::<Vec<u8>>();
        let log_out = log_file();
        std::thread::spawn(move || {
            for bytes in out_rx {
                log(&log_out, "->", &bytes);
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let log_in = log_file();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            while let Ok(Some(msg)) = jsonrpc::read_message(&mut r) {
                log(&log_in, "<-", msg.to_string().as_bytes());
                if let Some(m) = jsonrpc::classify(msg) {
                    if events.send((id, Some(m))).is_err() {
                        return;
                    }
                }
            }
            let _ = events.send((id, None));
        });
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let tail = stderr_tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    *tail.lock().unwrap() = line;
                }
            }
        });
        let mut s = Server {
            id,
            name: cfg.name.clone(),
            cfg: cfg.clone(),
            root: root.to_path_buf(),
            child,
            out,
            next_id: 1,
            ready: false,
            caps: Value::Null,
            queued: Vec::new(),
            pending: HashMap::new(),
            stderr_tail,
        };
        let vars = super::config::vars(exe, root);
        s.cfg.init_options = super::config::expand(&cfg.init_options, &vars);
        s.cfg.settings = super::config::expand(&cfg.settings, &vars);
        let init = super::initialize_params(root, &s.cfg.init_options);
        s.request_now("initialize", init, Pending::Initialize);
        Ok(s)
    }

    fn send(&self, msg: &Value) {
        let _ = self.out.send(jsonrpc::encode(msg));
    }

    fn request_now(&mut self, method: &str, params: Value, p: Pending) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, p);
        self.send(&jsonrpc::request(id, method, params));
        id
    }

    /// Sends a request (queued until the server is initialized).
    pub fn request(&mut self, method: &str, params: Value, p: Pending) {
        if self.ready {
            self.request_now(method, params, p);
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.pending.insert(id, p);
            self.queued.push(jsonrpc::request(id, method, params));
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let msg = jsonrpc::notification(method, params);
        if self.ready { self.send(&msg) } else { self.queued.push(msg) }
    }

    /// Whether the server wants diagnostics pulled (TypeScript 7 does, instead of pushing them).
    pub fn pulls_diagnostics(&self) -> bool {
        self.caps.get("diagnosticProvider").is_some_and(|d| !d.is_null())
    }

    pub fn reply(&self, id: Value, result: Value) {
        self.send(&jsonrpc::response(id, result));
    }

    /// After the `initialize` response: `initialized`, then everything queued.
    pub fn initialized(&mut self, caps: Value) {
        self.caps = caps;
        self.ready = true;
        self.send(&jsonrpc::notification("initialized", serde_json::json!({})));
        for m in std::mem::take(&mut self.queued) {
            self.send(&m);
        }
    }

    pub fn shutdown(&mut self) {
        self.request_now("shutdown", Value::Null, Pending::Ignore);
        self.send(&jsonrpc::notification("exit", Value::Null));
    }

    pub fn kill(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_the_nearest_marker_folder() {
        let tmp = std::env::temp_dir().join(format!("catlas-root-{}", std::process::id()));
        let file = tmp.join("crate/src/deep/x.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        let m = ["Cargo.toml".to_string()];
        assert_eq!(find_root(&file, &m), tmp, "falls back to the .git folder");
        std::fs::write(tmp.join("crate/Cargo.toml"), "").unwrap();
        assert_eq!(find_root(&file, &m), tmp.join("crate"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn which_finds_commands_and_paths() {
        assert!(which("sh").is_some());
        assert!(which("/bin/sh").is_some());
        assert!(which("/bin/surely-not-here").is_none());
        assert!(which("surely-not-a-real-tool-xyz").is_none());
    }
}
