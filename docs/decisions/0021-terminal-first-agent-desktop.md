# 0021: Terminal-first agent desktop direction

Date: 2026-11-01. Status: desktop direction superseded by 0022; workflow requirements retained.

## Context

After choosing Neovim tentatively, astra brainstormed a dedicated desktop application for directing agents and reviewing their work, and accepted all discussed features. Terminal-first interaction and effective management of multiple Claude Code and Codex sessions are central.

## Decision

Record the complete scope in [the product design](../AGENT-DESKTOP.md). Tasks/worktrees organize durable interactive sessions, an attention inbox, Git review, Java navigation and Markdown tools. Reopening the UI should reconnect to surviving processes. Completion claims require evidence; task approval and process lifecycle are separate.

This reopens product design after decision 0020. It does not replace the working Neovim setup or claim that the new desktop has shipped. Existing Code Atlas capabilities and the parked native-window branch will be evaluated for reuse. Build order starts with durable sessions.

## Consequences

The implementation needs a process supervisor independent of the UI and provider adapters for both agents. Full scope and acceptance checks are documented before choosing a desktop framework. No runtime changes were made in this decision.
