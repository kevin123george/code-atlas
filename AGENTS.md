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
input), `ropey` (text buffer), `unicode-width`, `ignore` (ripgrep's gitignore-aware walker, project view). Planned: `tree-sitter` (highlighting), `lsp-types` + own JSON-RPC
client (LSP), `portable-pty` + `vt100` (terminal pane), `git` CLI or `gix` (git).

## Layout
- `src/main.rs` – terminal setup/restore, event loop.
- `src/doc.rs` – one open file: rope, cursor/selection (char offsets), undo/redo snapshots, all edit commands, unit tests.
- `src/app.rs` – tabs, clipboard, prompts (find, go to line, save as), IDEA key map, mouse.
- `src/ui.rs` – drawing (tab bar, gutter, text, status bar) and the `theme` colours (Ledger Terminal dark tokens).
- `src/project/` – project view (M2): `mod.rs` state, keys, mouse, `App::open_target` / `tick` glue; `tree.rs`
  sidebar tree (flat row list, lazy listing, `walker()` = the ignore rules); `fuzzy.rs` file-name scorer;
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
- `docs/ROADMAP.md` – milestones and who owns which files. `docs/decisions/` – project decisions.

## Run & test
```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test                 # unit tests (doc.rs edit logic, project tree/fuzzy/search, Vim engine)
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
  documented fallback (Ctrl+Shift+Z redo → also Alt+Z).
- Colours only from `ui::theme`. Line endings of a file are kept (LF/CRLF). Popups: centred box, `SURFACE_1`
  background, `ACCENT` title, selected row `SELECTION`.
- Background work (walks, searches) runs on threads and streams through `mpsc` channels; `App::tick()` drains them
  before every draw (the loop wakes at least every 250 ms). Never block the main loop on the file system.

## Keys (project view)
| Key | Does |
|---|---|
| Alt+1 | show + focus the project tree; again hides it (focus elsewhere: focuses it) |
| ↑↓ PgUp PgDn Home End | move in the tree · → / ← unfold / fold (or step in / out) · Enter / F4 open file or toggle folder · Esc back to the editor |
| Alt+F1 | reveal the current file in the tree |
| Ctrl+Shift+N, **Ctrl+P** | Go to File: type to fuzzy-filter, `name:42` jumps to line 42, ↑↓, Enter |
| Ctrl+Shift+F, **Alt+F** | Find in Files (selection pre-fills it; smart case), Enter opens at the match, selected |
| Ctrl+E | Recent Files (previous file preselected, type to filter) |
| in popups | Backspace, Ctrl+Backspace / Ctrl+H word, Ctrl+U clear, Esc close; mouse click opens a row, wheel scrolls |
- New features go in their own module (`src/<feature>/…`) with a small hook in `app.rs`/`ui.rs`, so parallel work
  doesn't collide.

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
- Shift+Shift ("search everywhere") needs key *release* events (kitty `REPORT_EVENT_TYPES`); plan a fallback.
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
- In a git worktree `.git` is a *file*; the tree hides `.git` whatever its type.
- In tmux tests, don't send `Escape` and an Alt key in one `send-keys` call: `ESC ESC f` reads as Esc + typed "f".
  Mouse clicks can be sent as SGR sequences: `tmux send-keys -t s -l $'\e[<0;COL;ROWM\e[<0;COL;ROWm'` (1-based).
- Find in files honours `.gitignore` even outside git repos (like `rg --no-require-git`), so e.g. a `.venv`
  listed in a non-git folder's `.gitignore` is skipped; plain `rg` would search it.
- System clipboard: copy goes out via pbcopy / wl-copy / xclip / OSC 52; paste from outside arrives as a
  bracketed paste (Cmd+V / Ctrl+Shift+V), Ctrl+V pastes the editor's own clipboard.

## Decisions
`docs/decisions/0001-stack-and-keymap.md` – Rust + ratatui, IDEA keys by default, Vim optional, internal tool.
`docs/decisions/0002-project-view.md` – M2 choices: args, ignore rules, own fuzzy scorer, smart-case find in files, fallback keys.
`docs/decisions/0003-vim-mode.md` – M4 choices: who gets which key, undo groups, register, `:q` per tab, plain-text search.

## Current state (2026-10-25)
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
