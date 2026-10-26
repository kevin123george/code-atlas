//! Shortcut helper tests: label parsing, fallback choice, placement, drawing, tips, context
//! selection, and the binding check: every key in `table.rs` is fed into a fresh `App` in its
//! context and must change something there (so the helper can't drift from the real bindings).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use super::table::{self, Ctx, Hint, ALL};
use super::ui::{lines, place, size};
use super::*;
use crate::app::{App, Prompt, PromptKind};
use crate::doc::Clip;
use crate::project::{canonical, PopupKind};
use crate::ui::theme;

// ---- labels and fallbacks ------------------------------------------------------------------

fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, m)
}

#[test]
fn labels_parse_into_the_keys_crossterm_reports() {
    use KeyModifiers as M;
    let one = |l: &str| parse(l).unwrap().into_iter().map(|s| s.into_iter().map(|e| (e.code, e.modifiers)).collect::<Vec<_>>()).collect::<Vec<_>>();
    assert_eq!(one("Ctrl+Shift+N"), vec![vec![(KeyCode::Char('N'), M::CONTROL | M::SHIFT)]]);
    assert_eq!(one("Ctrl+P"), vec![vec![(KeyCode::Char('p'), M::CONTROL)]]);
    assert_eq!(one("Alt+←/→"), vec![vec![(KeyCode::Left, M::ALT)], vec![(KeyCode::Right, M::ALT)]]);
    assert_eq!(one("Ctrl+C/X")[1], vec![(KeyCode::Char('x'), M::CONTROL)]);
    assert_eq!(one("Alt+1/F12")[1], vec![(KeyCode::F(12), M::ALT)]);
    assert_eq!(one("F7/Shift+F7")[1], vec![(KeyCode::F(7), M::SHIFT)]);
    assert_eq!(one("Ctrl+/"), vec![vec![(KeyCode::Char('/'), M::CONTROL)]]);
    assert_eq!(one("/"), vec![vec![(KeyCode::Char('/'), M::NONE)]]);
    assert_eq!(one(">/<"), vec![vec![(KeyCode::Char('>'), M::NONE)], vec![(KeyCode::Char('<'), M::NONE)]]);
    assert_eq!(one("Tab/Shift+Tab")[1], vec![(KeyCode::BackTab, M::SHIFT)]);
    assert_eq!(one("v/V")[1], vec![(KeyCode::Char('V'), M::SHIFT)]);
    assert_eq!(one("ciw")[0].len(), 3);
    assert_eq!(one(":w/:q")[1], vec![(KeyCode::Char(':'), M::NONE), (KeyCode::Char('q'), M::NONE)]);
    assert_eq!(one("Ctrl+Alt+↑"), vec![vec![(KeyCode::Up, M::CONTROL | M::ALT)]]);
    assert_eq!(one("Shift+PgUp/PgDn")[1], vec![(KeyCode::PageDown, M::SHIFT)]);
    assert_eq!(one("Space"), vec![vec![(KeyCode::Char(' '), M::NONE)]]);
    assert_eq!(one("Ctrl+(Shift+)Z"), vec![vec![(KeyCode::Char('z'), M::CONTROL)], vec![(KeyCode::Char('Z'), M::CONTROL | M::SHIFT)]]);
    assert!(matches("Ctrl+F/G", &k(KeyCode::Char('g'), M::CONTROL)));
    assert!(!matches("Ctrl+F/G", &k(KeyCode::Char('g'), M::ALT)));
}

#[test]
fn every_label_in_the_table_parses_and_fits() {
    for ctx in ALL {
        let rows = table::rows(ctx);
        assert!(!rows.is_empty() && rows.len() <= 8, "{ctx:?}: {} rows", rows.len());
        for h in rows.iter().chain(table::FALLBACKS) {
            for l in [h.key].into_iter().chain(h.legacy) {
                assert!(parse(l).is_some(), "{ctx:?}: can't parse {l:?}");
            }
        }
        for kitty in [true, false] {
            let ls = lines(rows, kitty, None);
            let (w, h, key_w) = size(&ls, table::title(ctx));
            assert!((ui::MIN_W..=ui::MAX_W).contains(&w), "{ctx:?}: width {w}");
            assert!(h <= 9, "{ctx:?}: {h} rows");
            let need = 2 + key_w as usize + 2 + ls.iter().map(|l| unicode_width::UnicodeWidthStr::width(l.does.as_str())).max().unwrap() + 1;
            assert!(need <= w as usize, "{ctx:?} (kitty {kitty}): text needs {need} columns, panel has {w}");
        }
    }
}

#[test]
fn fallbacks_show_the_key_that_works_here() {
    let goto = table::rows(Ctx::Idea)[0];
    assert_eq!(goto.does, "go to file");
    assert_eq!(goto.shown(true), "Ctrl+Shift+N");
    assert_eq!(goto.shown(false), "Ctrl+P");
    let commit = table::rows(Ctx::Commit).iter().find(|h| h.does == "commit ticked").unwrap();
    assert_eq!((commit.shown(true), commit.shown(false)), ("Ctrl+Enter", "Alt+Enter"));
    // Keys every terminal sends stay the same.
    let complete = table::rows(Ctx::Idea)[2];
    assert_eq!((complete.shown(true), complete.shown(false)), ("Ctrl+Space/J", "Ctrl+Space/J"), "macOS takes Ctrl+Space in every terminal");
    let ls = lines(table::rows(Ctx::Idea), false, None);
    assert!(ls.iter().any(|l| l.key == "Ctrl+P") && !ls.iter().any(|l| l.key.contains("Shift+N")));
    let ls = lines(table::rows(Ctx::Idea), true, None);
    assert!(ls.iter().any(|l| l.key == "Ctrl+Shift+N"));
}

#[test]
fn toggle_key_is_alt_slash() {
    assert!(is_toggle(&k(KeyCode::Char('/'), KeyModifiers::ALT)));
    assert!(is_toggle(&k(KeyCode::Char('7'), KeyModifiers::ALT | KeyModifiers::SHIFT)), "German layout");
    assert!(!is_toggle(&k(KeyCode::Char('/'), KeyModifiers::CONTROL)), "Ctrl+/ comments");
    assert!(!is_toggle(&k(KeyCode::Char('/'), KeyModifiers::CONTROL | KeyModifiers::ALT)));
    assert!(!is_toggle(&k(KeyCode::Char('/'), KeyModifiers::NONE)), "Vim search");
    // Nothing in the table uses it.
    for ctx in ALL {
        for h in table::rows(ctx) {
            for l in [h.key].into_iter().chain(h.legacy) {
                for seq in parse(l).unwrap() {
                    assert!(!seq.iter().any(is_toggle), "{ctx:?} {l} clashes with the toggle");
                }
            }
        }
    }
}

// ---- placement and tips --------------------------------------------------------------------

#[test]
fn placement_bottom_right_moves_up_when_the_cursor_is_under_it() {
    let area = Rect::new(30, 1, 100, 25); // editor area right of a 30-column sidebar
    let r = place(area, 34, 9, Some(3)).unwrap();
    assert_eq!(r, Rect::new(30 + 100 - 34 - 1, 1 + 25 - 9, 34, 9), "bottom right, one column from the edge");
    // Cursor line on the panel's first and last rows: top right instead.
    for row in [r.y, r.bottom() - 1] {
        assert_eq!(place(area, 34, 9, Some(row)).unwrap(), Rect::new(r.x, 1, 34, 9));
    }
    assert_eq!(place(area, 34, 9, Some(r.y - 1)).unwrap(), r, "the line above is fine");
    assert_eq!(place(area, 34, 9, None).unwrap(), r);
    // Too small: under both corners, or no room.
    assert_eq!(place(Rect::new(0, 0, 80, 12), 34, 9, Some(5)), None);
    assert_eq!(place(Rect::new(0, 0, 40, 30), 34, 9, None), None);
    assert_eq!(place(Rect::new(0, 0, 80, 9), 34, 9, None), None);
}

#[test]
fn tips_last_three_seconds_and_dont_nag() {
    let mut h = Hints { on: true, kitty: true, store: None, tip: None, tipped: Default::default(), rect: Rect::default() };
    let t0 = Instant::now();
    h.promote(table::TAB_SWITCH, t0);
    assert_eq!(h.current_tip(t0 + Duration::from_secs(2)), Some(table::TAB_SWITCH));
    assert_eq!(h.current_tip(t0 + Duration::from_secs(4)), None);
    h.tip = None;
    h.promote(table::TAB_SWITCH, t0 + Duration::from_secs(10));
    assert_eq!(h.tip, None, "the same tip waits 30 s");
    h.promote(table::TAB_SWITCH, t0 + Duration::from_secs(40));
    assert!(h.tip.is_some());
    h.on = false;
    h.tip = None;
    h.promote(table::SCROLL, t0);
    assert_eq!(h.tip, None, "no tips while the helper is off");
    // A tip goes first and keeps the panel at 8 rows.
    let ls = lines(table::rows(Ctx::Idea), false, Some(table::TAB_SWITCH));
    assert!(ls[0].tip && ls[0].key == "Alt+←/→" && ls.len() == 8);
}

// ---- fixtures: a scratch project with a git repo ---------------------------------------------

static ENV: Once = Once::new();
static N: AtomicUsize = AtomicUsize::new(0);

/// Test runs never touch the user's config, clipboard or a real `claude`.
fn env() {
    ENV.call_once(|| {
        let cfg = std::env::temp_dir().join(format!("catlas-hints-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&cfg).unwrap();
        std::env::set_var("CATLAS_CONFIG_DIR", &cfg);
        std::env::set_var("CATLAS_CLAUDE", "true");
    });
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

const A_HEAD: &str = "one\n    two (x, y)\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\nfourteen\nfifteen\nsixteen\nseventeen\neighteen\nnineteen\ntwenty\n";

/// A project with a.txt, b.txt, sub/c.txt committed, then a.txt changed in two places (two
/// hunks) and b.txt changed: the Commit panel has two files.
fn project() -> PathBuf {
    env();
    let d = std::env::temp_dir().join(format!("catlas-hints-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let d = canonical(&d);
    std::fs::write(d.join("a.txt"), A_HEAD).unwrap();
    std::fs::write(d.join("b.txt"), "alpha\nbeta\n").unwrap();
    std::fs::write(d.join("sub/c.txt"), "gamma\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "Test"], &["config", "user.email", "t@example.com"], &["config", "commit.gpgsign", "false"], &["add", "."], &["commit", "-q", "-m", "first"]] {
        git(&d, args);
    }
    std::fs::write(d.join("a.txt"), A_HEAD.replace("three\n", "THREE\n").replace("eighteen\n", "EIGHTEEN\n")).unwrap();
    std::fs::write(d.join("b.txt"), "alpha\nbeta\ngamma\n").unwrap();
    d
}

fn app_in(d: &Path) -> App {
    let mut app = App::new(vec![d.to_path_buf(), d.join("a.txt"), d.join("b.txt")]).unwrap();
    app.active = 0;
    app.message = None;
    app
}

fn press(app: &mut App, k: KeyEvent) {
    app.handle(Event::Key(k));
}

fn keys(app: &mut App, label: &str) {
    for e in parse(label).unwrap().remove(0) {
        press(app, e);
    }
}

fn tick(app: &mut App) {
    app.tick();
    let _ = app.dock_tick();
}

fn wait(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
    let t = Instant::now();
    while !until(app) {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        tick(app);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Everything a key could change that the tests look at (not the terminal screen: shell output
/// comes on its own).
fn fingerprint(app: &App) -> String {
    use crate::lsp::popup::Popup;
    let docs: Vec<String> = app.docs.iter().map(|d| format!("{:?}|{}|{:?}|{}|{}", d.path, d.rope, d.selection(), d.cursor, d.dirty())).collect();
    let p = &app.project;
    let popup = p.popup.as_ref().map(|q| format!("{:?} {:?} {} {:?} {:?}", q.kind, q.input, q.selected, p.quick.mode, p.quick.scope));
    let tree = (p.sidebar, p.focus_tree, p.tree.selected, p.tree.rows.len(), p.tree.speed.clone());
    let a = &app.agents;
    let agents = format!("{} {} {:?} {} {} {:?} {:?} {} {} {}", a.visible, a.focus, a.area, a.mode_idx, a.include_ctx, a.input.text, a.selected, a.runs.len(), a.scroll, a.follow);
    let dk = &app.dock;
    let g = &dk.git;
    let dock = format!(
        "{} {} {:?} {} {} {} {} {:?} {:?} {:?}",
        dk.open,
        dk.focused,
        dk.tab,
        dk.height,
        g.entries.len(),
        g.panel.cursor,
        g.panel.checked.len(),
        g.panel.message,
        g.panel.field,
        g.diff.as_ref().map(|v| (v.path.clone(), v.scroll))
    );
    let term = dk.term.as_ref().map(|t| t.screen().scrollback());
    let vim = app.vim.as_ref().map(|v| format!("{:?} {:?} {:?} {:?}", v.mode.label(), v.cmdline(), v.pending(), v.visual_range(app.doc())));
    let lsp = match &app.lsp.popup {
        None => String::new(),
        Some(Popup::Completion { sel, shown, .. }) => format!("completion {sel} {}", shown.len()),
        Some(Popup::Picker(pk)) => format!("picker {} {:?}", pk.sel, pk.query),
        Some(Popup::Actions { sel, .. }) => format!("actions {sel}"),
        Some(Popup::Hover { scroll, .. }) => format!("hover {scroll}"),
    };
    let prompt = app.prompt.as_ref().map(|p| format!("{} {:?}", p.kind.label(), p.input));
    let gv = &app.git_view;
    let git_view = format!(
        "{} {:?} {:?} {} {} {} {:?} {:?} {:?} {:?} {} {:?} {:?} {:?} {} {}",
        gv.open,
        gv.tab,
        gv.scope_label(),
        gv.review.cursor,
        gv.review.rows.len(),
        gv.review.sbs,
        gv.sbs_pref,
        gv.confirm.as_ref().map(|c| c.question.clone()),
        gv.input.as_ref().map(|i| i.text.clone()),
        gv.branches.as_ref().map(|b| (b.sel, b.list.len())),
        gv.log.sel,
        gv.log.path,
        gv.log.filter,
        gv.details.as_ref().map(|d| (d.cursor, d.rows.len())),
        gv.undo.len(),
        gv.blames.on.len()
    );
    format!(
        "active {} docs {docs:?}\nprompt {prompt:?} msg {:?} quit {} clip {:?}\nproject {popup:?} {tree:?}\nagents {agents}\ndock {dock} term {term:?}\nvim {vim:?} lsp {lsp} hints {}\ngit view {git_view}",
        app.active, app.message, app.quit, app.clip.text, app.hints.on
    )
}

/// Lets background work (git status, file index, searches) finish so it doesn't count as a
/// key's effect.
fn settle(app: &mut App) {
    wait(app, "git status", |a| a.dock.git.root.is_some());
    let mut last = fingerprint(app);
    let mut same = 0;
    while same < 3 {
        std::thread::sleep(Duration::from_millis(10));
        tick(app);
        let now = fingerprint(app);
        same = if now == last { same + 1 } else { 0 };
        last = now;
    }
}

// ---- contexts --------------------------------------------------------------------------------

type Setup = fn(&mut App);

fn vim_at(app: &mut App, line: usize, col: usize) {
    app.docs[0].goto_line(line, col);
    app.set_vim(true);
    app.clip = Clip { text: "zz\n".into(), whole_line: true };
}

/// One or more ways to get into each context; a key counts as handled when it changes something
/// in at least one of them (↑ needs a row above, redo needs an undo, ...).
fn setups(ctx: Ctx) -> Vec<Setup> {
    match ctx {
        Ctx::Idea => vec![
            |_| {},
            |app| keys(app, "x"),
            |app| {
                keys(app, "x");
                keys(app, "Ctrl+Z");
            },
        ],
        Ctx::Selection => vec![|app| {
            let d = &mut app.docs[0];
            d.select(d.line_start(1), d.line_end(1));
        }],
        Ctx::VimNormal => vec![
            |app| vim_at(app, 1, 4),
            |app| {
                vim_at(app, 1, 4);
                keys(app, "x");
            },
            |app| {
                vim_at(app, 1, 4);
                keys(app, "x");
                keys(app, "u");
            },
        ],
        Ctx::VimInsert => vec![|app| {
            vim_at(app, 1, 4);
            keys(app, "i");
        }],
        Ctx::VimVisual => vec![
            |app| {
                vim_at(app, 1, 4);
                keys(app, "vl");
            },
            |app| {
                vim_at(app, 1, 9); // on the x in "(x)"
                keys(app, "v");
            },
        ],
        Ctx::VimCommand => vec![|app| {
            vim_at(app, 0, 0);
            keys(app, ":3");
        }],
        Ctx::Tree => vec![
            |app| {
                let c = app.project.root.join("sub/c.txt");
                assert!(app.project.tree.reveal(&c));
                app.project.focus_tree = true;
            },
            |app| {
                app.project.focus_tree = true;
                app.project.tree.selected = 0;
            },
        ],
        Ctx::TreeSearch => vec![|app| {
            app.project.focus_tree = true;
            app.project.tree.expand(app.project.tree.rows.iter().position(|n| n.name == "sub").unwrap());
            keys(app, "txt");
        }],
        Ctx::QuickSearch => vec![|app| {
            app.project.open_quick(Some("ee".into()), None);
            wait(app, "search hits", |a| a.project.hits.len() >= 3 && !a.project.searching());
            app.project.popup.as_mut().unwrap().selected = 1;
        }],
        Ctx::GotoFile => vec![|app| {
            keys(app, "Ctrl+P");
            keys(app, "txt");
            wait(app, "the file index", |a| a.project.matches.len() >= 3);
            app.project.popup.as_mut().unwrap().selected = 1;
        }],
        Ctx::Recent => vec![
            |app| keys(app, "Ctrl+E"),
            |app| {
                keys(app, "Ctrl+E");
                app.project.popup.as_mut().unwrap().selected = 0;
            },
        ],
        Ctx::FindInFiles => vec![|app| {
            app.project.open_popup(PopupKind::Search, Some("ee".into()));
            wait(app, "search hits", |a| a.project.hits.len() >= 3);
            app.project.popup.as_mut().unwrap().selected = 1;
        }],
        Ctx::Terminal => vec![
            |app| {
                keys(app, "Alt+F12");
                app.dock.term.as_mut().expect("a shell").feed("line\r\n".repeat(80).as_bytes());
            },
            |app| {
                keys(app, "Alt+F12");
                let t = app.dock.term.as_mut().expect("a shell");
                t.feed("line\r\n".repeat(80).as_bytes());
                t.scroll(5);
            },
        ],
        Ctx::Commit => vec![|app| {
            keys(app, "Alt+0");
            wait(app, "two changed files", |a| a.dock.git.entries.len() == 2);
        }],
        Ctx::CommitMessage => vec![|app| {
            keys(app, "Alt+0");
            wait(app, "two changed files", |a| a.dock.git.entries.len() == 2);
            keys(app, "wip");
        }],
        Ctx::Diff => vec![
            |app| {
                keys(app, "Alt+0");
                wait(app, "two changed files", |a| a.dock.git.entries.len() == 2);
                keys(app, "Enter");
                wait(app, "the diff", |a| a.dock.git.diff.is_some());
            },
            |app| {
                keys(app, "Alt+0");
                wait(app, "two changed files", |a| a.dock.git.entries.len() == 2);
                keys(app, "Enter");
                wait(app, "the diff", |a| a.dock.git.diff.is_some());
                app.dock.git.diff.as_mut().unwrap().scroll = 2;
            },
            |app| {
                keys(app, "Alt+0");
                wait(app, "two changed files", |a| a.dock.git.entries.len() == 2);
                keys(app, "↓");
                keys(app, "Enter");
                wait(app, "the second file's diff", |a| a.dock.git.diff.is_some());
            },
        ],
        Ctx::AgentsPrompt => vec![|app| {
            keys(app, "Alt+9");
            let root = app.project.root.clone();
            app.agents.runs = vec![crate::agents::run::Run::new(1, root, "first")];
            app.agents.selected = Some(1); // a follow-up: Ctrl+N goes back to a new agent
            app.agents.input.insert_str("hello");
        }],
        Ctx::AgentsRuns => vec![|app| {
            keys(app, "Alt+9");
            let root = app.project.root.clone();
            app.agents.runs = vec![crate::agents::run::Run::new(1, root.clone(), "first"), crate::agents::run::Run::new(2, root, "second")];
            app.agents.selected = Some(2);
            app.agents.area = crate::agents::Area::Runs;
        }],
        Ctx::AgentsTranscript => vec![|app| {
            keys(app, "Alt+9");
            let a = &mut app.agents;
            a.area = crate::agents::Area::Transcript;
            a.max_scroll = 10;
            a.scroll = 5;
            a.follow = false;
        }],
        Ctx::Completion => vec![
            |app| completion(app, 0),
            |app| completion(app, 1),
        ],
        Ctx::List => vec![|app| {
            use crate::lsp::popup::{PickItem, PickKind, Picker, Popup};
            let path = app.project.root.join("a.txt");
            let item = |label: &str, line| PickItem { kind: "E", label: label.into(), detail: String::new(), path: path.clone(), pos: lsp_types::Position { line, character: 0 }, depth: 0 };
            let mut p = Picker::new(PickKind::Problems, "Problems".into(), vec![item("first", 2), item("second", 5), item("third", 9)]);
            p.sel = 1;
            app.lsp.popup = Some(Popup::Picker(p));
        }],
        Ctx::Prompt => vec![|app| app.prompt = Some(Prompt { kind: PromptKind::Find, input: "five".into() })],
        Ctx::GitChanges => vec![
            |app| git_view(app),
            |app| {
                git_view(app);
                keys(app, "F7");
            },
        ],
        Ctx::GitLog => vec![
            |app| git_log(app),
            |app| {
                git_log(app);
                keys(app, "↓");
            },
        ],
        Ctx::GitCommit => vec![
            |app| git_commit(app),
            |app| {
                git_commit(app);
                keys(app, "F7");
            },
        ],
        Ctx::GitBranches => vec![
            |app| git_branches(app, false),
            |app| git_branches(app, true),
        ],
        Ctx::GitConfirm => vec![|app| {
            git_view(app);
            keys(app, "r");
        }],
        Ctx::GitInput => vec![|app| {
            git_log(app);
            keys(app, "/");
            keys(app, "fi");
        }],
        Ctx::Blame => vec![|app| {
            let path = app.docs[0].path.clone().unwrap();
            assert!(app.git_view.blames.toggle(&path));
            let rev = app.docs[0].rev();
            wait(app, "blame", |a| a.git_view.blames.get(Some(&path)).is_some_and(|b| b.rev == Some(rev)));
        }],
    }
}

/// The Git view open on the Changes review (a.txt with two hunks, b.txt).
fn git_view(app: &mut App) {
    keys(app, "Ctrl+L");
    assert!(app.git_view.open, "{:?}", app.message);
    wait(app, "the review", |a| a.git_view.review.loaded && !a.git_view.review.busy() && a.git_view.review.model.files.len() == 2);
}

/// The Log tab with two commits (the changes committed as "second").
fn git_log(app: &mut App) {
    let root = app.project.root.clone();
    git(&root, &["commit", "-qam", "second"]);
    keys(app, "Ctrl+L");
    keys(app, "2");
    wait(app, "the log", |a| a.git_view.log.loaded && a.git_view.log.shown.len() == 2);
}

/// Details of the "second" commit (two files, three hunks).
fn git_commit(app: &mut App) {
    git_log(app);
    keys(app, "Enter");
    wait(app, "commit details", |a| a.git_view.details.as_ref().is_some_and(|d| d.loaded && !d.busy()));
}

fn git_branches(app: &mut App, other: bool) {
    let root = app.project.root.clone();
    git(&root, &["branch", "side"]);
    git_view(app);
    keys(app, "B");
    let b = app.git_view.branches.as_mut().expect("branches popup");
    assert_eq!(b.list.len(), 2);
    if other {
        b.sel = 1 - b.sel;
    }
}

fn completion(app: &mut App, sel: usize) {
    use crate::lsp::popup::{Item, Popup};
    let item = |l: &str| Item::from_lsp(lsp_types::CompletionItem { label: l.into(), ..Default::default() });
    app.docs[0].goto_line(0, 3); // after "one"
    app.lsp.popup = Some(Popup::Completion {
        items: vec![item("onerous"), item("oneself")],
        shown: vec![0, 1],
        sel,
        start: 0,
        incomplete: false,
        prefix: "one".into(),
        req: lsp_types::Position { line: 0, character: 3 },
    });
}

/// A key no context binds: what any stray key does (the terminal scrolls to the bottom and
/// sends it to the shell, the agents panel shows its hint, ...).
const STRAY: KeyEvent = KeyEvent { code: KeyCode::F(11), modifiers: KeyModifiers::NONE, kind: crossterm::event::KeyEventKind::Press, state: crossterm::event::KeyEventState::NONE };

/// Feeds `seq` into a fresh app set up by `setup` and returns (before, after), with the scratch
/// folder's path taken out so results of different apps compare. Waits until `done(before,
/// after)` or `max`.
fn trial(ctx: Ctx, setup: Setup, seq: &[KeyEvent], done: &dyn Fn(&str, &str) -> bool, max: Duration) -> (String, String) {
    let d = project();
    let norm = |s: String| s.replace(&d.display().to_string(), "<root>");
    let mut app = app_in(&d);
    setup(&mut app);
    settle(&mut app);
    assert_eq!(context(&app), ctx, "setup for {ctx:?} landed elsewhere");
    app.message = None;
    let before = norm(fingerprint(&app));
    for e in seq {
        press(&mut app, *e);
    }
    let t = Instant::now();
    let mut after = norm(fingerprint(&app));
    while !done(&before, &after) && t.elapsed() < max {
        std::thread::sleep(Duration::from_millis(10));
        tick(&mut app);
        after = norm(fingerprint(&app));
    }
    drop(app);
    let _ = std::fs::remove_dir_all(&d);
    (before, after)
}

/// Every key of every row (and its fallback) does something in its context, beyond what a
/// stray key does there; Alt+/ toggles the helper there.
fn check_bindings(ctx: Ctx) {
    let setups = setups(ctx);
    let mut labels: Vec<&str> = Vec::new();
    for h in table::rows(ctx) {
        labels.push(h.key);
        labels.extend(h.legacy);
    }
    if ctx == Ctx::Idea {
        // The Key Promoter's keys too.
        let tips: [Hint; 7] = [table::TAB_SWITCH, table::TREE_OPEN, table::TREE_FOCUS, table::SCROLL, table::DOCK_TERMINAL, table::DOCK_COMMIT, table::AGENTS_FOCUS];
        for h in tips.iter().chain(table::FALLBACKS) {
            labels.push(h.key);
            labels.extend(h.legacy);
        }
    }
    let strays: Vec<String> = setups.iter().map(|s| trial(ctx, *s, &[STRAY], &|_, _| false, Duration::from_millis(150)).1).collect();
    let mut missing = Vec::new();
    for label in labels {
        for mut seq in parse(label).unwrap() {
            if label.starts_with(':') {
                seq.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // a Vim command runs on Enter
            }
            let works = setups.iter().zip(&strays).any(|(s, stray)| {
                let (before, after) = trial(ctx, *s, &seq, &|b, a| a != b && a != stray.as_str(), Duration::from_millis(1500));
                after != before && after != *stray
            });
            if !works {
                missing.push(format!("{label} ({:?})", seq.iter().map(|e| (e.code, e.modifiers)).collect::<Vec<_>>()));
            }
        }
    }
    assert!(missing.is_empty(), "{ctx:?}: keys in the hint table that do nothing there: {missing:?}");
    // The toggle works in this context.
    let d = project();
    let mut app = app_in(&d);
    setups[0](&mut app);
    assert!(app.hints.on);
    press(&mut app, k(KeyCode::Char('/'), KeyModifiers::ALT));
    assert!(!app.hints.on, "{ctx:?}: Alt+/ didn't hide the helper");
    drop(app);
    let _ = std::fs::remove_dir_all(&d);
}

macro_rules! binding_tests {
    ($($name:ident => $ctx:ident),* $(,)?) => {
        $(#[test] fn $name() { check_bindings(Ctx::$ctx); })*
        /// The macro list covers every context.
        #[test]
        fn binding_tests_cover_every_context() {
            let covered = [$(Ctx::$ctx),*];
            for c in ALL {
                assert!(covered.contains(&c), "no binding test for {c:?}");
            }
        }
    };
}

binding_tests! {
    keys_idea => Idea,
    keys_selection => Selection,
    keys_vim_normal => VimNormal,
    keys_vim_insert => VimInsert,
    keys_vim_visual => VimVisual,
    keys_vim_command => VimCommand,
    keys_tree => Tree,
    keys_tree_search => TreeSearch,
    keys_quick_search => QuickSearch,
    keys_goto_file => GotoFile,
    keys_recent => Recent,
    keys_find_in_files => FindInFiles,
    keys_terminal => Terminal,
    keys_commit => Commit,
    keys_commit_message => CommitMessage,
    keys_diff => Diff,
    keys_agents_prompt => AgentsPrompt,
    keys_agents_runs => AgentsRuns,
    keys_agents_transcript => AgentsTranscript,
    keys_completion => Completion,
    keys_list => List,
    keys_prompt => Prompt,
    keys_git_changes => GitChanges,
    keys_git_log => GitLog,
    keys_git_commit => GitCommit,
    keys_git_branches => GitBranches,
    keys_git_confirm => GitConfirm,
    keys_git_input => GitInput,
    keys_blame => Blame,
}

// ---- context selection and drawing in a real App ---------------------------------------------

#[test]
fn context_follows_the_focus() {
    let d = project();
    let mut app = app_in(&d);
    settle(&mut app);
    assert_eq!(context(&app), Ctx::Idea);
    assert_eq!(title(&app, Ctx::Idea), "editor · IDEA");
    keys(&mut app, "Shift+→");
    assert_eq!(context(&app), Ctx::Selection);
    keys(&mut app, "Esc");
    keys(&mut app, "Alt+F12");
    assert_eq!(context(&app), Ctx::Terminal);
    keys(&mut app, "Ctrl+Alt+↑");
    assert_eq!(context(&app), Ctx::Idea, "back in the editor, the dock stays open");
    keys(&mut app, "Alt+1");
    assert_eq!(context(&app), Ctx::Tree);
    keys(&mut app, "b");
    assert_eq!(context(&app), Ctx::TreeSearch);
    assert_eq!(title(&app, Ctx::TreeSearch), "tree · speed search");
    keys(&mut app, "Esc");
    keys(&mut app, "Ctrl+T");
    assert_eq!(context(&app), Ctx::QuickSearch);
    keys(&mut app, "Esc");
    keys(&mut app, "Esc");
    keys(&mut app, "Alt+9");
    assert_eq!(context(&app), Ctx::AgentsPrompt);
    keys(&mut app, "Esc");
    keys(&mut app, "Ctrl+G");
    assert_eq!(context(&app), Ctx::Prompt);
    assert_eq!(title(&app, Ctx::Prompt), "prompt · Go to line");
    keys(&mut app, "Esc");
    keys(&mut app, "Ctrl+Alt+V");
    assert_eq!(context(&app), Ctx::VimNormal);
    keys(&mut app, "V");
    assert_eq!(title(&app, context(&app)), "vim · V-LINE");
    keys(&mut app, "Esc");
    keys(&mut app, "i");
    assert_eq!(context(&app), Ctx::VimInsert);
    drop(app);
    let _ = std::fs::remove_dir_all(&d);
}

fn render(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut term = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::ui::draw(f, app)).unwrap();
    term.backend().buffer().clone()
}

fn row_text(buf: &ratatui::buffer::Buffer, y: u16, x0: u16, x1: u16) -> String {
    (x0..x1).map(|x| buf[(x, y)].symbol().to_string()).collect()
}

#[test]
fn draws_as_glass_and_moves_away_from_the_cursor() {
    let d = project();
    // Long lines, so there is code under the panel.
    let long: String = (1..=20).map(|i| format!("let value_{i} = compute_something_long(alpha, beta, gamma, delta, epsilon, zeta, eta);\n")).collect();
    std::fs::write(d.join("a.txt"), long).unwrap();
    let mut app = app_in(&d);
    app.project.sidebar = false;
    settle(&mut app);
    let (w, h) = (100, 30);
    let buf = render(&mut app, w, h);
    let r = app.hints.rect;
    let bottom = r;
    assert!(r.width >= ui::MIN_W && r.right() == w - 1, "{r:?}");
    assert_eq!(r.bottom(), h - 1, "just above the status bar");
    let title_row = row_text(&buf, r.y, r.x, r.right());
    assert!(title_row.contains("editor · IDEA") && title_row.contains("Alt+/"), "{title_row}");
    let all: String = (r.y..r.bottom()).map(|y| row_text(&buf, y, r.x, r.right()) + "\n").collect();
    assert!(all.contains("Ctrl+P") && all.contains("go to file") && !all.contains("Ctrl+Shift+N"), "no kitty: the fallback\n{all}");
    // See-through: no cell in the panel gets a fill of its own; code under it is kept, dimmed.
    let mut dimmed = 0;
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            let c = &buf[(x, y)];
            assert!(c.bg == theme::PAPER || c.bg == theme::SURFACE_1, "cell ({x},{y}) has bg {:?}", c.bg);
            if c.fg == theme::GHOST && c.symbol() != " " {
                dimmed += 1;
            }
        }
    }
    assert!(dimmed > 0, "the code under the panel should show through dimmed");
    let key_cell = (r.x..r.right()).map(|x| &buf[(x, r.y + 1)]).find(|c| c.symbol() == "C").unwrap();
    assert_eq!(key_cell.fg, theme::ACCENT, "keys in the accent colour");

    // With kitty the real key shows.
    app.hints.kitty = true;
    let buf = render(&mut app, w, h);
    let r = app.hints.rect;
    let all: String = (r.y..r.bottom()).map(|y| row_text(&buf, y, r.x, r.right()) + "\n").collect();
    assert!(all.contains("Ctrl+Shift+N"), "{all}");

    // Cursor on a line under the panel: it moves to the top right.
    app.docs[0].goto_line(19, 0);
    app.follow = true;
    let _ = render(&mut app, w, h);
    let cur_row = app.layout.text_y + (app.docs[0].line() - app.docs[0].scroll_line) as u16;
    let r = app.hints.rect;
    assert!((bottom.y..bottom.bottom()).contains(&cur_row), "cursor line {cur_row} would be under {bottom:?}");
    assert_eq!(r.y, 1, "top right, under the tab bar: {r:?}");
    assert!(!(r.y..r.bottom()).contains(&cur_row));

    // The terminal context, above the dock.
    keys(&mut app, "Alt+F12");
    let buf = render(&mut app, w, h);
    let r = app.hints.rect;
    assert!(r.bottom() <= app.dock.rect.y, "above the dock: {r:?} dock {:?}", app.dock.rect);
    assert!(row_text(&buf, r.y, r.x, r.right()).contains("terminal"));

    // Off: nothing drawn.
    keys(&mut app, "Alt+/");
    let _ = render(&mut app, w, h);
    assert_eq!(app.hints.rect, Rect::default());
    drop(app);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn mouse_and_fallbacks_promote_their_keys() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let d = project();
    let mut app = app_in(&d);
    settle(&mut app);
    let _ = render(&mut app, 100, 30);
    // Click the second tab.
    let (x, _) = app.tab_spans[1];
    app.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x + 1, row: 0, modifiers: KeyModifiers::NONE }));
    assert_eq!(app.active, 1);
    assert_eq!(app.hints.current_tip(Instant::now()), Some(table::TAB_SWITCH));
    let buf = render(&mut app, 100, 30);
    let r = app.hints.rect;
    assert!(row_text(&buf, r.y + 1, r.x, r.right()).contains("Tip: Alt+←/→"), "{}", row_text(&buf, r.y + 1, r.x, r.right()));
    // A fallback where the real key works (kitty on): the real key is promoted.
    app.hints.kitty = true;
    app.hints.tip = None;
    keys(&mut app, "Ctrl+P");
    assert_eq!(app.hints.current_tip(Instant::now()).map(|h| h.key), Some("Ctrl+Shift+N"));
    // Without kitty the fallback is the right key: no tip.
    keys(&mut app, "Esc");
    app.hints.kitty = false;
    app.hints.tip = None;
    keys(&mut app, "Alt+F");
    assert_eq!(app.hints.current_tip(Instant::now()), None);
    // Ctrl+J (completion) with kitty: Ctrl+Space is promoted, except on a Mac, where macOS
    // keeps Ctrl+Space for switching input sources.
    keys(&mut app, "Esc");
    app.hints.kitty = true;
    app.hints.tip = None;
    keys(&mut app, "Ctrl+J");
    let tip = app.hints.current_tip(Instant::now()).map(|h| h.key);
    assert_eq!(tip, if cfg!(target_os = "macos") { None } else { Some("Ctrl+Space") });
    drop(app);
    let _ = std::fs::remove_dir_all(&d);
}
