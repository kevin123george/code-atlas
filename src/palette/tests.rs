//! Find Action tests: the registry is sound and agrees with the shortcut helper's table,
//! ranking (recent first, fuzzy), and running an action does what its key does.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::actions::{self, Run};
use super::*;
use crate::hints::parse;
use crate::hints::table::{self, Ctx};

pub fn env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("CATLAS_CONFIG_DIR").is_none() {
            let cfg = std::env::temp_dir().join(format!("catlas-palette-cfg-{}", std::process::id()));
            std::fs::create_dir_all(&cfg).unwrap();
            std::env::set_var("CATLAS_CONFIG_DIR", &cfg);
        }
        std::env::set_var("CATLAS_CLAUDE", "true");
    });
}

/// An app on a scratch file with three lines.
pub fn app(name: &str) -> (App, std::path::PathBuf) {
    env();
    let d = std::env::temp_dir().join(format!("catlas-palette-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let d = crate::project::canonical(&d);
    std::fs::write(d.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(vec![d.clone(), d.join("a.txt")]).unwrap();
    app.project.focus_tree = false;
    app.message = None;
    (app, d)
}

pub fn press(app: &mut App, label: &str) {
    for e in parse(label).unwrap().remove(0) {
        app.handle(Event::Key(e));
    }
}

pub fn typ(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
    }
}

#[test]
fn registry_ids_are_unique_and_keys_parse() {
    let all = actions::all();
    assert!(all.len() >= 60, "{} actions", all.len());
    for (i, a) in all.iter().enumerate() {
        assert!(!all[..i].iter().any(|b| b.id == a.id), "id {} twice", a.id);
        assert!(!all[..i].iter().any(|b| b.name == a.name), "name {:?} twice", a.name);
        for l in a.key.into_iter().chain(a.legacy) {
            assert!(parse(l).is_some(), "{}: can't parse key {l:?}", a.id);
        }
        if let Run::Keys(ls) | Run::Editor(ls) = a.run {
            assert!(!ls.is_empty(), "{}", a.id);
            for l in ls {
                assert!(parse(l).is_some(), "{}: can't parse {l:?}", a.id);
            }
        }
    }
}

/// Every chord the shortcut helper shows in the editor contexts is an action in the palette.
#[test]
fn hint_table_keys_are_all_registered() {
    let norm = |e: &KeyEvent| (e.code, e.modifiers);
    let registered: Vec<(KeyCode, KeyModifiers)> = actions::all()
        .iter()
        .flat_map(|a| a.key.into_iter().chain(a.legacy))
        .flat_map(|l| parse(l).unwrap())
        .filter(|s| s.len() == 1)
        .map(|s| norm(&s[0]))
        .collect();
    let mut missing = Vec::new();
    for ctx in [Ctx::Idea, Ctx::Selection, Ctx::Blame, Ctx::VimInsert] {
        for h in table::rows(ctx) {
            for l in [h.key].into_iter().chain(h.legacy) {
                for seq in parse(l).unwrap() {
                    let e = &seq[0];
                    let chord = seq.len() == 1 && (e.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) || matches!(e.code, KeyCode::F(_)));
                    if chord && !registered.contains(&norm(e)) {
                        missing.push(format!("{ctx:?}: {l} ({})", h.does));
                    }
                }
            }
        }
    }
    assert!(missing.is_empty(), "hint rows without an action: {missing:?}");
}

#[test]
fn ranking_recent_first_then_fuzzy() {
    let recent = vec!["theme.light".to_string(), "nav.file".to_string()];
    let empty = rank("", &recent);
    assert_eq!(empty[0].action.id, "theme.light");
    assert_eq!(empty[1].action.id, "nav.file");
    assert_eq!(empty.len(), actions::all().len());
    assert_eq!(rank("dupl", &[])[0].action.id, "edit.duplicate");
    let hits = rank("contr", &[]);
    assert_eq!(hits[0].action.id, "control.open", "{:?}", hits.iter().take(3).map(|h| h.action.id).collect::<Vec<_>>());
    assert_eq!(hits[0].pos, vec![0, 1, 2, 3, 4]);
    assert!(rank("grep", &[]).iter().take(3).any(|h| h.action.id == "nav.find-in-files"), "alias words count");
    let b = rank("theme", &["theme.contrast".to_string()]);
    assert_eq!(b[0].action.id, "theme.contrast", "a recent action wins a close race");
    assert!(rank("zzqx", &[]).is_empty());
}

#[test]
fn openers() {
    let k = |c, m| KeyEvent::new(KeyCode::Char(c), m);
    assert!(is_opener(&k('r', KeyModifiers::CONTROL)));
    assert!(!is_opener(&k('o', KeyModifiers::CONTROL)), "Ctrl+O is the Repos popup");
    assert!(is_opener(&k('A', KeyModifiers::CONTROL | KeyModifiers::SHIFT)));
    assert!(!is_opener(&k('a', KeyModifiers::CONTROL)), "Ctrl+A stays select all");
    assert!(!is_opener(&k('r', KeyModifiers::CONTROL | KeyModifiers::ALT)));
}

#[test]
fn running_an_action_does_what_its_key_does() {
    let (mut by_key, d1) = app("bykey");
    press(&mut by_key, "Ctrl+D");
    let (mut a, d2) = app("bypalette");
    press(&mut a, "Ctrl+R");
    assert!(a.palette.open);
    typ(&mut a, "duplicate");
    assert_eq!(a.palette.hits()[0].action.id, "edit.duplicate");
    press(&mut a, "Enter");
    assert!(!a.palette.open);
    assert_eq!(a.docs[0].rope.to_string(), by_key.docs[0].rope.to_string());
    assert_eq!(a.docs[0].rope.to_string(), "one\none\ntwo\nthree\n");
    assert_eq!(a.palette.recent[0], "edit.duplicate");
    // Esc closes without running.
    press(&mut a, "Ctrl+R");
    typ(&mut a, "dupl");
    press(&mut a, "Esc");
    assert!(!a.palette.open);
    assert_eq!(a.docs[0].rope.len_lines(), 5);
    // Editor actions take the focus from the tree first.
    a.project.sidebar = true;
    a.project.focus_tree = true;
    a.palette.show();
    typ(&mut a, "delete line");
    press(&mut a, "Enter");
    assert!(!a.project.focus_tree);
    assert_eq!(a.docs[0].rope.to_string(), "one\ntwo\nthree\n");
    // A tool window action toggles it like its key.
    let before = (a.project.sidebar, a.project.focus_tree);
    a.palette.show();
    typ(&mut a, "project tree");
    press(&mut a, "Enter");
    assert_ne!((a.project.sidebar, a.project.focus_tree), before, "Alt+1 ran");
    drop((a, by_key));
    for d in [d1, d2] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn the_terminal_keeps_ctrl_o() {
    let (mut a, d) = app("term");
    press(&mut a, "Alt+F12");
    assert!(a.dock.has_focus());
    press(&mut a, "Ctrl+R");
    assert!(!a.palette.open, "Ctrl+R goes to the shell");
    press(&mut a, "Alt+O");
    assert!(!a.control.open);
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn draws_names_keys_and_states() {
    let (mut a, d) = app("draw");
    press(&mut a, "Ctrl+R");
    typ(&mut a, "vim");
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let buf = term.backend().buffer().clone();
    let text: String = (0..30).map(|y| (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect();
    assert!(text.contains("Find Action"), "{text}");
    let row = text.lines().find(|l| l.contains("Vim mode")).unwrap_or_else(|| panic!("{text}"));
    assert!(row.contains("off") && row.contains("Ctrl+Alt+V"), "{row}");
    let pos = term.get_cursor_position().unwrap();
    assert!(text.lines().nth(pos.y as usize).unwrap().contains("› vim"), "cursor on the input line");
    drop(a);
    let _ = std::fs::remove_dir_all(d);
}
