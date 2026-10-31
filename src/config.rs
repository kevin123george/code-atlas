//! User settings: `settings.toml` in the config folder (`$CATLAS_CONFIG_DIR`, else
//! `$XDG_CONFIG_HOME/code-atlas`, else `~/.config/code-atlas`, the same folder as `languages.toml`
//! and `agents.json`). Minimal on purpose (M7 brings the full settings file):
//!
//! ```toml
//! hints = true          # the shortcut helper in the bottom-right corner (Alt+/ toggles and saves it)
//! autocomplete = true   # completion opens by itself while typing (Ctrl+Space / Ctrl+J always work)
//! large_file_mb = 20    # bigger files (or ones with 20 000+ char lines) open without highlighting / LSP
//!
//! [index]               # the persistent code index (decision 0014)
//! enabled = true        # off: Go to File / Find in Files walk the disk as before
//! max_file_kb = 4096    # bigger files are indexed by name only
//! max_files = 1000000   # per root
//! symbols = true        # tree-sitter symbols (Search Everywhere SYMBOLS, Alt+N without a server)
//! watch = true          # follow file changes (FSEvents / inotify)
//! memory_mb = 256       # memory for building segments
//! ```
//!
//! A missing, unreadable or broken file gives the defaults. Saving keeps the other keys in the file.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub hints: bool,
    pub autocomplete: bool,
    pub large_file_mb: u64,
    pub index: IndexSettings,
}

/// `[index]` in `settings.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexSettings {
    pub enabled: bool,
    pub max_file_kb: u64,
    pub max_files: usize,
    pub symbols: bool,
    pub watch: bool,
    pub memory_mb: usize,
}

impl Default for IndexSettings {
    fn default() -> Self {
        IndexSettings { enabled: true, max_file_kb: 4096, max_files: 1_000_000, symbols: true, watch: true, memory_mb: 256 }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings { hints: true, autocomplete: true, large_file_mb: 20, index: IndexSettings::default() }
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
        if let Some(b) = table.get("autocomplete").and_then(|v| v.as_bool()) {
            s.autocomplete = b;
        }
        let num = |t: &toml::Table, k: &str| t.get(k).and_then(|v| v.as_integer()).filter(|n| *n > 0).map(|n| n as u64);
        if let Some(n) = num(&table, "large_file_mb") {
            s.large_file_mb = n;
        }
        if let Some(t) = table.get("index").and_then(|v| v.as_table()) {
            let i = &mut s.index;
            let flag = |k: &str| t.get(k).and_then(|v| v.as_bool());
            i.enabled = flag("enabled").unwrap_or(i.enabled);
            i.symbols = flag("symbols").unwrap_or(i.symbols);
            i.watch = flag("watch").unwrap_or(i.watch);
            i.max_file_kb = num(t, "max_file_kb").unwrap_or(i.max_file_kb);
            i.max_files = num(t, "max_files").map_or(i.max_files, |n| n as usize);
            i.memory_mb = num(t, "memory_mb").map_or(i.memory_mb, |n| (n as usize).max(16));
        }
        s
    }

    /// This run's settings (read once; the defaults in unit tests).
    pub fn get() -> &'static Settings {
        static S: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();
        S.get_or_init(|| Settings::load(if cfg!(test) { None } else { settings_path() }.as_deref()))
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
        assert_eq!(Settings::load(Some(&p)), Settings::default());
        assert_eq!(Settings::load(None), Settings::default());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&p, "hints = [not toml").unwrap();
        assert!(Settings::load(Some(&p)).hints, "a broken file is ignored");
        std::fs::write(&p, "hints = \"no\"\n").unwrap();
        assert!(Settings::load(Some(&p)).hints, "a wrong type is ignored");
        std::fs::write(&p, "autocomplete = false\n").unwrap();
        assert_eq!(Settings::load(Some(&p)), Settings { autocomplete: false, ..Settings::default() }, "field by field");
        std::fs::write(&p, "large_file_mb = 5\n[index]\nenabled = false\nmax_file_kb = 64\nwatch = \"no\"\n").unwrap();
        let s = Settings::load(Some(&p));
        assert_eq!(s.large_file_mb, 5);
        assert_eq!(s.index, IndexSettings { enabled: false, max_file_kb: 64, ..IndexSettings::default() }, "a wrong type keeps that default");
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
