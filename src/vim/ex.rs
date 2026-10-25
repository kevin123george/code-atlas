//! The command line: `/` and `?` searches and the `:` commands (w, q, wq, x, q!, wa, qa, e,
//! {n}, d, s/a/b/flags with ranges `%`, `n,m`, `.`, `$`, `'<,'>`). Patterns are plain text, not
//! regular expressions.

use super::motion::{first_non_blank, last_line, line_of};
use super::{Effect, Op, Range, Vim, K};
use crate::doc::{Clip, Doc};

impl Vim {
    pub(super) fn cmdline_key(&mut self, d: &mut Doc, clip: &mut Clip, k: K) -> Effect {
        let Some((_, buf)) = &mut self.cmdline else { return Effect::None };
        match k {
            K::Esc => self.cmdline = None,
            K::Back => {
                if buf.pop().is_none() {
                    self.cmdline = None;
                }
            }
            K::Ch(c) => buf.push(c),
            K::Enter => {
                let (kind, text) = self.cmdline.take().unwrap();
                return match kind {
                    ':' => self.ex(d, clip, &text),
                    _ => {
                        let fwd = kind == '/';
                        let q = if text.is_empty() {
                            match &self.last_search {
                                Some((q, _, _)) => q.clone(),
                                None => return Effect::Message("E35: No previous regular expression".into()),
                            }
                        } else {
                            text
                        };
                        self.last_search = Some((q.clone(), fwd, false));
                        if let Some(p) = self.search(d, &q, fwd, 1, false, d.cursor) {
                            d.cursor = p;
                            d.set_want_col(None);
                        }
                        Effect::None
                    }
                };
            }
            _ => {}
        }
        Effect::None
    }

    fn ex(&mut self, d: &mut Doc, clip: &mut Clip, text: &str) -> Effect {
        let t = text.trim();
        let (range, rest) = self.parse_range(d, t);
        let rest = rest.trim_start();
        let cur = line_of(d, d.cursor);
        if rest.is_empty() {
            if let Some((_, b)) = range {
                d.cursor = first_non_blank(d, b);
                d.set_want_col(None);
            }
            return Effect::None;
        }
        let mut chars = rest.chars();
        let first = chars.next().unwrap();
        let second = chars.next();
        if first == 's' && second.is_some_and(|c| !c.is_alphanumeric() && !c.is_whitespace()) {
            let (a, b) = range.unwrap_or((cur, cur));
            return self.substitute(d, a, b, &rest[1..]);
        }
        if rest == "d" {
            let (a, b) = range.unwrap_or((cur, cur));
            return self.apply(d, clip, Op::Delete, Range::Lines(a, b));
        }
        if range.is_some() {
            return Effect::Message("E481: No range allowed".into());
        }
        let (cmd, arg) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, ""),
        };
        let arg = (!arg.is_empty()).then(|| arg.to_string());
        match cmd {
            "w" | "w!" | "write" => Effect::Save(arg),
            "q" | "quit" | "clo" | "close" => Effect::Close { save: false, force: false, path: None },
            "q!" | "quit!" => Effect::Close { save: false, force: true, path: None },
            "wq" | "x" | "xit" | "exit" => Effect::Close { save: true, force: false, path: arg },
            "wq!" | "x!" => Effect::Close { save: true, force: true, path: arg },
            "wa" | "wall" => Effect::SaveAll,
            "qa" | "qall" | "quita" | "quitall" => Effect::QuitAll { save: false, force: false },
            "qa!" | "qall!" | "quitall!" => Effect::QuitAll { save: false, force: true },
            "wqa" | "wqall" | "xa" | "xall" => Effect::QuitAll { save: true, force: false },
            "e" | "edit" => match arg {
                Some(p) => Effect::Open(p),
                None => Effect::Message("E32: No file name".into()),
            },
            "noh" | "nohlsearch" => Effect::None,
            _ => Effect::Message(format!("E492: Not an editor command: {t}")),
        }
    }

    /// `%`, `n`, `n,m`, `.`, `$`, `'<`, `'>`, each optionally `+k` / `-k`. Lines are 0-based.
    fn parse_range<'a>(&self, d: &Doc, t: &'a str) -> (Option<(usize, usize)>, &'a str) {
        if let Some(rest) = t.strip_prefix('%') {
            return (Some((0, last_line(d))), rest);
        }
        let Some((a, rest)) = self.address(d, t) else { return (None, t) };
        if let Some(r) = rest.strip_prefix(',') {
            if let Some((b, rest)) = self.address(d, r) {
                return (Some((a.min(b), a.max(b))), rest);
            }
        }
        (Some((a, a)), rest)
    }

    fn address<'a>(&self, d: &Doc, t: &'a str) -> Option<(usize, &'a str)> {
        let last = last_line(d) as isize;
        let digits = |s: &str| s.chars().take_while(|c| c.is_ascii_digit()).count();
        let (mut line, mut rest) = if let Some(r) = t.strip_prefix('.') {
            (line_of(d, d.cursor) as isize, r)
        } else if let Some(r) = t.strip_prefix('$') {
            (last, r)
        } else if let Some(r) = t.strip_prefix("'<") {
            (self.last_visual.0 as isize, r)
        } else if let Some(r) = t.strip_prefix("'>") {
            (self.last_visual.1 as isize, r)
        } else {
            let n = digits(t);
            if n == 0 {
                return None;
            }
            (t[..n].parse::<isize>().ok()? - 1, &t[n..])
        };
        while let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
            let r = &rest[1..];
            let n = digits(r);
            let k: isize = if n == 0 { 1 } else { r[..n].parse().ok()? };
            line += if sign == '+' { k } else { -k };
            rest = &r[n..];
        }
        Some((line.clamp(0, last) as usize, rest))
    }

    /// `s/pat/rep/flags` on lines `a..=b`: `g` = every match on a line, `i` = ignore case.
    /// In the replacement `&` is the match (`\&` a plain &).
    fn substitute(&mut self, d: &mut Doc, a: usize, b: usize, spec: &str) -> Effect {
        let mut it = spec.chars();
        let Some(delim) = it.next() else { return Effect::None };
        let mut parts = vec![String::new()];
        let mut esc = false;
        for c in it {
            if esc {
                if c != delim && c != '\\' {
                    parts.last_mut().unwrap().push('\\');
                }
                parts.last_mut().unwrap().push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == delim && parts.len() < 3 {
                parts.push(String::new());
            } else {
                parts.last_mut().unwrap().push(c);
            }
        }
        let pat = if parts[0].is_empty() {
            match &self.last_search {
                Some((q, _, _)) => q.clone(),
                None => return Effect::Message("E35: No previous regular expression".into()),
            }
        } else {
            parts[0].clone()
        };
        let rep = parts.get(1).cloned().unwrap_or_default();
        let flags = parts.get(2).cloned().unwrap_or_default();
        let (global, icase) = (flags.contains('g'), flags.contains('i'));
        self.last_search = Some((pat.clone(), true, false));
        let (mut subs, mut lines, mut last) = (0, 0, None);
        // Bottom-up, so a line break in the replacement doesn't move lines still to do.
        for line in (a..=b.min(last_line(d))).rev() {
            let (ls, le) = (d.line_start(line), d.line_end(line));
            let text = d.rope.slice(ls..le).to_string();
            let (new, n) = replace_literal(&text, &pat, &rep, global, icase);
            if n > 0 {
                d.replace(ls, le, &new);
                subs += n;
                lines += 1;
                last.get_or_insert(line);
            }
        }
        let Some(last) = last else { return Effect::Message(format!("E486: Pattern not found: {pat}")) };
        d.cursor = first_non_blank(d, last.min(last_line(d)));
        d.set_want_col(None);
        if lines > 1 || subs > 1 {
            Effect::Message(format!("{subs} substitution{} on {lines} line{}", plural(subs), plural(lines)))
        } else {
            Effect::None
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Replaces the first (or every) plain-text match of `pat` in `text`. Returns the new text and
/// the number of replacements.
fn replace_literal(text: &str, pat: &str, rep: &str, global: bool, icase: bool) -> (String, usize) {
    if pat.is_empty() {
        return (text.to_string(), 0);
    }
    let (hay, needle) = if icase { (text.to_ascii_lowercase(), pat.to_ascii_lowercase()) } else { (text.to_string(), pat.to_string()) };
    let mut out = String::new();
    let mut i = 0;
    let mut n = 0;
    while let Some(off) = hay[i..].find(&needle) {
        let s = i + off;
        let e = s + needle.len();
        out.push_str(&text[i..s]);
        out.push_str(&expand(rep, &text[s..e]));
        i = e;
        n += 1;
        if !global {
            break;
        }
    }
    out.push_str(&text[i..]);
    (out, n)
}

fn expand(rep: &str, m: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in rep.chars() {
        match (esc, c) {
            (true, '&') => out.push('&'),
            (true, 'n') | (true, 'r') => out.push('\n'),
            (true, c) => {
                out.push('\\');
                out.push(c);
            }
            (false, '\\') => {
                esc = true;
                continue;
            }
            (false, '&') => out.push_str(m),
            (false, c) => out.push(c),
        }
        esc = false;
    }
    out
}
