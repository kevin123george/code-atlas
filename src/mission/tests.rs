//! Mission control in a real `App` with a fake `claude` (a shell script printing stream-json):
//! two agents in their own worktrees, landing one, discarding the other, asking from a
//! selection, notifications and drawing. Never a real Claude run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::git::run as git;

fn scratch(name: &str) -> PathBuf {
    let d = crate::git::tests::scratch_repo(name);
    let _ = std::fs::remove_dir_all(worktree::wt_dir(&d));
    d
}

fn cleanup(d: &Path) {
    let _ = std::fs::remove_dir_all(worktree::wt_dir(d));
    let _ = std::fs::remove_dir_all(d.with_extension("log"));
    let _ = std::fs::remove_dir_all(d);
}

/// A fake `claude`: `--help` lists permission modes; a turn logs its arguments and prompt to
/// `<repo>.log/`, writes `agent-<second word of the prompt>.txt` in its folder with a Write tool
/// call, and ends with a result line (cost, usage).
fn fake_claude(d: &Path) -> String {
    let log = d.with_extension("log");
    std::fs::create_dir_all(&log).unwrap();
    let script = log.join("claude");
    let body = format!(
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then echo '  --permission-mode <mode>  Permission mode (choices: "acceptEdits", "bypassPermissions", "default", "plan")'; exit 0; fi
n=$(basename "$PWD")
echo "$@" > "{log}/args-$n"
cat > "{log}/prompt-$n"
word=$(head -n 1 "{log}/prompt-$n" | awk '{{print $2}}')
f="$PWD/agent-$word.txt"
echo '{{"type":"system","subtype":"init","session_id":"s-'$n'","model":"fake","permissionMode":"acceptEdits"}}'
echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"Write","input":{{"file_path":"'$f'"}}}}]}}}}'
echo "by the agent" > "$f"
echo '{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok"}}]}}}}'
echo '{{"type":"result","subtype":"success","is_error":false,"duration_ms":50,"num_turns":1,"result":"ok","session_id":"s-'$n'","total_cost_usd":0.02,"usage":{{"input_tokens":1000,"output_tokens":234}},"permission_denials":[]}}'
"#,
        log = log.display()
    );
    std::fs::write(&script, body).unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&script).status().unwrap();
    script.display().to_string()
}

fn app_in(d: &Path, files: &[&str]) -> App {
    let mut paths = vec![d.to_path_buf()];
    paths.extend(files.iter().map(|f| d.join(f)));
    let mut app = App::new(paths).unwrap();
    app.agents.forget_store();
    app.agents.runs.clear();
    let prog = fake_claude(d);
    app.agents.set_program(&prog);
    app
}

fn tick(app: &mut App) {
    app.tick();
    let _ = app.agents_tick();
    let _ = app.dock_tick();
    let _ = app.mission_tick();
}

fn wait(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
    let t = Instant::now();
    while !until(app) {
        assert!(t.elapsed() < Duration::from_secs(15), "timed out waiting for {what}; message {:?}", app.message);
        tick(app);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn key(app: &mut App, code: KeyCode, m: KeyModifiers) {
    app.handle(Event::Key(KeyEvent::new(code, m)));
}

fn press(app: &mut App, c: char) {
    key(app, KeyCode::Char(c), KeyModifiers::NONE);
}

fn typ(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, c);
    }
}

fn run(app: &App, id: u64) -> &Run {
    app.agents.runs.iter().find(|r| r.id == id).unwrap()
}

fn modes_known(app: &mut App) {
    app.agents.probe_cli();
    wait(app, "claude --help", |a| a.agents.modes.len() > 1);
    assert!(!app.agents.modes.iter().any(|m| m == "bypassPermissions"), "never offered");
}

#[test]
fn two_agents_in_worktrees_then_land_one_and_discard_the_other() {
    let d = scratch("mission-two");
    let mut app = app_in(&d, &["a.txt"]);
    modes_known(&mut app);
    key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert!(app.mission.open);
    for task in ["add alpha", "add beta"] {
        press(&mut app, 'n');
        typ(&mut app, task);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    }
    assert_eq!(app.mission.pending.len() + app.agents.runs.len(), 2, "two agents at once");
    wait(&mut app, "both agents done", |a| a.agents.runs.len() == 2 && a.agents.runs.iter().all(|r| r.state == State::Done && r.stat.is_some()));
    let mut wt: Vec<Worktree> = app.agents.runs.iter().map(|r| r.worktree.clone().unwrap()).collect();
    wt.sort_by(|a, b| a.branch.cmp(&b.branch));
    for (w, word) in wt.iter().zip(["alpha", "beta"]) {
        assert!(w.path.join(format!("agent-{word}.txt")).exists(), "each agent wrote in its own worktree");
        assert!(!d.join(format!("agent-{word}.txt")).exists(), "the checkout is untouched");
        assert_eq!(w.base_branch.as_deref(), Some("main"));
        let args = std::fs::read_to_string(d.with_extension("log").join(format!("args-{}", w.branch))).unwrap();
        assert!(args.contains("--permission-mode acceptEdits") && !args.contains("dangerously"), "{args}");
    }
    assert_eq!(wt[0].branch, "add-alpha");
    assert_eq!(run(&app, 1).worktree.as_ref().unwrap().branch, "add-alpha");
    let r1 = run(&app, 1);
    assert_eq!(r1.stat.as_deref(), Some("1 file +1 −0"));
    assert_eq!(r1.tokens, 1234);
    assert_eq!(r1.activity(), crate::agents::run::Activity::Waiting);
    assert_eq!(app.mission.notified.len(), 2, "one notification per finished agent: {:?}", app.mission.notified);

    // The screen: cards with branch, cost, tokens, changes.
    let s = super::ui::screen(&mut app, 120, 30).join("\n");
    for want in ["MISSION CONTROL", "2 agents", "#2", "#1", "⎇ add-alpha ← main", "waiting for you", "$0.02 · 1.2k tok · 1 file +1 −0", "⏺ Write agent-alpha.txt"] {
        assert!(s.contains(want), "missing {want:?} in\n{s}");
    }
    let narrow = super::ui::screen(&mut app, 80, 24).join("\n");
    assert!(narrow.contains("#1") && narrow.contains("add-alpha"), "{narrow}");

    // Land #1 (newest first: Home is #2, one down is #1).
    key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(app.mission.sel, Some(2));
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.mission.sel, Some(1));
    press(&mut app, 'l');
    wait(&mut app, "the land question", |a| a.mission.confirm.is_some());
    let q = app.mission.confirm.as_ref().unwrap().question.clone();
    assert!(q.contains("commit 1 uncommitted file on add-alpha, then fast-forward main"), "{q}");
    press(&mut app, 'y');
    wait(&mut app, "landed", |a| a.mission.confirm.as_ref().is_some_and(|c| c.act == Act::Remove(1)));
    assert!(d.join("agent-alpha.txt").exists(), "landed in the checkout");
    assert!(git(&d, &["log", "-1", "--format=%B"]).unwrap().contains("Co-Authored-By: Claude"));
    assert_eq!(run(&app, 1).landed.as_deref(), Some("landed in main"));
    press(&mut app, 'y');
    wait(&mut app, "worktree removed", |a| a.mission.busy.is_none() && a.mission.confirm.is_none());
    assert!(!wt[0].path.exists(), "{:?}", app.message);
    assert!(git(&d, &["rev-parse", "-q", "--verify", "refs/heads/add-alpha"]).is_err(), "merged branch deleted");
    // Landing again says what happened.
    press(&mut app, 'l');
    assert!(app.message.as_deref().unwrap_or("").contains("landed in main"), "{:?}", app.message);

    // Discard #2: asks, says what is lost.
    key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.mission.sel, Some(2));
    press(&mut app, 'x');
    wait(&mut app, "the discard question", |a| a.mission.confirm.is_some());
    let q = app.mission.confirm.as_ref().unwrap().question.clone();
    assert!(q.contains("Discard") && q.contains("1 uncommitted file") && q.contains("Can't be undone"), "{q}");
    press(&mut app, 'n');
    assert!(app.mission.confirm.is_none() && wt[1].path.exists(), "n keeps it");
    press(&mut app, 'x');
    wait(&mut app, "the discard question again", |a| a.mission.confirm.is_some());
    press(&mut app, 'y');
    wait(&mut app, "discarded", |a| a.mission.busy.is_none() && run(a, 2).landed.is_some());
    assert!(!wt[1].path.exists());
    assert!(git(&d, &["rev-parse", "-q", "--verify", "refs/heads/add-beta"]).is_err());
    assert!(!d.join("agent-beta.txt").exists());
    // Gone worktrees: Del takes a card off the list.
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(app.agents.runs.len(), 1);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.mission.open);
    drop(app);
    cleanup(&d);
}

#[test]
fn ask_from_a_selection_explains_in_place_and_fixes_in_a_worktree() {
    let d = scratch("mission-ask");
    let mut app = app_in(&d, &["a.txt"]);
    modes_known(&mut app);
    // Lines 2-3 selected.
    let doc = &mut app.docs[0];
    let (s, e) = (doc.line_start(1), doc.line_start(3));
    doc.select(s, e);
    key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    let a = app.mission.ask.clone().expect("the ask prompt");
    assert_eq!(a.label, "a.txt:2-3");
    assert!(!a.worktree, "explain runs in place");
    let s = super::ui::screen(&mut app, 100, 24).join("\n");
    assert!(s.contains("Ask an agent · a.txt:2-3") && s.contains("write tests"), "{s}");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.mission.ask.is_none());
    let id = app.agents.runs.last().unwrap().id;
    assert!(app.agents.visible && app.agents.selected == Some(id), "the answer shows in the agents panel");
    wait(&mut app, "the explain run", |a| run(a, id).state == State::Done);
    let r = run(&app, id);
    assert!(r.worktree.is_none());
    assert_eq!(r.title, "explain a.txt:2-3");
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    let prompt = std::fs::read_to_string(d.with_extension("log").join(format!("prompt-{name}"))).unwrap();
    assert!(prompt.starts_with("Explain this code") && prompt.contains("a.txt (lines 2-3 selected)") && prompt.contains("    2  two\n    3  three\n"), "{prompt}");
    let args = std::fs::read_to_string(d.with_extension("log").join(format!("args-{name}"))).unwrap();
    assert!(!args.contains("--permission-mode"), "explain uses the CLI default: {args}");

    // Fix → a worktree.
    key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.mission.ask.as_ref().unwrap().sel, 1);
    assert!(app.mission.ask.as_ref().unwrap().worktree);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    wait(&mut app, "the fix run", |a| a.agents.runs.len() == 2 && a.agents.runs[1].state == State::Done);
    let w = app.agents.runs[1].worktree.clone().expect("fix runs in a worktree");
    assert!(w.branch.starts_with("fix-a-txt-2-3"), "{}", w.branch);
    let prompt = std::fs::read_to_string(d.with_extension("log").join(format!("prompt-{}", w.branch))).unwrap();
    assert!(prompt.starts_with("Find and fix") && prompt.contains("    3  three"), "{prompt}");
    // Typed text is a code task (worktree); Tab makes it in place; Esc cancels.
    key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    typ(&mut app, "why");
    assert!(app.mission.ask.as_ref().unwrap().worktree);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(!app.mission.ask.as_ref().unwrap().worktree);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.mission.ask.is_none());
    assert_eq!(app.agents.runs.len(), 2);
    drop(app);
    cleanup(&d);
}

#[test]
fn keys_that_refuse_say_why_and_follow_ups_resume() {
    let d = scratch("mission-refuse");
    let mut app = app_in(&d, &["a.txt"]);
    key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
    press(&mut app, 'l');
    assert_eq!(app.message.as_deref(), Some("No agent selected"));
    // An in-place agent: nothing to land.
    press(&mut app, 'n');
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.mission.input.as_ref().unwrap().kind, InputKind::New { worktree: false, repo: 0 });
    typ(&mut app, "tidy up");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    wait(&mut app, "in-place run", |a| a.agents.runs.first().is_some_and(|r| r.state == State::Done));
    assert!(d.join("agent-up.txt").exists(), "in place: the checkout itself");
    press(&mut app, 'l');
    assert!(app.message.as_deref().unwrap_or("").contains("worked in place"), "{:?}", app.message);
    press(&mut app, 'x');
    assert!(app.message.as_deref().unwrap_or("").contains("worked in place"));
    // Follow-up into the same session.
    press(&mut app, 'f');
    typ(&mut app, "and more");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    wait(&mut app, "the follow-up", |a| a.agents.runs[0].turns == 2 && a.agents.runs[0].state == State::Done);
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    let args = std::fs::read_to_string(d.with_extension("log").join(format!("args-{name}"))).unwrap();
    assert!(args.contains(&format!("--resume s-{name}")), "{args}");
    // Transcript view and back.
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.mission.view, View::Transcript);
    let s = super::ui::screen(&mut app, 100, 24).join("\n");
    assert!(s.contains("› and more") && s.contains("in place · "), "{s}");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.mission.view, View::Cards);
    assert!(app.mission.open);
    key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert!(!app.mission.open);
    drop(app);
    cleanup(&d);
}

#[test]
fn sparkline_and_token_labels() {
    assert_eq!(super::ui::sparkline(&[0, 1, 2, 4, 8]), "·▁▂▄█");
    assert_eq!(super::ui::tokens(812), "812 tok");
    assert_eq!(super::ui::tokens(41_300), "41.3k tok");
    assert_eq!(super::ui::tokens(1_250_000), "1.2M tok");
    let mut r = Run::new(1, PathBuf::from("/tmp"), "x");
    r.pulse = vec![10_000, 14_000, 19_500, 19_900];
    assert_eq!(r.sparkline(20_000, 5_000, 3), vec![1, 1, 2]);
}
