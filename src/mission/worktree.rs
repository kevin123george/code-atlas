//! Git worktrees for agents (decision 0015): each agent that changes code gets its own checkout,
//! `<repo>-wt/<slug>` on a new branch `<slug>` from the current HEAD, so several agents can work
//! at once without touching each other or your checkout. Landing merges the branch into the base
//! branch (fast-forward, a merge commit, or a rebase first), never pushes, never forces; removing
//! refuses a dirty worktree; discarding deletes worktree and branch after a question.
//!
//! Everything here is plain `git` calls (`crate::git::run`); the callers run them on threads.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::run;
use crate::project::canonical;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Worktree {
    /// The agent's checkout (canonical).
    pub path: PathBuf,
    pub branch: String,
    /// The checkout it was made from: landing merges there.
    pub repo: PathBuf,
    /// The branch checked out in `repo` when it was made (`None`: a detached HEAD).
    pub base_branch: Option<String>,
    /// The commit it started from.
    pub base_head: String,
}

impl Worktree {
    /// `fix-parser` (in `code-atlas-wt`).
    pub fn short(&self) -> String {
        let dir = self.path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{dir}/{}", self.branch)
    }

    pub fn base_label(&self) -> String {
        self.base_branch.clone().unwrap_or_else(|| self.base_head.get(..8).unwrap_or(&self.base_head).to_string())
    }
}

/// A branch name from a task: lower-case words joined by `-`, at most 5 words / 32 characters.
pub fn slugify(text: &str) -> String {
    let mut words = Vec::new();
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if w.is_empty() {
            continue;
        }
        words.push(w.to_ascii_lowercase());
        if words.len() == 5 {
            break;
        }
    }
    let mut s = String::new();
    for w in words {
        if !s.is_empty() && s.len() + 1 + w.len() > 32 {
            break;
        }
        if !s.is_empty() {
            s.push('-');
        }
        s.push_str(&w[..w.len().min(32)]);
    }
    if s.is_empty() {
        "agent".into()
    } else {
        s
    }
}

/// The main checkout of the repository `root` is in (a linked worktree's main one).
fn main_checkout(root: &Path) -> PathBuf {
    match run(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        Ok(c) => {
            let c = PathBuf::from(c.trim());
            match c.file_name() {
                Some(n) if n == ".git" => c.parent().map(canonical).unwrap_or_else(|| root.to_path_buf()),
                _ => root.to_path_buf(),
            }
        }
        Err(_) => root.to_path_buf(),
    }
}

/// Where the worktrees of `root`'s repository go: `<main checkout>-wt` next to it.
pub fn wt_dir(root: &Path) -> PathBuf {
    let main = main_checkout(root);
    let name = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    main.parent().unwrap_or(&main).join(format!("{name}-wt"))
}

fn branch_exists(root: &Path, b: &str) -> bool {
    run(root, &["rev-parse", "-q", "--verify", &format!("refs/heads/{b}")]).is_ok()
}

/// Makes `<repo>-wt/<slug>` on a new branch from HEAD of the checkout `dir` is in. A name that is
/// taken (folder or branch) gets `-2`, `-3`, ...
pub fn create(dir: &Path, task: &str) -> Result<Worktree, String> {
    let root = crate::git::toplevel(dir).ok_or_else(|| format!("{} is not in a git repository", dir.display()))?;
    let head = run(&root, &["rev-parse", "-q", "--verify", "HEAD"]).map_err(|_| "the repository has no commit yet: a worktree needs one".to_string())?;
    let head = head.trim().to_string();
    let base_branch = run(&root, &["symbolic-ref", "--short", "-q", "HEAD"]).ok().map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    let parent = wt_dir(&root);
    let slug = slugify(task);
    let mut name = slug.clone();
    let mut n = 2;
    while parent.join(&name).exists() || branch_exists(&root, &name) {
        name = format!("{slug}-{n}");
        n += 1;
        if n > 999 {
            return Err("no free worktree name".into());
        }
    }
    std::fs::create_dir_all(&parent).map_err(|e| format!("couldn't create {}: {e}", parent.display()))?;
    let path = parent.join(&name);
    let p = path.to_string_lossy().into_owned();
    run(&root, &["worktree", "add", "-q", "-b", &name, &p, &head])?;
    Ok(Worktree { path: canonical(&path), branch: name, repo: root, base_branch, base_head: head })
}

/// `git status --porcelain` lines of the worktree (empty = clean). A missing folder is an error.
pub fn dirty(w: &Path) -> Result<Vec<String>, String> {
    if !w.exists() {
        return Err(format!("{} is gone", w.display()));
    }
    let out = run(w, &["status", "--porcelain", "--untracked-files=all"])?;
    Ok(out.lines().filter(|l| !l.trim().is_empty()).map(|l| l.get(3..).unwrap_or(l).to_string()).collect())
}

fn count(root: &Path, range: &str) -> usize {
    run(root, &["rev-list", "--count", range]).ok().and_then(|c| c.trim().parse().ok()).unwrap_or(0)
}

/// What landing would do, for the question.
#[derive(Clone, Debug, PartialEq)]
pub struct LandPlan {
    pub base: String,
    /// Uncommitted files in the worktree (committed first, as one commit).
    pub uncommitted: usize,
    /// Commits on the agent's branch that the base doesn't have.
    pub ahead: usize,
    /// Commits the base got since (0 = a fast-forward).
    pub behind: usize,
}

impl LandPlan {
    pub fn ff(&self) -> bool {
        self.behind == 0
    }

    /// The question asked before landing (y merge, b rebase first, n no).
    pub fn question(&self, w: &Worktree) -> String {
        let mut what = Vec::new();
        if self.uncommitted > 0 {
            what.push(format!("commit {} uncommitted file{} on {}", self.uncommitted, plural(self.uncommitted), w.branch));
        }
        let commits = self.ahead + usize::from(self.uncommitted > 0);
        if self.ff() {
            what.push(format!("fast-forward {} to {} ({commits} commit{})", self.base, w.branch, plural(commits)));
            format!("Land: {}? Never pushed. (y yes · n no)", what.join(", then "))
        } else {
            what.push(format!("merge {} into {} ({} moved on by {})", w.branch, self.base, self.base, self.behind));
            format!("Land: {}? (y merge · b rebase first, then fast-forward · n no) Never pushed.", what.join(", then "))
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Checks that landing can happen and says what it would do.
pub fn plan_land(w: &Worktree) -> Result<LandPlan, String> {
    let base = w.base_branch.clone().ok_or("it started from a detached HEAD: merge its branch by hand")?;
    let cur = run(&w.repo, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|b| b.trim().to_string()).unwrap_or_default();
    if cur != base {
        return Err(format!("{} is on {} now, not {base}: switch back first", w.repo.display(), if cur.is_empty() { "a detached HEAD" } else { &cur }));
    }
    let uncommitted = dirty(&w.path)?.len();
    let ahead = count(&w.repo, &format!("{base}..{}", w.branch));
    let behind = count(&w.repo, &format!("{}..{base}", w.branch));
    if uncommitted == 0 && ahead == 0 {
        return Err(format!("nothing to land: {} has no commits or changes beyond {base}", w.branch));
    }
    Ok(LandPlan { base, uncommitted, ahead, behind })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// Fast-forward when possible, else a merge commit.
    Merge,
    /// Rebase the agent's branch onto the base first, then fast-forward.
    Rebase,
}

/// The commit message for an agent's uncommitted work (the trailer marks it ✳ in the log).
pub fn commit_message(title: &str, id: u64) -> String {
    let first: String = title.lines().next().unwrap_or("agent work").chars().take(72).collect();
    format!("{first}\n\nAgent run #{id}, landed from Code Atlas mission control.\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n")
}

/// Lands the agent's branch in its base branch: commits leftover changes in the worktree, then
/// fast-forwards / merges (or rebases first). A conflict aborts the merge or rebase, so nothing
/// is left half done. Never pushes, never forces. Returns what happened.
pub fn land(w: &Worktree, title: &str, id: u64, how: How) -> Result<String, String> {
    let plan = plan_land(w)?;
    let base = &plan.base;
    if plan.uncommitted > 0 {
        run(&w.path, &["add", "-A"])?;
        run(&w.path, &["commit", "-q", "-m", &commit_message(title, id)]).map_err(|e| format!("couldn't commit the agent's changes: {e}"))?;
    }
    let mut how_label = "fast-forward";
    if !plan.ff() {
        match how {
            How::Rebase => {
                if let Err(e) = run(&w.path, &["rebase", "-q", base]) {
                    let _ = run(&w.path, &["rebase", "--abort"]);
                    return Err(format!("rebase onto {base} hit conflicts and was aborted ({base} unchanged; the work stays on {}): {}", w.branch, last_line(&e)));
                }
                how_label = "rebased, fast-forward";
            }
            How::Merge => how_label = "merge commit",
        }
    }
    let ff = run(&w.repo, &["merge-base", "--is-ancestor", base, &w.branch]).is_ok();
    let r = if ff {
        run(&w.repo, &["merge", "-q", "--ff-only", &w.branch])
    } else {
        let msg = format!("Merge agent branch {}: {}", w.branch, title.lines().next().unwrap_or(""));
        run(&w.repo, &["merge", "-q", "--no-ff", "--no-edit", "-m", &msg, &w.branch])
    };
    if let Err(e) = r {
        if run(&w.repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok() {
            let _ = run(&w.repo, &["merge", "--abort"]);
            return Err(format!("merging {} hit conflicts and was aborted ({base} unchanged; the work stays on {}; resolve in its worktree): {}", w.branch, w.branch, last_line(&e)));
        }
        return Err(format!("couldn't merge {}: {}", w.branch, last_line(&e)));
    }
    let n = plan.ahead + usize::from(plan.uncommitted > 0);
    Ok(format!("Landed {} in {base} ({how_label}, {n} commit{})", w.branch, plural(n)))
}

fn last_line(e: &str) -> String {
    e.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or(e).to_string()
}

/// Removes a clean worktree and its branch (`git branch -d`: only when merged). Refuses when the
/// worktree has uncommitted files.
pub fn remove(w: &Worktree) -> Result<String, String> {
    if w.path.exists() {
        let d = dirty(&w.path)?;
        if !d.is_empty() {
            return Err(format!("{} has {} uncommitted file{} ({}): not removed", w.short(), d.len(), plural(d.len()), d.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
        }
        run(&w.repo, &["worktree", "remove", &w.path.to_string_lossy()])?;
    } else {
        let _ = run(&w.repo, &["worktree", "prune"]);
    }
    match run(&w.repo, &["branch", "-q", "-d", &w.branch]) {
        Ok(_) => Ok(format!("Removed worktree {} and branch {}", w.short(), w.branch)),
        Err(_) if !branch_exists(&w.repo, &w.branch) => Ok(format!("Removed worktree {}", w.short())),
        Err(_) => Ok(format!("Removed worktree {} (branch {} kept: it isn't merged)", w.short(), w.branch)),
    }
}

/// What discarding would lose, for the question.
pub fn discard_question(w: &Worktree) -> String {
    let files = dirty(&w.path).map(|d| d.len()).unwrap_or(0);
    let base = w.base_branch.clone().unwrap_or_else(|| w.base_head.clone());
    let commits = count(&w.repo, &format!("{base}..{}", w.branch));
    format!(
        "Discard {}? Can't be undone: deletes its worktree ({files} uncommitted file{}) and branch {} ({commits} commit{} not in {}). (y yes · n no)",
        w.branch,
        plural(files),
        w.branch,
        plural(commits),
        w.base_label()
    )
}

/// Deletes the worktree and its branch, whatever is in them (after the question).
pub fn discard(w: &Worktree) -> Result<String, String> {
    if w.path.exists() {
        run(&w.repo, &["worktree", "remove", "--force", "--force", &w.path.to_string_lossy()])?;
    } else {
        let _ = run(&w.repo, &["worktree", "prune"]);
    }
    if branch_exists(&w.repo, &w.branch) {
        run(&w.repo, &["branch", "-q", "-D", &w.branch])?;
    }
    Ok(format!("Discarded {}: worktree and branch deleted", w.short()))
}

/// The tree of a commit (an agent run's baseline in a fresh worktree is HEAD's tree: no snapshot).
pub fn tree_of(dir: &Path, rev: &str) -> Result<String, String> {
    run(dir, &["rev-parse", &format!("{rev}^{{tree}}")]).map(|t| t.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::scratch_repo;

    fn repo(name: &str) -> PathBuf {
        let d = scratch_repo(name);
        std::fs::write(d.join("a.txt"), "one\ntwo\n").unwrap();
        run(&d, &["add", "."]).unwrap();
        run(&d, &["commit", "-qm", "first"]).unwrap();
        let _ = std::fs::remove_dir_all(wt_dir(&d));
        d
    }

    fn cleanup(d: &Path) {
        let _ = std::fs::remove_dir_all(wt_dir(d));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Fix the parser bug in src/parse.rs, please!"), "fix-the-parser-bug-in");
        assert_eq!(slugify("  ##  "), "agent");
        assert_eq!(slugify("écrire des tests"), "crire-des-tests");
        assert!(slugify(&"x".repeat(80)).len() <= 32);
    }

    #[test]
    fn create_land_fast_forward_and_remove() {
        let d = repo("wt-ff");
        let branch = run(&d, &["symbolic-ref", "--short", "HEAD"]).unwrap().trim().to_string();
        let w = create(&d, "Add a third line").unwrap();
        assert_eq!(w.branch, "add-a-third-line");
        assert_eq!(w.path, canonical(&wt_dir(&d).join("add-a-third-line")));
        assert!(w.path.join("a.txt").exists());
        assert_eq!(w.base_branch.as_deref(), Some(branch.as_str()));
        // A second agent with the same task gets its own name.
        let w2 = create(&d, "Add a third line").unwrap();
        assert_eq!(w2.branch, "add-a-third-line-2");
        // Nothing to land yet.
        assert!(plan_land(&w).unwrap_err().contains("nothing to land"));
        // The agent edits without committing (as `claude -p` in acceptEdits does).
        std::fs::write(w.path.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(w.path.join("new.txt"), "new\n").unwrap();
        let plan = plan_land(&w).unwrap();
        assert_eq!((plan.uncommitted, plan.ahead, plan.behind), (2, 0, 0));
        assert!(plan.question(&w).contains("commit 2 uncommitted files on add-a-third-line, then fast-forward"), "{}", plan.question(&w));
        // A dirty worktree isn't removed.
        assert!(remove(&w).unwrap_err().contains("2 uncommitted files"));
        let msg = land(&w, "Add a third line", 4, How::Merge).unwrap();
        assert!(msg.contains("fast-forward, 1 commit"), "{msg}");
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
        let log = run(&d, &["log", "-1", "--format=%B"]).unwrap();
        assert!(log.contains("Co-Authored-By: Claude") && log.contains("Agent run #4"), "{log}");
        // Clean now: removed with its (merged) branch.
        let msg = remove(&w).unwrap();
        assert!(msg.contains("and branch"), "{msg}");
        assert!(!w.path.exists());
        assert!(!branch_exists(&d, &w.branch));
        // Discard: the other one goes with its work.
        std::fs::write(w2.path.join("junk.txt"), "x\n").unwrap();
        let q = discard_question(&w2);
        assert!(q.contains("1 uncommitted file") && q.contains("Can't be undone"), "{q}");
        discard(&w2).unwrap();
        assert!(!w2.path.exists() && !branch_exists(&d, &w2.branch));
        assert!(!d.join("junk.txt").exists());
        cleanup(&d);
    }

    #[test]
    fn land_merges_or_rebases_when_the_base_moved_and_aborts_conflicts() {
        let d = repo("wt-merge");
        let w = create(&d, "agent one").unwrap();
        std::fs::write(w.path.join("b.txt"), "agent\n").unwrap();
        // Meanwhile the base moves on.
        std::fs::write(d.join("c.txt"), "mine\n").unwrap();
        run(&d, &["add", "."]).unwrap();
        run(&d, &["commit", "-qm", "mine"]).unwrap();
        let plan = plan_land(&w).unwrap();
        assert_eq!(plan.behind, 1);
        assert!(plan.question(&w).contains("b rebase first"));
        let msg = land(&w, "agent one", 1, How::Merge).unwrap();
        assert!(msg.contains("merge commit"), "{msg}");
        assert!(d.join("b.txt").exists() && d.join("c.txt").exists());
        assert_eq!(run(&d, &["log", "-1", "--format=%p"]).unwrap().split_whitespace().count(), 2, "a merge commit");

        // Rebase first: a straight line.
        let w2 = create(&d, "agent two").unwrap();
        std::fs::write(w2.path.join("e.txt"), "two\n").unwrap();
        std::fs::write(d.join("f.txt"), "mine again\n").unwrap();
        run(&d, &["add", "."]).unwrap();
        run(&d, &["commit", "-qm", "mine again"]).unwrap();
        let msg = land(&w2, "agent two", 2, How::Rebase).unwrap();
        assert!(msg.contains("rebased"), "{msg}");
        assert_eq!(run(&d, &["log", "-1", "--format=%p"]).unwrap().split_whitespace().count(), 1, "no merge commit");

        // A conflict: aborted, the base is untouched.
        let w3 = create(&d, "agent three").unwrap();
        std::fs::write(w3.path.join("a.txt"), "AGENT\n").unwrap();
        std::fs::write(d.join("a.txt"), "MINE\n").unwrap();
        run(&d, &["commit", "-qam", "conflicting"]).unwrap();
        let head = run(&d, &["rev-parse", "HEAD"]).unwrap();
        let e = land(&w3, "agent three", 3, How::Merge).unwrap_err();
        assert!(e.contains("conflicts and was aborted"), "{e}");
        assert_eq!(run(&d, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "MINE\n");
        assert!(run(&d, &["status", "--porcelain"]).unwrap().trim().is_empty(), "no half-done merge left");
        let e = land(&w3, "agent three", 3, How::Rebase).unwrap_err();
        assert!(e.contains("rebase") && e.contains("aborted"), "{e}");
        assert!(dirty(&w3.path).unwrap().is_empty(), "the worktree's rebase was aborted too");
        // Off the base branch: refused.
        run(&d, &["checkout", "-q", "-b", "elsewhere"]).unwrap();
        assert!(plan_land(&w3).unwrap_err().contains("switch back first"));
        cleanup(&d);
    }

    #[test]
    fn worktrees_of_a_worktree_go_next_to_the_main_checkout() {
        let d = repo("wt-nested");
        let w = create(&d, "outer").unwrap();
        let inner = create(&w.path, "inner").unwrap();
        assert_eq!(inner.path.parent(), w.path.parent(), "one <repo>-wt folder per repository");
        assert_eq!(inner.repo, w.path, "lands where it was made from");
        assert!(create(&std::env::temp_dir().join("catlas-no-such-dir"), "x").is_err());
        cleanup(&d);
    }
}
