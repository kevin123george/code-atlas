//! `catlas --index-status [dir]` and `catlas --bench-index <dir> [--files N] [--keep]`.
//!
//! The status reads the cache without writing anything. The bench indexes `dir` into a scratch
//! cache (an empty or missing `dir` first gets a synthetic repository of N files in mixed
//! languages) and prints a Markdown table: cold build, warm start, query latencies (p50 / p95),
//! memory, an incremental update, a 100k-entry folder in the tree and a large file in the
//! editor. Everything it generates is removed again unless `--keep` is given.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::root::{self, root_dir, seg_name, walk_all};
use super::segment::{Segment, F_DELETED};
use super::trigram::fnv64;
use super::{cache_dir, FindOpts, Index};

pub fn run(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("--bench-warm") if args.len() >= 3 => warm(Path::new(&args[1]), Path::new(&args[2])),
        Some("--index-status") => {
            let dir = args.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            status(&dir);
            Ok(())
        }
        _ => {
            let Some(dir) = args.get(1).filter(|a| !a.starts_with("--")) else {
                anyhow::bail!("usage: catlas --bench-index <dir> [--files N] [--keep]");
            };
            let files = args.iter().position(|a| a == "--files").and_then(|i| args.get(i + 1)).and_then(|n| n.parse().ok()).unwrap_or(200_000);
            bench(Path::new(dir), files, args.iter().any(|a| a == "--keep"))
        }
    }
}

fn mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / (1 << 20) as f64)
}

/// Everything about the cache of `dir`, read only.
pub fn status(dir: &Path) {
    let root = crate::project::canonical(dir);
    let Some(cache) = cache_dir() else {
        println!("No cache folder (HOME is not set and CATLAS_CACHE_DIR isn't either).");
        return;
    };
    let s = &crate::config::Settings::get().index;
    println!("Code index (decision 0014)   enabled = {}   symbols = {}   watch = {}   max_file_kb = {}", s.enabled, s.symbols, s.watch, s.max_file_kb);
    println!("cache    {}", cache.display());
    let rdir = root_dir(&cache, &root);
    println!("root     {}", root.display());
    println!("folder   {}", rdir.display());
    let manifest: Option<serde_json::Value> = fs::read(rdir.join("manifest.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    match manifest {
        None => println!("state    not indexed yet (opening the folder in catlas builds it)"),
        Some(m) => {
            let seqs: Vec<u64> = m["segments"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).collect()).unwrap_or_default();
            let mut segs = Vec::new();
            for &q in &seqs {
                match Segment::open(&rdir.join(seg_name(q)), q) {
                    Ok(s) => segs.push(s),
                    Err(e) => println!("broken   {e} (catlas rebuilds it on the next start)"),
                }
            }
            let mut latest: HashMap<u64, (usize, usize)> = HashMap::new();
            for (si, s) in segs.iter().enumerate() {
                for i in 0..s.len() {
                    latest.insert(fnv64(s.path(i).as_bytes()), (si, i));
                }
            }
            let live: Vec<(usize, usize)> = latest.values().copied().filter(|&(s, i)| segs[s].meta[i].flags & F_DELETED == 0).collect();
            let mut alive: Vec<Vec<bool>> = segs.iter().map(|s| vec![false; s.len()]).collect();
            for &(s, i) in &live {
                alive[s][i] = true;
            }
            let syms: usize = segs.iter().zip(&alive).map(|(s, a)| s.syms.iter().filter(|y| a[y.file as usize]).count()).sum();
            let disk: u64 = segs.iter().map(|s| s.disk_bytes).sum();
            let heap: usize = segs.iter().map(|s| s.heap_bytes()).sum();
            let age = fs::metadata(rdir.join("manifest.json")).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
            println!("files    {}   symbols {syms}   segments {}   trigrams {}", live.len(), segs.len(), segs.iter().map(|s| s.trigrams()).max().unwrap_or(0));
            println!("size     {} on disk, ~{} of tables in memory when open", mb(disk), mb(heap as u64));
            if let Some(a) = age {
                println!("updated  {} ago", ago(a));
            }
            // What changed since (mtime + size), like the scan at start.
            let t = Instant::now();
            let (seen, _, capped) = walk_all(&root, &cache, 8, None, s.max_files);
            let mut changed = 0;
            let mut seen_keys = std::collections::HashSet::new();
            for e in &seen {
                let k = fnv64(e.rel.as_bytes());
                seen_keys.insert(k);
                let same = latest.get(&k).is_some_and(|&(s, i)| {
                    let m = segs[s].meta[i];
                    m.flags & F_DELETED == 0 && m.mtime == e.mtime && m.size == e.size
                });
                changed += usize::from(!same);
            }
            let gone = live.iter().filter(|&&(s, i)| !seen_keys.contains(&fnv64(segs[s].path(i).as_bytes()))).count();
            println!(
                "disk     {} files now, {changed} new or changed, {gone} deleted since (checked in {} ms){}",
                seen.len(),
                t.elapsed().as_millis(),
                if capped { ", listing capped at max_files" } else { "" }
            );
        }
    }
    let others: Vec<String> = fs::read_dir(cache.join("index"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m: serde_json::Value = serde_json::from_slice(&fs::read(e.path().join("manifest.json")).ok()?).ok()?;
            let size: u64 = fs::read_dir(e.path()).ok()?.flatten().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum();
            Some(format!("  {}  ({})", m["root"].as_str()?, mb(size)))
        })
        .collect();
    if !others.is_empty() {
        println!("all cached roots:");
        for o in others {
            println!("{o}");
        }
    }
}

fn ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        3600..=86_399 => format!("{} h", s / 3600),
        _ => format!("{} days", s / 86_400),
    }
}

// ---- bench -----------------------------------------------------------------------------------

/// Resident memory of this process (via `ps`; no extra crate).
pub fn rss() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const SYL: &[&str] = &[
    "ka", "lo", "mi", "ne", "ru", "ta", "vo", "zen", "pri", "sto", "gra", "fle", "dun", "bar", "qui", "wex", "yol", "hep", "jor", "cal", "dex", "fin", "gol", "hal", "ion", "jet", "kip", "lum", "mor", "nix",
];

fn word(r: &mut Rng) -> String {
    let n = 2 + r.below(2);
    (0..n).map(|_| SYL[r.below(SYL.len())]).collect()
}

fn camel(r: &mut Rng) -> String {
    let a = word(r);
    let b = word(r);
    let mut c = b.chars();
    format!("{a}{}{}", c.next().map(|x| x.to_ascii_uppercase()).unwrap_or('X'), c.as_str())
}

/// One synthetic file of `ext` (~2–8 KB) with functions, types and comments. File `i` divisible
/// by 50 000 also gets the marker `needle_marker_<i>` (a selective query with known hits).
fn file_text(r: &mut Rng, ext: &str, i: usize) -> String {
    let mut s = String::new();
    let n = 6 + r.below(18);
    for k in 0..n {
        let f = camel(r);
        let t = word(r);
        let body: Vec<String> = (0..3 + r.below(6)).map(|_| format!("{} {} {}", word(r), camel(r), r.below(1000))).collect();
        match ext {
            "rs" => s += &format!("/// {} {}\npub struct {}{k} {{ value: u32 }}\n\npub fn {f}(x: u32) -> u32 {{\n    // {}\n    let total = x + {};\n    total\n}}\n\n", word(r), word(r), cap(&t), body.join(" "), r.below(99)),
            "ts" => s += &format!("export interface {}{k} {{ id: number }}\n\nexport function {f}(x: number): number {{\n  // {}\n  return x * {};\n}}\n\n", cap(&t), body.join(" "), r.below(99)),
            "py" => s += &format!("class {}{k}:\n    def {f}(self, x):\n        # {}\n        return x + {}\n\n", cap(&t), body.join(" "), r.below(99)),
            "go" => s += &format!("type {}{k} struct {{\n\tValue int\n}}\n\nfunc {}(x int) int {{\n\t// {}\n\treturn x + {}\n}}\n\n", cap(&t), cap(&f), body.join(" "), r.below(99)),
            "java" => s += &format!("class {}{k} {{\n    int {f}(int x) {{\n        // {}\n        return x + {};\n    }}\n}}\n\n", cap(&t), body.join(" "), r.below(99)),
            "md" => s += &format!("## {} {}\n\n{}\n\n", cap(&t), f, body.join(". ")),
            _ => s += &format!("{{\"{t}\": \"{f}\", \"n\": {}, \"text\": \"{}\"}}\n", r.below(1000), body.join(" ")),
        }
    }
    if i % 50_000 == 7 {
        s += &format!("// needle_marker_{i}\n");
    }
    s
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|x| x.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
}

/// Writes `n` files under `dir` (8 threads): `pkgNN/modNN/fileN.ext`, ~20 files per folder,
/// plus a `.gitignore` and an ignored `build/` folder.
pub fn generate(dir: &Path, n: usize) -> std::io::Result<u64> {
    const EXTS: &[&str] = &["rs", "ts", "py", "go", "java", "md", "json"];
    fs::create_dir_all(dir)?;
    fs::write(dir.join(".gitignore"), "build/\n*.log\n")?;
    fs::create_dir_all(dir.join("build"))?;
    for k in 0..200 {
        fs::write(dir.join(format!("build/out{k}.rs")), "fn ignored_build_output() {}\n")?;
    }
    let total = std::sync::atomic::AtomicU64::new(0);
    let threads = 8;
    std::thread::scope(|sc| {
        for t in 0..threads {
            let total = &total;
            sc.spawn(move || {
                let mut r = Rng(0x9e37_79b9_7f4a_7c15 ^ (t as u64 + 1) * 0x1234_5678);
                let mut i = t;
                while i < n {
                    let d = dir.join(format!("pkg{:02}/mod{:03}", i % 50, (i / 50) % 200));
                    let _ = fs::create_dir_all(&d);
                    let ext = EXTS[i % EXTS.len()];
                    let text = file_text(&mut r, ext, i);
                    total.fetch_add(text.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    let _ = fs::write(d.join(format!("file{i}.{ext}")), text);
                    i += threads;
                }
            });
        }
    });
    Ok(total.into_inner())
}

/// The warm half of the bench (its own process): load the cache, check the disk, query.
fn warm(root: &Path, cache: &Path) -> anyhow::Result<()> {
    let rss0 = rss();
    let cfg = root::Config { watch: false, ..super::config_from(&crate::config::Settings::get().index) };
    let t = Instant::now();
    let idx = Index::new(cfg, cache.to_path_buf());
    let r = idx.add_root(root);
    while !r.view().content_ready {
        std::thread::sleep(Duration::from_millis(1));
    }
    let loaded = t.elapsed();
    let rss_loaded = rss();
    let mut lat = Vec::new();
    for q in ["needle_marker_7", "return", "kalo"] {
        let (_, a, _) = run_find(idx.find(root, None, q.into(), FindOpts::default()).expect("ready"));
        lat.push(format!("{q} {a:.0} ms"));
    }
    idx.wait_idle(Duration::from_secs(600));
    let checked = t.elapsed();
    println!("| warm start (new process): index loaded, searchable | {} ms |", loaded.as_millis());
    println!("| warm start: first queries (cold page cache) | {} |", lat.join(", "));
    println!("| warm start: disk check done (mtime + size of every file) | {} ms |", checked.as_millis());
    println!("| memory (RSS) after warm start | {} loaded, {} after queries + disk check (process before: {}) |", mb(rss_loaded), mb(rss()), mb(rss0));
    Ok(())
}

fn incremental(idx: &Index, r: &std::sync::Arc<super::Root>, root: &Path, files: &[String]) -> anyhow::Result<()> {
    let changed: Vec<PathBuf> = files.iter().step_by((files.len() / 10).max(1)).take(10).map(|f| root.join(f)).collect();
    let t = Instant::now();
    for (k, p) in changed.iter().enumerate() {
        let mut f = fs::OpenOptions::new().append(true).open(p)?;
        writeln!(f, "// fresh_edit_token_{k}")?;
    }
    r.changed(changed.clone());
    idx.wait_idle(Duration::from_secs(60));
    let (_, _, got) = run_find(idx.find(&root, None, "fresh_edit_token_".into(), FindOpts::default()).expect("ready"));
    println!("| {} | {} |", "incremental: 10 edited files → searchable", format!("{} ms (found {got} of 10)", t.elapsed().as_millis()));
    Ok(())
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Runs a find job to the end: (ms to the first batch, ms to all, hits).
fn run_find(job: crate::project::search::Job<Vec<crate::project::search::Hit>>) -> (f64, f64, usize) {
    let t = Instant::now();
    let mut first = None;
    let mut n = 0;
    loop {
        match job.rx.recv_timeout(Duration::from_millis(5)) {
            Ok(b) => {
                first.get_or_insert(t.elapsed());
                n += b.len();
            }
            Err(_) if job.done() => {
                while let Ok(b) = job.rx.try_recv() {
                    first.get_or_insert(t.elapsed());
                    n += b.len();
                }
                break;
            }
            Err(_) => {}
        }
    }
    let all = t.elapsed();
    (first.unwrap_or(all).as_secs_f64() * 1e3, all.as_secs_f64() * 1e3, n)
}

pub fn bench(dir: &Path, n: usize, keep: bool) -> anyhow::Result<()> {
    let scratch = std::env::temp_dir().join(format!("catlas-bench-{}", std::process::id()));
    fs::create_dir_all(&scratch)?;
    // Never the real config or cache.
    std::env::set_var("CATLAS_CONFIG_DIR", scratch.join("config"));
    std::env::set_var("CATLAS_CACHE_DIR", scratch.join("cache-app"));
    let generated = !dir.exists() || fs::read_dir(dir).map(|mut d| d.next().is_none()).unwrap_or(true);
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut row = |k: &str, v: String| {
        println!("| {k} | {v} |");
        rows.push((k.into(), v));
    };
    println!("| measure | result |\n|---|---|");
    if generated {
        let t = Instant::now();
        let bytes = generate(dir, n)?;
        row("synthetic repo", format!("{n} files, {} in {:.1} s (generation, not part of the index)", mb(bytes), t.elapsed().as_secs_f64()));
    }
    let root = crate::project::canonical(dir);
    let cache = scratch.join("cache");
    let cfg = root::Config { watch: false, ..super::config_from(&crate::config::Settings::get().index) };
    let rss0 = rss();

    // Baseline: the old way, walking for the file list and for each search.
    let t = Instant::now();
    let walk = crate::project::search::index_files(root.clone());
    let mut walked = 0;
    while !walk.done() || walk.rx.try_recv().map(|b| walked += b.len()).is_ok() {
        if let Ok(b) = walk.rx.recv_timeout(Duration::from_millis(5)) {
            walked += b.len();
        }
    }
    row("walk for the file list (before: every start)", format!("{walked} files in {} ms", t.elapsed().as_millis()));

    // Cold build.
    let t = Instant::now();
    let idx = Index::new(cfg.clone(), cache.clone());
    let r = idx.add_root(&root);
    while !r.view().files_ready {
        std::thread::sleep(Duration::from_millis(2));
    }
    let list_ms = t.elapsed().as_millis();
    idx.wait_idle(Duration::from_secs(3600));
    let cold = t.elapsed();
    let v = r.view();
    let disk: u64 = v.segs.iter().map(|s| s.disk_bytes).sum();
    row("cold index: file list ready", format!("{list_ms} ms"));
    row("cold index: contents + symbols ready", format!("{:.1} s ({} files, {} symbols, {} segment(s), {} on disk{})", cold.as_secs_f64(), v.files.len(), v.symbols(), v.segs.len(), mb(disk), if v.capped { ", capped at max_files" } else { "" }));
    let rss1 = rss();
    row("memory (RSS) with the index open", format!("{} (+{} over the empty process)", mb(rss1), mb(rss1.saturating_sub(rss0))));
    if let Some(e) = r.error.lock().ok().and_then(|e| e.clone()) {
        row("index error", e);
    }

    // Queries.
    // Identifiers that exist: every n-th symbol name of the index.
    let words: Vec<String> = {
        let seg = &v.segs[0];
        let step = (seg.syms.len() / 20).max(1);
        seg.syms.iter().step_by(step).take(20).map(|s| seg.sym_name(s).to_string()).collect()
    };
    let lat = |label: &str, qs: &[String], per_line: bool| {
        let mut firsts = Vec::new();
        let mut alls = Vec::new();
        let mut hits = 0;
        for q in qs {
            let job = idx.find(&root, None, q.clone(), FindOpts { per_line, boost: Vec::new() }).expect("index ready");
            let (f, a, h) = run_find(job);
            firsts.push(f);
            alls.push(a);
            hits += h;
        }
        format!(
            "{label}: first rows p50 {:.1} ms / p95 {:.1} ms, all p50 {:.1} / p95 {:.1} ms ({} queries, {:.0} hits avg)",
            pct(&mut firsts.clone(), 0.5),
            pct(&mut firsts, 0.95),
            pct(&mut alls.clone(), 0.5),
            pct(&mut alls, 0.95),
            qs.len(),
            hits as f64 / qs.len().max(1) as f64
        )
    };
    let rare: Vec<String> = (0..4).map(|k| format!("needle_marker_{}", k * 50_000 + 7)).chain(["zenkaloFle".into(), "nonexistent_token_xyz".into()]).collect();
    row("find in files, selective (index)", lat("rare", &rare, false));
    row("find in files, identifiers (index)", lat("camel", &words[..10], true));
    let common: Vec<String> = ["return", "value", "self", "x +"].iter().map(|s| s.to_string()).collect();
    row("find in files, common words (index, capped at 2000 hits)", lat("common", &common, true));
    let mut scan = Vec::new();
    for q in rare.iter().take(3) {
        let (_, a, _) = run_find(crate::project::search::find_in_files(root.clone(), q.clone()));
        scan.push(a);
    }
    row("find in files, selective (before: walk + read everything)", format!("p50 {:.0} ms", pct(&mut scan, 0.5)));

    let files = v.files.clone();
    let mut ft = Vec::new();
    for q in ["file123", "pkg07mod", "f99j", "mod1/file", "zz"] {
        let t = Instant::now();
        let mut m = 0;
        for f in files.iter() {
            if crate::project::fuzzy::score(q, f).is_some() {
                m += 1;
            }
        }
        std::hint::black_box(m);
        ft.push(t.elapsed().as_secs_f64() * 1e3);
    }
    row("Go to File: fuzzy over the whole list", format!("p50 {:.0} ms / p95 {:.0} ms ({} paths)", pct(&mut ft.clone(), 0.5), pct(&mut ft, 0.95), files.len()));

    let mut st = Vec::new();
    let mut sh = 0;
    for q in words.iter().take(10).map(|w| w[..w.len().min(6)].to_string()).chain(["kalo".into(), "PubFn".into(), "zxq".into()]) {
        let t = Instant::now();
        sh += idx.symbols_now(None, None, &q, 40).len();
        st.push(t.elapsed().as_secs_f64() * 1e3);
    }
    row("symbols (Search Everywhere / Alt+N)", format!("p50 {:.1} ms / p95 {:.1} ms ({:.0} results avg, top 40)", pct(&mut st.clone(), 0.5), pct(&mut st, 0.95), sh as f64 / 13.0));

    // Incremental: change files, see them in a query (only in a generated repo: a real folder
    // is never written to).
    if generated {
        incremental(&idx, &r, &root, &files)?;
    }
    drop(r);
    drop(idx);
    std::thread::sleep(Duration::from_millis(100));

    // Warm start, in a fresh process (honest memory numbers).
    let _ = cfg;
    let out = std::process::Command::new(std::env::current_exe()?).arg("--bench-warm").arg(&root).arg(&cache).output()?;
    print!("{}", String::from_utf8_lossy(&out.stdout));

    // A folder with 100 000 entries in the tree.
    let big = scratch.join("bigdir");
    fs::create_dir_all(big.join("flat"))?;
    for i in 0..100_000 {
        fs::write(big.join(format!("flat/f{i:06}.txt")), "")?;
    }
    let t = Instant::now();
    let mut tree = crate::project::tree::Tree::new(&big);
    tree.expand(1);
    row("tree: expand a folder of 100 000 files", format!("{} ms ({} rows)", t.elapsed().as_millis(), tree.rows.len()));

    // A large file in the editor.
    fs::create_dir_all(scratch.join("huge"))?;
    let huge = scratch.join("huge/huge.rs");
    {
        let mut w = std::io::BufWriter::new(fs::File::create(&huge)?);
        let mut rr = Rng(7);
        while w.get_ref().metadata()?.len() < 40 << 20 {
            for _ in 0..1000 {
                writeln!(w, "fn {}() {{ let x = {}; }} // {}", camel(&mut rr), rr.below(1000), word(&mut rr))?;
            }
            w.flush()?;
        }
        writeln!(w, "{}", "x".repeat(200_000))?;
    }
    let t = Instant::now();
    let mut app = crate::app::App::new(vec![huge.clone()])?;
    let open_ms = t.elapsed().as_millis();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 50))?;
    let t = Instant::now();
    term.draw(|f| crate::ui::draw(f, &mut app))?;
    let first = t.elapsed();
    let mut keys = Vec::new();
    for c in "hello".chars() {
        let t = Instant::now();
        app.handle(crossterm::event::Event::Key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char(c), crossterm::event::KeyModifiers::NONE)));
        app.tick();
        term.draw(|f| crate::ui::draw(f, &mut app))?;
        keys.push(t.elapsed().as_secs_f64() * 1e3);
    }
    app.handle(crossterm::event::Event::Key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::End, crossterm::event::KeyModifiers::CONTROL)));
    let t = Instant::now();
    term.draw(|f| crate::ui::draw(f, &mut app))?;
    let long = t.elapsed();
    row(
        "large file (40 MB + a 200 000-char line)",
        format!(
            "open {open_ms} ms, first draw {:.1} ms, keystroke + draw p50 {:.1} ms / max {:.1} ms, draw at the long line {:.1} ms",
            first.as_secs_f64() * 1e3,
            pct(&mut keys.clone(), 0.5),
            pct(&mut keys, 1.0),
            long.as_secs_f64() * 1e3
        ),
    );
    drop(app);

    if !keep {
        let _ = fs::remove_dir_all(&scratch);
        if generated {
            let _ = fs::remove_dir_all(dir);
        }
    } else {
        println!("kept: {} and {}", scratch.display(), dir.display());
    }
    Ok(())
}
