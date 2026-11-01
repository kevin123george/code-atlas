# Terminal-first agent desktop (parked)

Latest direction: [0022](decisions/0022-neovim-agent-workspace.md) keeps Neovim as the primary interface. Desktop-specific architecture below is historical; workflow requirements remain useful.

2026-11-01. Product scope accepted by astra in conversation. Architecture below is proposed; this document does not claim implementation.

## Purpose

Direct Claude Code and Codex across large repositories, keep their sessions organized, and review their work. The primary activity is supervising agents. Code editing supports inspection and corrections. The desktop window centers real interactive terminals, with keyboard navigation and the Ledger Terminal visual language.

## Workspace model

Repository → task → worktree → sessions. A task records its title, instructions, base branch and base commit, worktree path, review state and checks. A session records its task, agent or shell, launch directory, process identity, timestamps, lifecycle and provider conversation identifier when available. Multiple sessions can serve one task: implementation, review and test shell. Prefer one writing agent per worktree; parallel writing tasks use separate worktrees.

Import existing agent-created worktrees. Discover them through Git rather than assumed directory names. App-created ones follow `<repo>-wt/<task>`. Never switch the main checkout's branch to navigate tasks. Detect externally removed or moved worktrees and surface them as unavailable. Do not silently recreate a missing worktree or redirect its session.

## Main interface

- Left: repositories, tasks, worktrees, named sessions; searchable and pinnable.
- Center: selected terminal with tabs/splits. Code, Markdown preview and diffs open beside it.
- Right: attention inbox and task details; collapse it for terminal space.
- Bottom: current task/branch, connection state and checks.
- Overview: all active tasks, latest evidenced activity, unresolved questions and review readiness.

Keyboard actions: command palette, task/session switcher, next attention item, split/focus pane, launch agent, review diff and return to terminal. Resolve exact shortcuts against terminal applications and German keyboard layouts before implementation. Keep terminal input intact; global shortcuts must be deliberate and discoverable.

## Durable session contract

A local supervisor owns PTYs independently of the desktop UI. The UI subscribes to output and attaches input. UI exit, reload or crash must not terminate agent processes. Persist metadata and bounded terminal history locally; reconnect restores terminal state and layout. Output replay needs sequence identifiers so reconnects do not lose or duplicate bytes. Test ANSI output, Unicode, resizing, alternate screens, input and backpressure.

Distinguish reattach (same live process), resume (new process continuing a provider conversation) and restart (new conversation). Provider resume is offered only when supported and a conversation identifier is known. Supervisor crashes, system reboot and machine sleep are separate recovery cases: never describe a process as alive based only on stale persisted metadata or a reused PID. Initial scope is local sessions; remote hosts are later.

Actions: hide closes a view; archive removes a task from the active list; interrupt sends an explicit interruption; terminate ends a process. Archive never implicitly kills sessions or deletes worktrees. Destructive cleanup names running sessions, uncommitted files and unmerged commits. Closing the app leaves agents running and makes that behavior visible.

## Agent integration and truthful progress

Keep the full Claude Code and Codex interactive terminal available, including their own authentication and permission prompts. Separate adapters map supported structured events into common activity records. Inspect installed CLI capabilities before choosing integration details. Do not infer completion, permissions or tool execution solely from arbitrary terminal text.

Session lifecycle: starting, running, exited, failed, disconnected. Attention: explicit question/approval, reported completion, failed check, connection problem. Task workflow: planned, active, review requested, changes requested, approved, merged, archived. These are separate dimensions. Exit code zero is not task approval; quiet output is not proof of waiting.

Every displayed action identifies its evidence: agent event, process observation, Git observation, check result or user decision. If only terminal output is observable, display running/last output and provide manual attention controls. No invented completion percentages. Activity feed can show changed files, tool events, check results and timestamps. Same-path edits across tasks are potential overlap, not a proven merge conflict.

## Review desk

Review against the task's recorded base, including committed, staged, unstaged and untracked changes. Surface renames, binary files and large diffs. Checks record command, working directory, exit status, timestamps and tested revision; mark results stale when changes occur afterward.

Select a diff hunk and draft feedback for a named session. Show destination task and worktree before sending. A second agent's review remains advisory; user approval is separate. Land only after checking the expected target branch and current repository state. Expose conflicts and preserve work. Cleanup follows successful review/merge as a distinct action.

## Code and documentation

Reuse or adapt Code Atlas's existing search/index, Git review, syntax and LSP work where appropriate. Java needs definitions, references, diagnostics, imports and Maven/Gradle checks, with language-server workspace storage isolated across worktrees. Markdown needs source, safe preview, links and diagrams. File browsing and searches are scoped to the chosen task; large repositories must not block terminal interaction. Watchers and indexing need cancellation, exclusions and bounded resource use.

## Build order and acceptance

1. **Session foundation:** supervisor + metadata + PTY reconnect. Run stand-in processes across two real Git worktrees; close/reopen and crash the UI; verify identical surviving processes, correct cwd, restored output and responsive input. Explicit termination affects only the selected session. Verify actual Claude/Codex screens without sending paid tasks automatically.
2. **Task desktop:** real terminal surface, worktree discovery/create/import, task/session search, names, pins and layout restoration. Show missing worktrees honestly. Keep unrelated workspaces untouched.
3. **Attention:** provider capability probes and adapters, inbox, timestamps and reconnect states. Verify transitions using captured/fake events; unsupported states remain unknown. Add notifications with deduplication.
4. **Review:** baseline-aware diffs, scoped check execution, stale results, feedback drafts and reviewer sessions. Verify divergence, staged/unstaged/untracked changes and failed checks.
5. **Code tools and polish:** Java navigation, Markdown preview, large-repo performance, command palette and restrained live visuals. Benchmark on a representative local repository and inspect the actual desktop UI.

All five stages are in the agreed scope. Staging is sequencing, not removal of features.

## Reuse assessment and open engineering decisions

Code Atlas already has a Rust terminal UI, PTY support, Git reviews, task worktrees, a large-repo index and Java LSP support. Its existing Claude print/stream-json runs do not provide the interactive durable-session contract above. The parked `native-window` branch must be inspected and tested before deciding whether to reuse its renderer. Prefer a shared session core behind the desktop interface; decide the desktop framework after a terminal-fidelity and reconnection prototype. Neovim remains available for daily editing while this is built.

Open: desktop rendering/backend choice, supervisor persistence format and IPC transport, terminal history limits, supported provider events/resume discovery, exact keyboard map. No new network service, downloads, publishing or background service installation is authorized by this design document alone.
