//! Where the Control Center meets the parallel tracks (decision 0016). Each function is the one
//! place to change when a track lands; until then it falls back to what `main` has.
//!
//! - `roots`: track A (multi-repo workspaces, decision 0013) → every workspace root.
//! - `index_status`: track B (persistent index, decision 0014) → its progress and size.
//! - `mission_control`: track C (Mission Control, decision 0015) → its full-screen view.

use std::path::{Path, PathBuf};

use crate::app::App;

/// The repositories / roots the workspace tile shows.
pub fn roots(app: &App) -> Vec<PathBuf> {
    // HOOK(track A, decision 0013): return the workspace's roots once `src/workspace/` lands.
    vec![app.project.root.clone()]
}

pub struct IndexInfo {
    /// Files in the in-memory list (Go to file, Search Everywhere).
    pub files: usize,
    /// Still listing them.
    pub listing: bool,
    /// The persistent index's state ("1.2M symbols · 84 MB · 100%"), when there is one.
    pub persistent: Option<String>,
}

pub fn index_status(app: &App) -> IndexInfo {
    // HOOK(track B, decision 0014): fill `persistent` from `src/index/` (progress, size).
    IndexInfo { files: app.project.files.len(), listing: app.project.indexing(), persistent: None }
}

/// Opens Mission Control on a run (or its overview). Until track C lands: the agents panel,
/// focused, with that run selected.
pub fn mission_control(app: &mut App, run: Option<u64>) {
    // HOOK(track C, decision 0015): open Mission Control (full screen, per agent) instead.
    app.control.open = false;
    app.palette.open = false;
    app.project.close_popup();
    if !app.agents.visible || !app.agents.focus {
        app.agents.toggle();
    }
    app.project.focus_tree = false;
    match run.filter(|id| app.agents.run_index(*id).is_some()) {
        Some(id) => {
            app.agents.selected = Some(id);
            app.agents.follow = true;
            app.agents.area = crate::agents::Area::Runs;
            app.message = Some("Agents panel · ↑↓ runs · Ctrl+D what it changed · Esc editor".into());
        }
        None => app.message = Some("Agents panel · type a prompt and Enter starts an agent · Esc editor".into()),
    }
}

/// Seconds since the epoch at the last local midnight (`utc_offset` from `date +%z`).
pub fn local_midnight(utc_offset: i64) -> u64 {
    let now = crate::agents::run::now_secs() as i64;
    let local = now + utc_offset;
    (local - local.rem_euclid(86_400) - utc_offset).max(0) as u64
}

/// `~/Projects/x` for a path under the home folder.
pub fn short_path(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if let Ok(rest) = p.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midnight_is_today_in_the_local_zone() {
        let now = crate::agents::run::now_secs();
        for off in [0i64, 3600, -4 * 3600, 14 * 3600] {
            let m = local_midnight(off);
            assert!(m <= now && now - m < 86_400, "{off}");
            assert_eq!((m as i64 + off).rem_euclid(86_400), 0, "{off}");
        }
    }
}
