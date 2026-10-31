//! The action registry (decision 0016): every command of the editor with its name, group, key and
//! what running it does. Find Action (the palette) lists these; the Control Center runs some;
//! `palette::tests` checks the table against the shortcut helper's rows (`hints/table.rs`), so
//! the two can't drift.
//!
//! **A new feature registers its actions** by adding a `pub const ACTIONS: &[Action]` slice
//! (in its own module) to [`all`]. Most actions just press their key (`Run::Keys`), so running one
//! from the palette does exactly what the key does; `Run::Editor` gives the editor the focus
//! first (for editing keys); `Run::Do` calls a function (toggles that also save, things with no
//! key).

use crate::app::App;

#[derive(Clone, Copy)]
pub enum Run {
    /// Press these keys in order (labels in the hint table's syntax; the first alternative).
    Keys(&'static [&'static str]),
    /// The same, with the editor focused first (tree, panels, dock and Git view let go).
    Editor(&'static [&'static str]),
    Do(fn(&mut App)),
}

#[derive(Clone, Copy)]
pub struct Action {
    /// Stable id (saved in the recent-actions list).
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    /// The key that does the same, as the kitty protocol sends it (hint-table syntax).
    pub key: Option<&'static str>,
    /// The key to show without the kitty protocol, when it differs.
    pub legacy: Option<&'static str>,
    /// More words to find it by.
    pub alias: &'static str,
    pub run: Run,
    /// A live value to show next to it ("on", "ledger-dark").
    pub state: Option<fn(&App) -> String>,
}

impl Action {
    /// The key shown for this terminal.
    pub fn shown_key(&self, kitty: bool) -> Option<&'static str> {
        if kitty { self.key } else { self.legacy.or(self.key) }
    }
}

const fn k(id: &'static str, group: &'static str, name: &'static str, key: &'static str, keys: &'static [&'static str]) -> Action {
    Action { id, name, group, key: Some(key), legacy: None, alias: "", run: Run::Keys(keys), state: None }
}

const fn e(id: &'static str, group: &'static str, name: &'static str, key: &'static str, keys: &'static [&'static str]) -> Action {
    Action { id, name, group, key: Some(key), legacy: None, alias: "", run: Run::Editor(keys), state: None }
}

const fn d(id: &'static str, group: &'static str, name: &'static str, run: fn(&mut App)) -> Action {
    Action { id, name, group, key: None, legacy: None, alias: "", run: Run::Do(run), state: None }
}

const fn alias(mut a: Action, words: &'static str) -> Action {
    a.alias = words;
    a
}

const fn legacy(mut a: Action, key: &'static str) -> Action {
    a.legacy = Some(key);
    a
}

const fn keyed(mut a: Action, key: &'static str) -> Action {
    a.key = Some(key);
    a
}

const fn state(mut a: Action, f: fn(&App) -> String) -> Action {
    a.state = Some(f);
    a
}

pub const FILE: &[Action] = &[
    e("file.save", "File", "Save", "Ctrl+S", &["Ctrl+S"]),
    d("file.save-all", "File", "Save all", |a| a.save_all()),
    e("file.new", "File", "New file", "Ctrl+N", &["Ctrl+N"]),
    alias(e("file.close", "File", "Close tab", "Ctrl+W", &["Ctrl+W"]), "close file"),
    e("file.next-tab", "File", "Next tab", "Alt+→", &["Alt+→"]),
    e("file.prev-tab", "File", "Previous tab", "Alt+←", &["Alt+←"]),
    alias(k("file.reload", "File", "Reload file from disk", "Ctrl+Alt+Y", &["Ctrl+Alt+Y"]), "revert refresh"),
    alias(e("file.quit", "File", "Quit", "Ctrl+Q", &["Ctrl+Q"]), "exit"),
];

pub const EDIT: &[Action] = &[
    e("edit.undo", "Edit", "Undo", "Ctrl+Z", &["Ctrl+Z"]),
    legacy(e("edit.redo", "Edit", "Redo", "Ctrl+Shift+Z", &["Alt+Z"]), "Alt+Z"),
    e("edit.copy", "Edit", "Copy", "Ctrl+C", &["Ctrl+C"]),
    e("edit.cut", "Edit", "Cut", "Ctrl+X", &["Ctrl+X"]),
    e("edit.paste", "Edit", "Paste", "Ctrl+V", &["Ctrl+V"]),
    e("edit.select-all", "Edit", "Select all", "Ctrl+A", &["Ctrl+A"]),
    e("edit.duplicate", "Edit", "Duplicate line or selection", "Ctrl+D", &["Ctrl+D"]),
    e("edit.delete-line", "Edit", "Delete line", "Ctrl+Y", &["Ctrl+Y"]),
    e("edit.move-up", "Edit", "Move lines up", "Alt+Shift+↑", &["Alt+Shift+↑"]),
    e("edit.move-down", "Edit", "Move lines down", "Alt+Shift+↓", &["Alt+Shift+↓"]),
    alias(e("edit.comment", "Edit", "Comment / uncomment lines", "Ctrl+/", &["Ctrl+/"]), "toggle comment"),
    e("edit.find", "Edit", "Find", "Ctrl+F", &["Ctrl+F"]),
    e("edit.find-next", "Edit", "Find next", "F3", &["F3"]),
    legacy(e("edit.find-prev", "Edit", "Find previous", "Shift+F3", &["Alt+P"]), "Alt+P"),
    alias(e("edit.goto-line", "Edit", "Go to line", "Ctrl+G", &["Ctrl+G"]), "jump line number"),
];

pub const NAVIGATE: &[Action] = &[
    alias(k("nav.everywhere", "Navigate", "Search Everywhere", "Ctrl+T", &["Ctrl+T"]), "files text"),
    legacy(k("nav.file", "Navigate", "Go to file", "Ctrl+Shift+N", &["Ctrl+P"]), "Ctrl+P"),
    alias(legacy(k("nav.find-in-files", "Navigate", "Find in files", "Ctrl+Shift+F", &["Alt+F"]), "Alt+F"), "grep search text"),
    k("nav.recent", "Navigate", "Recent files", "Ctrl+E", &["Ctrl+E"]),
    legacy(e("nav.symbol", "Navigate", "Go to symbol", "Ctrl+Alt+Shift+N", &["Alt+N"]), "Alt+N"),
    alias(e("nav.structure", "Navigate", "File structure", "Ctrl+F12", &["Ctrl+F12"]), "outline symbols"),
    k("nav.reveal", "Navigate", "Reveal file in project tree", "Alt+F1", &["Alt+F1"]),
];

pub const CODE: &[Action] = &[
    alias(legacy(e("code.complete", "Code", "Complete", "Ctrl+Space", &["Ctrl+J"]), "Ctrl+J"), "completion autocomplete"),
    e("code.definition", "Code", "Go to definition", "Ctrl+B", &["Ctrl+B"]),
    e("code.implementation", "Code", "Go to implementation", "Ctrl+Alt+B", &["Ctrl+Alt+B"]),
    e("code.back", "Code", "Back", "Ctrl+Alt+←", &["Ctrl+Alt+←"]),
    alias(e("code.usages", "Code", "Find usages", "Alt+F7", &["Alt+F7"]), "references"),
    alias(e("code.docs", "Code", "Quick documentation", "F1", &["F1"]), "hover"),
    e("code.rename", "Code", "Rename symbol", "Shift+F6", &["Shift+F6"]),
    alias(e("code.format", "Code", "Reformat file", "Ctrl+Alt+L", &["Ctrl+Alt+L"]), "rustfmt prettier"),
    alias(e("code.fixes", "Code", "Quick fixes", "Alt+Enter", &["Alt+Enter"]), "code actions intentions"),
    e("code.next-problem", "Code", "Next problem", "F2", &["F2"]),
    e("code.prev-problem", "Code", "Previous problem", "Shift+F2", &["Shift+F2"]),
    alias(e("code.problems", "Code", "Problems of this file", "Alt+6", &["Alt+6"]), "errors warnings diagnostics"),
];

pub const TOOLS: &[Action] = &[
    alias(k("ws.repos", "Tool windows", "Repos (workspace)", "Ctrl+O", &["Ctrl+O"]), "repositories roots fetch pull all"),
    alias(legacy(keyed(d("palette.open", "Tool windows", "Find Action", |a| a.open_palette()), "Ctrl+Shift+A"), "Ctrl+R"), "command palette actions"),
    alias(k("tool.project", "Tool windows", "Project tree", "Alt+1", &["Alt+1"]), "sidebar files"),
    alias(k("tool.agents", "Tool windows", "Agents panel", "Alt+9", &["Alt+9"]), "claude"),
    alias(k("tool.terminal", "Tool windows", "Terminal", "Alt+F12", &["Alt+F12"]), "shell"),
    k("tool.commit", "Tool windows", "Commit panel", "Alt+0", &["Alt+0"]),
    k("tool.commit-message", "Tool windows", "Commit (message)", "Ctrl+K", &["Ctrl+K"]),
    alias(k("tool.git", "Tool windows", "Git view (review, log)", "Ctrl+L", &["Ctrl+L"]), "changes diff review"),
];

pub const GIT: &[Action] = &[
    alias(d("git.log", "Git", "Git log", |a| a.git_view_tab('2')), "history commits"),
    alias(d("git.changes", "Git", "Review changes", |a| a.git_view_tab('1')), "diff working tree"),
    d("git.branches", "Git", "Branches", |a| a.git_view_tab('B')),
    alias(d("git.blame", "Git", "Blame this file", |a| a.git_view_tab('b')), "annotate"),
    d("git.push", "Git", "Push… (asks first)", |a| a.git_view_tab('P')),
    d("git.pull", "Git", "Pull… (asks first)", |a| a.git_view_tab('U')),
    d("git.fetch", "Git", "Fetch… (asks first)", |a| a.git_view_tab('F')),
];

pub const AGENTS: &[Action] = &[
    alias(d("agents.new", "Agents", "New agent", |a| a.agents_new()), "claude prompt run"),
    alias(d("agents.mission", "Agents", "Mission Control", |a| crate::control::hooks::mission_control(a, None)), "agents overview runs"),
    alias(d("agents.review-last", "Agents", "Review the last agent run", |a| a.review_last_run()), "what changed"),
    alias(d("agents.staff", "Agents", "Refresh Aa staff status", |a| a.refresh_staff()), "atelier astra sessions"),
];

/// Every action, in the order the palette lists them without a query (after the recent ones).
/// Track hooks: multi-repo workspaces (A, decision 0013), the index (B, 0014) and Mission
/// Control (C, 0015) add their `ACTIONS` slices here when they land.
pub fn all() -> Vec<&'static Action> {
    let slices: &[&[Action]] = &[
        crate::control::ACTIONS,
        FILE,
        EDIT,
        NAVIGATE,
        CODE,
        TOOLS,
        GIT,
        AGENTS,
        // HOOK(track B, decision 0014): crate::index::ACTIONS,
        // HOOK(track C, decision 0015): crate::agents::mission::ACTIONS,
    ];
    slices.iter().flat_map(|s| s.iter()).collect()
}

// Used by `control::ACTIONS` (re-exported builders).
pub(crate) const fn act_do(id: &'static str, group: &'static str, name: &'static str, run: fn(&mut App)) -> Action {
    d(id, group, name, run)
}
pub(crate) const fn with_key(a: Action, key: &'static str) -> Action {
    keyed(a, key)
}
pub(crate) const fn with_alias(a: Action, words: &'static str) -> Action {
    alias(a, words)
}
pub(crate) const fn with_state(a: Action, f: fn(&App) -> String) -> Action {
    state(a, f)
}
