//! Symbols without a language server: tree-sitter queries for definitions (functions, methods,
//! types, classes, modules …) per language, run on every indexed source file. The queries are
//! our own, definition-only versions of the grammars' `tags.scm` (no references, no doc-comment
//! predicates), so indexing stays cheap. `@name` is the symbol's name; the other capture's name
//! is its [`Kind`]. The first pattern that matches a name wins (methods before functions).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use crate::syntax::Lang;

/// At most this many symbols per file (generated files can have huge numbers).
pub const MAX_PER_FILE: usize = 2000;
/// Longer names are cut (they're noise in a picker).
const MAX_NAME: usize = 120;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Class = 1,
    Interface,
    Enum,
    Struct,
    Function,
    Method,
    Module,
    Macro,
    Constant,
    Type,
}

impl Kind {
    pub fn from_u8(v: u8) -> Kind {
        match v {
            1 => Kind::Class,
            2 => Kind::Interface,
            3 => Kind::Enum,
            4 => Kind::Struct,
            6 => Kind::Method,
            7 => Kind::Module,
            8 => Kind::Macro,
            9 => Kind::Constant,
            10 => Kind::Type,
            _ => Kind::Function,
        }
    }

    fn from_capture(name: &str) -> Option<Kind> {
        Some(match name {
            "class" => Kind::Class,
            "interface" => Kind::Interface,
            "enum" => Kind::Enum,
            "struct" => Kind::Struct,
            "function" => Kind::Function,
            "method" => Kind::Method,
            "module" => Kind::Module,
            "macro" => Kind::Macro,
            "constant" => Kind::Constant,
            "type" => Kind::Type,
            _ => return None,
        })
    }

    /// The short label the symbol pickers show (same style as the LSP's kinds).
    pub fn label(self) -> &'static str {
        match self {
            Kind::Class => "C",
            Kind::Interface => "I",
            Kind::Enum => "E",
            Kind::Struct => "S",
            Kind::Function => "fn",
            Kind::Method => "me",
            Kind::Module => "mod",
            Kind::Macro => "mac",
            Kind::Constant => "c",
            Kind::Type => "T",
        }
    }

    /// Types rank a little higher than members in the pickers (IDEA's "classes first").
    pub fn is_type(self) -> bool {
        matches!(self, Kind::Class | Kind::Interface | Kind::Enum | Kind::Struct | Kind::Type)
    }
}

/// One definition: name, kind, 0-based line and char column of the name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    pub kind: Kind,
    pub line: u32,
    pub col: u32,
}

const RUST: &str = r#"
(declaration_list (function_item name: (identifier) @name) @method)
(declaration_list (function_signature_item name: (identifier) @name) @method)
(function_item name: (identifier) @name) @function
(function_signature_item name: (identifier) @name) @function
(struct_item name: (type_identifier) @name) @struct
(enum_item name: (type_identifier) @name) @enum
(union_item name: (type_identifier) @name) @struct
(type_item name: (type_identifier) @name) @type
(trait_item name: (type_identifier) @name) @interface
(mod_item name: (identifier) @name) @module
(macro_definition name: (identifier) @name) @macro
(const_item name: (identifier) @name) @constant
(static_item name: (identifier) @name) @constant
"#;

const JS: &str = r#"
(method_definition name: (property_identifier) @name) @method
(class_declaration name: (_) @name) @class
(function_declaration name: (identifier) @name) @function
(generator_function_declaration name: (identifier) @name) @function
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @function
"#;

const TS_EXTRA: &str = r#"
(interface_declaration name: (type_identifier) @name) @interface
(type_alias_declaration name: (type_identifier) @name) @type
(enum_declaration name: (identifier) @name) @enum
(abstract_class_declaration name: (type_identifier) @name) @class
(method_signature name: (property_identifier) @name) @method
(abstract_method_signature name: (property_identifier) @name) @method
(function_signature name: (identifier) @name) @function
(module name: (identifier) @name) @module
"#;

const JAVA: &str = r#"
(class_declaration name: (identifier) @name) @class
(interface_declaration name: (identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum
(record_declaration name: (identifier) @name) @class
(annotation_type_declaration name: (identifier) @name) @interface
(method_declaration name: (identifier) @name) @method
(constructor_declaration name: (identifier) @name) @method
"#;

const KOTLIN: &str = r#"
(class_declaration (type_identifier) @name) @class
(object_declaration (type_identifier) @name) @class
(class_body (function_declaration (simple_identifier) @name) @method)
(function_declaration (simple_identifier) @name) @function
(type_alias (type_identifier) @name) @type
"#;

const PYTHON: &str = r#"
(class_definition body: (block (function_definition name: (identifier) @name) @method))
(class_definition body: (block (decorated_definition (function_definition name: (identifier) @name) @method)))
(class_definition name: (identifier) @name) @class
(function_definition name: (identifier) @name) @function
"#;

const GO: &str = r#"
(function_declaration name: (identifier) @name) @function
(method_declaration name: (field_identifier) @name) @method
(type_spec name: (type_identifier) @name type: (struct_type)) @struct
(type_spec name: (type_identifier) @name type: (interface_type)) @interface
(type_spec name: (type_identifier) @name) @type
(method_elem name: (field_identifier) @name) @method
"#;

const C: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @function
(struct_specifier name: (type_identifier) @name body: (_)) @struct
(union_specifier name: (type_identifier) @name body: (_)) @struct
(enum_specifier name: (type_identifier) @name body: (_)) @enum
(type_definition declarator: (type_identifier) @name) @type
(preproc_function_def name: (identifier) @name) @macro
"#;

const CPP_EXTRA: &str = r#"
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))) @method
(function_definition declarator: (function_declarator declarator: (field_identifier) @name)) @method
(class_specifier name: (type_identifier) @name body: (_)) @class
(namespace_definition name: (namespace_identifier) @name) @module
"#;

const LUA: &str = r#"
(function_declaration name: (identifier) @name) @function
(function_declaration name: (dot_index_expression field: (identifier) @name)) @function
(function_declaration name: (method_index_expression method: (identifier) @name)) @method
"#;

const BASH: &str = r#"
(function_definition name: (word) @name) @function
"#;

struct Tags {
    language: Language,
    query: Query,
    name_idx: u32,
    /// Kind per capture index (`None` for `@name`).
    kinds: Vec<Option<Kind>>,
}

fn tags(lang: Lang) -> Option<&'static Tags> {
    const N: usize = 12;
    const LANGS: [Lang; N] = [
        Lang::Rust, Lang::TypeScript, Lang::Tsx, Lang::JavaScript, Lang::Java, Lang::Kotlin, Lang::Python, Lang::Go, Lang::C,
        Lang::Cpp, Lang::Lua, Lang::Bash,
    ];
    static TAGS: [OnceLock<Option<Tags>>; N] = [const { OnceLock::new() }; N];
    let i = LANGS.iter().position(|l| *l == lang)?;
    TAGS[i]
        .get_or_init(|| {
            let (language, src): (Language, String) = match lang {
                Lang::Rust => (tree_sitter_rust::LANGUAGE.into(), RUST.into()),
                Lang::TypeScript => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), format!("{TS_EXTRA}{JS}")),
                Lang::Tsx => (tree_sitter_typescript::LANGUAGE_TSX.into(), format!("{TS_EXTRA}{JS}")),
                Lang::JavaScript => (tree_sitter_javascript::LANGUAGE.into(), JS.into()),
                Lang::Java => (tree_sitter_java::LANGUAGE.into(), JAVA.into()),
                Lang::Kotlin => (tree_sitter_kotlin_sg::LANGUAGE.into(), KOTLIN.into()),
                Lang::Python => (tree_sitter_python::LANGUAGE.into(), PYTHON.into()),
                Lang::Go => (tree_sitter_go::LANGUAGE.into(), GO.into()),
                Lang::C => (tree_sitter_c::LANGUAGE.into(), C.into()),
                Lang::Cpp => (tree_sitter_cpp::LANGUAGE.into(), format!("{CPP_EXTRA}{C}")),
                Lang::Lua => (tree_sitter_lua::LANGUAGE.into(), LUA.into()),
                Lang::Bash => (tree_sitter_bash::LANGUAGE.into(), BASH.into()),
                _ => return None,
            };
            let query = Query::new(&language, &src).ok()?;
            let name_idx = query.capture_index_for_name("name")?;
            let kinds = query.capture_names().iter().map(|n| Kind::from_capture(n)).collect();
            Some(Tags { language, query, name_idx, kinds })
        })
        .as_ref()
}

/// Whether symbols are extracted for files of `lang`.
pub fn supported(lang: Lang) -> bool {
    tags(lang).is_some()
}

thread_local! {
    static PARSERS: RefCell<HashMap<Lang, Parser>> = RefCell::new(HashMap::new());
}

/// The definitions in `src` (a file of language `lang`), in file order.
pub fn extract(lang: Lang, src: &[u8]) -> Vec<Def> {
    let Some(t) = tags(lang) else { return Vec::new() };
    let tree = PARSERS.with(|ps| {
        let mut ps = ps.borrow_mut();
        let p = ps.entry(lang).or_insert_with(|| {
            let mut p = Parser::new();
            let _ = p.set_language(&t.language);
            p
        });
        p.parse(src, None)
    });
    let Some(tree) = tree else { return Vec::new() };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&t.query, tree.root_node(), src);
    // name start byte → (pattern, def); the first pattern wins.
    let mut found: HashMap<usize, (usize, Def)> = HashMap::new();
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut kind = None;
        for c in m.captures() {
            if c.index == t.name_idx {
                name = Some(c.node);
            } else if let Some(k) = t.kinds[c.index as usize] {
                kind = Some(k);
            }
        }
        let (Some(node), Some(kind)) = (name, kind) else { continue };
        let start = node.start_byte();
        if found.get(&start).is_some_and(|(p, _)| *p <= m.pattern_index) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&src[start..node.end_byte().min(src.len())]) else { continue };
        if text.is_empty() {
            continue;
        }
        let pos = node.start_position();
        let line_start = start - pos.column.min(start);
        let col = std::str::from_utf8(&src[line_start..start]).map_or(pos.column, |s| s.chars().count());
        let name: String = text.chars().take(MAX_NAME).collect();
        found.insert(start, (m.pattern_index, Def { name, kind, line: pos.row as u32, col: col as u32 }));
        if found.len() >= MAX_PER_FILE {
            break;
        }
    }
    let mut defs: Vec<(usize, Def)> = found.into_iter().map(|(s, (_, d))| (s, d)).collect();
    defs.sort_by_key(|(s, _)| *s);
    defs.into_iter().map(|(_, d)| d).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(lang: Lang, src: &str) -> Vec<(String, &'static str, u32)> {
        extract(lang, src.as_bytes()).into_iter().map(|d| (d.name, d.kind.label(), d.line)).collect()
    }

    #[test]
    fn every_query_compiles() {
        for l in [Lang::Rust, Lang::TypeScript, Lang::Tsx, Lang::JavaScript, Lang::Java, Lang::Kotlin, Lang::Python, Lang::Go, Lang::C, Lang::Cpp, Lang::Lua, Lang::Bash] {
            assert!(supported(l), "{l:?}");
        }
        assert!(!supported(Lang::Json));
        assert!(extract(Lang::Markdown, b"# hi").is_empty());
    }

    #[test]
    fn rust_definitions_with_methods() {
        let src = "struct Point { x: i32 }\nimpl Point {\n    fn len(&self) -> i32 { 0 }\n}\nfn main() {}\ntrait Shape {}\nenum E { A }\nmod m {}\nmacro_rules! mac { () => {} }\nconst MAX: u32 = 1;\n";
        assert_eq!(
            names(Lang::Rust, src),
            vec![
                ("Point".into(), "S", 0),
                ("len".into(), "me", 2),
                ("main".into(), "fn", 4),
                ("Shape".into(), "I", 5),
                ("E".into(), "E", 6),
                ("m".into(), "mod", 7),
                ("mac".into(), "mac", 8),
                ("MAX".into(), "c", 9),
            ]
        );
        let d = &extract(Lang::Rust, "  fn ünïcode_x() {}\nfn  b() {}".as_bytes());
        assert_eq!((d[0].line, d[0].col), (0, 5));
        assert_eq!((d[1].line, d[1].col), (1, 4));
    }

    #[test]
    fn other_languages() {
        assert_eq!(names(Lang::Python, "class A:\n    def m(self): pass\ndef f(): pass\n"), vec![("A".into(), "C", 0), ("m".into(), "me", 1), ("f".into(), "fn", 2)]);
        assert_eq!(
            names(Lang::TypeScript, "interface Shape { area(): number }\nclass Box { size() { return 1 } }\nfunction make() {}\nconst arrow = () => 1;\ntype Id = string;\nenum Color { Red }\n"),
            vec![
                ("Shape".into(), "I", 0),
                ("area".into(), "me", 0),
                ("Box".into(), "C", 1),
                ("size".into(), "me", 1),
                ("make".into(), "fn", 2),
                ("arrow".into(), "fn", 3),
                ("Id".into(), "T", 4),
                ("Color".into(), "E", 5),
            ]
        );
        assert_eq!(names(Lang::Go, "package p\ntype S struct{}\ntype I interface{ M() }\nfunc (s S) Run() {}\nfunc main() {}\n"), vec![
            ("S".into(), "S", 1),
            ("I".into(), "I", 2),
            ("M".into(), "me", 2),
            ("Run".into(), "me", 3),
            ("main".into(), "fn", 4),
        ]);
        assert_eq!(names(Lang::Java, "class A { void run() {} A() {} }\ninterface B {}\n"), vec![("A".into(), "C", 0), ("run".into(), "me", 0), ("A".into(), "me", 0), ("B".into(), "I", 1)]);
        assert_eq!(names(Lang::C, "struct s { int x; };\nint add(int a) { return a; }\n#define SQ(x) x*x\n"), vec![("s".into(), "S", 0), ("add".into(), "fn", 1), ("SQ".into(), "mac", 2)]);
        assert_eq!(names(Lang::Bash, "deploy() {\n  echo\n}\n"), vec![("deploy".into(), "fn", 0)]);
        assert_eq!(names(Lang::Kotlin, "class K {\n  fun go() {}\n}\nfun top() {}\n"), vec![("K".into(), "C", 0), ("go".into(), "me", 1), ("top".into(), "fn", 3)]);
        assert_eq!(names(Lang::Lua, "function M.run() end\nlocal function f() end\n"), vec![("run".into(), "fn", 0), ("f".into(), "fn", 1)]);
    }
}
