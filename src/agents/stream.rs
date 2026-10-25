//! Parsing `claude -p --output-format stream-json --verbose` output: one JSON object per line.
//! Only what the panel shows is read; unknown lines and fields are skipped, so newer CLI
//! versions with extra events still work.

use std::path::Path;

use serde_json::Value;

/// Tools whose successful result means a file on disk changed (path in `file_path` /
/// `notebook_path`).
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `system/init`: the session starts (also on every resumed turn).
    Init { session_id: String, model: String, mode: String },
    /// Assistant text (one content block).
    Text(String),
    /// A tool call, `arg` already shortened for one line; `file` = the absolute path an edit tool writes.
    ToolUse { id: String, name: String, arg: String, file: Option<String> },
    /// The result of a tool call; `text` = first line of it.
    ToolResult { id: String, ok: bool, text: String },
    /// `system/permission_denied`: a tool call was refused by the permission mode.
    Denied { tool: String, message: String },
    /// The final `result` line of a turn.
    Result(Outcome),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub ok: bool,
    pub subtype: String,
    pub text: String,
    pub cost: f64,
    pub duration_ms: u64,
    pub turns: u64,
    pub session_id: String,
    pub denials: usize,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Parses one output line. Lines that aren't JSON or carry nothing to show give no events.
pub fn parse_line(line: &str, cwd: &Path) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { return Vec::new() };
    let mut out = Vec::new();
    match v.get("type").and_then(Value::as_str) {
        Some("system") => match v.get("subtype").and_then(Value::as_str) {
            Some("init") => out.push(Event::Init { session_id: s(&v, "session_id"), model: s(&v, "model"), mode: s(&v, "permissionMode") }),
            Some("permission_denied") => out.push(Event::Denied { tool: s(&v, "tool_name"), message: s(&v, "message") }),
            _ => {}
        },
        Some("assistant") => {
            for c in content(&v) {
                match c.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let t = s(c, "text");
                        if !t.trim().is_empty() {
                            out.push(Event::Text(t));
                        }
                    }
                    Some("tool_use") => {
                        let name = s(c, "name");
                        let input = c.get("input").cloned().unwrap_or(Value::Null);
                        let file = EDIT_TOOLS
                            .contains(&name.as_str())
                            .then(|| input.get("file_path").or(input.get("notebook_path")).and_then(Value::as_str).map(str::to_string))
                            .flatten();
                        out.push(Event::ToolUse { id: s(c, "id"), arg: tool_arg(&name, &input, cwd), name, file });
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            for c in content(&v) {
                if c.get("type").and_then(Value::as_str) == Some("tool_result") {
                    let ok = !c.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    out.push(Event::ToolResult { id: s(c, "tool_use_id"), ok, text: first_line(&result_text(c.get("content"))) });
                }
            }
        }
        Some("result") => out.push(Event::Result(Outcome {
            ok: !v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            subtype: s(&v, "subtype"),
            text: s(&v, "result"),
            cost: v.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
            duration_ms: v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
            turns: v.get("num_turns").and_then(Value::as_u64).unwrap_or(0),
            session_id: s(&v, "session_id"),
            denials: v.get("permission_denials").and_then(Value::as_array).map_or(0, Vec::len),
        })),
        _ => {}
    }
    out
}

fn content(v: &Value) -> &[Value] {
    v.pointer("/message/content").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// A tool result's content is a string or a list of `{type: text, text}` blocks.
fn result_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(items)) => items.iter().filter_map(|i| i.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn first_line(t: &str) -> String {
    t.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string()
}

/// Paths inside the project are shown relative to it.
fn short_path(p: &str, cwd: &Path) -> String {
    let path = Path::new(p);
    for base in [cwd.to_path_buf(), crate::project::canonical(cwd)] {
        if let Ok(r) = path.strip_prefix(&base) {
            if !r.as_os_str().is_empty() {
                return r.display().to_string();
            }
        }
    }
    p.to_string()
}

/// The one-line argument shown after a tool's name, e.g. `Edit src/app.rs`, `Bash cargo test`.
pub fn tool_arg(name: &str, input: &Value, cwd: &Path) -> String {
    let get = |k: &str| input.get(k).and_then(Value::as_str);
    let arg = match name {
        "Read" | "Edit" | "Write" | "MultiEdit" => get("file_path").map(|p| short_path(p, cwd)),
        "NotebookEdit" => get("notebook_path").map(|p| short_path(p, cwd)),
        "Bash" => get("command").map(|c| first_line(c)),
        "Grep" | "Glob" => get("pattern").map(|p| match get("path") {
            Some(dir) => format!("{p} in {}", short_path(dir, cwd)),
            None => p.to_string(),
        }),
        "WebFetch" => get("url").map(str::to_string),
        "WebSearch" => get("query").map(str::to_string),
        "Task" | "Agent" => get("description").or(get("subagent_type")).map(str::to_string),
        "TodoWrite" => input.get("todos").and_then(Value::as_array).map(|t| format!("{} todos", t.len())),
        _ => None,
    };
    arg.or_else(|| {
        // Anything else: its first string field.
        input.as_object()?.values().find_map(Value::as_str).map(first_line)
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITE_RUN: &str = include_str!("testdata/write-run.jsonl");
    const DENIED_RUN: &str = include_str!("testdata/denied-run.jsonl");

    fn events(sample: &str) -> Vec<Event> {
        sample.lines().flat_map(|l| parse_line(l, Path::new("/tmp/scratch"))).collect()
    }

    #[test]
    fn recorded_write_run() {
        let ev = events(WRITE_RUN);
        assert_eq!(
            ev[0],
            Event::Init { session_id: "b0eef9d3-b00c-4198-8db6-544dac4f792b".into(), model: "claude-opus-5-5".into(), mode: "acceptEdits".into() }
        );
        match &ev[1] {
            Event::ToolUse { name, arg, file, .. } => {
                assert_eq!(name, "Write");
                assert_eq!(arg, "hello.txt");
                assert_eq!(file.as_deref(), Some("/tmp/scratch/hello.txt"));
            }
            e => panic!("expected the Write call, got {e:?}"),
        }
        assert!(matches!(&ev[2], Event::ToolResult { ok: true, text, .. } if text.starts_with("File created successfully")));
        assert_eq!(ev[3], Event::Text("I created `hello.txt` containing \"hi\". Done.".into()));
        let Event::Result(o) = &ev[4] else { panic!("no result: {ev:?}") };
        assert!(o.ok);
        assert_eq!((o.subtype.as_str(), o.turns, o.duration_ms, o.denials), ("success", 2, 7497, 0));
        assert!((o.cost - 0.1125078).abs() < 1e-9);
        assert_eq!(ev.len(), 5, "thinking blocks and rate-limit lines are skipped");
    }

    #[test]
    fn recorded_denied_run() {
        let ev = events(DENIED_RUN);
        assert!(matches!(&ev[0], Event::Init { mode, .. } if mode == "default"));
        assert!(matches!(&ev[2], Event::Denied { tool, .. } if tool == "Write"));
        assert!(matches!(&ev[3], Event::ToolResult { ok: false, text, .. } if text.contains("haven't granted")));
        let Event::Result(o) = ev.last().unwrap() else { panic!() };
        assert_eq!(o.denials, 1);
    }

    #[test]
    fn junk_and_unknown_lines_are_ignored() {
        let cwd = Path::new("/p");
        assert!(parse_line("", cwd).is_empty());
        assert!(parse_line("not json", cwd).is_empty());
        assert!(parse_line(r#"{"type":"stream_event","event":{}}"#, cwd).is_empty());
        assert!(parse_line(r#"{"type":"system","subtype":"api_retry"}"#, cwd).is_empty());
    }

    #[test]
    fn tool_args() {
        let cwd = Path::new("/p");
        let v = |j: &str| serde_json::from_str::<Value>(j).unwrap();
        assert_eq!(tool_arg("Edit", &v(r#"{"file_path":"/p/src/app.rs"}"#), cwd), "src/app.rs");
        assert_eq!(tool_arg("Read", &v(r#"{"file_path":"/etc/hosts"}"#), cwd), "/etc/hosts");
        assert_eq!(tool_arg("Bash", &v(r#"{"command":"cargo test\necho hi","description":"x"}"#), cwd), "cargo test");
        assert_eq!(tool_arg("Grep", &v(r#"{"pattern":"fn main","path":"/p/src"}"#), cwd), "fn main in src");
        assert_eq!(tool_arg("TodoWrite", &v(r#"{"todos":[1,2,3]}"#), cwd), "3 todos");
        assert_eq!(tool_arg("mcp__x__y", &v(r#"{"n":1,"q":"hello"}"#), cwd), "hello");
        // Tool results given as content blocks.
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"\n  line one\nline two"}]}]}}"#;
        assert_eq!(parse_line(line, cwd), vec![Event::ToolResult { id: "t".into(), ok: true, text: "line one".into() }]);
    }
}
