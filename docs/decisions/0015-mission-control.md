# 0015 – Mission control: agent-native editing

Date: 2026-10-29 · Status: accepted (aa-app-dev, track C of "futuristic + agents + huge code bases + many
repos"; auto mode. astra: "let make the editor more futristic and adavnce to work wit h agents and huge code
bases and mutiple repos at once", and earlier "we mostly as claude to do work and we mostly wanna see how dise it go")

## Context
astra mostly has Claude agents do the work and then reviews it. Until now catlas ran agents only **in place**
(agents panel, decision 0005), one checkout shared by every agent and by astra, and reviewed them with the Git
view (decision 0012). Several agents at once stepped on each other, and there was no overview of who does what.
Track A (decision 0013, multi-repo workspaces) and track B (decision 0014, a persistent index) run in parallel;
this track must work with one repo per agent and any root.

## Decision
- **New module `src/mission/`** (`mod.rs` state, keys, ticks, launch; `worktree.rs` git ops; `presence.rs`;
  `notify.rs`; `ui.rs`; `tests.rs`). Runs stay the agents panel's (`src/agents/`): one list, two views. `Run`
  gained `worktree`, `landed`, `tokens` (from the result's `usage`), a stream-line `pulse` (sparkline),
  `turn_touched`, `home()`, `activity()`, `last_line()`, `editing_now()`; `Agents` gained `start_new`,
  `follow_up`, `offered_mode`, `probe_cli`, `new_id`; the panel lists runs by `home()` (worktree runs show under
  their repository).
- **Agents in worktrees.** A new agent (mission control `n`, or ask-an-agent fix / tests / free text) gets
  `git worktree add -b <slug> <main checkout>-wt/<slug> HEAD`, the company's `<repo>-wt/<task>` rule. The slug is
  the task's first words (`fix-the-parser-bug`), `-2`, `-3` when taken; worktrees of a worktree go next to the
  main checkout (one `-wt` folder per repository). Made on a thread (a big checkout takes seconds), shown as a
  "creating worktree…" card meanwhile. Its baseline for reviews is HEAD's tree (no snapshot needed in a fresh
  checkout). Uncommitted edits of your checkout are **not** in it (it is HEAD); the ask prompt carries the
  selected text itself, so the agent sees what you see.
- **Permission mode:** worktree agents start with `acceptEdits` when `claude --help` offers it (`-p` can't ask,
  so the default mode refuses every edit, and the edits only reach your branch after you land them); a ready
  worktree waits for that `--help` answer, so a fast Enter can't start an agent without it. Explain runs
  in place with the CLI default (read-only in practice); in-place agents from mission control use the panel's
  mode. **Never `bypassPermissions`** (still filtered out of the choices) and never `--dangerously-…`.
- **Mission control** (Ctrl+O, Alt+7; not in the terminal pane, where Ctrl+O is the shell's). Full screen under
  the tab bar (Clear + PAPER), title bar with counts / working / needs-you / cost / tokens, then a 3-row card
  per agent, newest first: `#id`, state glyph (spinner / ✓ / ! / ✗ / ■ / ◆ landed / ○ discarded), title,
  activity ("thinking", "editing src/x.rs", "running cargo test", "waiting for you", "needs you · 2 denied"),
  elapsed; `⎇ branch ← base · repo · mode` (or "in place"), `$cost · tokens · N files +a −d` (live every 3 s
  while working, from the run's baseline); the last tool / text line and a 2-minute sparkline (stream lines per
  5 s) in the agent's colour. Footer: keys, an input line, a question (wraps to two rows), or the running job.
  Cards list this project's / repository's runs and every running agent (any repo). Keys: ↑↓ (j k) Home End,
  Enter transcript (↑↓ PgUp PgDn Home End, Esc back), `n` new (Tab worktree / in place, Ctrl+R next repository),
  `f` follow-up (`--resume`), `s` / Ctrl+C stop, `d` / Ctrl+D review (the Git view's run review **on the
  worktree**, `open_git_view_in`), `l` land, `w` remove worktree, `x` discard, Del forget the card, Esc close.
  Single letters like the Git view: they work in any terminal.
- **Land (`l`)** asks first and says exactly what will happen: commit the worktree's uncommitted files as one
  commit (title + `Co-Authored-By: Claude <noreply@anthropic.com>`, so the log shows ✳), then fast-forward the
  base branch, or, when the base moved on, a merge commit (`y`) or a rebase of the agent's branch first (`b`).
  It runs in the checkout the worktree came from and refuses when that checkout is no longer on the base branch.
  A conflict aborts the merge / rebase: the base stays as it was, the work stays on the agent's branch. Never
  pushes, never forces, hooks run. After landing it asks to **remove the worktree** (`git worktree remove`,
  refused when dirty) and the branch (`git branch -d`, merged only). Open files without unsaved edits reload.
- **Discard (`x`)** asks with what is lost ("deletes its worktree (2 uncommitted files) and branch x (3 commits
  not in main). Can't be undone"), then `git worktree remove --force` and `git branch -D`. That is the one
  forced git command in catlas, behind a question that names the loss; nothing else is ever forced.
- **Ask an agent from the code** (Ctrl+U, Alt+A; only when the editor has the keys): a popup titled with
  `file:lines` (the selection, Vim's visual range, or the cursor line), presets **explain** (in place, the
  answer opens in the agents panel), **fix**, **write tests** (worktree), or free text (worktree; Tab toggles in
  place / worktree). The prompt = the preset's instruction or the typed text + path + line numbers + the lines.
  The agent works in the file's repository (the project folder when the file is in it).
- **Presence:** a `✳` in the agent's colour (6 colours from the series palette, by id) on tabs and tree rows
  of files a running agent is writing (pending edit tools + files written this turn; a worktree file maps to the
  same path in your checkout, so you see which agent touches what); **line numbers in the agent's colour** for
  lines the newest run of this session changed in the current file (the file now vs that run's baseline: like
  the run review it is "since", so your own edits after it count too); a **status-bar ticker** left of the
  branch: `✳ 2 agents · 1 editing · #3 editing src/x.rs`.
- **Notifications:** when a run stops working (done, failed, stopped, or done with permission denials = "needs
  you"), the status bar says so (worktree runs: "Ctrl+O: review (d) and land (l)") and the terminal gets OSC 9
  (iTerm2, Ghostty, WezTerm, kitty: a macOS notification) or a bell (Terminal.app, inside tmux).
  `CATLAS_NOTIFY=off|bell` overrides.
- **Shortcut helper:** 5 contexts (34 in all): `mission control`, `mission · transcript`, `mission · prompt`,
  `mission · confirm`, `ask an agent`; the editor rows show `Ctrl+O/U agents / ask agent` (in place of
  `Ctrl+F/G`), the selection rows `Ctrl+U ask an agent` (in place of the bracket-wrap hint), the agents list
  `Ctrl+O mission control`. The binding test covers every row; over mission control the helper sits above the
  footer.
- **Multi-repo (track A):** every agent carries its own repository (`Worktree.repo`, `Run::home()`); a new
  agent picks one with Ctrl+R from `App::mission_repos()` (the editor's repository + those of earlier runs). Track
  A's workspace roots go into `mission_repos()`.

## Consequences
- Several agents edit at once without touching each other or your checkout; nothing reaches your branch
  without a question, and nothing is pushed.
- Worktrees cost disk (a checkout each) until landed / removed / discarded; the cards show which are left.
- Git work (worktree add, land, remove, discard, live counts) runs on threads; the editor never waits for it.
- Tested with a fake `claude` (unit: `src/mission/tests.rs`, `worktree.rs`, `presence.rs`, `notify.rs`; e2e:
  `tests/e2e/scenarios/mission_control.sh`) and one real run ($0.11).
- Not done: answering permission prompts mid-run, a per-agent terminal, conflict resolution inside catlas,
  landing several agents in one go, cost limits per agent, OSC 777 / kitty OSC 99 notifications, presence for
  agents started outside catlas.
