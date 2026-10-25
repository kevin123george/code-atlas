//! Which language server runs for which language, and which files are which language.
//!
//! Three layers, later ones win field by field: the built-in `languages.toml` (compiled in),
//! `~/.config/code-atlas/languages.toml` (or `$CATLAS_CONFIG_DIR` / `$XDG_CONFIG_HOME/code-atlas`),
//! and `<project>/.code-atlas/languages.toml`. `initialization-options` and `settings` tables are
//! merged deeply, every other field is replaced. Format: `docs/languages.example.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

const BUILTIN: &str = include_str!("languages.toml");

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct LangFile {
    extensions: Option<Vec<String>>,
    roots: Option<Vec<String>>,
    servers: Option<Vec<String>>,
    also: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ServerFile {
    command: Option<String>,
    args: Option<Vec<String>>,
    initialization_options: Option<toml::Value>,
    settings: Option<toml::Value>,
    version_args: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct File {
    #[serde(default)]
    language: BTreeMap<String, LangFile>,
    #[serde(default)]
    language_server: BTreeMap<String, ServerFile>,
}

/// One language after merging.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LangCfg {
    pub extensions: Vec<String>,
    /// Layer that last set `extensions` (0 built-in, 1 user, 2 project): a later one wins a clash.
    ext_layer: usize,
    pub roots: Vec<String>,
    pub servers: Vec<String>,
    pub also: Vec<String>,
}

/// One language server after merging.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerCfg {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub init_options: Value,
    pub settings: Value,
    pub version_args: Vec<String>,
}

impl ServerCfg {
    /// The answer to one `workspace/configuration` item: `settings` at the dotted `section`.
    pub fn section(&self, section: Option<&str>) -> Value {
        let mut v = &self.settings;
        for part in section.unwrap_or("").split('.').filter(|p| !p.is_empty()) {
            match v.get(part) {
                Some(x) => v = x,
                None => return Value::Null,
            }
        }
        v.clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SourceState {
    Loaded,
    Missing,
    Error(String),
}

#[derive(Clone, Debug)]
pub struct Source {
    pub path: PathBuf,
    pub state: SourceState,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub langs: BTreeMap<String, LangCfg>,
    pub servers: BTreeMap<String, ServerCfg>,
    /// The user and project files that were looked at, for `--health`.
    pub sources: Vec<Source>,
}

/// The servers chosen for a language.
#[derive(Debug, Default)]
pub struct Selection {
    pub main: Option<(ServerCfg, PathBuf)>,
    pub also: Vec<(ServerCfg, PathBuf)>,
    /// Commands of the main candidates, in order (for "not found" messages).
    pub tried: Vec<String>,
}

/// `$CATLAS_CONFIG_DIR`, else `$XDG_CONFIG_HOME/code-atlas`, else `~/.config/code-atlas`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CATLAS_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("code-atlas"));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/code-atlas"))
}

fn to_json(v: toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// Merges `b` into `a`: objects key by key (recursively), anything else replaced.
pub fn merge_json(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(x) => merge_json(x, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (a, b) => *a = b,
    }
}

/// Replaces `${name}` in every string of `v` (`initialization-options`, `settings`).
pub fn expand(v: &Value, vars: &[(&str, String)]) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for (k, val) in vars {
                s = s.replace(&format!("${{{k}}}"), val);
            }
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| expand(x, vars)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), expand(x, vars))).collect()),
        other => other.clone(),
    }
}

/// Variables for [`expand`]: `${root}` (project root), `${home}`, and `${server-node-modules}`
/// (the `node_modules` folder the server's own package sits in, e.g. to find a global TypeScript).
pub fn vars(exe: &Path, root: &Path) -> Vec<(&'static str, String)> {
    let real = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let nm = real.ancestors().find(|a| a.file_name().is_some_and(|n| n == "node_modules")).map(|p| p.display().to_string()).unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    vec![("root", root.display().to_string()), ("home", home), ("server-node-modules", nm)]
}

/// `*` matches any run of characters; case-insensitive.
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.to_lowercase(), name.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == n;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !n.starts_with(first) || !n[first.len()..].ends_with(last) || n.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &n[first.len()..n.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

impl Config {
    pub fn builtin() -> Config {
        let mut c = Config::default();
        c.merge_str(BUILTIN, 0).expect("built-in languages.toml parses");
        c
    }

    /// Built-ins, then the user file, then the project's `.code-atlas/languages.toml`.
    pub fn load(project: Option<&Path>) -> Config {
        let mut c = Config::builtin();
        let files = [config_dir().map(|d| d.join("languages.toml")), project.map(|p| p.join(".code-atlas/languages.toml"))];
        for (layer, path) in files.into_iter().enumerate() {
            let Some(path) = path else { continue };
            let state = match std::fs::read_to_string(&path) {
                Err(_) => SourceState::Missing,
                Ok(text) => match c.merge_str(&text, layer + 1) {
                    Ok(()) => SourceState::Loaded,
                    Err(e) => SourceState::Error(e),
                },
            };
            c.sources.push(Source { path, state });
        }
        c
    }

    /// Merges one file's text on top. On a parse error nothing changes.
    pub fn merge_str(&mut self, text: &str, layer: usize) -> Result<(), String> {
        let file: File = toml::from_str(text).map_err(|e| {
            // "TOML parse error at line 2, column 11" + the explanation (its last line).
            let t = e.to_string();
            let mut lines = t.lines().map(str::trim).filter(|l| !l.is_empty());
            let first = lines.next().unwrap_or("bad TOML").to_string();
            match lines.last() {
                Some(last) => format!("{first}: {last}"),
                None => first,
            }
        })?;
        for (name, l) in file.language {
            let cur = self.langs.entry(name).or_default();
            if let Some(v) = l.extensions {
                cur.extensions = v;
                cur.ext_layer = layer;
            }
            if let Some(v) = l.roots {
                cur.roots = v;
            }
            if let Some(v) = l.servers {
                cur.servers = v;
            }
            if let Some(v) = l.also {
                cur.also = v;
            }
        }
        for (name, s) in file.language_server {
            let cur = self.servers.entry(name.clone()).or_insert_with(|| ServerCfg {
                name: name.clone(),
                command: name.clone(),
                version_args: vec!["--version".into()],
                ..Default::default()
            });
            if let Some(v) = s.command {
                cur.command = v;
            }
            if let Some(v) = s.args {
                cur.args = v;
            }
            if let Some(v) = s.initialization_options {
                merge_json(&mut cur.init_options, to_json(v));
            }
            if let Some(v) = s.settings {
                merge_json(&mut cur.settings, to_json(v));
            }
            if let Some(v) = s.version_args {
                cur.version_args = v;
            }
        }
        Ok(())
    }

    /// The language a file belongs to by name: whole file name beats a glob beats an extension;
    /// a later layer beats an earlier one.
    pub fn lang_for(&self, path: &Path) -> Option<&str> {
        let name = path.file_name()?.to_string_lossy().to_string();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
        let mut best: Option<((usize, u8), &str)> = None;
        for (lang, l) in &self.langs {
            for pat in &l.extensions {
                let kind = if pat.contains('*') {
                    glob(pat, &name).then_some(1)
                } else if pat.eq_ignore_ascii_case(&name) {
                    Some(2)
                } else if ext.as_deref() == Some(pat.to_lowercase().as_str()) {
                    Some(0)
                } else {
                    None
                };
                if let Some(k) = kind {
                    let score = (l.ext_layer, k);
                    if best.is_none_or(|(b, _)| score > b) {
                        best = Some((score, lang));
                    }
                }
            }
        }
        best.map(|(_, l)| l)
    }

    /// Picks the servers for `lang`: the first `servers` entry `which` finds (it may also skip
    /// servers that failed before), plus every `also` entry that is found (and isn't the main one).
    pub fn select(&self, lang: &str, which: impl Fn(&ServerCfg) -> Option<PathBuf>) -> Selection {
        let mut sel = Selection::default();
        let Some(l) = self.langs.get(lang) else { return sel };
        for name in &l.servers {
            let Some(s) = self.servers.get(name) else { continue };
            sel.tried.push(s.command.clone());
            if let Some(exe) = which(s) {
                sel.main = Some((s.clone(), exe));
                break;
            }
        }
        for name in &l.also {
            let Some(s) = self.servers.get(name) else { continue };
            if sel.main.as_ref().is_some_and(|(m, _)| m.name == s.name) || sel.also.iter().any(|(a, _)| a.name == s.name) {
                continue;
            }
            if let Some(exe) = which(s) {
                sel.also.push((s.clone(), exe));
            }
        }
        sel
    }

    pub fn has_servers(&self, lang: &str) -> bool {
        self.langs.get(lang).is_some_and(|l| !l.servers.is_empty() || !l.also.is_empty())
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Loads the configuration for this run (call once, before any file is opened).
pub fn init(project: Option<&Path>) {
    let _ = CONFIG.set(Config::load(project));
}

/// The configuration of this run (the built-ins if [`init`] wasn't called, as in unit tests).
pub fn get() -> &'static Config {
    CONFIG.get_or_init(Config::builtin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtin_parses_and_every_language_is_known() {
        let c = Config::builtin();
        for (name, l) in &c.langs {
            assert!(crate::syntax::Lang::from_key(name).is_some(), "{name} is no Lang");
            for s in l.servers.iter().chain(&l.also) {
                assert!(c.servers.contains_key(s), "{name} names undefined server {s}");
            }
        }
        for l in crate::syntax::Lang::ALL {
            assert!(c.langs.contains_key(l.key()), "{l:?} missing from languages.toml");
        }
        assert_eq!(c.servers["gopls"].version_args, ["version"]);
        assert_eq!(c.servers["rust-analyzer"].version_args, ["--version"], "default version args");
        assert_eq!(c.servers["typescript-language-server"].init_options["preferences"]["completeFunctionCalls"], json!(true));
    }

    #[test]
    fn files_map_to_languages() {
        let c = Config::builtin();
        let l = |p: &str| c.lang_for(Path::new(p));
        assert_eq!(l("src/main.rs"), Some("rust"));
        assert_eq!(l("x/Dockerfile"), Some("dockerfile"));
        assert_eq!(l("Dockerfile.prod"), Some("dockerfile"));
        assert_eq!(l("api.Dockerfile"), Some("dockerfile"));
        assert_eq!(l("Cargo.lock"), Some("toml"));
        assert_eq!(l(".bashrc"), Some("bash"));
        assert_eq!(l("a.H"), Some("c"));
        assert_eq!(l("b.hpp"), Some("cpp"));
        assert_eq!(l("k8s.yml"), Some("yaml"));
        assert_eq!(l("q.sql"), Some("sql"));
        assert_eq!(l("notes.txt"), None);
        assert_eq!(l("Makefile"), None);
    }

    #[test]
    fn later_layers_win_field_by_field() {
        let mut c = Config::builtin();
        c.merge_str(
            r#"
            [language.python]
            servers = ["basedpyright"]

            [language-server.basedpyright]
            args = ["--stdio", "--verbose"]
            settings = { basedpyright = { analysis = { typeCheckingMode = "strict" } } }

            [language.cpp]
            extensions = ["h", "cpp"]

            [language-server.my-ls]
            command = "/opt/my-ls"
            initialization-options = { a = 1 }
            "#,
            1,
        )
        .unwrap();
        let py = &c.langs["python"];
        assert_eq!(py.servers, ["basedpyright"]);
        assert_eq!(py.also, ["ruff"], "untouched fields stay");
        assert_eq!(py.extensions, ["py", "pyi", "pyw"]);
        let b = &c.servers["basedpyright"];
        assert_eq!(b.command, "basedpyright-langserver");
        assert_eq!(b.args, ["--stdio", "--verbose"]);
        // Settings merge deeply: the built-in diagnosticMode stays next to the new key.
        assert_eq!(b.settings, json!({ "basedpyright": { "analysis": { "diagnosticMode": "openFilesOnly", "typeCheckingMode": "strict" } } }));
        assert_eq!(b.section(Some("basedpyright.analysis"))["typeCheckingMode"], json!("strict"));
        assert_eq!(b.section(Some("nope")), Value::Null);
        // `.h` is claimed by C (built-in) and C++ (user): the later layer wins.
        assert_eq!(c.lang_for(Path::new("x.h")), Some("cpp"));
        assert_eq!(c.lang_for(Path::new("x.c")), Some("c"));
        assert_eq!(c.servers["my-ls"].init_options, json!({ "a": 1 }));
        assert_eq!(c.servers["my-ls"].version_args, ["--version"]);
    }

    #[test]
    fn bad_files_are_reported_and_change_nothing() {
        let mut c = Config::builtin();
        let before = c.langs["go"].clone();
        let e = c.merge_str("[language.go]\nservers = \"gopls\"\n", 1).unwrap_err();
        assert!(e.contains("invalid type") || e.contains("expected"), "{e}");
        assert!(c.merge_str("[language.go]\nsurvers = []\n", 1).unwrap_err().contains("unknown field"));
        assert_eq!(c.langs["go"], before);
    }

    #[test]
    fn loads_user_and_project_files() {
        let tmp = std::env::temp_dir().join(format!("catlas-cfg-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("proj/.code-atlas")).unwrap();
        std::fs::write(tmp.join("proj/.code-atlas/languages.toml"), "[language.go]\nroots = [\"go.mod\"]\n").unwrap();
        let mut c = Config::builtin();
        // Same steps as `load`, with explicit paths (the env var is shared by parallel tests).
        for (layer, p) in [tmp.join("missing.toml"), tmp.join("proj/.code-atlas/languages.toml")].iter().enumerate() {
            if let Ok(t) = std::fs::read_to_string(p) {
                c.merge_str(&t, layer + 1).unwrap();
            }
        }
        assert_eq!(c.langs["go"].roots, ["go.mod"]);
        let loaded = Config::load(Some(&tmp.join("proj")));
        assert_eq!(loaded.sources.last().unwrap().state, SourceState::Loaded);
        assert_eq!(loaded.langs["go"].roots, ["go.mod"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn selection_takes_the_first_found_server_plus_extras() {
        let c = Config::builtin();
        let only = |found: &'static [&'static str]| move |s: &ServerCfg| found.contains(&s.command.as_str()).then(|| PathBuf::from(format!("/bin/{}", s.command)));
        let s = c.select("python", only(&["basedpyright-langserver", "pylsp", "ruff"]));
        assert_eq!(s.main.as_ref().unwrap().0.name, "basedpyright");
        assert_eq!(s.main.as_ref().unwrap().1, PathBuf::from("/bin/basedpyright-langserver"));
        assert_eq!(s.also.iter().map(|a| a.0.name.as_str()).collect::<Vec<_>>(), ["ruff"]);
        assert_eq!(s.tried, ["pyright-langserver", "basedpyright-langserver"]);
        let none = c.select("python", only(&[]));
        assert!(none.main.is_none() && none.also.is_empty());
        assert_eq!(none.tried.len(), 3);
        assert!(c.select("sql", only(&["sqls"])).main.is_none());
        assert!(!c.has_servers("sql"));
        assert!(c.has_servers("go"));
    }

    #[test]
    fn variables_expand_in_strings() {
        let v = json!({ "tsserver": { "fallbackPath": "${server-node-modules}/typescript/lib" }, "n": 1, "l": ["${root}/x"] });
        let e = expand(&v, &[("server-node-modules", "/nm".into()), ("root", "/p".into())]);
        assert_eq!(e, json!({ "tsserver": { "fallbackPath": "/nm/typescript/lib" }, "n": 1, "l": ["/p/x"] }));
        let tmp = std::env::temp_dir().join(format!("catlas-nm-{}", std::process::id()));
        let exe = tmp.join("lib/node_modules/ls/bin/ls");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        let v = vars(&exe, Path::new("/r"));
        let nm = &v.iter().find(|(k, _)| *k == "server-node-modules").unwrap().1;
        assert!(nm.ends_with("lib/node_modules"), "{nm}");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn the_shipped_example_parses() {
        let mut c = Config::builtin();
        c.merge_str(include_str!("../../docs/languages.example.toml"), 1).unwrap();
        assert_eq!(c.langs["python"].servers, ["basedpyright", "pyright"]);
        assert_eq!(c.lang_for(Path::new("x.h")), Some("cpp"));
        assert_eq!(c.lang_for(Path::new("app.k8s")), Some("yaml"));
        assert_eq!(c.servers["yaml-language-server"].section(Some("yaml.schemaStore.enable")), json!(false));
        assert_eq!(c.servers["yaml-language-server"].section(Some("yaml.validate")), json!(true), "built-in settings kept");
    }

    #[test]
    fn globs() {
        assert!(glob("Dockerfile.*", "dockerfile.dev"));
        assert!(glob("*.Dockerfile", "web.dockerfile"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXc"));
        assert!(!glob("ab*ba", "aba"));
    }
}
