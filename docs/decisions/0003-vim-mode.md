# 0003 – Vim mode (M4)

Date: 2026-10-25 · Status: accepted (aa-app-dev, full auto mode from astra; astra may revise)

## Context
Decision 0001 makes Vim an optional mode on top of the IDEA key map, "like IdeaVim". M4 had to pick how the two
key maps share keys, undo, the clipboard and the file commands.

## Decision
- **Off by default.** On with `--vim`, `CATLAS_VIM=1|true|yes|on`, or Ctrl+Alt+V at runtime (until M7 adds a
  settings file and command palette).
- **Ctrl/Alt chords stay IDEA's in every mode** (Ctrl+S, Ctrl+Q, Ctrl+F, Ctrl+P, Alt+arrows, Ctrl+D, Ctrl+V ...),
  like IdeaVim's "IDE" handler choice; Vim only keeps Ctrl+R (redo) and Ctrl+[ (Esc). So Vim's Ctrl+D/U/V/O are
  not available. Shift+arrows and mouse drags select like IDEA and enter visual mode.
- **Undo:** one Vim command, or one whole insert session, is one undo step (`Doc::begin_group`/`end_group`), not
  IDEA's typing runs. Arrow keys inside insert mode start a new step, as in Vim.
- **Insert mode still types through `Doc`** (bracket pairing, smart Enter), as in IdeaVim; Vim records the keys
  for `.` and counts.
- **One register**, the editor clipboard; only yanks also go to the system clipboard, so `x`/`dd` don't overwrite
  what was copied elsewhere.
- **`:q` closes the current tab** (IdeaVim does the same); on the last tab it quits. `:qa` quits everything.
  Unsaved work needs `!` (E37), mirroring the IDEA Ctrl+Q double-press guard.
- **Search and `:s` are plain text**, reusing `Doc::find` (smart case) for `/ ? n N * #`; `:s` is case-sensitive
  unless `i`. No regex engine dependency for now.
- Vim motions stop at the last content line (`Doc::content_lines`), not at IDEA's reachable empty line after a
  final line break.

## Consequences
- The Vim engine is self-contained in `src/vim/` and talks to `App` through `Effect`s; `App` needs only a few
  hooks (`key`, `vim_effect`, `after_ide`).
- Missing for later: named registers, marks, macros, block visual, regex.
