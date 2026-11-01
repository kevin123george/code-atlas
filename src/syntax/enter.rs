//! What Enter does per language (decision 0019): continue Markdown lists and quotes, indent YAML
//! after a key, shell after `then` / `do`, carry ` * ` through a block comment. Pure: text in, plan out.

use super::Lang;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Remove what is before the cursor on this line and stay there (an empty list item ends the list).
    pub clear: bool,
    /// Take this many leading spaces off the current line first (`fi` back under its `if`).
    pub dedent: usize,
    /// What the new line starts with.
    pub text: String,
    /// A further line below the cursor (the ` */` of a new comment).
    pub close: Option<String>,
}

fn leading(s: &str) -> &str {
    &s[..s.len() - s.trim_start_matches([' ', '\t']).len()]
}

/// `before` / `after`: the current line on each side of the cursor. `in_fence`: inside a Markdown
/// code block, where nothing is a list. `None`: the editor's ordinary Enter.
pub fn plan(lang: Lang, before: &str, after: &str, unit: usize, in_fence: bool) -> Option<Plan> {
    match lang {
        Lang::Markdown if !in_fence => markdown(before, after),
        Lang::Yaml => yaml(before, after, unit),
        Lang::Bash => shell(before, after, unit),
        Lang::Java | Lang::Kotlin | Lang::JavaScript | Lang::TypeScript | Lang::Tsx | Lang::C | Lang::Cpp | Lang::Css | Lang::Go | Lang::Rust => block_comment(before, after),
        Lang::Python if before.trim_end().ends_with(':') && after.trim().is_empty() && !before.trim_start().starts_with('#') => {
            Some(Plan { text: format!("{}{}", leading(before), " ".repeat(unit)), ..Default::default() })
        }
        _ => None,
    }
}

fn markdown(before: &str, after: &str) -> Option<Plan> {
    let indent = leading(before);
    let mut rest = &before[indent.len()..];
    // Quote markers, any depth.
    let mut prefix = indent.to_string();
    let mut quoted = false;
    while let Some(r) = rest.strip_prefix('>') {
        quoted = true;
        prefix.push('>');
        rest = r;
        if let Some(r) = rest.strip_prefix(' ') {
            prefix.push(' ');
            rest = r;
        }
    }
    let list_indent = leading(rest);
    prefix.push_str(list_indent);
    rest = &rest[list_indent.len()..];
    let (marker, body) = list_marker(rest)?.map_or_else(|| (None, rest), |(m, b)| (Some(m), b));
    let (task, body) = match body.strip_prefix("[ ] ").or_else(|| body.strip_prefix("[x] ")).or_else(|| body.strip_prefix("[X] ")) {
        Some(b) if marker.is_some() => (true, b),
        _ => (false, body),
    };
    if marker.is_none() && !quoted {
        return None;
    }
    if body.trim().is_empty() && after.trim().is_empty() {
        return Some(Plan { clear: true, ..Default::default() });
    }
    let mut text = prefix;
    if let Some(m) = marker {
        text.push_str(&m);
        text.push(' ');
    }
    if task {
        text.push_str("[ ] ");
    }
    Some(Plan { text, ..Default::default() })
}

/// `Some(None)`: no marker. `Some(Some((next marker, rest)))`: `- x` → `-`, `3. x` → `4.`.
#[allow(clippy::type_complexity)]
fn list_marker(s: &str) -> Option<Option<(String, &str)>> {
    for bullet in ['-', '*', '+'] {
        if let Some(r) = s.strip_prefix(bullet) {
            if let Some(body) = r.strip_prefix(' ') {
                return Some(Some((bullet.to_string(), body)));
            }
        }
    }
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if (1..=9).contains(&digits) {
        let r = &s[digits..];
        for d in ['.', ')'] {
            if let Some(body) = r.strip_prefix(d).and_then(|r| r.strip_prefix(' ')) {
                let n: u64 = s[..digits].parse().ok()?;
                return Some(Some((format!("{}{d}", n + 1), body)));
            }
        }
    }
    Some(None)
}

fn yaml(before: &str, after: &str, unit: usize) -> Option<Plan> {
    if !after.trim().is_empty() {
        return None;
    }
    let indent = leading(before);
    let body = before[indent.len()..].trim_end();
    if body.starts_with('#') {
        return None;
    }
    let (item, content) = match body.strip_prefix("- ") {
        Some(c) => (true, c.trim_start()),
        None if body == "-" => (true, ""),
        None => (false, body),
    };
    if item && content.is_empty() {
        return Some(Plan { clear: true, ..Default::default() });
    }
    let opens = |s: &str| s.ends_with(':') || [": |", ": >", ": |-", ": >-", ": |+", ": >+"].iter().any(|e| s.ends_with(e));
    let is_pair = |s: &str| s.contains(": ") || s.ends_with(':');
    // Under `- key:` the next line belongs to the map that starts after the dash.
    let col = indent.len() + if item { 2 } else { 0 };
    if opens(content) {
        return Some(Plan { text: " ".repeat(col + unit), ..Default::default() });
    }
    if item && is_pair(content) {
        return Some(Plan { text: " ".repeat(col), ..Default::default() });
    }
    if item {
        return Some(Plan { text: format!("{indent}- "), ..Default::default() });
    }
    None
}

fn shell(before: &str, after: &str, unit: usize) -> Option<Plan> {
    if !after.trim().is_empty() {
        return None;
    }
    let indent = leading(before);
    let code = before[indent.len()..].trim_end();
    if code.starts_with('#') {
        return None;
    }
    // The last word, for `if x; then` as well as a bare `then`.
    let last = code.rsplit([' ', ';', '\t']).next().unwrap_or("");
    let first_only = code.trim_end_matches(';');
    let spaces = indent.chars().count();
    let back = if spaces >= unit { unit } else { spaces };
    let kept = spaces - back;
    match first_only {
        "fi" | "done" | "esac" => return Some(Plan { dedent: back, text: " ".repeat(kept), ..Default::default() }),
        "else" => return Some(Plan { dedent: back, text: " ".repeat(kept + unit), ..Default::default() }),
        _ => {}
    }
    if first_only.starts_with("elif ") && last == "then" {
        return Some(Plan { dedent: back, text: " ".repeat(kept + unit), ..Default::default() });
    }
    let case_label = code.ends_with(')') && !code.contains('(') && !code.contains(";;");
    if matches!(last, "then" | "do" | "in") && (last != "in" || code.starts_with("case ")) || case_label {
        return Some(Plan { text: format!("{indent}{}", " ".repeat(unit)), ..Default::default() });
    }
    if code == ";;" {
        return Some(Plan { dedent: 0, text: " ".repeat(kept), ..Default::default() });
    }
    None
}

fn block_comment(before: &str, after: &str) -> Option<Plan> {
    let indent = leading(before);
    let body = before[indent.len()..].trim_end();
    if (body == "/**" || body == "/*") && after.trim().is_empty() {
        return Some(Plan { text: format!("{indent} * "), close: Some(format!("{indent} */")), ..Default::default() });
    }
    if (body == "*" || body.starts_with("* ")) && !body.contains("*/") {
        return Some(Plan { text: format!("{indent}* "), ..Default::default() });
    }
    None
}

/// Whether the line after `lines` lies in a fenced Markdown code block.
pub fn in_fence<'a>(lines: impl Iterator<Item = &'a str>) -> bool {
    let mut open: Option<(char, usize)> = None;
    for line in lines {
        let t = line.trim_start_matches([' ', '>']);
        let Some(c) = t.chars().next().filter(|c| *c == '`' || *c == '~') else { continue };
        let run = t.chars().take_while(|x| *x == c).count();
        if run < 3 {
            continue;
        }
        match open {
            None => open = Some((c, run)),
            Some((oc, orun)) if oc == c && run >= orun && t[run * c.len_utf8()..].trim().is_empty() => open = None,
            _ => {}
        }
    }
    open.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lang: Lang, before: &str) -> Option<String> {
        plan(lang, before, "", 2, false).map(|p| p.text)
    }
    fn cleared(lang: Lang, before: &str) -> bool {
        plan(lang, before, "", 2, false).is_some_and(|p| p.clear)
    }

    #[test]
    fn markdown_lists_continue() {
        let md = Lang::Markdown;
        assert_eq!(text(md, "- one").as_deref(), Some("- "));
        assert_eq!(text(md, "  * two").as_deref(), Some("  * "));
        assert_eq!(text(md, "+ x").as_deref(), Some("+ "));
        assert_eq!(text(md, "3. three").as_deref(), Some("4. "));
        assert_eq!(text(md, "9) nine").as_deref(), Some("10) "));
        assert_eq!(text(md, "- [x] done").as_deref(), Some("- [ ] "), "a new task starts open");
        assert_eq!(text(md, "- [ ] todo").as_deref(), Some("- [ ] "));
        assert_eq!(text(md, "> quote").as_deref(), Some("> "));
        assert_eq!(text(md, "> - in a quote").as_deref(), Some("> - "));
        assert_eq!(text(md, "plain text"), None);
        assert_eq!(text(md, "2020. was a year").as_deref(), Some("2021. "), "as other editors do");
        assert_eq!(text(md, "-not a list"), None);
        assert_eq!(text(md, "---"), None);
    }

    #[test]
    fn an_empty_item_ends_the_list() {
        for line in ["- ", "1. ", "- [ ] ", "> ", "  - "] {
            assert!(cleared(Lang::Markdown, line), "{line:?}");
        }
        assert!(!cleared(Lang::Markdown, "- x"));
        let split = plan(Lang::Markdown, "- ", "text after", 2, false).unwrap();
        assert!(!split.clear, "text after the cursor is not an empty item");
    }

    #[test]
    fn code_blocks_are_not_lists() {
        assert_eq!(plan(Lang::Markdown, "- x", "", 2, true), None);
        assert!(in_fence(["text", "```java", "- x"].into_iter()));
        assert!(!in_fence(["```", "code", "```", "text"].into_iter()));
        assert!(in_fence(["````", "```", "still"].into_iter()), "a shorter fence doesn't close");
        assert!(in_fence(["~~~", "```"].into_iter()), "another fence character doesn't close");
        assert!(!in_fence(["``inline``", "text"].into_iter()));
        assert!(in_fence(["> ```", "> code"].into_iter()));
    }

    #[test]
    fn yaml_indents_after_a_key() {
        let y = Lang::Yaml;
        assert_eq!(text(y, "services:").as_deref(), Some("  "));
        assert_eq!(text(y, "  api:").as_deref(), Some("    "));
        assert_eq!(text(y, "script: |").as_deref(), Some("  "));
        assert_eq!(text(y, "cmd: >-").as_deref(), Some("  "));
        assert_eq!(text(y, "- name: x").as_deref(), Some("  "), "the map after the dash");
        assert_eq!(text(y, "  - steps:").as_deref(), Some("      "));
        assert_eq!(text(y, "- first").as_deref(), Some("- "));
        assert_eq!(text(y, "key: value"), None);
        assert_eq!(text(y, "# note:"), None);
        assert!(cleared(y, "- "));
        assert_eq!(plan(y, "a:", " b", 2, false), None, "splitting a line is plain");
        assert_eq!(plan(y, "services:", "", 4, false).unwrap().text, "    ", "the file's own width");
    }

    #[test]
    fn shell_blocks_indent_and_closers_come_back() {
        let s = Lang::Bash;
        assert_eq!(text(s, "if [ -f x ]; then").as_deref(), Some("  "));
        assert_eq!(text(s, "  then").as_deref(), Some("    "));
        assert_eq!(text(s, "for f in *; do").as_deref(), Some("  "));
        assert_eq!(text(s, "case \"$x\" in").as_deref(), Some("  "));
        assert_eq!(text(s, "  a*)").as_deref(), Some("    "));
        assert_eq!(text(s, "for f in a b"), None, "`in` opens only a case");
        assert_eq!(text(s, "# then"), None);
        assert_eq!(text(s, "echo done"), None);
        let fi = plan(s, "  fi", "", 2, false).unwrap();
        assert_eq!((fi.dedent, fi.text.as_str()), (2, ""));
        let els = plan(s, "    else", "", 2, false).unwrap();
        assert_eq!((els.dedent, els.text.as_str()), (2, "    "));
        let top = plan(s, "fi", "", 2, false).unwrap();
        assert_eq!(top.dedent, 0, "already at the margin");
    }

    #[test]
    fn block_comments_carry_on() {
        let j = Lang::Java;
        let open = plan(j, "    /**", "", 4, false).unwrap();
        assert_eq!((open.text.as_str(), open.close.as_deref()), ("     * ", Some("     */")));
        assert_eq!(text(j, "     * text").as_deref(), Some("     * "));
        assert_eq!(text(j, "     */"), None);
        assert_eq!(text(j, "int x = 2 * 3;"), None);
        assert_eq!(plan(j, "/**", " done */", 4, false), None);
    }

    #[test]
    fn python_opens_blocks() {
        assert_eq!(plan(Lang::Python, "def f():", "", 4, false).unwrap().text, "    ");
        assert_eq!(plan(Lang::Python, "# a:", "", 4, false), None);
    }
}
