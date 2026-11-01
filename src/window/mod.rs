//! The native window (decision 0017): `catlas --window`, or the `Code Atlas.app` bundle. The same
//! `App` and `ui::draw` as in a terminal, on a `GridBackend`; window keys and the mouse become the
//! crossterm events `App::handle` already takes.

mod backend;
mod input;
mod raster;

use std::io::IsTerminal;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton as TermButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersKeyState;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Theme, Window, WindowId};

use crate::app::App;
use crate::ui;
use backend::GridBackend;
use input::{translate, Action, Mods};
use raster::{draw_cursor, write_png, Canvas, FontSet};

/// Set while running in a window: notifications go to the desktop instead of stdout.
static WINDOWED: AtomicBool = AtomicBool::new(false);

pub fn is_windowed() -> bool {
    WINDOWED.load(Ordering::Relaxed)
}

const DEFAULT_PT: f32 = 13.0;
const MIN_PT: f32 = 8.0;
const MAX_PT: f32 = 40.0;

/// `[window]` in `settings.toml`: `font` (a font file) and `font_size` (points).
fn settings() -> (Option<PathBuf>, f32) {
    let table = crate::config::settings_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| t.parse::<toml::Table>().ok());
    let section = table.as_ref().and_then(|t| t.get("window")).and_then(|v| v.as_table());
    let font = section.and_then(|s| s.get("font")).and_then(|v| v.as_str()).map(PathBuf::from);
    let size = section.and_then(|s| s.get("font_size")).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
    (font, size.map_or(DEFAULT_PT, |s| (s as f32).clamp(MIN_PT, MAX_PT)))
}

/// Opened from Finder or the Dock there is no terminal: no shell `PATH` (so no `claude`, `git`,
/// language servers) and the folder is `/`. Takes both from the login shell and the recent list.
pub fn prepare(args: &[String]) {
    if std::io::stdin().is_terminal() {
        return;
    }
    if let Some(path) = login_path() {
        std::env::set_var("PATH", path);
    }
    if !args.is_empty() {
        return;
    }
    let list = crate::workspace::load_recent(crate::workspace::recent_path().as_deref());
    if let Some(Ok(ws)) = crate::workspace::pick_recent(&list, "1").map(|r| r.open()) {
        *crate::workspace::PRESET.lock().unwrap() = Some(ws);
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let projects = home.join("Projects");
        let _ = std::env::set_current_dir(if projects.is_dir() { projects } else { home });
    }
}

/// `$PATH` as an interactive login shell sees it; gives up after 3 s (a slow or odd shell config).
fn login_path() -> Option<String> {
    const MARK: &str = "__CATLAS_PATH__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(shell).args(["-lic", &format!("printf '\\n{MARK}%s\\n' \"$PATH\"")]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if start.elapsed() > Duration::from_secs(3) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|l| l.strip_prefix(MARK)).filter(|p| !p.is_empty()).map(str::to_owned)
}

/// The system clipboard's text. Blocks for the few ms the helper takes, only on Cmd+V.
fn read_clipboard() -> Option<String> {
    let tools: [(&str, &[&str]); 3] = [("pbpaste", &[]), ("wl-paste", &["-n"]), ("xclip", &["-selection", "clipboard", "-o"])];
    tools.iter().find_map(|(cmd, args)| {
        let out = Command::new(cmd).args(*args).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

/// A desktop notification (agent finished / needs you). The text goes in as an argument, never as script.
pub fn notify(text: &str) {
    if std::env::var("CATLAS_NOTIFY").is_ok_and(|v| matches!(v.trim(), "off" | "0" | "false" | "no")) {
        return;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("osascript");
        c.args(["-e", "on run argv", "-e", "display notification (item 1 of argv) with title \"Code Atlas\"", "-e", "end run", text]);
        c
    } else {
        let mut c = Command::new("notify-send");
        c.args(["Code Atlas", text]);
        c
    };
    let _ = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// One frame through the real painter into a PNG, no window (`--render-png`): how the look is checked
/// without screen-recording permission. `keys`: labels like `alt+1`, pressed before the picture.
pub fn render_png(app: &mut App, out: &std::path::Path, cols: u16, rows: u16, wait: Duration, keys: &[String]) -> Result<()> {
    let (font, pt) = settings();
    let px = pt * 2.0;
    let mut canvas = Canvas::new(&FontSet::system(font), px, 1, 1)?;
    canvas.resize(cols as usize * canvas.cell_w, rows as usize * canvas.cell_h);
    app.hints.kitty = true;
    let mut term = Terminal::new(GridBackend::new(canvas))?;
    let settle = |app: &mut App, term: &mut Terminal<GridBackend>, wait: Duration| -> Result<()> {
        let start = Instant::now();
        loop {
            app.tick();
            app.agents_tick();
            app.dock_tick();
            app.mission_tick();
            app.lsp_tick();
            term.draw(|f| ui::draw(f, app))?;
            if start.elapsed() >= wait {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    };
    settle(app, &mut term, wait)?;
    for label in keys {
        let k = parse_key(label).ok_or_else(|| anyhow!("unknown key {label:?}"))?;
        app.handle(TermEvent::Key(k));
        settle(app, &mut term, Duration::from_millis(250))?;
    }
    if !keys.is_empty() {
        settle(app, &mut term, wait)?; // what the keys started (a shell, a search) has time to show
    }
    let block = ui::block_cursor(app);
    let b = term.backend_mut();
    let mut pixels = b.canvas.pixels.clone();
    if b.cursor_on {
        draw_cursor(&mut pixels, b.canvas.width, b.canvas.cell_w, b.canvas.cell_h, b.cursor.x, b.cursor.y, block);
    }
    write_png(out, &pixels, b.canvas.width, b.canvas.height)
}

/// `ctrl+shift+f`, `alt+1`, `esc`, `f12`, `x`.
fn parse_key(label: &str) -> Option<KeyEvent> {
    let parts: Vec<&str> = label.split('+').collect();
    let (name, mods) = parts.split_last()?;
    let mut m = KeyModifiers::NONE;
    for p in mods {
        m |= match *p {
            "ctrl" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return None,
        };
    }
    let code = match *name {
        "esc" => KeyCode::Esc,
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "down" => KeyCode::Down,
        "up" => KeyCode::Up,
        "space" => KeyCode::Char(' '),
        f if f.len() > 1 && f.starts_with('f') => KeyCode::F(f[1..].parse().ok()?),
        c if c.chars().count() == 1 => KeyCode::Char(c.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent::new(code, m))
}

pub fn run(app: &mut App) -> Result<()> {
    WINDOWED.store(true, Ordering::Relaxed);
    app.hints.kitty = true; // every chord arrives intact here
    let mut builder = EventLoop::builder();
    #[cfg(target_os = "macos")]
    {
        // winit's default menu quits on Cmd+Q without asking; unsaved files must get the question.
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        builder.with_default_menu(false);
    }
    let event_loop = builder.build()?;
    let (font, pt) = settings();
    let mut win = Win { app, shell: None, fonts: FontSet::system(font), pt, base_pt: pt, mods: Default::default(), left_alt: false, right_alt: false, pointer: (0, 0), pressed: None, scroll: 0.0, redraw: true, last_draw: Instant::now(), dump: std::env::var_os("CATLAS_WINDOW_DUMP").map(PathBuf::from), last_dump: None, error: None };
    event_loop.run_app(&mut win)?;
    match win.error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

struct Shell {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    term: Terminal<GridBackend>,
}

struct Win<'a> {
    app: &'a mut App,
    shell: Option<Shell>,
    fonts: FontSet,
    pt: f32,
    base_pt: f32,
    mods: winit::keyboard::ModifiersState,
    left_alt: bool,
    right_alt: bool,
    pointer: (u16, u16),
    pressed: Option<TermButton>,
    scroll: f64,
    redraw: bool,
    last_draw: Instant,
    dump: Option<PathBuf>,
    last_dump: Option<Instant>,
    error: Option<anyhow::Error>,
}

impl Win<'_> {
    fn open(&mut self, el: &ActiveEventLoop) -> Result<Shell> {
        let attrs = Window::default_attributes().with_title(title()).with_inner_size(LogicalSize::new(1280.0, 820.0)).with_min_inner_size(LogicalSize::new(480.0, 300.0)).with_theme(Some(Theme::Dark));
        let window = Rc::new(el.create_window(attrs).map_err(|e| anyhow!("{e}"))?);
        window.set_ime_allowed(true);
        let context = softbuffer::Context::new(window.clone()).map_err(|e| anyhow!("{e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| anyhow!("{e}"))?;
        let size = window.inner_size();
        let canvas = Canvas::new(&self.fonts, self.pt * window.scale_factor() as f32, size.width as usize, size.height as usize)?;
        let term = Terminal::new(GridBackend::new(canvas))?;
        let mut shell = Shell { window, surface, term };
        resize_surface(&mut shell, size)?;
        Ok(shell)
    }

    fn resized(&mut self, size: PhysicalSize<u32>) -> Result<()> {
        let Some(shell) = &mut self.shell else { return Ok(()) };
        resize_surface(shell, size)?;
        let c = &mut shell.term.backend_mut().canvas;
        c.resize(size.width as usize, size.height as usize);
        let (cols, rows) = c.grid();
        shell.term.clear()?;
        self.app.handle(TermEvent::Resize(cols, rows));
        self.redraw = true;
        Ok(())
    }

    fn zoom(&mut self, step: i8) -> Result<()> {
        self.pt = match step {
            0 => self.base_pt,
            s => (self.pt + s as f32).clamp(MIN_PT, MAX_PT),
        };
        self.apply_font()
    }

    fn apply_font(&mut self) -> Result<()> {
        let Some(shell) = &mut self.shell else { return Ok(()) };
        let px = self.pt * shell.window.scale_factor() as f32;
        let c = &mut shell.term.backend_mut().canvas;
        c.set_px(px);
        let (cols, rows) = c.grid();
        shell.term.clear()?;
        self.app.handle(TermEvent::Resize(cols, rows));
        self.redraw = true;
        Ok(())
    }

    fn keys_mods(&self) -> Mods {
        // Right Option alone stays a text key on a Mac (German @ [ ] { }), left Option is Alt.
        let alt = self.mods.alt_key() && !(cfg!(target_os = "macos") && self.right_alt && !self.left_alt);
        Mods { ctrl: self.mods.control_key(), shift: self.mods.shift_key(), alt, cmd: self.mods.super_key() }
    }

    fn key(&mut self, ev: winit::event::KeyEvent) -> Result<()> {
        if ev.state != ElementState::Pressed {
            return Ok(());
        }
        let shell_focus = self.app.dock.focused && self.app.dock.tab == crate::dock::Tab::Terminal;
        match translate(&ev.logical_key, &ev.key_without_modifiers(), self.keys_mods(), shell_focus) {
            Action::Key(k) => self.app.handle(TermEvent::Key(k)),
            Action::Paste => {
                if let Some(text) = read_clipboard() {
                    self.app.handle(TermEvent::Paste(text));
                }
            }
            Action::Quit => self.app.close_requested(),
            Action::Zoom(s) => self.zoom(s)?,
            Action::Nothing => return Ok(()),
        }
        self.redraw = true;
        Ok(())
    }

    fn mouse_mods(&self) -> KeyModifiers {
        let m = self.keys_mods();
        let mut out = KeyModifiers::NONE;
        if m.ctrl {
            out |= KeyModifiers::CONTROL;
        }
        if m.shift {
            out |= KeyModifiers::SHIFT;
        }
        if m.alt {
            out |= KeyModifiers::ALT;
        }
        out
    }

    fn mouse(&mut self, kind: MouseEventKind) {
        let (column, row) = self.pointer;
        let modifiers = self.mouse_mods();
        self.app.handle(TermEvent::Mouse(MouseEvent { kind, column, row, modifiers }));
        self.redraw = true;
    }

    fn moved(&mut self, pos: PhysicalPosition<f64>) {
        let Some(shell) = &self.shell else { return };
        let c = &shell.term.backend().canvas;
        let (cols, rows) = c.grid();
        let cell = ((pos.x.max(0.0) as usize / c.cell_w).min(cols as usize - 1) as u16, (pos.y.max(0.0) as usize / c.cell_h).min(rows as usize - 1) as u16);
        if cell != self.pointer {
            self.pointer = cell;
            self.mouse(match self.pressed {
                Some(b) => MouseEventKind::Drag(b),
                None => MouseEventKind::Moved,
            });
        }
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64,
            MouseScrollDelta::PixelDelta(p) => {
                let h = self.shell.as_ref().map_or(20, |s| s.term.backend().canvas.cell_h);
                p.y / h as f64
            }
        };
        self.scroll += lines;
        while self.scroll.abs() >= 1.0 {
            let up = self.scroll > 0.0;
            self.scroll -= if up { 1.0 } else { -1.0 };
            self.mouse(if up { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown });
        }
    }

    fn draw(&mut self) -> Result<()> {
        let Some(shell) = &mut self.shell else { return Ok(()) };
        let app = &mut *self.app;
        shell.term.draw(|f| ui::draw(f, app))?;
        self.last_draw = Instant::now();
        self.redraw = false;
        if shell.term.backend().dirty {
            self.present()?;
        }
        Ok(())
    }

    fn present(&mut self) -> Result<()> {
        let block = ui::block_cursor(self.app);
        let Some(shell) = &mut self.shell else { return Ok(()) };
        let b = shell.term.backend_mut();
        b.dirty = false;
        let c = &b.canvas;
        let mut buffer = shell.surface.buffer_mut().map_err(|e| anyhow!("{e}"))?;
        if buffer.len() != c.pixels.len() {
            return Ok(()); // a resize is on its way
        }
        buffer.copy_from_slice(&c.pixels);
        if b.cursor_on {
            draw_cursor(&mut buffer, c.width, c.cell_w, c.cell_h, b.cursor.x, b.cursor.y, block);
            let scale = (c.cell_w as i32 * b.cursor.x as i32, c.cell_h as i32 * b.cursor.y as i32);
            shell.window.set_ime_cursor_area(PhysicalPosition::new(scale.0, scale.1), PhysicalSize::new(c.cell_w as u32, c.cell_h as u32));
        }
        if let Some(path) = &self.dump {
            if self.last_dump.is_none_or(|t| t.elapsed() > Duration::from_millis(300)) {
                write_png(path, &buffer, c.width, c.height)?;
                self.last_dump = Some(Instant::now());
            }
        }
        buffer.present().map_err(|e| anyhow!("{e}"))
    }

    fn fail(&mut self, el: &ActiveEventLoop, r: Result<()>) {
        if let Err(e) = r {
            self.error = Some(e);
            el.exit();
        }
    }
}

fn title() -> String {
    let dir = std::env::current_dir().ok();
    let name = dir.as_deref().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned());
    name.map_or_else(|| "Code Atlas".into(), |n| format!("{n} — Code Atlas"))
}

fn resize_surface(shell: &mut Shell, size: PhysicalSize<u32>) -> Result<()> {
    if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
        shell.surface.resize(w, h).map_err(|e| anyhow!("{e}"))?;
    }
    Ok(())
}

impl ApplicationHandler for Win<'_> {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.shell.is_some() {
            return;
        }
        match self.open(el) {
            Ok(shell) => self.shell = Some(shell),
            Err(e) => {
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.app.close_requested();
                self.redraw = true;
            }
            WindowEvent::Resized(size) => {
                let r = self.resized(size);
                self.fail(el, r);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let r = self.apply_font();
                self.fail(el, r);
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                self.left_alt = m.lalt_state() == ModifiersKeyState::Pressed;
                self.right_alt = m.ralt_state() == ModifiersKeyState::Pressed;
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                let r = self.key(event);
                self.fail(el, r);
            }
            // A dead key's result (´ then e = é) and input methods.
            WindowEvent::Ime(Ime::Commit(text)) => {
                for c in text.chars() {
                    let shift = if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
                    self.app.handle(TermEvent::Key(KeyEvent::new(KeyCode::Char(c), shift)));
                }
                self.redraw = true;
            }
            WindowEvent::CursorMoved { position, .. } => self.moved(position),
            WindowEvent::MouseInput { state, button, .. } => {
                let b = match button {
                    MouseButton::Left => TermButton::Left,
                    MouseButton::Right => TermButton::Right,
                    MouseButton::Middle => TermButton::Middle,
                    _ => return,
                };
                if state == ElementState::Pressed {
                    self.pressed = Some(b);
                    self.mouse(MouseEventKind::Down(b));
                } else {
                    self.pressed = None;
                    self.mouse(MouseEventKind::Up(b));
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            WindowEvent::RedrawRequested => {
                if let Some(s) = &mut self.shell {
                    s.term.backend_mut().dirty = true;
                }
                let r = self.draw();
                self.fail(el, r);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.app.quit {
            el.exit();
            return;
        }
        self.app.tick();
        self.redraw |= self.app.agents_tick();
        self.redraw |= self.app.dock_tick();
        self.redraw |= self.app.mission_tick();
        self.redraw |= self.app.lsp_tick();
        if self.redraw || self.last_draw.elapsed() >= Duration::from_millis(250) {
            let r = self.draw();
            self.fail(el, r);
        }
        // The same 30 ms wake-up as the terminal loop: background threads report through channels.
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(30)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_shell_gives_a_path() {
        let path = login_path().expect("a login shell answers");
        assert!(path.split(':').any(|d| d == "/usr/bin"), "{path}");
    }

    #[test]
    fn key_labels_for_render_png() {
        assert_eq!(parse_key("alt+1"), Some(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT)));
        assert_eq!(parse_key("ctrl+shift+f"), Some(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)));
        assert_eq!(parse_key("f12"), Some(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE)));
        assert_eq!(parse_key("esc"), Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(parse_key("hyper+x"), None);
    }
}
