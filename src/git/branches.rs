//! Branches (list, switch, create, rename, delete with `-d` only) and the remote actions (push,
//! pull fast-forward only, fetch), each described first so the view can ask before running it.
//! Nothing here forces, resets or deletes unmerged work.

use std::path::Path;

use super::run;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub upstream: String,
    /// `[ahead 1, behind 2]` (empty when even or no upstream).
    pub track: String,
    pub age: String,
    pub subject: String,
}

pub fn parse(out: &str) -> Vec<Branch> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\x1f').collect();
            (f.len() >= 6).then(|| Branch {
                name: f[0].to_string(),
                current: f[1] == "*",
                upstream: f[2].to_string(),
                track: f[3].to_string(),
                age: f[4].to_string(),
                subject: f[5].to_string(),
            })
        })
        .collect()
}

pub fn load(root: &Path) -> Result<Vec<Branch>, String> {
    let out = run(root, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)%1f%(HEAD)%1f%(upstream:short)%1f%(upstream:track)%1f%(committerdate:relative)%1f%(subject)", "refs/heads"])?;
    Ok(parse(&out.replace("%1f", "\x1f")))
}

/// A branch name git accepts (`git check-ref-format --branch`).
pub fn valid_name(root: &Path, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Type a branch name".into());
    }
    run(root, &["check-ref-format", "--branch", name]).map(|_| ()).map_err(|_| format!("\"{name}\" isn't a valid branch name"))
}

/// The popup's state.
pub struct Branches {
    pub list: Vec<Branch>,
    pub sel: usize,
    pub error: Option<String>,
}

impl Branches {
    pub fn open(root: &Path) -> Branches {
        match load(root) {
            Ok(list) => {
                let sel = list.iter().position(|b| b.current).unwrap_or(0);
                Branches { list, sel, error: None }
            }
            Err(e) => Branches { list: Vec::new(), sel: 0, error: Some(e) },
        }
    }

    pub fn selected(&self) -> Option<&Branch> {
        self.list.get(self.sel)
    }

    pub fn step(&mut self, d: isize) {
        if !self.list.is_empty() {
            self.sel = (self.sel as isize + d).clamp(0, self.list.len() as isize - 1) as usize;
        }
    }
}

/// Where the current branch stands against its upstream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remote {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub remotes: Vec<String>,
    pub ahead: usize,
    pub behind: usize,
}

pub fn remote_info(root: &Path) -> Remote {
    let branch = run(root, &["symbolic-ref", "--short", "-q", "HEAD"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let upstream = run(root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let remotes = run(root, &["remote"]).map(|o| o.lines().map(str::to_string).collect()).unwrap_or_default();
    let (mut ahead, mut behind) = (0, 0);
    if upstream.is_some() {
        if let Ok(o) = run(root, &["rev-list", "--left-right", "--count", "@{u}...HEAD"]) {
            let mut it = o.split_whitespace();
            behind = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            ahead = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }
    Remote { branch, upstream, remotes, ahead, behind }
}

/// A remote action, ready to run once confirmed: git arguments and the question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteOp {
    pub args: Vec<String>,
    pub question: String,
    pub done: String,
}

fn commits(n: usize) -> String {
    if n == 1 { "1 commit".into() } else { format!("{n} commits") }
}

pub fn push_op(r: &Remote) -> Result<RemoteOp, String> {
    let Some(b) = &r.branch else { return Err("Not on a branch (detached HEAD): nothing to push".into()) };
    if let Some(u) = &r.upstream {
        if r.ahead == 0 {
            return Err(format!("Nothing to push: {b} has no commits that {u} lacks"));
        }
        let (remote, rbranch) = u.split_once('/').unwrap_or(("origin", u));
        let behind = if r.behind > 0 { format!(" · {u} has {} you don't have: push will be refused, pull first", commits(r.behind)) } else { String::new() };
        return Ok(RemoteOp {
            args: vec!["push".into(), remote.into(), format!("{b}:{rbranch}")],
            question: format!("Push {} of {b} → {u}{behind}? (never forced)", commits(r.ahead)),
            done: format!("Pushed {b} → {u}"),
        });
    }
    let remote = r.remotes.iter().find(|x| *x == "origin").or(r.remotes.first()).ok_or("No remote configured (git remote add …): nothing to push to")?;
    Ok(RemoteOp {
        args: vec!["push".into(), "-u".into(), remote.clone(), b.clone()],
        question: format!("Push {b} → {remote}/{b} (new there; sets it as upstream)? (never forced)"),
        done: format!("Pushed {b} → {remote}/{b}"),
    })
}

pub fn pull_op(r: &Remote) -> Result<RemoteOp, String> {
    let Some(b) = &r.branch else { return Err("Not on a branch (detached HEAD): nothing to pull into".into()) };
    let Some(u) = &r.upstream else {
        return Err(if r.remotes.is_empty() { "No remote configured: nothing to pull".into() } else { format!("{b} has no upstream yet (push it first with P)") });
    };
    let behind = if r.behind > 0 { format!("{} behind as of the last fetch", commits(r.behind)) } else { "even as of the last fetch".into() };
    Ok(RemoteOp {
        args: vec!["pull".into(), "--ff-only".into()],
        question: format!("Pull {u} into {b}, fast-forward only ({behind})?"),
        done: format!("Pulled {u} into {b}"),
    })
}

pub fn fetch_op(r: &Remote) -> Result<RemoteOp, String> {
    if r.remotes.is_empty() {
        return Err("No remote configured: nothing to fetch".into());
    }
    Ok(RemoteOp { args: vec!["fetch".into(), "--all".into()], question: format!("Fetch from {} (nothing local changes)?", r.remotes.join(", ")), done: "Fetched".into() })
}

/// Runs a confirmed remote action (on a background thread). No password prompts: git's terminal
/// prompt is off and ssh runs in batch mode unless the user set their own ssh command.
pub fn run_remote(root: &Path, op: &RemoteOp) -> Result<String, String> {
    let args: Vec<&str> = op.args.iter().map(String::as_str).collect();
    let own_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() || run(root, &["config", "core.sshCommand"]).is_ok();
    let batch = std::ffi::OsString::from("ssh -o BatchMode=yes");
    let env: Vec<(&str, &std::ffi::OsStr)> = if own_ssh { Vec::new() } else { vec![("GIT_SSH_COMMAND", batch.as_os_str())] };
    super::run_with(root, &args, &env, None).map(|_| op.done.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branches() {
        let out = "main\x1f*\x1forigin/main\x1f[ahead 1]\x1f2 hours ago\x1fFix\nside\x1f \x1f\x1f\x1f3 days ago\x1fWip\n";
        let bs = parse(out);
        assert_eq!(bs.len(), 2);
        assert!(bs[0].current && !bs[1].current);
        assert_eq!(bs[0].track, "[ahead 1]");
        assert_eq!(bs[1].upstream, "");
    }

    #[test]
    fn questions_say_what_goes_where() {
        let r = Remote { branch: Some("main".into()), upstream: Some("origin/main".into()), remotes: vec!["origin".into()], ahead: 2, behind: 0 };
        let p = push_op(&r).unwrap();
        assert_eq!(p.args, ["push", "origin", "main:main"]);
        assert_eq!(p.question, "Push 2 commits of main → origin/main? (never forced)");
        assert!(!p.args.iter().any(|a| a.contains("force") || a == "-f"));
        assert!(push_op(&Remote { ahead: 0, ..r.clone() }).unwrap_err().starts_with("Nothing to push"));
        let new = push_op(&Remote { upstream: None, ahead: 0, ..r.clone() }).unwrap();
        assert_eq!(new.args, ["push", "-u", "origin", "main"]);
        assert!(push_op(&Remote { upstream: None, remotes: vec![], ..r.clone() }).is_err());
        assert!(push_op(&Remote { branch: None, ..r.clone() }).is_err());
        let pl = pull_op(&Remote { behind: 3, ..r.clone() }).unwrap();
        assert_eq!(pl.args, ["pull", "--ff-only"]);
        assert!(pl.question.contains("3 commits behind"));
        assert!(fetch_op(&Remote::default()).is_err());
    }

    /// Push and pull against a bare repository in the temp dir (never a real remote).
    #[test]
    fn push_and_pull_with_a_local_bare_remote() {
        let dir = crate::git::tests::scratch_repo("push");
        let bare = dir.with_extension("bare.git");
        let _ = std::fs::remove_dir_all(&bare);
        run(&dir, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]).unwrap();
        run(&dir, &["remote", "add", "origin", bare.to_str().unwrap()]).unwrap();
        let r = remote_info(&dir);
        assert_eq!((r.branch.as_deref(), r.upstream.as_deref()), (Some("main"), None));
        let op = push_op(&r).unwrap();
        assert_eq!(run_remote(&dir, &op).unwrap(), "Pushed main → origin/main");
        let r = remote_info(&dir);
        assert_eq!(r.upstream.as_deref(), Some("origin/main"));
        assert_eq!((r.ahead, r.behind), (0, 0));
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        run(&dir, &["commit", "-qam", "two"]).unwrap();
        let r = remote_info(&dir);
        assert_eq!(r.ahead, 1);
        run_remote(&dir, &push_op(&r).unwrap()).unwrap();
        assert_eq!(run(&bare, &["log", "--format=%s", "-n1"]).unwrap(), "two\n");
        run_remote(&dir, &fetch_op(&r).unwrap()).unwrap();
        run_remote(&dir, &pull_op(&remote_info(&dir)).unwrap()).unwrap();
        // Branch list and a valid name check.
        let bs = load(&dir).unwrap();
        assert_eq!(bs[0].name, "main");
        assert!(bs[0].current);
        assert!(valid_name(&dir, "feature/x").is_ok());
        assert!(valid_name(&dir, "bad..name").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bare).unwrap();
    }
}
