//! Index tests: every query is checked against the plain walk-and-read search
//! (`project::search::find_in_files`), before and after incremental updates, a restart from
//! the cache and compaction.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::root::Config;
use super::*;
use crate::project::search::{find_in_files, Hit, Job};

fn scratch(name: &str) -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("catlas-idx-{name}-{}-{n}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    crate::project::canonical(&d)
}

fn cfg() -> Config {
    Config { watch: false, threads: 3, budget: 8 << 20, ..Config::default() }
}

fn collect(job: Job<Vec<Hit>>) -> Vec<Hit> {
    let t = Instant::now();
    let mut out = Vec::new();
    loop {
        match job.rx.recv_timeout(Duration::from_millis(5)) {
            Ok(b) => out.extend(b),
            Err(_) if job.done() => {
                while let Ok(b) = job.rx.try_recv() {
                    out.extend(b);
                }
                return out;
            }
            Err(_) => {}
        }
        assert!(t.elapsed() < Duration::from_secs(20));
    }
}

type Key = (String, usize, usize);

fn keys(h: &[Hit]) -> BTreeSet<Key> {
    h.iter().map(|h| (h.path.clone(), h.line, h.start)).collect()
}

/// Index results == brute force for `q`.
fn same_as_scan(idx: &Index, root: &Path, q: &str) {
    let got = collect(idx.find(root, None, q.into(), FindOpts::default()).expect("content index ready"));
    let want = collect(find_in_files(root.to_path_buf(), q.into()));
    assert_eq!(keys(&got), keys(&want), "query {q:?}");
    assert_eq!(got.len(), want.len(), "no duplicates for {q:?}");
}

const WORDS: &[&str] = &["alpha", "Beta", "gamma_delta", "fooBar", "zeta", "x", "ünïcode", "tab\there", "return", "qux"];

fn corpus(root: &Path, n: usize, seed: u64) {
    let mut r = seed;
    let mut next = || {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        r
    };
    for i in 0..n {
        let dir = root.join(format!("d{}/e{}", i % 4, i % 3));
        fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        for _ in 0..(next() % 30) {
            let w = WORDS[(next() % WORDS.len() as u64) as usize];
            text.push_str(w);
            text.push(if next() % 5 == 0 { '\n' } else { ' ' });
        }
        let ext = ["rs", "py", "txt", "md"][i % 4];
        fs::write(dir.join(format!("f{i}.{ext}")), text).unwrap();
    }
    fs::write(root.join(".gitignore"), "ignored/\n*.log\n").unwrap();
    fs::create_dir_all(root.join("ignored")).unwrap();
    fs::write(root.join("ignored/a.rs"), "alpha in an ignored folder\n").unwrap();
    fs::write(root.join("x.log"), "alpha in a log\n").unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/t.rs"), "alpha in target\n").unwrap();
    fs::write(root.join("bin.dat"), b"alpha\0binary").unwrap();
}

const QUERIES: &[&str] = &["alpha", "Beta", "beta", "gamma_d", "oBa", "fooBar", "ünïc", "ta ze", "x", "qu", "nothing_here", "a\tb", "return qux", "eta"];

fn ready(cache: &Path, root: &Path) -> Index {
    let idx = Index::new(cfg(), cache.to_path_buf());
    idx.add_root(root);
    assert!(idx.wait_idle(Duration::from_secs(30)), "index idle");
    idx
}

#[test]
fn matches_brute_force_search() {
    let root = scratch("brute");
    let cache = scratch("brute-cache");
    corpus(&root, 120, 7);
    let idx = ready(&cache, &root);
    let v = idx.view(&root).unwrap();
    assert!(v.content_ready && v.files_ready);
    for q in QUERIES {
        same_as_scan(&idx, &root, q);
    }
    // gitignore, target/ and binaries: listed or not like the tree; never searched.
    assert!(!v.files.iter().any(|f| f.starts_with("ignored/") || f.ends_with(".log") || f.starts_with("target/")));
    assert!(v.files.iter().any(|f| f == "bin.dat"), "binaries are found by name");
    assert!(v.files.windows(2).all(|w| w[0] < w[1]), "sorted");
    let mut walked: Vec<String> = Vec::new();
    let job = crate::project::search::index_files(root.clone());
    while !job.done() || job.rx.try_recv().map(|b| walked.extend(b)).is_ok() {
        if let Ok(b) = job.rx.recv_timeout(Duration::from_millis(5)) {
            walked.extend(b);
        }
    }
    walked.sort();
    assert_eq!(*v.files, walked, "same files as the project's walk");
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn incremental_updates_and_scope() {
    let root = scratch("incr");
    let cache = scratch("incr-cache");
    corpus(&root, 40, 11);
    let idx = ready(&cache, &root);
    let r = idx.root(&root).unwrap();
    // Edit, add (also in a new folder), delete a file and a folder.
    std::thread::sleep(Duration::from_millis(20));
    fs::write(root.join("d0/e0/f0.rs"), "brand new alpha text\n").unwrap();
    fs::write(root.join("d1/added.rs"), "fn freshly_added() { alpha }\n").unwrap();
    fs::create_dir_all(root.join("newdir/deep")).unwrap();
    fs::write(root.join("newdir/deep/n.md"), "freshly_added in a new folder\n").unwrap();
    fs::remove_file(root.join("d2/e2/f2.txt")).ok();
    fs::remove_dir_all(root.join("d3")).unwrap();
    fs::write(root.join("ignored/b.rs"), "freshly_added but ignored\n").unwrap();
    r.changed(vec![root.join("d0/e0/f0.rs"), root.join("d1/added.rs"), root.join("newdir/deep/n.md"), root.join("d2/e2/f2.txt"), root.join("d3"), root.join("ignored/b.rs")]);
    assert!(idx.wait_idle(Duration::from_secs(30)));
    let v = idx.view(&root).unwrap();
    assert!(v.files.iter().any(|f| f == "d1/added.rs") && v.files.iter().any(|f| f == "newdir/deep/n.md"));
    assert!(!v.files.iter().any(|f| f == "d2/e2/f2.txt" || f.starts_with("d3/")));
    assert!(!v.files.iter().any(|f| f.starts_with("ignored/")));
    for q in QUERIES.iter().chain(&["freshly_added", "brand new"]) {
        same_as_scan(&idx, &root, q);
    }
    // A scoped search only looks in the folder; paths stay relative to the root.
    let hits = collect(idx.find(&root, Some("d1".into()), "alpha".into(), FindOpts::default()).unwrap());
    assert!(!hits.is_empty() && hits.iter().all(|h| h.path.starts_with("d1/")));
    // Symbols follow too.
    let s = idx.symbols_now(None, None, "freshly_added", 5);
    assert_eq!(s.first().map(|s| (s.path.as_str(), s.line)), Some(("d1/added.rs", 0)));
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn warm_start_loads_the_cache_and_sees_what_changed_meanwhile() {
    let root = scratch("warm");
    let cache = scratch("warm-cache");
    corpus(&root, 60, 3);
    drop(ready(&cache, &root));
    std::thread::sleep(Duration::from_millis(50));
    // Changes while catlas is closed: an edit and a new file.
    fs::write(root.join("d0/e0/f0.rs"), "while_closed\n").unwrap();
    fs::write(root.join("later.rs"), "while_closed too\n").unwrap();
    let idx = Index::new(cfg(), cache.clone());
    let r = idx.add_root(&root);
    let t = Instant::now();
    while !r.view().content_ready {
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(idx.wait_idle(Duration::from_secs(30)));
    for q in ["while_closed", "alpha", "Beta"] {
        same_as_scan(&idx, &root, q);
    }
    // The cache folder sits in the cache, keyed by the root.
    assert!(r.dir.starts_with(&cache) && r.dir.join("manifest.json").exists());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn compaction_keeps_results_right() {
    let root = scratch("compact");
    let cache = scratch("compact-cache");
    corpus(&root, 30, 5);
    let idx = ready(&cache, &root);
    let r = idx.root(&root).unwrap();
    for round in 0..12 {
        let p = root.join(format!("d{}/round{round}.rs", round % 4));
        fs::write(&p, format!("round_marker_{round} alpha\n")).unwrap();
        let mut changed = vec![p];
        if round % 3 == 0 && round > 0 {
            let old = root.join(format!("d{}/round{}.rs", (round - 1) % 4, round - 1));
            fs::remove_file(&old).unwrap();
            changed.push(old);
        }
        r.changed(changed);
        assert!(idx.wait_idle(Duration::from_secs(30)));
    }
    let v = idx.view(&root).unwrap();
    assert!(v.segs.len() <= 7, "compacted: {} segments", v.segs.len());
    for q in ["round_marker_", "alpha", "round_marker_11", "qux"] {
        same_as_scan(&idx, &root, q);
    }
    drop(idx);
    // And again after loading the compacted cache.
    let idx = ready(&cache, &root);
    same_as_scan(&idx, &root, "round_marker_");
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn a_second_instance_is_read_only_but_works() {
    let root = scratch("ro");
    let cache = scratch("ro-cache");
    corpus(&root, 20, 9);
    let first = ready(&cache, &root);
    let second = ready(&cache, &root);
    let r2 = second.root(&root).unwrap();
    assert!(r2.read_only.load(std::sync::atomic::Ordering::Relaxed));
    fs::write(root.join("d0/ro.rs"), "seen_by_the_reader\n").unwrap();
    r2.changed(vec![root.join("d0/ro.rs")]);
    assert!(second.wait_idle(Duration::from_secs(30)));
    same_as_scan(&second, &root, "seen_by_the_reader");
    drop(first);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn cache_inside_the_root_is_not_indexed() {
    let root = scratch("inside");
    corpus(&root, 10, 1);
    let cache = root.join("bin/.cache");
    let idx = ready(&cache, &root);
    let v = idx.view(&root).unwrap();
    assert!(!v.files.iter().any(|f| f.starts_with("bin/.cache")), "{:?}", v.files);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ranking_puts_definitions_and_whole_words_first() {
    let root = scratch("rank");
    let cache = scratch("rank-cache");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(root.join("src/a.rs"), "fn use_it() { parse_config(); parse_configs(); }\n").unwrap();
    fs::write(root.join("tests/t.rs"), "fn t() { parse_config(); }\n").unwrap();
    fs::write(root.join("src/config.rs"), "// helpers\n\npub fn parse_config() {}\n").unwrap();
    let idx = ready(&cache, &root);
    let hits = collect(idx.find(&root, None, "parse_config".into(), FindOpts { per_line: true, boost: Vec::new() }).unwrap());
    assert_eq!((hits[0].path.as_str(), hits[0].line), ("src/config.rs", 2), "the definition first: {hits:?}");
    assert_eq!(hits.last().unwrap().path, "tests/t.rs", "tests last");
    assert_eq!(hits.len(), 3, "one row per line");
    // Find in Files: files by their best hit, each file's hits in line order.
    let hits = collect(idx.find(&root, None, "parse_config".into(), FindOpts::default()).unwrap());
    assert_eq!(hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), vec!["src/config.rs", "src/a.rs", "src/a.rs", "tests/t.rs"]);
    // Symbols: exact name first, fuzzy after.
    let s = idx.symbols_now(None, None, "parse_config", 10);
    assert_eq!(s[0].name, "parse_config");
    assert_eq!(s[0].kind.label(), "fn");
    assert!(idx.symbols_now(None, None, "pcfg", 10).iter().any(|s| s.name == "parse_config"), "humps");
    assert!(idx.symbols_now(None, Some("tests"), "parse_config", 10).is_empty(), "scope");
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn several_roots_at_once() {
    let (a, b) = (scratch("multi-a"), scratch("multi-b"));
    let cache = scratch("multi-cache");
    fs::write(a.join("one.rs"), "fn shared_name() {}\n").unwrap();
    fs::write(b.join("two.rs"), "fn shared_name() {}\nfn only_b() {}\n").unwrap();
    let idx = Index::new(cfg(), cache.clone());
    idx.add_root(&a);
    idx.add_root(&b);
    idx.add_root(&a);
    assert!(idx.wait_idle(Duration::from_secs(30)));
    assert_eq!(idx.roots().len(), 2);
    let s = idx.symbols_now(None, None, "shared_name", 10);
    let roots: BTreeSet<PathBuf> = s.iter().map(|h| h.root.clone()).collect();
    assert_eq!(roots.len(), 2, "both roots");
    assert_eq!(idx.symbols_now(Some(&a), None, "only_b", 10).len(), 0);
    same_as_scan(&idx, &b, "only_b");
    assert!(idx.remove_root(&b));
    assert!(idx.view(&b).is_none() && idx.symbols_now(None, None, "only_b", 10).is_empty());
    for d in [a, b, cache] {
        fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn the_watcher_picks_up_changes() {
    let root = scratch("watch");
    let cache = scratch("watch-cache");
    corpus(&root, 10, 2);
    let idx = Index::new(Config { watch: true, ..cfg() }, cache.clone());
    idx.add_root(&root);
    assert!(idx.wait_idle(Duration::from_secs(30)));
    std::thread::sleep(Duration::from_millis(300)); // FSEvents needs a moment to start
    fs::write(root.join("d0/watched.rs"), "fn seen_by_watcher() {}\n").unwrap();
    let t = Instant::now();
    loop {
        let v = idx.view(&root).unwrap();
        if v.files.iter().any(|f| f == "d0/watched.rs") && idx.wait_idle(Duration::from_secs(5)) {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(15), "the watcher didn't report the new file");
        std::thread::sleep(Duration::from_millis(50));
    }
    same_as_scan(&idx, &root, "seen_by_watcher");
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn project_uses_the_index() {
    use crate::project::quick::Item;
    use crate::project::{PopupKind, Project};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let root = crate::project::canonical(&crate::project::tree::tests::sample());
    fs::write(root.join("src/lib.rs"), "pub fn Hello() {}\npub struct HelloWorld;\n").unwrap();
    let cache = scratch("project-cache");
    let idx = Index::new(cfg(), cache.clone());
    let mut p = Project::with_index(crate::workspace::Workspace::single(root.clone()), true, Some(idx.clone()));
    assert!(idx.wait_idle(Duration::from_secs(30)));
    p.poll();
    assert!(!p.indexing());
    assert!(p.files.iter().any(|f| f == "src/nested/deep.rs"));
    assert!(!p.files.iter().any(|f| f.contains("secret") || f.starts_with("target")));
    p.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL), None, None);
    assert_eq!(p.popup.as_ref().map(|x| x.kind), Some(PopupKind::Quick));
    for c in "hello".chars() {
        p.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), None, None);
    }
    let t = Instant::now();
    while p.searching() || p.syms.is_empty() {
        p.poll();
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(p.syms[0].name, "Hello", "{:?}", p.syms);
    let its = p.quick_items();
    let sym = its.iter().position(|i| matches!(i, Item::Sym(_))).expect("a symbol row");
    p.popup.as_mut().unwrap().selected = sym;
    let t = p.popup_target().unwrap();
    assert_eq!((t.path, t.at), (root.join("src/lib.rs"), Some((0, 7, 5))));
    let paths: BTreeSet<&str> = p.hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths, ["README.md", "src/lib.rs", "src/main.rs", "src/nested/deep.rs"].into_iter().collect());
    assert_eq!(p.hit_files, 4);
    // A file created now shows in Go to File once the index sees it.
    fs::write(root.join("docs/new_file.md"), "x\n").unwrap();
    idx.root(&root).unwrap().changed(vec![root.join("docs/new_file.md")]);
    assert!(idx.wait_idle(Duration::from_secs(30)));
    p.poll();
    assert!(p.files.iter().any(|f| f == "docs/new_file.md"));
    drop(p);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(cache).unwrap();
}

#[test]
fn large_files_open_without_highlighting_and_draw_fast() {
    use crate::doc::{is_large, Doc, LONG_LINE};
    assert!(!is_large(b"short\nlines\n", 1 << 20));
    assert!(is_large(&vec![b'x'; LONG_LINE], 1 << 20), "one long line");
    assert!(is_large(b"0123456789", 5), "over the size limit");
    let d = scratch("large");
    let p = d.join("min.rs");
    fs::write(&p, format!("fn main() {{}}\n{}\n", "let x = 1; ".repeat(30_000))).unwrap();
    let doc = Doc::open(&p).unwrap();
    assert!(doc.large);
    assert_eq!(doc.syntax.lang, crate::syntax::Lang::Rust, "the language name stays");
    assert!(doc.large_note().is_some());
    let mut app = crate::app::App::new(vec![p.clone()]).unwrap();
    assert!(app.message.as_deref().is_some_and(|m| m.starts_with("Large file")), "{:?}", app.message);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    // The 330 000-char line is cut at the screen edge, not walked to its end every frame.
    app.handle(crossterm::event::Event::Key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Down, crossterm::event::KeyModifiers::NONE)));
    let t = Instant::now();
    for _ in 0..20 {
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    }
    assert!(t.elapsed() < Duration::from_secs(2), "{:?} for 20 draws", t.elapsed());
    let screen: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    assert!(screen.contains("let x = 1;"));
    // A normal file still gets colours.
    let q = d.join("small.rs");
    fs::write(&q, "fn main() {}\n").unwrap();
    assert!(!Doc::open(&q).unwrap().large);
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn fuzzy_prefilter_agrees_with_the_scorer() {
    use crate::project::fuzzy::score;
    for (q, c, m) in [("app", "src/app.rs", true), ("APP", "src/app.rs", true), ("src app", "src/app.rs", true), ("acb", "abc", false), ("zz", "src/app.rs", false), ("ü", "ü.rs", true), ("doc", "Docs/x", true)] {
        assert_eq!(score(q, c).is_some(), m, "{q} in {c}");
    }
}


#[test]
fn a_workspace_of_several_repos_uses_the_index() {
    use crate::project::{PopupKind, Project};
    use crate::workspace::Workspace;
    let home = scratch("ws");
    let cache = scratch("ws-cache");
    for (repo, body) in [("alpha", "fn in_alpha() { shared_word }\n"), ("beta", "fn in_beta() { shared_word }\n")] {
        fs::create_dir_all(home.join(repo).join("src")).unwrap();
        fs::write(home.join(repo).join("src/lib.rs"), body).unwrap();
    }
    let ws = Workspace::from_dirs(None, home.clone(), vec![(home.join("alpha"), None, None), (home.join("beta"), None, None)]);
    let idx = Index::new(cfg(), cache.clone());
    let mut p = Project::with_index(ws, true, Some(idx.clone()));
    assert!(idx.wait_idle(Duration::from_secs(30)));
    assert_eq!(idx.roots().len(), 2, "one index root per repository");
    p.poll();
    assert_eq!(*p.files, vec!["alpha/src/lib.rs".to_string(), "beta/src/lib.rs".to_string()]);
    p.open_popup(PopupKind::Search, Some("shared_word".into()));
    let t = Instant::now();
    while p.searching() || p.hits.len() < 2 {
        p.poll();
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    let paths: BTreeSet<&str> = p.hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths, ["alpha/src/lib.rs", "beta/src/lib.rs"].into_iter().collect());
    assert_eq!(p.popup_target().unwrap().path.parent().unwrap().parent().unwrap().file_name().unwrap(), paths.iter().next().unwrap().split('/').next().unwrap());
    // `@beta` scopes to one repository.
    p.close_popup();
    p.open_popup(PopupKind::Search, Some("@beta shared_word".into()));
    let t = Instant::now();
    while p.searching() || p.hits.is_empty() {
        p.poll();
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(p.hits.iter().all(|h| h.path == "beta/src/lib.rs"), "{:?}", p.hits);
    // Removing a repository removes its index root.
    p.close_popup();
    p.set_workspace(Workspace::single(home.join("alpha")));
    assert_eq!(idx.roots(), vec![crate::project::canonical(&home.join("alpha"))]);
    assert!(idx.wait_idle(Duration::from_secs(30)));
    p.poll();
    assert_eq!(*p.files, vec!["src/lib.rs".to_string()]);
    drop(p);
    fs::remove_dir_all(home).unwrap();
    fs::remove_dir_all(cache).unwrap();
}
