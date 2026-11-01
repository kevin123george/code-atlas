//! Window keys → the same crossterm events a kitty-protocol terminal produces, so `App::handle`
//! and every key table stay as they are. Pure, so the mapping is unit tested without a window.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers as M};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    /// Alt as a chord key. On a Mac only the left Option key: the right one keeps typing `@ [ ] { }`
    /// on German layouts (see `window_event`).
    pub alt: bool,
    pub cmd: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Key(KeyEvent),
    Paste,
    Quit,
    Zoom(i8),
    Nothing,
}

fn mods(m: Mods) -> M {
    let mut out = M::NONE;
    if m.ctrl {
        out |= M::CONTROL;
    }
    if m.shift {
        out |= M::SHIFT;
    }
    if m.alt {
        out |= M::ALT;
    }
    out
}

fn single(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// `logical`: the key with Shift and the layout applied. `base`: the key without modifiers.
/// `shell`: the terminal pane has the focus, where a Cmd chord must never become Ctrl+C.
pub fn translate(logical: &Key, base: &Key, m: Mods, shell: bool) -> Action {
    if let Key::Named(n) = logical {
        return named(*n, m, shell);
    }
    let Key::Character(typed) = logical else { return Action::Nothing };
    let typed = single(typed);
    let base = match base {
        Key::Character(s) => single(s),
        _ => None,
    }
    .or(typed);
    let Some(base) = base.map(|c| c.to_lowercase().next().unwrap_or(c)) else { return Action::Nothing };

    if m.cmd {
        return match base {
            'v' => Action::Paste,
            'q' => Action::Quit,
            '=' | '+' => Action::Zoom(1),
            '-' => Action::Zoom(-1),
            '0' => Action::Zoom(0),
            _ if shell => Action::Nothing,
            // Every IDEA Ctrl chord also works with Cmd, as a Mac user's hands expect.
            _ => chord(base, typed, Mods { ctrl: true, cmd: false, ..m }),
        };
    }
    if m.ctrl || m.alt {
        return chord(base, typed, m);
    }
    let Some(c) = typed else { return Action::Nothing };
    let shift = if c.is_uppercase() { M::SHIFT } else { M::NONE };
    Action::Key(KeyEvent::new(KeyCode::Char(c), shift))
}

/// Lower-case letter + modifiers, the way crossterm reports the kitty protocol's `CSI 122;6u`. A
/// shifted symbol is the symbol itself, so Ctrl+Shift+7 on a German layout is Ctrl+/ (comment).
fn chord(base: char, typed: Option<char>, m: Mods) -> Action {
    if m.ctrl && m.shift && !m.alt {
        if let Some(t) = typed.filter(|t| t.is_ascii_punctuation()) {
            return Action::Key(KeyEvent::new(KeyCode::Char(t), mods(Mods { shift: false, ..m })));
        }
    }
    Action::Key(KeyEvent::new(KeyCode::Char(base), mods(m)))
}

fn named(n: NamedKey, m: Mods, shell: bool) -> Action {
    use NamedKey as N;
    if m.cmd {
        if shell {
            return Action::Nothing;
        }
        // The Mac's own line and file movement, on the editor's keys.
        let (code, ctrl) = match n {
            N::ArrowLeft => (KeyCode::Home, false),
            N::ArrowRight => (KeyCode::End, false),
            N::ArrowUp => (KeyCode::Home, true),
            N::ArrowDown => (KeyCode::End, true),
            N::Enter => (KeyCode::Enter, true),
            N::Backspace => (KeyCode::Backspace, true),
            N::Space => (KeyCode::Char(' '), true),
            _ => return Action::Nothing,
        };
        return Action::Key(KeyEvent::new(code, mods(Mods { ctrl, cmd: false, ..m })));
    }
    let code = match n {
        N::Enter => KeyCode::Enter,
        N::Tab if m.shift => KeyCode::BackTab,
        N::Tab => KeyCode::Tab,
        N::Space => KeyCode::Char(' '),
        N::Backspace => KeyCode::Backspace,
        N::Escape => KeyCode::Esc,
        N::Delete => KeyCode::Delete,
        N::Insert => KeyCode::Insert,
        N::Home => KeyCode::Home,
        N::End => KeyCode::End,
        N::PageUp => KeyCode::PageUp,
        N::PageDown => KeyCode::PageDown,
        N::ArrowLeft => KeyCode::Left,
        N::ArrowRight => KeyCode::Right,
        N::ArrowUp => KeyCode::Up,
        N::ArrowDown => KeyCode::Down,
        N::F1 => KeyCode::F(1),
        N::F2 => KeyCode::F(2),
        N::F3 => KeyCode::F(3),
        N::F4 => KeyCode::F(4),
        N::F5 => KeyCode::F(5),
        N::F6 => KeyCode::F(6),
        N::F7 => KeyCode::F(7),
        N::F8 => KeyCode::F(8),
        N::F9 => KeyCode::F(9),
        N::F10 => KeyCode::F(10),
        N::F11 => KeyCode::F(11),
        N::F12 => KeyCode::F(12),
        _ => return Action::Nothing,
    };
    Action::Key(KeyEvent::new(code, mods(m)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }
    fn press(logical: &str, base: &str, m: Mods) -> Action {
        translate(&ch(logical), &ch(base), m, false)
    }
    fn key(c: char, m: M) -> Action {
        Action::Key(KeyEvent::new(KeyCode::Char(c), m))
    }

    #[test]
    fn typing_keeps_the_layout() {
        assert_eq!(press("a", "a", Mods::default()), key('a', M::NONE));
        assert_eq!(press("A", "a", Mods { shift: true, ..Default::default() }), key('A', M::SHIFT));
        assert_eq!(press("!", "1", Mods { shift: true, ..Default::default() }), key('!', M::NONE));
        assert_eq!(press("ü", "ü", Mods::default()), key('ü', M::NONE));
        // Right Option arrives without `alt`: it types what the layout says (German @).
        assert_eq!(press("@", "l", Mods::default()), key('@', M::NONE));
    }

    #[test]
    fn chords_look_like_the_kitty_protocol() {
        let ctrl_shift = Mods { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(press("Z", "z", ctrl_shift), key('z', M::CONTROL | M::SHIFT));
        assert_eq!(press("/", "7", ctrl_shift), key('/', M::CONTROL), "German Ctrl+Shift+7 is Ctrl+/");
        assert_eq!(press("å", "a", Mods { alt: true, ..Default::default() }), key('a', M::ALT));
        assert_eq!(press("¡", "1", Mods { alt: true, ..Default::default() }), key('1', M::ALT));
    }

    #[test]
    fn cmd_is_ctrl_except_in_the_shell() {
        let cmd = Mods { cmd: true, ..Default::default() };
        assert_eq!(press("s", "s", cmd), key('s', M::CONTROL));
        assert_eq!(press("F", "f", Mods { shift: true, ..cmd }), key('f', M::CONTROL | M::SHIFT));
        assert_eq!(press("v", "v", cmd), Action::Paste);
        assert_eq!(press("q", "q", cmd), Action::Quit);
        assert_eq!(press("+", "+", cmd), Action::Zoom(1));
        assert_eq!(translate(&ch("c"), &ch("c"), cmd, true), Action::Nothing, "Cmd+C must not interrupt the shell");
        assert_eq!(translate(&ch("v"), &ch("v"), cmd, true), Action::Paste);
    }

    #[test]
    fn named_keys() {
        let n = |k, m| translate(&Key::Named(k), &Key::Named(k), m, false);
        let shift = Mods { shift: true, ..Default::default() };
        let cmd = Mods { cmd: true, ..Default::default() };
        assert_eq!(n(NamedKey::Tab, shift), Action::Key(KeyEvent::new(KeyCode::BackTab, M::SHIFT)));
        assert_eq!(n(NamedKey::F12, Mods { alt: true, ..Default::default() }), Action::Key(KeyEvent::new(KeyCode::F(12), M::ALT)));
        assert_eq!(n(NamedKey::Space, Mods { ctrl: true, ..Default::default() }), key(' ', M::CONTROL));
        assert_eq!(n(NamedKey::ArrowLeft, cmd), Action::Key(KeyEvent::new(KeyCode::Home, M::NONE)));
        assert_eq!(n(NamedKey::ArrowDown, cmd), Action::Key(KeyEvent::new(KeyCode::End, M::CONTROL)));
    }
}
