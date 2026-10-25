//! LSP snippet syntax: `$1`, `${1:placeholder}` (placeholders may nest), `${1|a,b|}` choices
//! (the first is used), `$0` (the final cursor), variables (`$TM_FILENAME`, `${VAR:default}`;
//! only the default is used) and `\` escapes. Mirrored tab stops aren't linked: a repeated
//! number is a stop only the first time.

/// One tab stop: char offsets into [`Snippet::text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop {
    pub index: u32,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// In jump order: 1, 2, … then 0 (added at the end of the text when the snippet has stops
    /// but no `$0`). Empty for a snippet without stops.
    pub stops: Vec<Stop>,
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    out: String,
    len: usize,
    stops: Vec<Stop>,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn push(&mut self, c: char) {
        self.out.push(c);
        self.len += 1;
    }

    fn number(&mut self) -> Option<u32> {
        let s = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        (self.i > s).then(|| self.chars[s..self.i].iter().collect::<String>().parse().unwrap_or(0))
    }

    fn name(&mut self) -> Option<String> {
        let s = self.i;
        if !self.peek().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
            return None;
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            self.i += 1;
        }
        Some(self.chars[s..self.i].iter().collect())
    }

    fn stop(&mut self, index: u32, start: usize) {
        if !self.stops.iter().any(|s| s.index == index) {
            self.stops.push(Stop { index, start, end: self.len });
        }
    }

    /// Text until an unescaped `}` at this level (consumed) or the end (`in_braces` = false).
    fn any(&mut self, in_braces: bool) {
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '\\' => match self.peek() {
                    Some(n @ ('$' | '}' | '\\' | ',' | '|')) => {
                        self.i += 1;
                        self.push(n);
                    }
                    _ => self.push('\\'),
                },
                '}' if in_braces => return,
                '$' => self.dollar(),
                _ => self.push(c),
            }
        }
    }

    /// After a `$`.
    fn dollar(&mut self) {
        let start = self.len;
        if let Some(n) = self.number() {
            return self.stop(n, start);
        }
        if self.peek() == Some('{') {
            let back = self.i;
            self.i += 1;
            if let Some(n) = self.number() {
                match self.peek() {
                    Some('}') => self.i += 1,
                    Some(':') => {
                        self.i += 1;
                        self.any(true);
                    }
                    Some('|') => {
                        self.i += 1;
                        let mut first = true;
                        while let Some(c) = self.peek() {
                            self.i += 1;
                            match c {
                                '|' if self.peek() == Some('}') => {
                                    self.i += 1;
                                    break;
                                }
                                ',' => first = false,
                                '\\' => {
                                    if let Some(n) = self.peek() {
                                        self.i += 1;
                                        if first {
                                            self.push(n);
                                        }
                                    }
                                }
                                _ if first => self.push(c),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                return self.stop(n, start);
            }
            if self.name().is_some() {
                match self.peek() {
                    Some(':') => {
                        self.i += 1;
                        self.any(true);
                    }
                    Some('}') => self.i += 1,
                    // `${VAR/regex/format/}` transforms: skip to the closing brace.
                    _ => {
                        while let Some(c) = self.peek() {
                            self.i += 1;
                            if c == '}' {
                                break;
                            }
                        }
                    }
                }
                return;
            }
            self.i = back;
            return self.push('$');
        }
        if self.name().is_none() {
            self.push('$');
        }
    }
}

pub fn parse(src: &str) -> Snippet {
    let mut p = Parser { chars: src.chars().collect(), i: 0, out: String::new(), len: 0, stops: Vec::new() };
    p.any(false);
    let mut stops = p.stops;
    // 1, 2, 3 … then 0.
    stops.sort_by_key(|s| if s.index == 0 { u32::MAX } else { s.index });
    if !stops.is_empty() && stops.last().is_none_or(|s| s.index != 0) {
        stops.push(Stop { index: 0, start: p.len, end: p.len });
    }
    Snippet { text: p.out, stops }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(index: u32, start: usize, end: usize) -> Stop {
        Stop { index, start, end }
    }

    #[test]
    fn plain_text_has_no_stops() {
        assert_eq!(parse("hello"), Snippet { text: "hello".into(), stops: vec![] });
        assert_eq!(parse("cost \\$5 and $ alone").text, "cost $5 and $ alone");
    }

    #[test]
    fn numbered_stops_and_placeholders() {
        let s = parse("foo(${1:a}, ${2:b})$0");
        assert_eq!(s.text, "foo(a, b)");
        assert_eq!(s.stops, [st(1, 4, 5), st(2, 7, 8), st(0, 9, 9)]);
        let s = parse("vec![$0]");
        assert_eq!(s.text, "vec![]");
        assert_eq!(s.stops, [st(0, 5, 5)]);
        // No $0: the end of the text is the last stop.
        let s = parse("push($1)");
        assert_eq!(s.stops, [st(1, 5, 5), st(0, 6, 6)]);
        // Order is by number, not by position.
        let s = parse("$2-$1");
        assert_eq!(s.stops, [st(1, 1, 1), st(2, 0, 0), st(0, 1, 1)]);
    }

    #[test]
    fn nested_choices_variables_and_escapes() {
        let s = parse("${1:outer(${2:inner})} x");
        assert_eq!(s.text, "outer(inner) x");
        assert_eq!(s.stops, [st(1, 0, 12), st(2, 6, 11), st(0, 14, 14)]);
        let s = parse("${1|one,two|} ${2}");
        assert_eq!(s.text, "one ");
        assert_eq!(s.stops[..2], [st(1, 0, 3), st(2, 4, 4)]);
        assert_eq!(parse("${TM_SELECTED_TEXT:sel} $TM_FILENAME!").text, "sel !");
        assert_eq!(parse("${1:a\\}b}").text, "a}b");
        assert_eq!(parse("ü${1:ä}").stops[0], st(1, 1, 2), "offsets are chars");
        // A repeated number is a stop only the first time; its text is still inserted.
        let s = parse("${1:x} = ${1:x}");
        assert_eq!(s.text, "x = x");
        assert_eq!(s.stops, [st(1, 0, 1), st(0, 5, 5)]);
    }

    #[test]
    fn broken_snippets_degrade_to_text() {
        assert_eq!(parse("${").text, "${");
        assert_eq!(parse("${1:open").text, "open");
        assert_eq!(parse("a$").text, "a$");
    }
}
