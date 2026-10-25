//! The panel's multi-line prompt box: a small text editor over a `String` (cursor = char index).

use unicode_width::UnicodeWidthChar;

#[derive(Default, Debug)]
pub struct Input {
    pub text: String,
    /// Char index into `text`.
    pub cursor: usize,
}

impl Input {
    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map_or(self.text.len(), |(b, _)| b)
    }

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn insert(&mut self, c: char) {
        let b = self.byte(self.cursor);
        self.text.insert(b, c);
        self.cursor += 1;
    }

    /// Pastes text (CRLF becomes LF, tabs become spaces).
    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        let b = self.byte(self.cursor);
        self.text.insert_str(b, &s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let b = self.byte(self.cursor);
            self.text.remove(b);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.chars().count() {
            let b = self.byte(self.cursor);
            self.text.remove(b);
        }
    }

    pub fn delete_word_left(&mut self) {
        let c = self.chars();
        let mut i = self.cursor;
        while i > 0 && !c[i - 1].is_alphanumeric() && c[i - 1] != '\n' {
            i -= 1;
        }
        while i > 0 && c[i - 1].is_alphanumeric() {
            i -= 1;
        }
        if i == self.cursor && i > 0 {
            i -= 1; // a lone line break
        }
        let (a, b) = (self.byte(i), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor = i;
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    /// (line start, line end) char indices around the cursor.
    fn line_bounds(&self, at: usize) -> (usize, usize) {
        let c = self.chars();
        let start = c[..at].iter().rposition(|&x| x == '\n').map_or(0, |i| i + 1);
        let end = c[at..].iter().position(|&x| x == '\n').map_or(c.len(), |i| at + i);
        (start, end)
    }

    pub fn home(&mut self) {
        self.cursor = self.line_bounds(self.cursor).0;
    }

    pub fn end(&mut self) {
        self.cursor = self.line_bounds(self.cursor).1;
    }

    /// Moves one logical line up/down keeping the column; false when there's no line there.
    pub fn vertical(&mut self, down: bool) -> bool {
        let (start, end) = self.line_bounds(self.cursor);
        let col = self.cursor - start;
        let len = self.text.chars().count();
        if down {
            if end >= len {
                return false;
            }
            let (s, e) = self.line_bounds(end + 1);
            self.cursor = (s + col).min(e);
        } else {
            if start == 0 {
                return false;
            }
            let (s, e) = self.line_bounds(start - 1);
            self.cursor = (s + col).min(e);
        }
        true
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Hard-wraps the text to `width` display columns. Returns the rows and the cursor's
    /// (row, column).
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut rows = vec![String::new()];
        let mut col = 0;
        let mut cur = (0, 0);
        for (i, ch) in self.text.chars().enumerate() {
            if i == self.cursor {
                cur = (rows.len() - 1, col);
            }
            if ch == '\n' {
                rows.push(String::new());
                col = 0;
                continue;
            }
            let w = ch.width().unwrap_or(0);
            if col + w > width {
                rows.push(String::new());
                col = 0;
                if i == self.cursor {
                    cur = (rows.len() - 1, 0);
                }
            }
            rows.last_mut().unwrap().push(ch);
            col += w;
        }
        if self.cursor >= self.text.chars().count() {
            cur = if col >= width { (rows.len(), 0) } else { (rows.len() - 1, col) };
            if col >= width {
                rows.push(String::new());
            }
        }
        (rows, cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(s: &str) -> Input {
        Input { text: s.into(), cursor: s.chars().count() }
    }

    #[test]
    fn editing_and_newlines() {
        let mut i = Input::default();
        for c in "héllo".chars() {
            i.insert(c);
        }
        i.insert('\n');
        i.insert_str("wörld\r\nx");
        assert_eq!(i.text, "héllo\nwörld\nx");
        i.backspace();
        i.backspace();
        assert_eq!(i.text, "héllo\nwörld");
        i.delete_word_left();
        assert_eq!(i.text, "héllo\n");
        i.delete_word_left();
        assert_eq!(i.text, "héllo");
        assert_eq!(i.cursor, 5);
    }

    #[test]
    fn up_down_keep_the_column() {
        let mut i = input("abcdef\nxy\n12345");
        assert!(i.vertical(false));
        assert_eq!(i.cursor, 7 + 2); // end of "xy"
        assert!(i.vertical(false));
        assert_eq!(i.cursor, 2);
        assert!(!i.vertical(false));
        i.end();
        assert_eq!(i.cursor, 6);
        assert!(i.vertical(true) && i.vertical(true));
        assert_eq!(i.cursor, 10 + 2); // the column of "xy"'s end
        assert!(!i.vertical(true));
        i.home();
        assert_eq!(i.cursor, 10);
    }

    #[test]
    fn layout_wraps_and_places_the_cursor() {
        let i = input("abcdefgh\nxy");
        let (rows, cur) = i.layout(5);
        assert_eq!(rows, ["abcde", "fgh", "xy"]);
        assert_eq!(cur, (2, 2));
        let mut j = input("abcde");
        assert_eq!(j.layout(5), (vec!["abcde".to_string(), String::new()], (1, 0)));
        j.cursor = 2;
        assert_eq!(j.layout(5).1, (0, 2));
        let k = input("ab\n");
        assert_eq!(k.layout(5), (vec!["ab".to_string(), String::new()], (1, 0)));
    }
}
