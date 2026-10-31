//! Workspace tests: paths across roots, opening (folders, files, nested repos), recent list,
//! excludes, the multi-root tree, cross-repo search, per-repo git and fetch/pull plans (against
//! local bare repos only, never a real remote).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::repos::{plan, summary, visible, Op};
use super::status::{parse_track, read, sum_numstat, Statuses};
use super::*;
use crate::project::{canonical, Project};

static N: AtomicUsize = AtomicUsize::new(0);

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("catlas-ws-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    canonical(&d)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A repo with `files` committed.
fn repo(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    for (f, body) in files {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    for a in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "T"], &["config", "user.email", "t@example.com"], &["config", "commit.gpgsign", "false"], &["add", "."], &["commit", "-qm", "first"]] {
        git(dir, a);
    }
}

/// `home/{alpha,beta}` repos plus a plain folder.
fn two_repos() -> PathBuf {
    let home = scratch("two");
    repo(&home.join("alpha"), &[("src/main.rs", "fn main() { needle(); }\n"), ("README.md", "alpha\n")]);
    repo(&home.join("beta"), &[("lib/util.rs", "pub fn needle() {}\n"), ("README.md", "beta\n")]);
    std::fs::create_dir_all(home.join("notes")).unwrap();
    home
}

fn ws_of(home: &Path) -> Workspace {
    Workspace::from_dirs(None, home.to_path_buf(), vec![(home.join("alpha"), None, None), (home.join("beta"), None, None)])
}

#[test]
fn paths_map_both_ways_across_roots() {
    let home = two_repos();
    let ws = ws_of(&home);
    assert!(ws.multi());
    let f = home.join("beta/lib/util.rs");
    assert_eq!(ws.rel(&f), "beta/lib/util.rs");
    assert_eq!(ws.abs("beta/lib/util.rs"), f);
    assert_eq!(ws.rel(&home.join("alpha")), "alpha");
    assert_eq!(ws.abs("alpha"), home.join("alpha"));
    assert_eq!(ws.show("beta/lib/util.rs"), "beta › lib/util.rs");
    assert_eq!(ws.root_of(&f), Some(1));
    assert_eq!(ws.root_of(&home.join("notes")), None);
    assert_eq!(ws.rel(Path::new("/elsewhere/x")), "/elsewhere/x");
    // One root: plain relative paths, as before workspaces.
    let one = Workspace::single(home.join("alpha"));
    assert_eq!(one.rel(&home.join("alpha/src/main.rs")), "src/main.rs");
    assert_eq!(one.show("src/main.rs"), "src/main.rs");
    assert_eq!(one.prefixes(), vec![(String::new(), home.join("alpha"))]);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn labels_are_unique_and_roots_dont_nest() {
    let home = two_repos();
    std::fs::create_dir_all(home.join("x/alpha")).unwrap();
    let mut ws = ws_of(&home);
    assert_eq!(ws.add_root(&home.join("x/alpha"), None, None), Ok(2));
    assert_eq!(ws.roots[2].label, "alpha-2");
    assert!(ws.add_root(&home.join("alpha"), None, None).unwrap_err().contains("already a root"));
    assert!(ws.add_root(&home.join("alpha/src"), None, None).unwrap_err().contains("overlaps"));
    assert!(ws.add_root(&home.join("missing"), None, None).is_err());
    assert_ne!(ws.roots[0].color, ws.roots[1].color);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn repo_scope_by_at_label() {
    let home = two_repos();
    let ws = ws_of(&home);
    assert_eq!(ws.repo_query("@beta needle"), (Some("beta".into()), "needle"));
    assert_eq!(ws.repo_query("@al main"), (Some("alpha".into()), "main"), "a unique start");
    assert_eq!(ws.repo_query("@Override foo"), (None, "@Override foo"));
    assert_eq!(ws.repo_query("@beta"), (None, "@beta"), "needs the space");
    assert_eq!(Workspace::single(home.join("alpha")).repo_query("@alpha x").0, None);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn opening_a_folder_of_repos_folders_and_files() {
    let home = two_repos();
    // A folder holding repos: they become the roots (the plain folder doesn't).
    let o = from_args(vec![home.clone()]);
    assert_eq!(o.ws.roots.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
    assert_eq!(o.ws.home, home);
    assert!(o.as_project && o.note.unwrap().contains("2 repos"));
    // A repo itself stays one root.
    let o = from_args(vec![home.join("alpha")]);
    assert_eq!(o.ws.roots.len(), 1);
    // Two folders: two roots, home = the first; a file opens as a tab.
    let o = from_args(vec![home.join("alpha"), home.join("notes"), home.join("alpha/README.md")]);
    assert_eq!(o.ws.roots.len(), 2);
    assert_eq!(o.ws.home, home.join("alpha"));
    assert_eq!(o.files, vec![home.join("alpha/README.md")]);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn workspace_files_with_labels_colours_and_excludes() {
    let home = two_repos();
    let f = home.join("studio.catlas.toml");
    std::fs::write(&f, "name = \"Studio\"\nexclude = [\"*.log\", \"lib/gen\"]\nroots = [\"beta\"]\n[[root]]\npath = \"alpha\"\nlabel = \"A\"\ncolor = \"orange\"\n[[root]]\npath = \"gone\"\n").unwrap();
    let o = from_args(vec![f.clone()]);
    let ws = &o.ws;
    assert_eq!(ws.name, "Studio");
    assert_eq!(ws.roots.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["beta", "A"]);
    assert_eq!(ws.roots[1].color, parse_color("orange").unwrap());
    assert!(o.note.unwrap().contains("gone"), "a missing root is reported");
    assert_eq!(ws.excludes, ["*.log", "lib/gen"]);
    // .code-atlas/workspace.toml in a folder opens that workspace.
    std::fs::create_dir_all(home.join(".code-atlas")).unwrap();
    std::fs::write(home.join(DIR_FILE), "roots = [\"alpha\"]\n").unwrap();
    let o = from_args(vec![home.clone()]);
    assert_eq!(o.ws.roots.len(), 1);
    assert_eq!(o.ws.file.as_deref(), Some(home.join(DIR_FILE).as_path()));
    // Exclude matching (pure: the global list is shared by parallel tests).
    let list = vec![Exclude::Name("*.log".into()), Exclude::Path(home.join("beta/lib/gen"))];
    assert!(matches(&list, Path::new("/x/debug.log"), "debug.log"));
    assert!(matches(&list, &home.join("beta/lib/gen"), "gen"));
    assert!(!matches(&list, &home.join("beta/lib"), "lib"));
    assert!(glob("a*b*c", "aXXbYc") && !glob("a*b*c", "acb") && glob("x", "x"));
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn recent_workspaces_round_trip() {
    let home = two_repos();
    let store = home.join("cfg/workspaces.toml");
    let ws = ws_of(&home);
    remember(&store, &ws).unwrap();
    remember(&store, &Workspace::single(home.join("alpha"))).unwrap();
    remember(&store, &ws).unwrap();
    let list = load_recent(Some(&store));
    assert_eq!(list.len(), 1, "single roots aren't remembered, the same workspace only once");
    assert_eq!(pick_recent(&list, "1").unwrap().roots.len(), 2);
    assert!(pick_recent(&list, "2").is_none());
    let back = list[0].open().unwrap();
    assert_eq!(back.roots.iter().map(|r| &r.label).collect::<Vec<_>>(), ["alpha", "beta"]);
    assert!(list_recent(&list).contains("2 roots"));
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn multi_root_tree_reveals_and_collapses_across_roots() {
    let home = two_repos();
    let mut p = Project::with_workspace(ws_of(&home), true);
    let names: Vec<(usize, String)> = p.tree.rows.iter().map(|n| (n.depth, n.name.clone())).collect();
    assert_eq!(names[0], (0, "alpha".into()));
    assert!(names.contains(&(0, "beta".into())));
    assert!(p.tree.reveal(&home.join("beta/lib/util.rs")));
    assert_eq!(p.tree.selected_node().unwrap().name, "util.rs");
    p.tree.collapse_all();
    assert!(p.tree.rows.iter().all(|n| n.depth == 0), "{:?}", p.tree.rows.iter().map(|n| &n.name).collect::<Vec<_>>());
    assert_eq!(p.tree.selected_node().unwrap().name, "beta");
    // Search Everywhere from a root row is scoped to that repo.
    assert_eq!(p.tree_scope(), Some("beta".into()));
    std::fs::remove_dir_all(home).unwrap();
}

fn ready(p: &mut Project) {
    let t = Instant::now();
    while p.indexing() || p.searching() {
        assert!(t.elapsed() < Duration::from_secs(10));
        p.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn finder_and_find_in_files_span_every_root() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let home = two_repos();
    let mut p = Project::with_workspace(ws_of(&home), true);
    ready(&mut p);
    let mut files = (*p.files).clone();
    files.sort();
    assert_eq!(files, ["alpha/README.md", "alpha/src/main.rs", "beta/README.md", "beta/lib/util.rs"]);
    let typ = |p: &mut Project, s: &str| {
        for c in s.chars() {
            p.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), None, None);
        }
    };
    // Go to File: both READMEs; `@beta` keeps one.
    p.open_popup(crate::project::PopupKind::Files, None);
    typ(&mut p, "readme");
    assert_eq!(p.matches.len(), 2);
    p.popup.as_mut().unwrap().input = "@beta readme".into();
    p.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), None, None);
    p.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), None, None);
    assert_eq!(p.matches.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(), ["beta/README.md"]);
    assert_eq!(p.popup_target().unwrap().path, home.join("beta/README.md"));
    p.close_popup();
    // Find in Files across repos, then scoped to one.
    p.open_popup(crate::project::PopupKind::Search, Some("needle".into()));
    ready(&mut p);
    let mut hits: Vec<&str> = p.hits.iter().map(|h| h.path.as_str()).collect();
    hits.sort();
    assert_eq!(hits, ["alpha/src/main.rs", "beta/lib/util.rs"]);
    p.popup.as_mut().unwrap().input = "@alpha needle".into();
    p.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), None, None);
    p.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), None, None);
    std::thread::sleep(Duration::from_millis(200));
    p.poll();
    ready(&mut p);
    assert_eq!(p.hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), ["alpha/src/main.rs"]);
    let t = p.popup_target().unwrap();
    assert_eq!(t.path, home.join("alpha/src/main.rs"));
    assert_eq!(t.at, Some((0, 12, 6)));
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn status_of_every_repo_branch_changes_ahead_behind() {
    let home = two_repos();
    let bare = home.join("remote.git");
    git(&home, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    let a = home.join("alpha");
    git(&a, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&a, &["push", "-q", "-u", "origin", "main"]);
    std::fs::write(a.join("README.md"), "alpha\nmore\n").unwrap();
    git(&a, &["commit", "-qam", "two"]);
    std::fs::write(a.join("README.md"), "alpha\nmore\nand more\n").unwrap();
    std::fs::write(a.join("new.txt"), "x\n").unwrap();
    let roots = vec![a.clone(), home.join("beta"), home.join("notes")];
    let mut st = Statuses::default();
    st.settle(&roots);
    let sa = st.get(&a).unwrap();
    assert_eq!(sa.branch.as_deref(), Some("main"));
    assert!(sa.upstream && sa.ahead == 1 && sa.behind == 0);
    assert_eq!((sa.changed, sa.untracked), (2, 1));
    assert_eq!(sa.stat, Some((1, 0)));
    assert_eq!(sa.last.as_ref().unwrap().1, "two");
    assert!(sa.kind_of(&a.join("README.md"), false).is_some() && sa.kind_of(&a, true).is_some());
    let sb = st.get(&home.join("beta")).unwrap();
    assert!(!sb.dirty() && !sb.upstream && sb.remotes == 0);
    assert!(!st.get(&home.join("notes")).unwrap().is_repo());
    assert_eq!(parse_track("main...origin/main [ahead 2, behind 3]"), (true, 2, 3));
    assert_eq!(parse_track("main"), (false, 0, 0));
    assert_eq!(sum_numstat("3\t1\ta\n-\t-\tbin\n2\t0\tb\n"), (5, 1));

    // The popup's filter and "only changed".
    let ws = Workspace::from_dirs(None, home.clone(), roots.iter().map(|r| (r.clone(), None, None)).collect());
    assert_eq!(visible(&ws, &st, "", true), vec![0]);
    assert_eq!(visible(&ws, &st, "bet", false), vec![1]);

    // Fetch / pull plans: what has a remote / upstream; then a real fetch + pull from the bare repo.
    let targets: Vec<(String, PathBuf, Option<&status::RepoState>)> = ws.roots.iter().map(|r| (r.label.clone(), r.path.clone(), st.get(&r.path))).collect();
    let f = plan(Op::Fetch, &targets).unwrap();
    assert_eq!(f.runs.len(), 1);
    assert_eq!(f.skipped.len(), 2);
    assert!(f.question.starts_with("Fetch alpha"), "{}", f.question);
    let pl = plan(Op::Pull, &targets).unwrap();
    assert_eq!(pl.runs[0].2.args, ["pull", "--ff-only"]);
    assert!(pl.runs.iter().all(|r| !r.2.args.iter().any(|a| a.contains("force"))));
    assert!(plan(Op::Pull, &targets[1..]).is_err());
    let results: Vec<(String, Result<String, String>)> = f.runs.iter().map(|(l, top, op)| (l.clone(), crate::git::branches::run_remote(top, op))).collect();
    assert_eq!(summary(&Op::Fetch, &results, 2), "Fetched 1 of 1 repo · 2 skipped");
    let failed = vec![("x".to_string(), Err::<String, String>("fatal: no".into()))];
    assert!(summary(&Op::Pull, &failed, 0).starts_with("Couldn't pull 1 repo"));
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn read_outside_a_repo_is_empty() {
    let d = scratch("plain");
    assert!(!read(&d, false).is_repo());
    std::fs::remove_dir_all(d).unwrap();
}
