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
use crate::syntax::Lang;

pub struct Spec {
    pub cmd: &'static str,
    pub args: &'static [&'static str],
    /// Files that mark a project root, nearest first.
    pub markers: &'static [&'static str],
}

/// Servers to try for a language, in order of preference.
pub fn specs(lang: Lang) -> &'static [Spec] {
    const RUST: &[Spec] = &[Spec { cmd: "rust-analyzer", args: &[], markers: &["Cargo.toml"] }];
    const TS: &[Spec] = &[Spec {
        cmd: "typescript-language-server",
        args: &["--stdio"],
        markers: &["tsconfig.json", "jsconfig.json", "package.json"],
    }];
    const PY_MARKERS: &[&str] = &["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt"];
    const PY: &[Spec] = &[
        Spec { cmd: "pyright-langserver", args: &["--stdio"], markers: PY_MARKERS },
        Spec { cmd: "basedpyright-langserver", args: &["--stdio"], markers: PY_MARKERS },
        Spec { cmd: "pylsp", args: &[], markers: PY_MARKERS },
    ];
    const JAVA: &[Spec] = &[Spec { cmd: "jdtls", args: &[], markers: &["pom.xml", "build.gradle", "build.gradle.kts"] }];
    match lang {
        Lang::Rust => RUST,
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => TS,
        Lang::Python => PY,
        Lang::Java => JAVA,
        _ => &[],
    }
}

/// Walks up from the file for one of `markers`, then for `.git`; else the file's folder.
pub fn find_root(file: &Path, markers: &[&str]) -> PathBuf {
    let start = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let has = |dir: &Path, names: &[&str]| names.iter().any(|m| dir.join(m).exists());
    let up = |names: &[&str]| start.ancestors().find(|d| has(d, names)).map(Path::to_path_buf);
    up(markers).or_else(|| up(&[".git"])).unwrap_or(start)
}

/// Finds `cmd` on PATH, plus the usual per-user tool folders (the Mac keeps ~/.cargo/bin off PATH).
pub fn which(cmd: &str) -> Option<PathBuf> {
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
    Completion { path: PathBuf, start: usize },
    Definition,
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
    pub fn start(id: usize, exe: &Path, spec: &Spec, root: &Path, events: Sender<Event>) -> std::io::Result<Server> {
        // Tools the server runs (cargo, rustfmt, node) usually sit next to it, even when that
        // folder isn't on PATH (the Mac's ~/.cargo/bin).
        let mut path: Vec<PathBuf> = exe.parent().map(Path::to_path_buf).into_iter().collect();
        path.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
        let mut child = Command::new(exe)
            .env("PATH", std::env::join_paths(path).unwrap_or_default())
            .args(spec.args)
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
        let name = spec.cmd.to_string();
        let mut s = Server {
            id,
            name,
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
        s.request_now("initialize", super::initialize_params(root), Pending::Initialize);
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
        assert_eq!(find_root(&file, &["Cargo.toml"]), tmp, "falls back to the .git folder");
        std::fs::write(tmp.join("crate/Cargo.toml"), "").unwrap();
        assert_eq!(find_root(&file, &["Cargo.toml"]), tmp.join("crate"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn languages_have_servers() {
        assert_eq!(specs(Lang::Rust)[0].cmd, "rust-analyzer");
        assert_eq!(specs(Lang::Tsx)[0].cmd, "typescript-language-server");
        assert!(specs(Lang::Python).iter().any(|s| s.cmd == "pylsp"));
        assert!(specs(Lang::Toml).is_empty());
        assert!(which("sh").is_some());
        assert!(which("surely-not-a-real-tool-xyz").is_none());
    }
}
