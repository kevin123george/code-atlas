//! Keys → the bytes an xterm sends, and the terminal queries we answer.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The bytes for a key press, xterm style. `app_cursor` = the program asked for
/// application cursor keys (`ESC O A` instead of `ESC [ A`).
pub fn encode(k: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    // xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let esc_prefixed = |mut v: Vec<u8>| {
        if alt {
            v.insert(0, 0x1b);
        }
        v
    };
    let cursor = |letter: char| {
        if m > 1 {
            format!("\x1b[1;{m}{letter}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let tilde = |n: u8| if m > 1 { format!("\x1b[{n};{m}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() };
    let out = match k.code {
        KeyCode::Char(c) => {
            let bytes = match ctrl.then(|| ctrl_byte(c)).flatten() {
                Some(b) => vec![b],
                None => c.to_string().into_bytes(),
            };
            esc_prefixed(bytes)
        }
        KeyCode::Enter => esc_prefixed(vec![b'\r']),
        KeyCode::Backspace => esc_prefixed(vec![if ctrl { 0x08 } else { 0x7f }]),
        KeyCode::Tab if shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => esc_prefixed(vec![b'\t']),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => esc_prefixed(vec![0x1b]),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let letter = (b'P' + n - 1) as char;
            if m > 1 {
                format!("\x1b[1;{m}{letter}").into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]),
        _ => return None,
    };
    Some(out)
}

/// Ctrl+key as a control byte (Ctrl+A = 1 … Ctrl+Z = 26, Ctrl+Space = 0, Ctrl+[ = Esc …).
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        ' ' | '@' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    /// `ESC [ 6 n`: where is the cursor?
    CursorPosition,
    /// `ESC [ c` / `ESC [ 0 c`: what are you?
    DeviceAttributes,
}

/// Queries in a chunk of program output, in order.
pub fn queries(bytes: &[u8]) -> Vec<Query> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let rest = &bytes[i + 2..];
            if rest.starts_with(b"6n") {
                out.push(Query::CursorPosition);
            } else if rest.starts_with(b"c") || rest.starts_with(b"0c") {
                out.push(Query::DeviceAttributes);
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    fn enc(code: KeyCode, m: KeyModifiers) -> Vec<u8> {
        encode(&k(code, m), false).unwrap()
    }

    #[test]
    fn plain_and_control_keys() {
        let none = KeyModifiers::NONE;
        assert_eq!(enc(KeyCode::Char('l'), none), b"l");
        assert_eq!(enc(KeyCode::Char('é'), none), "é".as_bytes());
        assert_eq!(enc(KeyCode::Char('c'), KeyModifiers::CONTROL), [3]);
        assert_eq!(enc(KeyCode::Char('d'), KeyModifiers::CONTROL), [4]);
        assert_eq!(enc(KeyCode::Char(' '), KeyModifiers::CONTROL), [0]);
        assert_eq!(enc(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(enc(KeyCode::Enter, none), b"\r");
        assert_eq!(enc(KeyCode::Backspace, none), [0x7f]);
        assert_eq!(enc(KeyCode::Esc, none), [0x1b]);
        assert_eq!(enc(KeyCode::BackTab, KeyModifiers::SHIFT), b"\x1b[Z");
    }

    #[test]
    fn cursor_and_function_keys() {
        let none = KeyModifiers::NONE;
        assert_eq!(enc(KeyCode::Up, none), b"\x1b[A");
        assert_eq!(encode(&k(KeyCode::Up, none), true).unwrap(), b"\x1bOA");
        assert_eq!(enc(KeyCode::Right, KeyModifiers::CONTROL), b"\x1b[1;5C");
        assert_eq!(enc(KeyCode::Left, KeyModifiers::ALT), b"\x1b[1;3D");
        assert_eq!(enc(KeyCode::Delete, none), b"\x1b[3~");
        assert_eq!(enc(KeyCode::PageUp, KeyModifiers::SHIFT), b"\x1b[5;2~");
        assert_eq!(enc(KeyCode::F(1), none), b"\x1bOP");
        assert_eq!(enc(KeyCode::F(5), none), b"\x1b[15~");
        assert_eq!(enc(KeyCode::F(12), none), b"\x1b[24~");
    }

    #[test]
    fn finds_queries() {
        assert_eq!(queries(b"abc\x1b[6nxyz\x1b[c"), vec![Query::CursorPosition, Query::DeviceAttributes]);
        assert_eq!(queries(b"\x1b[0c"), vec![Query::DeviceAttributes]);
        assert!(queries(b"\x1b[31mred\x1b[0m").is_empty());
    }
}
