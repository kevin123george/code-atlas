# 0018: Several terminals in the dock

Date: 2026-11-01. Status: done.

## Context
Asked whether Code Atlas should become a stand-alone window, astra said "i think nvim supporting mutiple terminal may be a better
option" and chose several terminals inside the editor, which stays a terminal program. The window is parked on branch `native-window`.

## Decision
- **Tabs, each with one or more panes side by side.** Top/bottom splits later if wanted; docks are short, so side by side comes first.
- **Keys:** the dock takes only Alt+T, Alt+S, Alt+W and Ctrl+Alt+←/→ from a focused shell, and Ctrl+Shift+T / D / W with the kitty protocol. Alt+S
  is the split key because `\\` needs extra fingers on German layouts. Alt+Left/Right stay the shell's word movement.
- **Closing a running shell asks** (press again), since it stops what runs there. Any other key cancels.
- **A shell that exits closes its pane** unless it is the last one, which stays to show why and restarts on Enter.
- **New shells start in the project root.**
