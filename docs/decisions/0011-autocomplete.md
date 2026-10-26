# 0011 – Completion as you type, a Mac key, word completion

Date: 2026-10-26 · Status: accepted (aa-app-dev, auto mode; astra on the Mac: "auto complete is missing")

## Context
Completion existed (M3, decision 0004 / 0007) but on a Mac it looked missing:
- It only opened after `.` / `::` or with **Ctrl+Space**, and macOS uses Ctrl+Space for "Select the
  previous input source" (System Settings → Keyboard → Keyboard Shortcuts → Input Sources). The OS
  takes it before any terminal sees it, the kitty protocol doesn't help, so the key usually never
  arrived in Terminal.app, iTerm2 or Ghostty.
- IntelliJ shows the list by itself while a name is typed; Code Atlas didn't.
- Files without a language server (XML: lemminx isn't installed, SQL, Markdown, plain text, any
  language whose server is missing) had no completion at all.

## Decision
- **Popup while typing** (`src/lsp/auto.rs`): after a word character, when the word so far has
  **1+ characters with a server** (IDEA) or **2+ for buffer words**, and doesn't start with a digit, a
  request is scheduled; it goes out after **100 ms without typing** (`AUTO_DELAY`, checked in
  `lsp_tick`, which the loop runs every ≤ 30 ms). Any key in between reschedules or cancels it (the
  pending request remembers doc, revision and cursor). Requests are asynchronous, so typing never
  waits for a server; fast typists see the list only when they pause.
- **Trigger characters** come from the server's `completionProvider.triggerCharacters` (default `.`),
  sent at once with `triggerKind = 2`. Whitespace triggers are ignored (typescript-language-server
  lists `" "`, which would ask after every space), and `:` only counts as `::` in Rust/C/C++.
- **Not by itself:** inside comments and strings in code (tree-sitter node at the cursor:
  `Syntax::in_comment_or_string`; interpolations such as `${x}`, f-string `{x}` and `$VAR` count as
  code; data and markup files only skip comments, so JSON keys and XML text still complete); in prose
  (plain text, Markdown), where Enter would complete words in running text; while a snippet's
  placeholders are walked (Tab belongs to the snippet); in Vim's normal/visual mode; after **Esc**
  closed the list for this word (until the next word); and when the list would only repeat what is
  typed.
- **Stale answers:** every completion request has a number (`comp_seq`); only the newest is shown.
  An answer is also dropped when another file is active, the cursor left the line or the word
  (anything but word characters between the word start and the cursor), or a picker opened. Esc bumps
  the number, so an answer on its way doesn't reopen the list.
- **Keys** follow IDEA and stay as they were: ↑↓ choose, **Enter inserts, Tab replaces the rest of
  the word, Esc closes**, typing filters. One change: **Enter on an item that only repeats the typed
  word** (no snippet, no `()`, no import) closes the list and makes the new line, so typing a complete
  name and pressing Enter never surprises.
- **Manual keys:** Ctrl+Space stays; **Ctrl+J** is the fallback that works in every Mac terminal
  without setup (Terminal.app sends 0x0A, crossterm reads it as Ctrl+J in raw mode; nothing in the
  editor used it; IDEA's own Ctrl+J is "insert live template", close in spirit). Ctrl+@ / Ctrl+Shift+2
  works too: most terminals send NUL for it, which crossterm reads as Ctrl+Space; with the kitty
  protocol it arrives as Ctrl+@ or Ctrl+Shift+2, and `KeyCode::Null` is accepted as well. Vim's normal
  mode keeps Ctrl+J (line down). Not chosen: Alt+Space / Ctrl+T (Search Everywhere), Alt+/ (the
  shortcut helper), Ctrl+R (kept for replace), Ctrl+N/P (new file / go to file), Alt+anything (needs
  "Option as Meta" on a Mac).
- **Shortcut helper:** `Ctrl+Space/J  complete` in the editor rows (it replaced `Ctrl+E recent files`,
  which the go-to-file popup still lists) and in Vim INSERT; both keys always show, because macOS
  takes Ctrl+Space whatever the terminal. `FALLBACKS` has Ctrl+Space ← Ctrl+J for the Key Promoter,
  but `MAC_TAKEN` keeps it from telling Mac users to press Ctrl+Space.
- **Word completion** (`src/lsp/words.rs`): words (2–64 word characters, not starting with a digit) of
  the current buffer (first 1 M chars) and the other open buffers (250 k chars each), the word being
  typed and words equal to it left out. Ranking: words that start with the prefix first, nearest to
  the cursor first; then other fuzzy matches by the project's fuzzy scorer (`project::fuzzy`, the one
  Go to File uses), then distance; other files' words after the current file's, with the file name as
  the detail; at most 50. Kind `w`. They are the whole list for files without a running server
  (re-collected per key: the list is marked incomplete), and are **appended under the server's items
  when fewer than 10 of those match** and the prefix isn't empty, deduplicated against the items'
  labels, names (`increment_by` for `increment_by(…)`) and inserted text. Tab replaces the rest of the
  word after the cursor.
- **Setting:** `autocomplete = true|false` in `settings.toml` (default true). False turns off every
  automatic popup (typing and trigger characters); Ctrl+Space / Ctrl+J still work. Read once at start.

## Consequences
- With a server the list may open after one letter; Enter then takes the first item (as in IDEA).
  Someone who wants a plain new line after a prefix presses Esc first (and that word stays quiet).
- rust-analyzer answers `isIncomplete: true`, so typing re-asks it per key (as before for Ctrl+Space);
  the newest-only rule keeps older answers from flickering in.
- Checked on the Mac (2026-10-26): rust-analyzer 1.94.1 (rustup component installed) answers the
  typed popup (`coun` → `counter`, `Counter`, `column!` + word `total_count`), `.` members, no popup in
  a `//` comment; pyright (`comp` → `compile`, `complex`, `compute_total`; `os.pa`) and TypeScript
  (`userAcc` → `userAccount`, `.dis` → `displayName`) too.
- Not done: completion item resolve (docs in the list), a delay or minimum length setting, word
  completion from files that aren't open, camel-hump prefix rules beyond the fuzzy scorer.
