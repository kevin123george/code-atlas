//! The persistent code index (decision 0014): file list, trigram content index and tree-sitter
//! symbols for one or more root folders, kept in the cache folder and updated in the
//! background, so Go to File, Search Everywhere, Find in Files and Alt+N stay instant on huge
//! repositories.
//!
//! - [`Index`] is a cheap handle (clone it freely) over a set of roots: `add_root` /
//!   `remove_root` at any time (multi-repo workspaces add one per repository).
//! - Each root has a worker thread (`root.rs`): load the cache → publish → scan the disk (mtime
//!   + size) → index what changed → watch for changes (`watch.rs`) → compact (`segment.rs`).
//! - Queries (`query.rs`) never block on the worker: they read the last published [`View`] and
//!   verify every candidate against the file's real contents. While a root has no ready view,
//!   the callers fall back to walking the disk (`project::search`).
//! - `catlas --index-status` and `catlas --bench-index` live in `cli.rs`.

pub mod cli;
pub mod glue;
pub mod query;
pub mod root;
pub mod segment;
pub mod symbols;
pub mod trigram;
pub mod ui;
pub mod watch;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use query::{FindOpts, SymbolHit};
pub use root::{Phase, Root, View};

use crate::project::search::{Hit, Job};

/// Where index caches live: `$CATLAS_CACHE_DIR`, else `~/Library/Caches/code-atlas` on macOS,
/// `$XDG_CACHE_HOME/code-atlas` or `~/.cache/code-atlas` elsewhere.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CATLAS_CACHE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Caches/code-atlas"));
    }
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("code-atlas"));
    }
    home.map(|h| h.join(".cache/code-atlas"))
}

/// The worker settings from `[index]` in `settings.toml`.
pub fn config_from(s: &crate::config::IndexSettings) -> root::Config {
    root::Config {
        max_file: s.max_file_kb.saturating_mul(1024),
        max_files: s.max_files,
        symbols: s.symbols,
        watch: s.watch,
        budget: s.memory_mb << 20,
        ..root::Config::default()
    }
}

#[derive(Clone)]
pub struct Index {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: root::Config,
    cache: PathBuf,
    roots: Mutex<Vec<Arc<Root>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(r) = self.roots.lock() {
            for x in r.iter() {
                x.stop();
            }
        }
    }
}

impl Index {
    pub fn new(cfg: root::Config, cache: PathBuf) -> Index {
        Index { inner: Arc::new(Inner { cfg, cache, roots: Mutex::new(Vec::new()) }) }
    }

    /// The index as `settings.toml` configures it (`None` when it's off, or in unit tests,
    /// which make their own with a scratch cache folder).
    pub fn from_settings() -> Option<Index> {
        if cfg!(test) {
            return None;
        }
        let s = &crate::config::Settings::get().index;
        if !s.enabled {
            return None;
        }
        Some(Index::new(config_from(s), cache_dir()?))
    }

    fn roots_lock(&self) -> std::sync::MutexGuard<'_, Vec<Arc<Root>>> {
        self.inner.roots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts indexing `path` (made canonical); a root that is already there is kept.
    pub fn add_root(&self, path: &Path) -> Arc<Root> {
        let path = crate::project::canonical(path);
        let mut roots = self.roots_lock();
        if let Some(r) = roots.iter().find(|r| r.path == path) {
            return r.clone();
        }
        let r = Root::start(path, &self.inner.cache, self.inner.cfg.clone());
        roots.push(r.clone());
        r
    }

    /// Stops indexing `path` (its cache stays for next time). Returns whether it was a root.
    #[allow(dead_code)] // for multi-root workspaces (decision 0013)
    pub fn remove_root(&self, path: &Path) -> bool {
        let path = crate::project::canonical(path);
        let mut roots = self.roots_lock();
        let before = roots.len();
        roots.retain(|r| {
            let keep = r.path != path;
            if !keep {
                r.stop();
            }
            keep
        });
        roots.len() != before
    }

    #[allow(dead_code)] // for multi-root workspaces (decision 0013)
    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots_lock().iter().map(|r| r.path.clone()).collect()
    }

    pub fn root(&self, path: &Path) -> Option<Arc<Root>> {
        self.roots_lock().iter().find(|r| r.path == path).cloned()
    }

    pub fn view(&self, root: &Path) -> Option<Arc<View>> {
        self.root(root).map(|r| r.view())
    }

    /// Find in files over the index of `root` (`None` while its content index isn't ready:
    /// walk the disk instead). Paths in the hits are relative to `root`, `scope` is a folder
    /// relative to it.
    pub fn find(&self, root: &Path, scope: Option<String>, query: String, opts: FindOpts) -> Option<Job<Vec<Hit>>> {
        let r = self.root(root)?;
        let v = r.view();
        v.content_ready.then(|| query::find(r.path.clone(), v, scope, query, opts))
    }

    /// Whether symbol search has something to search (some root's content index is ready).
    pub fn symbols_ready(&self) -> bool {
        self.inner.cfg.symbols && self.roots_lock().iter().any(|r| r.view().content_ready)
    }

    /// The best `limit` symbols matching `query` in `root` (or all roots), in the background.
    pub fn find_symbols(&self, root: Option<&Path>, scope: Option<String>, query: String, limit: usize) -> Option<Job<Vec<SymbolHit>>> {
        let views = self.ready_views(root);
        if views.is_empty() || !self.inner.cfg.symbols {
            return None;
        }
        Some(crate::project::search::spawn(move |tx, cancel| {
            let found = query::symbols(&views, scope.as_deref(), &query, limit, cancel);
            if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(found);
            }
        }))
    }

    /// The same, on this thread (tests, the bench).
    pub fn symbols_now(&self, root: Option<&Path>, scope: Option<&str>, query: &str, limit: usize) -> Vec<SymbolHit> {
        query::symbols(&self.ready_views(root), scope, query, limit, &AtomicBool::new(false))
    }

    fn ready_views(&self, root: Option<&Path>) -> Vec<(PathBuf, Arc<View>)> {
        self.roots_lock()
            .iter()
            .filter(|r| root.is_none_or(|p| r.path == p))
            .map(|r| (r.path.clone(), r.view()))
            .filter(|(_, v)| v.content_ready)
            .collect()
    }

    /// A short status for the status bar while a root is busy ("Indexing 45%"), else `None`.
    pub fn busy(&self) -> Option<String> {
        let roots = self.roots_lock();
        let (mut done, mut total, mut phase) = (0usize, 0usize, None);
        for r in roots.iter() {
            let v = r.view();
            let p = r.phase();
            let show = match p {
                Phase::Loading | Phase::Merging => true,
                Phase::Scanning => !v.content_ready,
                Phase::Indexing => r.total.load(std::sync::atomic::Ordering::Relaxed) >= 200 || !v.content_ready,
                Phase::Idle | Phase::Stopped => false,
            };
            if show {
                done += r.done.load(std::sync::atomic::Ordering::Relaxed);
                total += r.total.load(std::sync::atomic::Ordering::Relaxed);
                phase = Some(phase.map_or(p, |q: Phase| if q == Phase::Indexing { q } else { p }));
            }
        }
        Some(match phase? {
            Phase::Indexing if total > 0 => format!("Indexing {}%", (done * 100 / total).min(99)),
            Phase::Indexing => "Indexing".into(),
            Phase::Loading => "Loading index".into(),
            Phase::Scanning => "Scanning files".into(),
            Phase::Merging => "Compacting index".into(),
            Phase::Idle | Phase::Stopped => return None,
        })
    }

    /// Waits until every root is idle (tests, the bench). Returns false on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let t = Instant::now();
        loop {
            if self.roots_lock().iter().all(|r| r.idle()) {
                return true;
            }
            if t.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
