//! Code Atlas: a code editor with IntelliJ-style keys, in a terminal or its own window.
//! Usage: `catlas [--window] [--vim] [dir] [file ...]` (no folder given = the current folder is the project)

mod agents;
mod app;
mod config;
mod dock;
mod doc;
mod git;
mod hints;
mod index;
mod project;
mod lsp;
mod mackeys;
mod mission;
mod syntax;
mod term;
mod ui;
mod vim;
mod window;
mod workspace;

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
        println!("catlas [--vim] [--no-hints] [dir] [file ...]   open a project folder (default: the current one) and files in Code Atlas");
        println!("catlas dir1 dir2 … | x.catlas.toml | folder-of-repos   a workspace with several roots (Ctrl+O: repos)");
        println!("  --window     open in its own window instead of the terminal (the Code Atlas app does this)");
        println!("  --single     open a folder that holds several repos as one folder (no workspace)");
        println!("  -w [n|name]  list the recent workspaces, or open one of them");
        println!("  --vim        start in Vim mode (also CATLAS_VIM=1; Ctrl+Alt+V toggles at runtime)");
        println!("  --no-hints   start without the shortcut helper (Alt+/ toggles it and saves the choice)");
        println!("catlas --health [dir]   which languages have highlighting and which language servers are installed");
        println!("catlas --index-status [dir]   the code index of a folder: files, symbols, size, cache folder, what changed since");
        println!("catlas --bench-index <dir> [--files N] [--keep]   benchmark the code index (an empty or missing dir gets a synthetic repo of N files)");
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--health") {
        let dir = args.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        lsp::health::run(&dir);
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--index-status" || a == "--bench-index" || a == "--bench-warm") {
        return index::cli::run(&args);
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("catlas {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // The native window (decision 0017): asked for, or running inside the app bundle.
    let mut args = args;
    let in_bundle = std::env::current_exe().is_ok_and(|e| e.to_string_lossy().contains(".app/Contents/MacOS/"));
    let wants_window = args.iter().any(|a| a == "--window") || in_bundle;
    args.retain(|a| a != "--window" && !a.starts_with("-psn_"));
    let render = take_render_args(&mut args)?;
    if wants_window {
        window::prepare(&args);
    }
    // Workspaces (decision 0013): -w lists / reopens recent ones, --single opts out of them.
    if let Some(i) = args.iter().position(|a| a == "-w" || a == "--workspace") {
        let list = workspace::load_recent(workspace::recent_path().as_deref());
        let Some(pick) = args.get(i + 1).cloned() else {
            println!("{}", workspace::list_recent(&list));
            return Ok(());
        };
        let Some(r) = workspace::pick_recent(&list, &pick) else {
            eprintln!("No recent workspace {pick:?}:\n{}", workspace::list_recent(&list));
            std::process::exit(2);
        };
        match r.open() {
            Ok(ws) => *workspace::PRESET.lock().unwrap() = Some(ws),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
        args.drain(i..i + 2);
    }
    if let Some(i) = args.iter().position(|a| a == "--single") {
        workspace::SINGLE.store(true, std::sync::atomic::Ordering::Relaxed);
        args.remove(i);
    }
    let env_vim = std::env::var("CATLAS_VIM").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
    let vim = env_vim || args.iter().any(|a| a == "--vim");
    let no_hints = args.iter().any(|a| a == "--no-hints");
    let files = args.into_iter().filter(|a| a != "--vim" && a != "--no-hints").map(PathBuf::from).collect();
    let mut app = app::App::new(files)?;
    if vim {
        app.set_vim(true);
    }
    if no_hints {
        app.hints.on = false; // this run only: the saved choice stays
    }

    if let Some(r) = render {
        return window::render_png(&mut app, &r.out, r.cols, r.rows, r.wait, &r.keys);
    }
    if wants_window {
        return window::run(&mut app);
    }

    let enhanced = setup()?;
    app.hints.kitty = enhanced;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore(enhanced);
        hook(info);
    }));

    let result = run(&mut app);
    restore(enhanced)?;
    result
}

struct RenderArgs {
    out: PathBuf,
    cols: u16,
    rows: u16,
    wait: Duration,
    keys: Vec<String>,
}

/// `--render-png out.png [--size 120x40] [--wait ms] [--keys alt+1,ctrl+t,x]`: one frame of the window
/// without opening one, the way its look is checked (decision 0017).
fn take_render_args(args: &mut Vec<String>) -> Result<Option<RenderArgs>> {
    let Some(i) = args.iter().position(|a| a == "--render-png") else { return Ok(None) };
    let mut value = |name: &str| -> Option<String> {
        let at = args.iter().position(|a| a == name)?;
        let v = args.get(at + 1).cloned();
        args.drain(at..(at + 2).min(args.len()));
        v
    };
    let _ = i;
    let out = value("--render-png").ok_or_else(|| anyhow::anyhow!("--render-png needs a file"))?;
    let size = value("--size").unwrap_or_else(|| "120x40".into());
    let wait = value("--wait").and_then(|w| w.parse().ok()).unwrap_or(1500);
    let keys = value("--keys").map(|k| k.split(',').map(str::to_owned).collect()).unwrap_or_default();
    let (cols, rows) = size.split_once('x').and_then(|(c, r)| Some((c.parse().ok()?, r.parse().ok()?))).ok_or_else(|| anyhow::anyhow!("--size is like 120x40"))?;
    Ok(Some(RenderArgs { out: out.into(), cols, rows, wait: Duration::from_millis(wait), keys }))
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
        redraw |= app.mission_tick();
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
