//! `catlas --health [dir]`: per language, whether highlighting works and which language servers
//! are installed (path and version), like Helix's `hx --health`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::config::{Config, ServerCfg, SourceState};
use crate::syntax::Lang;

/// What was found for one server.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    pub version: Option<String>,
}

/// The report text. `probe` finds a server (path + version); tests pass a fake one.
pub fn report(cfg: &Config, probe: impl Fn(&ServerCfg) -> Option<Found> + Sync) -> String {
    let mut out = String::from("Code Atlas health\n\nConfig files (later ones override earlier ones):\n  built-in defaults\n");
    for s in &cfg.sources {
        let state = match &s.state {
            SourceState::Loaded => "loaded".to_string(),
            SourceState::Missing => "not there".to_string(),
            SourceState::Error(e) => format!("ERROR, ignored: {e}"),
        };
        out.push_str(&format!("  {} ({state})\n", s.path.display()));
    }
    // Every server any language names, probed once and in parallel (versions can take a second).
    let mut names: Vec<&str> = cfg.langs.values().flat_map(|l| l.servers.iter().chain(&l.also)).map(String::as_str).collect();
    names.sort();
    names.dedup();
    let found: Vec<(&str, Option<Found>)> = std::thread::scope(|sc| {
        let jobs: Vec<_> = names
            .iter()
            .map(|&n| (n, cfg.servers.get(n).map(|s| sc.spawn(|| probe(s)))))
            .collect();
        jobs.into_iter().map(|(n, h)| (n, h.and_then(|h| h.join().ok().flatten()))).collect()
    });
    let lookup = |n: &str| found.iter().find(|(m, _)| *m == n).and_then(|(_, f)| f.clone());

    out.push_str(&format!("\n{:<11} {:<10} {:<30} {}\n", "Language", "Highlight", "Server", "Found"));
    let (mut ok, mut total) = (0, 0);
    for (name, l) in &cfg.langs {
        let known = Lang::from_key(name);
        let hl = match known {
            Some(lang) if lang.highlights() => "yes",
            Some(_) => "BROKEN",
            None => "unknown",
        };
        let mut rows: Vec<(String, String)> = Vec::new();
        let mut main_done = false;
        for (role, list) in [("", &l.servers), ("+", &l.also)] {
            for s in list {
                let label = format!("{role}{s}");
                let status = match (cfg.servers.get(s), lookup(s)) {
                    (None, _) => "ERROR: no [language-server.".to_string() + s + "]",
                    (Some(c), None) => format!("not found ({})", c.command),
                    (Some(_), Some(f)) => {
                        let tag = if role.is_empty() && !main_done {
                            main_done = true;
                            "✓ used"
                        } else if role.is_empty() {
                            "✓ (fallback)"
                        } else {
                            "✓ also runs"
                        };
                        format!("{tag}  {}{}", f.path.display(), f.version.map(|v| format!("  {v}")).unwrap_or_default())
                    }
                };
                rows.push((label, status));
            }
        }
        total += 1;
        if main_done || l.servers.is_empty() {
            ok += 1;
        }
        if rows.is_empty() {
            rows.push(("-".into(), "no language server configured".into()));
        }
        for (i, (label, status)) in rows.into_iter().enumerate() {
            let (n, h) = if i == 0 { (name.as_str(), hl) } else { ("", "") };
            out.push_str(&format!("{n:<11} {h:<10} {label:<30} {status}\n"));
        }
    }
    out.push_str(&format!(
        "\n{ok} of {total} languages ready (a server found, or none needed). '+' = runs next to the main server.\n\
         Install hints: docs/languages.example.toml · format: AGENTS.md \"Language servers\".\n"
    ));
    out
}

/// Finds a server binary and its version: from the install path (brew Cellar, npm package.json)
/// or by running `command version-args` (3 s at most).
pub fn probe(s: &ServerCfg) -> Option<Found> {
    let path = super::server::which(&s.command)?;
    let version = run_version(&path, &s.version_args).or_else(|| version_from_path(&path));
    Some(Found { path, version })
}

fn run_version(exe: &Path, args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let mut child = Command::new(exe).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let t = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t.elapsed() < Duration::from_secs(3) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    if text.trim().is_empty() {
        child.stderr.take()?.read_to_string(&mut text).ok()?;
    }
    first_line(&text)
}

fn first_line(text: &str) -> Option<String> {
    let l = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(l.chars().take(70).collect())
}

/// `/opt/homebrew/Cellar/<name>/<version>/…` or the nearest `package.json` above the real file.
pub fn version_from_path(exe: &Path) -> Option<String> {
    let real = std::fs::canonicalize(exe).ok()?;
    let parts: Vec<String> = real.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if let Some(i) = parts.iter().position(|p| p == "Cellar") {
        if let (Some(name), Some(ver)) = (parts.get(i + 1), parts.get(i + 2)) {
            return Some(format!("{name} {ver} (brew)"));
        }
    }
    for dir in real.ancestors().skip(1).take(4) {
        let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else { continue };
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let (name, ver) = (v.get("name")?.as_str()?, v.get("version")?.as_str()?);
        return Some(format!("{name} {ver} (npm)"));
    }
    None
}

/// Prints the report for `dir` (its `.code-atlas/languages.toml` is read too).
pub fn run(dir: &Path) {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let cfg = Config::load(Some(&dir));
    print!("{}", report(&cfg, probe));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_lists_languages_servers_and_versions() {
        let mut cfg = Config::builtin();
        cfg.merge_str("[language.go]\nservers = [\"gopls\", \"nope\"]\n", 1).unwrap();
        let fake = |s: &ServerCfg| match s.name.as_str() {
            "gopls" => Some(Found { path: "/x/gopls".into(), version: Some("golang.org/x/tools/gopls v0.20.0".into()) }),
            "pyright" | "ruff" => Some(Found { path: format!("/x/{}", s.command).into(), version: None }),
            _ => None,
        };
        let r = report(&cfg, fake);
        let line = |start: &str| r.lines().find(|l| l.starts_with(start)).unwrap_or_else(|| panic!("no {start:?} in\n{r}")).to_string();
        let go = line("go ");
        assert!(go.contains("yes") && go.contains("✓ used") && go.contains("/x/gopls") && go.contains("v0.20.0"), "{go}");
        assert!(r.contains("ERROR: no [language-server.nope]"), "{r}");
        assert!(line("python").contains("✓ used  /x/pyright-langserver"));
        assert!(r.lines().any(|l| l.contains("+ruff") && l.contains("✓ also runs")), "{r}");
        assert!(line("rust").contains("not found (rust-analyzer)"));
        assert!(line("sql").contains("no language server configured"));
        // Every language with a grammar is listed, with working highlighting.
        for l in Lang::ALL {
            assert!(line(&format!("{} ", l.key())).contains(" yes "), "{l:?}");
        }
        assert!(r.contains("languages ready"));
    }

    #[test]
    fn versions_come_from_install_paths() {
        let tmp = std::env::temp_dir().join(format!("catlas-ver-{}", std::process::id()));
        let bin = tmp.join("lib/node_modules/some-ls/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(tmp.join("lib/node_modules/some-ls/package.json"), r#"{"name":"some-ls","version":"1.2.3"}"#).unwrap();
        std::fs::write(bin.join("some-ls"), "").unwrap();
        assert_eq!(version_from_path(&bin.join("some-ls")).as_deref(), Some("some-ls 1.2.3 (npm)"));
        let cellar = tmp.join("Cellar/gopls/0.20.0/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        std::fs::write(cellar.join("gopls"), "").unwrap();
        assert_eq!(version_from_path(&cellar.join("gopls")).as_deref(), Some("gopls 0.20.0 (brew)"));
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(first_line("\n  v1 \nmore"), Some("v1".into()));
        assert_eq!(run_version(Path::new("/bin/echo"), &["hi".into()]).as_deref(), Some("hi"));
    }
}
