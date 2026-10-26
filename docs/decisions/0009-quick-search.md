# 0009 – Quick search (Search Everywhere) and the tree

Date: 2026-10-26 · Status: accepted (aa-app-dev, auto mode; astra asked for "quick search that is either file
name or content and as well it should work really well with tree")

## Context
Code Atlas had Go to File (Ctrl+Shift+N / Ctrl+P) and Find in Files (Ctrl+Shift+F / Alt+F) as two popups, and a
tree that only moved by arrow keys. IDEA's Search Everywhere (Shift Shift) finds names and text in one place,
and its project view has speed search and "autoscroll to source". astra works on a Mac.

## Decision
- **One popup, two sections** (`src/project/quick.rs`, `PopupKind::Quick`): **FILES** (the finder's fuzzy
  scorer, same ranking, recent files on an empty input, `name:42[:7]` jumps) then **TEXT** (`path:line  snippet`,
  match in the accent), each with a header and a count. File names are ranked at once; contents come from the
  find-in-files job (background threads, 150 ms debounce, smart case, at least 2 characters, a new query drops
  the old job). Hits stream in *below* the file rows, so the selection never jumps while you look. Text rows
  are one per line (the first match on it); Find in Files keeps one row per match.
- **Tabs, not prefixes:** Tab / Shift+Tab cycle **All → Files → Text**. "All" shows the 8 best file names
  ("8 of 120 · Tab for all"), "Files" all of them, "Text" only content.
- **Keys** (IDEA's Shift Shift needs key-release events of a lone Shift; terminals don't send those, and with
  the kitty protocol they'd need `REPORT_ALL_KEYS_AS_ESCAPE_CODES`, which changes how every key arrives —
  not worth the risk here):
  - **Ctrl+T** is the main key. Every terminal sends it (Terminal.app and iTerm2 without any setup, tmux, Linux
    terminals); nothing in Code Atlas used it (checked: app, project, LSP, agents, dock, Vim). It isn't
    Ctrl+Space (completion, and macOS input-source switching) and not Cmd+anything (never reaches a terminal).
  - **Alt+S** and **Alt+Space** too, where Option/Alt is Meta (Ghostty `macos-option-as-alt`, iTerm2/Terminal.app
    "Use Option as Meta key", Linux). Alt+Space may be taken by KDE (KRunner), hence not the main key.
  - **Option+Space without Meta** types U+00A0 (no-break space) on a Mac; catlas reads that character as the
    key too (nobody means to type it into code). Option+S without Meta types `ß`, a real letter on German
    keyboards, so that one is **not** treated as a key.
  - The kitty protocol doesn't add a different key: Ctrl+T is already the IDEA-ish one-chord key everywhere.
- **Scope from the tree:** Ctrl+T with the tree focused searches the selected folder (or the selected file's
  folder; the root = whole project): title `Search Everywhere · in src/project/`, both sections filtered, the
  content walk starts in that folder (still gitignore-aware) and paths stay relative to the root.
  Backspace on an empty input, or the key again, widens to the whole project. From another popup the key
  switches to Search Everywhere and takes its input along.
- **Tree follows the selection** (all project popups: Go to File, Recent, Find in Files, Search Everywhere),
  when the tree is shown: it expands down to the selected row's file and selects it, without taking the focus.
  Each step starts from the tree as it was when the popup opened (a snapshot), so browsing doesn't leave
  folders open; **Esc puts the tree back**, opening a file keeps it revealed. The popup is centred in the area
  right of the sidebar (when 64+ columns are left) so the tree stays visible. The tree is not opened if hidden
  (it would shift the layout under the popup); **Alt+F1** in a popup reveals the row in the tree and focuses it.
- **Speed search in the focused tree** (IDEA): typing letters (not Space) jumps to the open row whose *name*
  matches best (the finder's fuzzy rule); matching names get their matched letters in the accent (bold,
  underlined), others dim; the header shows `Search: …` (red when nothing matches). ↑↓ step through matches
  (wrapping), Backspace narrows back, Esc clears (a second Esc leaves the tree), Enter opens. When no open row
  matches, the best file *name* from the index is revealed (folders expand), so it finds closed files too.
  Tree keys (←→ Home End PgUp PgDn Enter F4) and chords end it; other keys leave it alone.
- **Shortcut helper:** two new contexts (22): `search everywhere` and `tree · speed search`; the tree rows gain
  `a-z speed search` and `Ctrl+T search in folder`; the editor rows gain `Ctrl+T search everywhere`
  (`Alt+Enter quick fix` left the editor panel to make room — it still works, and LSP popups list it).

## Consequences
- Content hits arrive in walk order (grouped by file), not ranked; ranking them would move rows under the
  selection while they stream.
- The tree follow re-lists the folders on the path for each selection change (small `read_dir`s on the main
  thread, as Alt+F1 always did).
- No double-Shift until the kitty flags are revisited (M7 settings could make it opt-in).
