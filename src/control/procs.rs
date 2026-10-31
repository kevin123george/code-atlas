//! Memory and CPU of catlas and everything it started (language servers, the terminal's shell,
//! agents), from one `ps` call on a background thread every few seconds while the Control Center
//! is open. Each descendant counts toward the nearest process catlas knows (a server, the shell,
//! an agent run); the rest is "other".

use std::collections::HashMap;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub rss_kb: u64,
    pub cpu: f32,
    pub comm: String,
}

/// One sample: the process table, and the local time zone's offset from UTC in seconds.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub procs: Vec<Proc>,
    pub utc_offset: i64,
}

/// `ps -A -o pid=,ppid=,rss=,pcpu=,comm=` (macOS and Linux both understand it).
pub fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let rss_kb = it.next()?.parse().ok()?;
            let cpu = it.next()?.replace(',', ".").parse().ok()?;
            let comm: Vec<&str> = it.collect();
            let comm = comm.join(" ");
            let comm = comm.rsplit('/').next().unwrap_or("").to_string();
            Some(Proc { pid, ppid, rss_kb, cpu, comm })
        })
        .collect()
}

/// `date +%z` ("+0100", "-0430") → seconds east of UTC.
pub fn parse_offset(s: &str) -> i64 {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'-') => (-1, &s[1..]),
        Some(b'+') => (1, &s[1..]),
        _ => (1, s),
    };
    if rest.len() < 4 {
        return 0;
    }
    let h: i64 = rest[..2].parse().unwrap_or(0);
    let m: i64 = rest[2..4].parse().unwrap_or(0);
    sign * (h * 3600 + m * 60)
}

/// Takes a sample (blocking: run it on a thread).
pub fn sample() -> Sample {
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    Sample { procs: parse_ps(&run("ps", &["-A", "-o", "pid=,ppid=,rss=,pcpu=,comm="])), utc_offset: parse_offset(&run("date", &["+%z"])) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub label: String,
    pub count: usize,
    pub rss_kb: u64,
    pub cpu: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    /// catlas itself.
    pub me_kb: u64,
    pub me_cpu: f32,
    /// Its descendants by what started them (known labels first, then "other").
    pub groups: Vec<Group>,
}

impl Usage {
    pub fn total_kb(&self) -> u64 {
        self.me_kb + self.groups.iter().map(|g| g.rss_kb).sum::<u64>()
    }

    pub fn total_cpu(&self) -> f32 {
        self.me_cpu + self.groups.iter().map(|g| g.cpu).sum::<f32>()
    }
}

/// Sums `me`'s process tree. `known`: (pid, label) of the children catlas started; a process
/// counts toward its nearest known ancestor, and a known process counts as one of `count`.
pub fn summarize(procs: &[Proc], me: u32, known: &[(u32, &str)]) -> Usage {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut u = Usage::default();
    if let Some(p) = by_pid.get(&me) {
        u.me_kb = p.rss_kb;
        u.me_cpu = p.cpu;
    }
    let label_of = |pid: u32| known.iter().find(|(k, _)| *k == pid).map(|(_, l)| *l);
    let order: Vec<&str> = {
        let mut v: Vec<&str> = known.iter().map(|(_, l)| *l).collect();
        v.dedup();
        let mut seen = Vec::new();
        v.retain(|l| if seen.contains(l) { false } else { seen.push(*l); true });
        v
    };
    let mut groups: Vec<Group> = order.iter().map(|l| Group { label: l.to_string(), count: 0, rss_kb: 0, cpu: 0.0 }).collect();
    for p in procs {
        if p.pid == me {
            continue;
        }
        // Walk up to `me`; remember the nearest known process on the way.
        let mut label = None;
        let mut cur = p;
        let mut steps = 0;
        let mine = loop {
            if label.is_none() {
                label = label_of(cur.pid);
            }
            if cur.ppid == me {
                break true;
            }
            match by_pid.get(&cur.ppid) {
                Some(up) if steps < 64 && up.pid != cur.pid => {
                    cur = up;
                    steps += 1;
                }
                _ => break false,
            }
        };
        if !mine {
            continue;
        }
        let label = label.unwrap_or("other");
        let g = match groups.iter_mut().position(|g| g.label == label) {
            Some(i) => &mut groups[i],
            None => {
                groups.push(Group { label: label.to_string(), count: 0, rss_kb: 0, cpu: 0.0 });
                groups.last_mut().unwrap()
            }
        };
        if label_of(p.pid).is_some() || label == "other" {
            g.count += 1;
        }
        g.rss_kb += p.rss_kb;
        g.cpu += p.cpu;
    }
    u.groups = groups;
    u
}

/// "84 MB", "1.2 GB".
pub fn fmt_kb(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "    1     0  9000  0.0 /sbin/launchd
  100     1 50000  3.5 /Users/astra/.cargo/bin/catlas
  101   100 400000 12.0 /Users/astra/.cargo/bin/rust-analyzer
  102   101  20000  1.0 /Users/astra/.rustup/toolchains/x/bin/cargo
  103   100  4000  0,5 /bin/zsh
  104   103  2000  0.0 git
  105   100 120000 30.0 node
  106   105  1000  0.0 sh
  107   100  3000  0.0 /usr/bin/git
  200     1  7000  1.0 /Applications/Ghostty.app/Contents/MacOS/ghostty
  bad line
";

    #[test]
    fn parses_ps_output() {
        let p = parse_ps(PS);
        assert_eq!(p.len(), 10);
        assert_eq!(p[1], Proc { pid: 100, ppid: 1, rss_kb: 50000, cpu: 3.5, comm: "catlas".into() });
        assert_eq!(p[4].cpu, 0.5, "comma decimals (German locale)");
        assert_eq!(p[9].comm, "ghostty");
    }

    #[test]
    fn descendants_count_toward_the_nearest_known_process() {
        let p = parse_ps(PS);
        let u = summarize(&p, 100, &[(101, "language servers"), (103, "terminal"), (105, "agents")]);
        assert_eq!((u.me_kb, u.me_cpu), (50000, 3.5));
        let g = |l: &str| u.groups.iter().find(|g| g.label == l).cloned().unwrap();
        assert_eq!(g("language servers"), Group { label: "language servers".into(), count: 1, rss_kb: 420000, cpu: 13.0 });
        assert_eq!(g("terminal").rss_kb, 6000);
        assert_eq!(g("agents").count, 1);
        assert_eq!(g("agents").rss_kb, 121000);
        assert_eq!(g("other"), Group { label: "other".into(), count: 1, rss_kb: 3000, cpu: 0.0 });
        assert_eq!(u.total_kb(), 50000 + 420000 + 6000 + 121000 + 3000, "Ghostty and launchd are not ours");
        // Unknown pid: nothing.
        assert_eq!(summarize(&p, 999, &[]).total_kb(), 0);
    }

    #[test]
    fn offsets_and_sizes() {
        assert_eq!(parse_offset("+0100\n"), 3600);
        assert_eq!(parse_offset("-0430"), -(4 * 3600 + 30 * 60));
        assert_eq!(parse_offset(""), 0);
        assert_eq!(fmt_kb(2048), "2.0 MB");
        assert_eq!(fmt_kb(51200), "50 MB");
        assert_eq!(fmt_kb(3 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn a_real_sample_finds_this_test() {
        let s = sample();
        let me = std::process::id();
        assert!(s.procs.iter().any(|p| p.pid == me), "ps should list this process");
        assert!(s.utc_offset.abs() <= 14 * 3600);
    }
}
