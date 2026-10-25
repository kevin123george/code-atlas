//! Terminal pane (M5): `$SHELL` on a real PTY (`portable-pty`), screen emulation with `vt100`.
//! A reader thread streams the shell's output through a channel; [`Terminal::poll`] feeds it to
//! the emulator every loop turn (the loop polls every ~25 ms while a terminal is shown).

pub mod keys;
pub mod ui;

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Lines kept above the screen (mouse wheel / Shift+PgUp scrolls into them).
const SCROLLBACK: usize = 5000;

pub struct Terminal {
    parser: vt100::Parser,
    rx: Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Set once the shell is gone: what to show.
    pub exited: Option<String>,
    size: (u16, u16),
    pub shell: String,
}

/// The user's shell: `$SHELL`, else /bin/sh.
pub fn shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

impl Terminal {
    /// Starts `$SHELL -l` in `cwd` on a PTY of `rows` × `cols`.
    pub fn spawn(cwd: &Path, rows: u16, cols: u16) -> Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("opening a pty")?;
        let shell = shell();
        let mut cmd = CommandBuilder::new(&shell);
        let name = Path::new(&shell).file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, "zsh" | "bash" | "fish" | "sh" | "ksh") {
            cmd.arg("-l");
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "CodeAtlas");
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("starting {shell}"))?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer = pair.master.take_writer().context("pty writer")?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Terminal {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            rx,
            writer,
            master: pair.master,
            child,
            exited: None,
            size: (rows, cols),
            shell,
        })
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Feeds pending output to the emulator; returns whether anything arrived.
    pub fn poll(&mut self) -> bool {
        let mut got = false;
        loop {
            match self.rx.try_recv() {
                Ok(bytes) => {
                    got = true;
                    self.parser.process(&bytes);
                    self.answer(&bytes);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.exited.is_none() {
                        let code = self.child.try_wait().ok().flatten().map(|s| s.exit_code());
                        self.exited = Some(match code {
                            Some(c) => format!("{} exited ({c})", self.shell),
                            None => format!("{} exited", self.shell),
                        });
                        got = true;
                    }
                    break;
                }
            }
        }
        got
    }

    /// Replies to the queries a shell waits for (cursor position, device attributes):
    /// `vt100` emulates the screen but doesn't answer.
    fn answer(&mut self, bytes: &[u8]) {
        for q in keys::queries(bytes) {
            let reply = match q {
                keys::Query::CursorPosition => {
                    let (r, c) = self.parser.screen().cursor_position();
                    format!("\x1b[{};{}R", r + 1, c + 1)
                }
                keys::Query::DeviceAttributes => "\x1b[?62;22c".to_string(),
            };
            self.write(reply.as_bytes());
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if self.exited.is_some() {
            return;
        }
        let _ = self.writer.write_all(bytes).and_then(|_| self.writer.flush());
    }

    /// Text pasted into the shell, bracketed when the program asked for it.
    pub fn paste(&mut self, text: &str) {
        self.scroll_to_bottom();
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.screen().bracketed_paste() {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        self.parser.set_size(rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    /// Scrolls the view up (+) or down (−) through the scrollback.
    pub fn scroll(&mut self, delta: isize) {
        let now = self.parser.screen().scrollback() as isize;
        self.parser.set_scrollback((now + delta).max(0) as usize);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.parser.set_scrollback(0);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn runs_a_command_in_a_real_pty() {
        let dir = std::env::temp_dir();
        let mut t = Terminal::spawn(&dir, 10, 60).unwrap();
        t.write(b"echo cat''las-$((40+2))\r");
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            t.poll();
            if t.screen().contents().contains("catlas-42") {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no output; screen:\n{}", t.screen().contents());
    }
}
