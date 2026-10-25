# 0003 – Code smarts (M3): highlighting and LSP

Date: 2026-10-25 · Status: accepted (aa-app-dev, full auto mode given by astra)

## Context
M3 adds syntax highlighting, line comments and an LSP client while other agents change `doc.rs`/`app.rs` in
parallel, so the edit commands must not each learn to report what they changed.

## Decision
- **Own tree-sitter query cursor**, not `tree-sitter-highlight`: that crate reparses from scratch and can't
  reuse a tree. Each `Doc` owns a `syntax::Syntax`; on a new revision it **diffs old and new text** (common
  prefix/suffix → one `InputEdit`) and reparses incrementally. Only visible lines are queried
  (`set_byte_range`), cached per (revision, first line). Bigger nodes paint first, inner ones win; for one node
  the earliest pattern wins. Measured on 5,000+ lines: ~1 ms per keystroke (release).
- Grammars' **bundled highlight queries**; TS/TSX = TypeScript + JavaScript (+ JSX) queries. Capture names map to
  a small `Token` set coloured from the Ledger Terminal series palette (keywords teal, strings aqua, comments
  muted italic, numbers orange, types yellow, functions blue, constants violet, macros/attributes magenta).
- **Ctrl+/** follows IntelliJ: all non-blank lines commented → uncomment, else `token ` at the smallest indent;
  without a selection the cursor moves down. One undo step via `Doc::edit_with`.
- **LSP:** own JSON-RPC over stdio (writer, reader, stderr threads; `mpsc` to the UI), typed results via
  `lsp-types`. One server per (command, project root); root = nearest language marker (Cargo.toml,
  package.json/tsconfig, pyproject.toml, pom.xml/build.gradle), else `.git`, else the file's folder.
  **Full-text sync** after 150 ms without typing (and at once before any request). UTF-16 positions.
  `snippetSupport: false` (snippets that still come are stripped to plain text).
- **Hover is F1** (Ctrl+Q quits in Code Atlas); back from a definition is Ctrl+Alt+← (IDEA's "Back").
- A server that is missing is skipped with one status message per language; one that dies isn't restarted.
- Workspace edits to files that aren't open open them as unsaved tabs (no silent writes to disk).

## Consequences
- Full sync sends the whole file on each pause; fine for normal files, revisit (incremental sync) if big files lag.
- Markdown gets block highlighting only until injections are added.
