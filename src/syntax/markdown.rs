//! What Markdown needs beyond its block grammar (decision 0019): inline styling from the grammar's
//! second, inline part, and code blocks coloured by their own language. Only visible blocks are
//! parsed, so a keystroke in a long file costs no more than one in a short one.

use std::cell::RefCell;
use std::ops::Range as StdRange;
use std::sync::OnceLock;

use tree_sitter::{Node, Parser, Query, QueryCursor, Range, StreamingIterator, Tree};

use super::{token_for_capture, Lang, Token};

/// Bigger code blocks stay one colour rather than slow typing down.
const MAX_INJECTED_BYTES: usize = 256 * 1024;

pub const BLOCK_EXTRA: &str = r#"
(block_quote) @text.quote
(block_quote_marker) @keyword
[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
] @keyword
(task_list_marker_checked) @string.escape
(task_list_marker_unchecked) @keyword
(thematic_break) @punctuation.delimiter
(pipe_table_header (pipe_table_cell) @text.strong)
(pipe_table_delimiter_row) @punctuation.delimiter
(pipe_table_header "|" @punctuation.delimiter)
(pipe_table_row "|" @punctuation.delimiter)
(info_string) @label
(minus_metadata) @comment
(plus_metadata) @comment
(html_block) @tag
"#;

pub const INLINE_EXTRA: &str = r#"
(strikethrough) @text.strike
(html_tag) @tag
(email_autolink) @text.uri
(code_span) @text.literal
(code_span_delimiter) @punctuation.delimiter
"#;

struct Inline {
    query: Query,
    tokens: Vec<Option<Token>>,
}

fn inline() -> Option<&'static Inline> {
    static CFG: OnceLock<Option<Inline>> = OnceLock::new();
    CFG.get_or_init(|| {
        let language = tree_sitter_md::INLINE_LANGUAGE.into();
        let text = format!("{INLINE_EXTRA}\n{}", tree_sitter_md::HIGHLIGHT_QUERY_INLINE);
        let query = Query::new(&language, &text).ok()?;
        let tokens = query.capture_names().iter().map(|n| token_for_capture(n)).collect();
        Some(Inline { query, tokens })
    })
    .as_ref()
}

thread_local! {
    static INLINE_PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

/// The language a code block's info string names (` ```java `, ` ```sh title="x" `).
pub fn fence_lang(info: &str) -> Lang {
    let word = info.trim().trim_start_matches('{').trim_start_matches('.');
    let word = word.split(|c: char| c.is_whitespace() || c == ',' || c == '}').next().unwrap_or("").to_ascii_lowercase();
    match word.as_str() {
        "java" => Lang::Java,
        "sh" | "bash" | "shell" | "zsh" | "console" | "shell-session" => Lang::Bash,
        "json" | "jsonc" | "json5" => Lang::Json,
        "yaml" | "yml" => Lang::Yaml,
        "toml" => Lang::Toml,
        "xml" | "pom" | "svg" => Lang::Xml,
        "html" => Lang::Html,
        "css" => Lang::Css,
        "js" | "javascript" | "mjs" | "cjs" => Lang::JavaScript,
        "jsx" | "tsx" => Lang::Tsx,
        "ts" | "typescript" => Lang::TypeScript,
        "rust" | "rs" => Lang::Rust,
        "python" | "py" => Lang::Python,
        "go" | "golang" => Lang::Go,
        "kotlin" | "kt" => Lang::Kotlin,
        "c" | "h" => Lang::C,
        "cpp" | "c++" | "cc" | "hpp" => Lang::Cpp,
        "lua" => Lang::Lua,
        "sql" => Lang::Sql,
        "dockerfile" | "docker" | "containerfile" => Lang::Dockerfile,
        _ => Lang::Plain,
    }
}

/// `node`'s range without its named children: a paragraph in a quote or list has `> ` continuations
/// in it. Its anonymous children are the text's own punctuation (`*`, `{`), which must stay.
fn ranges_without_children(node: Node) -> Vec<Range> {
    let mut out = Vec::new();
    let (mut byte, mut point) = (node.start_byte(), node.start_position());
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.start_byte() > byte {
            out.push(Range { start_byte: byte, end_byte: child.start_byte(), start_point: point, end_point: child.start_position() });
        }
        byte = child.end_byte().max(byte);
        point = child.end_position();
    }
    if node.end_byte() > byte {
        out.push(Range { start_byte: byte, end_byte: node.end_byte(), start_point: point, end_point: node.end_position() });
    }
    out
}

/// Spans to paint over the block grammar's, in paint order. `None` clears a span's colour.
pub fn extras(source: &str, tree: &Tree, window: StdRange<usize>) -> Vec<(usize, usize, Option<Token>)> {
    let mut out = Vec::new();
    visit(tree.root_node(), source, &window, &mut out);
    out
}

fn visit(node: Node, source: &str, window: &StdRange<usize>, out: &mut Vec<(usize, usize, Option<Token>)>) {
    if node.end_byte() <= window.start || node.start_byte() >= window.end {
        return;
    }
    match node.kind() {
        "inline" => inline_spans(node, source, window, out),
        "fenced_code_block" => fence_spans(node, source, window, out),
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                visit(child, source, window, out);
            }
        }
    }
}

fn inline_spans(node: Node, source: &str, window: &StdRange<usize>, out: &mut Vec<(usize, usize, Option<Token>)>) {
    let Some(cfg) = inline() else { return };
    let ranges = ranges_without_children(node);
    if ranges.is_empty() {
        return;
    }
    let tree = INLINE_PARSER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let mut p = Parser::new();
            p.set_language(&tree_sitter_md::INLINE_LANGUAGE.into()).ok()?;
            *slot = Some(p);
        }
        let parser = slot.as_mut()?;
        parser.set_included_ranges(&ranges).ok()?;
        parser.parse(source, None)
    });
    let Some(tree) = tree else { return };
    collect(&cfg.query, &cfg.tokens, &tree, source, window, out);
}

fn fence_spans(node: Node, source: &str, window: &StdRange<usize>, out: &mut Vec<(usize, usize, Option<Token>)>) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let info = children.iter().find(|c| c.kind() == "info_string").and_then(|c| source.get(c.byte_range())).unwrap_or("");
    let Some(code) = children.iter().find(|c| c.kind() == "code_fence_content") else { return };
    let lang = fence_lang(info);
    // A block without a known language stays one colour, the block grammar's.
    let Some(cfg) = lang.config().filter(|_| code.byte_range().len() <= MAX_INJECTED_BYTES) else { return };
    let mut parser = Parser::new();
    if parser.set_language(&cfg.language).is_err() || parser.set_included_ranges(&ranges_without_children(*code)).is_err() {
        return;
    }
    let Some(tree) = parser.parse(source, None) else { return };
    out.push((code.start_byte(), code.end_byte(), None));
    collect(&cfg.query, &cfg.tokens, &tree, source, window, out);
}

fn collect(query: &Query, tokens: &[Option<Token>], tree: &Tree, source: &str, window: &StdRange<usize>, out: &mut Vec<(usize, usize, Option<Token>)>) {
    let mut spans: Vec<(usize, usize, usize, Token)> = Vec::new();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(window.clone());
    let mut caps = cursor.captures(query, tree.root_node(), source.as_bytes());
    while let Some((m, i)) = caps.next() {
        let c = m.captures()[*i];
        if let Some(tok) = tokens[c.index as usize] {
            spans.push((c.node.start_byte(), c.node.end_byte(), m.pattern_index, super::refine(tok, c.node.kind())));
        }
    }
    spans.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)).then(b.2.cmp(&a.2)));
    out.extend(spans.into_iter().map(|(s, e, _, t)| (s, e, Some(t))));
}
