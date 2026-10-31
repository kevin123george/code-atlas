//! Agents in the editor (decision 0015): which files running agents are writing right now (a
//! coloured `✳` in the tree and on tabs), which lines of the open file an agent changed in this
//! session (line numbers in the agent's colour), and the status-bar ticker.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ratatui::style::Color;

use crate::agents::run::Run;
use crate::agents::Agents;
use crate::doc::Doc;
use crate::project::canonical;
use crate::ui::theme::*;

/// One colour per agent (by id), from the Ledger Terminal series palette.
pub const COLORS: [Color; 6] = [S_BLUE, S_ORANGE, S_MAGENTA, S_VIOLET, S_YELLOW, S_AQUA];

pub fn color(id: u64) -> Color {
    COLORS[(id as usize) % COLORS.len()]
}

/// The agent glyph next to files and on cards.
pub const GLYPH: &str = "✳";

/// `p` as the editor's checkout sees it: a file in an agent's worktree maps to the same path in
/// the repository the worktree was made from.
pub fn in_checkout(r: &Run, p: &Path) -> PathBuf {
    let p = canonical(p);
    if let Some(w) = &r.worktree {
        if let Ok(rel) = p.strip_prefix(&w.path) {
            return w.repo.join(rel);
        }
    }
    p
}

/// Files running agents are writing now → the newest such agent's id.
pub fn editing(agents: &Agents) -> HashMap<PathBuf, u64> {
    let mut m = HashMap::new();
    for r in agents.runs.iter().filter(|r| r.running()) {
        for f in r.editing_now() {
            let p = in_checkout(r, &f);
            let id = m.entry(p).or_insert(r.id);
            *id = (*id).max(r.id);
        }
    }
    m
}

/// The status-bar ticker: "✳ 2 agents · #3 editing src/x.rs", or who needs you. `None` when no
/// agent is working, starting or waiting for a worktree.
pub fn ticker(agents: &Agents, pending: usize) -> Option<(String, Color)> {
    let working: Vec<&Run> = agents.runs.iter().filter(|r| r.running()).collect();
    let n = working.len() + pending;
    if n == 0 {
        return None;
    }
    let mut s = format!("{GLYPH} {n} agent{}", if n == 1 { "" } else { "s" });
    let editing = working.iter().filter(|r| matches!(r.activity(), crate::agents::run::Activity::Editing(_))).count();
    if editing > 0 && n > 1 {
        s.push_str(&format!(" · {editing} editing"));
    }
    // The newest working agent, what it does now.
    if let Some(r) = working.iter().max_by_key(|r| r.id) {
        s.push_str(&format!(" · #{} {}", r.id, crate::agents::transcript::ellipsize(&r.activity().label(), 32)));
    } else {
        s.push_str(" · creating a worktree");
    }
    Some((s, ACCENT))
}

struct Cur {
    /// The document's path as given (compared on every draw) and canonical.
    raw: PathBuf,
    path: PathBuf,
    run: u64,
    color: Color,
    /// The file when the run started (`None` = still loading or unreadable).
    base: Option<String>,
    rev: Option<u64>,
    lines: Vec<bool>,
    computed: Option<Instant>,
}

/// Lines of the current file an agent changed this session: the file now vs. the snapshot its
/// newest run (in this session) took before its first turn. It is "since", like the run review
/// (decision 0012): your own edits after that count too.
#[derive(Default)]
pub struct AgentLines {
    cur: Option<Cur>,
    job: Option<Receiver<Option<String>>>,
    checked: Option<(PathBuf, Instant)>,
}

/// How often the run that changed a file is looked up again, and how often lines are recounted
/// while typing.
const LOOKUP_EVERY: Duration = Duration::from_millis(500);
const RECOUNT_EVERY: Duration = Duration::from_millis(250);

impl AgentLines {
    /// The changed lines of `path` (as the editor names it) and the agent's colour.
    pub fn get(&self, path: Option<&Path>) -> Option<(Color, &[bool])> {
        let c = self.cur.as_ref()?;
        (path == Some(c.raw.as_path()) && c.lines.iter().any(|x| *x)).then_some((c.color, c.lines.as_slice()))
    }

    /// Follows the editor's current document. True = the marks changed (redraw).
    pub fn tick(&mut self, agents: &Agents, doc: &Doc, since: u64) -> bool {
        let Some(raw) = doc.path.clone() else {
            return self.clear();
        };
        let now = Instant::now();
        let same_doc = self.cur.as_ref().is_some_and(|c| c.raw == raw);
        let due = self.checked.as_ref().is_none_or(|(p, t)| *p != raw || now.duration_since(*t) >= LOOKUP_EVERY);
        if due {
            self.checked = Some((raw.clone(), now));
            let path = canonical(&raw);
            let run = agents
                .runs
                .iter()
                .filter(|r| r.updated >= since && r.base.is_some() && r.touched.iter().any(|t| canonical(t) == path))
                .max_by_key(|r| r.id);
            match run {
                None => return self.clear(),
                Some(r) if !(same_doc && self.cur.as_ref().is_some_and(|c| c.run == r.id)) => {
                    let base = r.base.as_ref().map(|b| b.tree.clone()).unwrap_or_default();
                    let cwd = r.cwd.clone();
                    let p = path.clone();
                    self.job = Some(crate::git::job(move || {
                        let root = crate::git::toplevel(&cwd)?;
                        let rel = p.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
                        // Not in the snapshot = a file the agent created: every line is its.
                        Some(crate::git::run(&root, &["show", &format!("{base}:{rel}")]).unwrap_or_default())
                    }));
                    self.cur = Some(Cur { raw: raw.clone(), path, run: r.id, color: color(r.id), base: None, rev: None, lines: Vec::new(), computed: None });
                }
                Some(_) => {}
            }
        }
        let mut changed = false;
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(base) => {
                    self.job = None;
                    if let Some(c) = &mut self.cur {
                        c.base = base;
                        c.rev = None;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => self.job = None,
            }
        }
        if let Some(c) = &mut self.cur {
            let stale = c.rev != Some(doc.rev()) && c.computed.is_none_or(|t| now.duration_since(t) >= RECOUNT_EVERY);
            if let (Some(base), true) = (&c.base, stale && c.path == canonical(&raw)) {
                let marks = crate::git::diff::gutter_marks(base, &doc.rope.to_string());
                c.lines = marks.iter().map(|m| matches!(m, Some(crate::git::diff::Mark::Added | crate::git::diff::Mark::Modified))).collect();
                c.rev = Some(doc.rev());
                c.computed = Some(now);
                changed = true;
            }
        }
        changed
    }

    fn clear(&mut self) -> bool {
        self.job = None;
        self.cur.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::run::{Baseline, Turn};

    #[test]
    fn worktree_files_map_to_the_checkout_and_the_ticker_names_the_newest() {
        let d = crate::git::tests::scratch_repo("presence");
        let mut agents = Agents::with_store(d.clone(), None);
        let mut r = Run::new(3, d.join("../x-wt/fix"), "fix it");
        r.worktree = Some(crate::mission::worktree::Worktree {
            path: canonical(&d.join("../x-wt/fix")),
            branch: "fix".into(),
            repo: d.clone(),
            base_branch: Some("main".into()),
            base_head: "abc".into(),
        });
        r.begin_turn(&Turn { shown: "fix it".into(), ..Turn::default() });
        let wt = canonical(&d.join("../x-wt/fix"));
        r.feed_line(&format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{"file_path":"{}/src/a.rs"}}}}]}}}}"#, wt.display()));
        agents.runs.push(r);
        let m = editing(&agents);
        assert_eq!(m.get(&d.join("src/a.rs")), Some(&3), "{m:?}");
        let (t, _) = ticker(&agents, 0).unwrap();
        assert_eq!(t, "✳ 1 agent · #3 editing src/a.rs");
        assert!(ticker(&Agents::with_store(d.clone(), None), 0).is_none());
        assert_eq!(ticker(&Agents::with_store(d.clone(), None), 1).unwrap().0, "✳ 1 agent · creating a worktree");
        agents.runs[0].proc = None;
        agents.runs[0].state = crate::agents::run::State::Done;
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn lines_an_agent_changed_this_session() {
        let d = crate::git::tests::scratch_repo("agent-lines");
        let f = d.join("a.txt");
        let mut agents = Agents::with_store(d.clone(), None);
        let mut r = Run::new(2, d.clone(), "louder");
        r.base = Baseline::take(&d);
        r.touched.push(f.clone());
        agents.runs.push(r);
        std::fs::write(&f, "one\nTWO\nthree\nfour\n").unwrap();
        let doc = Doc::open(&f).unwrap();
        let mut l = AgentLines::default();
        let t = Instant::now();
        while l.get(Some(&f)).is_none() && t.elapsed() < Duration::from_secs(5) {
            l.tick(&agents, &doc, 0);
            std::thread::sleep(Duration::from_millis(10));
        }
        let (c, lines) = l.get(Some(&f)).expect("marks");
        assert_eq!(c, color(2));
        assert_eq!(&lines[..4], &[false, true, false, true]);
        // A run from before this session doesn't count.
        let mut l = AgentLines::default();
        l.tick(&agents, &doc, u64::MAX);
        assert!(l.get(Some(&f)).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
