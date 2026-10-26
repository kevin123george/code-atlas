//! macOS keys that terminals mangle (decision 0010), used by `App::handle` for editor keys:
//! - **Cmd chords** reach a terminal program only with the kitty keyboard protocol and when the
//!   terminal doesn't use the chord itself (Ghostty binds Cmd+Z to its own undo; `keybind =
//!   super+z=unbind` passes it on). They then arrive with `SUPER` and act like their Ctrl twins.
//! - **Option+Z** types `Ω` (and Option+Shift+Z `¸`, US layout) unless the terminal treats Option
//!   as Meta, so Alt+Z (redo) never arrives in a default Terminal.app or iTerm2. Those characters
//!   redo only right after an undo or redo; anywhere else they are typed.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Cmd+Z / Cmd+Shift+Z undo / redo, Cmd+S save, Cmd+A select all, Cmd+C / Cmd+X copy / cut: the
/// key as its Ctrl twin. Cmd+V stays the terminal's paste (a bracketed paste of the system
/// clipboard), so it isn't mapped; other Cmd keys pass unchanged (and do nothing).
pub fn command_as_ctrl(k: KeyEvent) -> KeyEvent {
    let m = k.modifiers;
    let plain_cmd = m.contains(KeyModifiers::SUPER) && !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match k.code {
        KeyCode::Char(c) if plain_cmd && matches!(c.to_ascii_lowercase(), 'z' | 's' | 'a' | 'c' | 'x') => {
            let mut k = k;
            k.modifiers = (m - KeyModifiers::SUPER) | KeyModifiers::CONTROL;
            k
        }
        _ => k,
    }
}

/// Whether `k` is what Option+Z or Option+Shift+Z types without "Option as Meta" (US layout:
/// `Ω`, `¸`). The caller treats it as redo only right after an undo/redo with something to redo.
pub fn option_redo(k: &KeyEvent) -> bool {
    matches!(k.code, KeyCode::Char('Ω' | '¸')) && !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crossterm::event::Event;
    use KeyModifiers as M;

    fn k(c: char, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), m)
    }

    #[test]
    fn cmd_chords_become_ctrl_chords() {
        assert_eq!(command_as_ctrl(k('z', M::SUPER)).modifiers, M::CONTROL);
        assert_eq!(command_as_ctrl(k('Z', M::SUPER | M::SHIFT)).modifiers, M::CONTROL | M::SHIFT);
        assert_eq!(command_as_ctrl(k('s', M::SUPER)), k('s', M::CONTROL));
        assert_eq!(command_as_ctrl(k('v', M::SUPER)), k('v', M::SUPER), "Cmd+V is the terminal's paste");
        assert_eq!(command_as_ctrl(k('q', M::SUPER)), k('q', M::SUPER), "Cmd+Q is not Ctrl+Q (quit)");
        assert_eq!(command_as_ctrl(k('z', M::SUPER | M::ALT)), k('z', M::SUPER | M::ALT));
        assert_eq!(command_as_ctrl(k('z', M::NONE)), k('z', M::NONE));
    }

    #[test]
    fn option_characters() {
        assert!(option_redo(&k('Ω', M::NONE)));
        assert!(option_redo(&k('¸', M::SHIFT)));
        assert!(!option_redo(&k('Ω', M::ALT)));
        assert!(!option_redo(&k('z', M::NONE)));
    }

    fn app(text: &str) -> App {
        let d = std::env::temp_dir().join(format!("catlas-mackeys-{}-{}", std::process::id(), text.len()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("CATLAS_CONFIG_DIR", d.join(".cfg"));
        std::fs::write(d.join("a.txt"), text).unwrap();
        let mut app = App::new(vec![d.clone(), d.join("a.txt")]).unwrap();
        app.active = 0;
        app
    }

    fn press(app: &mut App, k: KeyEvent) {
        app.handle(Event::Key(k));
    }

    fn typ(app: &mut App, s: &str) {
        s.chars().for_each(|c| press(app, k(c, M::NONE)));
    }

    fn text(app: &App) -> String {
        app.docs[0].rope.to_string()
    }

    #[test]
    fn undo_redo_through_the_key_map() {
        let mut a = app("x\n");
        typ(&mut a, "ab");
        press(&mut a, k('z', M::SUPER)); // Cmd+Z (kitty protocol)
        assert_eq!(text(&a), "x\n");
        press(&mut a, k('Z', M::SUPER | M::SHIFT)); // Cmd+Shift+Z
        assert_eq!(text(&a), "abx\n");
        press(&mut a, k('z', M::CONTROL));
        press(&mut a, k('Z', M::ALT | M::SHIFT)); // Alt+Shift+Z
        assert_eq!(text(&a), "abx\n");
        press(&mut a, k('z', M::CONTROL));
        press(&mut a, k('z', M::ALT)); // Alt+Z
        assert_eq!(text(&a), "abx\n");
        press(&mut a, k('z', M::CONTROL));
        press(&mut a, k('z', M::CONTROL | M::SHIFT)); // Ctrl+Shift+Z
        assert_eq!(text(&a), "abx\n");
        press(&mut a, k('z', M::CONTROL));
        assert!(a.message.is_none());
        press(&mut a, k('z', M::CONTROL));
        assert_eq!(a.message.as_deref(), Some("Nothing to undo"));
    }

    #[test]
    fn omega_redoes_only_right_after_an_undo() {
        let mut a = app("x\n\n");
        typ(&mut a, "ab");
        press(&mut a, KeyEvent::new(KeyCode::Enter, M::NONE));
        typ(&mut a, "c");
        press(&mut a, k('z', M::CONTROL));
        press(&mut a, k('z', M::CONTROL));
        assert_eq!(text(&a), "abx\n\n");
        typ(&mut a, "Ω"); // Option+Z in Terminal.app
        assert_eq!(text(&a), "ab\nx\n\n");
        typ(&mut a, "Ω");
        assert_eq!(text(&a), "ab\ncx\n\n");
        typ(&mut a, "Ω"); // nothing left to redo: typed
        assert_eq!(text(&a), "ab\ncΩx\n\n");
        // After another key the character is just typed, redo steps or not.
        press(&mut a, k('z', M::CONTROL));
        press(&mut a, KeyEvent::new(KeyCode::Down, M::NONE));
        typ(&mut a, "Ω");
        assert_eq!(text(&a), "ab\ncx\nΩ\n");
        // Typing it with nothing undone is plain typing too.
        let mut b = app("y");
        typ(&mut b, "Ω¸");
        assert_eq!(text(&b), "Ω¸y");
    }

    #[test]
    fn cmd_s_saves_and_cmd_a_selects_all() {
        let mut a = app("hello");
        typ(&mut a, "1");
        press(&mut a, k('s', M::SUPER));
        assert!(!a.docs[0].dirty());
        press(&mut a, k('a', M::SUPER));
        assert_eq!(a.docs[0].selection(), Some((0, 6)));
    }
}
