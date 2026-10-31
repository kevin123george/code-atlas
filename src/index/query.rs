//! Queries over a root's [`View`]: find in files (trigram candidates, checked against the real
//! contents, then ranked) and symbols (letter masks, then the finder's fuzzy scorer).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::root::View;
use super::segment::{letter_masks, F_NO_CONTENT};
use super::symbols::Kind;
use super::trigram::{intersect, query_trigrams};
use crate::project::fuzzy;
use crate::project::search::{search_file, Hit, MAX_HITS};

/// How hits are ordered.
#[derive(Clone, Debug, Default)]
pub struct FindOpts {
    /// One row per line, best lines first (Search Everywhere); otherwise files by their best
    /// line, each file's hits in line order (Find in Files).
    pub per_line: bool,
    /// Paths (relative) that rank higher: recently opened files.
    pub boost: Vec<String>,
}

/// A file to check: its path and, for indexed files, where its symbols are.
struct Cand {
    path: String,
    at: Option<(usize, u32)>,
    bonus: i32,
}

fn in_scope(path: &str, scope: Option<&str>) -> bool {
    scope.is_none_or(|s| s.is_empty() || path.strip_prefix(s).is_some_and(|r| r.starts_with('/')))
}

/// Folders whose files usually aren't what one looks for first.
fn noise(path: &str) -> bool {
    const DIRS: &[&str] = &["test", "tests", "__tests__", "spec", "vendor", "third_party", "dist", "build", "out", "generated", "fixtures", "testdata", "examples"];
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').rev().skip(1).any(|c| DIRS.contains(&c)) || name.contains(".min.") || name.contains(".generated.") || name.ends_with(".lock") || name.ends_with(".map")
}

/// File-level part of a hit's score: the query in the file name, recently opened, shallow,
/// not in a test / vendor folder.
fn file_bonus(path: &str, q_low: &str, boost: &[String]) -> i32 {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let stem = name.split('.').next().unwrap_or("");
    let mut b = 0;
    if stem == q_low {
        b += 40;
    } else if name.contains(q_low) {
        b += 20;
    }
    if boost.iter().any(|p| p == path) {
        b += 20;
    }
    if noise(path) {
        b -= 15;
    }
    b - path.matches('/').count().min(10) as i32
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Line-level part: whole word, exact case, a definition of the query on this line.
fn line_score(h: &Hit, query: &str, def: i32) -> i32 {
    let chars: Vec<char> = h.text.chars().collect();
    let len = h.end - h.start;
    let s = h.text_start.min(chars.len());
    let e = (s + len).min(chars.len());
    let mut score = def;
    if s == 0 || !is_ident(chars[s - 1]) {
        score += 10;
    }
    if e >= chars.len() || !is_ident(chars[e]) {
        score += 10;
    }
    if chars[s..e].iter().copied().eq(query.chars()) {
        score += 8;
    }
    if chars.len() > 300 {
        score -= 10;
    }
    score
}

/// The files that may contain `needle` (lowercase bytes), best guesses first.
fn candidates(v: &View, needle: &[u8], scope: Option<&str>, q_low: &str, boost: &[String]) -> Vec<Cand> {
    let tris = query_trigrams(needle);
    let mut out = Vec::new();
    for (si, seg) in v.segs.iter().enumerate() {
        let alive = &v.alive[si];
        let ids: Vec<u32> = if tris.is_empty() {
            (0..seg.len() as u32).collect()
        } else {
            // Shortest lists first: the intersection shrinks fastest.
            let mut sized: Vec<(u64, u32)> = Vec::with_capacity(tris.len());
            let mut missing = false;
            for &t in &tris {
                match seg.posting_bytes(t) {
                    Some(n) => sized.push((n, t)),
                    None => missing = true,
                }
            }
            if missing {
                continue;
            }
            sized.sort_unstable();
            let mut ids: Option<Vec<u32>> = None;
            for (_, t) in sized {
                let p = seg.postings(t).unwrap_or_default();
                let next = match ids {
                    None => p,
                    Some(i) => intersect(&i, &p),
                };
                let empty = next.is_empty();
                ids = Some(next);
                if empty {
                    break;
                }
            }
            ids.unwrap_or_default()
        };
        for id in ids {
            let i = id as usize;
            if !alive[i] || seg.meta[i].flags & F_NO_CONTENT != 0 {
                continue;
            }
            let path = seg.path(i);
            if in_scope(path, scope) {
                out.push(Cand { path: path.to_string(), at: Some((si, id)), bonus: file_bonus(path, q_low, boost) });
            }
        }
    }
    for p in v.pending.iter() {
        if in_scope(p, scope) {
            out.push(Cand { path: p.clone(), at: None, bonus: file_bonus(p, q_low, boost) });
        }
    }
    out.sort_by(|a, b| b.bonus.cmp(&a.bonus).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Ranks one file's hits: (file score, hits with their scores).
fn rank_file(v: &View, c: &Cand, hits: Vec<Hit>, query: &str, q_low: &str) -> (i32, Vec<(i32, Hit)>) {
    let defs: Vec<(u32, i32)> = match c.at {
        Some((si, id)) => {
            let seg = &v.segs[si];
            seg.file_syms(id)
                .iter()
                .filter_map(|s| {
                    let name = seg.sym_name(s);
                    if name.eq_ignore_ascii_case(query) {
                        Some((s.line, 50))
                    } else if name.to_ascii_lowercase().contains(q_low) {
                        Some((s.line, 15))
                    } else {
                        None
                    }
                })
                .collect()
        }
        None => Vec::new(),
    };
    let scored: Vec<(i32, Hit)> = hits
        .into_iter()
        .map(|h| {
            let def = defs.iter().filter(|(l, _)| *l as usize == h.line).map(|(_, b)| *b).max().unwrap_or(0);
            (line_score(&h, query, def) + c.bonus, h)
        })
        .collect();
    let density = (scored.len() as i32).min(5);
    let best = scored.iter().map(|(s, _)| *s).max().unwrap_or(0) + density;
    (best, scored)
}

/// Finds `query` in the files of `v` (smart case). Hits come as ranked batches: the first
/// after at most ~250 ms (or when all candidates are read), the rest at the end.
pub fn find(root: PathBuf, v: Arc<View>, scope: Option<String>, query: String, opts: FindOpts) -> crate::project::search::Job<Vec<Hit>> {
    crate::project::search::spawn(move |tx, cancel| {
        let ignore_case = !query.chars().any(char::is_uppercase);
        let q_low = query.to_ascii_lowercase();
        let needle: Vec<u8> = if ignore_case { q_low.clone().into_bytes() } else { query.clone().into_bytes() };
        if needle.is_empty() {
            return;
        }
        let cands = candidates(&v, q_low.as_bytes(), scope.as_deref(), &q_low, &opts.boost);
        let next = AtomicUsize::new(0);
        let hits = AtomicUsize::new(0);
        let found: Mutex<Vec<(i32, Vec<(i32, Hit)>)>> = Mutex::new(Vec::new());
        let busy = AtomicUsize::new(0);
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8)).min(cands.len().max(1));
        let t0 = Instant::now();
        let send = |batch: Vec<(i32, Vec<(i32, Hit)>)>| {
            if batch.is_empty() {
                return true;
            }
            tx.send(order(batch, opts.per_line)).is_ok()
        };
        std::thread::scope(|sc| {
            for _ in 0..threads {
                busy.fetch_add(1, Ordering::SeqCst);
                sc.spawn(|| {
                    loop {
                        if cancel.load(Ordering::Relaxed) || hits.load(Ordering::Relaxed) >= MAX_HITS {
                            break;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(c) = cands.get(i) else { break };
                        let mut h = search_file(&root.join(&c.path), &needle, ignore_case);
                        if h.is_empty() {
                            continue;
                        }
                        for x in &mut h {
                            x.path = c.path.clone();
                        }
                        if opts.per_line {
                            h.dedup_by(|b, a| a.line == b.line);
                        }
                        hits.fetch_add(h.len(), Ordering::Relaxed);
                        let r = rank_file(&v, c, h, &query, &q_low);
                        found.lock().unwrap().push(r);
                    }
                    busy.fetch_sub(1, Ordering::SeqCst);
                });
            }
            let mut sent_first = false;
            while busy.load(Ordering::SeqCst) > 0 {
                std::thread::sleep(Duration::from_millis(10));
                if !sent_first && t0.elapsed() >= Duration::from_millis(250) {
                    let batch = std::mem::take(&mut *found.lock().unwrap());
                    if !batch.is_empty() {
                        sent_first = true;
                        // A closed receiver also sets `cancel` (the job was dropped).
                        send(batch);
                    }
                }
            }
        });
        let batch = std::mem::take(&mut *found.lock().unwrap());
        send(batch);
    })
}

/// Orders ranked files into the hits list (see [`FindOpts::per_line`]), at most [`MAX_HITS`].
fn order(mut files: Vec<(i32, Vec<(i32, Hit)>)>, per_line: bool) -> Vec<Hit> {
    if per_line {
        let mut all: Vec<(i32, Hit)> = files.into_iter().flat_map(|(_, h)| h).collect();
        all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)).then(a.1.line.cmp(&b.1.line)));
        return all.into_iter().take(MAX_HITS).map(|(_, h)| h).collect();
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1[0].1.path.cmp(&b.1[0].1.path)));
    files.into_iter().flat_map(|(_, h)| h.into_iter().map(|(_, h)| h)).take(MAX_HITS).collect()
}

// ---- symbols ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolHit {
    /// The root it was found in, and the file relative to it.
    pub root: PathBuf,
    pub path: String,
    pub name: String,
    pub kind: Kind,
    /// 0-based line and char column of the name.
    pub line: u32,
    pub col: u32,
    pub score: i32,
}

impl SymbolHit {
    pub fn abs(&self) -> PathBuf {
        self.root.join(&self.path)
    }
}

/// The best `limit` symbols for `query` over the given roots' views (fuzzy, IDEA-like: the
/// first letter must start a word of the name).
pub fn symbols(views: &[(PathBuf, Arc<View>)], scope: Option<&str>, query: &str, limit: usize, cancel: &AtomicBool) -> Vec<SymbolHit> {
    let q: String = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Vec::new();
    }
    let qb = q.as_bytes();
    let (q_all, _) = letter_masks(qb);
    let (first, _) = letter_masks(&qb[..1]);
    let q_low = q.to_ascii_lowercase();
    let mut out: Vec<SymbolHit> = Vec::new();
    let mut seen = 0usize;
    for (root, v) in views {
        for (si, seg) in v.segs.iter().enumerate() {
            let alive = &v.alive[si];
            for s in &seg.syms {
                seen += 1;
                if seen % 8192 == 0 && cancel.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                if s.all & q_all != q_all || s.starts & first == 0 || !alive[s.file as usize] {
                    continue;
                }
                let name = seg.sym_name(s);
                let Some((score, _)) = fuzzy::score(&q, name) else { continue };
                let path = seg.path(s.file as usize);
                if !in_scope(path, scope) {
                    continue;
                }
                let kind = Kind::from_u8(s.kind);
                let low = name.to_ascii_lowercase();
                let mut sc = score;
                if name == q {
                    sc += 220;
                } else if low == q_low {
                    sc += 200;
                } else if low.starts_with(&q_low) {
                    sc += 80;
                }
                if kind.is_type() {
                    sc += 10;
                }
                if noise(path) {
                    sc -= 15;
                }
                sc -= path.matches('/').count().min(10) as i32;
                out.push(SymbolHit { root: root.clone(), path: path.to_string(), name: name.to_string(), kind, line: s.line, col: s.col, score: sc });
                if out.len() > limit * 8 + 256 {
                    trim(&mut out, limit);
                }
            }
        }
    }
    trim(&mut out, limit);
    out
}

fn trim(v: &mut Vec<SymbolHit>, limit: usize) {
    v.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.len().cmp(&b.name.len())).then_with(|| a.path.cmp(&b.path)).then(a.line.cmp(&b.line)));
    v.truncate(limit);
}
