# 0016 Control Center, Find Action, settings and themes (track D, roadmap M7)

Date: 2026-10-31 · Status: accepted · Owner request: "something like a control center as well"
(next to agents, huge code bases and many repos at once).

## Decisions

**Control Center** (`src/control/`): a full-screen view (status bar stays) to see and steer the
whole editor. Six tiles, keyboard first:
- **Workspace**: one row per root/repo (track A's roots): `⎇ branch`, ↑ahead ↓behind, changed, new,
  conflicts; Enter opens the Git view on that repo.
- **Agents**: running / done today, cost today (runs updated since local midnight; the zone from
  `date +%z`), "needs attention" (failed runs, refused tools, files changed on disk), this project's
  runs; Enter → Mission Control (track C) via `hooks::mission_control`; Aa staff from the existing
  `aa-staff.sh status` fetch (off the UI thread, 60 s cache, never from unit tests).
- **Language servers**: every configured language: running (pid, memory) / starting / crashed /
  ready (installed) / missing / no server, from the live `Lsp` plus a background PATH probe (the
  `--health` lookup). Enter or `r` restarts a server (`lsp::status::lsp_restart`: kill, forget it
  crashed, its files open again on the next sync).
- **Index**: the in-memory file list now; the persistent index of track B plugs into
  `hooks::index_status`.
- **Editor**: errors / warnings, unsaved files (Enter saves all, a file row switches to it), tabs,
  memory and CPU of catlas and its children, grouped by the nearest process catlas knows (language
  servers, terminal, agents, other) from one `ps -A -o pid=,ppid=,rss=,pcpu=,comm=` every 2 s on a
  thread, with a CPU sparkline.
- **Settings**: the toggles (theme, Vim, shortcut helper, autocomplete) change live and are saved.

Layout: 3 columns from 170 columns, 2 from 110, else one column that scrolls to the selected
tile (reads at 80). A title bar, a strip of status chips, a key legend. Drawing pieces live in
`control/widgets.rs` (tile, row, section, chip, keys, sparkline, popup) so they can be restyled in
one place (track E, decision 0017).

**Keys.** Control Center: **Alt+O** (Option+O with "Option as Meta"); where Option isn't Meta, macOS
types `ø`, which opens it too while the editor text has the keys (typed normally in prompts and
panels). Also from Find Action. Inside: Tab / Shift+Tab (←/→) tiles, ↑↓ rows (crossing tiles),
Enter / Space act, `r` restart / refresh, F5 refresh, `m` agents, `1` / `2` overview / settings,
`e` / `l` open settings.toml / languages.toml, Ctrl+R find action, Esc close.
Find Action: **Ctrl+Shift+A** (IDEA; kitty protocol) and **Ctrl+R** in every terminal
(Terminal.app sends Ctrl+Shift+A as Ctrl+A). Why Ctrl+R: the only Ctrl letter left — Ctrl+O went to
track A's Repos popup, the rest are taken; IDEA's Replace isn't built. Exceptions: the terminal pane
(the shell's history search) and Vim normal / visual mode in the editor (redo; Ctrl+Shift+A or Alt+O
still work there).

**Find Action** (`src/palette/`): every action from the registry, fuzzy (the project scorer, on the
name, then group + alias words; multi-word queries match in any order), recently used first
(`recent-actions` in the config folder), the key on the right (the kitty or legacy one for this
terminal), live values for toggles. Running one does what its key does: `Run::Keys` presses the
key through `App::handle`; `Run::Editor` gives the editor the focus first; `Run::Do` for things
without a key or that also save. Registry = `palette::actions::all()`: a list of per-feature
`ACTIONS` slices; a new feature adds its slice there. A test checks every chord the shortcut
helper shows in the editor contexts is a registered action.

**Settings** (`settings.toml`): schema in `control::settings::FIELDS` (key, label, type, help);
`check` reports syntax errors with the line, wrong types, unknown values and keys (shown on the
settings screen, which also previews the file from 140 columns). Writing changes only that key's
line (`config::set_key_in`: comment and its column kept; a new key goes before the first table;
falls back to a plain rewrite only if the line edit wouldn't parse). Edits saved elsewhere apply
live (one `stat` every 2 s). New keys: `vim` (start in Vim mode), `theme`.

**Themes** (`control/theme.rs`): a registry of named palettes (`PALETTES`): Ledger Terminal dark
(default), Ledger Terminal light (the Cashflow Atlas light tokens, a few darkened for contrast) and
High contrast. Every module keeps drawing with the dark `ui::theme` tokens; `theme::apply` recolours
the finished frame (exact counterpart per token in `TOKENS`, a fallback transform for other colours
such as terminal programs'). Per syntax capture overrides go through `Palette::syntax`
(`token_style` emits a marker colour that `apply` swaps). Tests: every palette maps every token
once; text / accent / series contrast (WCAG) per palette. Track E adds tokyonight / catppuccin as
more `Palette`s.

## Hooks for the parallel tracks (`src/control/hooks.rs`)
- Track A (workspaces, 0013) — **wired**: `roots` = the workspace roots, `repo` uses A's background
  status (falls back to its own `git status`), `git_view_on` switches the current repo first, the
  Repos popup (Ctrl+O) is in the action registry.
- Track B (index, 0014) — **pending**: `index_status().persistent`, plus its `ACTIONS` in
  `palette::actions::all()` (marked `HOOK(track B)`).
- Track C (Mission Control, 0015) — see the current state in AGENTS.md: `mission_control` (marked
  `HOOK(track C)`) opens the agents panel on the run until it points at Mission Control.
- Track E (look, 0017): new palettes in `PALETTES`; restyle `control/widgets.rs`.

## Not done
Mouse beyond select / act / wheel; per-root ahead/behind upstream names with track A's status;
editing non-bool settings by typing; theme previews; a settings key for the helper's position.
