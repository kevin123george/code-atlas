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
    Kotlin,
    Python,
    Go,
    C,
    Cpp,
    Bash,
    Lua,
    Json,
    Yaml,
    Toml,
    Html,
    Css,
    Markdown,
    Dockerfile,
    Sql,
    Xml,
    Dtd,
    Plain,
}

impl Lang {
    /// Every language with a grammar (all but `Plain`).
    pub const ALL: [Lang; 22] = [
        Lang::Rust, Lang::TypeScript, Lang::Tsx, Lang::JavaScript, Lang::Java, Lang::Kotlin, Lang::Python, Lang::Go,
        Lang::C, Lang::Cpp, Lang::Bash, Lang::Lua, Lang::Json, Lang::Yaml, Lang::Toml, Lang::Html, Lang::Css,
        Lang::Markdown, Lang::Dockerfile, Lang::Sql, Lang::Xml, Lang::Dtd,
    ];

    /// The name in `languages.toml` (`[language.<key>]`).
    pub fn key(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Java => "java",
            Lang::Kotlin => "kotlin",
            Lang::Python => "python",
            Lang::Go => "go",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Bash => "bash",
            Lang::Lua => "lua",
            Lang::Json => "json",
            Lang::Yaml => "yaml",
            Lang::Toml => "toml",
            Lang::Html => "html",
            Lang::Css => "css",
            Lang::Markdown => "markdown",
            Lang::Dockerfile => "dockerfile",
            Lang::Sql => "sql",
            Lang::Xml => "xml",
            Lang::Dtd => "dtd",
            Lang::Plain => "text",
        }
    }

    pub fn from_key(key: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.key() == key)
    }

    /// By file name and extension (`languages.toml`), then by a `#!` line.
    pub fn detect(path: Option<&Path>, first_line: &str) -> Lang {
        if let Some(l) = path.and_then(|p| crate::lsp::config::get().lang_for(p)).and_then(Lang::from_key) {
            return l;
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
            if rest.contains("lua") {
                return Lang::Lua;
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
            Lang::Kotlin => "Kotlin",
            Lang::Python => "Python",
            Lang::Go => "Go",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::Bash => "Shell",
            Lang::Lua => "Lua",
            Lang::Json => "JSON",
            Lang::Yaml => "YAML",
            Lang::Toml => "TOML",
            Lang::Html => "HTML",
            Lang::Css => "CSS",
            Lang::Markdown => "Markdown",
            Lang::Dockerfile => "Dockerfile",
            Lang::Sql => "SQL",
            Lang::Xml => "XML",
            Lang::Dtd => "DTD",
            Lang::Plain => "Text",
        }
    }

    /// Token that starts a line comment, if the language has one.
    pub fn line_comment(self) -> Option<&'static str> {
        match self {
            Lang::Rust | Lang::TypeScript | Lang::Tsx | Lang::JavaScript | Lang::Java | Lang::Kotlin | Lang::Go | Lang::C | Lang::Cpp => Some("//"),
            Lang::Python | Lang::Toml | Lang::Bash | Lang::Yaml | Lang::Dockerfile => Some("#"),
            Lang::Lua | Lang::Sql => Some("--"),
            Lang::Json | Lang::Markdown | Lang::Html | Lang::Css | Lang::Xml | Lang::Dtd | Lang::Plain => None,
        }
    }

    /// Start and end of a block comment, for languages without line comments: Ctrl+/ wraps each
    /// line in them (`<!-- line -->`), as IntelliJ does for XML.
    pub fn block_comment(self) -> Option<(&'static str, &'static str)> {
        match self {
            Lang::Xml | Lang::Dtd | Lang::Html | Lang::Markdown => Some(("<!--", "-->")),
            Lang::Css => Some(("/*", "*/")),
            _ => None,
        }
    }

    /// The LSP `languageId` (some depend on the extension: `.scss`, `.jsonc`).
    pub fn lsp_id(self, path: Option<&Path>) -> &'static str {
        let ext = path.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "typescriptreact",
            Lang::JavaScript if ext == "jsx" => "javascriptreact",
            Lang::JavaScript => "javascript",
            Lang::Java => "java",
            Lang::Kotlin => "kotlin",
            Lang::Python => "python",
            Lang::Go => "go",
            Lang::C => "c",
            Lang::Cpp if ext == "mm" => "objective-cpp",
            Lang::Cpp => "cpp",
            Lang::Bash => "shellscript",
            Lang::Lua => "lua",
            Lang::Json if ext == "jsonc" => "jsonc",
            Lang::Json => "json",
            Lang::Yaml => "yaml",
            Lang::Toml => "toml",
            Lang::Html => "html",
            Lang::Css if ext == "scss" => "scss",
            Lang::Css if ext == "less" => "less",
            Lang::Css => "css",
            Lang::Markdown => "markdown",
            Lang::Dockerfile => "dockerfile",
            Lang::Sql => "sql",
            Lang::Xml if ext == "xsl" || ext == "xslt" => "xsl",
            Lang::Xml => "xml",
            Lang::Dtd => "dtd",
            Lang::Plain => "plaintext",
        }
    }

    /// Whether highlighting works for this language (its grammar and query load).
    pub fn highlights(self) -> bool {
        self.config().is_some()
    }

    fn config(self) -> Option<&'static Config> {
        static CONFIGS: [OnceLock<Option<Config>>; Lang::ALL.len()] = [const { OnceLock::new() }; Lang::ALL.len()];
        let i = Lang::ALL.iter().position(|&l| l == self)?;
        CONFIGS[i]
            .get_or_init(|| {
                let (lang, queries): (Language, Vec<&str>) = match self {
                    Lang::Rust => (tree_sitter_rust::LANGUAGE.into(), vec![tree_sitter_rust::HIGHLIGHTS_QUERY]),
                    Lang::TypeScript => (
                        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                        vec![tree_sitter_typescript::HIGHLIGHTS_QUERY, tree_sitter_javascript::HIGHLIGHT_QUERY],
                    ),
                    Lang::Tsx => (
                        tree_sitter_typescript::LANGUAGE_TSX.into(),
                        vec![
                            tree_sitter_typescript::HIGHLIGHTS_QUERY,
                            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                            tree_sitter_javascript::HIGHLIGHT_QUERY,
                        ],
                    ),
                    Lang::JavaScript => (
                        tree_sitter_javascript::LANGUAGE.into(),
                        vec![tree_sitter_javascript::JSX_HIGHLIGHT_QUERY, tree_sitter_javascript::HIGHLIGHT_QUERY],
                    ),
                    Lang::Java => (tree_sitter_java::LANGUAGE.into(), vec![tree_sitter_java::HIGHLIGHTS_QUERY]),
                    Lang::Kotlin => (tree_sitter_kotlin_sg::LANGUAGE.into(), vec![tree_sitter_kotlin_sg::HIGHLIGHTS_QUERY]),
                    Lang::Python => (tree_sitter_python::LANGUAGE.into(), vec![tree_sitter_python::HIGHLIGHTS_QUERY]),
                    Lang::Go => (tree_sitter_go::LANGUAGE.into(), vec![tree_sitter_go::HIGHLIGHTS_QUERY]),
                    Lang::C => (tree_sitter_c::LANGUAGE.into(), vec![tree_sitter_c::HIGHLIGHT_QUERY]),
                    // C++'s query only adds to C's (as in Helix and Neovim).
                    Lang::Cpp => (tree_sitter_cpp::LANGUAGE.into(), vec![tree_sitter_cpp::HIGHLIGHT_QUERY, tree_sitter_c::HIGHLIGHT_QUERY]),
                    Lang::Bash => (tree_sitter_bash::LANGUAGE.into(), vec![tree_sitter_bash::HIGHLIGHT_QUERY]),
                    Lang::Lua => (tree_sitter_lua::LANGUAGE.into(), vec![tree_sitter_lua::HIGHLIGHTS_QUERY]),
                    Lang::Json => (tree_sitter_json::LANGUAGE.into(), vec![tree_sitter_json::HIGHLIGHTS_QUERY]),
                    Lang::Yaml => (tree_sitter_yaml::LANGUAGE.into(), vec![tree_sitter_yaml::HIGHLIGHTS_QUERY]),
                    Lang::Toml => (tree_sitter_toml_ng::LANGUAGE.into(), vec![tree_sitter_toml_ng::HIGHLIGHTS_QUERY]),
                    Lang::Html => (tree_sitter_html::LANGUAGE.into(), vec![tree_sitter_html::HIGHLIGHTS_QUERY]),
                    Lang::Css => (tree_sitter_css::LANGUAGE.into(), vec![tree_sitter_css::HIGHLIGHTS_QUERY]),
                    Lang::Markdown => (tree_sitter_md::LANGUAGE.into(), vec![tree_sitter_md::HIGHLIGHT_QUERY_BLOCK]),
                    Lang::Dockerfile => (tree_sitter_containerfile::LANGUAGE.into(), vec![tree_sitter_containerfile::HIGHLIGHTS_QUERY]),
                    Lang::Sql => (tree_sitter_sequel::LANGUAGE.into(), vec![tree_sitter_sequel::HIGHLIGHTS_QUERY]),
                    Lang::Xml => (tree_sitter_xml::LANGUAGE_XML.into(), vec![XML_EXTRA, tree_sitter_xml::XML_HIGHLIGHT_QUERY]),
                    Lang::Dtd => (tree_sitter_xml::LANGUAGE_DTD.into(), vec![tree_sitter_xml::DTD_HIGHLIGHT_QUERY]),
                    Lang::Plain => return None,
                };
                Config::new(lang, &queries)
            })
            .as_ref()
    }
}

/// Put before the XML grammar's own query (the first pattern wins for a node): attribute names
/// coloured like HTML's (the bundled query makes them plain `@property`), quotes in the string
/// colour like the other languages' strings, and the CDATA brackets as punctuation, not headings.
const XML_EXTRA: &str = r#"
(Attribute (Name) @attribute)
["\"" "'"] @string
(CDSect (CDStart) @punctuation.delimiter)
(CDSect "]]>" @punctuation.delimiter)
"#;

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
        for l in Lang::ALL {
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
    fn highlights_the_newer_languages() {
        let at = |l: Lang, text: &str, col: usize| tokens_of(l, text)[0][col];
        assert_eq!(at(Lang::Go, "func main() {} // c\n", 0), Some(Token::Keyword));
        assert_eq!(at(Lang::Go, "func main() {} // c\n", 16), Some(Token::Comment));
        assert_eq!(at(Lang::C, "int x = 1; // c\n", 8), Some(Token::Number));
        assert_eq!(at(Lang::C, "#include <stdio.h>\n", 0), Some(Token::Keyword));
        assert_eq!(at(Lang::Cpp, "class A { public: int x; };\n", 0), Some(Token::Keyword));
        assert_eq!(at(Lang::Cpp, "int x = 1; // c\n", 13), Some(Token::Comment), "C rules apply to C++");
        assert_eq!(at(Lang::Kotlin, "fun f(): Int = 1\n", 0), Some(Token::Keyword));
        assert_eq!(at(Lang::Yaml, "name: \"x\" # c\n", 7), Some(Token::String));
        assert_eq!(at(Lang::Yaml, "name: \"x\" # c\n", 11), Some(Token::Comment));
        assert_eq!(at(Lang::Html, "<p class=\"a\">x</p>\n", 1), Some(Token::Tag));
        assert_eq!(at(Lang::Css, "a { color: red; }\n", 4), Some(Token::Property));
        assert_eq!(at(Lang::Lua, "local x = 'a' -- c\n", 0), Some(Token::Keyword));
        assert_eq!(at(Lang::Lua, "local x = 'a' -- c\n", 15), Some(Token::Comment));
        assert_eq!(at(Lang::Sql, "SELECT id FROM t; -- c\n", 0), Some(Token::Keyword));
        assert_eq!(at(Lang::Dockerfile, "FROM alpine:3 # c\n", 0), Some(Token::Keyword));
        assert_eq!(lang("Dockerfile"), Lang::Dockerfile);
        assert_eq!(lang("main.go"), Lang::Go);
        assert_eq!(lang("x.hpp"), Lang::Cpp);
        assert_eq!(lang("build.gradle.kts"), Lang::Kotlin);
        assert_eq!(lang("ci.yml"), Lang::Yaml);
        assert_eq!(Lang::Lua.line_comment(), Some("--"));
        assert_eq!(Lang::Css.lsp_id(Some(Path::new("a.scss"))), "scss");
        assert_eq!(Lang::Json.lsp_id(Some(Path::new("tsconfig.jsonc"))), "jsonc");
        for l in Lang::ALL {
            assert_eq!(Lang::from_key(l.key()), Some(l));
        }
    }

    #[test]
    fn detects_xml_and_its_dialects() {
        for name in [
            "pom.xml", "a.XML", "schema.xsd", "t.xsl", "t.xslt", "icon.svg", "Info.plist", "lib-1.0.pom",
            "App.csproj", "x.fsproj", "Directory.Build.props", "x.targets", "MainWindow.xaml", "code-atlas.iml",
            "feed.rss", "feed.atom", "Main.storyboard", "phpunit.xml.dist",
        ] {
            assert_eq!(lang(name), Lang::Xml, "{name}");
        }
        assert_eq!(lang("x.dtd"), Lang::Dtd);
        assert_eq!(lang("go.mod"), Lang::Plain, "not taken by DTD");
        assert_eq!(Lang::Xml.name(), "XML");
        assert_eq!(Lang::Xml.line_comment(), None);
        assert_eq!(Lang::Xml.block_comment(), Some(("<!--", "-->")));
        assert_eq!(Lang::Html.block_comment(), Some(("<!--", "-->")));
        assert_eq!(Lang::Css.block_comment(), Some(("/*", "*/")));
        assert_eq!(Lang::Rust.block_comment(), None);
        assert_eq!(Lang::Xml.lsp_id(Some(Path::new("t.xslt"))), "xsl");
        assert_eq!(Lang::Xml.lsp_id(Some(Path::new("pom.xml"))), "xml");
    }

    #[test]
    fn highlights_xml() {
        let text = "<?xml version=\"1.0\"?>\n<!-- note -->\n<project a=\"1\">&amp;<b/>text\n<![CDATA[raw]]></project>\n";
        let t = tokens_of(Lang::Xml, text);
        assert_eq!(t[0][2], Some(Token::Keyword), "xml");
        assert_eq!(t[0][6], Some(Token::Property), "version");
        assert_eq!(t[0][15], Some(Token::Number), "1.0");
        assert_eq!(t[1][0], Some(Token::Comment));
        assert_eq!(t[1][12], Some(Token::Comment));
        assert_eq!(t[2][0], Some(Token::Punctuation), "<");
        assert_eq!(t[2][1], Some(Token::Tag), "project");
        assert_eq!(t[2][9], Some(Token::Attribute), "a, coloured like HTML attributes");
        assert_eq!(t[2][11], Some(Token::String), "the quote");
        assert_eq!(t[2][12], Some(Token::String), "1");
        assert_eq!(t[2][15], Some(Token::Constant), "&amp;");
        assert_eq!(t[2][21], Some(Token::Tag), "b");
        assert_eq!(t[2][24], None, "text stays plain");
        assert_eq!(t[3][0], Some(Token::Punctuation), "<![CDATA[");
        assert_eq!(t[3][9], Some(Token::String), "raw");
        assert_eq!(t[3][12], Some(Token::Punctuation), "]]>");
        assert_eq!(t[3][17], Some(Token::Tag), "closing project");
        let d = tokens_of(Lang::Dtd, "<!ELEMENT note (to)>\n");
        assert_eq!(d[0][2], Some(Token::Keyword), "ELEMENT");
        assert_eq!(d[0][10], Some(Token::Tag), "note");
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
