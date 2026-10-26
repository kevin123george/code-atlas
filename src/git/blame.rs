//! Annotate (blame) in the editor gutter: who last changed each line and when, from
//! `git blame --porcelain --contents -` with the buffer on stdin, so the lines match what is on
//! screen even with unsaved edits. Commits Claude co-authored get a `✳`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};

use super::{job, run, run_with};
use crate::ui::theme::*;

/// Gutter columns blame takes: `✳ 3d kevin   ` (age 3, author 8, spaces).
pub const WIDTH: u16 = 15;
/// A changed buffer is blamed again after this pause.
const AGAIN_AFTER: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub sha: String,
    pub author: String,
    pub time: i64,
    pub summary: String,
    pub claude: bool,
}

impl Info {
    pub fn uncommitted(&self) -> bool {
        self.sha.bytes().all(|b| b == b'0')
    }
}

/// Parses `git blame --porcelain`: one info index per final line, and the infos.
pub fn parse(out: &str) -> (Vec<usize>, Vec<Info>) {
    let mut infos: Vec<Info> = Vec::new();
    let mut by_sha: HashMap<String, usize> = HashMap::new();
    let mut lines: Vec<usize> = Vec::new();
    let mut cur: Option<usize> = None;
    for l in out.split('\n') {
        if let Some(_text) = l.strip_prefix('\t') {
            if let Some(c) = cur {
                lines.push(c);
            }
            continue;
        }
        let mut parts = l.split(' ');
        let first = parts.next().unwrap_or("");
        if first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit()) && parts.next().is_some() {
            let i = *by_sha.entry(first.to_string()).or_insert_with(|| {
                infos.push(Info { sha: first.to_string(), author: String::new(), time: 0, summary: String::new(), claude: false });
                infos.len() - 1
            });
            cur = Some(i);
            continue;
        }
        let Some(c) = cur else { continue };
        if let Some(a) = l.strip_prefix("author ") {
            infos[c].author = a.to_string();
        } else if let Some(t) = l.strip_prefix("author-time ") {
            infos[c].time = t.trim().parse().unwrap_or(0);
        } else if let Some(s) = l.strip_prefix("summary ") {
            infos[c].summary = s.to_string();
        }
    }
    (lines, infos)
}

pub struct Blame {
    /// Info index per buffer line.
    pub lines: Vec<usize>,
    pub infos: Vec<Info>,
    /// The doc revision the lines are for; the error of the last try.
    pub rev: Option<u64>,
    pub error: Option<String>,
    job: Option<(u64, Receiver<Result<(Vec<usize>, Vec<Info>), String>>)>,
    started: Option<Instant>,
}

impl Blame {
    fn new() -> Blame {
        Blame { lines: Vec::new(), infos: Vec::new(), rev: None, error: None, job: None, started: None }
    }

    pub fn at(&self, line: usize) -> Option<&Info> {
        self.lines.get(line).and_then(|&i| self.infos.get(i))
    }
}

fn blame(root: &Path, rel: &str, text: String) -> Result<(Vec<usize>, Vec<Info>), String> {
    let out = run_with(root, &["blame", "--porcelain", "--contents", "-", "--", rel], &[], Some(text.as_bytes()))?;
    let (lines, mut infos) = parse(&out);
    // Which of these commits Claude co-authored.
    let shas: Vec<&str> = infos.iter().filter(|i| !i.uncommitted()).map(|i| i.sha.as_str()).take(500).collect();
    if !shas.is_empty() {
        let mut a = vec!["show", "-s", "--format=%H%x1f%ae%x1f%(trailers:key=Co-authored-by,valueonly,separator=%x1d)%x1e"];
        a.extend(shas);
        if let Ok(out) = run(root, &a) {
            let claude: std::collections::HashSet<String> = out
                .split('\x1e')
                .filter_map(|r| {
                    let f: Vec<&str> = r.trim_start_matches('\n').split('\x1f').collect();
                    (f.len() >= 3 && super::log::is_claude(f[2], "", f[1])).then(|| f[0].to_string())
                })
                .collect();
            for i in &mut infos {
                i.claude = claude.contains(&i.sha);
            }
        }
    }
    Ok((lines, infos))
}

/// Blame for the open files that have it switched on, keyed by the doc's path.
#[derive(Default)]
pub struct Blames {
    pub on: HashMap<PathBuf, Blame>,
}

impl Blames {
    /// Switches blame for `path`; returns whether it is on now.
    pub fn toggle(&mut self, path: &Path) -> bool {
        if self.on.remove(path).is_some() {
            return false;
        }
        self.on.insert(path.to_path_buf(), Blame::new());
        true
    }

    pub fn get(&self, path: Option<&Path>) -> Option<&Blame> {
        path.and_then(|p| self.on.get(p))
    }

    /// Takes finished jobs and blames `doc` again when its text changed. True = redraw.
    pub fn tick(&mut self, root: &Path, doc: &crate::doc::Doc) -> bool {
        let mut changed = false;
        for b in self.on.values_mut() {
            if let Some((rev, rx)) = &b.job {
                if let Ok(r) = rx.try_recv() {
                    let rev = *rev;
                    b.job = None;
                    changed = true;
                    match r {
                        Ok((lines, infos)) => {
                            b.lines = lines;
                            b.infos = infos;
                            b.rev = Some(rev);
                            b.error = None;
                        }
                        Err(e) => {
                            b.rev = Some(rev);
                            b.error = Some(e);
                        }
                    }
                }
            }
        }
        let Some(path) = &doc.path else { return changed };
        let Some(b) = self.on.get_mut(path) else { return changed };
        let rev = doc.rev();
        if b.job.is_some() || b.rev == Some(rev) || b.started.is_some_and(|t| t.elapsed() < AGAIN_AFTER) {
            return changed;
        }
        let abs = crate::project::canonical(path);
        let Ok(rel) = abs.strip_prefix(root) else {
            b.error = Some("not in the repository".into());
            b.rev = Some(rev);
            return true;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let (root, text) = (root.to_path_buf(), doc.rope.to_string());
        b.started = Some(Instant::now());
        b.job = Some((rev, job(move || blame(&root, &rel, text))));
        changed
    }
}

/// Draws one line's blame cell at (x, y), `WIDTH` wide: age, author (✳ for Claude).
pub fn draw_cell(buf: &mut Buffer, x: u16, y: u16, b: &Blame, line: usize, cur: bool, now: i64) {
    let bg = if cur { SURFACE_2 } else { SURFACE_1 };
    buf.set_stringn(x, y, " ".repeat(WIDTH as usize), WIDTH as usize, Style::new().bg(bg));
    let Some(info) = b.at(line) else {
        if line == 0 {
            if let Some(e) = &b.error {
                let short = if e.contains("no such path") { "not in HEAD" } else { "no blame" };
                buf.set_stringn(x + 1, y, short, WIDTH as usize - 2, Style::new().fg(MUTED).bg(bg));
            } else if b.rev.is_none() {
                buf.set_stringn(x + 1, y, "blaming…", WIDTH as usize - 2, Style::new().fg(MUTED).bg(bg));
            }
        }
        return;
    };
    if info.uncommitted() {
        buf.set_stringn(x + 1, y, "  · changed", WIDTH as usize - 2, Style::new().fg(MUTED).bg(bg));
        return;
    }
    // Same commit as the line above: only a thin mark, so blocks read as blocks.
    let same_above = line > 0 && b.lines.get(line - 1) == b.lines.get(line) && !cur;
    let age_s = (now - info.time).max(0);
    let fg = if age_s < 86_400 {
        ACCENT
    } else if age_s < 7 * 86_400 {
        TEXT
    } else if age_s < 90 * 86_400 {
        TEXT_2
    } else {
        MUTED
    };
    if same_above {
        buf.set_stringn(x + 1, y, "│", 1, Style::new().fg(GRID).bg(bg));
        return;
    }
    let mark = if info.claude { "✳" } else { " " };
    buf.set_stringn(x, y, mark, 1, Style::new().fg(ACCENT).bg(bg).add_modifier(Modifier::BOLD));
    let age = super::log::age(now, info.time);
    buf.set_stringn(x + 1, y, format!("{age:>3}"), 3, Style::new().fg(fg).bg(bg));
    let author: String = info.author.split_whitespace().next().unwrap_or("").chars().take(9).collect();
    buf.set_stringn(x + 5, y, author, WIDTH as usize - 6, Style::new().fg(fg).bg(bg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parsing_shares_infos_between_lines() {
        let a = "a".repeat(40);
        let z = "0".repeat(40);
        let out = format!("{a} 1 1 2\nauthor Kevin Example\nauthor-mail <k@x>\nauthor-time 1700000000\nsummary First\nfilename x\n\tone\n{a} 2 2\n\ttwo\n{z} 3 3 1\nauthor External file (--contents)\nauthor-time 1800000000\nsummary Version of x from -\nfilename x\n\tTHREE\n");
        let (lines, infos) = parse(&out);
        assert_eq!(lines, vec![0, 0, 1]);
        assert_eq!(infos[0].author, "Kevin Example");
        assert_eq!(infos[0].time, 1_700_000_000);
        assert_eq!(infos[0].summary, "First");
        assert!(infos[1].uncommitted() && !infos[0].uncommitted());
    }

    #[test]
    fn blames_the_buffer_text_and_marks_claude_commits() {
        let dir = crate::git::tests::scratch_repo("blame");
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        run(&dir, &["commit", "-qam", "second\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"]).unwrap();
        let mut doc = crate::doc::Doc::open(&dir.join("a.txt")).unwrap();
        doc.goto_line(2, 0);
        doc.insert_str("unsaved\n");
        let mut b = Blames::default();
        assert!(b.toggle(doc.path.as_ref().unwrap()));
        let t = Instant::now();
        while b.get(doc.path.as_deref()).unwrap().rev != Some(doc.rev()) {
            assert!(t.elapsed() < Duration::from_secs(10));
            b.tick(&dir, &doc);
            std::thread::sleep(Duration::from_millis(5));
        }
        let bl = b.get(doc.path.as_deref()).unwrap();
        assert!(bl.error.is_none(), "{:?}", bl.error);
        assert_eq!(bl.lines.len(), 4);
        assert!(!bl.at(0).unwrap().claude, "line 1 is from the first commit");
        assert!(bl.at(1).unwrap().claude, "TWO came with Claude's commit");
        assert!(bl.at(2).unwrap().uncommitted(), "the unsaved line");
        assert!(!b.toggle(doc.path.as_ref().unwrap()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
