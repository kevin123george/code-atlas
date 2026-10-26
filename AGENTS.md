# Code Atlas

> Shared rules for all projects: `~/Projects/AGENTS.md` and `~/Projects/atelier-astra/AGENTS.md` (worktrees,
> decision records, verification, secrets). Read this repo's `docs/decisions/` and `docs/ROADMAP.md` first.

## What it is
An internal Atelier Astra tool: a terminal code editor that feels like IntelliJ IDEA (type straight away, IDEA
shortcuts) with an optional Vim mode, project view, code smarts over LSP, git, a terminal pane, and a panel to
run and watch Claude agents. Binary: `catlas`.
`catlas <dir>` opens a project (no arguments = the current folder); `catlas <dir> file …` also opens files.

## Where it runs
Locally in a terminal on the Mac and the laptop (`catlas [--vim] [dir] [file ...]`). No server. Best in a terminal with the
kitty keyboard protocol (Ghostty, kitty, WezTerm, iTerm2 with "CSI u" on): it is needed for Ctrl+Shift combos.

## Stack
Rust 2021 (Mac: `~/.cargo/bin`, not on PATH; laptop: pacman `rust`). `ratatui` 0.29 + `crossterm` 0.28 (screen,
input), `ropey` (text buffer), `unicode-width`, `ignore` (ripgrep's gitignore-aware walker, project view),
`tree-sitter` 0.27 + grammar crates (rust, typescript/tsx, javascript, java, kotlin-sg, python, go, c, cpp, bash,
lua, json, yaml, toml-ng, html, css, md, containerfile, sequel, xml (+ DTD); their bundled highlight queries) for highlighting,
`lsp-types` 0.97 + `serde_json` with an own JSON-RPC client (LSP), `toml` (`languages.toml`).
`serde` + `serde_json` also for the agents panel (stream-json, `agents.json`); agents run the `claude` CLI.
`portable-pty` 0.9 + `vt100` 0.15 (terminal pane; vt100 0.16 conflicts with ratatui's pinned `unicode-width`),
`similar` (line diffs) and the `git` CLI (git).
Language servers: which one runs for which language comes from `languages.toml` (see **Language servers** below);
they're found on PATH plus `~/.cargo/bin`, `~/.local/bin`, `~/.bun/bin`, `/opt/homebrew/bin`, `/usr/local/bin`.
Installed on the Mac (2026-10-25, decision 0007): `npm i -g typescript typescript-language-server pyright
bash-language-server yaml-language-server vscode-langservers-extracted dockerfile-language-server-nodejs`
(Homebrew node, global prefix `/opt/homebrew`), `brew install jdtls gopls lua-language-server shellcheck ruff`
(jdtls pulled in brew's `openjdk`; the Mac also has JDK 17/21/23/24/25), `rustup component add rust-analyzer rustfmt`;
clangd comes with the Xcode command line tools. Not installed: kotlin-language-server, marksman, taplo,
basedpyright, pylsp, lemminx (XML; no Homebrew formula: native binary `lemminx-osx-aarch_64.zip` from the
vscode-xml GitHub releases → `~/.local/bin/lemminx`, or the Maven Central uber jar behind a `java -jar`
wrapper of that name, decision 0010). `catlas --health` shows what is found. Missing servers are skipped with a status message.

## Language servers
`catlas --health [dir]` lists per language: highlighting, the configured servers, which one is used, its path and
version. Configuration = three layers, later ones win **field by field**: built-in `src/lsp/languages.toml`
(compiled in), `~/.config/code-atlas/languages.toml` (or `$CATLAS_CONFIG_DIR`, `$XDG_CONFIG_HOME/code-atlas`), and
`<project>/.code-atlas/languages.toml`. A commented example: `docs/languages.example.toml`.
```toml
[language.python]                 # a catlas grammar name (rust, typescript, tsx, javascript, java, kotlin,
servers = ["pyright", "pylsp"]    # python, go, c, cpp, bash, lua, json, yaml, toml, html, css, markdown,
also = ["ruff"]                   # dockerfile, sql, xml, dtd). servers: tried in order, the first found (and starting)
roots = ["pyproject.toml"]        # is the main one; also: extra servers next to it (diagnostics + quick fixes)
extensions = ["py", "pyi"]        # extensions, file names ("Dockerfile") or globs ("Dockerfile.*")

[language-server.pyright]
command = "pyright-langserver"    # name on PATH, or a path (~/ ok)
args = ["--stdio"]
initialization-options = { }      # sent in initialize; merged deeply with the built-in one
settings = { python = { analysis = { typeCheckingMode = "strict" } } }   # answers workspace/configuration
version-args = []                 # how --health asks for a version; [] = from the install path only
```
Strings in `initialization-options`/`settings` may use `${root}`, `${home}`, `${server-node-modules}`. A broken file
is ignored as a whole and shown as ERROR in `--health`. Built-in picks (decision 0007): TS/JS
typescript-language-server, then TypeScript 7's own `tsc --lsp --stdio` (`typescript-native`); Python pyright →
basedpyright → pylsp, plus ruff; Java jdtls; Kotlin kotlin-language-server; Go gopls; C/C++ clangd; Bash
bash-language-server (uses shellcheck); Lua lua-language-server; JSON/HTML/CSS vscode-*-language-server; YAML
yaml-language-server; TOML taplo; Markdown marksman; Dockerfile docker-langserver; XML/DTD lemminx; SQL
highlighting only.

## Layout
- `src/main.rs` – terminal setup/restore, event loop.
- `src/doc.rs` – one open file: rope, cursor/selection (char offsets), undo/redo snapshots, all edit commands, unit tests.
- `src/app.rs` – tabs, clipboard, prompts (find, go to line, save as), IDEA key map, mouse.
- `src/ui.rs` – drawing (tab bar, gutter, text, status bar) and the `theme` colours (Ledger Terminal dark tokens).
- `src/project/` – project view (M2): `mod.rs` state, keys, mouse, `App::open_target` / `tick` glue; `tree.rs`
  sidebar tree (flat row list, lazy listing, `walker()` = the ignore rules, speed search `speed_*`); `fuzzy.rs` file-name scorer;
  `quick.rs` Search Everywhere (tabs, sections, scope, tree follow + snapshot, speed-search glue);
  `search.rs` background file index + find in files (threads, channels, cancel on drop); `ui.rs` sidebar + popups.
  Hooks elsewhere: `App.project`, `project_key`/`project_mouse` calls in `App::handle`, `draw_sidebar`/`draw_popup`
  in `ui::draw`, `app.tick()` in the main loop.
- `src/vim/` – optional Vim mode (M4): `mod.rs` modes, key parser (counts, operators, `.` record/replay),
  operators, visual mode, the `Effect`s it hands back to `App`, and the IDEA-side sync (`wants`, `before_ide`,
  `after_ide`, `fix`, `leave`); `motion.rs` motions + text objects (pure functions over a `Doc`); `ex.rs` the
  `:` / `/` / `?` command line; `tests.rs` key-sequence tests (`check("ab|c", "dw", "a|b")`, `|` = cursor).
  Hooks elsewhere: `App.vim` + `App::key` (Vim first, then `idea_key`), `App::vim_effect`, `after_ide` calls after
  prompts/mouse/paste/project keys; `ui::block_cursor` + mode badge, showcmd and command line in the status bar;
  `Doc::begin_group`/`end_group`/`replace`/`indent`/`set_want_col`/`content_lines` helpers in `doc.rs`.
- `src/syntax/` – code smarts without a server (M3): `mod.rs` `Lang` (detection by name/extension/`#!`, status-bar
  name, comment token, LSP id), capture name → `Token`, per-doc `Syntax` (parser + tree, incremental reparse,
  visible-lines cache); `comment.rs` Ctrl+/ toggling. Colours: `ui::theme::token_style`.
- `src/lsp/` – LSP client (M3, extended by lsp-more): `mod.rs` `Lsp` state + `impl App` (sync to the main + `also`
  servers, requests, responses, keys, pull + push diagnostics, snippet sessions, pickers, signature help, F2);
  `config.rs` `languages.toml` layers, merging, file → language, server selection, `${var}` expansion;
  `languages.toml` the built-in table; `health.rs` `catlas --health`; `server.rs` project root, PATH lookup,
  process + threads, `CATLAS_LSP_LOG`; `jsonrpc.rs` framing; `text.rs` UTF-16 positions, file URIs, applying
  `TextEdit`s; `snippet.rs` LSP snippet parser (tab stops); `auto.rs` completion as you type (debounce,
  trigger characters, stale answers, Ctrl+J, merging words, decision 0011); `words.rs` word completion from the
  open buffers; `popup.rs` completion / hover / quick-fix popups,
  signature line, centred pickers (usages, structure, symbols, implementations, problems). Hooks elsewhere: `Doc.syntax`, `Doc::rev`/`edit_with`/
  `set_path`/`line_span` (pub), `App.lsp`, `smart_key`/`smart_after_key` in `App::handle`, `PromptKind::Rename`,
  `lsp_did_save` in `App::save`, Ctrl+/ in `App::key`, `app.lsp_tick()` in the main loop (30 ms poll, draws only
  when something changed or every 250 ms), highlighting/diagnostics/language and `app.draw_lsp_popups` in `ui.rs`,
  `lsp::config::init(root)` in `App::new` (before files open: detection reads it), `--health` in `main.rs`.
- `src/agents/` – agents panel (M6, right side, Alt+9): `mod.rs` `Agents` state, keys, mouse, persistence
  (`agents.json`), reload-on-change and the `impl App` glue (`agents_key`/`agents_mouse`/`agents_paste`/
  `agents_tick`, `file_context`); `run.rs` one run: state machine (Running → Done/Failed/Stopped), transcript
  `Entry`s, `claude_args`, the child process (own process group, reader threads, SIGTERM → SIGKILL);
  `stream.rs` stream-json line parser + one-line tool summaries; `transcript.rs` entries → wrapped styled lines;
  `input.rs` multi-line prompt box; `staff.rs` `aa-staff.sh status` fetch + parse; `cli.rs` `claude --help`
  probe (permission modes, `--agent`), agent types, finding `claude`; `ui.rs` the panel; `testdata/*.jsonl`
  recorded real runs. Hooks elsewhere: `App.agents`, `agents_key` before `project_key` (so Vim never sees panel
  keys), `agents_mouse`/`agents_paste` first in `App::handle`, `draw_panel` in `ui::draw` (takes the right
  45 columns before the sidebar), `app.agents_tick()` in the main loop, `Doc::reload_text` in `doc.rs`.
- `src/dock.rs` – bottom dock (M5): tabs Terminal / Commit, focus, layout (`split_rows`), keys, mouse, paste,
  `dock_tick` glue (returns "redraw"), drawing the tab row and bodies.
- `src/term/` – terminal pane (M5): `mod.rs` PTY + reader thread + `vt100` parser, query replies, resize,
  scrollback; `keys.rs` key → xterm bytes and query detection; `ui.rs` screen → buffer (ANSI palette in theme colours).
- `src/git/` – git (M5): `mod.rs` state + background jobs (status, HEAD texts, gutter diffs, commit, diff view);
  `status.rs` porcelain parser; `diff.rs` gutter marks + unified rows (`similar`); `ui.rs` gutter marks, Commit
  panel, diff view, branch label, tree colours.
  Hooks elsewhere: `App.dock`; `dock_key`/`dock_paste`/`dock_mouse` first in `App::handle` (before the agents panel's); `redraw |= app.dock_tick()` in the
  main loop; `dock::split`/`draw`/`draw_over_editor`, `git::ui::draw_mark` (in `draw_text`),
  `draw_branch` (status bar) and `!app.dock.has_focus()` (cursor, `block_cursor`) in `ui.rs`; `tree_color` in
  `project/ui.rs`.
- `src/hints/` – shortcut helper (key-hints, decision 0008): `mod.rs` `Hints` state (on, kitty, tip), `context()`
  (what has focus → one of 22 `Ctx`), the key-label parser, Key Promoter (`hints_key`, `hints_mouse`); `table.rs`
  **the hint rows per context, as data** (+ fallbacks and tip keys); `ui.rs` placement (bottom right, top right
  when the cursor line is under it) and the glass drawing; `tests.rs` the binding check and drawing tests.
  Hooks elsewhere: `App.hints`, `hints_key` first in `App::handle`, `hints_before`/`hints_mouse` around the mouse
  handlers, `hints::ui::draw` in `ui::draw` (after the editor, before popups), `theme::GHOST`, `--no-hints` and
  `app.hints.kitty = enhanced` in `main.rs`; `Terminal::feed` (tests only).
- `src/config.rs` – `settings.toml` (`hints = true`, `autocomplete = true`) in the config folder: load (defaults on any
  problem, field by field), save one key.
- `src/mackeys.rs` – macOS keys terminals mangle (decision 0010): Cmd chords with SUPER → Ctrl twins
  (editor only), `Ω`/`¸` (Option+Z without Option as Meta) → redo right after an undo. Hook: `App::handle`
  (editor branch) and `App.undo_chain`, set by the undo/redo arms in `idea_key`.
- `docs/ROADMAP.md` – milestones and who owns which files. `docs/decisions/` – project decisions.

## Run & test
```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test                 # unit tests (doc.rs edit logic, project tree/fuzzy/search, Vim engine, syntax, comments, LSP)
cargo run -- some/file.rs  # try it
cargo build --release      # target/release/catlas
```
End-to-end tests drive the real binary in tmux (bash + tmux + perl, nothing else):
```sh
tests/e2e/run.sh              # builds release, runs every scenario, prints a pass/fail table (~10 s)
tests/e2e/run.sh pair undo    # only scenarios whose name contains "pair" or "undo"; --list lists them
E2E_NO_BUILD=1 CATLAS=path tests/e2e/run.sh   # test another binary; E2E_KEEP=1 keeps scratch dirs
```
- Scenarios are `t_<name>` functions in `tests/e2e/scenarios/*.sh`; helpers live in `tests/e2e/run.sh`:
  `start <cols> <rows> [file…]`, `typ <text>`, `key <tmux key names…>`, `raw <hex bytes>`, `wait_for`,
  `wait_gone`, `status "Ln 3, Col 2 "`, `wait_line <n> <text>`, `cursor_at <x> <y>`, `save`,
  `assert_file <path> $'…'`, `assert_clipboard`, `assert_exited`, `note` (extra column, e.g. timings).
- `start` points `CATLAS_CONFIG_DIR` at `$CFG` (`$SCRATCH/bin/.catlas-config`), never the real `~/.config`, and
  writes `hints = false` there so the shortcut helper doesn't cover screen checks; `E2E_HINTS=1` (in the
  scenario, before `start`) gives the real default (on). See `scenarios/hints.sh`.
- Each scenario gets its own scratch dir under `$TMPDIR` (cwd of the editor) and session on a private tmux
  socket (`-L catlas-e2e-<pid>`), so your own tmux is never touched. A failing scenario saves its screen to
  `tests/e2e/out/<scenario>.txt` (gitignored). A fake `pbcopy` on PATH catches Ctrl+C/X, so the real
  clipboard is never overwritten.
- New feature (M2+): add a `scenarios/<feature>.sh` with its keys; keep using the fallbacks below in tmux.
- **tmux can't send** Ctrl+Shift+letter (tmux 3.5a sends Ctrl+Shift+Z as plain Ctrl+Z, even with
  `extended-keys always`) and its Shift+F3 (`ESC[1;2R`) is swallowed by crossterm as a cursor-position
  report. Test those with `raw` and the bytes a kitty-protocol terminal sends (`1b 5b 31 32 32 3b 36 75` =
  Ctrl+Shift+Z, `1b 5b 31 33 3b 32 7e` = Shift+F3). Mouse and bracketed paste (Cmd+V) aren't covered yet.

## Deploy
None yet. Later: `cargo install --path .` on the Mac and the laptop.

## Conventions
- Positions are **char offsets** into the rope; on-screen columns are display columns (`Doc::display_col`, tabs = 4).
- Every edit goes through `Doc::begin(kind)` so undo works; runs of typing/deleting group into one step.
- Keys follow IntelliJ's default Windows/Linux keymap (see `app.rs` `key()`); terminal-impossible keys get a
  documented fallback (Ctrl+Shift+Z redo → also Alt+Z, Alt+Shift+Z, Ω right after an undo; see Keys (undo/redo)).
- Colours only from `ui::theme` (M5 exceptions, kept local to avoid clashing with M3's theme edits: `git::ui`
  BLUE / ADD_BG / DEL_BG and the terminal's ANSI palette in `term::ui`). Line endings of a file are kept (LF/CRLF). Popups: centred box, `SURFACE_1`
  background, `ACCENT` title, selected row `SELECTION`.
- **New key bindings must be added to `src/hints/table.rs`** (when they are among the useful few for their
  context; at most 8 rows each, fallbacks via `fb(kitty key, legacy key, …)`). `cargo test hints` feeds every row
  into a fresh `App` and fails on a key that does nothing there; keep that test green instead of loosening it.
- Background work (walks, searches) runs on threads and streams through `mpsc` channels; `App::tick()` drains them
  before every draw (the loop wakes at least every 250 ms). Never block the main loop on the file system.

## Keys (undo/redo, also on a Mac)
| Terminal | Undo | Redo |
|---|---|---|
| any | Ctrl+Z | Ctrl+Shift+Z (kitty protocol only; elsewhere it arrives as Ctrl+Z and undoes), Alt+Z, Alt+Shift+Z |
| Terminal.app, iTerm2 (defaults) | Ctrl+Z | **Option+Z right after an undo** (arrives as `Ω`, Option+Shift+Z as `¸`); Alt+Z needs "Use Option as Meta key" |
| iTerm2 with CSI u / kitty protocol | Ctrl+Z | Ctrl+Shift+Z |
| Ghostty, kitty, WezTerm (kitty protocol) | Ctrl+Z, **Cmd+Z** where the terminal passes it | Ctrl+Shift+Z, **Cmd+Shift+Z** |
- Ghostty keeps Cmd+Z / Cmd+Shift+Z for its own undo (closed tabs); to give them to catlas add
  `keybind = super+z=unbind` and `keybind = super+shift+z=unbind` to its config. iTerm2 / Terminal.app
  use Cmd+Z themselves (iTerm2: map it in Settings → Keys → Key Bindings to "Send Hex Code" `0x1a`).
- With SUPER, Cmd+S / Cmd+A / Cmd+C / Cmd+X work as their Ctrl twins too (editor only); Cmd+V stays
  the terminal's paste.
- `Ω`/`¸` are typed as usual unless the previous editor key was undo/redo and something is left to
  redo. "Nothing to undo" / "Nothing to redo" show in the status bar.
- Undo restores the text, cursor and selection from before the edit; edits that change nothing make no
  undo step.

## Keys (shortcut helper)
| Key | Does |
|---|---|
| **Alt+/** (also Alt+?; Alt+Shift+7 on German layouts) | show / hide the shortcut helper, everywhere (terminal too); saved as `hints = true/false` in `~/.config/code-atlas/settings.toml` |
| `catlas --no-hints` | start without it (this run only, nothing saved) |
The panel sits bottom right in the editor area (top right while the cursor line is under it), shows the keys for
what has focus (title: `editor · IDEA`, `editor · selection`, `vim · NORMAL`, `terminal`, `commit`, `diff view`,
`project tree`, `go to file`, `agents · prompt`, `completion`, `prompt · Find`, …) and the fallback keys when the
terminal has no kitty protocol. A mouse action with a key (tab click, tree open, wheel, dock tabs) or a fallback
used where the real key works shows "Tip: key" at its top for 3 s.

## Keys (project view)
| Key | Does |
|---|---|
| Alt+1 | show + focus the project tree; again hides it (focus elsewhere: focuses it) |
| ↑↓ PgUp PgDn Home End | move in the tree · → / ← unfold / fold (or step in / out) · Enter / F4 open file or toggle folder · Esc back to the editor |
| Alt+F1 | reveal the current file in the tree |
| Ctrl+Shift+N, **Ctrl+P** | Go to File: type to fuzzy-filter, `name:42` jumps to line 42, ↑↓, Enter |
| Ctrl+Shift+F, **Alt+F** | Find in Files (selection pre-fills it; smart case), Enter opens at the match, selected |
| Ctrl+E | Recent Files (previous file preselected, type to filter) |
| **Ctrl+T** (also Alt+S, Alt+Space; Option+Space on a Mac) | Search Everywhere: file names (FILES) and contents (TEXT) at once; Tab / Shift+Tab = All / Files / Text; `name:42` works; Enter opens at the match; Alt+F1 shows the row in the tree; from the focused tree it searches the selected folder (Backspace on an empty input widens) |
| typing in the focused tree | speed search: jumps to the best matching name (closed folders too, via the index), ↑↓ next/previous match, Backspace, Esc clears |
| while a popup row is selected | the tree (if shown) opens down to that file and selects it; Esc puts it back, opening keeps it |
| in popups | Backspace, Ctrl+Backspace / Ctrl+H word, Ctrl+U clear, Esc close; mouse click opens a row, wheel scrolls |

## Keys (code smarts)
| Key | Does |
|---|---|
| Ctrl+/ (also arrives as Ctrl+7, Ctrl+_) | comment / uncomment the line or selected lines (`//`, `#` or `--` for Lua/SQL; XML, DTD, HTML, Markdown wrap each line in `<!-- -->`, CSS in `/* */`; JSON has none) |
| Ctrl+Space, **Ctrl+J** (Mac: macOS takes Ctrl+Space), Ctrl+@ | completion. **Opens by itself** while you type a name (100 ms pause; 1+ letters with a server, 2+ for buffer words; not in comments/strings, plain text or Markdown) and at once after the server's trigger characters (`.`, `::` …). ↑↓ PgUp PgDn, **Enter** inserts (a word that is typed out already: new line), **Tab** replaces the rest of the word, **Esc** closes (and it stays shut for that word), typing filters (fuzzy). Without a server: words from the open files (kind `w`), nearest first; with a server that has < 10 matches they come under its items. Functions come with `()` (cursor inside) or the server's snippet: its placeholders are selected one by one, **Tab / Shift+Tab** next / previous, the last Tab lands after it, Esc ends. `autocomplete = false` in `settings.toml` keeps only the keys |
| (typing `(` or `,`) | signature help above the line, current parameter in bold accent; follows typing, closes off the line or with Esc |
| Ctrl+B | go to definition (opens or switches the tab; several → a list) · **Ctrl+Alt+←** back |
| Ctrl+Alt+B | go to implementation(s) (one jumps, several → a list) |
| Alt+F7 | find usages: list of `line text … file:line`, type to filter, Enter opens |
| Ctrl+F12 | file structure (document symbols, nested), the symbol at the cursor preselected, type to filter |
| Ctrl+Alt+Shift+N, **Alt+N** | go to symbol in the project (workspace symbols; starts with the word at the cursor, the server filters) |
| F2 / Shift+F2 | next / previous error or warning in the file (wraps; infos when there are none), "3/7 message" in the status bar |
| Alt+6 | the file's problems as a list (IDEA's Problems window) |
| in the lists | ↑↓ PgUp PgDn, Ctrl+Home/End, typing filters, Backspace / Ctrl+Backspace / Ctrl+U, Enter or F4 open, Esc close |
| **F1** | quick documentation (hover). IDEA's Ctrl+Q is quit here. ↑↓ scroll, Esc or any key closes |
| Shift+F6 | rename symbol (prompt pre-filled with the word); files changed elsewhere open as unsaved tabs |
| Ctrl+Alt+L | reformat file (server's formatter, e.g. rustfmt) |
| Alt+Enter | quick fixes / intentions (code actions) at the cursor, preferred first |
Diagnostics: ● in the gutter (red error, yellow warning, blue info; hints hidden), coloured underline, and the
message in the status bar when the cursor is on it.
In Vim normal/visual mode Ctrl+B stays Vim's page up and `.` stays repeat: go to definition is **Ctrl+]** there,
and completion opens by itself only in insert mode (Ctrl+J stays Vim's line down in normal mode). The language name sits before the IDEA/Vim mode badge. Server progress ("rust-analyzer: Indexing 40%") shows there too.

- New features go in their own module (`src/<feature>/…`) with a small hook in `app.rs`/`ui.rs`, so parallel work
  doesn't collide.

## Keys (agents panel)
| Key | Does |
|---|---|
| Alt+9 | show + focus the agents panel; again hides it (focus elsewhere: focuses it) |
| Esc | back to the editor (the panel stays) · Tab / Shift+Tab: prompt → run list → transcript |
| in the prompt | Enter send (new agent, or a follow-up to the selected run) · **Alt+Enter**, Shift+Enter, Ctrl+J newline · Ctrl+U clear · ↑ on the first line goes to the list |
| Alt+M / Alt+A / Alt+I | next permission mode (from `claude --help`, default = your settings) / agent type / include the current file + selected lines |
| Ctrl+N | "+ New agent" · **Ctrl+C** stop the selected run · Del (in the list) remove a run from the list |
| ↑↓ (list) | pick a run (its transcript shows) · ↑↓ PgUp PgDn Home End (transcript) scroll, End follows again |
| Ctrl+Alt+Y | reload the current file from disk (asks once if it has unsaved edits) |
| mouse | click a run / the prompt, wheel scrolls the transcript |

## Keys (terminal, git)
| Key | Does |
|---|---|
| **Alt+F12** | terminal: open + focus (starts `$SHELL -l` in the project root); focused: hide the dock |
| in the terminal | every key goes to the shell (Esc, Ctrl+C, Ctrl+R ...) · **Ctrl+Alt+↑** back to the editor (dock stays) · Shift+Esc hide · Shift+PgUp/PgDn or wheel scrollback · Alt+1 / Alt+9 switch tool window · Enter restarts an exited shell |
| **Alt+0** | Commit panel (toggles) · **Ctrl+K** opens it with the cursor in the message (again with a message: commit) |
| in the Commit panel | ↑↓ PgUp PgDn Home End · Space tick/untick · Ctrl+A all/none · Enter or F4 diff · Tab list ↔ message · typing starts the message · **Ctrl+Enter** commit (fallbacks **Alt+Enter**, Ctrl+J) · F5 refresh · Esc editor; click a row, click `[x]` to tick |
| in the diff view | ↑↓ PgUp PgDn (Space) Home End scroll · F7 / Shift+F7 (n / p) next/previous hunk · Alt+↑/↓ previous/next file · F4 / Enter open the file there · Esc back to the panel |
| dock focused | Ctrl+Shift+↑/↓ taller/shorter |
- Gutter: green bar = added line, blue bar = changed, red ▁ = lines deleted below (▔ at the top of the file), vs
  HEAD and live while typing. Tree: modified blue, new green, folders with changes blue. Branch: `⎇ main` in the
  status bar. The editor never pushes, forces, amends or resets.

## Keys (Vim mode)
Off by default. On with `catlas --vim`, `CATLAS_VIM=1`, or **Ctrl+Alt+V** at runtime (toggles; the status bar
badge shows IDEA / NORMAL / INSERT / VISUAL / V-LINE, the cursor is a block in normal/visual mode).
| Area | Keys |
|---|---|
| modes | `i a I A o O` insert (with a count: `3ix<Esc>`), `v` visual, `V` visual line, `:` command line, Esc / Ctrl+[ back to normal |
| motions (all take counts) | `h j k l` + arrows, `w b e W B E`, `0 ^ $` Home End, `gg G {n}G`, `f F t T ; ,`, `%`, `{ }`, `+ - Enter`, `n N * #` |
| operators | `d c y > < gu gU g~` + motion, text object (`iw aw iW aW i" a" i' a' i( a( ib i{ a{ iB i[ a[ i< a<`) or doubled (`dd cc yy >> << guu`) |
| short commands | `x X s S r{c} J ~ D C Y p P u Ctrl+R .` (`.` repeats the last change incl. typed text; a count replaces its count) |
| visual | motions, text objects, `o` swap ends, `d x c s y > < ~ u U J r{c} p`, `D X C S Y` whole lines, `:` gives `:'<,'>` |
| search | `/text` `?text` (Enter; empty = last), `n N`, `* #` whole word; plain text, smart case (`Doc::find`) |
| `:` commands | `w [path]`, `q`, `q!`, `wq`, `x`, `wa`, `qa`, `qa!`, `wqa`, `e path`, `{n}`, `d`, `s/a/b/[gi]` (`&` = match, any delimiter), ranges `%`, `n,m`, `.`, `$`, `.+k`, `'<,'>` |
- One Vim command (or one insert session from `i` to Esc) is one undo step; moving with arrows inside insert
  mode starts a new one (as in Vim).
- Ctrl/Alt chords stay IDEA's in every mode (Ctrl+S, Ctrl+Q, Ctrl+F, Ctrl+P, Alt+arrows, Ctrl+D duplicate, Ctrl+V
  paste...) except Ctrl+R (redo) and Ctrl+[ (Esc). Shift+arrows select like IDEA and so enter visual mode, as
  does a mouse drag or a Ctrl+F match.
- Register: the editor clipboard (`y`/`d`/`p` and Ctrl+C/Ctrl+V share it); only `y` also copies to the system
  clipboard (so `x`/`dd` don't overwrite it).
- `:q` closes the tab (the last tab quits); `:qa` quits; both refuse unsaved work without `!` (E37).

## Gotchas
- Terminals without the kitty protocol can't send Ctrl+Shift+letter, Ctrl+/ arrives as Ctrl+7 or Ctrl+_,
  Ctrl+Backspace as Ctrl+H. macOS Terminal.app lacks the protocol.
- Shift+F3 (find previous) doesn't work without the kitty protocol: legacy terminals send `ESC[1;2R`, which
  crossterm 0.28 (`parse.rs`) reads as a cursor-position report and drops. Fallback: **Alt+P**.
- A final line break opens one more (empty) line that is drawn and reachable (as in IntelliJ):
  `Doc::visible_lines()` counts it; line moves/deletes use `content_lines()` so the final break stays. Vim
  motions (`G`, `j`, `dG`) use `content_lines()` too: in Vim that empty line doesn't exist.
- tmux `send-keys -l 'x;'`: a trailing `;` is a tmux command separator and is lost; send it as `'\;'`
  (the e2e `typ` helper does this).
- Shift+Shift ("search everywhere") needs key *release* events of a lone Shift (kitty `REPORT_ALL_KEYS_AS_ESCAPE_CODES`
  + `REPORT_EVENT_TYPES`); not done. Search Everywhere is **Ctrl+T** (works in Terminal.app/iTerm2 with no setup).
  On a Mac, Alt+S / Alt+Space need Option as Meta (Terminal.app: Settings → Profiles → Keyboard → "Use Option as
  Meta key"; iTerm2: Profiles → Keys → Left Option key = Esc+; Ghostty: `macos-option-as-alt = true`). Without it
  Option+Space types U+00A0, which catlas also reads as Search Everywhere; Option+S types `ß` and stays a letter.
- Ctrl+Shift+N / Ctrl+Shift+F arrive as Ctrl+N / Ctrl+F without the kitty protocol (new file / find), hence the
  Ctrl+P / Alt+F fallbacks. Alt+1, Alt+F need "Option as Meta" on macOS terminals (else they type `¡`, `ƒ`).
- Vim mode's undo groups: `Doc::begin_group` makes every edit until `end_group` one step; `undo`/`redo`
  inside an open group re-arm it. Anything editing for Vim must stay inside `Vim::key` (it opens/closes the group)
  or the insert session; the Vim visual range is inclusive while `Doc::selection` is `[s, e)`, so `before_ide`
  selects the exact range before an IDEA chord and `after_ide` restores direction afterwards.
- Vim `wants()` decides who gets a key; insert-mode typing goes through Vim too (to record it for `.` and
  repeat counts) but uses the same `Doc::type_char`/`newline`, so IDEA's pairing and smart Enter still apply.
- tmux shows no cursor shape: check Vim's block/bar cursor in the raw stream (`tmux pipe-pane -O 'cat > f'`,
  look for `ESC[2 q` block / `ESC[6 q` bar).
- Terminal: login shells can drop keys typed in their first second (zsh plugins); e2e waits 1.5 s first.
  `vt100` doesn't answer queries, so `term::keys::queries` replies to `ESC[6n` and `ESC[c` (fish/zsh wait for
  them). Shells need Esc, hence Ctrl+Alt+↑ / Alt+F12 to leave. Ctrl+Enter only arrives with the kitty protocol;
  elsewhere it's Enter or Ctrl+J, hence Alt+Enter. `set_style` doesn't clear symbols: anything drawn over the
  editor (diff view) must `Clear` first.
- Git e2e: scratch repos in `$SCRATCH` must exclude the harness files (`bin/pbcopy`, `.expected` ...) or they
  show as untracked. Never commit in a real repo from the editor during tests.
- In a git worktree `.git` is a *file*; the tree hides `.git` whatever its type.
- In tmux tests, don't send `Escape` and an Alt key in one `send-keys` call: `ESC ESC f` reads as Esc + typed "f".
  Mouse clicks can be sent as SGR sequences: `tmux send-keys -t s -l $'\e[<0;COL;ROWM\e[<0;COL;ROWm'` (1-based).
- Find in files honours `.gitignore` even outside git repos (like `rg --no-require-git`), so e.g. a `.venv`
  listed in a non-git folder's `.gitignore` is skipped; plain `rg` would search it.
- Highlighting: Rust's bundled query tags integer literals `@constant.builtin`; `syntax::refine` recolours numeric
  node kinds as numbers. TS/TSX highlighting = the TypeScript query + JavaScript's (+ JSX); Markdown uses the block
  grammar only (no inline emphasis/links yet).
- Language servers inherit a PATH with their own folder first, else rust-analyzer can't find `cargo`/`rustfmt`
  when `~/.cargo/bin` isn't on PATH. rust-analyzer answers formatting with `null` when rustfmt is missing.
- Positions to and from servers are UTF-16 (`lsp::text::{to_lsp, from_lsp}`); never send char columns.
- Debug a server: `CATLAS_LSP_LOG=/tmp/lsp.log catlas file.rs` logs every message (first 2000 chars).
- typescript-language-server needs the JavaScript `tsserver` of TypeScript ≤ 6: with only TypeScript 7 (the
  global `npm i -g typescript` is 7.0.2, Go, no `lib/tsserver.js`) its `initialize` fails, catlas marks it dead
  and starts the next server in the list, `tsc --lsp --stdio`. A project with its own TypeScript ≤ 6 in
  `node_modules` gets typescript-language-server.
- TypeScript 7's server **pulls** diagnostics (`textDocument/diagnostic`, LSP 3.17) instead of publishing them;
  catlas pulls after open/change/save when a server has `diagnosticProvider`.
- jdtls (brew) keeps its workspace in `$TMPDIR/jdtls-<hash of the folder>`; the first open of a Maven project
  imports it (downloads into `~/.m2`, ~1 min on the Spring Boot backend), diagnostics come after that.
- yaml-language-server fetches schemas from schemastore.org for known file names (`schemaStore.enable`, on by
  default); `$schema` URLs in JSON are fetched by vscode-json-language-server. Turn off in your languages.toml.
- jdtls sends filter texts like `apiHit() : void`; completion filters on the leading name only. `$` is part of
  a word in JS/TS/Java/Kotlin, not in shell (`$TAR` completes `TARGET`).
- vscode-json-language-server has no go to definition: catlas checks the server's capabilities first and says
  "<server> has no go to definition" instead of sending the request.
- tmux drops a trailing `;` of `send-keys -l` (see above): in manual LSP checks type `\;`.
- rust-analyzer needs ~1–2 min to index this crate on the Mac before diagnostics/definitions come; cargo-check
  diagnostics come after a save (didSave).
- Completion on a Mac: Ctrl+Space is macOS's "Select the previous input source" (System Settings → Keyboard →
  Keyboard Shortcuts → Input Sources); the OS takes it before the terminal, kitty protocol or not. Use **Ctrl+J**
  (or untick that shortcut). Ctrl+@ (Ctrl+Shift+2) sends NUL, which crossterm reads as Ctrl+Space.
- Completion as you type is scheduled in `smart_after_key` (`auto_after_key`) and sent from `lsp_tick`
  (`auto_tick`), not from `App::tick`: unit tests that only call `tick()` never see automatic popups; the
  `lsp::auto` tests call `auto_tick` with a time.
- Cmd+Z doesn't reach terminal programs by default on macOS: Ghostty 1.3 binds `super+z=undo` itself
  (`ghostty +list-keybinds --default`), iTerm2/Terminal.app use it in their menus. crossterm only sees
  Cmd (`KeyModifiers::SUPER`) through the kitty protocol. Option+Z types `Ω` without "Option as Meta".
- XML highlighting: `tree-sitter-xml`'s query tags attribute names `@property`, quotes as delimiters and
  CDATA brackets `@markup.heading`; `syntax::XML_EXTRA` (prepended, first pattern wins) recolours them.
  CDATA outside the root element is a parse error (no colours there).
- e2e colour checks: `tmux capture-pane -e` repeats an SGR only when it changes, so a token can inherit
  its colour from the gutter before it; `scenarios/xml.sh` `colour_before` replays the codes per char.
- Ctrl+Alt+L / Alt+Enter / Alt+F7 / Alt+N / Alt+6 / Ctrl+Alt+B need "Option as Meta" on macOS terminals for the Alt part. Ctrl+Alt+Shift+N arrives as Ctrl+Alt+N without the kitty protocol (both open the symbol search).
- Agents panel: `claude -p` can't ask for permissions, so in the default mode edits are refused (red `⏺` line +
  "N denied"); use Alt+M → acceptEdits for edits. `manual` reports itself as `default` in the init line.
- Agents panel: never test the stream with a nested `claude` you don't need: `CATLAS_CLAUDE=fake.sh` (prints
  stream-json lines) and `CATLAS_STAFF_CMD='exit 1'` (unreachable) make free e2e runs; `CATLAS_CONFIG_DIR`
  keeps test runs out of your real `~/.config/code-atlas/agents.json`.
- `aa-staff.sh status` repeats every old stopped session (dozens of lines); the panel groups by name. Alt+9 / Alt+M
  need "Option as Meta" on macOS terminals (else `ª`, `µ`).
- On macOS `$TMPDIR` is under `/var`, a link to `/private/var`: compare paths via `project::canonical`.
- System clipboard: copy goes out via pbcopy / wl-copy / xclip / OSC 52; paste from outside arrives as a
  bracketed paste (Cmd+V / Ctrl+Shift+V), Ctrl+V pastes the editor's own clipboard.

## Decisions
`docs/decisions/0011-autocomplete.md` – completion as you type (100 ms pause, server trigger characters, not in comments/strings/prose, newest answer only), Ctrl+J / Ctrl+@ next to Ctrl+Space (macOS takes it), Enter on a typed-out word = new line, word completion from open buffers (alone or under few server items), `autocomplete` setting.
`docs/decisions/0010-mac-undo-and-xml.md` – Cmd+Z/Cmd chords via SUPER (editor only), Ω-after-undo redo, Alt+Shift+Z, "Nothing to undo", no-op edits make no undo step, XML/DTD highlighting (tree-sitter-xml), per-line `<!-- -->` comments, lemminx entry.
`docs/decisions/0009-quick-search.md` – Search Everywhere: Ctrl+T (+ Alt+S/Alt+Space, Mac NBSP), FILES + TEXT sections, tabs, folder scope from the tree, tree follow with snapshot/Esc restore, speed search.
`docs/decisions/0008-shortcut-helper.md` – key-hints: glass panel bottom right, 20 contexts, fallbacks by kitty, Alt+/ + `settings.toml`, Key Promoter tips, `table.rs` as the single source with a binding test.
`docs/decisions/0001-stack-and-keymap.md` – Rust + ratatui, IDEA keys by default, Vim optional, internal tool.
`docs/decisions/0002-project-view.md` – M2 choices: args, ignore rules, own fuzzy scorer, smart-case find in files, fallback keys.
`docs/decisions/0003-vim-mode.md` – M4 choices: who gets which key, undo groups, register, `:q` per tab, plain-text search.
`docs/decisions/0004-code-smarts.md` – M3 choices: own query cursor + diff-based incremental parse, full-text sync, F1 hover, no snippets.
`docs/decisions/0005-agents-panel.md` – M6 choices: one `claude -p` per turn + `--resume`, flags only from `claude --help` (never bypass), stop by process group, reload rules, `agents.json`, staff over SSH.
`docs/decisions/0006-git-and-terminal.md` – M5 choices: bottom dock, PTY + vt100, git CLI + in-memory gutter diffs, commit rules (never push/force), leave keys.
`docs/decisions/0007-more-language-servers.md` – languages.toml layers + merge rules, server per language, fallback on a failed start, `also` servers, snippets, pull diagnostics, new keys, what was installed.

## Current state (2026-10-26)
Completion as you type done 2026-10-26 (autocomplete, decision 0011, see Keys (code smarts)): the list opens by
itself after a 100 ms pause in a name and on the server's trigger characters, stale answers dropped by sequence
number, Ctrl+J (and Ctrl+@) where macOS takes Ctrl+Space, words from the open buffers without a server and under
few server items, `autocomplete = true|false` in `settings.toml`. Checked live on the Mac: rust-analyzer 1.94.1
(component installed; `catlas --health`: 17 of 22 languages ready, rust-analyzer used from `~/.cargo/bin`),
pyright, TypeScript (`tsc --lsp`). Tests: unit (`src/lsp/auto.rs`, `words.rs`, `Syntax::in_comment_or_string`,
settings, hint tip on a Mac; 261 in all), e2e `scenarios/autocomplete.sh` (4 scenarios, 58 in all). Not done:
completion item resolve (docs), a delay/min-length setting, words from files that aren't open.
Search Everywhere done 2026-10-26 (quick-search, decision 0009, see Keys (project view)): Ctrl+T popup with
file names and contents at once (FILES / TEXT sections, All/Files/Text tabs, `name:42`), folder scope from the
tree, the tree following the selection in every project popup (Esc restores), Alt+F1 reveal, IDEA speed
search in the tree (fuzzy on names, closed files via the index). Shortcut helper: 22 contexts. Tests: unit tests
in `src/project/quick.rs` + `tree.rs` and binding tests for the 2 new contexts (235 unit), e2e
`scenarios/quick_search.sh` (8 scenarios, 50 in all). Not done: double Shift (kitty), ranking content hits,
opening a hidden tree to follow.
Mac undo/redo + XML done 2026-10-26 (decision 0010, see Keys (undo/redo)): Cmd+Z / Cmd+Shift+Z (and Cmd+S/A/C/X)
when the terminal passes Cmd on, redo with Option+Z (`Ω`) right after an undo in Terminal.app/iTerm2, Alt+Shift+Z,
"Nothing to undo/redo" messages, undo/redo row in the editor hints. Bug fixed: edits that changed nothing
(Shift+Tab on an unindented line, Ctrl+Backspace at the start, empty paste, Ctrl+Y on an empty file) left an
empty undo step, so Ctrl+Z seemed dead, and marked the file changed. XML + DTD highlighting (pom.xml, .csproj,
.plist, .svg, .xsd, .xslt, .xaml, .iml, feeds …), Ctrl+/ wraps lines in `<!-- -->` (also HTML, Markdown; CSS
`/* */`), `lemminx` in `languages.toml` (not installed). Tests: unit (`src/mackeys.rs`, doc undo, XML detection
/ spans, comment wrapping), e2e `redo_option_omega`, `kitty_cmd_z_undo_redo`, `scenarios/xml.sh` (colours
from the tmux screen, comment toggle). Not done yet: Cmd+Z checked by bytes only (not pressed in Ghostty/kitty
themselves), `--` inside a commented XML line isn't escaped, no block comment key (Ctrl+Shift+/), LemMinX untried.
Shortcut helper done 2026-10-25 (key-hints, decision 0008, see Keys (shortcut helper)): see-through panel bottom
right in the editor area, 20 contexts, kitty/legacy key choice, moves to the top right off the cursor line, Alt+/
toggle saved in `settings.toml` (new `src/config.rs`), `--no-hints`, Key Promoter tips. Tests: 32 unit tests in
`src/hints/` + `src/config.rs` (one binding test per context feeding every table key into a fresh `App`, checked
with a mutation: a bogus row fails) and e2e `scenarios/hints.sh` (4 scenarios); the harness now uses a scratch
config folder for every scenario. Not done yet: a key to list *all* keys of a context (the panel shows the useful
few), tips for more mouse actions (diff/commit clicks, completion clicks), hints for Vim's `:` commands beyond
`:w`/`:q`; the helper isn't configurable beyond on/off (M7 settings/palette could add position or
row choice).
M1 core editing done (tabs, undo/redo, selection, bracket pairing, smart Enter/Home, duplicate/delete/move line,
find, go to line, mouse, clipboard). M2–M6 assigned to Aa staff, see `docs/ROADMAP.md`.
M2 project view done 2026-10-25: tree sidebar, Go to File, Find in Files (whole `~/Projects` in under 1 s),
Recent Files, `catlas <dir>`. Not done yet: recent files aren't saved across runs (M7 settings), the finder index
and the tree don't watch the disk (new files show after re-expanding a folder / a restart), no sidebar resizing.
E2E suite `tests/e2e/` added 2026-10-25 (QA, 31 scenarios). Fixed: Ctrl+Y on the last line dropped the
file's final line break; a quote or bracket typed over a selection replaced it instead of wrapping when a
letter/quote was next to it. Then (same day) the empty line after a final line break is
drawn and reachable like in IntelliJ (fixes Ctrl+End landing on a hidden line), and Alt+P finds the previous
match where Shift+F3 can't arrive. Suite: 31/31 pass.
M4 Vim mode done 2026-10-25 (see Keys (Vim mode)): 60 engine tests (several hundred key-sequence checks) in `src/vim/tests.rs`, checked in tmux
(ci", dd/u, /foo n, :%s, Vj>, `.`, Ctrl+Alt+V, Ctrl+S in normal mode, :q refusing, :wq, :e, block/bar cursor).
Not done yet: named registers and `"`, marks, macros (`q`/`@`), Ctrl+V block visual, Ctrl+D/U/O/I (IDEA keeps
Ctrl+D), `gv`, `ge`, regex search/substitute, `:set`, a Vim toggle in the settings file / command palette (M7),
and an e2e scenario file for Vim (`tests/e2e/scenarios/vim.sh`, QA's area).
M3 code smarts done 2026-10-25: tree-sitter highlighting for 10 languages (5,000+ line file: ~1 ms per keystroke
in release, ~10 ms debug), language in the status bar, Ctrl+/ comments, LSP client with diagnostics, completion,
go to definition + back, hover, rename, format, quick fixes; verified with rust-analyzer in tmux. Not done yet:
TypeScript/Python/Java servers untested here (none installed on the Mac; missing-server message checked),
no e2e scenarios for M3 in `tests/e2e/` (QA's harness; needs a server, so they'd be slow), no signature help,
find usages (Alt+F7), semantic tokens, inline Markdown, file create/rename ops in workspace edits, or completion
item resolve (docs in the list).
LSP extended 2026-10-25 (lsp-more, decision 0007): configurable servers (`languages.toml`, user + project
layers), `catlas --health`, 20 highlighted languages (+ Go, C, C++, Kotlin, YAML, HTML, CSS, Lua, Dockerfile, SQL),
signature help, find usages (Alt+F7), file structure (Ctrl+F12), workspace symbols (Ctrl+Alt+Shift+N / Alt+N),
implementations (Ctrl+Alt+B), F2 / Shift+F2 + Alt+6 problems, snippet completion with Tab stops and `()` for
functions, extra servers per language (ruff next to pyright: diagnostics + quick fixes), pull diagnostics, fallback
to the next server when one fails to start. Verified in tmux on scratch copies under `$TMPDIR` (diagnostic,
completion, Ctrl+B each): TypeScript (Cashflow Atlas `frontend/server.ts`, via `tsc --lsp`), Java (jdtls on the
Spring Boot backend, + Ctrl+Alt+B, Alt+F7, Ctrl+F12, Alt+N), Python (pyright + ruff on Tech Shorts `app/calm.py`,
ruff quick fix), Bash (deploy.sh, shellcheck), YAML and JSON (local schema), Go (gopls, snippet Tab stops,
Ctrl+Alt+B); quick checks for C (clangd), Lua, Dockerfile, HTML. E2E `tests/e2e/scenarios/lsp.sh` uses a fake
server (`tests/e2e/fake-lsp.pl`, Perl + JSON::PP). Not done yet: Kotlin/Markdown/TOML servers not installed or
tried, mirrored snippet stops (`${1}` twice) aren't linked, quick fixes from `also` servers only (requests other
than code actions go to the main server), no mouse in the pickers, semantic tokens, completion item resolve,
user-defined languages without a built-in grammar.
M6 agents panel done 2026-10-25 (see Keys (agents panel), decision 0005): runs `claude -p` stream-json in the
project root with a live transcript, follow-ups via `--resume`, stop, several runs at once, permission mode and
agent choosers read from `claude --help`, include file/selection, staff status from the laptop every 60 s,
reload of open files the agent changed (or "changed on disk" when they have unsaved edits), runs saved in
`~/.config/code-atlas/agents.json`. Verified with three real runs in tmux (edit in acceptEdits, resumed
follow-up editing a dirty file, stop) and free fake-CLI runs. Not done yet: answering permission prompts from the
panel (`--permission-prompt-tool`), listing local `claude agents` sessions, attaching to / messaging the staff
sessions, partial-message streaming (text shows per block), an e2e scenario file for the panel (QA's area).
M5 git + terminal done 2026-10-25 (see Keys (terminal, git), decision 0006): bottom dock with a real PTY terminal
(colours, resize, scrollback, exit/restart), live gutter marks vs HEAD, Commit panel with ticks + message +
commit, unified diff view, branch in the status bar, git colours in the tree. Tests: unit (status parsing, diff
→ marks/hunks, key encoding, query detection, dock layout, a real PTY, a scratch-repo status → gutter → diff →
commit flow) and e2e `tests/e2e/scenarios/git_term.sh` (terminal echo/ls, gutter + branch, panel → diff →
commit landing in `git log`). Not done yet: several terminals as tabs, mouse forwarding to terminal programs,
copying text out of the terminal, side-by-side diff, staging single hunks, rollback/revert, a cursor in the
commit message (append-only editing), Problems tab (M3 diagnostics could go there).
