//! What the installed `claude` CLI offers: the `--permission-mode` choices and whether it has
//! `--agent`, read from `claude --help` in the background (so the panel never offers a flag the
//! CLI doesn't list), and the agent types in `~/.claude/agents` and `<project>/.claude/agents`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Never offered from the panel: it switches all permission checks off (decision 0005).
const EXCLUDED_MODES: &[&str] = &["bypassPermissions"];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CliInfo {
    pub modes: Vec<String>,
    pub has_agent: bool,
}

/// `claude` from PATH, else the native installer's place (`~/.local/bin/claude`).
pub fn find_claude() -> String {
    if let Ok(c) = std::env::var("CATLAS_CLAUDE") {
        return c;
    }
    let on_path = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("claude").is_file()));
    if !on_path {
        if let Some(home) = std::env::var_os("HOME") {
            let p = PathBuf::from(home).join(".local/bin/claude");
            if p.is_file() {
                return p.display().to_string();
            }
        }
    }
    "claude".into()
}

/// Reads the flags out of `claude --help` text (wrapped lines and all).
pub fn parse_help(help: &str) -> CliInfo {
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut modes = Vec::new();
    if let Some(i) = flat.find("--permission-mode <mode>") {
        let rest = &flat[i..];
        if let Some(c) = rest.find("(choices:") {
            let list = &rest[c + 9..];
            let list = &list[..list.find(')').unwrap_or(list.len())];
            for part in list.split(',') {
                let m = part.trim().trim_matches('"');
                if !m.is_empty() && !EXCLUDED_MODES.contains(&m) {
                    modes.push(m.to_string());
                }
            }
        }
    }
    CliInfo { modes, has_agent: flat.contains("--agent <agent>") }
}

pub fn probe(program: String) -> Receiver<CliInfo> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let out = Command::new(&program).arg("--help").stdin(Stdio::null()).stderr(Stdio::null()).output();
        let info = out.map(|o| parse_help(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default();
        let _ = tx.send(info);
    });
    rx
}

/// Agent names from `*.md` files: the `name:` of the front matter, else the file name.
pub fn list_agents(dirs: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                if let Some(n) = agent_name(&p) {
                    names.push(n);
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn agent_name(p: &Path) -> Option<String> {
    let text = std::fs::read_to_string(p).ok()?;
    let mut lines = text.lines();
    if lines.next().map(str::trim) == Some("---") {
        for l in lines {
            if l.trim() == "---" {
                break;
            }
            if let Some(v) = l.strip_prefix("name:") {
                let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    Some(p.file_stem()?.to_string_lossy().into_owned())
}

pub fn agent_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h).join(".claude/agents"));
    }
    v.push(root.join(".claude/agents"));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From `claude --help`, Claude Code 2.1.310 (2026-10-25).
    const HELP: &str = r#"
  --agent <agent>                       Agent for the current session. Overrides
                                        the 'agent' setting.
  --permission-mode <mode>              Permission mode to use for the session
                                        (choices: "acceptEdits", "auto",
                                        "bypassPermissions", "manual",
                                        "dontAsk", "plan")
  --permission-prompts <target>         Who answers permission prompts with
                                        (choices: "host", "none", default:
"#;

    #[test]
    fn modes_come_from_the_help_without_bypass() {
        let i = parse_help(HELP);
        assert_eq!(i.modes, ["acceptEdits", "auto", "manual", "dontAsk", "plan"]);
        assert!(i.has_agent);
        assert_eq!(parse_help("usage: claude"), CliInfo::default());
    }

    #[test]
    fn agents_from_front_matter_or_file_name() {
        let d = std::env::temp_dir().join(format!("catlas-agents-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("aa-qa.md"), "---\nname: aa-qa\ndescription: x\n---\nbody").unwrap();
        std::fs::write(d.join("other.md"), "no front matter").unwrap();
        std::fs::write(d.join("notes.txt"), "skip").unwrap();
        std::fs::write(d.join("x.md"), "---\nname: \"reviewer\"\n---\n").unwrap();
        assert_eq!(list_agents(&[d.clone(), d.join("missing")]), ["aa-qa", "other", "reviewer"]);
        std::fs::remove_dir_all(d).unwrap();
    }
}
