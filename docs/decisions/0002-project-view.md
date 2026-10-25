# 0002 – Project view (M2): how the tree, finder and find in files work

Date: 2026-10-25 · Status: accepted (aa-app-dev, full auto mode given by astra)

## Context
M2 adds IDEA's project view to Code Atlas: the file tree (Alt+1), Go to File (Ctrl+Shift+N), Find in Files
(Ctrl+Shift+F) and Recent Files (Ctrl+E). astra asked for full auto mode, so these are the choices made.

## Decision
- **Arguments:** the first folder argument is the project root; without one the current folder is. Files given
  open as tabs. The sidebar shows (and has focus) when catlas is started on a folder or with no arguments;
  `catlas file.rs` alone starts with the sidebar hidden (Alt+1 shows it).
- **Ignore rules:** the `ignore` crate (ripgrep's walker) with `.gitignore` honoured **also outside git repos**
  (`require_git(false)`, same as `rg --no-require-git`), dotfiles shown (IDEA shows them), and `.git`, `target/`,
  `node_modules/`, `.DS_Store` always hidden. `.git` is hidden whether it's a folder or a file (worktrees).
- **Fuzzy finder:** our own fzf-style scorer (`src/project/fuzzy.rs`, small DP, no extra crate): word starts,
  file-name matches and runs score higher; shorter paths win ties. `name:42[:7]` jumps to a line, as in IDEA.
  An empty query lists recent files. The file list is indexed once in the background at start (up to 300 000).
- **Find in files:** plain substring search on a parallel walk in a background thread (no `grep-searcher`),
  **smart case** (lowercase query = ignore case), 150 ms debounce, at least 2 characters, files over 4 MB and
  binaries (NUL in the first 8 KB) skipped, stops at 2 000 hits. Hits of one file arrive together. Typing again
  drops (cancels) the old search. One row per occurrence. The query is remembered when the popup closes.
- **Fallback keys:** terminals without the kitty protocol can't send Ctrl+Shift+letter, so Ctrl+P = Go to File
  and Alt+F = Find in Files. Alt+F1 reveals the current file in the tree (IDEA "Select In", without the menu).
- **Recent files** live in memory for the session (opened files, most recent first); not saved to disk yet
  (that belongs with the settings file in M7).
- Opening a file that is already open switches to its tab; a fresh empty untitled tab is replaced, not kept.

## Consequences
- The tree re-reads a folder each time it is expanded, but doesn't watch the disk; collapse/expand refreshes.
- The finder index is built once per start, so files created later only show after a restart (or a future
  refresh key).
