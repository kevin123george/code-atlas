//! Telling you an agent finished or needs you (decision 0015): the status bar says it, and the
//! terminal gets a desktop notification (OSC 9: iTerm2, Ghostty, WezTerm, kitty) or, elsewhere
//! (Terminal.app, tmux), a bell. `CATLAS_NOTIFY=off` silences both, `=bell` forces the bell.

use std::io::Write;

/// How this terminal is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Off,
    Bell,
    Osc9,
}

/// From the environment: `CATLAS_NOTIFY`, `TMUX`, `TERM_PROGRAM`, `TERM`.
pub fn kind_for(setting: Option<&str>, tmux: bool, term_program: &str, term: &str) -> Kind {
    match setting.map(str::trim) {
        Some("off" | "0" | "false" | "no") => return Kind::Off,
        Some("bell") => return Kind::Bell,
        _ => {}
    }
    if tmux {
        // tmux would swallow OSC 9; its bell flag still marks the window.
        return Kind::Bell;
    }
    let osc9 = matches!(term_program, "iTerm.app" | "ghostty" | "WezTerm") || ["kitty", "ghostty", "wezterm"].iter().any(|t| term.contains(t));
    if osc9 {
        Kind::Osc9
    } else {
        Kind::Bell
    }
}

pub fn kind() -> Kind {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    kind_for(std::env::var("CATLAS_NOTIFY").ok().as_deref(), std::env::var_os("TMUX").is_some(), &var("TERM_PROGRAM"), &var("TERM"))
}

/// The bytes for `text` (control characters dropped: they would end the sequence).
pub fn sequence(kind: Kind, text: &str) -> String {
    match kind {
        Kind::Off => String::new(),
        Kind::Bell => "\x07".into(),
        Kind::Osc9 => {
            let clean: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
            format!("\x1b]9;{clean}\x07")
        }
    }
}

/// Sends the notification to the terminal (never in unit tests).
pub fn send(text: &str) {
    if cfg!(test) {
        return;
    }
    let s = sequence(kind(), text);
    if s.is_empty() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_terminal_gets_what() {
        assert_eq!(kind_for(None, false, "iTerm.app", "xterm-256color"), Kind::Osc9);
        assert_eq!(kind_for(None, false, "ghostty", "xterm-ghostty"), Kind::Osc9);
        assert_eq!(kind_for(None, false, "", "xterm-kitty"), Kind::Osc9);
        assert_eq!(kind_for(None, false, "Apple_Terminal", "xterm-256color"), Kind::Bell);
        assert_eq!(kind_for(None, true, "iTerm.app", "screen"), Kind::Bell, "inside tmux: a bell");
        assert_eq!(kind_for(Some("off"), false, "iTerm.app", ""), Kind::Off);
        assert_eq!(kind_for(Some("bell"), false, "iTerm.app", ""), Kind::Bell);
        assert_eq!(sequence(Kind::Osc9, "Agent #2 done\n\x1b"), "\x1b]9;Agent #2 done\x07");
        assert_eq!(sequence(Kind::Bell, "x"), "\x07");
        assert_eq!(sequence(Kind::Off, "x"), "");
    }
}
