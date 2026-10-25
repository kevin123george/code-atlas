# Roadmap

Each milestone is one worktree (`~/Projects/code-atlas-wt/<task>`, branch `<task>`), merged into `main`.
Own new files freely; touch `app.rs` / `ui.rs` / `main.rs` only with small hooks, and rebase on `main` before merging.

| # | Milestone | Owns | Status |
|---|---|---|---|
| M1 | Core editing: tabs, undo/redo, selection, pairing, smart Enter/Home, line ops, find, go to line, mouse, clipboard | `doc.rs`, `app.rs`, `ui.rs` | done 2026-10-25 |
| M2 | Project view: file tree sidebar (Alt+1), fuzzy file finder (Ctrl+Shift+N, fallback Ctrl+P), find in files (Ctrl+Shift+F), recent files (Ctrl+E), open a folder (`catlas .`) | `src/project/` | done 2026-10-25 |
| M3 | Code smarts: tree-sitter highlighting (Rust, TS/JS, Java, Python, JSON, TOML, Markdown, Bash), LSP client (completion Ctrl+Space, go to definition Ctrl+B, hover Ctrl+Q→ F1, diagnostics in gutter, rename Shift+F6, quick fix Alt+Enter, format Ctrl+Alt+L), comment line Ctrl+/ | `src/syntax/`, `src/lsp/` | done 2026-10-25 (left: signature help, find usages, semantic tokens, M3 e2e scenarios, non-Rust servers untested) |
| M4 | Vim mode: normal/insert/visual, motions, operators, counts, `:w :q`, toggle in settings/command palette | `src/vim/` | done 2026-10-25 (toggle: `--vim`, `CATLAS_VIM=1`, Ctrl+Alt+V; settings/palette toggle comes with M7) |
| M5 | Git + terminal: changed-line gutter marks, diff view, commit panel (Ctrl+K), terminal pane (Alt+F12) | `src/dock.rs`, `src/git/`, `src/term/` | done 2026-10-25 (bottom dock; Ctrl+K / Alt+0 commit panel, Alt+F12 terminal; decision 0006) |
| M6 | Agents: panel (Alt+9) listing Claude Code sessions (local + Aa staff via `aa-staff.sh status`), start an agent with a prompt on the open project, stream its output, send follow-ups | `src/agents/` | done 2026-10-25 (runs started from the panel + Aa staff; local `claude agents` sessions not listed yet) |
| M7 | Command palette (Ctrl+Shift+A "Find Action"), settings file `~/.config/code-atlas/config.toml`, themes | `src/palette/`, `src/config.rs` | later |
