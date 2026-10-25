//! User settings: `settings.toml` in the config folder (`$CATLAS_CONFIG_DIR`, else
//! `$XDG_CONFIG_HOME/code-atlas`, else `~/.config/code-atlas`, the same folder as `languages.toml`
//! and `agents.json`). Minimal on purpose (M7 brings the full settings file):
//!
//! ```toml
//! hints = true   # the shortcut helper in the bottom-right corner (Alt+/ toggles and saves it)
//! ```
//!
//! A missing, unreadable or broken file gives the defaults. Saving keeps the other keys in the file.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub hints: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { hints: true }
    }
}

/// Where the settings file lives (no file system access).
pub fn settings_path() -> Option<PathBuf> {
    crate::lsp::config::config_dir().map(|d| d.join("settings.toml"))
}

impl Settings {
    /// Reads `path`; anything wrong with it gives the defaults (field by field).
    pub fn load(path: Option<&Path>) -> Settings {
        let mut s = Settings::default();
        let Some(table) = path.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| t.parse::<toml::Table>().ok()) else {
            return s;
        };
        if let Some(b) = table.get("hints").and_then(|v| v.as_bool()) {
            s.hints = b;
        }
        s
    }

    /// Writes `hints` into `path`, keeping whatever else the file holds (comments are lost).
    pub fn save_hints(path: &Path, on: bool) -> std::io::Result<()> {
        let mut table = std::fs::read_to_string(path).ok().and_then(|t| t.parse::<toml::Table>().ok()).unwrap_or_default();
        table.insert("hints".into(), toml::Value::Boolean(on));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(&table).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
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
        assert_eq!(Settings::load(Some(&p)), Settings { hints: true });
        assert_eq!(Settings::load(None), Settings { hints: true });
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&p, "hints = [not toml").unwrap();
        assert!(Settings::load(Some(&p)).hints, "a broken file is ignored");
        std::fs::write(&p, "hints = \"no\"\n").unwrap();
        assert!(Settings::load(Some(&p)).hints, "a wrong type is ignored");
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
}
