# 0001 – Stack, key map and scope

Date: 2026-10-25 · Status: accepted (astra)

## Context
astra wants a terminal editor for code "sort of like nvim, easy to use like IDEA".

## Decision
- **Rust** with ratatui/crossterm, ropey, tree-sitter and LSP: the base Helix and Zed use; one fast binary.
- **IntelliJ keys by default** (no modes: type straight away), with an **optional Vim mode** toggled at runtime.
- **Internal tool** named **Code Atlas** (binary `catlas`), like the other "… Atlas" tools; not public.
- v1 scope: core editing, project view, code smarts (LSP), git + terminal pane, and **agent management** (start
  and watch Claude Code agents, incl. the Aa staff, from inside the editor).
- Built by Aa staff agents in parallel, one module and one worktree each (`code-atlas-wt/<task>`).

## Consequences
- Some IDEA chords don't exist in plain terminals; we enable the kitty keyboard protocol and give fallbacks.
- Repo on Forgejo `atelier-astra/code-atlas`, added to `aa-sync.sh`.
