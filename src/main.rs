//! Code Atlas: a terminal code editor with IntelliJ-style keys.
//! Usage: `catlas [--vim] [dir] [file ...]` (no folder given = the current folder is the project)

mod agents;
mod app;
mod dock;
mod doc;
mod git;
mod project;
mod lsp;
mod syntax;
mod term;
mod ui;
mod vim;

use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
        println!("catlas [--vim] [dir] [file ...]   open a project folder (default: the current one) and files in Code Atlas");
        println!("  --vim   start in Vim mode (also CATLAS_VIM=1; Ctrl+Alt+V toggles at runtime)");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("catlas {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let env_vim = std::env::var("CATLAS_VIM").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
    let vim = env_vim || args.iter().any(|a| a == "--vim");
    let files = args.into_iter().filter(|a| a != "--vim").map(PathBuf::from).collect();
    let mut app = app::App::new(files)?;
    if vim {
        app.set_vim(true);
    }

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
    let mut block = false;
    let mut redraw = true;
    let mut last_draw = Instant::now();
    while !app.quit {
        app.tick();
        redraw |= app.agents_tick();
        redraw |= app.dock_tick();
        if redraw || last_draw.elapsed() >= Duration::from_millis(250) {
            term.draw(|f| ui::draw(f, app))?;
            redraw = false;
            last_draw = Instant::now();
            if ui::block_cursor(app) != block {
                block = !block;
                execute!(stdout(), if block { SetCursorStyle::SteadyBlock } else { SetCursorStyle::SteadyBar })?;
            }
        }
        // Short poll so language-server replies show up quickly.
        if event::poll(Duration::from_millis(30))? {
            app.handle(event::read()?);
            // Drain queued events (fast typing, pastes) before the next draw.
            while !app.quit && event::poll(Duration::ZERO)? {
                app.handle(event::read()?);
            }
            redraw = true;
        }
        redraw |= app.lsp_tick();
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
