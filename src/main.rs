//! Code Atlas: a terminal code editor with IntelliJ-style keys.
//! Usage: `catlas [dir] [file ...]` (no folder given = the current folder is the project)

mod app;
mod doc;
mod project;
mod ui;

use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("catlas [dir] [file ...]   open a project folder (default: the current one) and files in Code Atlas");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("catlas {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut app = app::App::new(args.into_iter().map(PathBuf::from).collect())?;

    let enhanced = setup()?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore(enhanced);
        hook(info);
    }));

    let result = run(&mut app);
    restore(enhanced)?;
    result
}

fn run(app: &mut app::App) -> Result<()> {
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    term.clear()?;
    while !app.quit {
        app.tick();
        term.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(250))? {
            app.handle(event::read()?);
            // Drain queued events (fast typing, pastes) before the next draw.
            while !app.quit && event::poll(Duration::ZERO)? {
                app.handle(event::read()?);
            }
        }
    }
    Ok(())
}

/// Raw mode, alternate screen, mouse, bracketed paste, and the kitty keyboard protocol where the
/// terminal has it (so Ctrl+Shift+Z and friends arrive intact). Returns whether that protocol is on.
fn setup() -> Result<bool> {
    terminal::enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, SetCursorStyle::SteadyBar)?;
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    Ok(enhanced)
}

fn restore(enhanced: bool) -> io::Result<()> {
    let mut out = stdout();
    if enhanced {
        execute!(out, PopKeyboardEnhancementFlags)?;
    }
    execute!(out, SetCursorStyle::DefaultUserShape, DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()
}
