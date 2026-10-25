//! Background work over the whole project: the file list for the finder, and find in files.
//! Both run on their own threads and stream results through channels; the UI drains them on
//! every tick, so the editor stays responsive on big trees. Dropping a [`Job`] cancels it.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use ignore::WalkState;

use super::tree::walker;

/// Files bigger than this aren't searched.
const MAX_FILE: u64 = 4 << 20;
/// Stop after this many hits (like IDEA's "too many usages").
pub const MAX_HITS: usize = 2000;
/// The finder keeps at most this many paths.
pub const MAX_FILES: usize = 300_000;

pub struct Job<T> {
    pub rx: Receiver<T>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl<T> Job<T> {
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn spawn<T: Send + 'static>(work: impl FnOnce(Sender<T>, &AtomicBool) + Send + 'static) -> Job<T> {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let (c, d) = (cancel.clone(), done.clone());
    thread::spawn(move || {
        work(tx, &c);
        d.store(true, Ordering::Relaxed);
    });
    Job { rx, cancel, done }
}

/// Path of `p` relative to `root` with `/` separators (absolute when outside the root).
pub fn rel(root: &Path, p: &Path) -> String {
    let r = p.strip_prefix(root).unwrap_or(p);
    r.to_string_lossy().replace('\\', "/")
}

/// Streams every (not ignored) file under `root` as a relative path, in batches.
pub fn index_files(root: PathBuf) -> Job<Vec<String>> {
    spawn(move |tx, cancel| {
        let count = AtomicUsize::new(0);
        walker(&root).threads(num_threads()).build_parallel().run(|| {
            let mut out = Batch { tx: tx.clone(), items: Vec::new() };
            let root = root.clone();
            let count = &count;
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) || count.load(Ordering::Relaxed) >= MAX_FILES {
                    return WalkState::Quit;
                }
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|t| t.is_file()) {
                        out.items.push(rel(&root, e.path()));
                        count.fetch_add(1, Ordering::Relaxed);
                        if out.items.len() >= 512 && !out.flush() {
                            return WalkState::Quit;
                        }
                    }
                }
                WalkState::Continue
            })
        });
    })
}

/// A worker's pending paths; whatever is left is sent when the worker finishes.
struct Batch {
    tx: Sender<Vec<String>>,
    items: Vec<String>,
}

impl Batch {
    fn flush(&mut self) -> bool {
        self.items.is_empty() || self.tx.send(std::mem::take(&mut self.items)).is_ok()
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.flush();
    }
}

fn num_threads() -> usize {
    thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Path relative to the project root.
    pub path: String,
    /// 0-based line.
    pub line: usize,
    /// Char columns of the match in the line, `[start, end)`.
    pub start: usize,
    pub end: usize,
    /// The line, tabs as spaces, cut to a sensible length.
    pub text: String,
    /// Char offset in `text` where the match starts (it may be cut at the front).
    pub text_start: usize,
}

/// Finds `query` in every text file under `root`. Smart case: lowercase queries ignore case.
/// Hits of one file arrive together, in line order.
pub fn find_in_files(root: PathBuf, query: String) -> Job<Vec<Hit>> {
    spawn(move |tx, cancel| {
        let hits = AtomicUsize::new(0);
        let ignore_case = !query.chars().any(char::is_uppercase);
        let needle: Vec<u8> = if ignore_case { query.to_lowercase().into_bytes() } else { query.clone().into_bytes() };
        if needle.is_empty() {
            return;
        }
        walker(&root).threads(num_threads()).build_parallel().run(|| {
            let tx = tx.clone();
            let root = root.clone();
            let needle = needle.clone();
            let hits = &hits;
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) || hits.load(Ordering::Relaxed) >= MAX_HITS {
                    return WalkState::Quit;
                }
                let Ok(e) = entry else { return WalkState::Continue };
                if !e.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                let found = search_file(e.path(), &needle, ignore_case);
                if !found.is_empty() {
                    let path = rel(&root, e.path());
                    let n = found.len();
                    let batch: Vec<Hit> = found.into_iter().map(|mut h| {
                        h.path = path.clone();
                        h
                    }).collect();
                    hits.fetch_add(n, Ordering::Relaxed);
                    if tx.send(batch).is_err() {
                        return WalkState::Quit;
                    }
                }
                WalkState::Continue
            })
        });
    })
}

/// All matches in one file (empty for binaries, unreadable or too big files).
pub fn search_file(path: &Path, needle: &[u8], ignore_case: bool) -> Vec<Hit> {
    let Ok(f) = fs::File::open(path) else { return Vec::new() };
    if f.metadata().map_or(true, |m| m.len() > MAX_FILE) {
        return Vec::new();
    }
    let mut data = Vec::new();
    if f.take(MAX_FILE).read_to_end(&mut data).is_err() || data[..data.len().min(8192)].contains(&0) {
        return Vec::new();
    }
    search_bytes(&data, needle, ignore_case)
}

pub fn search_bytes(data: &[u8], needle: &[u8], ignore_case: bool) -> Vec<Hit> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let hay_low;
    let hay: &[u8] = if ignore_case {
        hay_low = data.to_ascii_lowercase();
        &hay_low
    } else {
        data
    };
    let mut line = 0;
    let mut line_start = 0;
    let mut scanned = 0; // newlines counted up to here
    let mut at = 0;
    while let Some(off) = find(&hay[at..], needle) {
        let pos = at + off;
        for (i, &b) in data[scanned..pos].iter().enumerate() {
            if b == b'\n' {
                line += 1;
                line_start = scanned + i + 1;
            }
        }
        scanned = pos;
        let line_end = data[pos..].iter().position(|&b| b == b'\n').map_or(data.len(), |e| pos + e);
        let text = String::from_utf8_lossy(&data[line_start..line_end]);
        let before = String::from_utf8_lossy(&data[line_start..pos]).chars().count();
        let len = String::from_utf8_lossy(&data[pos..pos + needle.len()]).chars().count();
        let (text, text_start) = snippet(&text, before);
        out.push(Hit { path: String::new(), line, start: before, end: before + len, text, text_start });
        if out.len() >= MAX_HITS {
            break;
        }
        // One hit per occurrence; continue after this one.
        at = pos + needle.len();
    }
    out
}

/// The line with tabs as spaces and the trailing `\r` dropped, trimmed at the front so the
/// match at char `col` stays in view; returns the text and the match's new start.
fn snippet(line: &str, col: usize) -> (String, usize) {
    let chars: Vec<char> = line.trim_end_matches('\r').chars().map(|c| if c == '\t' { ' ' } else if c.is_control() { '?' } else { c }).collect();
    let lead = chars.iter().take_while(|c| **c == ' ').count().min(col);
    if col - lead <= 40 {
        let s: String = chars[lead.min(chars.len())..].iter().take(300).collect();
        return (s, col - lead);
    }
    // Start at a word break shortly before the match, marked with an ellipsis.
    let mut cut = col - 30;
    if let Some(k) = chars[cut..col].iter().position(|c| *c == ' ').filter(|k| *k < 15) {
        cut += k + 1;
    }
    let s: String = std::iter::once('…').chain(chars[cut..].iter().copied().take(300)).collect();
    (s, col - cut + 1)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        match hay[i..hay.len() - needle.len() + 1].iter().position(|&b| b == first) {
            Some(p) => {
                i += p;
                if &hay[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
            None => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::tree::tests::sample;
    use std::time::{Duration, Instant};

    fn collect<T>(job: Job<Vec<T>>) -> Vec<T> {
        let mut out = Vec::new();
        let t = Instant::now();
        while !job.done() || job.rx.try_recv().map(|b| out.extend(b)).is_ok() {
            if let Ok(b) = job.rx.recv_timeout(Duration::from_millis(20)) {
                out.extend(b);
            }
            assert!(t.elapsed() < Duration::from_secs(10));
        }
        while let Ok(b) = job.rx.try_recv() {
            out.extend(b);
        }
        out
    }

    #[test]
    fn hits_have_lines_and_columns() {
        let h = search_bytes(b"one\n\ttwo two\nthree", b"two", false);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].line, h[0].start, h[0].end), (1, 1, 4));
        assert_eq!((h[1].line, h[1].start), (1, 5));
        assert_eq!(h[0].text, "two two");
        assert_eq!(h[0].text_start, 0);
        let h = search_bytes(b"x\r\nHello", b"hello", true);
        assert_eq!((h[0].line, h[0].start), (1, 0));
        assert!(search_bytes(b"Hello", b"hello", false).is_empty());
    }

    #[test]
    fn long_lines_are_cut_around_the_match() {
        let line = format!("{}needle", "a".repeat(100));
        let h = search_bytes(line.as_bytes(), b"needle", false);
        assert_eq!(h[0].start, 100);
        let shown: String = h[0].text.chars().skip(h[0].text_start).take(6).collect();
        assert_eq!(shown, "needle");
        assert!(h[0].text.starts_with('…'));
    }

    #[test]
    fn finds_in_project_but_not_in_ignored_places() {
        let root = sample();
        let mut hits = collect(find_in_files(root.clone(), "hello".into()));
        hits.sort_by(|a, b| a.path.cmp(&b.path));
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["README.md", "src/lib.rs", "src/main.rs", "src/nested/deep.rs"]);
        // Smart case: an uppercase letter makes it case-sensitive.
        let hits = collect(find_in_files(root.clone(), "Hello".into()));
        assert_eq!(hits.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn index_lists_project_files() {
        let root = sample();
        let mut files = collect(index_files(root.clone()));
        files.sort();
        assert_eq!(files, vec![".gitignore", "README.md", "docs/a.md", "src/lib.rs", "src/main.rs", "src/nested/deep.rs"]);
        fs::remove_dir_all(root).unwrap();
    }
}
