//! The editor as a whole: open tabs, the clipboard, prompts (find, go to line, save as)
//! and the IntelliJ-style key map.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::doc::{Clip, Doc};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Find,
    GotoLine,
    SaveAs,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
}

impl PromptKind {
    pub fn label(self) -> &'static str {
        match self {
            PromptKind::Find => "Find",
            PromptKind::GotoLine => "Go to line[:column]",
            PromptKind::SaveAs => "Save as",
        }
    }
}

/// Screen areas from the last draw, used to map mouse clicks.
#[derive(Default, Clone, Copy)]
pub struct Layout {
    pub text_x: u16,
    pub text_y: u16,
    pub text_h: u16,
}

pub struct App {
    pub docs: Vec<Doc>,
    pub active: usize,
    pub prompt: Option<Prompt>,
    pub message: Option<String>,
    pub quit: bool,
    /// Set by a first Ctrl+Q (or Ctrl+W) on unsaved work; the second press goes through.
    armed: Option<char>,
    clip: Clip,
    last_find: String,
    pub layout: Layout,
    /// Tab titles' x ranges from the last draw.
    pub tab_spans: Vec<(u16, u16)>,
    dragging: bool,
    /// Scroll to the cursor at the next draw (keyboard actions do; mouse wheel doesn't).
    pub follow: bool,
    /// Project view: tree, finder, find in files, recent files (`src/project/`).
    pub project: crate::project::Project,
    /// Agents panel: Claude Code runs, staff (`src/agents/`).
    pub agents: crate::agents::Agents,
}

impl App {
    pub fn new(paths: Vec<PathBuf>) -> anyhow::Result<Self> {
        let (root, paths, as_project) = crate::project::split_args(paths);
        let mut project = crate::project::Project::new(root, as_project);
        let mut docs = Vec::new();
        for p in &paths {
            project.note_opened(p);
            docs.push(Doc::open(p)?);
        }
        if docs.is_empty() {
            docs.push(Doc::empty());
        }
        project.focus_tree = as_project && paths.is_empty();
        Ok(App {
            agents: crate::agents::Agents::new(project.root.clone()),
            project,
            docs,
            active: 0,
            prompt: None,
            message: Some("Ctrl+S save · Ctrl+P file · Alt+1 project · Alt+F find in files · Ctrl+E recent · Ctrl+Q quit".into()),
            quit: false,
            armed: None,
            clip: Clip::default(),
            last_find: String::new(),
            layout: Layout::default(),
            tab_spans: Vec::new(),
            dragging: false,
            follow: true,
        })
    }

    pub fn doc(&self) -> &Doc {
        &self.docs[self.active]
    }

    fn doc_mut(&mut self) -> &mut Doc {
        &mut self.docs[self.active]
    }

    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let armed = self.armed.take();
                if self.prompt.is_some() {
                    self.prompt_key(k);
                } else if self.agents_key(k) {
                    // Handled by the agents panel (src/agents/).
                } else if self.project_key(k) {
                    // Handled by the project view (src/project/).
                } else {
                    self.message = None;
                    self.key(k, armed);
                }
                self.follow = true;
            }
            Event::Paste(text) => {
                if let Some(p) = &mut self.prompt {
                    p.input.push_str(text.lines().next().unwrap_or(""));
                } else if self.project.popup.is_some() {
                    self.project.paste(&text);
                } else if self.agents_paste(&text) {
                } else {
                    self.doc_mut().insert_str(&text);
                    self.follow = true;
                }
            }
            Event::Mouse(m) => {
                if !self.agents_mouse(m) && !self.project_mouse(m) {
                    self.mouse(m);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, k: KeyEvent, armed: Option<char>) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let d = &mut self.docs[self.active];
        match k.code {
            // Files and tabs
            KeyCode::Char('s') if ctrl => self.save(),
            KeyCode::Char('q') if ctrl => {
                let unsaved = self.docs.iter().filter(|d| d.dirty()).count();
                if unsaved == 0 || armed == Some('q') {
                    self.quit = true;
                } else {
                    self.armed = Some('q');
                    self.message = Some(format!("{unsaved} file(s) not saved. Ctrl+Q again quits without saving."));
                }
            }
            KeyCode::Char('w') if ctrl => {
                if !d.dirty() || armed == Some('w') {
                    self.close_tab();
                } else {
                    self.armed = Some('w');
                    self.message = Some("Not saved. Ctrl+W again closes without saving.".into());
                }
            }
            KeyCode::Char('n') if ctrl => {
                self.docs.push(Doc::empty());
                self.active = self.docs.len() - 1;
            }
            KeyCode::Left if alt && !shift => self.active = (self.active + self.docs.len() - 1) % self.docs.len(),
            KeyCode::Right if alt && !shift => self.active = (self.active + 1) % self.docs.len(),

            // Prompts
            KeyCode::Char('f') if ctrl => {
                let input = d.selected_text().filter(|s| !s.contains('\n')).unwrap_or_else(|| self.last_find.clone());
                self.prompt = Some(Prompt { kind: PromptKind::Find, input });
            }
            KeyCode::F(3) => {
                let q = self.last_find.clone();
                if !self.docs[self.active].find(&q, shift) {
                    self.message = Some(format!("Not found: {q}"));
                }
            }
            KeyCode::Char('g') if ctrl => self.prompt = Some(Prompt { kind: PromptKind::GotoLine, input: String::new() }),

            // History and clipboard
            KeyCode::Char('z') | KeyCode::Char('Z') if ctrl && shift => {
                d.redo();
            }
            KeyCode::Char('z') if alt => {
                d.redo();
            }
            KeyCode::Char('z') if ctrl => {
                d.undo();
            }
            KeyCode::Char('c') if ctrl => {
                self.clip = d.copy();
                set_system_clipboard(&self.clip.text);
            }
            KeyCode::Char('x') if ctrl => {
                self.clip = d.cut();
                set_system_clipboard(&self.clip.text);
            }
            KeyCode::Char('v') if ctrl => d.paste(&self.clip.clone()),
            KeyCode::Char('a') if ctrl => d.select_all(),
            KeyCode::Char('d') if ctrl => d.duplicate(),
            KeyCode::Char('y') if ctrl => d.delete_lines(),

            // Movement (Shift selects)
            KeyCode::Up if alt && shift => d.move_lines(false),
            KeyCode::Down if alt && shift => d.move_lines(true),
            KeyCode::Left if ctrl => d.word_left(shift),
            KeyCode::Right if ctrl => d.word_right(shift),
            KeyCode::Left => d.left(shift),
            KeyCode::Right => d.right(shift),
            KeyCode::Up => d.vertical(-1, shift),
            KeyCode::Down => d.vertical(1, shift),
            KeyCode::Home if ctrl => d.doc_start(shift),
            KeyCode::End if ctrl => d.doc_end(shift),
            KeyCode::Home => d.home(shift),
            KeyCode::End => d.end(shift),
            KeyCode::PageUp => d.page(false, shift),
            KeyCode::PageDown => d.page(true, shift),

            // Typing
            KeyCode::Backspace if ctrl || alt => d.delete_word_left(),
            KeyCode::Char('h') if ctrl => d.delete_word_left(),
            KeyCode::Delete if ctrl => d.delete_word_right(),
            KeyCode::Backspace => d.backspace(),
            KeyCode::Delete => d.delete(),
            KeyCode::Enter => d.newline(),
            KeyCode::BackTab => d.untab(),
            KeyCode::Tab if shift => d.untab(),
            KeyCode::Tab => d.tab(),
            KeyCode::Esc => d.move_to(d.cursor, false),
            KeyCode::Char(c) if !ctrl && !alt => d.type_char(c),
            _ => {}
        }
    }

    fn prompt_key(&mut self, k: KeyEvent) {
        let Some(p) = &mut self.prompt else { return };
        match k.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Backspace => {
                p.input.pop();
            }
            KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => p.input.push(c),
            KeyCode::Enter => {
                let Prompt { kind, input } = self.prompt.take().unwrap();
                match kind {
                    PromptKind::Find => {
                        self.last_find = input.clone();
                        if !self.doc_mut().find(&input, false) {
                            self.message = Some(format!("Not found: {input}"));
                        } else {
                            self.message = Some("F3 next · Shift+F3 previous".into());
                        }
                    }
                    PromptKind::GotoLine => {
                        let mut parts = input.trim().split(':');
                        let line = parts.next().and_then(|s| s.trim().parse::<usize>().ok());
                        let col = parts.next().and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1);
                        match line {
                            Some(l) => self.doc_mut().goto_line(l.saturating_sub(1), col.saturating_sub(1)),
                            None => self.message = Some("Type a line number, like 42 or 42:7".into()),
                        }
                    }
                    PromptKind::SaveAs => {
                        if !input.trim().is_empty() {
                            self.doc_mut().path = Some(PathBuf::from(input.trim()));
                            self.save();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn save(&mut self) {
        let d = &mut self.docs[self.active];
        if d.path.is_none() {
            self.prompt = Some(Prompt { kind: PromptKind::SaveAs, input: String::new() });
            return;
        }
        self.message = Some(match d.save() {
            Ok(()) => format!("Saved {}", d.name()),
            Err(e) => format!("Couldn't save: {e:#}"),
        });
    }

    fn close_tab(&mut self) {
        self.docs.remove(self.active);
        if self.docs.is_empty() {
            self.docs.push(Doc::empty());
        }
        self.active = self.active.min(self.docs.len() - 1);
    }

    fn mouse(&mut self, m: MouseEvent) {
        let l = self.layout;
        let in_text = m.column >= l.text_x && m.row >= l.text_y && m.row < l.text_y + l.text_h;
        match m.kind {
            MouseEventKind::ScrollDown => {
                let d = self.doc_mut();
                d.scroll_line = (d.scroll_line + 3).min(d.visible_lines().saturating_sub(1));
                self.follow = false;
            }
            MouseEventKind::ScrollUp => {
                let d = self.doc_mut();
                d.scroll_line = d.scroll_line.saturating_sub(3);
                self.follow = false;
            }
            MouseEventKind::Down(MouseButton::Left) if m.row == 0 => {
                if let Some(i) = self.tab_spans.iter().position(|(a, b)| m.column >= *a && m.column < *b) {
                    self.active = i;
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_text => {
                let shift = m.modifiers.contains(KeyModifiers::SHIFT);
                let pos = self.pos_at(m.column, m.row);
                self.doc_mut().move_to(pos, shift);
                self.dragging = true;
                self.follow = true;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let row = m.row.clamp(l.text_y, l.text_y + l.text_h.saturating_sub(1));
                let pos = self.pos_at(m.column.max(l.text_x), row);
                self.doc_mut().move_to(pos, true);
                self.follow = true;
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = false,
            _ => {}
        }
    }

    fn pos_at(&self, x: u16, y: u16) -> usize {
        let d = self.doc();
        let line = (d.scroll_line + (y - self.layout.text_y) as usize).min(d.visible_lines() - 1);
        let col = d.scroll_col + x.saturating_sub(self.layout.text_x) as usize;
        d.line_start(line) + d.char_col_at(line, col)
    }
}

/// Puts copied text on the system clipboard: pbcopy / wl-copy / xclip when there,
/// else the OSC 52 escape (works over SSH in most terminals).
fn set_system_clipboard(text: &str) {
    for (cmd, args) in [("pbcopy", &[][..]), ("wl-copy", &[][..]), ("xclip", &["-selection", "clipboard"][..])] {
        if let Ok(mut child) = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}
