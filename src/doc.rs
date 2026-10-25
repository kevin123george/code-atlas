//! One open file: the text (a rope), the cursor and selection, undo history and scroll position.
//! Positions are char offsets into the rope; columns shown on screen are "display columns"
//! (tabs expanded, wide characters counted twice).

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;
use unicode_width::UnicodeWidthChar;

pub const TAB: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EditKind {
    None,
    Typing,
    Deleting,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    rope: Rope,
    cursor: usize,
    anchor: Option<usize>,
    rev: u64,
}

/// What a copy put on the clipboard: a copy without a selection takes the whole line,
/// and pasting it puts it above the current line (as IntelliJ does).
#[derive(Clone, Default)]
pub struct Clip {
    pub text: String,
    pub whole_line: bool,
}

pub struct Doc {
    pub rope: Rope,
    pub path: Option<PathBuf>,
    pub cursor: usize,
    pub anchor: Option<usize>,
    /// Display column kept while moving up and down through shorter lines.
    want_col: Option<usize>,
    pub scroll_line: usize,
    pub scroll_col: usize,
    /// Height of the text area at the last draw, for Page Up/Down.
    pub view_h: usize,
    pub crlf: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: EditKind,
    rev: u64,
    next_rev: u64,
    saved_rev: u64,
}

impl Doc {
    pub fn empty() -> Self {
        Self::from_rope(Rope::new(), None)
    }

    #[cfg(test)]
    pub fn from_text(text: &str) -> Self {
        Self::from_rope(Rope::from_str(text), None)
    }

    /// Opens `path`; a file that doesn't exist yet becomes an empty document saved there later.
    pub fn open(path: &Path) -> Result<Self> {
        let rope = if path.exists() {
            let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            Rope::from_str(&text)
        } else {
            Rope::new()
        };
        Ok(Self::from_rope(rope, Some(path.to_path_buf())))
    }

    fn from_rope(rope: Rope, path: Option<PathBuf>) -> Self {
        let crlf = rope.chars().take(100_000).collect::<String>().contains("\r\n");
        Doc {
            rope,
            path,
            cursor: 0,
            anchor: None,
            want_col: None,
            scroll_line: 0,
            scroll_col: 0,
            view_h: 20,
            crlf,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: EditKind::None,
            rev: 0,
            next_rev: 1,
            saved_rev: 0,
        }
    }

    pub fn save(&mut self) -> Result<()> {
        let path = self.path.clone().context("no file name")?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let mut w = BufWriter::new(fs::File::create(&path).with_context(|| format!("writing {}", path.display()))?);
        self.rope.write_to(&mut w)?;
        w.flush()?;
        self.saved_rev = self.rev;
        self.last_edit = EditKind::None;
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into())
    }

    pub fn eol(&self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }

    // ---- positions -------------------------------------------------------------------------

    pub fn line(&self) -> usize {
        self.rope.char_to_line(self.cursor)
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line)
    }

    /// Chars in `line` without its line break.
    pub fn line_len(&self, line: usize) -> usize {
        let s = self.rope.line(line);
        let mut n = s.len_chars();
        if n > 0 && s.char(n - 1) == '\n' {
            n -= 1;
            if n > 0 && s.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        n
    }

    pub fn line_end(&self, line: usize) -> usize {
        self.line_start(line) + self.line_len(line)
    }

    pub fn col(&self) -> usize {
        self.cursor - self.line_start(self.line())
    }

    /// Number of lines a user sees: a final line break doesn't open an extra empty line.
    pub fn visible_lines(&self) -> usize {
        let n = self.rope.len_lines();
        if n > 1 && self.rope.line(n - 1).len_chars() == 0 { n - 1 } else { n }
    }

    pub fn display_col(&self, line: usize, char_col: usize) -> usize {
        let mut col = 0;
        for c in self.rope.line(line).chars().take(char_col) {
            col += char_width(c, col);
        }
        col
    }

    /// The char column in `line` closest to display column `target`.
    pub fn char_col_at(&self, line: usize, target: usize) -> usize {
        let mut col = 0;
        let len = self.line_len(line);
        for (i, c) in self.rope.line(line).chars().take(len).enumerate() {
            let w = char_width(c, col);
            if col + w > target {
                return if target - col > w / 2 { i + 1 } else { i };
            }
            col += w;
        }
        len
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| self.rope.slice(s..e).to_string())
    }

    /// First and last line touched by the selection (or the cursor line). A selection that
    /// ends at the start of a line doesn't count that line.
    fn line_span(&self) -> (usize, usize) {
        match self.selection() {
            Some((s, e)) => {
                let a = self.rope.char_to_line(s);
                let mut b = self.rope.char_to_line(e);
                if b > a && e == self.line_start(b) {
                    b -= 1;
                }
                (a, b)
            }
            None => (self.line(), self.line()),
        }
    }

    // ---- movement --------------------------------------------------------------------------

    pub fn move_to(&mut self, pos: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos.min(self.rope.len_chars());
        self.last_edit = EditKind::None;
    }

    fn move_h(&mut self, pos: usize, select: bool) {
        self.want_col = None;
        self.move_to(pos, select);
    }

    pub fn left(&mut self, select: bool) {
        match self.selection() {
            Some((s, _)) if !select => self.move_h(s, false),
            _ => {
                let mut p = self.cursor.saturating_sub(1);
                if p > 0 && self.rope.char(p) == '\n' && self.rope.char(p - 1) == '\r' {
                    p -= 1;
                }
                self.move_h(p, select)
            }
        }
    }

    pub fn right(&mut self, select: bool) {
        match self.selection() {
            Some((_, e)) if !select => self.move_h(e, false),
            _ => {
                let len = self.rope.len_chars();
                let mut p = (self.cursor + 1).min(len);
                if p < len && self.rope.char(p - 1) == '\r' && self.rope.char(p) == '\n' {
                    p += 1;
                }
                self.move_h(p, select)
            }
        }
    }

    pub fn vertical(&mut self, delta: isize, select: bool) {
        let line = self.line();
        let want = self.want_col.unwrap_or_else(|| self.display_col(line, self.col()));
        let last = self.visible_lines() - 1;
        let target = (line as isize + delta).clamp(0, last as isize) as usize;
        let pos = if delta < 0 && line == 0 {
            0
        } else if delta > 0 && line == last {
            self.line_end(last)
        } else {
            self.line_start(target) + self.char_col_at(target, want)
        };
        self.move_to(pos, select);
        self.want_col = Some(want);
    }

    pub fn page(&mut self, down: bool, select: bool) {
        let h = self.view_h.max(2) as isize - 1;
        self.vertical(if down { h } else { -h }, select);
        if down {
            self.scroll_line = (self.scroll_line + h as usize).min(self.visible_lines().saturating_sub(1));
        } else {
            self.scroll_line = self.scroll_line.saturating_sub(h as usize);
        }
    }

    /// Home goes to the first non-blank character, and from there to column 0.
    pub fn home(&mut self, select: bool) {
        let line = self.line();
        let start = self.line_start(line);
        let indent = self.rope.line(line).chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let first = start + indent.min(self.line_len(line));
        self.move_h(if self.cursor == first { start } else { first }, select);
    }

    pub fn end(&mut self, select: bool) {
        self.move_h(self.line_end(self.line()), select);
    }

    pub fn doc_start(&mut self, select: bool) {
        self.move_h(0, select);
    }

    pub fn doc_end(&mut self, select: bool) {
        self.move_h(self.rope.len_chars(), select);
    }

    pub fn word_left(&mut self, select: bool) {
        let mut p = self.cursor;
        while p > 0 && class(self.rope.char(p - 1)) == 0 {
            p -= 1;
        }
        if p > 0 {
            let k = class(self.rope.char(p - 1));
            while p > 0 && class(self.rope.char(p - 1)) == k {
                p -= 1;
            }
        }
        self.move_h(p, select);
    }

    pub fn word_right(&mut self, select: bool) {
        let len = self.rope.len_chars();
        let mut p = self.cursor;
        if p < len {
            let k = class(self.rope.char(p));
            while p < len && class(self.rope.char(p)) == k {
                p += 1;
            }
        }
        while p < len && class(self.rope.char(p)) == 0 && self.rope.char(p) != '\n' {
            p += 1;
        }
        self.move_h(p, select);
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.rope.len_chars();
        self.last_edit = EditKind::None;
    }

    /// Selects `[start, end)` with the cursor at the end.
    pub fn select(&mut self, start: usize, end: usize) {
        self.anchor = Some(start);
        self.cursor = end;
        self.want_col = None;
        self.last_edit = EditKind::None;
    }

    pub fn goto_line(&mut self, line: usize, col: usize) {
        let line = line.min(self.visible_lines() - 1);
        let col = col.min(self.line_len(line));
        self.move_h(self.line_start(line) + col, false);
    }

    // ---- undo ------------------------------------------------------------------------------

    /// Records the state before an edit. Runs of typing (or deleting) form one undo step.
    fn begin(&mut self, kind: EditKind) {
        if kind == EditKind::Other || kind != self.last_edit {
            self.undo.push(Snapshot { rope: self.rope.clone(), cursor: self.cursor, anchor: self.anchor, rev: self.rev });
            if self.undo.len() > 1000 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = kind;
        self.rev = self.next_rev;
        self.next_rev += 1;
        self.want_col = None;
    }

    fn restore(&mut self, s: Snapshot) -> Snapshot {
        let current = Snapshot { rope: self.rope.clone(), cursor: self.cursor, anchor: self.anchor, rev: self.rev };
        self.rope = s.rope;
        self.cursor = s.cursor.min(self.rope.len_chars());
        self.anchor = s.anchor;
        self.rev = s.rev;
        self.last_edit = EditKind::None;
        self.want_col = None;
        current
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(s) => {
                let cur = self.restore(s);
                self.redo.push(cur);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(s) => {
                let cur = self.restore(s);
                self.undo.push(cur);
                true
            }
            None => false,
        }
    }

    // ---- edits -----------------------------------------------------------------------------

    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((s, e)) => {
                self.rope.remove(s..e);
                self.cursor = s;
                self.anchor = None;
                true
            }
            None => {
                self.anchor = None;
                false
            }
        }
    }

    fn insert_raw(&mut self, text: &str) {
        self.rope.insert(self.cursor, text);
        self.cursor += text.chars().count();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.begin(EditKind::Other);
        self.delete_selection();
        let text = normalize_eol(text, self.eol());
        self.insert_raw(&text);
    }

    /// Typing one character, with IntelliJ's bracket and quote pairing.
    pub fn type_char(&mut self, c: char) {
        let next = self.rope.get_char(self.cursor);
        let prev = self.cursor.checked_sub(1).map(|p| self.rope.char(p));
        if self.selection().is_none() && matches!(c, ')' | ']' | '}' | '"' | '\'') && next == Some(c) {
            self.move_h(self.cursor + 1, false);
            return;
        }
        if let Some((s, e)) = self.selection() {
            // A bracket or quote typed over a selection wraps it, whatever is around it.
            let wrap = match c {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                '"' | '\'' => Some(c),
                _ => None,
            };
            if let Some(close) = wrap {
                self.begin(EditKind::Other);
                self.rope.insert_char(e, close);
                self.rope.insert_char(s, c);
                self.select(s + 1, e + 1);
                return;
            }
        }
        let close = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            '"' | '\'' if !prev.is_some_and(|p| p.is_alphanumeric() || p == c) => Some(c),
            _ => None,
        };
        let close = close.filter(|_| next.is_none_or(|n| n.is_whitespace() || ")]};,".contains(n)));
        self.begin(if c.is_whitespace() { EditKind::Other } else { EditKind::Typing });
        self.delete_selection();
        self.rope.insert_char(self.cursor, c);
        self.cursor += 1;
        if let Some(close) = close {
            self.rope.insert_char(self.cursor, close);
        }
    }

    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            self.begin(EditKind::Other);
            self.delete_selection();
            return;
        }
        self.anchor = None;
        if self.cursor == 0 {
            return;
        }
        self.begin(EditKind::Deleting);
        let mut s = self.cursor - 1;
        let mut e = self.cursor;
        let c = self.rope.char(s);
        if c == '\n' && s > 0 && self.rope.char(s - 1) == '\r' {
            s -= 1;
        } else if let Some(n) = self.rope.get_char(e) {
            if matches!((c, n), ('(', ')') | ('[', ']') | ('{', '}') | ('"', '"') | ('\'', '\'')) {
                e += 1;
            }
        }
        self.rope.remove(s..e);
        self.cursor = s;
    }

    pub fn delete(&mut self) {
        if self.selection().is_some() {
            self.begin(EditKind::Other);
            self.delete_selection();
            return;
        }
        self.anchor = None;
        let len = self.rope.len_chars();
        if self.cursor >= len {
            return;
        }
        self.begin(EditKind::Deleting);
        let mut e = self.cursor + 1;
        if self.rope.char(self.cursor) == '\r' && e < len && self.rope.char(e) == '\n' {
            e += 1;
        }
        self.rope.remove(self.cursor..e);
    }

    pub fn delete_word_left(&mut self) {
        if self.selection().is_none() {
            let end = self.cursor;
            self.word_left(false);
            self.anchor = Some(end);
        }
        self.begin(EditKind::Other);
        self.delete_selection();
    }

    pub fn delete_word_right(&mut self) {
        if self.selection().is_none() {
            let start = self.cursor;
            self.word_right(false);
            self.anchor = Some(start);
        }
        self.begin(EditKind::Other);
        self.delete_selection();
    }

    /// Enter keeps the indent; between brackets it opens an indented line.
    pub fn newline(&mut self) {
        self.begin(EditKind::Other);
        self.delete_selection();
        let line = self.line();
        let start = self.line_start(line);
        let indent: String = self
            .rope
            .slice(start..self.cursor)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let prev = self.cursor.checked_sub(1).map(|p| self.rope.char(p));
        let next = self.rope.get_char(self.cursor);
        let eol = self.eol();
        if matches!((prev, next), (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))) {
            let inner = format!("{eol}{indent}{}", " ".repeat(TAB));
            self.insert_raw(&inner);
            let back = self.cursor;
            self.insert_raw(&format!("{eol}{indent}"));
            self.cursor = back;
        } else {
            self.insert_raw(&format!("{eol}{indent}"));
        }
    }

    /// Tab indents the selected lines, or inserts spaces up to the next tab stop.
    pub fn tab(&mut self) {
        let (a, b) = self.line_span();
        if self.selection().is_some() && b > a {
            self.indent_lines(a, b, true);
            return;
        }
        self.begin(EditKind::Other);
        self.delete_selection();
        let line = self.line();
        let col = self.display_col(line, self.col());
        let n = TAB - col % TAB;
        self.insert_raw(&" ".repeat(n));
    }

    pub fn untab(&mut self) {
        let (a, b) = self.line_span();
        self.indent_lines(a, b, false);
    }

    fn indent_lines(&mut self, a: usize, b: usize, add: bool) {
        self.begin(EditKind::Other);
        let (mut cur, mut anc) = (self.cursor, self.anchor);
        for line in a..=b {
            let start = self.line_start(line);
            let shift = |p: usize, d: isize| if p >= start { (p as isize + d).max(start as isize) as usize } else { p };
            if add {
                if self.line_len(line) == 0 {
                    continue;
                }
                self.rope.insert(start, &" ".repeat(TAB));
                cur = shift(cur, TAB as isize);
                anc = anc.map(|p| shift(p, TAB as isize));
            } else {
                let chars = self.rope.line(line);
                let n = if chars.get_char(0) == Some('\t') {
                    1
                } else {
                    chars.chars().take(TAB).take_while(|c| *c == ' ').count()
                };
                if n == 0 {
                    continue;
                }
                self.rope.remove(start..start + n);
                cur = shift(cur, -(n as isize));
                anc = anc.map(|p| shift(p, -(n as isize)));
            }
        }
        self.cursor = cur;
        self.anchor = anc;
    }

    /// Ctrl+D: duplicates the selection, or the current line.
    pub fn duplicate(&mut self) {
        self.begin(EditKind::Other);
        match self.selection() {
            Some((s, e)) => {
                let text = self.rope.slice(s..e).to_string();
                self.rope.insert(e, &text);
                self.select(e, e + (e - s));
            }
            None => {
                let line = self.line();
                let (start, end) = (self.line_start(line), self.line_end(line));
                let text = self.rope.slice(start..end).to_string();
                let eol = self.eol();
                self.rope.insert(end, &format!("{eol}{text}"));
                self.cursor += text.chars().count() + eol.len();
            }
        }
    }

    /// Ctrl+Y: deletes the current line(s).
    pub fn delete_lines(&mut self) {
        let (a, b) = self.line_span();
        self.begin(EditKind::Other);
        let col = self.display_col(self.line(), self.col());
        let start = self.line_start(a);
        let end = if b + 1 < self.rope.len_lines() { self.line_start(b + 1) } else { self.rope.len_chars() };
        // Deleting a last line without a line break of its own also removes the break before it;
        // a file's final line break stays.
        let no_eol = end > 0 && self.rope.char(end - 1) != '\n';
        let start = if end == self.rope.len_chars() && no_eol && a > 0 && b + 1 >= self.visible_lines() {
            self.line_end(a - 1)
        } else {
            start
        };
        self.rope.remove(start..end);
        self.anchor = None;
        let line = self.rope.char_to_line(start.min(self.rope.len_chars()));
        let line = line.min(self.visible_lines() - 1);
        self.cursor = self.line_start(line) + self.char_col_at(line, col);
    }

    /// Alt+Shift+Up/Down: moves the current line(s) past the neighbouring line.
    pub fn move_lines(&mut self, down: bool) {
        let (a, b) = self.line_span();
        let last = self.visible_lines() - 1;
        if (down && b >= last) || (!down && a == 0) {
            return;
        }
        self.begin(EditKind::Other);
        // Swap two adjacent blocks: first = lines [x, y], second = lines [y+1, z].
        let (x, y, z) = if down { (a, b, b + 1) } else { (a - 1, a - 1, b) };
        let start = self.line_start(x);
        let mid = self.line_start(y + 1);
        let end = if z + 1 < self.rope.len_lines() { self.line_start(z + 1) } else { self.rope.len_chars() };
        let mut first = self.rope.slice(start..mid).to_string();
        let mut second = self.rope.slice(mid..end).to_string();
        let eol = self.eol();
        if !second.ends_with('\n') {
            second.push_str(eol);
            first.truncate(first.len() - eol.len());
        }
        let second_len = second.chars().count();
        self.rope.remove(start..end);
        self.rope.insert(start, &format!("{second}{first}"));
        let shift = |p: usize| {
            if down {
                p + second_len
            } else {
                start + (p - mid)
            }
        };
        self.cursor = shift(self.cursor);
        self.anchor = self.anchor.map(shift);
    }

    pub fn copy(&self) -> Clip {
        match self.selected_text() {
            Some(text) => Clip { text, whole_line: false },
            None => {
                let line = self.line();
                let end = if line + 1 < self.rope.len_lines() { self.line_start(line + 1) } else { self.rope.len_chars() };
                let mut text = self.rope.slice(self.line_start(line)..end).to_string();
                if !text.ends_with('\n') {
                    text.push_str(self.eol());
                }
                Clip { text, whole_line: true }
            }
        }
    }

    pub fn cut(&mut self) -> Clip {
        let clip = self.copy();
        if clip.whole_line {
            self.delete_lines();
        } else {
            self.begin(EditKind::Other);
            self.delete_selection();
        }
        clip
    }

    pub fn paste(&mut self, clip: &Clip) {
        if clip.whole_line && self.selection().is_none() {
            self.begin(EditKind::Other);
            let start = self.line_start(self.line());
            let text = normalize_eol(&clip.text, self.eol());
            self.rope.insert(start, &text);
            self.cursor += text.chars().count();
        } else {
            self.insert_str(&clip.text);
        }
    }

    /// Finds `query` after (or before) the cursor, wrapping around, and selects it.
    /// Lowercase queries ignore ASCII case ("smart case").
    pub fn find(&mut self, query: &str, backwards: bool) -> bool {
        if query.is_empty() {
            return false;
        }
        let text = self.rope.to_string();
        let fold = !query.chars().any(|c| c.is_uppercase());
        let (hay, needle) = if fold {
            (text.to_ascii_lowercase(), query.to_ascii_lowercase())
        } else {
            (text, query.to_string())
        };
        let (sel_s, sel_e) = self.selection().unwrap_or((self.cursor, self.cursor));
        let hit = if backwards {
            let before = self.rope.char_to_byte(sel_s);
            hay[..before].rfind(&needle).or_else(|| hay.rfind(&needle))
        } else {
            let after = self.rope.char_to_byte(sel_e);
            hay[after..].find(&needle).map(|i| i + after).or_else(|| hay.find(&needle))
        };
        match hit {
            Some(b) => {
                let s = self.rope.byte_to_char(b);
                let e = self.rope.byte_to_char(b + needle.len());
                self.select(s, e);
                true
            }
            None => false,
        }
    }

    pub fn ensure_cursor_visible(&mut self, h: usize, w: usize) {
        let line = self.line();
        if line < self.scroll_line {
            self.scroll_line = line;
        } else if h > 0 && line >= self.scroll_line + h {
            self.scroll_line = line + 1 - h;
        }
        let col = self.display_col(line, self.col());
        if col < self.scroll_col {
            self.scroll_col = col.saturating_sub(4);
        } else if w > 0 && col >= self.scroll_col + w {
            self.scroll_col = col + 5 - w.min(col + 5);
        }
    }
}

/// Display width of `c` at display column `col` (tabs run to the next tab stop).
pub fn char_width(c: char, col: usize) -> usize {
    match c {
        '\t' => TAB - col % TAB,
        '\n' | '\r' => 0,
        c => c.width().unwrap_or(1).max(if c.is_control() { 1 } else { 0 }),
    }
}

/// 0 = space, 1 = word character, 2 = punctuation.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn normalize_eol(text: &str, eol: &str) -> String {
    let lf = text.replace("\r\n", "\n");
    if eol == "\n" { lf } else { lf.replace('\n', eol) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, cursor: usize) -> Doc {
        let mut d = Doc::from_text(text);
        d.cursor = cursor;
        d
    }

    fn text(d: &Doc) -> String {
        d.rope.to_string()
    }

    #[test]
    fn typing_groups_into_one_undo_step() {
        let mut d = doc("", 0);
        for c in "hello".chars() {
            d.type_char(c);
        }
        assert_eq!(text(&d), "hello");
        assert!(d.dirty());
        d.undo();
        assert_eq!(text(&d), "");
        assert!(!d.dirty());
        d.redo();
        assert_eq!(text(&d), "hello");
    }

    #[test]
    fn brackets_pair_and_skip_over() {
        let mut d = doc("", 0);
        d.type_char('(');
        assert_eq!(text(&d), "()");
        d.type_char('a');
        d.type_char(')');
        assert_eq!(text(&d), "(a)");
        assert_eq!(d.cursor, 3);
        let mut d = doc("", 0);
        d.type_char('[');
        d.backspace();
        assert_eq!(text(&d), "");
    }

    #[test]
    fn apostrophe_in_a_word_does_not_pair() {
        let mut d = doc("don", 3);
        d.type_char('\'');
        assert_eq!(text(&d), "don'");
    }

    #[test]
    fn enter_keeps_indent_and_opens_braces() {
        let mut d = doc("    fn x() {}", 12);
        d.newline();
        assert_eq!(text(&d), "    fn x() {\n        \n    }");
        assert_eq!(d.line(), 1);
        assert_eq!(d.col(), 8);
    }

    #[test]
    fn smart_home_toggles() {
        let mut d = doc("    abc", 6);
        d.home(false);
        assert_eq!(d.cursor, 4);
        d.home(false);
        assert_eq!(d.cursor, 0);
    }

    #[test]
    fn up_down_keep_column() {
        let mut d = doc("abcdef\nab\nabcdef", 5);
        d.vertical(1, false);
        assert_eq!(d.cursor, 9); // end of "ab"
        d.vertical(1, false);
        assert_eq!(d.cursor, 15); // column 5 again
    }

    #[test]
    fn duplicate_and_delete_line() {
        let mut d = doc("a\nb\n", 0);
        d.duplicate();
        assert_eq!(text(&d), "a\na\nb\n");
        assert_eq!(d.line(), 1);
        d.delete_lines();
        assert_eq!(text(&d), "a\nb\n");
        let mut d = doc("a\nb", 2);
        d.delete_lines();
        assert_eq!(text(&d), "a");
    }

    #[test]
    fn move_lines_up_and_down() {
        let mut d = doc("one\ntwo\nthree", 1);
        d.move_lines(true);
        assert_eq!(text(&d), "two\none\nthree");
        assert_eq!(d.line(), 1);
        assert_eq!(d.col(), 1);
        d.move_lines(true);
        assert_eq!(text(&d), "two\nthree\none");
        assert_eq!(d.line(), 2);
        d.move_lines(false);
        assert_eq!(text(&d), "two\none\nthree");
        assert_eq!(d.line(), 1);
        d.move_lines(false);
        d.move_lines(false);
        assert_eq!(text(&d), "one\ntwo\nthree");
    }

    #[test]
    fn copy_without_selection_takes_the_line() {
        let mut d = doc("a\nb", 2);
        let clip = d.copy();
        assert_eq!(clip.text, "b\n");
        d.cursor = 0;
        d.paste(&clip);
        assert_eq!(text(&d), "b\na\nb");
    }

    #[test]
    fn tab_indents_selected_lines() {
        let mut d = doc("a\nb\n", 0);
        d.select(0, 3);
        d.tab();
        assert_eq!(text(&d), "    a\n    b\n");
        d.untab();
        assert_eq!(text(&d), "a\nb\n");
    }

    #[test]
    fn find_wraps_and_uses_smart_case() {
        let mut d = doc("Foo foo FOO", 5);
        assert!(d.find("foo", false));
        assert_eq!(d.selection(), Some((8, 11)));
        assert!(d.find("foo", false));
        assert_eq!(d.selection(), Some((0, 3)));
        assert!(d.find("Foo", false));
        assert_eq!(d.selection(), Some((0, 3)));
    }

    #[test]
    fn deleting_the_last_line_keeps_the_final_line_break() {
        let mut d = doc("1\n3\n", 2);
        d.delete_lines();
        assert_eq!(text(&d), "1\n");
        assert_eq!(d.line(), 0);
        let mut d = doc("1\r\n3\r\n", 3);
        d.delete_lines();
        assert_eq!(text(&d), "1\r\n");
    }

    #[test]
    fn brackets_and_quotes_wrap_any_selection() {
        // Selection "abc" inside "(abc) def": the quote must wrap, not replace.
        let mut d = doc("(abc) def", 1);
        d.select(1, 4);
        d.type_char('"');
        assert_eq!(text(&d), "(\"abc\") def");
        assert_eq!(d.selected_text().as_deref(), Some("abc"));
        // Selection inside a word ("xy" of "xyz"): next char is a letter.
        let mut d = doc("xyz", 0);
        d.select(0, 2);
        d.type_char('[');
        assert_eq!(text(&d), "[xy]z");
        let mut d = doc("it", 0);
        d.select(0, 2);
        d.type_char('\'');
        assert_eq!(text(&d), "'it'");
    }

    #[test]
    fn crlf_is_kept() {
        let mut d = doc("a\r\nb", 1);
        d.newline();
        assert_eq!(text(&d), "a\r\n\r\nb");
        d.right(false);
        assert_eq!(d.line(), 2);
        d.left(false);
        d.backspace();
        assert_eq!(text(&d), "a\r\nb");
    }

    #[test]
    fn tabs_and_wide_chars_in_display_columns() {
        let d = doc("\tx界y", 0);
        assert_eq!(d.display_col(0, 1), 4);
        assert_eq!(d.display_col(0, 3), 7);
        assert_eq!(d.char_col_at(0, 7), 3);
    }
}
