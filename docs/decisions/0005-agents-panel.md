# 0005 – Agents panel (M6): how Code Atlas runs and watches Claude Code agents

Date: 2026-10-25 · Status: accepted (aa-app-dev, full auto mode given by astra)

## Context
M6 adds a right-side tool window (Alt+9) to start Claude Code agents on the open project, follow them, send
follow-ups, stop them and see the persistent Aa staff (central decision 0023). astra asked for full auto mode,
so these are the choices made. Checked against Claude Code 2.1.310 (`claude --help`, 2026-10-25).

## Decision
- **One process per turn:** `claude -p --output-format stream-json --verbose` in the project root, the prompt on
  **stdin** (no quoting or length limits). A follow-up is a new process with `--resume <session_id>` (session id
  from the `system/init` line). We don't keep one long-lived `--input-format stream-json` process per
  conversation: per-turn processes are simpler to stop, survive editor restarts the same way, and cost nothing
  while idle.
- **Flags only from `claude --help`:** the permission-mode chooser lists the `(choices: …)` that `claude --help`
  prints (read in the background when the panel first opens), and `--agent` is offered only if the help lists it.
  Default = no `--permission-mode` flag, i.e. the user's own Claude Code settings. **`bypassPermissions` is never
  offered**, and `--dangerously-skip-permissions` is never passed: the panel may not switch permission checks off.
  In `-p` mode nothing can answer a permission prompt, so in `default`/`manual` mode edits are refused (shown as
  a red tool line with the reason, and "N denied" in the summary). For edits pick `acceptEdits` (Alt+M).
- **Mode is shown per turn:** the transcript prints the mode the CLI reported in `system/init` (`manual` reports
  as `default`), marked "(CLI default)" when no flag was passed; the divider above the transcript shows the run's
  mode, agent, session and cost. Choosers apply to the next send (new run or follow-up); selecting a run sets
  them to that run's last mode and agent.
- **Agent types** come from `~/.claude/agents/*.md` and `<project>/.claude/agents/*.md` (front matter `name:`,
  else the file name). On the Mac that is the installed `aa-*` roles (no `aa-director` there).
- **Stop** = SIGTERM to the agent's own process group (it is spawned with `process_group(0)`, so the shells it
  started end too), SIGKILL after 3 s. Closing the editor stops running agents and saves them as stopped.
- **Transcript:** assistant text word-wrapped, each tool call one line (`⏺ Edit src/app.rs`; yellow spinner while
  pending, green done, red failed + `⎿ reason`), thinking blocks, rate-limit and other events skipped.
- **Changed files:** a successful Edit/Write/MultiEdit/NotebookEdit result checks that file at once; after each
  turn all open files are compared with the disk (catches Bash edits). No unsaved edits → reloaded in place as
  one undoable edit (`Doc::reload_text`, new revision so syntax and LSP see it); unsaved edits → kept, status bar
  "changed on disk", Ctrl+Alt+Y (IDEA: Reload from Disk) reloads after a second press. Files over 8 MB are skipped.
- **Persistence:** `~/.config/code-atlas/agents.json` (`$XDG_CONFIG_HOME/code-atlas`, or `$CATLAS_CONFIG_DIR` for
  tests): every run with prompt(s), session id, state, times, cost and transcript (up to 2 000 entries a run,
  100 runs). The panel lists the current project's runs; a run that was running at exit comes back "stopped"
  and can be resumed.
- **Staff:** `aa-staff.sh status` over `ssh -o ConnectTimeout=5 -o BatchMode=yes astra@astra` (BatchMode:
  never a password prompt over the editor), every 60 s while the panel is open, on a thread. It is run locally
  only on the laptop itself (hostname `astra`): the Mac has `~/atelier-astra` too (a link to the repo) but no
  staff sessions. The script prints every old stopped session again, so we show one row per name (the live one,
  else the newest). Failure = muted "laptop unreachable". `CATLAS_STAFF_CMD` overrides the command.
- `claude` is found on PATH, else `~/.local/bin/claude`; `CATLAS_CLAUDE` overrides it (tests use a fake).

## Consequences
- An agent can't ask for permission mid-run from the panel (no `--permission-prompt-tool` host yet); choose the
  mode before sending.
- Local interactive or background Claude sessions (`claude agents`) aren't listed, only runs started here and
  the staff.
