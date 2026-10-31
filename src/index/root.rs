//! One indexed folder (a "root"): a background worker that loads the index from the cache,
//! checks it against the disk (mtime + size), indexes what changed, follows file-system events
//! and compacts its segments; queries read the immutable [`View`] it publishes.
//!
//! The worker owns all mutable state. A path's newest entry wins (`by_path`), so an update is
//! "write a small segment with the changed files and tombstones for deleted ones". Files that
//! changed but aren't in a segment yet are `pending`: queries read them directly.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use ignore::{WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};

use super::segment::{self, Builder, FileMeta, Segment, F_DELETED, F_NO_CONTENT};
use super::symbols;
use super::trigram::{fnv64, Extractor};
use crate::project::tree::skip_name;

/// Changed files above this count make the content index "not ready" until they're indexed
/// (reading that many files per query would be slower than the plain scan).
const MAX_PENDING: usize = 5000;
/// Compact when there are more segments than this.
const MAX_SEGMENTS: usize = 6;
/// Symbols are extracted from files up to this size.
const MAX_SYMBOL_FILE: u64 = 1 << 20;
const MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct Config {
    /// Bigger files are indexed by name only.
    pub max_file: u64,
    /// Stop listing after this many files.
    pub max_files: usize,
    pub symbols: bool,
    pub watch: bool,
    /// Memory for segments being built (all threads together), bytes.
    pub budget: usize,
    pub threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_file: 4 << 20,
            max_files: 1_000_000,
            symbols: true,
            watch: true,
            budget: 256 << 20,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Loading,
    Scanning,
    Indexing,
    Merging,
    Idle,
    Stopped,
}

impl Phase {
    fn from_u8(v: u8) -> Phase {
        [Phase::Loading, Phase::Scanning, Phase::Indexing, Phase::Merging, Phase::Idle, Phase::Stopped][v.min(5) as usize]
    }
}

/// What queries see: never changed once published.
pub struct View {
    pub gen: u64,
    /// Live files, relative, sorted.
    pub files: Arc<Vec<String>>,
    /// The file list is complete (the first scan finished or the cache was loaded).
    pub files_ready: bool,
    /// Content and symbol queries can use the index.
    pub content_ready: bool,
    pub segs: Vec<Arc<Segment>>,
    /// Per segment and entry: this is the live version of a file that exists.
    pub alive: Vec<Arc<Vec<bool>>>,
    /// Changed files not in a segment yet (queries read them directly).
    pub pending: Arc<Vec<String>>,
    /// The listing stopped at `max_files`.
    pub capped: bool,
}

impl View {
    fn empty() -> View {
        View { gen: 0, files: Arc::new(Vec::new()), files_ready: false, content_ready: false, segs: Vec::new(), alive: Vec::new(), pending: Arc::new(Vec::new()), capped: false }
    }

    pub fn symbols(&self) -> usize {
        self.segs.iter().zip(&self.alive).map(|(s, a)| s.syms.iter().filter(|y| a[y.file as usize]).count()).sum()
    }
}

enum Msg {
    Changed(Vec<PathBuf>),
    Rescan,
    Stop,
}

pub struct Root {
    pub path: PathBuf,
    /// This root's folder in the cache.
    pub dir: PathBuf,
    view: RwLock<Arc<View>>,
    phase: AtomicU8,
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    tx: Mutex<Sender<Msg>>,
    queued: AtomicUsize,
    pub read_only: AtomicBool,
    pub error: Mutex<Option<String>>,
    /// Time of the last finished scan / update, for `--index-status`.
    pub last_ms: AtomicU64,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    root: String,
    segments: Vec<u64>,
    next_seq: u64,
    max_file: u64,
    symbols: bool,
}

/// A file as seen on disk.
#[derive(Clone, Debug)]
pub struct Entry {
    pub rel: String,
    pub mtime: i64,
    pub size: u64,
}

fn mtime_ns(m: &fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64)
}

pub fn seg_name(seq: u64) -> String {
    format!("seg-{seq:08}.cidx")
}

/// The cache folder for `root` under `cache`: `index/<name>-<hash>`.
pub fn root_dir(cache: &Path, root: &Path) -> PathBuf {
    let name: String = root.file_name().map(|n| n.to_string_lossy().chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(40).collect()).unwrap_or_default();
    cache.join("index").join(format!("{name}-{:016x}", fnv64(root.to_string_lossy().as_bytes())))
}

/// A gitignore-aware walker over `dir` with the project tree's rules ([`skip_name`]), that
/// also skips the cache folder when it lies inside the root.
pub fn walker(dir: &Path, skip: Option<PathBuf>, depth: Option<usize>) -> WalkBuilder {
    let mut w = WalkBuilder::new(dir);
    w.hidden(false).require_git(false).follow_links(false).max_depth(depth).filter_entry(move |e| {
        let is_dir = e.file_type().is_some_and(|t| t.is_dir());
        !skip_name(&e.file_name().to_string_lossy(), is_dir) && skip.as_ref().is_none_or(|s| !e.path().starts_with(s))
    });
    w
}

fn rel_of(root: &Path, p: &Path) -> Option<String> {
    let r = p.strip_prefix(root).ok()?;
    Some(r.to_string_lossy().replace('\\', "/"))
}

fn parent_of(rel: &str) -> &str {
    rel.rfind('/').map_or("", |i| &rel[..i])
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty() || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/')
}

impl Root {
    /// Starts the worker for `path` (canonical) with its cache under `cache`.
    pub fn start(path: PathBuf, cache: &Path, cfg: Config) -> Arc<Root> {
        let (tx, rx) = channel();
        let root = Arc::new(Root {
            dir: root_dir(cache, &path),
            path,
            view: RwLock::new(Arc::new(View::empty())),
            phase: AtomicU8::new(Phase::Loading as u8),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            tx: Mutex::new(tx),
            queued: AtomicUsize::new(1),
            read_only: AtomicBool::new(false),
            error: Mutex::new(None),
            last_ms: AtomicU64::new(0),
        });
        let cache = cache.to_path_buf();
        let r = root.clone();
        std::thread::Builder::new()
            .name("catlas-index".into())
            .spawn(move || Worker::new(r, cfg, cache).run(rx))
            .expect("index thread");
        root
    }

    pub fn view(&self) -> Arc<View> {
        self.view.read().map(|v| v.clone()).unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Relaxed))
    }

    fn send(&self, m: Msg) {
        self.queued.fetch_add(1, Ordering::SeqCst);
        if self.tx.lock().map(|t| t.send(m).is_err()).unwrap_or(true) {
            self.queued.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Files or folders changed on disk (the watcher calls this; tests too).
    pub fn changed(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.send(Msg::Changed(paths));
        }
    }

    /// Checks the whole root against the disk again.
    pub fn rescan(&self) {
        self.send(Msg::Rescan);
    }

    pub fn stop(&self) {
        self.send(Msg::Stop);
    }

    /// Nothing queued and nothing running.
    pub fn idle(&self) -> bool {
        self.queued.load(Ordering::SeqCst) == 0 && matches!(self.phase(), Phase::Idle | Phase::Stopped)
    }
}

struct Worker {
    root: Arc<Root>,
    cfg: Config,
    cache: PathBuf,
    segs: Vec<Arc<Segment>>,
    /// Path hash → (segment index, entry): the newest entry of each path.
    by_path: HashMap<u64, (u32, u32)>,
    files: Arc<Vec<String>>,
    /// Known (listed, not ignored) folders, relative; "" is the root.
    dirs: HashSet<String>,
    pending: Vec<String>,
    next_seq: u64,
    lock: Option<File>,
    gen: u64,
    files_ready: bool,
    content_ready: bool,
    capped: bool,
}

impl Worker {
    fn new(root: Arc<Root>, cfg: Config, cache: PathBuf) -> Worker {
        Worker {
            root,
            cfg,
            cache,
            segs: Vec::new(),
            by_path: HashMap::new(),
            files: Arc::new(Vec::new()),
            dirs: HashSet::new(),
            pending: Vec::new(),
            next_seq: 1,
            lock: None,
            gen: 0,
            files_ready: false,
            content_ready: false,
            capped: false,
        }
    }

    fn set_phase(&self, p: Phase) {
        self.root.phase.store(p as u8, Ordering::Relaxed);
    }

    fn fail(&self, what: String) {
        if let Ok(mut e) = self.root.error.lock() {
            *e = Some(what);
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        self.open_cache();
        self.publish();
        self.full_scan();
        self.root.queued.fetch_sub(1, Ordering::SeqCst);
        let _watcher = if self.cfg.watch { super::watch::start(self.root.clone(), self.cache.clone()) } else { None };
        self.publish();
        self.set_phase(Phase::Idle);
        loop {
            let Ok(m) = rx.recv() else { break };
            let mut stop = false;
            let mut rescan = false;
            let mut changed: Vec<PathBuf> = Vec::new();
            let mut taken = 1;
            fn absorb(m: Msg, changed: &mut Vec<PathBuf>, rescan: &mut bool, stop: &mut bool) {
                match m {
                    Msg::Changed(p) => changed.extend(p),
                    Msg::Rescan => *rescan = true,
                    Msg::Stop => *stop = true,
                }
            }
            absorb(m, &mut changed, &mut rescan, &mut stop);
            // Gather a burst of events (an agent writing many files, a git checkout).
            let t = Instant::now();
            while !stop && !changed.is_empty() && t.elapsed() < Duration::from_secs(2) {
                match rx.recv_timeout(Duration::from_millis(120)) {
                    Ok(m) => {
                        taken += 1;
                        absorb(m, &mut changed, &mut rescan, &mut stop);
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        stop = true;
                        break;
                    }
                }
            }
            if stop {
                self.root.queued.fetch_sub(taken, Ordering::SeqCst);
                break;
            }
            if rescan {
                self.full_scan();
            } else if !changed.is_empty() {
                self.update(changed);
            }
            self.set_phase(Phase::Idle);
            self.root.queued.fetch_sub(taken, Ordering::SeqCst);
        }
        self.set_phase(Phase::Stopped);
    }

    // ---- cache -------------------------------------------------------------------------------

    fn manifest_path(&self) -> PathBuf {
        self.root.dir.join("manifest.json")
    }

    /// Takes the cache folder's lock (another catlas on the same folder makes this one read
    /// only) and loads the segments the manifest lists. Anything broken: start from scratch.
    fn open_cache(&mut self) {
        let dir = self.root.dir.clone();
        if let Err(e) = fs::create_dir_all(&dir) {
            self.fail(format!("cache folder {}: {e}", dir.display()));
            self.root.read_only.store(true, Ordering::Relaxed);
            return;
        }
        match File::options().create(true).truncate(false).write(true).open(dir.join("lock")) {
            Ok(f) if f.try_lock().is_ok() => self.lock = Some(f),
            _ => self.root.read_only.store(true, Ordering::Relaxed),
        }
        let Some(m) = fs::read(self.manifest_path()).ok().and_then(|b| serde_json::from_slice::<Manifest>(&b).ok()) else {
            self.wipe();
            return;
        };
        if m.version != MANIFEST_VERSION || m.root != self.root.path.to_string_lossy() || m.max_file != self.cfg.max_file || m.symbols != self.cfg.symbols {
            self.wipe();
            return;
        }
        let mut segs = Vec::new();
        for &seq in &m.segments {
            match Segment::open(&dir.join(seg_name(seq)), seq) {
                Ok(s) => segs.push(Arc::new(s)),
                Err(e) => {
                    self.fail(format!("index cache was broken, rebuilding ({e})"));
                    self.wipe();
                    return;
                }
            }
        }
        self.next_seq = m.next_seq;
        self.segs = segs;
        self.rebuild_by_path();
        self.files = Arc::new(self.live_paths());
        // Folders of the known files (a scan will fill in empty ones).
        for f in self.files.iter() {
            let mut p = parent_of(f);
            while self.dirs.insert(p.to_string()) && !p.is_empty() {
                p = parent_of(p);
            }
        }
        self.dirs.insert(String::new());
        self.files_ready = !self.segs.is_empty();
        self.content_ready = !self.segs.is_empty();
        if self.lock.is_some() {
            // Leftovers of an interrupted write.
            let listed: HashSet<String> = m.segments.iter().map(|s| seg_name(*s)).collect();
            for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if (n.starts_with("seg-") && !listed.contains(&n)) || n.ends_with(".tmp") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    /// Forgets the cache: no segments, and (when we own the folder) no files.
    fn wipe(&mut self) {
        self.segs.clear();
        self.by_path.clear();
        if self.lock.is_some() {
            for e in fs::read_dir(&self.root.dir).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("seg-") || n == "manifest.json" || n.ends_with(".tmp") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    fn save_manifest(&self) {
        if self.lock.is_none() {
            return;
        }
        let m = Manifest {
            version: MANIFEST_VERSION,
            root: self.root.path.to_string_lossy().into_owned(),
            segments: self.segs.iter().map(|s| s.seq).collect(),
            next_seq: self.next_seq,
            max_file: self.cfg.max_file,
            symbols: self.cfg.symbols,
        };
        let tmp = self.root.dir.join("manifest.tmp");
        let ok = serde_json::to_vec(&m).ok().is_some_and(|b| fs::write(&tmp, b).is_ok()) && fs::rename(&tmp, self.manifest_path()).is_ok();
        if !ok {
            self.fail("couldn't write the index manifest".into());
        }
    }

    // ---- state -------------------------------------------------------------------------------

    fn rebuild_by_path(&mut self) {
        self.by_path.clear();
        for (si, s) in self.segs.iter().enumerate() {
            for i in 0..s.len() {
                self.by_path.insert(fnv64(s.path(i).as_bytes()), (si as u32, i as u32));
            }
        }
    }

    fn meta_of(&self, rel: &str) -> Option<FileMeta> {
        let &(s, i) = self.by_path.get(&fnv64(rel.as_bytes()))?;
        Some(self.segs[s as usize].meta[i as usize])
    }

    fn live_paths(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .by_path
            .values()
            .filter(|(s, i)| self.segs[*s as usize].meta[*i as usize].flags & F_DELETED == 0)
            .map(|(s, i)| self.segs[*s as usize].path(*i as usize).to_string())
            .collect();
        v.sort_unstable();
        v
    }

    fn publish(&mut self) {
        let mut alive: Vec<Vec<bool>> = self.segs.iter().map(|s| vec![false; s.len()]).collect();
        for &(s, i) in self.by_path.values() {
            if self.segs[s as usize].meta[i as usize].flags & F_DELETED == 0 {
                alive[s as usize][i as usize] = true;
            }
        }
        for p in &self.pending {
            if let Some(&(s, i)) = self.by_path.get(&fnv64(p.as_bytes())) {
                alive[s as usize][i as usize] = false;
            }
        }
        self.gen += 1;
        let v = View {
            gen: self.gen,
            files: self.files.clone(),
            files_ready: self.files_ready,
            content_ready: self.content_ready && self.pending.len() <= MAX_PENDING,
            segs: self.segs.clone(),
            alive: alive.into_iter().map(Arc::new).collect(),
            pending: Arc::new(self.pending.clone()),
            capped: self.capped,
        };
        if let Ok(mut w) = self.root.view.write() {
            *w = Arc::new(v);
        }
    }

    /// Applies added / removed paths to the sorted file list.
    fn patch_files(&mut self, added: &[String], removed: &[String]) {
        let files = Arc::make_mut(&mut self.files);
        if added.len() + removed.len() > 256 {
            let gone: HashSet<&str> = removed.iter().map(String::as_str).collect();
            files.retain(|f| !gone.contains(f.as_str()));
            files.extend(added.iter().cloned());
            files.sort_unstable();
            files.dedup();
            return;
        }
        for r in removed {
            if let Ok(i) = files.binary_search(r) {
                files.remove(i);
            }
        }
        for a in added {
            if let Err(i) = files.binary_search(a) {
                files.insert(i, a.clone());
            }
        }
    }

    // ---- scanning ----------------------------------------------------------------------------

    /// Full check of the root against the disk: new, changed and deleted files are indexed.
    fn full_scan(&mut self) {
        self.set_phase(Phase::Scanning);
        let t = Instant::now();
        let (seen, dirs, capped) = walk_all(&self.root.path, &self.cache, self.cfg.threads, None, self.cfg.max_files);
        self.capped = capped;
        let mut changed = Vec::new();
        let mut seen_set: HashSet<u64> = HashSet::with_capacity(seen.len());
        for e in &seen {
            seen_set.insert(fnv64(e.rel.as_bytes()));
            if !self.same(e) {
                changed.push(e.clone());
            }
        }
        let deleted: Vec<String> = self.files.iter().filter(|f| !seen_set.contains(&fnv64(f.as_bytes()))).cloned().collect();
        self.dirs = dirs.into_iter().collect();
        self.dirs.insert(String::new());
        if !self.files_ready {
            let mut list: Vec<String> = seen.iter().map(|e| e.rel.clone()).collect();
            list.sort_unstable();
            self.files = Arc::new(list);
            self.files_ready = true;
            self.pending = changed.iter().map(|e| e.rel.clone()).collect();
            self.publish();
        }
        self.apply(changed, deleted);
        self.root.last_ms.store(t.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Whether `e` matches the index's entry (same mtime and size).
    fn same(&self, e: &Entry) -> bool {
        self.meta_of(&e.rel).is_some_and(|m| m.flags & F_DELETED == 0 && m.mtime == e.mtime && m.size == e.size)
    }

    /// Paths changed on disk (absolute files or folders): re-lists the folders they're in and
    /// indexes what differs. New folders are walked; folders that went away drop their files.
    fn update(&mut self, paths: Vec<PathBuf>) {
        self.set_phase(Phase::Scanning);
        let root = self.root.path.clone();
        let mut list: HashSet<String> = HashSet::new();
        for p in &paths {
            let Some(rel) = rel_of(&root, p) else { continue };
            if self.dirs.contains(&rel) {
                list.insert(rel.clone());
            }
            // The nearest known folder above it: its listing shows new folders too.
            let mut d = parent_of(&rel);
            if !rel.is_empty() {
                loop {
                    if self.dirs.contains(d) {
                        list.insert(d.to_string());
                        break;
                    }
                    if d.is_empty() {
                        break;
                    }
                    d = parent_of(d);
                }
            }
        }
        let mut changed: Vec<Entry> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();
        let mut dirs: Vec<String> = list.into_iter().collect();
        dirs.sort();
        for d in dirs {
            if !self.dirs.contains(&d) {
                continue; // dropped meanwhile (a parent went away)
            }
            let abs = if d.is_empty() { root.clone() } else { root.join(&d) };
            if !abs.is_dir() {
                self.drop_dir(&d, &mut deleted);
                continue;
            }
            let (seen, sub, _) = walk_all(&root, &self.cache, 1, Some((&abs, 1)), usize::MAX);
            let seen_names: HashSet<&str> = seen.iter().map(|e| e.rel.as_str()).collect();
            for e in &seen {
                if !self.same(e) {
                    changed.push(e.clone());
                }
            }
            // Direct children the index has but the folder doesn't.
            for f in self.children(&d) {
                if !seen_names.contains(f.as_str()) {
                    deleted.push(f);
                }
            }
            let sub_set: HashSet<&str> = sub.iter().map(String::as_str).filter(|s| *s != d).collect();
            let known_subs: Vec<String> = self.dirs.iter().filter(|k| !k.is_empty() && *k != &d && parent_of(k) == d).cloned().collect();
            for k in known_subs {
                if !sub_set.contains(k.as_str()) {
                    self.drop_dir(&k, &mut deleted);
                }
            }
            for s in sub_set {
                if !self.dirs.contains(s) {
                    // A new folder: everything in it is new.
                    let (files, subdirs, _) = walk_all(&root, &self.cache, self.cfg.threads, Some((&root.join(s), usize::MAX)), self.cfg.max_files);
                    changed.extend(files.into_iter().filter(|e| !self.same(e)));
                    self.dirs.extend(subdirs);
                    self.dirs.insert(s.to_string());
                }
            }
        }
        changed.sort_by(|a, b| a.rel.cmp(&b.rel));
        changed.dedup_by(|a, b| a.rel == b.rel);
        deleted.sort();
        deleted.dedup();
        self.apply(changed, deleted);
    }

    /// The indexed files directly in folder `d`.
    fn children(&self, d: &str) -> Vec<String> {
        let files = &self.files;
        let (lo, hi) = if d.is_empty() {
            (0, files.len())
        } else {
            let pre = format!("{d}/");
            let lo = files.partition_point(|f| f.as_str() < pre.as_str());
            let hi = lo + files[lo..].partition_point(|f| f.starts_with(&pre));
            (lo, hi)
        };
        files[lo..hi].iter().filter(|f| parent_of(f) == d).cloned().collect()
    }

    fn drop_dir(&mut self, d: &str, deleted: &mut Vec<String>) {
        deleted.extend(self.files.iter().filter(|f| under(f, d)).cloned());
        self.dirs.retain(|k| k != d && !under(k, d));
    }

    // ---- indexing ----------------------------------------------------------------------------

    /// Indexes `changed`, records `deleted`, publishes, and compacts when needed.
    fn apply(&mut self, changed: Vec<Entry>, deleted: Vec<String>) {
        let deleted: Vec<String> = deleted.into_iter().filter(|d| self.meta_of(d).is_some_and(|m| m.flags & F_DELETED == 0)).collect();
        let added: Vec<String> = changed.iter().filter(|e| self.meta_of(&e.rel).is_none_or(|m| m.flags & F_DELETED != 0)).map(|e| e.rel.clone()).collect();
        if changed.is_empty() && deleted.is_empty() {
            if !self.content_ready && self.files_ready {
                self.content_ready = true;
                self.publish();
            }
            return;
        }
        self.patch_files(&added, &deleted);
        if self.lock.is_none() {
            // Read-only (another catlas owns the cache): changed files are read directly.
            self.pending.extend(changed.iter().map(|e| e.rel.clone()));
            self.pending.sort();
            self.pending.dedup();
            let gone: HashSet<&String> = deleted.iter().collect();
            self.pending.retain(|p| !gone.contains(p));
            self.content_ready = true;
            self.publish();
            return;
        }
        // Readers see the changes as pending while they're indexed.
        self.pending = changed.iter().map(|e| e.rel.clone()).collect();
        self.publish();
        self.set_phase(Phase::Indexing);
        self.root.done.store(0, Ordering::Relaxed);
        self.root.total.store(changed.len(), Ordering::Relaxed);
        let first = self.next_seq;
        let seq = AtomicU64::new(self.next_seq);
        let written = build(&self.root, &self.cfg, &changed, &seq);
        let mut seqs = match written {
            Ok(s) => s,
            Err(e) => {
                self.fail(format!("indexing: {e}"));
                self.content_ready = true;
                self.publish();
                return;
            }
        };
        if !deleted.is_empty() {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let mut b = Builder::default();
            for d in &deleted {
                b.add(d, FileMeta { mtime: 0, size: 0, flags: F_DELETED }, &[], &[]);
            }
            if let Err(e) = b.write(&self.root.dir.join(seg_name(s))) {
                self.fail(format!("indexing: {e}"));
            } else {
                seqs.push(s);
            }
        }
        self.next_seq = seq.load(Ordering::Relaxed).max(first);
        seqs.sort_unstable();
        for s in seqs {
            match Segment::open(&self.root.dir.join(seg_name(s)), s) {
                Ok(seg) => {
                    let si = self.segs.len() as u32;
                    for i in 0..seg.len() {
                        self.by_path.insert(fnv64(seg.path(i).as_bytes()), (si, i as u32));
                    }
                    self.segs.push(Arc::new(seg));
                }
                Err(e) => self.fail(format!("indexing: {e}")),
            }
        }
        self.pending.clear();
        self.content_ready = true;
        self.save_manifest();
        self.publish();
        self.compact();
    }

    /// Merges segments when there are too many: the small newer ones into one when the oldest
    /// holds most entries (cheap), else all of them (tombstones and dead entries dropped).
    fn compact(&mut self) {
        if self.segs.len() <= MAX_SEGMENTS || self.lock.is_none() {
            return;
        }
        self.set_phase(Phase::Merging);
        let total: usize = self.segs.iter().map(|s| s.len()).sum();
        let from = if self.segs[0].len() * 4 >= total * 3 { 1 } else { 0 };
        let mut keep: Vec<Vec<bool>> = self.segs[from..].iter().map(|s| vec![false; s.len()]).collect();
        for &(s, i) in self.by_path.values() {
            let s = s as usize;
            if s >= from {
                let deleted = self.segs[s].meta[i as usize].flags & F_DELETED != 0;
                // A full merge has nothing older to hide: tombstones can go.
                keep[s - from][i as usize] = !(deleted && from == 0);
            }
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let out = self.root.dir.join(seg_name(seq));
        let parts: Vec<&Segment> = self.segs[from..].iter().map(|s| s.as_ref()).collect();
        if let Err(e) = segment::merge(&parts, &keep, &out) {
            self.fail(format!("compacting: {e}"));
            let _ = fs::remove_file(&out);
            return;
        }
        let merged = match Segment::open(&out, seq) {
            Ok(s) => s,
            Err(e) => {
                self.fail(format!("compacting: {e}"));
                return;
            }
        };
        let old: Vec<u64> = self.segs[from..].iter().map(|s| s.seq).collect();
        self.segs.truncate(from);
        self.segs.push(Arc::new(merged));
        self.rebuild_by_path();
        self.save_manifest();
        self.publish();
        for s in old {
            let _ = fs::remove_file(self.root.dir.join(seg_name(s)));
        }
    }
}

/// Walks `start` (default: the whole root; with a depth) and returns files, folders (both
/// relative to `root`) and whether `limit` cut the listing short.
pub fn walk_all(root: &Path, cache: &Path, threads: usize, start: Option<(&Path, usize)>, limit: usize) -> (Vec<Entry>, Vec<String>, bool) {
    let (dir, depth) = match start {
        Some((d, n)) => (d.to_path_buf(), (n != usize::MAX).then_some(n)),
        None => (root.to_path_buf(), None),
    };
    let skip = cache.starts_with(root).then(|| cache.to_path_buf());
    let out: Mutex<(Vec<Entry>, Vec<String>)> = Mutex::new((Vec::new(), Vec::new()));
    let count = AtomicUsize::new(0);
    let capped = AtomicBool::new(false);
    walker(&dir, skip, depth).threads(threads.max(1)).build_parallel().run(|| {
        let mut batch = Batch { out: &out, files: Vec::new(), dirs: Vec::new() };
        let count = &count;
        let capped = &capped;
        Box::new(move |e| {
            let Ok(e) = e else { return WalkState::Continue };
            let Some(rel) = rel_of(root, e.path()) else { return WalkState::Continue };
            let ft = e.file_type();
            if ft.is_some_and(|t| t.is_dir()) {
                batch.dirs.push(rel);
            } else if ft.is_some_and(|t| t.is_file()) {
                if count.fetch_add(1, Ordering::Relaxed) >= limit {
                    capped.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
                let (mtime, size) = e.metadata().map_or((0, 0), |m| (mtime_ns(&m), m.len()));
                batch.files.push(Entry { rel, mtime, size });
            }
            if batch.files.len() + batch.dirs.len() >= 8192 {
                batch.flush();
            }
            WalkState::Continue
        })
    });
    let (f, d) = out.into_inner().unwrap_or_default();
    (f, d, capped.load(Ordering::Relaxed))
}

/// A walker thread's pending results; the rest is handed over when the walker drops it.
struct Batch<'a> {
    out: &'a Mutex<(Vec<Entry>, Vec<String>)>,
    files: Vec<Entry>,
    dirs: Vec<String>,
}

impl Batch<'_> {
    fn flush(&mut self) {
        if let Ok(mut o) = self.out.lock() {
            o.0.append(&mut self.files);
            o.1.append(&mut self.dirs);
        }
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Reads one file for the index: (contents, flags). Binary, unreadable and too big files give
/// no contents.
fn read_for_index(path: &Path, size: u64, max: u64) -> (Option<Vec<u8>>, u8) {
    if size > max {
        return (None, F_NO_CONTENT);
    }
    let mut data = Vec::with_capacity(size as usize + 1);
    match File::open(path).and_then(|f| f.take(max + 1).read_to_end(&mut data)) {
        Ok(_) if data.len() as u64 <= max && !data[..data.len().min(8192)].contains(&0) => (Some(data), 0),
        _ => (None, F_NO_CONTENT),
    }
}

/// Indexes `files` on worker threads into new segments (numbered from `seq`); each thread
/// writes a segment whenever its share of the memory budget fills up. Returns their numbers.
fn build(root: &Root, cfg: &Config, files: &[Entry], seq: &AtomicU64) -> std::io::Result<Vec<u64>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let threads = cfg.threads.clamp(1, 16).min(files.len().div_ceil(64).max(1));
    let per_thread = (cfg.budget / threads).max(4 << 20);
    let next = AtomicUsize::new(0);
    let written: Mutex<Vec<u64>> = Mutex::new(Vec::new());
    let error: Mutex<Option<std::io::Error>> = Mutex::new(None);
    const CHUNK: usize = 64;
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| {
                let mut x = Extractor::default();
                let mut b = Builder::default();
                let flush = |b: Builder| {
                    let s = seq.fetch_add(1, Ordering::Relaxed);
                    match b.write(&root.dir.join(seg_name(s))) {
                        Ok(()) => written.lock().unwrap().push(s),
                        Err(e) => *error.lock().unwrap() = Some(e),
                    }
                };
                loop {
                    let start = next.fetch_add(CHUNK, Ordering::Relaxed);
                    if start >= files.len() {
                        break;
                    }
                    for e in &files[start..(start + CHUNK).min(files.len())] {
                        let abs = root.path.join(&e.rel);
                        let (data, flags) = read_for_index(&abs, e.size, cfg.max_file);
                        let meta = FileMeta { mtime: e.mtime, size: e.size, flags };
                        match data {
                            Some(d) => {
                                let defs = if cfg.symbols && e.size <= MAX_SYMBOL_FILE {
                                    let lang = crate::syntax::Lang::detect(Some(Path::new(&e.rel)), "");
                                    if symbols::supported(lang) { symbols::extract(lang, &d) } else { Vec::new() }
                                } else {
                                    Vec::new()
                                };
                                let tris = x.extract(&d);
                                b.add(&e.rel, meta, tris, &defs);
                            }
                            None => {
                                b.add(&e.rel, meta, &[], &[]);
                            }
                        }
                        root.done.fetch_add(1, Ordering::Relaxed);
                    }
                    if b.mem >= per_thread {
                        flush(std::mem::take(&mut b));
                    }
                }
                if !b.is_empty() {
                    flush(b);
                }
            });
        }
    });
    if let Some(e) = error.into_inner().unwrap_or(None) {
        return Err(e);
    }
    Ok(written.into_inner().unwrap_or_default())
}
