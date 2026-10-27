//! The "Staff" section: status of the persistent Aa staff sessions (central decision 0023), from
//! `aa-staff.sh status` on the laptop. Fetched off the UI thread every 60 s while the panel shows.
//!
//! The script prints one line per `claude agents` session named "Aa · …":
//! `printf "%-14s %s  %-8s %-7s %s"  name id state live|stopped cwd`, oldest first, and keeps old
//! stopped sessions, so the same name shows up many times. We keep one row per name: the live
//! session if there is one, else the newest.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

pub const REFRESH: Duration = Duration::from_secs(60);
const LAPTOP: &str = "astra@astra"; // Tailscale MagicDNS name of the GPU laptop

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub name: String,
    pub id: String,
    pub state: String,
    pub live: bool,
    pub cwd: String,
    /// Other (older, stopped) sessions under the same name.
    pub others: usize,
}

pub enum Status {
    Never,
    Ok(Vec<Member>),
    Unreachable,
}

pub struct Staff {
    pub status: Status,
    pub at: Option<Instant>,
    pending: Option<Receiver<Option<String>>>,
    /// "laptop" or "local".
    pub source: &'static str,
}

impl Default for Staff {
    fn default() -> Self {
        Staff { status: Status::Never, at: None, pending: None, source: if local_script().is_some() { "local" } else { "laptop" } }
    }
}

/// On the laptop itself (hostname `astra`) the script is run directly instead of over SSH. The
/// script's presence alone says nothing: the Mac has `~/atelier-astra` too (a link to the repo),
/// but the staff sessions live on the laptop.
fn local_script() -> Option<PathBuf> {
    let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if host.trim() != "astra" {
        return None;
    }
    let p = PathBuf::from(std::env::var_os("HOME")?).join("atelier-astra/tools/aa-staff.sh");
    p.is_file().then_some(p)
}

fn command() -> Command {
    // Override for tests and other setups: any shell command that prints the status lines.
    if let Ok(c) = std::env::var("CATLAS_STAFF_CMD") {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &c]);
        return cmd;
    }
    if let Some(p) = local_script() {
        let mut cmd = Command::new("bash");
        cmd.arg(p).arg("status");
        return cmd;
    }
    let mut cmd = Command::new("ssh");
    // BatchMode: never ask for a password or host key (it would draw over the editor).
    cmd.args(["-o", "ConnectTimeout=5", "-o", "BatchMode=yes", LAPTOP, "~/atelier-astra/tools/aa-staff.sh status"]);
    cmd
}

impl Staff {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts a refresh when due (never fetched, or 60 s old) and none is running.
    pub fn maybe_refresh(&mut self) {
        if self.pending.is_none() && self.at.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.refresh();
        }
    }

    pub fn refresh(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        thread::spawn(move || {
            let out = command().stdin(Stdio::null()).stderr(Stdio::null()).output();
            let text = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            let _ = tx.send(text);
        });
    }

    /// Takes a finished fetch. Returns true when the status changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else { return false };
        match rx.try_recv() {
            Ok(text) => {
                self.status = match text {
                    Some(t) => Status::Ok(parse(&t)),
                    None => Status::Unreachable,
                };
                self.at = Some(Instant::now());
                self.pending = None;
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.status = Status::Unreachable;
                self.at = Some(Instant::now());
                self.pending = None;
                true
            }
        }
    }
}

/// Parses the status lines into one row per name (order of first appearance).
pub fn parse(out: &str) -> Vec<Member> {
    let mut rows: Vec<Member> = Vec::new();
    for line in out.lines() {
        // From the right: cwd, live, state, id; the name (with spaces) is the rest.
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() < 5 {
            continue;
        }
        let n = toks.len();
        let (id, state, live, cwd) = (toks[n - 4], toks[n - 3], toks[n - 2], toks[n - 1]);
        if !matches!(live, "live" | "stopped") {
            continue;
        }
        let m = Member { name: toks[..n - 4].join(" "), id: id.into(), state: state.into(), live: live == "live", cwd: cwd.into(), others: 0 };
        match rows.iter_mut().find(|r| r.name == m.name) {
            Some(r) => {
                let others = r.others + 1;
                // A live one wins; otherwise the later (newer) line does.
                if m.live || !r.live {
                    *r = m;
                }
                r.others = others;
            }
            None => rows.push(m),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output from the laptop on 2026-10-25 (shortened).
    const SAMPLE: &str = "\
Aa · Director  6aa4b472  blocked  stopped /home/astra/atelier-astra
Aa · App Dev   3cdc8348  blocked  stopped /home/astra/mobile-apps
Aa · Video     afbdb3a5  blocked  stopped /home/astra/Projects/tech-shorts
Aa · Director  347e5939  blocked  stopped /home/astra/atelier-astra
Aa · App Dev   f907d356  blocked  live    /home/astra/mobile-apps
Aa · Ops       85360144  done     live    /home/astra/home-lab
Aa · Director  34eb1d5c  done     live    /home/astra/atelier-astra
Aa · Video     afd38ea8  done     stopped /home/astra/Projects/tech-shorts
";

    #[test]
    fn one_row_per_name_live_first() {
        let rows = parse(SAMPLE);
        let short: Vec<(&str, &str, &str, bool, usize)> =
            rows.iter().map(|m| (m.name.as_str(), m.id.as_str(), m.state.as_str(), m.live, m.others)).collect();
        assert_eq!(
            short,
            [
                ("Aa · Director", "34eb1d5c", "done", true, 2),
                ("Aa · App Dev", "f907d356", "blocked", true, 1),
                ("Aa · Video", "afd38ea8", "done", false, 1),
                ("Aa · Ops", "85360144", "done", true, 0),
            ]
        );
        assert_eq!(rows[3].cwd, "/home/astra/home-lab");
    }

    #[test]
    fn a_live_session_is_not_replaced_by_a_newer_stopped_one() {
        let rows = parse("Aa · Ops  aaaa1111  done  live  /x\nAa · Ops  bbbb2222  blocked  stopped  /x\n");
        assert_eq!((rows[0].id.as_str(), rows[0].others), ("aaaa1111", 1));
    }

    #[test]
    fn noise_is_skipped() {
        assert!(parse("").is_empty());
        assert!(parse("ssh: connect to host astra port 22: Operation timed out\n").is_empty());
    }

    #[test]
    fn a_failing_command_reads_as_unreachable() {
        std::env::set_var("CATLAS_STAFF_CMD", "exit 255");
        let mut s = Staff::default();
        s.refresh();
        let t = Instant::now();
        while !s.poll() && t.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(10));
        }
        std::env::remove_var("CATLAS_STAFF_CMD");
        assert!(matches!(s.status, Status::Unreachable));
        assert!(!s.busy());
    }
}
