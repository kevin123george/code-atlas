# 0010 – Undo/redo on a Mac, XML highlighting

Date: 2026-10-26 · Status: accepted (aa-app-dev, auto mode; astra on the Mac: "undo redo is missing i
think and xml highlight is missing")

## Context
Undo/redo existed (Ctrl+Z, Ctrl+Shift+Z, Alt+Z) but on a Mac it looked missing:
- **Cmd+Z never reached the editor.** Ghostty 1.3 (astra's terminal, `ghostty +list-keybinds --default`)
  binds `super+z=undo` / `super+shift+z=redo` to its own undo of closed tabs; iTerm2 and Terminal.app
  use Cmd+Z in their Edit menu. Only a terminal with the kitty keyboard protocol that leaves Cmd alone
  can pass it on (crossterm 0.28 then reports `KeyModifiers::SUPER`; `DISAMBIGUATE_ESCAPE_CODES` is
  enough, no more flags needed).
- **Redo had no working key in Terminal.app / iTerm2 defaults:** without the kitty protocol Ctrl+Shift+Z
  arrives as Ctrl+Z (0x1a, so it undoes once more), and Alt+Z without "Option as Meta" arrives as the
  character `Ω` (US layout; Option+Shift+Z is `¸`).
- The shortcut helper's editor rows didn't list undo/redo at all.
- A real bug: edits with nothing to do (Shift+Tab on an unindented line, Ctrl+Backspace at the start
  of the file, an empty paste, Ctrl+Y on an empty file, an LSP edit with no changes) still pushed an undo
  step. The next Ctrl+Z "did nothing", and the file was marked changed.
XML files (pom.xml, .csproj, .plist, .svg …) had no highlighting and Ctrl+/ said "no line comments".

## Decision
- **Cmd chords** (`src/mackeys.rs`): Cmd+Z / Cmd+Shift+Z / Cmd+S / Cmd+A / Cmd+C / Cmd+X, when they
  arrive with SUPER, act as their Ctrl twins **in the editor only** (in the terminal pane Cmd+C must not
  become Ctrl+C = SIGINT). Cmd+V is not mapped: the terminal pastes the system clipboard itself (a
  bracketed paste), while Ctrl+V pastes the editor's own clipboard. Ghostty users pass Cmd+Z on with
  `keybind = super+z=unbind` and `keybind = super+shift+z=unbind`; catlas never edits a terminal's config.
- **Redo fallbacks:** Alt+Z and now also Alt+Shift+Z (Option as Meta). **`Ω` / `¸` redo only right after
  an undo or redo** (the previous key in the editor was one, and there is something to redo); anywhere
  else they are typed. Someone typing Ω on purpose right after an undo is the only one who loses (the
  redo happens instead; a second press then types it once nothing is left). No Ctrl letter was free for
  redo: Ctrl+Y is IDEA's delete line, Ctrl+R stays free for a later replace.
- **Messages:** "Nothing to undo" / "Nothing to redo" in the status bar, so a key that arrives but has no
  effect is visible (and a Ctrl+Shift+Z that arrived as Ctrl+Z shows "Nothing to undo" at the end).
- **Shortcut helper:** the editor context shows `Ctrl+(Shift+)Z  undo / redo` (kitty) or
  `Ctrl+Z/Alt+Z` (legacy); the label parser learned `(Shift+)` = with and without Shift, so the row fits
  38 columns. It replaces `Alt+←/→ previous / next tab`, which the Key Promoter still teaches on a tab
  click.
- **No-op edits:** they return before `Doc::begin` (no step, file not dirty), and `undo`/`redo` skip any
  step whose text equals the current text, so one press always shows a change.
- **XML** (`tree-sitter-xml` 0.7.0, its XML and DTD grammars and bundled queries): `Lang::Xml` and
  `Lang::Dtd`. Extensions in `languages.toml`: xml, xsd, xsl, xslt, svg, plist, pom, wsdl, xliff, xlf, rss,
  atom, xaml, csproj, fsproj, vbproj, vcxproj, proj, props, targets, nuspec, resx, iml, fxml, xib,
  storyboard, kml, gpx, `*.xml.dist`; DTD: dtd, ent (not `mod`: go.mod). A small query in front of the
  bundled one colours attribute names like HTML's (`@attribute`), quotes as strings, and the CDATA
  brackets as punctuation (the bundled query calls them headings).
- **Block comments:** `Lang::block_comment()` for languages without line comments: XML, DTD, HTML,
  Markdown `<!-- -->`, CSS `/* */`. Ctrl+/ wraps each non-blank line (`<!-- line -->` at the smallest
  indent, as IntelliJ does for XML) and unwraps when all are wrapped; already wrapped lines aren't
  wrapped again (XML comments don't nest). A `--` inside a commented line isn't escaped (IntelliJ writes
  `&#45;&#45;`); not done.
- **LemMinX** as the XML/DTD server (`[language-server.lemminx]`, `command = "lemminx"`, no args, version
  from the path). Not installed: it has no Homebrew formula. Install by hand: the native binary from the
  vscode-xml GitHub releases (`lemminx-osx-aarch_64.zip` → `~/.local/bin/lemminx`, `chmod +x`, clear the
  quarantine flag) or the uber jar from Maven Central (`org.eclipse.lemminx:org.eclipse.lemminx:<v>:uber`)
  with a `~/.local/bin/lemminx` wrapper `exec java -jar …/org.eclipse.lemminx-uber.jar "$@"`.

## Consequences
- What works where (Mac): **Terminal.app** undo Ctrl+Z, redo Option+Z right after an undo (or Alt+Z /
  Alt+Shift+Z with "Use Option as Meta key"); **iTerm2** the same, plus Ctrl+Shift+Z with "Report keys
  using CSI u" / the kitty protocol, and Cmd+Z only when mapped in its Keys settings (e.g. to hex `0x1a`);
  **Ghostty / kitty / WezTerm** undo Ctrl+Z, redo Ctrl+Shift+Z (kitty protocol), Cmd+Z / Cmd+Shift+Z where
  the terminal doesn't use them (Ghostty: after the unbind lines). Verified with the bytes those
  terminals send (e2e `raw`), not by pressing keys in each app.
- `tests/e2e/scenarios/xml.sh` reads colours from `tmux capture-pane -e` (SGR replay per character).
- The other agent's work on `src/hints/table.rs` (quick search) only meets this change in the
  `Ctx::Idea` rows.
