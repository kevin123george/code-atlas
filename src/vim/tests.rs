//! Vim engine tests: feed key sequences, check the text and the cursor.
//! Key notation: plain characters, plus <Esc> <CR> <BS> <Del> <Tab> <Left> <Right> <Up> <Down> <C-r> <C-[>.

use super::*;

struct T {
    d: Doc,
    v: Vim,
    clip: Clip,
    effects: Vec<Effect>,
}

fn keys(s: &str) -> Vec<K> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '<' && it.peek().is_some_and(|c| c.is_ascii_uppercase()) {
            let name: String = it.by_ref().take_while(|c| *c != '>').collect();
            out.push(match name.as_str() {
                "Esc" | "C-[" => K::Esc,
                "CR" => K::Enter,
                "BS" => K::Back,
                "Del" => K::Del,
                "Tab" => K::Tab,
                "Left" => K::Left,
                "Right" => K::Right,
                "Up" => K::Up,
                "Down" => K::Down,
                "C-r" => K::CtrlR,
                other => panic!("unknown key <{other}>"),
            });
        } else {
            out.push(K::Ch(c));
        }
    }
    out
}

/// A doc with `|` marking the cursor.
fn t(text: &str) -> T {
    let cursor = text.chars().position(|c| c == '|').expect("cursor mark");
    let mut d = Doc::from_text(&text.replacen('|', "", 1));
    d.cursor = cursor;
    let mut v = Vim::new();
    v.fix(&mut d);
    T { d, v, clip: Clip::default(), effects: Vec::new() }
}

impl T {
    fn run(mut self, s: &str) -> Self {
        for k in keys(s) {
            let e = self.v.key(&mut self.d, &mut self.clip, k);
            if e == Effect::Pass {
                // What the IDEA key map would do with the keys Vim passes on in insert mode.
                match k {
                    K::Left => self.d.left(false),
                    K::Right => self.d.right(false),
                    K::Up => self.d.vertical(-1, false),
                    K::Down => self.d.vertical(1, false),
                    _ => {}
                }
                self.v.after_ide(&mut self.d);
            } else if e != Effect::None {
                self.effects.push(e);
            }
        }
        self
    }

    /// The text with `|` at the cursor.
    fn show(&self) -> String {
        let mut s = self.d.rope.to_string();
        let b = self.d.rope.char_to_byte(self.d.cursor);
        s.insert(b, '|');
        s
    }

    fn text(&self) -> String {
        self.d.rope.to_string()
    }

    fn undo(mut self) -> Self {
        self.d.undo();
        self
    }
}

fn check(start: &str, ks: &str, want: &str) {
    let r = t(start).run(ks);
    assert_eq!(r.show(), want, "keys {ks:?} on {start:?}");
}

// ---- motions ---------------------------------------------------------------------------------

#[test]
fn hjkl_and_counts() {
    check("|abc\ndef\nghi", "l", "a|bc\ndef\nghi");
    check("|abc\ndef\nghi", "2l", "ab|c\ndef\nghi");
    check("|abc\ndef\nghi", "9l", "ab|c\ndef\nghi");
    check("ab|c\ndef\nghi", "j", "abc\nde|f\nghi");
    check("ab|c\ndef\nghi", "2j", "abc\ndef\ngh|i");
    check("abc\ndef\ngh|i", "2k", "ab|c\ndef\nghi");
    check("abc\nd|ef", "h", "abc\n|def");
    check("abc\n|def", "h", "abc\n|def");
    check("abc\ndef\ngh|i", "j", "abc\ndef\ngh|i");
}

#[test]
fn arrow_keys_move_in_normal_mode() {
    check("|abc\ndef", "<Right><Down>", "abc\nd|ef");
    check("abc\nd|ef", "<Up><Left>", "|abc\ndef");
}

#[test]
fn column_is_kept_through_short_lines_and_after_dollar() {
    check("abcdef\nab\nabc|def", "kk", "abc|def\nab\nabcdef");
    check("ab|c\nabcdef\nx", "$j", "abc\nabcde|f\nx");
}

#[test]
fn word_motions() {
    check("|foo bar.baz qux", "w", "foo |bar.baz qux");
    check("|foo bar.baz qux", "2w", "foo bar|.baz qux");
    check("|foo bar.baz qux", "2W", "foo bar.baz |qux");
    check("foo bar.baz |qux", "b", "foo bar.|baz qux");
    check("foo bar.baz |qux", "B", "foo |bar.baz qux");
    check("|foo bar.baz qux", "e", "fo|o bar.baz qux");
    check("|foo bar.baz qux", "3e", "foo bar|.baz qux");
    check("|foo bar.baz qux", "2E", "foo bar.ba|z qux");
    check("fo|o\n\nbar", "w", "foo\n|\nbar");
    check("foo\n|\nbar", "w", "foo\n\n|bar");
    check("foo\n\n|bar", "b", "foo\n|\nbar");
    check("one |two", "w", "one tw|o");
}

#[test]
fn line_motions() {
    check("  ab|cd", "0", "|  abcd");
    check("|  abcd", "^", "  |abcd");
    check("  |abcd", "$", "  abc|d");
    check("|ab\ncd\nef", "2$", "ab\nc|d\nef");
    check("a\n  |b\nc", "+", "a\n  b\n|c");
    check("a\n  b\n|c", "-", "a\n  |b\nc");
    check("a\n  b\n|c", "<CR>", "a\n  b\n|c");
}

#[test]
fn file_motions() {
    check("a\n  b\n|c", "gg", "|a\n  b\nc");
    check("|a\n  b\nc", "G", "a\n  b\n|c");
    check("|a\n  b\nc", "2G", "a\n  |b\nc");
    check("|a\n  b\nc", "2gg", "a\n  |b\nc");
    check("|a\nb\n", "G", "a\n|b\n");
    // The empty line IDEA draws after a final line break is not a Vim line.
    check("a\n|b\n", "j", "a\n|b\n");
    check("|a\nb\n", "5j", "a\n|b\n");
    check("|a\nb\n", "dG", "|");
}

#[test]
fn find_char_motions_and_repeat() {
    check("|a,b,c,d", "f,", "a|,b,c,d");
    check("|a,b,c,d", "2f,", "a,b|,c,d");
    check("|a,b,c,d", "f,;", "a,b|,c,d");
    check("|a,b,c,d", "f,;;,", "a,b|,c,d");
    check("|a,b,c,d", "t,", "|a,b,c,d");
    check("|a,b,c,d", "lt,", "a,|b,c,d");
    check("|xa,b,c,d", "t,;", "xa,|b,c,d");
    check("a,b,c,|d", "F,", "a,b,c|,d");
    check("a,b,c,|d", "T,", "a,b,c,|d");
    check("a,b,c,|d", "hT,", "a,b,|c,d");
    check("|abc", "fz", "|abc");
}

#[test]
fn match_and_paragraph_motions() {
    check("|if (a(b)) {}", "%", "if (a(b)|) {}");
    check("if (a(b)|) {}", "%", "if |(a(b)) {}");
    check("fn x() {\n  |y\n}", "j%", "fn x() |{\n  y\n}");
    check("|a\nb\n\nc\nd\n\ne", "}", "a\nb\n|\nc\nd\n\ne");
    check("|a\nb\n\nc\nd\n\ne", "2}", "a\nb\n\nc\nd\n|\ne");
    check("a\nb\n\nc\nd\n\n|e", "{", "a\nb\n\nc\nd\n|\ne");
    check("a\nb\n\nc\n|d", "}", "a\nb\n\nc\n|d");
}

// ---- operators -------------------------------------------------------------------------------

#[test]
fn delete_with_motions() {
    check("|foo bar baz", "dw", "|bar baz");
    check("|foo bar baz", "d2w", "|baz");
    check("|foo bar baz", "2dw", "|baz");
    check("|one two three four five", "2d2w", "|five");
    check("foo |bar baz", "de", "foo | baz");
    check("foo bar |baz", "db", "foo |baz");
    check("foo b|ar", "d$", "foo |b");
    check("foo b|ar", "d0", "|ar");
    check("|a,b,c", "dt,", "|,b,c");
    check("|a,b,c", "df,", "|b,c");
    check("|a,b,c", "d2f,", "|c");
    check("x |(a b) y", "d%", "x | y");
    check("foo |bar\nbaz", "dw", "foo| \nbaz");
}

#[test]
fn dw_at_end_of_line_keeps_the_line_break() {
    check("|foo\nbar", "dw", "|\nbar");
    check("a |foo\nbar", "dw", "a| \nbar");
}

#[test]
fn linewise_deletes() {
    check("a\n|b\nc\n", "dd", "a\n|c\n");
    check("a\nb\n|c", "dd", "a\n|b");
    check("|a\nb\nc\nd", "2dd", "|c\nd");
    check("|a\nb\nc\nd", "d2d", "|c\nd");
    check("|a\nb\nc\nd", "dj", "|c\nd");
    check("a\nb\n|c\nd", "dk", "a\n|d");
    check("a\n|b\nc\nd", "dG", "|a");
    check("a\nb\n|c\nd", "dgg", "|d");
    check("|only", "dd", "|");
    check("|a\n  b\nc", "9dd", "|");
    check("a\n|b\n", "dd", "|a\n");
}

#[test]
fn d_paragraph_is_linewise_from_line_start() {
    check("|a\nb\n\nc", "d}", "|\nc");
}

#[test]
fn change_and_friends() {
    check("|foo bar", "cwxy<Esc>", "x|y bar");
    check("|foo bar", "cexy<Esc>", "x|y bar");
    check("|a bar", "cwxy<Esc>", "x|y bar");
    check("|foo bar baz", "c2wq<Esc>", "|q baz");
    check("  foo |bar\nx", "ccnew<Esc>", "  ne|w\nx");
    check("  foo |bar\nx", "Snew<Esc>", "  ne|w\nx");
    check("foo |bar", "Cxy<Esc>", "foo x|y");
    check("foo |bar", "Dx", "fo|o");
    check("|abc", "sX<Esc>", "|Xbc");
    check("|abc", "2sX<Esc>", "|Xc");
}

#[test]
fn x_and_x_upper_with_counts() {
    check("|abcd", "x", "|bcd");
    check("|abcd", "3x", "|d");
    check("|abcd", "9x", "|");
    check("ab|cd", "X", "a|cd");
    check("ab|cd", "5X", "|cd");
    check("ab|c\nd", "x", "a|b\nd");
    check("|\nd", "x", "|\nd");
}

#[test]
fn replace_toggle_join() {
    check("|abc", "rx", "|xbc");
    check("|abc", "3rx", "xx|x");
    check("|abc", "4rx", "|abc");
    check("a|bc", "r<CR>", "a\n|c");
    check("|abC", "~~~", "AB|c");
    check("|abc def", "4~", "ABC |def");
    check("|a\n   b\nc", "J", "a| b\nc");
    check("|a\nb\nc", "3J", "a b| c");
    check("|a(\n)", "J", "a(|)");
    check("|a \nb", "J", "a |b");
}

#[test]
fn yank_and_put() {
    let r = t("|foo bar").run("yw");
    assert_eq!(r.clip.text, "foo ");
    assert!(r.effects.contains(&Effect::Yank("foo ".into())));
    check("|foo bar", "ywP", "foo| foo bar");
    check("|foo bar", "ywp", "ffoo| oo bar");
    check("|foo bar", "yw$p", "foo barfoo| ");
    check("|a\nb", "yyp", "a\n|a\nb");
    check("|a\nb", "yyjp", "a\nb\n|a");
    check("a\n|b", "yyP", "a\n|b\nb");
    check("|a\nb\n", "Yjp", "a\nb\n|a\n");
    check("|a\nb", "2yyGp", "a\nb\n|a\nb");
    check("|a\nb", "yy3p", "a\n|a\na\na\nb");
    check("  |a", "yyp", "  a\n  |a");
    check("|abc", "x$p", "bc|a");
    check("|a\nb\nc", "ddp", "b\n|a\nc");
    check("a\n|b\nc", "yk", "|a\nb\nc");
}

#[test]
fn indent_and_case_operators() {
    check("|a\nb", ">>", "    |a\nb");
    check("|a\nb\nc", "2>>", "    |a\n    b\nc");
    check("|a\nb\nc", ">j", "    |a\n    b\nc");
    check("    |a\nb", "<<", "|a\nb");
    check("|foo bar", "gUw", "|FOO bar");
    check("|FOO BAR", "guu", "|foo bar");
    check("|Foo bar", "g~~", "|fOO BAR");
    check("|Foo bar", "g~iw", "|fOO bar");
}

// ---- text objects ----------------------------------------------------------------------------

#[test]
fn word_objects() {
    check("foo b|ar baz", "diw", "foo | baz");
    check("foo b|ar baz", "daw", "foo |baz");
    check("foo b|ar", "daw", "fo|o");
    check("foo b|ar baz", "ciwX<Esc>", "foo |X baz");
    check("a foo.b|ar b", "diW", "a | b");
    check("foo b|ar baz", "yiwP", "foo ba|rbar baz");
}

#[test]
fn quote_objects() {
    check("x = \"he|llo\" + y", "di\"", "x = \"|\" + y");
    check("x = \"he|llo\" + y", "da\"", "x = |+ y");
    check("|x = \"hello\" + y", "ci\"bye<Esc>", "x = \"by|e\" + y");
    check("a 'b|c' d", "di'", "a '|' d");
    check("\"a\" \"b|\"", "di\"", "\"a\" \"|\"");
}

#[test]
fn bracket_objects() {
    check("f(a, |b) + 1", "di(", "f(|) + 1");
    check("f(a, |b) + 1", "dib", "f(|) + 1");
    check("f(a, |b) + 1", "da(", "f| + 1");
    check("f(a, (|b)) + 1", "di)", "f(a, (|)) + 1");
    check("f(a, (b)|) + 1", "di(", "f(|) + 1");
    check("|f(a, b)", "di(", "|f(a, b)");
    check("x[1|2]", "ci[3<Esc>", "x[|3]");
    check("fn x() {\n    a|;\n    b;\n}\n", "di{", "fn x() {\n|}\n");
    check("fn x() {\n    a|;\n}\n", "diB", "fn x() {\n|}\n");
    check("fn x() { a|; }", "di{", "fn x() {|}");
    check("fn x() {\n    a|;\n}\nz", "da{", "fn x()| \nz");
    check("fn x() {\n    a|;\n}\n", "ci{b<Esc>", "fn x() {\n|b}\n");
    let r = t("call(a, |b)").run("yi(");
    assert_eq!(r.clip.text, "a, b");
}

// ---- insert mode -----------------------------------------------------------------------------

#[test]
fn entering_insert_mode() {
    check("ab|c", "iX<Esc>", "ab|Xc");
    check("ab|c", "aX<Esc>", "abc|X");
    check("  ab|c", "IX<Esc>", "  |Xabc");
    check("ab|c", "AX<Esc>", "abc|X");
    check("  a|b\nc", "oX<Esc>", "  ab\n  |X\nc");
    check("  a|b\nc", "OX<Esc>", "  |X\n  ab\nc");
    check("ab|c", "i<Esc>", "a|bc");
    check("|abc", "i<Esc>", "|abc");
    check("abc|", "a<Esc>", "ab|c");
    check("ab|c", "i<C-[>", "a|bc");
}

#[test]
fn insert_with_count_repeats_the_text() {
    check("|x", "3ia<Esc>", "aa|ax");
    check("|x", "2ofoo<Esc>", "x\nfoo\nfo|o");
}

#[test]
fn insert_mode_typing_keys() {
    check("|ab", "ixy<BS>z<Esc>", "x|zab");
    check("a|b", "i<CR><Esc>", "a\n|b");
    check("|ab", "i<Del><Esc>", "|b");
}

#[test]
fn insert_mode_toggles_cursor_states() {
    let r = t("|ab").run("i");
    assert_eq!(r.v.mode, Mode::Insert);
    let r = r.run("<Esc>");
    assert_eq!(r.v.mode, Mode::Normal);
}

// ---- undo / redo -----------------------------------------------------------------------------

#[test]
fn one_command_is_one_undo_step() {
    let r = t("|foo bar baz").run("ciwhello world<Esc>");
    assert_eq!(r.text(), "hello world bar baz");
    let r = r.run("u");
    assert_eq!(r.text(), "foo bar baz");
    let r = r.run("<C-r>");
    assert_eq!(r.text(), "hello world bar baz");
}

#[test]
fn insert_session_with_newlines_is_one_undo_step() {
    let r = t("|x").run("oline one<CR>line two<CR>  three<Esc>");
    assert_eq!(r.text(), "x\nline one\nline two\n  three");
    let r = r.run("u");
    assert_eq!(r.text(), "x");
}

#[test]
fn counted_commands_undo_once() {
    let r = t("|a\nb\nc\nd").run("3dd");
    assert_eq!(r.text(), "d");
    assert_eq!(r.run("u").text(), "a\nb\nc\nd");
    let r = t("|a\nb\nc").run("3J");
    assert_eq!(r.run("u").text(), "a\nb\nc");
    let r = t("|a\nb\nc").run("dddd");
    assert_eq!(r.text(), "c");
    let r = r.run("u");
    assert_eq!(r.text(), "b\nc");
    assert_eq!(r.run("u").text(), "a\nb\nc");
}

#[test]
fn undo_and_redo_with_counts() {
    let r = t("|abcd").run("xxx");
    assert_eq!(r.text(), "d");
    let r = r.run("2u");
    assert_eq!(r.text(), "bcd");
    let r = r.run("2<C-r>");
    assert_eq!(r.text(), "d");
    let r = r.run("uuuu");
    assert_eq!(r.text(), "abcd");
    assert!(!r.d.dirty());
}

#[test]
fn arrow_in_insert_mode_starts_a_new_undo_step() {
    let r = t("|").run("iab<Left>X<Esc>");
    assert_eq!(r.text(), "aXb");
    let r = r.run("u");
    assert_eq!(r.text(), "ab");
}

#[test]
fn undo_without_history_says_so() {
    let r = t("|a").run("u");
    assert_eq!(r.effects, vec![Effect::Message("Already at oldest change".into())]);
}

// ---- dot repeat ------------------------------------------------------------------------------

#[test]
fn dot_repeats_deletes() {
    check("|a b c d e", "dw.", "|c d e");
    check("|a b c d e", "dw..", "|d e");
    check("|abcdef", "2x.", "|ef");
    check("|abcdefgh", "2x3.", "|fgh");
    check("|a\nb\nc\nd", "dd.", "|c\nd");
    check("|a,b,c,d", "df,.", "|c,d");
}

#[test]
fn dot_repeats_changes_with_their_text() {
    check("|foo bar", "ciwX<Esc>w.", "X |X");
    check("|a\nb", "A;<Esc>j.", "a;\nb|;");
    check("|a\nb", "Ix<Esc>j.", "xa\n|xb");
    check("|a", "ox<Esc>.", "a\nx\n|x");
    check("|ab", "3ix<Esc>.", "xxxx|xxab");
    check("|foo foo", "cwbar<Esc>w.", "bar ba|r");
}

#[test]
fn dot_repeats_other_changes() {
    check("|abc", "rx.l.", "x|xc");
    check("|a\nb\nc", ">>j.", "    a\n    |b\nc");
    check("|a\nb\nc\nd", "Jj.", "a b\nc| d");
    check("|ab", "~.", "A|B");
    check("|a", "yyp.", "a\na\n|a");
}

#[test]
fn dot_is_one_undo_step() {
    let r = t("|foo bar").run("ciwX<Esc>w.");
    assert_eq!(r.text(), "X X");
    let r = r.run("u");
    assert_eq!(r.text(), "X bar");
    let r = r.run("u");
    assert_eq!(r.text(), "foo bar");
}

#[test]
fn dot_after_visual_change_repeats_the_same_size() {
    check("|a\nb\nc\nd\ne", "Vjdj.", "|c");
    check("|abcdef", "vld.", "|ef");
    check("|a\nb\nc\nd", "Vj>jj.", "    a\n    b\n    |c\n    d");
}

#[test]
fn dot_without_a_change_does_nothing() {
    check("|abc", ".", "|abc");
    check("|abc", "yw.", "|abc");
}

// ---- visual mode -----------------------------------------------------------------------------

#[test]
fn visual_charwise() {
    check("|abcdef", "vlld", "|def");
    check("abc|def", "vhhd", "a|ef");
    check("|ab\ncd", "vjd", "|d");
    check("|abc def", "vey$p", "abc defab|c");
    check("|abcdef", "vlc<Esc>", "|cdef");
    check("|abcdef", "vlcX<Esc>", "|Xcdef");
    check("|abc", "vlU", "|ABc");
    check("|ABC", "vlu", "|abC");
    check("|abc", "vl~", "|ABc");
    check("|abc", "vlrx", "|xxc");
    check("|abcd", "vlx", "|cd");
    check("|abcd", "lvlohd", "|d");
}

#[test]
fn visual_range_is_inclusive() {
    let r = t("|abcdef").run("vll");
    assert_eq!(r.v.visual_range(&r.d), Some((0, 3)));
    assert_eq!(r.v.mode, Mode::Visual);
    let r = t("abc|def").run("vhh");
    assert_eq!(r.v.visual_range(&r.d), Some((1, 4)));
    let r = r.run("<Esc>");
    assert_eq!(r.v.mode, Mode::Normal);
    assert_eq!(r.d.anchor, None);
}

#[test]
fn visual_line() {
    check("a\n|b\nc\nd", "Vjd", "a\n|d");
    check("a\n|b\nc\nd", "Vjy", "a\n|b\nc\nd");
    check("a\n|b\nc", "VyP", "a\n|b\nb\nc");
    check("|a\nb\nc", "Vj>", "    |a\n    b\nc");
    check("    |a\n    b\nc", "Vj<", "|a\nb\nc");
    check("|a\nb\nc", "VjJ", "a| b\nc");
    check("|  a\nb\nc", "VjcX<Esc>", "  |X\nc");
    check("|a\nb", "vjVd", "|");
    let r = t("|a\nb").run("Vj");
    assert_eq!(r.v.visual_range(&r.d), Some((0, 3)));
    let r = t("|a\nb").run("vV");
    assert_eq!(r.v.mode, Mode::VisualLine);
    let r = r.run("V");
    assert_eq!(r.v.mode, Mode::Normal);
}

#[test]
fn visual_text_objects() {
    check("foo b|ar baz", "viwd", "foo | baz");
    check("f(a, |b)", "vi(d", "f(|)");
    check("say \"h|i there\"", "vi\"c!<Esc>", "say \"|!\"");
}

#[test]
fn visual_put_replaces_and_swaps() {
    let r = t("|foo bar").run("yiwwviwp");
    assert_eq!(r.text(), "foo foo");
    assert_eq!(r.clip.text, "bar");
}

#[test]
fn visual_mode_follows_ide_selections() {
    // A selection made by the IDEA side (mouse drag, Shift+arrows, Ctrl+F) starts visual mode.
    let mut r = t("|abcdef");
    r.d.select(1, 4);
    r.v.after_ide(&mut r.d);
    assert_eq!(r.v.mode, Mode::Visual);
    assert_eq!(r.v.visual_range(&r.d), Some((1, 4)));
    // Before an IDEA chord the exact range is selected, and it comes back unchanged after.
    r.v.before_ide(&mut r.d);
    assert_eq!(r.d.selection(), Some((1, 4)));
    r.v.after_ide(&mut r.d);
    assert_eq!(r.v.visual_range(&r.d), Some((1, 4)));
    assert_eq!(r.d.cursor, 3);
    // A cut by the IDEA side ends visual mode.
    r.v.before_ide(&mut r.d);
    r.d.cut();
    r.v.after_ide(&mut r.d);
    assert_eq!(r.v.mode, Mode::Normal);
    assert_eq!(r.text(), "aef");
}

// ---- search ----------------------------------------------------------------------------------

#[test]
fn slash_search_and_n() {
    check("|foo bar foo baz foo", "/foo<CR>", "foo bar |foo baz foo");
    check("|foo bar foo baz foo", "/foo<CR>n", "foo bar foo baz |foo");
    check("|foo bar foo baz foo", "/foo<CR>nn", "|foo bar foo baz foo");
    check("|foo bar foo baz foo", "/foo<CR>N", "|foo bar foo baz foo");
    check("foo bar foo baz |foo", "?foo<CR>", "foo bar |foo baz foo");
    check("foo bar foo baz |foo", "?foo<CR>n", "|foo bar foo baz foo");
    check("foo bar foo baz |foo", "?foo<CR>N", "foo bar foo baz |foo");
    check("|a\nxyz\nb xyz", "/xyz<CR>2n", "a\n|xyz\nb xyz");
    check("|a b", "/zzz<CR>", "|a b");
}

#[test]
fn search_messages() {
    let r = t("|a b").run("/zzz<CR>");
    assert_eq!(r.effects, vec![Effect::Message("E486: Pattern not found: zzz".into())]);
    let r = t("a |b a").run("/a<CR>");
    assert_eq!(r.show(), "a b |a");
    let r = r.run("n");
    assert_eq!(r.show(), "|a b a");
    assert_eq!(r.effects, vec![Effect::Message("search hit BOTTOM, continuing at TOP".into())]);
}

#[test]
fn empty_search_repeats_the_last_one() {
    check("|x a a a", "/a<CR>/<CR>", "x a |a a");
}

#[test]
fn backspace_on_empty_cmdline_cancels() {
    let r = t("|ab").run("/<BS>");
    assert_eq!(r.v.cmdline(), None);
    let r = t("|ab").run(":w<Esc>");
    assert_eq!(r.v.cmdline(), None);
    assert!(r.effects.is_empty());
}

#[test]
fn star_and_hash_search_whole_words() {
    check("|foo foobar foo", "*", "foo foobar |foo");
    check("|foo foobar foo", "**", "|foo foobar foo");
    check("foo foobar |foo", "#", "|foo foobar foo");
    check("|foo foobar foo", "*n", "|foo foobar foo");
    check("  |foo x foo", "*", "  foo x |foo");
}

#[test]
fn operators_with_search_motions() {
    check("|abc def ghi", "/gh<CR>ggdn", "|ghi");
    check("|one two one", "d*", "|one");
}

// ---- command line ----------------------------------------------------------------------------

fn ex(start: &str, cmd: &str) -> T {
    t(start).run(&format!(":{cmd}<CR>"))
}

#[test]
fn ex_file_commands() {
    assert_eq!(ex("|a", "w").effects, vec![Effect::Save(None)]);
    assert_eq!(ex("|a", "w out.txt").effects, vec![Effect::Save(Some("out.txt".into()))]);
    assert_eq!(ex("|a", "q").effects, vec![Effect::Close { save: false, force: false, path: None }]);
    assert_eq!(ex("|a", "q!").effects, vec![Effect::Close { save: false, force: true, path: None }]);
    assert_eq!(ex("|a", "wq").effects, vec![Effect::Close { save: true, force: false, path: None }]);
    assert_eq!(ex("|a", "x").effects, vec![Effect::Close { save: true, force: false, path: None }]);
    assert_eq!(ex("|a", "wa").effects, vec![Effect::SaveAll]);
    assert_eq!(ex("|a", "qa!").effects, vec![Effect::QuitAll { save: false, force: true }]);
    assert_eq!(ex("|a", "e src/main.rs").effects, vec![Effect::Open("src/main.rs".into())]);
    assert_eq!(ex("|a", "e").effects, vec![Effect::Message("E32: No file name".into())]);
    assert_eq!(ex("|a", "frob").effects, vec![Effect::Message("E492: Not an editor command: frob".into())]);
}

#[test]
fn ex_line_numbers() {
    assert_eq!(ex("|a\nb\n  c\nd", "3").show(), "a\nb\n  |c\nd");
    assert_eq!(ex("|a\nb\n  c\nd", "$").show(), "a\nb\n  c\n|d");
    assert_eq!(ex("a\nb\n  c\n|d", "0").show(), "|a\nb\n  c\nd");
    assert_eq!(ex("a\nb\n  c\n|d", "99").show(), "a\nb\n  c\n|d");
    assert_eq!(ex("|a\nb\nc\nd", ".+2").show(), "a\nb\n|c\nd");
}

#[test]
fn ex_substitute() {
    assert_eq!(ex("|a a\na a", "s/a/b/").text(), "b a\na a");
    assert_eq!(ex("|a a\na a", "s/a/b/g").text(), "b b\na a");
    assert_eq!(ex("|a a\na a", "%s/a/b/g").text(), "b b\nb b");
    assert_eq!(ex("|a a\na a\na", "2,3s/a/b/").text(), "a a\nb a\nb");
    assert_eq!(ex("|A a", "s/a/b/gi").text(), "b b");
    assert_eq!(ex("|a/b", "s/\\//-/").text(), "a-b");
    assert_eq!(ex("|a/b", "s#/#+#").text(), "a+b");
    assert_eq!(ex("|foo", "s/o/[&]/g").text(), "f[o][o]");
    assert_eq!(ex("|a,b", "s/,/\\n/").text(), "a\nb");
    assert_eq!(ex("|foo", "s/o//g").text(), "f");
    let r = ex("|x\ny", "%s/a/b/");
    assert_eq!(r.effects, vec![Effect::Message("E486: Pattern not found: a".into())]);
    let r = ex("|a\na\nx", "%s/a/b/");
    assert_eq!(r.effects, vec![Effect::Message("2 substitutions on 2 lines".into())]);
    assert_eq!(r.show(), "b\n|b\nx");
}

#[test]
fn ex_substitute_is_one_undo_step() {
    let r = ex("|a\na\na", "%s/a/b/");
    assert_eq!(r.text(), "b\nb\nb");
    assert_eq!(r.undo().text(), "a\na\na");
}

#[test]
fn ex_substitute_on_visual_lines() {
    let r = t("a\n|a\na\na").run("Vj:s/a/b/<CR>");
    assert_eq!(r.text(), "a\nb\nb\na");
}

#[test]
fn ex_delete_lines() {
    assert_eq!(ex("|a\nb\nc\nd", "2,3d").text(), "a\nd");
    assert_eq!(ex("|a\nb", "d").text(), "b");
}

#[test]
fn count_before_colon_gives_a_range() {
    let r = t("|a\na\na\na").run("3:s/a/b/<CR>");
    assert_eq!(r.text(), "b\nb\nb\na");
}

// ---- odds and ends ---------------------------------------------------------------------------

#[test]
fn escape_cancels_a_pending_command() {
    let r = t("|abc").run("d<Esc>x");
    assert_eq!(r.show(), "|bc");
    let r = t("|abc").run("2d");
    assert_eq!(r.v.pending(), "2d");
}

#[test]
fn unknown_keys_are_dropped() {
    check("|abc", "Qx", "|bc");
}

#[test]
fn normal_cursor_never_sits_past_the_line_end() {
    let mut r = t("|abc\nde");
    r.d.cursor = 3;
    r.v.fix(&mut r.d);
    assert_eq!(r.d.cursor, 2);
    let mut r = t("|abc\n");
    r.d.cursor = 4;
    r.v.fix(&mut r.d);
    assert_eq!(r.d.cursor, 2);
}

#[test]
fn crlf_files() {
    check("|a\r\nb\r\nc", "dd", "|b\r\nc");
    check("|a\r\nb", "yyp", "a\r\n|a\r\nb");
    check("|a\r\nb", "$x", "|\r\nb");
    check("|a\r\nb", "ox<Esc>", "a\r\n|x\r\nb");
    check("|a\r\nb", "J", "a| b");
}

#[test]
fn ide_chords_stay_with_idea() {
    use crossterm::event::{KeyCode as C, KeyEvent, KeyModifiers as M};
    let v = Vim::new();
    assert!(!v.wants(&KeyEvent::new(C::Char('s'), M::CONTROL)));
    assert!(!v.wants(&KeyEvent::new(C::Char('q'), M::CONTROL)));
    assert!(!v.wants(&KeyEvent::new(C::Left, M::ALT)));
    assert!(!v.wants(&KeyEvent::new(C::Right, M::SHIFT)));
    assert!(v.wants(&KeyEvent::new(C::Char('r'), M::CONTROL)));
    assert!(v.wants(&KeyEvent::new(C::Char('['), M::CONTROL)));
    assert!(v.wants(&KeyEvent::new(C::Char('x'), M::NONE)));
    assert!(v.wants(&KeyEvent::new(C::Char('X'), M::SHIFT)));
    assert_eq!(key_of(&KeyEvent::new(C::Char('['), M::CONTROL)), K::Esc);
}
