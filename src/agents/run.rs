//! One agent run: a Claude Code conversation started from the panel, its transcript and state,
//! and the `claude -p` child process of the turn in progress.
//!
//! State machine: a turn is sent → `Running`; the turn's `result` line → `Done` (or `Failed` when
//! it reports an error); the process ending without a result → `Failed`; Ctrl+C → `Stopped`.
//! A follow-up (`--resume <session_id>`) starts the next turn from any state but `Running`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::stream::{self, Event};

/// Entries kept per run (oldest dropped first) and characters kept per text entry.
const MAX_ENTRIES: usize = 2000;
const MAX_TEXT: usize = 20_000;
/// How long a stopped agent gets after SIGTERM before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(3);

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Running,
    Done,
    Failed,
    Stopped,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Stopped => "stopped",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolStatus {
    Pending,
    Ok,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Entry {
    /// What was sent (as typed; `ctx` names the file/lines that were added to it).
    User { text: String, ctx: Option<String> },
    Text { text: String },
    Tool { id: String, name: String, arg: String, status: ToolStatus, note: String },
    /// Muted line: session, mode, model of a turn.
    Info { text: String },
    Done { ok: bool, subtype: String, ms: u64, cost: f64, turns: u64, denials: usize },
    Error { text: String },
    Stopped,
}

/// What a turn is started with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Turn {
    /// The full prompt (typed text plus the added file context).
    pub prompt: String,
    /// The typed text and a label of the added context, for the transcript.
    pub shown: String,
    pub ctx: Option<String>,
    /// `--permission-mode` value; `None` = the CLI's default (the user's settings).
    pub mode: Option<String>,
    /// `--agent` value.
    pub agent: Option<String>,
}

pub enum Msg {
    Line(String),
    Stderr(String),
    /// The process ended (exit code; `None` = killed by a signal or couldn't be waited for).
    Exit(Option<i32>),
}

/// The child process of the running turn.
pub struct Proc {
    pub rx: Receiver<Msg>,
    pub pid: u32,
    pub started: Instant,
    stop_sent: Option<Instant>,
    killed: bool,
    stderr: Vec<String>,
}

impl Proc {
    /// Starts `program args` in `cwd` in its own process group, writes `stdin` to it and streams
    /// its stdout lines (and stderr) back through a channel from background threads.
    pub fn spawn(program: &str, args: &[String], cwd: &Path, stdin: String) -> std::io::Result<Proc> {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Own process group: Stop can end the agent and the commands it started, and the
            // terminal's own signals don't reach it.
            cmd.process_group(0);
        }
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let (tx, rx) = mpsc::channel();
        let mut input = child.stdin.take();
        let out = child.stdout.take();
        let err = child.stderr.take();
        let etx = tx.clone();
        let err_thread = thread::spawn(move || {
            if let Some(err) = err {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    let _ = etx.send(Msg::Stderr(line));
                }
            }
        });
        thread::spawn(move || {
            if let Some(mut i) = input.take() {
                let _ = i.write_all(stdin.as_bytes());
                // Dropping it closes stdin, so claude starts.
            }
            if let Some(out) = out {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if tx.send(Msg::Line(line)).is_err() {
                        break;
                    }
                }
            }
            let _ = err_thread.join();
            let code = child.wait().ok().and_then(|s| s.code());
            let _ = tx.send(Msg::Exit(code));
        });
        Ok(Proc { rx, pid, started: Instant::now(), stop_sent: None, killed: false, stderr: Vec::new() })
    }

    fn signal(&self, sig: &str) {
        #[cfg(unix)]
        {
            // The whole group (negative pid): claude and the shells it started.
            let _ = Command::new("kill")
                .args([sig, "--", &format!("-{}", self.pid)])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(unix))]
        let _ = sig;
    }

    pub fn stop(&mut self) {
        if self.stop_sent.is_none() {
            self.signal("-TERM");
            self.stop_sent = Some(Instant::now());
        }
    }

    /// Kills a stopped process that ignored SIGTERM. Called every tick.
    pub fn escalate(&mut self) {
        if !self.killed && self.stop_sent.is_some_and(|t| t.elapsed() >= STOP_GRACE) {
            self.signal("-KILL");
            self.killed = true;
        }
    }

    pub fn stopping(&self) -> bool {
        self.stop_sent.is_some()
    }
}

/// The command-line arguments for one turn. Prompt goes on stdin (no quoting issues, any length).
pub fn claude_args(turn: &Turn, resume: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"].iter().map(|s| s.to_string()).collect();
    if let Some(sid) = resume {
        a.extend(["--resume".into(), sid.into()]);
    }
    if let Some(m) = &turn.mode {
        a.extend(["--permission-mode".into(), m.clone()]);
    }
    if let Some(ag) = &turn.agent {
        a.extend(["--agent".into(), ag.clone()]);
    }
    a
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Serialize, Deserialize)]
pub struct Run {
    pub id: u64,
    pub title: String,
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    /// Requested mode and agent of the latest turn (`None` = CLI default / no agent).
    pub mode: Option<String>,
    pub agent: Option<String>,
    /// The permission mode the CLI reported for the latest turn.
    pub mode_used: Option<String>,
    pub model: Option<String>,
    pub state: State,
    /// Unix seconds of the first and the latest turn.
    pub started: u64,
    pub updated: u64,
    /// Wall time of the finished turns, and their total cost.
    pub ms: u64,
    pub cost: f64,
    pub turns: u32,
    pub entries: Vec<Entry>,
    #[serde(skip)]
    pub proc: Option<Proc>,
    /// Edit-tool calls waiting for their result: tool id → the file they write.
    #[serde(skip)]
    pending_files: HashMap<String, PathBuf>,
}

/// What feeding the run a line or an exit changed, for the panel to act on.
#[derive(Default, Debug, PartialEq)]
pub struct Changes {
    /// Files an edit tool wrote successfully (absolute, as the agent named them).
    pub files: Vec<PathBuf>,
    /// The session id became known or changed (worth saving).
    pub session: bool,
    /// The turn finished (any way).
    pub finished: bool,
}

impl Run {
    pub fn new(id: u64, cwd: PathBuf, first_prompt: &str) -> Run {
        let title = first_prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("agent").to_string();
        Run {
            id,
            title,
            cwd,
            session_id: None,
            mode: None,
            agent: None,
            mode_used: None,
            model: None,
            state: State::Done,
            started: now_secs(),
            updated: now_secs(),
            ms: 0,
            cost: 0.0,
            turns: 0,
            entries: Vec::new(),
            proc: None,
            pending_files: HashMap::new(),
        }
    }

    pub fn running(&self) -> bool {
        self.state == State::Running
    }

    /// Wall time so far, including the running turn.
    pub fn elapsed_ms(&self) -> u64 {
        self.ms + self.proc.as_ref().map_or(0, |p| p.started.elapsed().as_millis() as u64)
    }

    fn push(&mut self, e: Entry) {
        self.entries.push(e);
        if self.entries.len() > MAX_ENTRIES {
            let n = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..n);
        }
    }

    /// Records a new turn (the transcript part; the process is attached by the caller).
    pub fn begin_turn(&mut self, turn: &Turn) {
        self.state = State::Running;
        self.turns += 1;
        self.updated = now_secs();
        self.mode = turn.mode.clone();
        self.agent = turn.agent.clone();
        self.mode_used = None;
        self.push(Entry::User { text: turn.shown.clone(), ctx: turn.ctx.clone() });
    }

    /// Starts `program` (normally `claude`) for `turn`, resuming this run's session if it has one.
    pub fn start(&mut self, program: &str, turn: &Turn) {
        let args = claude_args(turn, self.session_id.as_deref());
        self.begin_turn(turn);
        match Proc::spawn(program, &args, &self.cwd.clone(), turn.prompt.clone()) {
            Ok(p) => self.proc = Some(p),
            Err(e) => {
                self.push(Entry::Error { text: format!("couldn't start `{program}`: {e}") });
                self.state = State::Failed;
            }
        }
    }

    /// Ctrl+C: ends the running turn.
    pub fn stop(&mut self) {
        if let Some(p) = &mut self.proc {
            p.stop();
        }
    }

    /// Handles one stdout line of the running turn.
    pub fn feed_line(&mut self, line: &str) -> Changes {
        let mut ch = Changes::default();
        for ev in stream::parse_line(line, &self.cwd) {
            match ev {
                Event::Init { session_id, model, mode } => {
                    if !session_id.is_empty() && self.session_id.as_deref() != Some(&session_id) {
                        self.session_id = Some(session_id.clone());
                        ch.session = true;
                    }
                    let mut info = format!("mode {}", if mode.is_empty() { "?" } else { &mode });
                    if self.mode.is_none() {
                        info.push_str(" (CLI default)");
                    }
                    if let Some(a) = &self.agent {
                        info.push_str(&format!(" · agent {a}"));
                    }
                    if !model.is_empty() {
                        info.push_str(&format!(" · {model}"));
                    }
                    info.push_str(&format!(" · session {}", short_id(&session_id)));
                    self.push(Entry::Info { text: info });
                    self.mode_used = Some(mode);
                    self.model = Some(model).filter(|m| !m.is_empty());
                }
                Event::Text(t) => self.push(Entry::Text { text: cap(t) }),
                Event::ToolUse { id, name, arg, file } => {
                    if let Some(f) = file {
                        self.pending_files.insert(id.clone(), PathBuf::from(f));
                    }
                    self.push(Entry::Tool { id, name, arg, status: ToolStatus::Pending, note: String::new() });
                }
                Event::ToolResult { id, ok, text } => {
                    if let Some(f) = self.pending_files.remove(&id).filter(|_| ok) {
                        ch.files.push(f);
                    }
                    if let Some(Entry::Tool { status, note, .. }) =
                        self.entries.iter_mut().rev().find(|e| matches!(e, Entry::Tool { id: i, .. } if *i == id))
                    {
                        *status = if ok { ToolStatus::Ok } else { ToolStatus::Failed };
                        if !ok {
                            *note = text;
                        }
                    }
                }
                Event::Denied { .. } => {} // the tool's failed result shows it; the summary counts them
                Event::Result(o) => {
                    if !o.session_id.is_empty() && self.session_id.as_deref() != Some(&o.session_id) {
                        self.session_id = Some(o.session_id.clone());
                        ch.session = true;
                    }
                    self.cost += o.cost;
                    if !o.ok && !o.text.trim().is_empty() {
                        self.push(Entry::Error { text: cap(o.text.clone()) });
                    }
                    self.push(Entry::Done { ok: o.ok, subtype: o.subtype, ms: o.duration_ms, cost: o.cost, turns: o.turns, denials: o.denials });
                    self.state = if o.ok { State::Done } else { State::Failed };
                }
            }
        }
        ch
    }

    /// The turn's process ended. A turn without a result line failed (or was stopped).
    pub fn finish(&mut self, code: Option<i32>) -> Changes {
        let proc = self.proc.take();
        if let Some(p) = &proc {
            self.ms += p.started.elapsed().as_millis() as u64;
        }
        self.updated = now_secs();
        let stopped = proc.as_ref().is_some_and(Proc::stopping);
        if self.state == State::Running {
            if stopped {
                self.mark_tools_failed();
                self.push(Entry::Stopped);
                self.state = State::Stopped;
            } else {
                let tail: Vec<String> = proc.map(|p| p.stderr).unwrap_or_default().into_iter().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
                let why = match code {
                    Some(c) => format!("claude exited with code {c} before finishing"),
                    None => "claude ended without a result".to_string(),
                };
                self.push(Entry::Error { text: if tail.is_empty() { why } else { format!("{why}: {}", tail.join(" ")) } });
                self.mark_tools_failed();
                self.state = State::Failed;
            }
        }
        Changes { finished: true, ..Changes::default() }
    }

    fn mark_tools_failed(&mut self) {
        for e in &mut self.entries {
            if let Entry::Tool { status: s @ ToolStatus::Pending, .. } = e {
                *s = ToolStatus::Failed;
            }
        }
    }

    /// Drains the running turn's channel. Returns everything that changed.
    pub fn poll(&mut self) -> Changes {
        let mut all = Changes::default();
        let Some(p) = &mut self.proc else { return all };
        p.escalate();
        let mut msgs = Vec::new();
        while let Ok(m) = p.rx.try_recv() {
            msgs.push(m);
        }
        for m in msgs {
            let ch = match m {
                Msg::Line(l) => self.feed_line(&l),
                Msg::Stderr(l) => {
                    if let Some(p) = &mut self.proc {
                        p.stderr.push(l);
                        if p.stderr.len() > 50 {
                            p.stderr.remove(0);
                        }
                    }
                    Changes::default()
                }
                Msg::Exit(code) => self.finish(code),
            };
            all.files.extend(ch.files);
            all.session |= ch.session;
            all.finished |= ch.finished;
        }
        all
    }

    /// After loading from disk: a run that was running when the editor closed can't be followed.
    pub fn after_load(&mut self) {
        if self.state == State::Running {
            self.mark_tools_failed();
            self.push(Entry::Info { text: "the editor closed while this was running".into() });
            self.state = State::Stopped;
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // Closing the editor (or removing a run) ends its agent: nothing would read its output.
        if let Some(p) = &mut self.proc {
            p.signal("-TERM");
        }
    }
}

pub fn short_id(s: &str) -> &str {
    s.get(..8).unwrap_or(s)
}

fn cap(mut t: String) -> String {
    if t.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        t.truncate(end);
        t.push_str(" …");
    }
    t
}

pub fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{}.{}s", s, (ms % 1000) / 100)
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s / 60) % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(text: &str, mode: Option<&str>) -> Turn {
        Turn { prompt: text.into(), shown: text.into(), ctx: None, mode: mode.map(Into::into), agent: None }
    }

    fn fed(sample: &str) -> (Run, Changes) {
        let mut r = Run::new(1, PathBuf::from("/tmp/scratch"), "create hello.txt");
        r.begin_turn(&turn("create hello.txt", Some("acceptEdits")));
        let mut all = Changes::default();
        for l in sample.lines() {
            let ch = r.feed_line(l);
            all.files.extend(ch.files);
            all.session |= ch.session;
        }
        (r, all)
    }

    #[test]
    fn args_use_only_flags_from_claude_help() {
        let mut t = turn("x", None);
        assert_eq!(claude_args(&t, None), ["-p", "--output-format", "stream-json", "--verbose"]);
        t.mode = Some("plan".into());
        t.agent = Some("aa-qa".into());
        assert_eq!(
            claude_args(&t, Some("abc")),
            ["-p", "--output-format", "stream-json", "--verbose", "--resume", "abc", "--permission-mode", "plan", "--agent", "aa-qa"]
        );
        assert!(!claude_args(&t, None).iter().any(|a| a.contains("dangerously")));
    }

    #[test]
    fn a_write_run_ends_done_with_the_file_reported() {
        let (r, ch) = fed(include_str!("testdata/write-run.jsonl"));
        assert_eq!(r.state, State::Done);
        assert_eq!(r.session_id.as_deref(), Some("b0eef9d3-b00c-4198-8db6-544dac4f792b"));
        assert_eq!(r.mode_used.as_deref(), Some("acceptEdits"));
        assert!(ch.session);
        assert_eq!(ch.files, vec![PathBuf::from("/tmp/scratch/hello.txt")]);
        assert!(matches!(&r.entries[2], Entry::Tool { name, status: ToolStatus::Ok, .. } if name == "Write"));
        assert!(matches!(r.entries.last(), Some(Entry::Done { ok: true, turns: 2, .. })));
        assert!((r.cost - 0.1125078).abs() < 1e-9);
    }

    #[test]
    fn a_denied_write_is_a_failed_tool_and_no_file_change() {
        let (r, ch) = fed(include_str!("testdata/denied-run.jsonl"));
        assert!(ch.files.is_empty());
        assert!(r.entries.iter().any(|e| matches!(e, Entry::Tool { status: ToolStatus::Failed, note, .. } if note.contains("haven't granted"))));
        assert!(matches!(r.entries.last(), Some(Entry::Done { denials: 1, .. })));
    }

    #[test]
    fn exit_without_result_fails_and_stop_marks_stopped() {
        let mut r = Run::new(1, PathBuf::from("/tmp"), "x");
        r.begin_turn(&turn("x", None));
        r.finish(Some(1));
        assert_eq!(r.state, State::Failed);
        assert!(matches!(r.entries.last(), Some(Entry::Error { text }) if text.contains("code 1")));

        // A real process, stopped while it runs.
        let mut r = Run::new(2, std::env::temp_dir(), "sleep");
        r.begin_turn(&turn("sleep", None));
        r.proc = Some(Proc::spawn("sh", &["-c".into(), "echo started; sleep 30".into()], &std::env::temp_dir(), String::new()).unwrap());
        r.stop();
        let t = Instant::now();
        while r.proc.is_some() && t.elapsed() < Duration::from_secs(5) {
            r.poll();
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(r.state, State::Stopped);
        assert_eq!(r.entries.last(), Some(&Entry::Stopped));
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_process_streams_lines_to_the_run() {
        let mut r = Run::new(3, std::env::temp_dir(), "x");
        r.begin_turn(&turn("x", None));
        // `cat` echoes the stdin "prompt" back: here, a recorded run.
        r.proc = Some(Proc::spawn("cat", &[], &std::env::temp_dir(), include_str!("testdata/write-run.jsonl").to_string()).unwrap());
        let t = Instant::now();
        let mut ch = Changes::default();
        while r.proc.is_some() && t.elapsed() < Duration::from_secs(5) {
            let c = r.poll();
            ch.files.extend(c.files);
            ch.finished |= c.finished;
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ch.finished);
        assert_eq!(r.state, State::Done);
        assert_eq!(ch.files.len(), 1);
    }

    #[test]
    fn loaded_running_runs_become_stopped_and_round_trip() {
        let (mut r, _) = fed(include_str!("testdata/write-run.jsonl"));
        r.state = State::Running;
        let json = serde_json::to_string(&r).unwrap();
        let mut back: Run = serde_json::from_str(&json).unwrap();
        back.after_load();
        assert_eq!(back.state, State::Stopped);
        assert_eq!(back.session_id, r.session_id);
        assert_eq!(back.entries.len(), r.entries.len() + 1);
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_ms(8_497), "8.4s");
        assert_eq!(fmt_ms(62_000), "1m02s");
        assert_eq!(fmt_ms(3_900_000), "1h05m");
    }
}
