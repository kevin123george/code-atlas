# 0019: Excellent support for the daily languages

Date: 2026-11-01. Status: first pass done.

## Context
astra: "what er want is exelect support for md file java and yml and json etc and sh". An audit with rendered samples and the real servers
showed Java close to good, Markdown weakest (no inline styling, code blocks one colour), YAML keys and values in one colour, shell variables hard to see,
and Enter and Tab unaware of the language.

## Decision
- **Markdown:** parse the grammar's inline part for visible lines only, and colour code blocks with their own language's grammar, so typing cost doesn't grow with the file.
- **Data keys get their own colour** (`Token::Key`) in YAML and JSON, where most of the file is keys and values.
- **Shell variables stand out**, also inside strings, as that is where quoting mistakes hide.
- **Enter follows the language** (`syntax/enter.rs`). Closers come back on Enter, not while typing: dedenting at `fi` would also hit a word like `file`.
- **Indent width follows the file** when it uses 2, 3, 4 or 8, else the language's habit. Other amounts are treated as misaligned, so formatting can repair them.
- **Ctrl+Alt+O organizes imports**, IDEA's key, through the server. Old Java UI and internal packages rank last in completion.

## Consequences
- marksman and shfmt are not installed yet, as installing needs astra's OK. Until then Markdown has no outline or link checks, and shell has no formatting.
