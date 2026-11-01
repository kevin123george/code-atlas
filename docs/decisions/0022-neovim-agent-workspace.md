# 0022: Keep Neovim as the agent workspace

Date: 2026-11-01. Status: accepted by astra.

## Context

After brainstorming a dedicated desktop, astra clarified: "i like my nvim much better".

## Decision

Neovim/LazyVim remains the primary interface for editing, directing Claude Code and Codex, worktree navigation and reviewing changes. Park the dedicated desktop direction from 0021. Preserve the workflow requirements from that discussion: durable sessions, task/worktree organization, trustworthy progress, attention and review. Adapt them to Neovim incrementally rather than replacing the editor.

## Consequences

Code Atlas desktop work is paused. The current Neovim additions are staged in `../nvim-agent-base`; live installation is still pending. Their built-in terminal jobs do not survive quitting Neovim. A separate session owner (candidate: installed tmux) needs evaluation before claiming persistence. No runtime changes accompany this decision.
