# 0007 – More languages and language servers (lsp-more)

Date: 2026-10-25 · Status: accepted (aa-app-dev, full auto mode; astra asked for "language server support", which
covers installing servers on the Mac) · Extends 0004 (replaces its "no snippets" point)

## Context
M3 had a hard-coded server table and was only tried with rust-analyzer. astra's projects use TypeScript (Bun),
Java (Spring Boot), Python, Bash, YAML and JSON, and a user must be able to swap or tune servers without a rebuild.

## Decision
- **Configuration like Helix, field-wise merging.** Built-in `src/lsp/languages.toml` (compiled in), then
  `~/.config/code-atlas/languages.toml` (`$CATLAS_CONFIG_DIR` / `$XDG_CONFIG_HOME` respected, as for agents.json),
  then `<project>/.code-atlas/languages.toml`. `[language.<grammar>]` has `extensions` (also whole file names and
  `*` globs), `roots`, `servers`, `also`; `[language-server.<name>]` has `command`, `args`,
  `initialization-options`, `settings`, `version-args`. Later layers replace fields; `initialization-options` and
  `settings` merge deeply. Unknown fields are errors; a broken file is ignored whole and shown in `--health`.
  Extension clashes: the later layer wins, then file name > glob > extension. File detection reads the same
  table (one source of truth), the `#!` line stays in code.
- **`catlas --health [dir]`** prints per language: highlighting, every configured server, which one is used, path
  and version (`version-args`, 3 s timeout, in parallel; else from the brew Cellar path or the npm package.json).
- **Server per language** (first found wins; one that fails to start is marked dead and the next one is tried):
  TS/JS: typescript-language-server, then `tsc --lsp --stdio` (TypeScript 7's own server: the global TypeScript
  is 7.0.2, which has no JS tsserver, so typescript-language-server can't start without a project-local TS ≤ 6).
  Python: pyright (npm; most used, fast, good types) → basedpyright → pylsp, **plus ruff** as an `also` server.
  Java: jdtls (brew). Kotlin: kotlin-language-server (not installed). Go: gopls (brew). C/C++: clangd (Xcode
  CLT). Bash: bash-language-server (+ shellcheck). Lua: lua-language-server. JSON/HTML/CSS:
  vscode-langservers-extracted. YAML: yaml-language-server (schemaStore on, as in VS Code). TOML: taplo, Markdown:
  marksman (not installed). Dockerfile: docker-langserver. SQL: highlighting only (no server crate/default worth it).
- **Grammars:** tree-sitter-go, -c, -cpp (C++ query + C query, as Helix/Neovim do), -kotlin-sg (kotlin-ng ships no
  highlight query; fwcd's crate pins tree-sitter < 0.23), -yaml, -html, -css, -lua, -sequel (SQL),
  -containerfile (Dockerfile). Every grammar's bundled query compiles against tree-sitter 0.27 (unit test).
- **Several servers per file (cheap, so done):** didOpen/didChange/didSave/didClose go to all; diagnostics are kept
  per server and merged (source shown as `[ruff]` when it isn't the server's own name); code actions are asked from
  all and merged into one list; everything else goes to the main server.
- **Pull diagnostics** (LSP 3.17 `textDocument/diagnostic`) for servers with `diagnosticProvider` (TypeScript 7),
  after open/change/save and on `workspace/diagnostic/refresh`.
- **Snippets on:** `snippetSupport: true`; own parser for `$n`, `${n:x}` (nested), `${n|a,b|}`, `$0`, variables
  (default text only), escapes. Placeholders are selected in order, Tab / Shift+Tab walk them, the stops follow
  typing inside the current one, an edit elsewhere or Esc ends it. Functions/methods without a `(` get `()` with the
  cursor inside (not when a `(` follows). Mirrored stops are not linked. Enter inserts, Tab replaces (IDEA).
- **Keys (IntelliJ):** Alt+F7 usages, Ctrl+F12 file structure, Ctrl+Alt+Shift+N symbols (Alt+N fallback; terminals
  without the kitty protocol send Ctrl+Alt+N, also accepted), Ctrl+Alt+B implementations, F2 / Shift+F2 next /
  previous problem, Alt+6 problems list; signature help opens on the server's trigger characters (`(` `,`).
  Lists are one centred picker type with a filter line (local fuzzy filter; workspace symbols re-ask the server
  on every key, stale answers dropped by sequence number).
- Requests the server can't do (no capability) aren't sent: "<server> has no go to definition".
- Init options/settings may use `${root}`, `${home}`, `${server-node-modules}` (to point a server at the global
  TypeScript next to it).
- Installed on the Mac: npm -g (Homebrew node) typescript, typescript-language-server, pyright,
  bash-language-server, yaml-language-server, vscode-langservers-extracted, dockerfile-language-server-nodejs;
  brew jdtls (pulled in brew openjdk), gopls, lua-language-server, shellcheck, ruff. Java 21 was already there
  (`/usr/libexec/java_home -V`: 17, 21, 23, 24, 25), so no `openjdk@21`.

## Consequences
- yaml/json servers may fetch schemas from the web for known files or `$schema` URLs; switch off per user file.
- A language without a built-in grammar can't be added from the config yet (needs a `Lang` variant + crate).
- Quick fixes from extra servers work; hover, completion, rename etc. stay with the main server.
- The e2e suite tests LSP features against a fake Perl server (`tests/e2e/fake-lsp.pl`), so it stays fast and
  needs no real server; real servers were checked by hand in tmux (see AGENTS.md Current state).
