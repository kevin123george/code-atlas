# 0008 – Shortcut helper (key-hints)

Date: 2026-10-25 · Status: accepted (aa-app-dev, full auto mode; astra asked for "an easy shortcut helper,
like transparent, bottom right corner")

## Context
Code Atlas has many keys (IDEA, Vim, project view, LSP, dock, agents), several with fallbacks for terminals
without the kitty keyboard protocol. The status bar shows one hint line at a time; there was no way to see the
useful keys for what has focus right now.

## Decision
- **A floating panel in the bottom-right corner of the editor area** (`src/hints/`), 30–38 columns, at most 9
  rows (title + 8 keys). It sits inside the editor area, so it never covers the tree, the agents panel, the dock
  or the status bar. It is drawn after the text and before every popup (popups cover it).
- **One context at a time**, chosen in the order `App::handle` hands keys around: diff view, terminal, Commit
  panel (list / message), prompt, project popups (go to file, recent, find in files), agents panel (prompt /
  runs / transcript), project tree, completion, lists (pickers, quick fixes), Vim command line / NORMAL /
  INSERT / VISUAL (V-LINE in the title), editor with a selection, editor IDEA. 20 contexts.
- **The keys that really work:** each row has the kitty key and, where needed, a fallback. `main.rs` passes
  `enhanced` (kitty protocol on) as `hints.kitty`; without it the fallback shows (`Ctrl+P` instead of
  `Ctrl+Shift+N`, `Alt+F` for `Ctrl+Shift+F`, `Alt+Enter` for `Ctrl+Enter` commit).
- **"Glass" look, no fill:** the cells under the panel keep their characters and backgrounds, repainted in a
  new `theme::GHOST` (#2a3040, just above PAPER), modifiers and underline colours cleared. Text goes on top
  with a foreground only: keys `ACCENT`, descriptions `TEXT_2`, title `TEXT_2` bold, the toggle `MUTED`, a
  thin `BASELINE` left edge with a rounded top. Each text run gets one blank cell on both sides (and the column
  after the edge is blank): in the first capture, dim code touching the text read as noise.
- **Never over the cursor line:** if the editor cursor's screen row is inside the panel, it moves to the
  top-right corner; if it would be under that too (very short editor), it hides. It also hides when the editor
  area is too small (less than panel + 12 columns, or panel + 2 rows).
- **Toggle Alt+/** (also Alt+? and Alt+Shift+7, where `/` sits on a German keyboard). Every terminal sends it
  as `ESC /`, nothing else binds it (checked against all key tables; Ctrl+Alt+K was the other candidate and is
  IDEA's "commit and push", a word the editor avoids). It works in every context, the terminal included
  (readline's `M-/` is lost there; Ctrl+K and Alt+1/9/0/F12 are already taken from the shell the same way).
  macOS terminals need "Option as Meta", as for Alt+1. **On by default.**
- **Settings file:** `src/config.rs`, `settings.toml` in the config folder (`$CATLAS_CONFIG_DIR`, else
  `$XDG_CONFIG_HOME/code-atlas`, else `~/.config/code-atlas`), one key: `hints = true`. Written only when Alt+/
  is pressed (other keys in the file are kept, comments are lost); a missing, unreadable or broken file means
  the defaults. `catlas --no-hints` turns it off for one run without writing anything. M7 can grow this file.
- **Key Promoter tips** (IntelliJ's plugin, small): for 3 s the panel's first row shows "Tip: key  what" when
  the mouse did something with a key (clicking a tab → Alt+←/→, opening a file from the tree → go to file,
  clicking into the tree / agents panel / dock tabs → Alt+1 / Alt+9 / Alt+F12 / Alt+0, wheel in the editor →
  Ctrl+G) or when a fallback was used where the real key works (kitty on: Ctrl+P → Ctrl+Shift+N, Alt+F, Alt+Z,
  Alt+N, Alt+P). The same tip waits 30 s before it shows again; no tips while the helper is off.
- **Single source of truth:** `src/hints/table.rs` holds the rows per context as data, in a small label syntax
  (`Ctrl+Shift+N`, `Alt+←/→` alternatives, Vim sequences like `ciw`, `a-z` = type to filter). The binding test
  (`src/hints/tests.rs`, one test per context) parses every label, feeds each key into a fresh `App` in that
  context (scratch git project, one or more setups per context) and requires a change in the app's state
  **beyond what an unbound key (F11) does there** (the terminal scrolls to the bottom and the agents panel shows
  its hint for any key, so "anything changed" alone let a bogus row through in a mutation check). Rule: a new key
  binding gets a row in `table.rs` when it is among the useful few for its context.

## Consequences
- Rows that only some states can show (↑ needs a row above, redo needs an undo, Alt+↑ needs a previous file)
  are tested with extra setups; the commit panel's F5 was dropped from the table (no observable effect to test).
- The e2e harness now gives every scenario a scratch config folder (`$SCRATCH/bin/.catlas-config`, exported as
  `CATLAS_CONFIG_DIR`) and starts with `hints = false` there, so old screen checks aren't covered;
  `E2E_HINTS=1` gets the real default. Before this, e2e runs used the real `~/.config/code-atlas`.
- Unit tests never write the user's settings (`Hints::load` has no store under `cfg(test)`) and never touch the
  system clipboard (`set_system_clipboard` is a no-op under `cfg(test)`).
