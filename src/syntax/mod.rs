//! Syntax: language detection, tree-sitter highlighting and line comments.
//!
//! Each [`Doc`](crate::doc::Doc) owns a [`Syntax`]. On every new document revision it diffs the
//! old and new text (common prefix and suffix), tells tree-sitter about that one edit and reparses
//! incrementally, so no edit command needs to report what it changed. Only the lines on screen are
//! queried, and the result is cached until the revision or the scroll position changes.

pub mod comment;

use std::path::Path;
use std::sync::OnceLock;

use ropey::Rope;
use tree_sitter::{InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

/// Languages Code Atlas knows. `Plain` gets no highlighting.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Java,
    Python,
    Json,
    Toml,
    Markdown,
    Bash,
    Plain,
}

impl Lang {
    /// By file name and extension, then by a `#!` line.
    pub fn detect(path: Option<&Path>, first_line: &str) -> Lang {
        if let Some(p) = path {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let by_name = match name.as_str() {
                "cargo.lock" | "pipfile" => Some(Lang::Toml),
                ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" | ".zprofile" | "pkgbuild" => Some(Lang::Bash),
                _ => None,
            };
            if let Some(l) = by_name {
                return l;
            }
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let by_ext = match ext.as_str() {
                "rs" => Lang::Rust,
                "ts" | "mts" | "cts" => Lang::TypeScript,
                "tsx" => Lang::Tsx,
                "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
                "java" => Lang::Java,
                "py" | "pyi" | "pyw" => Lang::Python,
                "json" | "jsonc" | "json5" => Lang::Json,
                "toml" => Lang::Toml,
                "md" | "markdown" => Lang::Markdown,
                "sh" | "bash" | "zsh" | "ksh" => Lang::Bash,
                _ => Lang::Plain,
            };
            if by_ext != Lang::Plain {
                return by_ext;
            }
        }
        if let Some(rest) = first_line.strip_prefix("#!") {
            if rest.contains("python") {
                return Lang::Python;
            }
            if ["bash", "/sh", " sh", "zsh", "ksh"].iter().any(|s| rest.contains(s)) {
                return Lang::Bash;
            }
            if rest.contains("node") || rest.contains("deno") || rest.contains("bun") {
                return Lang::JavaScript;
            }
        }
        Lang::Plain
    }

    /// The name shown in the status bar.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "Rust",
            Lang::TypeScript => "TypeScript",
            Lang::Tsx => "TSX",
            Lang::JavaScript => "JavaScript",
            Lang::Java => "Java",
            Lang::Python => "Python",
            Lang::Json => "JSON",
            Lang::Toml => "TOML",
            Lang::Markdown => "Markdown",
            Lang::Bash => "Shell",
            Lang::Plain => "Text",
        }
    }

    /// Token that starts a line comment, if the language has one.
    pub fn line_comment(self) -> Option<&'static str> {
        match self {
            Lang::Rust | Lang::TypeScript | Lang::Tsx | Lang::JavaScript | Lang::Java => Some("//"),
            Lang::Python | Lang::Toml | Lang::Bash => Some("#"),
            Lang::Json | Lang::Markdown | Lang::Plain => None,
        }
    }

    /// The LSP `languageId`.
    pub fn lsp_id(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "typescriptreact",
            Lang::JavaScript => "javascript",
            Lang::Java => "java",
            Lang::Python => "python",
            Lang::Json => "json",
            Lang::Toml => "toml",
            Lang::Markdown => "markdown",
            Lang::Bash => "shellscript",
            Lang::Plain => "plaintext",
        }
    }

    fn config(self) -> Option<&'static Config> {
        static CONFIGS: [OnceLock<Option<Config>>; 10] = [const { OnceLock::new() }; 10];
        let (i, lang, queries): (usize, Language, Vec<&str>) = match self {
            Lang::Rust => (0, tree_sitter_rust::LANGUAGE.into(), vec![tree_sitter_rust::HIGHLIGHTS_QUERY]),
            Lang::TypeScript => (
                1,
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                vec![tree_sitter_typescript::HIGHLIGHTS_QUERY, tree_sitter_javascript::HIGHLIGHT_QUERY],
            ),
            Lang::Tsx => (
                2,
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                vec![
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                ],
            ),
            Lang::JavaScript => (
                3,
                tree_sitter_javascript::LANGUAGE.into(),
                vec![tree_sitter_javascript::JSX_HIGHLIGHT_QUERY, tree_sitter_javascript::HIGHLIGHT_QUERY],
            ),
            Lang::Java => (4, tree_sitter_java::LANGUAGE.into(), vec![tree_sitter_java::HIGHLIGHTS_QUERY]),
            Lang::Python => (5, tree_sitter_python::LANGUAGE.into(), vec![tree_sitter_python::HIGHLIGHTS_QUERY]),
            Lang::Json => (6, tree_sitter_json::LANGUAGE.into(), vec![tree_sitter_json::HIGHLIGHTS_QUERY]),
            Lang::Toml => (7, tree_sitter_toml_ng::LANGUAGE.into(), vec![tree_sitter_toml_ng::HIGHLIGHTS_QUERY]),
            Lang::Markdown => (8, tree_sitter_md::LANGUAGE.into(), vec![tree_sitter_md::HIGHLIGHT_QUERY_BLOCK]),
            Lang::Bash => (9, tree_sitter_bash::LANGUAGE.into(), vec![tree_sitter_bash::HIGHLIGHT_QUERY]),
            Lang::Plain => return None,
        };
        CONFIGS[i].get_or_init(|| Config::new(lang, &queries)).as_ref()
    }
}

/// What a piece of code is, as far as colouring goes. `ui::theme::token_style` gives the colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Token {
    Keyword,
    String,
    Escape,
    Comment,
    Number,
    Constant,
    Type,
    Function,
    Macro,
    Property,
    Parameter,
    Builtin,
    Attribute,
    Operator,
    Punctuation,
    Tag,
    Label,
    Heading,
    Link,
}

/// Maps a tree-sitter capture name (`function.method`, `string.special.key`, …) to a [`Token`].
/// Unknown names, and ones that mean "leave as is" (`variable`, `embedded`, `none`), give `None`.
pub fn token_for_capture(name: &str) -> Option<Token> {
    let first = name.split('.').next().unwrap_or("");
    Some(match name {
        "string.special.key" => Token::Property,
        "string.escape" | "escape" | "string.special" => Token::Escape,
        "variable.parameter" => Token::Parameter,
        "variable.builtin" | "function.builtin" | "type.builtin" | "constant.builtin" | "boolean" => Token::Builtin,
        "function.macro" => Token::Macro,
        "text.title" | "markup.heading" => Token::Heading,
        "text.uri" | "text.reference" | "markup.link" => Token::Link,
        "text.literal" | "markup.raw" => Token::String,
        "punctuation.special" => Token::Operator,
        _ => match first {
            "keyword" | "conditional" | "repeat" | "include" | "exception" | "storageclass" => Token::Keyword,
            "string" | "character" => Token::String,
            "comment" => Token::Comment,
            "number" | "float" => Token::Number,
            "constant" => Token::Constant,
            "type" | "constructor" | "namespace" | "module" => Token::Type,
            "function" | "method" => Token::Function,
            "property" | "field" => Token::Property,
            "attribute" => Token::Attribute,
            "operator" => Token::Operator,
            "punctuation" => Token::Punctuation,
            "tag" => Token::Tag,
            "label" => Token::Label,
            _ => return None,
        },
    })
}

struct Config {
    language: Language,
    query: Query,
    /// Token per capture index.
    tokens: Vec<Option<Token>>,
}

impl Config {
    /// Joins the query files (the first wins for equal nodes). Drops files that don't compile
    /// against this grammar rather than losing highlighting altogether.
    fn new(language: Language, queries: &[&str]) -> Option<Config> {
        let joined = queries.join("\n");
        let query = Query::new(&language, &joined).ok().or_else(|| {
            queries.iter().find_map(|q| Query::new(&language, q).ok())
        })?;
        let tokens = query.capture_names().iter().map(|n| token_for_capture(n)).collect();
        Some(Config { language, query, tokens })
    }
}

/// Highlighted visible lines: one entry per line, one token per char.
struct Cache {
    rev: u64,
    first: usize,
    lines: Vec<Vec<Option<Token>>>,
}

/// Syntax state of one document.
pub struct Syntax {
    pub lang: Lang,
    parser: Option<Parser>,
    tree: Option<Tree>,
    /// Text of the parsed revision (tree-sitter works on bytes).
    source: String,
    rev: Option<u64>,
    cache: Option<Cache>,
}

impl Syntax {
    pub fn new(lang: Lang) -> Self {
        let parser = lang.config().and_then(|c| {
            let mut p = Parser::new();
            p.set_language(&c.language).ok()?;
            Some(p)
        });
        Syntax { lang, parser, tree: None, source: String::new(), rev: None, cache: None }
    }

    pub fn detect(path: Option<&Path>, rope: &Rope) -> Self {
        let first: String = rope.lines().next().map(|l| l.chars().take(200).collect()).unwrap_or_default();
        Self::new(Lang::detect(path, &first))
    }

    /// Brings the tree up to date with revision `rev` of `rope` (incremental reparse).
    pub fn update(&mut self, rope: &Rope, rev: u64) {
        if self.rev == Some(rev) {
            return;
        }
        self.rev = Some(rev);
        let Some(parser) = self.parser.as_mut() else { return };
        let new = rope.to_string();
        if let Some(tree) = self.tree.as_mut() {
            if let Some(edit) = diff_edit(&self.source, &new) {
                tree.edit(&edit);
            }
        }
        self.tree = parser.parse(&new, self.tree.as_ref());
        self.source = new;
    }

    /// Tokens for lines `first .. first + count` (clamped to the document). Call [`update`](Self::update) first.
    pub fn visible(&mut self, rope: &Rope, first: usize, count: usize) -> &[Vec<Option<Token>>] {
        let rev = self.rev.unwrap_or(0);
        let fresh = self.cache.as_ref().is_some_and(|c| c.rev == rev && c.first == first && c.lines.len() >= count.min(rope.len_lines().saturating_sub(first)));
        if !fresh {
            let lines = self.highlight(rope, first, count);
            self.cache = Some(Cache { rev, first, lines });
        }
        let c = self.cache.as_ref().unwrap();
        &c.lines[..count.min(c.lines.len())]
    }

    fn highlight(&self, rope: &Rope, first: usize, count: usize) -> Vec<Vec<Option<Token>>> {
        let total = rope.len_lines();
        let first = first.min(total);
        let last = (first + count).min(total);
        let mut lines: Vec<Vec<Option<Token>>> =
            (first..last).map(|l| vec![None; rope.line(l).len_chars()]).collect();
        let (Some(cfg), Some(tree)) = (self.lang.config(), self.tree.as_ref()) else { return lines };
        if self.source.len() != rope.len_bytes() {
            return lines;
        }
        let start_byte = rope.line_to_byte(first);
        let end_byte = rope.line_to_byte(last);
        let mut spans: Vec<(usize, usize, usize, Token)> = Vec::new(); // start, end, pattern, token
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);
        let mut caps = cursor.captures(&cfg.query, tree.root_node(), self.source.as_bytes());
        while let Some((m, i)) = caps.next() {
            let c = m.captures()[*i];
            if let Some(tok) = cfg.tokens[c.index as usize] {
                let tok = refine(tok, c.node.kind());
                spans.push((c.node.start_byte(), c.node.end_byte(), m.pattern_index, tok));
            }
        }
        // Paint big nodes first so inner ones win; for the same node the earliest pattern wins.
        spans.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)).then(b.2.cmp(&a.2)));
        let first_char = rope.line_to_char(first);
        let end_char = rope.line_to_char(last);
        let line_starts: Vec<usize> = (first..=last).map(|l| rope.line_to_char(l) - first_char).collect();
        for (s, e, _, tok) in spans {
            let s = rope.byte_to_char(s.clamp(start_byte, end_byte)).max(first_char) - first_char;
            let e = rope.byte_to_char(e.clamp(start_byte, end_byte)).min(end_char) - first_char;
            let mut row = line_starts.partition_point(|&ls| ls <= s).saturating_sub(1);
            let mut p = s;
            while p < e && row < lines.len() {
                let line_end = line_starts[row + 1];
                let to = e.min(line_end);
                for slot in &mut lines[row][p - line_starts[row]..to - line_starts[row]] {
                    *slot = Some(tok);
                }
                p = to;
                row += 1;
            }
        }
        lines
    }
}

/// Some grammars tag numbers as constants (Rust: `@constant.builtin`); colour them as numbers.
fn refine(tok: Token, kind: &str) -> Token {
    let numeric = kind.contains("integer") || kind.contains("float") || kind == "number" || kind.ends_with("_number");
    if numeric && matches!(tok, Token::Builtin | Token::Constant) { Token::Number } else { tok }
}

/// The single edit that turns `old` into `new`: common prefix and suffix kept, the middle replaced.
fn diff_edit(old: &str, new: &str) -> Option<InputEdit> {
    if old == new {
        return None;
    }
    let (o, n) = (old.as_bytes(), new.as_bytes());
    let mut start = o.iter().zip(n).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(start) {
        start -= 1;
    }
    let max_suffix = o.len().min(n.len()) - start;
    let mut suffix = o.iter().rev().zip(n.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(o.len() - suffix) || !new.is_char_boundary(n.len() - suffix) {
        suffix -= 1;
    }
    let old_end = o.len() - suffix;
    let new_end = n.len() - suffix;
    Some(InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_at(o, start),
        old_end_position: point_at(o, old_end),
        new_end_position: point_at(n, new_end),
    })
}

fn point_at(text: &[u8], byte: usize) -> Point {
    let before = &text[..byte];
    let row = before.iter().filter(|&&b| b == b'\n').count();
    let col = byte - before.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
    Point::new(row, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn lang(name: &str) -> Lang {
        Lang::detect(Some(&PathBuf::from(name)), "")
    }

    #[test]
    fn detects_languages_by_extension_and_shebang() {
        assert_eq!(lang("src/doc.rs"), Lang::Rust);
        assert_eq!(lang("a.ts"), Lang::TypeScript);
        assert_eq!(lang("App.tsx"), Lang::Tsx);
        assert_eq!(lang("x.mjs"), Lang::JavaScript);
        assert_eq!(lang("C.jsx"), Lang::JavaScript);
        assert_eq!(lang("Main.java"), Lang::Java);
        assert_eq!(lang("x.py"), Lang::Python);
        assert_eq!(lang("package.json"), Lang::Json);
        assert_eq!(lang("Cargo.toml"), Lang::Toml);
        assert_eq!(lang("Cargo.lock"), Lang::Toml);
        assert_eq!(lang("README.MD"), Lang::Markdown);
        assert_eq!(lang("deploy.sh"), Lang::Bash);
        assert_eq!(lang(".bashrc"), Lang::Bash);
        assert_eq!(lang("notes.txt"), Lang::Plain);
        assert_eq!(Lang::detect(Some(&PathBuf::from("bin/tool")), "#!/usr/bin/env python3"), Lang::Python);
        assert_eq!(Lang::detect(Some(&PathBuf::from("run")), "#!/bin/sh"), Lang::Bash);
        assert_eq!(Lang::detect(None, "plain"), Lang::Plain);
        assert_eq!(Lang::Rust.name(), "Rust");
        assert_eq!(Lang::Python.line_comment(), Some("#"));
        assert_eq!(Lang::Json.line_comment(), None);
    }

    #[test]
    fn capture_names_map_to_tokens() {
        assert_eq!(token_for_capture("keyword"), Some(Token::Keyword));
        assert_eq!(token_for_capture("keyword.return"), Some(Token::Keyword));
        assert_eq!(token_for_capture("string"), Some(Token::String));
        assert_eq!(token_for_capture("string.special.key"), Some(Token::Property));
        assert_eq!(token_for_capture("string.escape"), Some(Token::Escape));
        assert_eq!(token_for_capture("comment.documentation"), Some(Token::Comment));
        assert_eq!(token_for_capture("number"), Some(Token::Number));
        assert_eq!(token_for_capture("type.builtin"), Some(Token::Builtin));
        assert_eq!(token_for_capture("type"), Some(Token::Type));
        assert_eq!(token_for_capture("constructor"), Some(Token::Type));
        assert_eq!(token_for_capture("function.method"), Some(Token::Function));
        assert_eq!(token_for_capture("function.macro"), Some(Token::Macro));
        assert_eq!(token_for_capture("text.title"), Some(Token::Heading));
        assert_eq!(token_for_capture("variable"), None);
        assert_eq!(token_for_capture("embedded"), None);
    }

    #[test]
    fn every_language_has_a_working_query() {
        for l in [Lang::Rust, Lang::TypeScript, Lang::Tsx, Lang::JavaScript, Lang::Java, Lang::Python, Lang::Json, Lang::Toml, Lang::Markdown, Lang::Bash] {
            let c = l.config().unwrap_or_else(|| panic!("{l:?} has no config"));
            assert!(c.tokens.iter().any(|t| t.is_some()), "{l:?} maps no captures");
        }
        // TypeScript gets the JavaScript rules too (keywords like `const` come from there).
        assert!(Lang::TypeScript.config().unwrap().query.pattern_count() > Query::new(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), tree_sitter_typescript::HIGHLIGHTS_QUERY).unwrap().pattern_count());
    }

    fn tokens_of(lang: Lang, text: &str) -> Vec<Vec<Option<Token>>> {
        let rope = Rope::from_str(text);
        let mut s = Syntax::new(lang);
        s.update(&rope, 1);
        s.visible(&rope, 0, 100).to_vec()
    }

    #[test]
    fn highlights_rust() {
        let t = tokens_of(Lang::Rust, "fn main() {\n    let x = \"hi\"; // note\n    42\n}\n");
        assert_eq!(t[0][0], Some(Token::Keyword)); // fn
        assert_eq!(t[0][3], Some(Token::Function)); // main
        assert_eq!(t[1][4], Some(Token::Keyword)); // let
        assert_eq!(t[1][12], Some(Token::String)); // "hi"
        assert_eq!(t[1][18], Some(Token::Comment));
        assert_eq!(t[2][4], Some(Token::Number));
    }

    #[test]
    fn highlights_other_languages() {
        assert_eq!(tokens_of(Lang::Python, "def f():\n    return 'a'\n")[1][11], Some(Token::String));
        assert_eq!(tokens_of(Lang::TypeScript, "const x: number = 1;\n")[0][0], Some(Token::Keyword));
        assert_eq!(tokens_of(Lang::Json, "{\"a\": 1}\n")[0][1], Some(Token::Property));
        assert_eq!(tokens_of(Lang::Toml, "# c\n")[0][0], Some(Token::Comment));
        assert_eq!(tokens_of(Lang::Bash, "echo hi # c\n")[0][9], Some(Token::Comment));
        assert_eq!(tokens_of(Lang::Java, "class A {}\n")[0][0], Some(Token::Keyword));
        assert_eq!(tokens_of(Lang::Markdown, "# Title\n")[0][2], Some(Token::Heading));
    }

    #[test]
    fn incremental_edits_match_a_fresh_parse() {
        let mut text = String::from("fn a() {}\nfn b() { let s = \"x\"; }\n");
        let mut s = Syntax::new(Lang::Rust);
        let rope = Rope::from_str(&text);
        s.update(&rope, 1);
        for (rev, (at, ins)) in [(10usize, "// c\n"), (0, "/* "), (3, " */"), (25, "é")].into_iter().enumerate() {
            text.insert_str(at, ins);
            let rope = Rope::from_str(&text);
            s.update(&rope, rev as u64 + 2);
            let fresh = tokens_of(Lang::Rust, &text);
            assert_eq!(s.visible(&rope, 0, 100), &fresh[..], "after inserting {ins:?}");
        }
    }

    #[test]
    fn diff_edit_finds_the_changed_middle() {
        let e = diff_edit("abc\ndef", "abc\ndXef").unwrap();
        assert_eq!((e.start_byte, e.old_end_byte, e.new_end_byte), (5, 5, 6));
        assert_eq!(e.start_position, Point::new(1, 1));
        assert!(diff_edit("same", "same").is_none());
        let e = diff_edit("aaa", "aa").unwrap();
        assert_eq!((e.start_byte, e.old_end_byte, e.new_end_byte), (2, 3, 2));
    }

    #[test]
    fn typing_in_a_big_file_stays_fast() {
        let block = "/// Docs for the item.\npub fn item_NN(x: u32) -> String {\n    let s = format!(\"{} {}\", x, \"NN\");\n    if x > 10 { s } else { String::new() }\n}\n\n";
        let text: String = (0..834).map(|i| block.replace("NN", &i.to_string())).collect();
        let mut rope = Rope::from_str(&text);
        assert!(rope.len_lines() > 5000);
        let mut s = Syntax::new(Lang::Rust);
        let t0 = std::time::Instant::now();
        s.update(&rope, 1);
        s.visible(&rope, 2500, 40);
        let initial = t0.elapsed();
        let t1 = std::time::Instant::now();
        for i in 0..20 {
            let at = rope.line_to_char(2510) + 4;
            rope.insert(at, "x");
            s.update(&rope, i + 2);
            s.visible(&rope, 2500, 40);
        }
        let per_key = t1.elapsed() / 20;
        eprintln!("5000+ lines: first parse+highlight {initial:?}, per keystroke {per_key:?}");
        // Generous for unoptimised test builds; release is far below this.
        assert!(per_key < std::time::Duration::from_millis(60), "per keystroke {per_key:?}");
    }
}
