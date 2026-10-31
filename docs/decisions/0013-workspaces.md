# 0013 – Workspaces: several repos at once

Date: 2026-10-28 · Status: accepted (aa-app-dev, auto mode; astra asked to "make the editor more futuristic and
advanced to work with agents and huge code bases and multiple repos at once"). Track A of three (B: persistent
multi-root index, decision 0014; C: parallel agents in worktrees + mission control, decision 0015).

## Context
Code Atlas opened one folder. astra's studio is ~16 git repos in `~/Projects`, mostly changed by agents. Opening
`~/Projects` gave one big tree with no notion of repos: no per-repo git (the Commit panel and Git view only saw a
repo at the project root, so none), paths without the repo, no overview of which repos have changes.

## Decision
- **Model** (`src/workspace/`): a `Workspace` = name, roots (canonical folder, unique label, colour), excludes,
  optional file, and a *home* (the folder given; agents and the terminal start there). One root behaves exactly
  as before: same tree, plain relative paths, no badges, no background status.
- **Opening:** `catlas dir1 dir2 …` (each folder a root; home = the first); `catlas x.catlas.toml`; `catlas dir`
  with `dir/.code-atlas/workspace.toml`; or `catlas dir` (also no arguments) where `dir` is no repo itself but
  holds **2+ git repos one level down** → those repos are the roots (other folders and loose files there are not
  shown; a workspace file can add them). `--single` opts out. `catlas file.rs` alone never turns the current folder
  into a workspace. Workspace file:
  ```toml
  name = "Studio"
  exclude = ["*.log", "dist", "docs/archive"]   # names / * globs anywhere, or paths relative to each root
  roots = ["../tech-shorts"]                     # short form
  [[root]]
  path = "../finance-tracker"                    # relative to the file's folder (~/ ok)
  label = "cashflow"
  color = "green"                                # blue green aqua orange violet yellow pink teal red, #rrggbb
  ```
  Missing roots are skipped with a status note; nested or duplicate roots are refused.
- **Paths:** with several roots a project path is `label/rel`, shown as `label › rel` (Go to File, Recent, Find in
  Files, Search Everywhere, LSP pickers, status messages). `Workspace::rel` / `abs` / `show` are the only mapping;
  the index and find-in-files walk root after root (`search::index_roots` / `find_in_roots`) with the label prefix,
  so nothing else in the project view had to know about roots.
- **Search scoping:** `@label ` (a label or a unique start of one, then a space) in any project popup searches only
  that repo; Ctrl+T on a root row of the tree does too (the tree's folder scope from 0009). No extra key needed.
- **Tree:** one top row per root (label in its colour), folded when there are more than 3; right-aligned badge:
  `⎇branch`, `●N` changed files, `↑ahead`, `↓behind`, `!N` conflicts (the branch is cut first when narrow). `-`
  (and Ctrl+minus with the kitty protocol) collapses all. Reveal, speed search and the popups' tree-follow work
  across roots (`Tree::reveal` starts from the root that holds the path).
- **Status of every repo** (`status.rs`): every 10 s (and on opening the popup, after fetch/pull) a pool of 4
  threads runs `git status --porcelain -z --branch`, `git log -1`, `git remote` per root (`git diff HEAD --numstat`
  only while the popup is open). ~30 repos stay cheap; nothing takes locks (`GIT_OPTIONAL_LOCKS=0`).
  Only with several roots (one root: nothing runs unless the popup is open). The tree's git colours come from the
  current repo's live 2 s status, and from this status for the other repos.
- **Current repo:** the dock's `Git` (Commit panel, Git view, status bar `repo ⎇ branch`). It **follows the
  editor**: switching to a file of another root makes that root current (edge-triggered, so a manual pick sticks
  until the editor moves to another root). Each root keeps its own `Git` (ticks, the commit message) while
  another is current (`WsState.stash`), and the editor file's own repo is polled for its gutter marks even when
  another repo is current.
- **Repos popup: Ctrl+O** (every terminal sends it, nothing used it; Vim's jump-back isn't implemented; `o` in the
  Git view). Summary line (repos with changes, +/− total, ahead, behind), one row per root: current marker, label,
  branch, sync (`✓ in sync`, `↑a ↓b`, no upstream / no remote), changed files and +/−, last commit (or the last
  fetch/pull result). Keys: ↑↓/jk, Enter make current (and reveal it in the tree), `g` / `l` / `c` its changes /
  log / Commit panel, `F` / `U` fetch / pull **all**, `f` / `u` the selected one, `/` filter, `x` only repos with
  changes, `a` add a folder as a root, `d`/Del remove a root (nothing on disk changes), `w` recent workspaces,
  `r`/F5 refresh, Esc. Single letters as in the Git view; Ctrl+Q still quits.
- **Fetch / pull all:** always behind a question that says how many repos and which are skipped (no remote, no
  upstream, not a repo); `fetch --all`, `pull --ff-only` (never a merge, never forced, never push-all). 4 at a
  time, ssh in batch mode, no password prompts (reuses `branches::run_remote`). Per-repo results show in the
  popup, a summary in the status bar; pulled open files reload (clean ones).
- **LSP:** already one server per (server, root found from the file by markers / `.git`), with that root as its
  `workspaceFolders`, so rust-analyzer, tsserver etc. get one server per repo without changes. One server with
  several workspace folders was not done: per-root servers isolate repos (different toolchains) and need nothing new.
- **Recent workspaces:** `workspaces.toml` in the config folder (multi-root or file workspaces only, newest first,
  20 kept, saved on open and on add/remove). `catlas -w` lists them, `catlas -w 2` / `-w name` opens one; `w` in the
  popup switches at runtime (agents and the terminal stay in the folder catlas started in).
- **Shortcut helper:** 4 new contexts (33): repos, repos · input, repos · confirm, recent workspaces; the tree rows
  gain `-` collapse all and `Ctrl+O` repos (Alt+1 and Go to File left that panel; both still work).

## Consequences
- In a folder of repos, folders without git and loose files at the top aren't shown (use a workspace file).
- Agents (`src/agents/`) and the terminal still run in the home folder; per-repo agents are track C's.
- The finder index is built once per start / workspace change (track B makes it persistent and incremental).
- Excludes are a small name / `*` glob / relative-path list, not full gitignore syntax.
- No per-repo terminal, no "commit in all repos", no push-all.
