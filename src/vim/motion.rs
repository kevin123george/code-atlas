//! Vim motions and text objects as pure functions over a `Doc`'s text.
//! Positions are char offsets, like everywhere else in the editor.

use crate::doc::Doc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// w / W (`true` = WORD: anything but blanks).
    WordFwd(bool),
    /// b / B
    WordBack(bool),
    /// e / E
    WordEnd(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    /// + or Enter
    NextLine,
    /// -
    PrevLine,
    /// gg ({count}gg = that line)
    Top,
    /// G ({count}G = that line)
    Bottom,
    Find { ch: char, fwd: bool, till: bool },
    /// ; (false) and , (true = the other way)
    RepeatFind(bool),
    /// %
    Match,
    /// }
    ParaFwd,
    /// {
    ParaBack,
    /// n (false) and N (true)
    SearchNext(bool),
    /// * (true) and # (false)
    Star(bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub pos: usize,
    pub kind: Kind,
}

impl Target {
    pub fn new(pos: usize, kind: Kind) -> Self {
        Target { pos, kind }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Obj {
    Word(bool),
    Quote(char),
    Pair(char, char),
}

/// The object named by the key after `i` / `a`.
pub fn obj_of(c: char) -> Option<Obj> {
    Some(match c {
        'w' => Obj::Word(false),
        'W' => Obj::Word(true),
        '"' | '\'' | '`' => Obj::Quote(c),
        '(' | ')' | 'b' => Obj::Pair('(', ')'),
        '{' | '}' | 'B' => Obj::Pair('{', '}'),
        '[' | ']' => Obj::Pair('[', ']'),
        '<' | '>' => Obj::Pair('<', '>'),
        _ => return None,
    })
}

/// 0 = blank, 1 = word character, 2 = punctuation. For WORDs everything non-blank is 1.
pub fn cls(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn ch(d: &Doc, p: usize) -> char {
    d.rope.char(p)
}

pub fn len(d: &Doc) -> usize {
    d.rope.len_chars()
}

pub fn line_of(d: &Doc, p: usize) -> usize {
    d.rope.char_to_line(p.min(len(d))).min(d.content_lines() - 1)
}

pub fn last_line(d: &Doc) -> usize {
    d.content_lines() - 1
}

pub fn indent_len(d: &Doc, line: usize) -> usize {
    d.rope.line(line).chars().take(d.line_len(line)).take_while(|c| *c == ' ' || *c == '\t').count()
}

pub fn first_non_blank(d: &Doc, line: usize) -> usize {
    d.line_start(line) + indent_len(d, line)
}

/// An empty line starts at `p`.
fn empty_line_at(d: &Doc, p: usize) -> bool {
    p < len(d) && matches!(ch(d, p), '\n' | '\r') && (p == 0 || ch(d, p - 1) == '\n')
}

pub fn word_fwd(d: &Doc, mut p: usize, big: bool) -> usize {
    let n = len(d);
    if p >= n {
        return n;
    }
    let c = cls(ch(d, p), big);
    if c != 0 {
        while p < n && cls(ch(d, p), big) == c {
            p += 1;
        }
    }
    while p < n {
        let x = ch(d, p);
        if x == '\n' {
            p += 1;
            if empty_line_at(d, p) {
                break;
            }
        } else if x.is_whitespace() {
            p += 1;
        } else {
            break;
        }
    }
    p
}

pub fn word_end(d: &Doc, p: usize, big: bool) -> usize {
    let n = len(d);
    if n == 0 {
        return 0;
    }
    let mut p = p + 1;
    while p < n && ch(d, p).is_whitespace() {
        p += 1;
    }
    if p >= n {
        return n - 1;
    }
    let c = cls(ch(d, p), big);
    while p + 1 < n && cls(ch(d, p + 1), big) == c {
        p += 1;
    }
    p
}

pub fn word_back(d: &Doc, p: usize, big: bool) -> usize {
    if p == 0 {
        return 0;
    }
    let mut p = p - 1;
    while p > 0 && ch(d, p).is_whitespace() {
        if empty_line_at(d, p) {
            return p;
        }
        p -= 1;
    }
    let c = cls(ch(d, p), big);
    while p > 0 && cls(ch(d, p - 1), big) == c {
        p -= 1;
    }
    p
}

/// f / F / t / T within the cursor line. `again` is set for ; and , so a `t` that already
/// stands before its character moves on to the next one.
pub fn find_char(d: &Doc, pos: usize, c: char, fwd: bool, till: bool, count: usize, again: bool) -> Option<usize> {
    let line = line_of(d, pos);
    let (ls, le) = (d.line_start(line), d.line_end(line));
    let skip = (again && till) as usize;
    let mut found = 0;
    if fwd {
        for p in pos + 1 + skip..le {
            if ch(d, p) == c {
                found += 1;
                if found == count {
                    return Some(if till { p - 1 } else { p });
                }
            }
        }
    } else {
        let end = pos.saturating_sub(skip);
        for p in (ls..end).rev() {
            if ch(d, p) == c {
                found += 1;
                if found == count {
                    return Some(if till { p + 1 } else { p });
                }
            }
        }
    }
    None
}

/// The first `open`/`close` at depth 0 scanning from `from` (inclusive) forwards or backwards.
fn unmatched(d: &Doc, from: usize, open: char, close: char, fwd: bool) -> Option<usize> {
    let mut depth = 0usize;
    if fwd {
        for p in from..len(d) {
            let c = ch(d, p);
            if c == open {
                depth += 1;
            } else if c == close {
                if depth == 0 {
                    return Some(p);
                }
                depth -= 1;
            }
        }
    } else {
        for p in (0..=from.min(len(d).saturating_sub(1))).rev() {
            let c = ch(d, p);
            if c == close {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    return Some(p);
                }
                depth -= 1;
            }
        }
    }
    None
}

/// %: the bracket at or after the cursor on its line, to its partner.
pub fn match_pair(d: &Doc, pos: usize) -> Option<usize> {
    let le = d.line_end(line_of(d, pos));
    let p = (pos..le).find(|p| "()[]{}".contains(ch(d, *p)))?;
    match ch(d, p) {
        '(' => unmatched(d, p + 1, '(', ')', true),
        '[' => unmatched(d, p + 1, '[', ']', true),
        '{' => unmatched(d, p + 1, '{', '}', true),
        ')' => p.checked_sub(1).and_then(|q| unmatched(d, q, '(', ')', false)),
        ']' => p.checked_sub(1).and_then(|q| unmatched(d, q, '[', ']', false)),
        _ => p.checked_sub(1).and_then(|q| unmatched(d, q, '{', '}', false)),
    }
}

fn blank(d: &Doc, line: usize) -> bool {
    d.line_len(line) == 0
}

/// } : the next empty line (or the end of the file).
pub fn para_fwd(d: &Doc, pos: usize, count: usize) -> usize {
    let last = last_line(d);
    let mut l = line_of(d, pos);
    for _ in 0..count {
        while l < last && blank(d, l) {
            l += 1;
        }
        while l < last && !blank(d, l) {
            l += 1;
        }
    }
    if l == last && !blank(d, l) {
        d.line_end(l)
    } else {
        d.line_start(l)
    }
}

/// { : the previous empty line (or the start of the file).
pub fn para_back(d: &Doc, pos: usize, count: usize) -> usize {
    let mut l = line_of(d, pos);
    for _ in 0..count {
        while l > 0 && blank(d, l) {
            l -= 1;
        }
        while l > 0 && !blank(d, l) {
            l -= 1;
        }
    }
    d.line_start(l)
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// The range `[s, e)` of a text object at `pos`.
pub fn object(d: &Doc, pos: usize, obj: Obj, inner: bool) -> Option<(usize, usize)> {
    if len(d) == 0 {
        return None;
    }
    let pos = pos.min(len(d) - 1);
    let line = line_of(d, pos);
    let (ls, le) = (d.line_start(line), d.line_end(line));
    match obj {
        Obj::Word(big) => {
            if pos >= le {
                return None;
            }
            let c = cls(ch(d, pos), big);
            let (mut s, mut e) = (pos, pos + 1);
            while s > ls && cls(ch(d, s - 1), big) == c {
                s -= 1;
            }
            while e < le && cls(ch(d, e), big) == c {
                e += 1;
            }
            if inner {
                return Some((s, e));
            }
            if c == 0 {
                // aw on blanks: the blanks and the word after them.
                if e < le {
                    let k = cls(ch(d, e), big);
                    while e < le && cls(ch(d, e), big) == k {
                        e += 1;
                    }
                }
                return Some((s, e));
            }
            let e0 = e;
            while e < le && is_blank(ch(d, e)) {
                e += 1;
            }
            if e == e0 {
                while s > ls && is_blank(ch(d, s - 1)) {
                    s -= 1;
                }
            }
            Some((s, e))
        }
        Obj::Quote(q) => {
            let quotes: Vec<usize> =
                (ls..le).filter(|p| ch(d, *p) == q && (*p == ls || ch(d, p - 1) != '\\')).collect();
            let pairs: Vec<(usize, usize)> = quotes.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0], c[1])).collect();
            let (a, b) = pairs
                .iter()
                .find(|(a, b)| *a <= pos && pos <= *b)
                .or_else(|| pairs.iter().find(|(a, _)| *a > pos))
                .copied()?;
            if inner {
                return Some((a + 1, b));
            }
            let mut e = b + 1;
            while e < le && is_blank(ch(d, e)) {
                e += 1;
            }
            let mut s = a;
            if e == b + 1 {
                while s > ls && is_blank(ch(d, s - 1)) {
                    s -= 1;
                }
            }
            Some((s, e))
        }
        Obj::Pair(open, close) => {
            let o = match ch(d, pos) {
                c if c == open => pos,
                c if c == close => unmatched(d, pos.checked_sub(1)?, open, close, false)?,
                _ => unmatched(d, pos.checked_sub(1)?, open, close, false)?,
            };
            let c = unmatched(d, o + 1, open, close, true)?;
            if !inner {
                return Some((o, c + 1));
            }
            let mut s = o + 1;
            let mut e = c;
            // A block over several lines: keep the lines of the brackets themselves.
            if s < e && ch(d, s) == '\n' {
                s += 1;
            } else if s + 1 < e && ch(d, s) == '\r' && ch(d, s + 1) == '\n' {
                s += 2;
            }
            let cl = line_of(d, c);
            let cls_ = d.line_start(cl);
            if cl > line_of(d, o) && (cls_..c).all(|p| is_blank(ch(d, p))) {
                e = cls_.max(s);
            }
            Some((s, e))
        }
    }
}
