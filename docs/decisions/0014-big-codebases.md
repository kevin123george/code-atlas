# 0014 – Huge codebases: a persistent code index, symbols without LSP, large-file mode

Date: 2026-10-31 · Status: accepted (aa-app-dev, auto mode; astra asked to make the editor "more futuristic
and advanced to work with agents and huge code bases and multiple repos at once"; track B of three, next to
0013 multi-repo workspaces and 0015 parallel agents)

## Context
Every start walked the whole project for Go to File, and every Find in Files / Search Everywhere query walked
and read every file again (3.5 s per query on 200 000 files). Alt+N needed a running language server. Opening
a 40 MB file or one with a 200 000-char line did O(file) work per frame or per keystroke, and the git
gutter compared the whole rope every tick.

## Decision
- **`src/index/`, one index for a set of roots.** `Index` is a cheap handle; `add_root` / `remove_root` at any
  time (track A adds one root per repository; queries take one root or all of them). Each root has a worker
  thread: load the cache → publish → check the disk → index what changed → watch → compact. Queries read the
  last published immutable `View` and never wait for the worker.
- **Content index = trigram posting lists** (the design of Google Code Search / zoekt, without positions):
  per file the distinct 3-byte windows, ASCII folded to lower case, windows across a line break dropped
  (searches are single-line). A query's trigrams are intersected (shortest list first) to get candidate files,
  and **every candidate is checked against its real contents** with the existing `search_bytes` (smart case,
  same columns and snippets), so results are exactly the old search's — the tests compare them with the
  brute-force walk. Why trigrams: substring search with any characters (`x +`, `a\tb`, `ünïc`) needs no
  tokeniser, the index is ~0.5× the source size, and verification makes false positives harmless. Rejected:
  a word/token index (misses substrings and punctuation), suffix arrays (memory), positional n-grams
  (2–3× bigger for little gain once candidates are verified). Queries under 3 bytes check every indexed file
  (still no walk).
- **On disk, log structured.** A root's cache folder (`<cache>/index/<name>-<fnv64 of the path>/`) holds
  immutable segments (`seg-N.cidx`: paths, mtime + size + flags per file, symbols, postings as varint deltas,
  sorted trigram table) and `manifest.json`. Opening a segment reads everything but the postings; posting lists
  are read with `pread` per query, so a big index costs page cache, not heap. An update writes a small new
  segment (the newest entry of a path wins; deletions are tombstones). More than 6 segments → compaction:
  the small newer ones into one when the oldest holds ≥ 75 % of the entries, else everything (tombstones and
  dead entries dropped); the merge streams one posting list at a time. Writes go through `.tmp` + rename;
  unknown or broken files → rebuild. A second catlas on the same folder sees the lock (`File::try_lock`)
  and runs **read only** (changed files are read directly instead).
- **Cache folder:** `$CATLAS_CACHE_DIR`, else `~/Library/Caches/code-atlas` (macOS),
  `$XDG_CACHE_HOME/code-atlas` or `~/.cache/code-atlas`. A cache inside a root is never indexed. e2e
  points it at `$SCRATCH/bin/.catlas-cache`; unit tests make their own `Index` with scratch folders
  (`Index::from_settings` is `None` under `cfg(test)`).
- **Freshness:** at start a parallel walk with the tree's ignore rules (`.gitignore` also outside git, `.git`,
  `target`, `node_modules` always skipped) compares mtime + size with the index; changed files are
  `pending` (queries read them directly) until they're in a segment. While running, **`notify` 8.2**
  (FSEvents on macOS, inotify on Linux; the one new crate) reports changes; events under `.git`, `target`,
  `node_modules` and the cache are dropped at once, bursts are gathered (120 ms quiet, ≤ 2 s), and the worker
  re-lists the nearest known folders (walking from a known folder keeps gitignore rules right; new folders
  are walked, vanished ones drop their files). A watcher error or "need rescan" → full check. Files over
  `max_file_kb` (4 MB, as Find in Files before) and binaries (NUL in the first 8 KB) are indexed by name only.
- **Memory bounds:** building flushes a segment whenever a thread's share of `memory_mb` (256 MB) fills;
  the trigram scratch set is a 2 MB bitset per thread; symbols ≤ 2 000 per file, files ≤ 1 MB, names ≤ 120
  chars; `max_files` (1 000 000) caps the listing.
- **Using it:** Go to File / speed search get the index's sorted file list (it follows the disk: new files
  appear without a restart); Find in Files and Search Everywhere ask `Index::find` and fall back to the old
  walk while a root's content index isn't ready. **Ranked results:** whole word, exact case, a definition of
  the query on that line (from the symbol table), the query in the file name, recently opened files rank up;
  test / vendor / build / minified files and deep paths down. Search Everywhere ranks single lines (one row
  per line); Find in Files orders files by their best line and keeps each file's hits in line order. Hits come
  as one ranked batch, or a first batch after 250 ms and the rest below it, so rows don't jump.
- **Symbols without LSP:** our own definition-only tree-sitter queries (functions, methods, classes,
  structs, enums, interfaces/traits, types, modules, macros, constants) for Rust, TS/TSX, JS, Java, Kotlin,
  Python, Go, C, C++, Lua, Bash. Search: letter masks (all letters / word-start letters of each name) reject
  most names without touching them, then the finder's fuzzy scorer; exact and prefix names, types first.
  Search Everywhere gains a **SYMBOLS** section (6 rows in All) and a **Symbols** tab (All → Files → Symbols →
  Text); **Alt+N / Ctrl+Alt+Shift+N** opens at once without a server; with one, its answer comes first and
  index rows at places it didn't name are added below (`Picker::merge`).
- **Large-file mode** (`large_file_mb = 20` or a line of 20 000+ bytes): no tree-sitter parse, no language
  server (full-text sync per edit), no git gutter; the status bar says so on open; editing works. Drawing stops
  at the right edge of each line (it walked whole lines before). The git gutter compares (revision, length)
  instead of whole ropes every tick. The tree sorts folder listings with cached lowercase keys (100 000
  entries: 76–91 ms, rows are already drawn virtually). The fuzzy scorer rejects non-matching ASCII paths
  before allocating.
- **Workspaces (0013):** the project adds one index root per repository and drops it when the repository
  leaves the workspace (`set_workspace`); the file list is the roots' lists with `label/` in front; find in
  files over all roots (`Index::find_multi`) or one (`@repo`, a scope folder); symbols over all roots.
- **Settings** (`settings.toml`): `large_file_mb = 20`, `[index]` `enabled`, `max_file_kb`, `max_files`,
  `symbols`, `watch`, `memory_mb`. **CLI:** `catlas --index-status [dir]` (cache folder, files, symbols, size,
  age, what changed since, all cached roots; read only), `catlas --bench-index <dir> [--files N] [--keep]`.
  Status bar: `⟳ Indexing 45%` / `Loading index` / `Scanning files` / `Compacting index` while busy.

## Benchmarks
Mac (Apple Silicon, 10 cores, 16 GB), release build, `catlas --bench-index`. Synthetic repo: 200 000 files,
644 MB, mixed Rust / TS / Python / Go / Java / Markdown / JSON, ~20 files per folder (generated in `$TMPDIR`,
deleted afterwards). Its vocabulary is 30 syllables, so nearly every trigram is common: a worst case for
trigram selectivity.

| measure | 200 000 synthetic files | `~/Projects` (9 012 files, read only) |
|---|---|---|
| file list at start, before (walk) | 195 ms (every start) | 58 ms |
| cold index: file list ready | 316 ms | 53 ms |
| cold index: contents + symbols | 12.8 s (4.1 M symbols, 340 MB on disk, 1 segment) | 1.1 s (31 k symbols, 11.6 MB) |
| warm start: searchable | **260 ms** | **5 ms** |
| warm start: disk check (mtime + size) done | 603 ms | 65 ms |
| find in files, rare string, before (walk + read all) | 3 453 ms | 114 ms |
| find in files, rare string (index) p50 / p95 | **12.5 / 912 ms** (p95 = a no-hit word made of common syllables) | 12.6 / 12.6 ms |
| find in files, identifiers p50 / p95 | 192 / 736 ms | 12.6 / 25.9 ms |
| find in files, common words (2 000-hit cap) p50 / p95 | 47 / 50 ms | 12.8 / 13.7 ms |
| symbols (top 40) p50 / p95 | 18 / 45 ms | 0.1 / 0.3 ms |
| Go to File: fuzzy over all paths p50 / p95 | 23 / 96 ms | 1 / 1 ms |
| incremental: 10 edited files → searchable | 158 ms | (not written to) |
| RSS with the index open after the cold build | 745 MB (build buffers kept by the allocator) | 122 MB |
| RSS warm, fresh process | 343 MB after queries + disk check | 19 MB loaded, 93 MB after queries |
| tree: expand a folder of 100 000 files | 91 ms | 76 ms |
| 40 MB file + a 200 000-char line | open 50 ms, keystroke + draw p50 0.3 ms / max 1 ms | same |

## Consequences
- The first open of a new huge repo indexes in the background (≈ 15 k files/s here); search works meanwhile
  via the old walk.
- The synthetic repo is symbol-dense (≈ 20 per file): symbol tables are most of the warm memory (≈ 28 B per
  symbol plus names). Real code has far fewer.
- inotify needs one watch per folder: on Linux, very big trees can hit `max_user_watches`; the scan at start
  still catches up. Two catlas on one folder: the second is read only and reads changed files directly.
- Symbol columns are char columns; Alt+N converts them as UTF-16 positions, which is off by a little on
  lines with non-BMP characters before the name.
- Not done: ranking by git recency, regex search over the index, symbol references (only definitions), a
  per-file symbol cap visible in the UI, inline highlighting of the visible part of large files.
