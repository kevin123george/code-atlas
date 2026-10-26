//! The shortcut helper's rows: the most useful keys per context, as data. **The one place hint
//! keys are written down**: a new key binding gets a row here (if it is among the useful few for
//! its context), and `tests.rs` feeds every row's key into a fresh `App` in that context and
//! checks that it does something, so this table can't drift from the real bindings.
//!
//! Key label syntax (what is shown is what the test presses):
//! - a chord: `Ctrl+Shift+N`, `Alt+F12`, `Ctrl+Alt+↑`, `Shift+Tab`; key names `↑ ↓ ← → PgUp PgDn
//!   Home End Enter Esc Tab Space Del Backspace F1..F12`, or one character;
//! - alternatives split by `/`: `Alt+←/→`, `Ctrl+C/X`, `F7/Shift+F7` (a bare second key takes the
//!   first one's modifiers; `Ctrl+/` is the slash key itself); `Ctrl+(Shift+)Z` = with and
//!   without Shift;
//! - anything else is typed key by key: Vim's `dd`, `ciw`, `:w` (the test ends a `:` command with
//!   Enter), `a-z` for "type to filter".

/// One row: the key (as a kitty-protocol terminal can send it), the key to show instead where the
/// terminal can't send it (no kitty keyboard protocol), and what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub legacy: Option<&'static str>,
    pub does: &'static str,
}

impl Hint {
    /// The key that really works in this terminal.
    pub fn shown(&self, kitty: bool) -> &'static str {
        if kitty {
            self.key
        } else {
            self.legacy.unwrap_or(self.key)
        }
    }
}

const fn h(key: &'static str, does: &'static str) -> Hint {
    Hint { key, legacy: None, does }
}

/// A key with a fallback for terminals without the kitty protocol.
const fn fb(key: &'static str, legacy: &'static str, does: &'static str) -> Hint {
    Hint { key, legacy: Some(legacy), does }
}

/// The toggle for the helper itself (shown in its title row). Alt+/ arrives the same in every
/// terminal (ESC /) and nothing else uses it; macOS terminals need "Option as Meta", as for Alt+1.
pub const TOGGLE: &str = "Alt+/";

/// What has the focus, as far as the helper is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctx {
    Idea,
    Selection,
    VimNormal,
    VimInsert,
    VimVisual,
    VimCommand,
    Tree,
    TreeSearch,
    GotoFile,
    Recent,
    FindInFiles,
    QuickSearch,
    Terminal,
    Commit,
    CommitMessage,
    Diff,
    AgentsPrompt,
    AgentsRuns,
    AgentsTranscript,
    Completion,
    List,
    Prompt,
    GitChanges,
    GitLog,
    GitCommit,
    GitBranches,
    GitConfirm,
    GitInput,
    Blame,
}

/// Every context (tests walk them all).
#[cfg(test)]
pub const ALL: [Ctx; 29] = [
    Ctx::Idea,
    Ctx::Selection,
    Ctx::VimNormal,
    Ctx::VimInsert,
    Ctx::VimVisual,
    Ctx::VimCommand,
    Ctx::Tree,
    Ctx::TreeSearch,
    Ctx::GotoFile,
    Ctx::Recent,
    Ctx::FindInFiles,
    Ctx::QuickSearch,
    Ctx::Terminal,
    Ctx::Commit,
    Ctx::CommitMessage,
    Ctx::Diff,
    Ctx::AgentsPrompt,
    Ctx::AgentsRuns,
    Ctx::AgentsTranscript,
    Ctx::Completion,
    Ctx::List,
    Ctx::Prompt,
    Ctx::GitChanges,
    Ctx::GitLog,
    Ctx::GitCommit,
    Ctx::GitBranches,
    Ctx::GitConfirm,
    Ctx::GitInput,
    Ctx::Blame,
];

const GOTO_FILE: Hint = fb("Ctrl+Shift+N", "Ctrl+P", "go to file");
const FIND_IN_FILES: Hint = fb("Ctrl+Shift+F", "Alt+F", "find in files");
/// Search Everywhere: Ctrl+T reaches every terminal (Alt+S / Alt+Space too where Option is Meta).
const QUICK: Hint = h("Ctrl+T", "search everywhere");
const COMMIT: Hint = fb("Ctrl+Enter", "Alt+Enter", "commit ticked");
const LEAVE_VIM: Hint = h("Ctrl+Alt+V", "IDEA keys (leave Vim)");
/// Without the kitty protocol Ctrl+Shift+Z arrives as Ctrl+Z; Alt+Z needs "Option as Meta" on a
/// Mac, where Option+Z (Ω) right after an undo also redoes (decision 0010). Cmd+Z / Cmd+Shift+Z
/// work too where the terminal passes them on.
const UNDO_REDO: Hint = fb("Ctrl+(Shift+)Z", "Ctrl+Z/Alt+Z", "undo / redo");
/// Completion (decision 0011): macOS takes Ctrl+Space for switching input sources, in every
/// terminal, so Ctrl+J is always shown next to it.
const COMPLETE: Hint = h("Ctrl+Space/J", "complete");
/// The Git view (decision 0012): Ctrl+L reaches every terminal; Alt+8 too where Option is Meta.
const GIT_VIEW: Hint = h("Ctrl+L", "git review / log");

/// The panel title for a context (some add a detail, see `super::title`).
pub fn title(c: Ctx) -> &'static str {
    match c {
        Ctx::Idea => "editor · IDEA",
        Ctx::Selection => "editor · selection",
        Ctx::VimNormal => "vim · NORMAL",
        Ctx::VimInsert => "vim · INSERT",
        Ctx::VimVisual => "vim · VISUAL",
        Ctx::VimCommand => "vim · command line",
        Ctx::Tree => "project tree",
        Ctx::TreeSearch => "tree · speed search",
        Ctx::GotoFile => "go to file",
        Ctx::Recent => "recent files",
        Ctx::FindInFiles => "find in files",
        Ctx::QuickSearch => "search everywhere",
        Ctx::Terminal => "terminal",
        Ctx::Commit => "commit",
        Ctx::CommitMessage => "commit · message",
        Ctx::Diff => "diff view",
        Ctx::AgentsPrompt => "agents · prompt",
        Ctx::AgentsRuns => "agents · runs",
        Ctx::AgentsTranscript => "agents · transcript",
        Ctx::Completion => "completion",
        Ctx::List => "list",
        Ctx::Prompt => "prompt",
        Ctx::GitChanges => "git · review",
        Ctx::GitLog => "git · log",
        Ctx::GitCommit => "git · commit",
        Ctx::GitBranches => "git · branches",
        Ctx::GitConfirm => "git · confirm",
        Ctx::GitInput => "git · input",
        Ctx::Blame => "editor · blame",
    }
}

/// At most 8 rows per context (the panel is 9 rows with its title).
pub fn rows(c: Ctx) -> &'static [Hint] {
    match c {
        Ctx::Idea => const { &[
            GOTO_FILE,
            QUICK,
            COMPLETE,
            GIT_VIEW,
            h("Ctrl+F/G", "find / go to line"),
            h("Ctrl+B", "go to definition"),
            h("Alt+1/F12", "project / terminal"),
            UNDO_REDO,
        ] },
        Ctx::Selection => const { &[
            h("Ctrl+D", "duplicate"),
            h("Ctrl+/", "comment lines"),
            h("Tab/Shift+Tab", "indent / unindent"),
            h("(", "wrap: ( [ { \" '"),
            h("Ctrl+C/X", "copy / cut"),
            h("Alt+Shift+↑/↓", "move lines"),
            FIND_IN_FILES,
            h("Esc", "deselect"),
        ] },
        Ctx::VimNormal => const { &[
            h("i/a/o", "insert"),
            h("v/V", "visual / lines"),
            h("dd/yy/p", "delete / yank / put"),
            h("ciw", "change word"),
            h("/", "search · n N next"),
            h("u/Ctrl+R", "undo / redo"),
            h(":w/:q", "save / close"),
            LEAVE_VIM,
        ] },
        Ctx::VimInsert => const { &[
            h("Esc", "normal mode"),
            COMPLETE,
            h("Ctrl+S", "save"),
            GOTO_FILE,
            h("Alt+F12", "terminal"),
            LEAVE_VIM,
        ] },
        Ctx::VimVisual => const { &[
            h("d/y/c", "delete / yank / change"),
            h(">/<", "indent / unindent"),
            h("iw/i(", "select word / ( )"),
            h("o", "other end"),
            h("~", "toggle case"),
            h("Esc", "normal mode"),
            LEAVE_VIM,
        ] },
        Ctx::VimCommand => const { &[h("Enter", "run"), h("Esc", "cancel"), h("Backspace", "delete")] },
        Ctx::Tree => const { &[
            h("↑/↓", "move"),
            h("→/←", "unfold / fold"),
            h("Enter", "open"),
            h("a-z", "speed search"),
            h("Ctrl+T", "search in folder"),
            h("Esc", "editor"),
            h("Alt+1", "hide tree"),
            GOTO_FILE,
        ] },
        Ctx::TreeSearch => const { &[
            h("a-z", "narrow"),
            h("↑/↓", "prev / next match"),
            h("Enter", "open"),
            h("Backspace", "delete a letter"),
            h("Esc", "clear search"),
        ] },
        Ctx::GotoFile => const { &[
            h("a-z", "filter · name:42"),
            h("↑/↓", "select"),
            h("Enter", "open"),
            h("Ctrl+U", "clear"),
            h("Ctrl+E", "recent files"),
            h("Esc", "close"),
        ] },
        Ctx::QuickSearch => const { &[
            h("a-z", "names + text · :42"),
            h("↑/↓", "pick · tree follows"),
            h("Enter", "open"),
            h("Tab/Shift+Tab", "all / files / text"),
            h("Alt+F1", "show in tree"),
            h("Ctrl+U", "clear"),
            h("Esc", "close"),
        ] },
        Ctx::Recent => const { &[h("a-z", "filter"), h("↑/↓", "select"), h("Enter", "open"), GOTO_FILE, h("Esc", "close")] },
        Ctx::FindInFiles => const { &[
            h("a-z", "search (smart case)"),
            h("↑/↓", "select"),
            h("Enter", "open at match"),
            h("Ctrl+U", "clear"),
            h("Esc", "close"),
        ] },
        Ctx::Terminal => const { &[
            h("Ctrl+Alt+↑", "back to editor"),
            h("Alt+F12", "hide terminal"),
            h("Shift+Esc", "hide terminal"),
            h("Shift+PgUp/PgDn", "scroll back"),
            h("Alt+0", "commit panel"),
            h("Alt+1/9", "project / agents"),
            h("Ctrl+Shift+↑/↓", "taller / shorter"),
        ] },
        Ctx::Commit => const { &[
            h("Space", "tick file"),
            h("Ctrl+A", "tick all / none"),
            h("Enter", "diff"),
            h("Tab", "message"),
            COMMIT,
            h("Esc", "editor"),
            h("Alt+0", "hide"),
        ] },
        Ctx::CommitMessage => const { &[
            h("a-z", "type the message"),
            h("Enter", "new line"),
            COMMIT,
            h("Ctrl+U", "clear line"),
            h("Tab", "file list"),
            h("Esc", "file list"),
        ] },
        Ctx::Diff => const { &[
            h("↑/↓", "scroll"),
            h("PgUp/PgDn", "page"),
            h("F7/Shift+F7", "next / prev change"),
            h("Alt+↑/↓", "prev / next file"),
            h("F4", "open in editor"),
            h("Esc", "back to commit"),
        ] },
        Ctx::AgentsPrompt => const { &[
            h("Enter", "send"),
            h("Alt+Enter", "new line"),
            h("Alt+M", "permission mode"),
            h("Alt+I", "add current file"),
            h("Tab/Shift+Tab", "runs / transcript"),
            h("Ctrl+N", "new agent"),
            h("Ctrl+U", "clear"),
            h("Esc", "editor"),
        ] },
        Ctx::AgentsRuns => const { &[
            h("↑/↓", "pick a run"),
            h("Ctrl+D", "what it changed (review)"),
            h("Enter", "follow-up"),
            h("Ctrl+C", "stop"),
            h("Del", "remove from list"),
            h("Tab", "transcript"),
            h("Esc", "editor"),
        ] },
        Ctx::AgentsTranscript => const { &[
            h("↑/↓", "scroll"),
            h("PgUp/PgDn", "page"),
            h("End", "follow the end"),
            h("Ctrl+D", "what this run changed"),
            h("Tab", "prompt"),
            h("Esc", "editor"),
        ] },
        Ctx::Completion => const { &[
            h("↑/↓", "choose"),
            h("Enter", "insert"),
            h("Tab", "replace word"),
            h("a-z", "filter"),
            h("Esc", "close"),
        ] },
        Ctx::List => const { &[h("a-z", "filter"), h("↑/↓", "select"), h("Enter", "open"), h("Esc", "close")] },
        Ctx::Prompt => const { &[h("Enter", "confirm"), h("Esc", "cancel"), h("Backspace", "delete")] },
        Ctx::GitChanges => const { &[
            h("F7/Shift+F7", "next / prev change"),
            h("Tab/Shift+Tab", "next / prev file"),
            h("s/S", "stage hunk / file"),
            h("r/R", "revert hunk / file"),
            h("u", "undo the revert"),
            h("v", "side by side"),
            h("a", "review an agent run"),
            h("Enter", "open in editor"),
        ] },
        Ctx::GitLog => const { &[
            h("↑/↓", "select"),
            h("Enter", "commit details"),
            h("/", "filter"),
            h("h", "this file's history"),
            h("y", "copy hash"),
            h("B", "branches"),
            h("P/U/F", "push / pull / fetch"),
            h("1", "changes"),
        ] },
        Ctx::GitCommit => const { &[
            h("F7/Shift+F7", "next / prev change"),
            h("Tab/Shift+Tab", "next / prev file"),
            h("v", "side by side"),
            h("Enter", "open in editor"),
            h("y", "copy hash"),
            h("Esc", "back to the log"),
        ] },
        Ctx::GitBranches => const { &[
            h("↑/↓", "select"),
            h("Enter", "switch (asks)"),
            h("n", "new branch"),
            h("r", "rename"),
            h("d", "delete merged (asks)"),
            h("Esc", "close"),
        ] },
        Ctx::GitConfirm => const { &[h("y/Enter", "yes, do it"), h("n/Esc", "no")] },
        Ctx::GitInput => const { &[h("a-z", "type"), h("Enter", "apply"), h("Backspace", "delete"), h("Esc", "done")] },
        Ctx::Blame => const { &[
            h("Ctrl+L", "this line's commit"),
            GOTO_FILE,
            QUICK,
            COMPLETE,
            h("Ctrl+F/G", "find / go to line"),
            UNDO_REDO,
        ] },
    }
}

/// Key Promoter tips for fallbacks: when the fallback (`.legacy`) is used in a terminal that can
/// send the real key, the panel says so.
pub const FALLBACKS: &[Hint] = &[
    GOTO_FILE,
    FIND_IN_FILES,
    fb("Ctrl+Shift+Z", "Alt+Z", "redo"),
    fb("Ctrl+Alt+Shift+N", "Alt+N", "go to symbol"),
    fb("Shift+F3", "Alt+P", "previous match"),
    fb("Ctrl+Space", "Ctrl+J", "completion"),
];

/// Fallback keys whose "real" key macOS keeps for itself (Ctrl+Space switches input sources):
/// no tip for them on a Mac, the fallback is the key there.
pub const MAC_TAKEN: &[&str] = &["Ctrl+Space"];

/// Key Promoter tips for mouse actions that have a key.
pub const TAB_SWITCH: Hint = h("Alt+←/→", "switch tab");
pub const TREE_OPEN: Hint = GOTO_FILE;
pub const TREE_FOCUS: Hint = h("Alt+1", "project tree");
pub const SCROLL: Hint = h("Ctrl+G", "go to line");
pub const DOCK_TERMINAL: Hint = h("Alt+F12", "terminal");
pub const DOCK_COMMIT: Hint = h("Alt+0", "commit panel");
pub const AGENTS_FOCUS: Hint = h("Alt+9", "agents panel");
