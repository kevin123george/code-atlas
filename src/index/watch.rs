//! File-system events for an indexed root (`notify`: FSEvents on macOS, inotify on Linux).
//! Events are filtered cheaply here (`.git`, `target`, `node_modules`, the cache folder) and
//! handed to the root's worker, which re-lists the folders they're in.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::root::Root;
use crate::project::tree::skip_name;

/// Whether an event for `p` can matter to the index of `root`.
pub fn relevant(root: &Path, cache: &Path, p: &Path) -> bool {
    let Ok(rel) = p.strip_prefix(root) else { return false };
    if p.starts_with(cache) {
        return false;
    }
    let n = rel.components().count();
    rel.components().enumerate().all(|(i, c)| match c {
        // The last part may be a file or a folder; earlier ones are folders.
        Component::Normal(s) => !skip_name(&s.to_string_lossy(), i + 1 < n) && !(i + 1 == n && skip_name(&s.to_string_lossy(), true)),
        _ => true,
    })
}

/// Starts watching `root` recursively; `None` when the platform refuses (then only the scan at
/// start keeps the index fresh).
pub fn start(root: Arc<Root>, cache: PathBuf) -> Option<RecommendedWatcher> {
    let r = root.clone();
    let base = root.path.clone();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else {
            r.rescan();
            return;
        };
        if ev.need_rescan() {
            r.rescan();
            return;
        }
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        let paths: Vec<PathBuf> = ev.paths.into_iter().filter(|p| relevant(&base, &cache, p)).collect();
        r.changed(paths);
    })
    .ok()?;
    w.watch(&root.path, RecursiveMode::Recursive).ok()?;
    Some(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_noise() {
        let root = Path::new("/p");
        let cache = Path::new("/p/bin/.cache");
        assert!(relevant(root, cache, Path::new("/p/src/a.rs")));
        assert!(relevant(root, cache, Path::new("/p/src")));
        assert!(!relevant(root, cache, Path::new("/p/.git/index")));
        assert!(!relevant(root, cache, Path::new("/p/.git")));
        assert!(!relevant(root, cache, Path::new("/p/target/debug/x")));
        assert!(!relevant(root, cache, Path::new("/p/web/node_modules/a/b.js")));
        assert!(!relevant(root, cache, Path::new("/p/bin/.cache/index/x/seg-1.cidx")));
        assert!(!relevant(root, cache, Path::new("/elsewhere/a.rs")));
        assert!(!relevant(root, cache, Path::new("/p/.DS_Store")));
    }
}
