# 0006 – Git and terminal (M5)

Date: 2026-10-25 · Status: accepted (made by aa-app-dev in full auto mode, astra can revisit)

## Context
M5 adds a terminal pane, changed-line gutter marks, a diff view and a commit panel, built next to M3 (LSP),
M4 (Vim) and M6 (agents panel, right side) in parallel.

## Decision
- **Bottom dock** (`src/dock.rs`): one tool-window area under the editor, full width (under the sidebar too, so
  the terminal gets every column), 12 rows by default, Ctrl+Shift+↑/↓ resizes it while it has the focus. Tabs:
  Terminal, Commit; Problems etc. can join later. The right side stays M6's.
- **Terminal**: `portable-pty` + `vt100` 0.15 (0.16 needs a newer `unicode-width` than ratatui 0.29 pins).
  `$SHELL -l` in the project root, `TERM=xterm-256color`. Every key goes to the shell, Esc too. To leave:
  **Ctrl+Alt+↑** (editor, dock stays) or **Alt+F12** (hide); Shift+Esc hides as in IDEA. Alt+1 / Alt+9 still
  switch tool windows. We answer the cursor-position and device-attribute queries that `vt100` doesn't. One
  terminal for now; tabs of terminals later. Shell output wakes a redraw through `dock_tick` (the main loop polls every 30 ms since M3).
- **Git = the `git` CLI** (not `gix`): status every 2 s (`--porcelain=v1 -z --branch --untracked-files=all`,
  `GIT_OPTIONAL_LOCKS=0`), HEAD text per open file via `git show HEAD:path`, re-read when HEAD moves. Gutter
  marks come from an in-memory line diff (`similar`, 300 ms timeout) of HEAD vs the unsaved buffer on a
  thread, so they follow every edit, not only saves. Marks: green bar added, blue bar changed, red ▁/▔ deleted.
- **Commit panel** (Alt+0 toggles, Ctrl+K opens it with the cursor in the message): changed tracked files are
  ticked by default, unversioned ones aren't (as in IDEA). Commit = `git add -A -- <ticked>` then
  `git commit -m <msg> -- <ticked>` (only those paths, whatever else is staged). Ctrl+Enter commits; Alt+Enter and
  Ctrl+J are fallbacks for terminals that can't send Ctrl+Enter. Git hooks run as usual (no `--no-verify`).
  **Never push, never force, no amend, no reset** from the editor.
- **Diff view**: unified (old/new line numbers, coloured rows), over the editor area; F7 / Shift+F7 hunks,
  Alt+↑/↓ previous/next file, F4 or Enter opens the source at that line, Esc back to the panel. Side by side later.
- Tree colours: modified blue, new/unversioned green, folders holding changes blue. Ignored files aren't shown
  in the tree at all (M2 walker), so there is no "ignored" colour to see.

## Consequences
- Keys typed in a shell's first second may be dropped by the shell's own startup (zsh plugins).
- No mouse forwarding to programs in the terminal (htop, vim mouse) and no copying text out of the terminal yet.
