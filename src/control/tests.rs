//! Control Center tests: opening and keys, tiles from real state, settings that change live and
//! are saved with comments kept, edits to settings.toml applied, themes, drawing at 80 and 200
//! columns.

use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;

use super::tiles::{self, Act, TileId};
use super::*;
use crate::palette::tests::{app, press, typ};
use crate::ui::theme as t;

fn render(app: &mut App, w: u16, h: u16) -> (ratatui::buffer::Buffer, String) {
    let mut term = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer().clone();
    let text = (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect();
    (buf, text)
}

/// Ticks until the background samples (git, processes, server probe) are in.
fn settle(app: &mut App) {
    let t0 = Instant::now();
    while (app.control.repos.is_empty() || app.control.usage.is_none() || app.control.installed.is_none()) && t0.elapsed() < Duration::from_secs(10) {
        app.control_tick();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn git(dir: &std::path::Path, args: &[&str]) {
    assert!(crate::git::run(dir, args).is_ok(), "git {args:?}");
}

#[test]
fn opens_with_alt_o_or_oslash_and_closes_with_esc() {
    let (mut a, d) = app("open");
    press(&mut a, "Alt+O");
    assert!(a.control.open && a.control.page == Page::Overview);
    press(&mut a, "Esc");
    assert!(!a.control.open);
    // ø (Option+O without "Option as Meta") in the editor, not typed.
    a.handle(crossterm::event::Event::Key(KeyEvent::new(KeyCode::Char('ø'), KeyModifiers::NONE)));
    assert!(a.control.open);
    assert_eq!(a.docs[0].rope.to_string(), "one\ntwo\nthree\n");
    press(&mut a, "Alt+O");
    assert!(!a.control.open, "Alt+O again closes");
    // In a prompt ø is typed.
    press(&mut a, "Ctrl+F");
    a.handle(crossterm::event::Event::Key(KeyEvent::new(KeyCode::Char('ø'), KeyModifiers::NONE)));
    assert!(!a.control.open);
    assert_eq!(a.prompt.as_ref().unwrap().input, "ø");
    press(&mut a, "Esc");
    // The palette from the Control Center, and the Control Center from the palette.
    press(&mut a, "Alt+O");
    press(&mut a, "Ctrl+R");
    assert!(a.palette.open && a.control.open);
    press(&mut a, "Esc");
    assert!(!a.palette.open && a.control.open);
    press(&mut a, "Esc");
    press(&mut a, "Ctrl+R");
    press(&mut a, "Alt+O");
    assert!(!a.palette.open && a.control.open);
    // Keys don't reach the editor while it is open.
    typ(&mut a, "zz");
    assert_eq!(a.docs[0].rope.to_string(), "one\ntwo\nthree\n");
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn navigation_moves_between_tiles_and_rows() {
    let (mut a, d) = app("nav");
    press(&mut a, "Alt+O");
    assert_eq!(a.control.tile, 0);
    press(&mut a, "Tab");
    assert_eq!(tiles::ORDER[a.control.tile], TileId::Agents);
    press(&mut a, "Shift+Tab");
    press(&mut a, "Shift+Tab");
    assert_eq!(tiles::ORDER[a.control.tile], TileId::Settings);
    // ↓ walks the rows and runs into the next tile at the end; ↑ back.
    let n = tiles::build_one(&a, TileId::Settings).selectable().len();
    for _ in 0..n {
        press(&mut a, "↓");
    }
    assert_eq!(a.control.tile, 0, "wrapped into the first tile");
    press(&mut a, "↑");
    assert_eq!(tiles::ORDER[a.control.tile], TileId::Settings);
    assert_eq!(a.control.rows[a.control.tile], tiles::build_one(&a, TileId::Settings).rows.len() - 1);
    // 2 = settings screen, Esc back, Esc closes.
    press(&mut a, "2");
    assert_eq!(a.control.page, Page::Settings);
    press(&mut a, "Esc");
    assert_eq!(a.control.page, Page::Overview);
    // m: agents (Mission Control hook → the agents panel for now).
    press(&mut a, "m");
    assert!(!a.control.open && a.agents.visible && a.agents.focus);
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn settings_change_live_and_are_saved_with_comments() {
    let (mut a, d) = app("settings");
    let file = d.join("cfg/settings.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "# mine\nautocomplete = true   # typing\n").unwrap();
    a.control.store = Some(file.clone());
    a.control.read_file_state();
    assert!(a.lsp.auto);
    press(&mut a, "Alt+O");
    a.control.tile = tiles::ORDER.iter().position(|x| *x == TileId::Settings).unwrap();
    let rows = tiles::build_one(&a, TileId::Settings).rows;
    a.control.rows[a.control.tile] = rows.iter().position(|r| r.act == Act::Setting("autocomplete")).unwrap();
    press(&mut a, "Enter");
    assert!(!a.lsp.auto, "live");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# mine\nautocomplete = false  # typing\n");
    // Vim on: live, and saved as `vim = true` (a new line).
    a.control.rows[a.control.tile] = rows.iter().position(|r| r.act == Act::Setting("vim")).unwrap();
    press(&mut a, "Space");
    assert!(a.vim.is_some());
    assert!(std::fs::read_to_string(&file).unwrap().ends_with("autocomplete = false  # typing\nvim = true\n"));
    // The settings screen: theme with →, hints with Enter.
    press(&mut a, "2");
    a.control.setting = 0;
    press(&mut a, "→");
    assert_eq!(a.control.settings.theme.key(), "ledger-light");
    assert_eq!(theme::active().key(), "ledger-light");
    press(&mut a, "←");
    assert_eq!(a.control.settings.theme.key(), "ledger-dark");
    a.control.setting = 2;
    let on = a.hints.on;
    press(&mut a, "Enter");
    assert_eq!(a.hints.on, !on);
    let saved = crate::config::Settings::load(Some(&file));
    assert_eq!((saved.hints, saved.vim, saved.autocomplete, saved.theme.key()), (!on, true, false, "ledger-dark"));
    assert!(a.control.file_problems.is_empty());
    drop(a);
    theme::set_active(Theme::default());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn edits_to_settings_toml_apply_and_problems_show() {
    let (mut a, d) = app("watch");
    let file = d.join("settings.toml");
    a.control.store = Some(file.clone());
    a.control.read_file_state();
    assert!(!a.control.file_exists);
    std::fs::write(&file, "autocomplete = false\ntheme = \"neon\"\n").unwrap();
    a.control.checked = None;
    assert!(a.control_tick());
    assert!(!a.lsp.auto, "applied without a restart");
    assert_eq!(a.control.file_problems.len(), 1, "{:?}", a.control.file_problems);
    assert!(a.control.file_problems[0].contains("theme"));
    let s = tiles::build_one(&a, TileId::Settings);
    assert!(s.summary.contains("1 problem"), "{}", s.summary);
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn tiles_show_repos_agents_servers_and_the_editor() {
    let (mut a, d) = app("tiles");
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "T"], &["config", "user.email", "t@example.com"], &["config", "commit.gpgsign", "false"], &["add", "."], &["commit", "-qm", "first"]] {
        git(&d, args);
    }
    std::fs::write(d.join("new.txt"), "x").unwrap();
    press(&mut a, "Alt+O");
    settle(&mut a);
    let w = tiles::build_one(&a, TileId::Workspace);
    assert!(w.rows[0].left.contains("⎇ main") && w.rows[0].right.contains("1 new"), "{:?}", w.rows[0]);
    assert_eq!(w.rows[0].act, Act::GitView(d.clone()));
    // Agents: a running, a failed and a finished run of this project.
    let mut r1 = crate::agents::run::Run::new(1, d.clone(), "first job");
    r1.state = crate::agents::run::State::Failed;
    r1.updated = crate::agents::run::now_secs();
    r1.cost = 0.25;
    let mut r2 = crate::agents::run::Run::new(2, d.clone(), "second job");
    r2.state = crate::agents::run::State::Done;
    r2.updated = crate::agents::run::now_secs();
    r2.cost = 0.5;
    a.agents.runs = vec![r1, r2];
    let ag = tiles::build_one(&a, TileId::Agents);
    assert!(ag.rows[0].right.contains("$0.75 today"), "{:?}", ag.rows[0]);
    assert!(ag.rows[1].left.contains("1 need attention") && ag.rows[1].act == Act::Mission(Some(1)), "{:?}", ag.rows[1]);
    assert!(ag.rows.iter().any(|r| r.left == "#2 second job" && r.right.contains("done")));
    assert!(ag.rows.iter().any(|r| r.head && r.left == "AA STAFF"));
    // Enter on the attention row: the agents panel with that run.
    a.control.tile = 1;
    a.control.rows[1] = 1;
    press(&mut a, "Enter");
    assert!(a.agents.visible && a.agents.selected == Some(1) && !a.control.open);
    // Servers: one row per language, from the probe.
    let sv = tiles::build_one(&a, TileId::Servers);
    assert_eq!(sv.rows.len(), crate::lsp::config::get().langs.len());
    // Editor: unsaved files and processes.
    a.docs[0].type_char('x');
    let ed = tiles::build_one(&a, TileId::Editor);
    assert!(ed.rows.iter().any(|r| r.left == "unsaved" && r.act == Act::SaveAll), "{:?}", ed.rows);
    assert!(ed.rows.iter().any(|r| r.left.starts_with("catlas")));
    assert!(ed.summary.contains("1 unsaved"));
    a.save_all();
    assert!(!a.docs.iter().any(|d| d.dirty()));
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn draws_at_80_and_200_columns() {
    let (mut a, d) = app("draw");
    press(&mut a, "Alt+O");
    settle(&mut a);
    // 80 columns: one column of tiles, the selected one in view, everything inside the width.
    let (_, text) = render(&mut a, 80, 24);
    assert!(text.lines().next().unwrap().contains("CONTROL CENTER"), "{text}");
    assert!(text.contains("WORKSPACE") && text.contains("1 Overview"), "{text}");
    assert_eq!(ui::columns(80), 1);
    a.control.tile = 5; // settings: the view scrolls down to it
    let (_, text) = render(&mut a, 80, 24);
    assert!(text.contains("SETTINGS") && text.contains("Autocomplete"), "{text}");
    assert!(!text.contains("WORKSPACE"), "scrolled past the first tile\n{text}");
    // 200 columns: three columns, every tile at once.
    a.control.tile = 0;
    let (buf, text) = render(&mut a, 200, 50);
    for title in ["WORKSPACE", "AGENTS", "LANGUAGE SERVERS", "INDEX", "EDITOR", "SETTINGS"] {
        assert!(text.contains(title), "{title} missing at 200 columns\n{text}");
    }
    let row = text.lines().find(|l| l.contains("WORKSPACE")).unwrap();
    assert!(row.contains("AGENTS") && row.contains("LANGUAGE SERVERS"), "three in a row: {row}");
    assert_eq!(a.control.tile_rects.len(), 6);
    assert_eq!(buf[(a.control.tile_rects[0].x, a.control.tile_rects[0].y)].fg, t::ACCENT, "the selected tile's border");
    // The settings screen with the file preview at 200 columns.
    press(&mut a, "2");
    let (_, text) = render(&mut a, 200, 50);
    assert!(text.contains("Vim mode") && text.contains("settings.toml") && text.contains("# Theme"), "{text}");
    let (_, text) = render(&mut a, 80, 24);
    assert!(text.contains("Vim mode") && !text.contains("# Theme"), "no preview at 80\n{text}");
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_theme_switch_recolours_everything() {
    let (mut a, d) = app("theme");
    let (buf, _) = render(&mut a, 80, 24);
    assert_eq!(buf[(40, 10)].bg, t::PAPER);
    press(&mut a, "Ctrl+R");
    typ(&mut a, "light theme");
    assert_eq!(a.palette.hits()[0].action.id, "theme.light");
    press(&mut a, "Enter");
    assert_eq!(a.control.settings.theme.key(), "ledger-light");
    let (buf, _) = render(&mut a, 80, 24);
    assert_eq!(buf[(40, 10)].bg, ratatui::style::Color::Rgb(0xf6, 0xf7, 0xf9), "light paper");
    assert_eq!(buf[(1, 0)].fg, ratatui::style::Color::Rgb(0x1d, 0x5f, 0xb8), "light accent on the brand");
    a.set_theme_key("high-contrast");
    let (buf, _) = render(&mut a, 80, 24);
    assert_eq!(buf[(40, 10)].bg, ratatui::style::Color::Rgb(0, 0, 0));
    a.set_theme_key("ledger-dark");
    let (buf, _) = render(&mut a, 80, 24);
    assert_eq!(buf[(40, 10)].bg, t::PAPER);
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}
