//! The settings schema (decision 0016): every key `settings.toml` knows, its type, default and
//! help text, as data. The Control Center's settings screen lists these; `check` validates a
//! file against them (the screen shows the problems, loading still takes what it can, field by
//! field, see `config::Settings::from_text`). A new setting = a row here + a field in
//! `config::Settings` + its `apply` arm in `control::mod`.

use crate::config::Settings;
use crate::control::theme::Theme;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Bool,
    /// One of these strings.
    Choice(fn() -> Vec<&'static str>),
}

#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub help: &'static str,
}

/// The registered theme keys (`control::theme::PALETTES`).
pub fn themes() -> Vec<&'static str> {
    Theme::all().map(|t| t.key()).collect()
}

pub const FIELDS: &[Field] = &[
    Field { key: "theme", label: "Theme", kind: Kind::Choice(themes), help: "Ledger Terminal dark (default), light, high contrast, …" },
    Field { key: "vim", label: "Vim mode", kind: Kind::Bool, help: "start in Vim mode; Ctrl+Alt+V switches for one run" },
    Field { key: "hints", label: "Shortcut helper", kind: Kind::Bool, help: "keys for what has focus, bottom right (Alt+/)" },
    Field { key: "autocomplete", label: "Autocomplete", kind: Kind::Bool, help: "completion opens by itself while typing" },
];

pub fn field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

/// A field's value in `s`, as it is written in the file.
pub fn value(s: &Settings, key: &str) -> toml::Value {
    use toml::Value as V;
    match key {
        "theme" => V::String(s.theme.key().into()),
        "vim" => V::Boolean(s.vim),
        "hints" => V::Boolean(s.hints),
        "autocomplete" => V::Boolean(s.autocomplete),
        _ => V::Boolean(false),
    }
}

/// The value after `v` for a field: a bool flips, a choice goes to the next (or previous).
pub fn cycle(f: &Field, v: &toml::Value, back: bool) -> toml::Value {
    match f.kind {
        Kind::Bool => toml::Value::Boolean(!v.as_bool().unwrap_or(false)),
        Kind::Choice(opts) => {
            let opts = opts();
            let i = opts.iter().position(|o| Some(*o) == v.as_str()).unwrap_or(0);
            let n = opts.len();
            let j = if back { (i + n - 1) % n } else { (i + 1) % n };
            toml::Value::String(opts[j].into())
        }
    }
}

/// `s` with `key` set to `v` (unknown keys or wrong types leave it as it was).
pub fn with(mut s: Settings, key: &str, v: &toml::Value) -> Settings {
    match (key, v) {
        ("theme", toml::Value::String(t)) => s.theme = Theme::from_key(t).unwrap_or(s.theme),
        ("vim", toml::Value::Boolean(b)) => s.vim = *b,
        ("hints", toml::Value::Boolean(b)) => s.hints = *b,
        ("autocomplete", toml::Value::Boolean(b)) => s.autocomplete = *b,
        _ => {}
    }
    s
}

/// The value as the screen shows it.
pub fn shown(v: &toml::Value) -> String {
    match v {
        toml::Value::Boolean(true) => "on".into(),
        toml::Value::Boolean(false) => "off".into(),
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Problems with a settings file's text: syntax errors (with the line), wrong types, unknown
/// values and unknown keys. Empty = fine.
pub fn check(text: &str) -> Vec<String> {
    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            let line = e.span().map(|s| text[..s.start.min(text.len())].lines().count().max(1));
            let msg = e.message().to_string();
            return vec![match line {
                Some(l) => format!("line {l}: {msg} (the whole file is ignored; defaults apply)"),
                None => format!("{msg} (the whole file is ignored; defaults apply)"),
            }];
        }
    };
    let mut out = Vec::new();
    for (k, v) in &table {
        let Some(f) = field(k) else {
            out.push(format!("unknown key `{k}` (ignored)"));
            continue;
        };
        match (f.kind, v) {
            (Kind::Bool, toml::Value::Boolean(_)) => {}
            (Kind::Bool, other) => out.push(format!("`{k}` = {other}: want true or false (default used)")),
            (Kind::Choice(opts), toml::Value::String(s)) if opts().contains(&s.as_str()) => {}
            (Kind::Choice(opts), other) => out.push(format!("`{k}` = {other}: want one of {} (default used)", opts().join(", "))),
        }
    }
    out
}

/// A commented settings file with every key at its default (written when the user opens
/// settings.toml and there is none yet).
pub fn template() -> String {
    let d = Settings::default();
    let mut s = String::from("# Code Atlas settings (decision 0016). The Control Center (Alt+O, then 2) changes these\n# live; edits saved here apply at once too.\n\n");
    for f in FIELDS {
        let opts = match f.kind {
            Kind::Bool => "true | false".to_string(),
            Kind::Choice(o) => o().join(" | "),
        };
        s.push_str(&format!("# {}: {} ({opts})\n{} = {}\n\n", f.label, f.help, f.key, value(&d, f.key)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_round_trips_through_settings() {
        for f in FIELDS {
            let d = Settings::default();
            let v = value(&d, f.key);
            let next = cycle(f, &v, false);
            assert_ne!(next, v, "{}", f.key);
            let s = with(d.clone(), f.key, &next);
            assert_eq!(value(&s, f.key), next, "{}", f.key);
            assert_eq!(cycle(f, &next, true), v, "{} back", f.key);
            // And the loader reads what the writer writes.
            let text = crate::config::set_key_in("", f.key, &next);
            assert_eq!(Settings::from_text(&text), s, "{}", f.key);
        }
        assert_eq!(themes()[0], "ledger-dark");
    }

    #[test]
    fn check_reports_what_is_wrong() {
        assert!(check("").is_empty());
        assert!(check(&template()).is_empty(), "{:?}", check(&template()));
        let p = check("hints = true\ntheme = \"neon\"\nvim = 1\ncolour = 2\n");
        assert_eq!(p.len(), 3, "{p:?}");
        assert!(p.iter().any(|m| m.contains("`theme`") && m.contains("ledger-light")));
        assert!(p.iter().any(|m| m.contains("`vim` = 1")));
        assert!(p.iter().any(|m| m.contains("unknown key `colour`")));
        let p = check("hints = true\nvim = [\n");
        assert!(p[0].starts_with("line 2") || p[0].starts_with("line 3"), "{p:?}");
        assert_eq!(Settings::from_text(&template()), Settings::default());
    }
}
