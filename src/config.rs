//! User settings: `settings.toml` in the config folder (`$CATLAS_CONFIG_DIR`, else
//! `$XDG_CONFIG_HOME/code-atlas`, else `~/.config/code-atlas`, the same folder as `languages.toml`
//! and `agents.json`). The keys, their types and help texts are listed once in
//! `control::settings::FIELDS` (the Control Center's settings screen, decision 0016):
//!
//! ```toml
//! hints = true            # the shortcut helper in the bottom-right corner (Alt+/ toggles and saves it)
//! autocomplete = true     # completion opens by itself while typing (Ctrl+Space / Ctrl+J always work)
//! vim = false             # start in Vim mode (Ctrl+Alt+V switches for one run)
//! theme = "ledger-dark"   # ledger-dark | ledger-light | high-contrast
//! ```
//!
//! A missing, unreadable or broken file gives the defaults (field by field). Saving one key edits
//! just that line, so comments and the other keys stay as they are.

use std::path::{Path, PathBuf};

use crate::control::theme::Theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub hints: bool,
    pub autocomplete: bool,
    pub vim: bool,
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { hints: true, autocomplete: true, vim: false, theme: Theme::default() }
    }
}

/// Where the settings file lives (no file system access).
pub fn settings_path() -> Option<PathBuf> {
    crate::lsp::config::config_dir().map(|d| d.join("settings.toml"))
}

impl Settings {
    /// Reads `path`; anything wrong with it gives the defaults (field by field).
    pub fn load(path: Option<&Path>) -> Settings {
        let text = path.and_then(|p| std::fs::read_to_string(p).ok());
        Self::from_text(text.as_deref().unwrap_or(""))
    }

    /// The settings in `text` (a broken file or a wrong type gives that field's default).
    pub fn from_text(text: &str) -> Settings {
        let mut s = Settings::default();
        let Ok(table) = text.parse::<toml::Table>() else { return s };
        let flag = |k: &str| table.get(k).and_then(|v| v.as_bool());
        if let Some(b) = flag("hints") {
            s.hints = b;
        }
        if let Some(b) = flag("autocomplete") {
            s.autocomplete = b;
        }
        if let Some(b) = flag("vim") {
            s.vim = b;
        }
        if let Some(t) = table.get("theme").and_then(|v| v.as_str()).and_then(Theme::from_key) {
            s.theme = t;
        }
        s
    }

    /// Writes `hints` into `path`, keeping whatever else the file holds.
    pub fn save_hints(path: &Path, on: bool) -> std::io::Result<()> {
        save_key(path, "hints", toml::Value::Boolean(on))
    }
}

/// Sets one top-level key in the file at `path` (created if missing), keeping comments, the
/// order and every other line: only that key's line changes (see [`set_key_in`]).
pub fn save_key(path: &Path, key: &str, value: toml::Value) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut new = set_key_in(&text, key, &value);
    // A file that parsed before must parse after (a key written as a multi-line value, say):
    // then fall back to a plain rewrite, which keeps the keys but not the comments.
    if let (Ok(mut table), Err(_)) = (text.parse::<toml::Table>(), new.parse::<toml::Table>()) {
        table.insert(key.into(), value);
        new = toml::to_string(&table).map_err(std::io::Error::other)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, new)
}

/// `text` with `key = value`: the first top-level line for `key` gets the new value (its
/// indentation and trailing `# comment` kept, the comment in the same column where it fits);
/// a missing key is added after the last top-level key (before the first `[table]`).
pub fn set_key_in(text: &str, key: &str, value: &toml::Value) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let first_table = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    let quoted = format!("\"{key}\"");
    let is_key = |l: &str| {
        let t = l.trim_start();
        t.strip_prefix(key).or_else(|| t.strip_prefix(quoted.as_str())).is_some_and(|r| r.trim_start().starts_with('='))
    };
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match lines[..first_table].iter().position(|l| is_key(l)) {
        Some(i) => {
            let line = lines[i];
            let eq = line.find('=').unwrap_or(line.len());
            let rest = &line[eq + 1..];
            let head = format!("{} = {value}", line[..eq].trim_end());
            out[i] = match comment_start(rest) {
                Some(c) => {
                    let col = line[..eq + 1 + c].chars().count();
                    let comment = &rest[c..];
                    if head.chars().count() < col {
                        format!("{head:<col$}{comment}")
                    } else {
                        format!("{head} {comment}")
                    }
                }
                None => head,
            };
        }
        None => {
            let last_key = lines[..first_table].iter().rposition(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#')
            });
            let at = last_key.map_or(0, |i| i + 1);
            out.insert(at, format!("{key} = {value}"));
            if at == first_table && at + 1 < out.len() && !out[at + 1].trim().is_empty() {
                out.insert(at + 1, String::new()); // a blank line before the first table
            }
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Where a `#` comment starts in a value (not inside a string), as a byte offset.
fn comment_start(v: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut esc = false;
    for (i, c) in v.char_indices() {
        match quote {
            Some(q) => {
                if esc {
                    esc = false;
                } else if c == '\\' && q == '"' {
                    esc = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return Some(i),
            None => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("catlas-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn defaults_when_missing_or_broken() {
        let d = scratch("broken");
        let p = d.join("settings.toml");
        assert_eq!(Settings::load(Some(&p)), Settings::default());
        assert_eq!(Settings::load(None), Settings::default());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&p, "hints = [not toml").unwrap();
        assert!(Settings::load(Some(&p)).hints, "a broken file is ignored");
        std::fs::write(&p, "hints = \"no\"\n").unwrap();
        assert!(Settings::load(Some(&p)).hints, "a wrong type is ignored");
        std::fs::write(&p, "autocomplete = false\n").unwrap();
        assert_eq!(Settings::load(Some(&p)), Settings { autocomplete: false, ..Settings::default() }, "field by field");
        std::fs::write(&p, "vim = true\ntheme = \"ledger-light\"\n").unwrap();
        let s = Settings::load(Some(&p));
        assert!(s.vim && s.theme.key() == "ledger-light", "{s:?}");
        std::fs::write(&p, "theme = \"neon\"\n").unwrap();
        assert_eq!(Settings::load(Some(&p)).theme, Theme::default(), "an unknown theme gives the default");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn save_round_trips_and_keeps_other_keys() {
        let d = scratch("save");
        let p = d.join("sub/settings.toml");
        Settings::save_hints(&p, false).unwrap();
        assert!(!Settings::load(Some(&p)).hints);
        std::fs::write(&p, "theme = \"ledger\"\nhints = false\n").unwrap();
        Settings::save_hints(&p, true).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("theme = \"ledger\"") && text.contains("hints = true"), "{text}");
        assert!(Settings::load(Some(&p)).hints);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn one_key_changes_and_comments_stay() {
        let v = toml::Value::Boolean;
        let text = "# my settings\nhints = true      # the helper\n\n# typing\nautocomplete = false\n\n[later]\nhints = 1\n";
        let out = set_key_in(text, "hints", &v(false));
        assert_eq!(out, "# my settings\nhints = false     # the helper\n\n# typing\nautocomplete = false\n\n[later]\nhints = 1\n");
        // A new key goes after the last top-level key, before the first table.
        let out = set_key_in(text, "vim", &v(true));
        assert!(out.contains("autocomplete = false\nvim = true\n\n[later]"), "{out}");
        // A string with a # inside keeps its comment apart.
        let out = set_key_in("theme = \"a#b\" # note\n", "theme", &toml::Value::String("ledger-light".into()));
        assert_eq!(out, "theme = \"ledger-light\" # note\n");
        // An empty file, and a file with only a table.
        assert_eq!(set_key_in("", "vim", &v(true)), "vim = true\n");
        assert_eq!(set_key_in("[x]\na = 1\n", "vim", &v(true)), "vim = true\n\n[x]\na = 1\n");
        // A longer value pushes the comment along.
        assert_eq!(set_key_in("t = 1 # c\n", "t", &toml::Value::String("much-longer".into())), "t = \"much-longer\" # c\n");
        let t: toml::Table = set_key_in(text, "hints", &v(false)).parse().unwrap();
        assert_eq!(t.get("hints"), Some(&v(false)));
    }

    #[test]
    fn save_key_creates_the_file_and_keeps_comments() {
        let d = scratch("savekey");
        let p = d.join("settings.toml");
        save_key(&p, "theme", toml::Value::String("high-contrast".into())).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "theme = \"high-contrast\"\n");
        std::fs::write(&p, "# hello\nhints = false # off\n").unwrap();
        Settings::save_hints(&p, true).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# hello\nhints = true  # off\n");
        std::fs::remove_dir_all(d).unwrap();
    }
}
