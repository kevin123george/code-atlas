# 0012 – Git view: reviewing what agents did

Date: 2026-10-26 · Status: accepted (aa-app-dev, auto mode; astra: "should we have a git view as well ?" and
"this should be terminal native as well beacuse we mosttly as claude to do work and we mostly wanna see how dise
it go")

## Context
Code Atlas had git basics (M5, decision 0006): gutter marks, a Commit panel with whole-file ticks, a one-file
diff view. astra mostly has Claude agents do the work, so the main question is **"what did the agent change,
and is it right?"**, while the agent is still working and after. It has to feel like a terminal tool (fast,
keyboard first, readable at 80 columns, working in a plain Terminal.app on a Mac without "Option as Meta"),
not a copy of IntelliJ's GUI.

## Decision
- **One view over the editor area** (`src/git/view.rs`, drawing in `view_ui.rs`), not a dock tab: a review
  needs rows, the 12-row dock is too small. It covers the tree and the editor but not the agents panel, so a
  running agent stays visible next to its changes; the dock (terminal, Commit panel) stays usable below it.
  While open it takes the keys whenever nothing else (tree, panel, dock, popup) has the focus, as the editor
  would. Two tabs: **1 Changes** and **2 Log**.
- **Keys:** **Ctrl+L** opens / closes it (every terminal sends it, nothing in Code Atlas used it; IDEA has no
  Ctrl+L of note, Vim's redraw isn't implemented) and **Alt+8** (next to Alt+9 agents and Alt+0 commit, where
  Option is Meta). Option+8 types `•` (US) or `{` (German) without Meta, so it is *not* read as the key.
  Inside, keys are single letters and digits like `tig` / `lazygit` (no text field to type into), so they work
  in every terminal; F7 / Shift+F7 and Tab / Shift+Tab as in IDEA's diff, with `n`/`p` and `]`/`[` too.
  Ctrl+L is not taken from the terminal pane (it is the shell's clear-screen there).
- **Changes = the review**, one scrollable document for all files: file header (status, path, +/−, `staged`,
  `✳ #id title` when an agent's tools wrote it), hunks with a counter (`3/7`), lines with old/new numbers,
  syntax colours (tree-sitter over the whole old/new text, at most 40 files per refresh and 1 MB each) and
  word-level highlights on changed line pairs (paired in order within a change block; skipped when less than a
  third of the line is shared). Unified, or **side by side** from 150 columns (`v` toggles, 60+ needed). The
  file list sits left of the document from 110 columns, above it below that. Working tree = unstaged
  (`git diff`), untracked files (as all-added, up to 200 files of 512 KB each) and a **STAGED** section
  (`git diff --cached`); `git diff` always runs with `--no-ext-diff --no-textconv --no-renames
  --src-prefix=a/ --dst-prefix=b/` and `core.quotePath=false` so the hunks can go back to `git apply`.
- **Live:** while the view is open a background thread re-reads the diff every second (an agent run's
  review: a new snapshot every 2 s); the raw output (+ untracked names, sizes, mtimes) is the fingerprint, so
  an unchanged tree costs one `git diff` and no re-render. The cursor stays on its line through refreshes and
  layout switches (anchor: path, section, line number or hunk). Measured in e2e: a change shows ~1 s later.
- **Stage / unstage a hunk** (`s`; `S` the whole file): `git apply --cached` with a patch made of the file's
  header lines plus that hunk, `-R` to unstage (untracked files: `git add`). **Commit panel → partial
  commits:** a ticked file with staged hunks *and* other changes shows `· staged hunks only`; when there is
  such a file the commit is `git commit` of the index (not `-- paths`), and it refuses if anything staged is
  unticked. Otherwise it commits as before (`git add` + `git commit -- paths`). Hooks still run.
- **Revert** (`r` hunk, `R` file; IDEA's rollback) always asks first (y / Enter yes, n / Esc no; other keys
  keep the question). Hunk: `git apply -R` on the working tree only; file: the index version
  (`git checkout -- path`), in a run's review the snapshot's version (written by hand, so the index is not
  touched), untracked files are deleted. A staged hunk must be unstaged first. Files with unsaved edits in the
  editor are refused. **`u` undoes** (also Ctrl+Z in the view): the file's bytes before and right after each
  revert are kept; undo writes the "before" back only if the file is still exactly "after". Open clean
  buffers are reloaded after a revert, undo, switch or pull.
- **Per-agent attribution** (`agents/run.rs` `Baseline`): before a run's first turn starts `claude`,
  catlas takes a **snapshot tree** of the working tree: a copy of the index in a temp file
  (`GIT_INDEX_FILE`), `git add -A`, `git write-tree`. Tracked + untracked files, ignored ones left out; the
  user's index, stash and refs are untouched (tested). Stored in `agents.json` with HEAD and the time. The
  run also remembers which files its edit tools wrote (`touched`). After each turn a background job counts
  `git diff --numstat <snapshot> <snapshot now>` → `stat` ("2 files +2 −1"), shown on the transcript divider
  and as a transcript line. **Ctrl+D** in the agents panel (or `a` in the review, which cycles through this
  project's runs, newest first, then back to the working tree) opens the review **since the run started**:
  everything the agent changed, even if it committed (`t`: only the files its tools wrote). Changes the user
  made at the same time show too: it is "since", not "by". Reverting there goes back to the snapshot.
  Snapshot objects are unreferenced (no ref is written), so `git gc` may prune them after its grace period
  (2 weeks by default); an old run then says the snapshot is gone.
- **Log:** `git log --branches --tags --remotes HEAD --date-order -n1500` with our own lane layout
  (`graph.rs`, one row per commit: `●` commit, `│` lanes, `╮`/`╭` a merge opening a lane, `╯`/`╰` lanes
  joining, `┤`/`├` a merge into an existing lane, a colour per lane), short hash, refs (`[main]`, tags yellow,
  remotes violet), **`✳` on commits Claude co-authored** (a `Co-Authored-By: Claude …` /
  noreply@anthropic.com trailer), subject, author and age (100+ columns). `/` filters: words over
  subject/hash/author/refs, `author:x` (or `@x`; `@claude` = the ✳ commits), `path:p` (reloads that path's
  history); the graph shows only for the unfiltered list. Enter = **commit details** (message block, then the
  same review document, vs the first parent; read-only), `y` copies the hash. `h` = the history of the file
  that was open in the editor (`--follow`), again = all branches.
- **Blame** (`b` in the view, for the editor's file): a 15-column gutter left of the line numbers, age +
  author per block of lines, colour by age, `✳` for Claude commits, `· changed` for lines not committed. From
  `git blame --porcelain --contents -` with the buffer on stdin, so it matches unsaved text; refreshed 0.5 s
  after edits. **Ctrl+L on a blamed line** opens that line's commit. `b` again turns it off.
- **Branches popup** (`B`): list (current ●, upstream track, age, subject), Enter switch (asks; says how many
  changed files come along; git refuses rather than overwrite), `n` new from HEAD, `r` rename, `d` delete with
  `git branch -d` only (unmerged branches are refused by git). Never `-D`, never a reset.
- **Push / pull / fetch** (`P` / `U` / `F`) only on that key and after a question saying what goes where
  ("Push 3 commits of main → origin/main? (never forced)", "Pull origin/main into main, fast-forward only
  (2 commits behind as of the last fetch)?", a new branch gets `-u`). Never `--force`, pull is `--ff-only`.
  They run on a thread with `GIT_TERMINAL_PROMPT=0` and, unless the user set their own ssh command,
  `ssh -o BatchMode=yes`: no password prompt can draw over the editor. Tests push only to a bare repository
  in the temp dir.
- **Shortcut helper:** 7 new contexts (29 in all): `git · review`, `git · log`, `git · commit`,
  `git · branches`, `git · confirm`, `git · input`, `editor · blame`; the editor's rows show `Ctrl+L git
  review / log` (in place of Alt+F find in files, which Ctrl+T covers and the selection rows keep); the agents
  runs / transcript rows gain `Ctrl+D`. The binding test covers every row.

## Consequences
- The review shows the disk, not unsaved buffers (that is what an agent changed); the gutter marks still
  follow unsaved edits.
- A `git diff` per second while the view is open; nothing runs while it is closed (blame only for files
  that have it on).
- Not done: hunk staging inside commit details or a run's review (they aren't index-relative), line-level
  staging, staging binary hunks (whole files only), renames shown as such (`--no-renames`: a rename is a
  delete + an add), a three-way conflict view, cherry-pick / revert-commit / rebase, stash UI, horizontal
  scrolling of long lines (they end in `›`), and listing runs started outside Code Atlas (their snapshots
  don't exist).
