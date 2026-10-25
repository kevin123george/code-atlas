//! Optional Vim mode, like IdeaVim: Normal, Insert, Visual, Visual Line and the `:` command line on
//! top of the normal editor. Off by default (`catlas --vim`, `CATLAS_VIM=1`, Ctrl+Alt+V at runtime).
//!
//! How it fits in: `App` sends keys here first while Vim is on (`Vim::wants`); IDEA chords with
//! Ctrl/Alt (Ctrl+S, Ctrl+Q, Alt+arrows, ...) keep going to the IDEA key map in every mode. All
//! edits go through `Doc`'s methods inside one undo group per command (`Doc::begin_group`), so one
//! Vim command, or one whole insert session, is one undo step. The register is the editor
//! clipboard (`Clip`), so `y` / `p` and Ctrl+C / Ctrl+V share it.

mod ex;
mod motion;
#[cfg(test)]
mod tests;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::doc::{Clip, Doc};
use motion::{first_non_blank, last_line, len, line_of, obj_of, Kind, Motion, Obj, Target};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        }
    }

    fn visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine)
    }
}

/// What the editor has to do after a Vim key (things outside one document).
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Not a Vim key here: let the IDEA key map handle it.
    Pass,
    Message(String),
    /// Text was yanked: also put it on the system clipboard.
    Yank(String),
    /// :w [path]
    Save(Option<String>),
    /// :q, :q!, :wq, :x — close the tab (quit when it is the last one).
    Close { save: bool, force: bool, path: Option<String> },
    /// :wa
    SaveAll,
    /// :qa, :qa!, :wqa
    QuitAll { save: bool, force: bool },
    /// :e path
    Open(String),
}

/// A key as the Vim engine sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum K {
    Ch(char),
    Esc,
    Enter,
    Back,
    Del,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    CtrlR,
    Other,
}

pub fn key_of(k: &KeyEvent) -> K {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Char('[') if ctrl => K::Esc,
        KeyCode::Char('r') if ctrl && !alt => K::CtrlR,
        KeyCode::Char(c) if !ctrl && !alt => K::Ch(c),
        _ if ctrl || alt => K::Other,
        KeyCode::Esc => K::Esc,
        KeyCode::Enter => K::Enter,
        KeyCode::Backspace => K::Back,
        KeyCode::Delete => K::Del,
        KeyCode::Tab => K::Tab,
        KeyCode::Left => K::Left,
        KeyCode::Right => K::Right,
        KeyCode::Up => K::Up,
        KeyCode::Down => K::Down,
        KeyCode::Home => K::Home,
        KeyCode::End => K::End,
        _ => K::Other,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lower,
    Upper,
    Toggle,
}

impl Op {
    fn keys(self) -> Vec<K> {
        let s = match self {
            Op::Delete => "d",
            Op::Change => "c",
            Op::Yank => "y",
            Op::Indent => ">",
            Op::Outdent => "<",
            Op::Lower => "gu",
            Op::Upper => "gU",
            Op::Toggle => "g~",
        };
        s.chars().map(K::Ch).collect()
    }
}

#[derive(Clone, Copy, Debug)]
enum Tgt {
    Motion(Motion),
    Obj(Obj, bool),
    /// dd, cc, yy, >>, guu, ...
    Lines,
}

#[derive(Clone, Copy, Debug)]
enum Act {
    Move(Motion),
    Op(Op, Tgt),
    VisOp(Op),
    VisObj(Obj, bool),
    Simple(char),
    Replace(K),
    Scroll(char),
}

#[derive(Clone, Copy, Debug)]
struct Cmd {
    count: Option<usize>,
    act: Act,
}

enum Parsed {
    More,
    Bad,
    Done(Cmd),
}

/// A range an operator works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Range {
    Chars(usize, usize),
    Lines(usize, usize),
}

/// The last change, for `.`: its keys (without the leading count) and the text typed after it.
#[derive(Clone, Debug)]
struct Change {
    keys: Vec<K>,
    count: Option<usize>,
    insert: Vec<K>,
}

pub struct Vim {
    pub mode: Mode,
    pending: Vec<K>,
    /// The open command line: ':' / '/' / '?' and what is typed so far.
    cmdline: Option<(char, String)>,
    /// The other end of a visual selection (the cursor is one end).
    vstart: usize,
    /// Lines of the last visual selection, for `'<,'>`.
    last_visual: (usize, usize),
    last_change: Option<Change>,
    /// Keys typed in the current insert session.
    session: Vec<K>,
    /// The current insert session belongs to `last_change`.
    recording: bool,
    replaying: bool,
    /// 3ifoo<Esc> types "foo" three times; `o`/`O` sessions repeat on new lines.
    insert_count: usize,
    insert_lines: bool,
    last_find: Option<(char, bool, bool)>,
    /// Query, forwards, whole word (from * and #).
    last_search: Option<(String, bool, bool)>,
    /// The doc's selection set up for an IDEA chord in visual mode, see `before_ide`.
    saved: Option<(usize, usize, usize, usize)>,
    msg: Option<String>,
}

impl Default for Vim {
    fn default() -> Self {
        Self::new()
    }
}

impl Vim {
    pub fn new() -> Self {
        Vim {
            mode: Mode::Normal,
            pending: Vec::new(),
            cmdline: None,
            vstart: 0,
            last_visual: (0, 0),
            last_change: None,
            session: Vec::new(),
            recording: false,
            replaying: false,
            insert_count: 1,
            insert_lines: false,
            last_find: None,
            last_search: None,
            saved: None,
            msg: None,
        }
    }

    // ---- what the editor asks -------------------------------------------------------------

    /// Whether this key goes to Vim (else the IDEA key map). Ctrl/Alt chords stay IDEA's, except
    /// Ctrl+[ (Esc) and Ctrl+R (redo) outside insert mode; Shift+arrows select like in IDEA.
    pub fn wants(&self, k: &KeyEvent) -> bool {
        if self.cmdline.is_some() {
            return true;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('[') if ctrl && !alt => true,
            KeyCode::Char('r') if ctrl && !alt => self.mode != Mode::Insert,
            _ if ctrl || alt => false,
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => !shift,
            _ => true,
        }
    }

    /// The command line as shown in the status bar, e.g. ":wq".
    pub fn cmdline(&self) -> Option<String> {
        self.cmdline.as_ref().map(|(k, s)| format!("{k}{s}"))
    }

    /// Keys of a command not finished yet (like Vim's showcmd).
    pub fn pending(&self) -> String {
        self.pending
            .iter()
            .map(|k| match k {
                K::Ch(c) => c.to_string(),
                _ => String::new(),
            })
            .collect()
    }

    /// The visual selection `[s, e)` (inclusive of the cursor character, whole lines in V-LINE).
    pub fn visual_range(&self, d: &Doc) -> Option<(usize, usize)> {
        match self.vis_range(d)? {
            Range::Chars(s, e) => Some((s, e)),
            Range::Lines(a, b) => Some((d.line_start(a), line_after(d, b))),
        }
    }

    fn vis_range(&self, d: &Doc) -> Option<Range> {
        let n = len(d);
        let (a, c) = (self.vstart.min(n), d.cursor.min(n));
        match self.mode {
            Mode::Visual => Some(Range::Chars(a.min(c), (a.max(c) + 1).min(n))),
            Mode::VisualLine => {
                let (x, y) = (line_of(d, a), line_of(d, c));
                Some(Range::Lines(x.min(y), x.max(y)))
            }
            _ => None,
        }
    }

    /// Pasted text while the command line is open goes there.
    pub fn paste(&mut self, text: &str) -> bool {
        match &mut self.cmdline {
            Some((_, s)) => {
                s.push_str(text.lines().next().unwrap_or(""));
                true
            }
            None => false,
        }
    }

    /// Before an IDEA chord in visual mode: make the doc's selection the exact visual range, so
    /// Ctrl+C, Ctrl+X, Tab ... act on what is highlighted.
    pub fn before_ide(&mut self, d: &mut Doc) {
        self.saved = None;
        if let Some((s, e)) = self.visual_range(d) {
            self.saved = Some((self.vstart, d.cursor, s, e));
            d.select(s, e);
        }
    }

    /// After the IDEA key map (or the mouse, a paste, a prompt) changed the doc: a new selection
    /// starts visual mode, a cleared one ends it, and the cursor is put back on a character.
    pub fn after_ide(&mut self, d: &mut Doc) {
        if let Some((vs, c, s, e)) = self.saved.take() {
            if d.anchor == Some(s) && d.cursor == e {
                self.vstart = vs;
                d.cursor = c;
                self.fix(d);
                return;
            }
        }
        match self.mode {
            Mode::Insert => {}
            Mode::Normal => {
                if let Some(a) = d.anchor.filter(|a| *a != d.cursor) {
                    self.mode = Mode::Visual;
                    self.vstart = a;
                    if d.cursor > a {
                        d.cursor -= 1;
                    }
                }
            }
            Mode::Visual | Mode::VisualLine => match d.anchor {
                Some(a) => self.vstart = a,
                None => self.mode = Mode::Normal,
            },
        }
        self.fix(d);
    }

    /// Leaves `d` cleanly (tab switch, Vim turned off): closes the undo group, drops the selection.
    pub fn leave(&mut self, d: &mut Doc) {
        if self.mode == Mode::Insert {
            self.session.clear();
            self.recording = false;
        }
        d.end_group();
        d.anchor = None;
        self.mode = Mode::Normal;
        self.pending.clear();
        self.cmdline = None;
        self.saved = None;
    }

    /// Keeps the doc in a state the mode allows: in normal mode no selection and the cursor on a
    /// character (not past the end of the line); in visual mode the doc's anchor is the visual start.
    pub fn fix(&mut self, d: &mut Doc) {
        match self.mode {
            Mode::Insert => return,
            Mode::Normal => d.anchor = None,
            Mode::Visual | Mode::VisualLine => {
                self.vstart = self.vstart.min(len(d));
                d.anchor = Some(self.vstart);
            }
        }
        clamp(d);
    }

    // ---- keys -----------------------------------------------------------------------------

    pub fn handle(&mut self, d: &mut Doc, clip: &mut Clip, ev: &KeyEvent) -> Effect {
        self.key(d, clip, key_of(ev))
    }

    fn key(&mut self, d: &mut Doc, clip: &mut Clip, k: K) -> Effect {
        self.msg = None;
        let eff = if self.cmdline.is_some() {
            d.begin_group();
            let e = self.cmdline_key(d, clip, k);
            d.end_group();
            e
        } else if self.mode == Mode::Insert {
            self.insert_key(d, k)
        } else {
            if k == K::Other {
                return Effect::Pass;
            }
            self.pending.push(k);
            match self.parse() {
                Parsed::More => return Effect::None,
                Parsed::Bad => {
                    self.pending.clear();
                    Effect::None
                }
                Parsed::Done(cmd) => {
                    let keys = std::mem::take(&mut self.pending);
                    d.begin_group();
                    let e = self.exec(d, clip, cmd, &keys);
                    if self.mode != Mode::Insert {
                        d.end_group();
                    }
                    e
                }
            }
        };
        self.fix(d);
        match (eff, self.msg.take()) {
            (Effect::None, Some(m)) => Effect::Message(m),
            (e, _) => e,
        }
    }

    fn insert_key(&mut self, d: &mut Doc, k: K) -> Effect {
        match k {
            K::Esc => {
                self.leave_insert(d);
                d.end_group();
                Effect::None
            }
            K::Ch(_) | K::Enter | K::Back | K::Del | K::Tab => {
                type_key(d, k);
                self.session.push(k);
                Effect::None
            }
            K::Left | K::Right | K::Up | K::Down | K::Home | K::End => {
                // Moving inside insert mode starts a new undo step; `.` repeats what came before.
                self.store_session();
                self.recording = false;
                self.insert_count = 1;
                d.end_group();
                d.begin_group();
                Effect::Pass
            }
            _ => Effect::Pass,
        }
    }

    fn store_session(&mut self) {
        if self.recording {
            if let Some(c) = &mut self.last_change {
                c.insert = self.session.clone();
            }
        }
        self.session.clear();
    }

    fn start_insert(&mut self, count: usize, lines: bool) {
        self.mode = Mode::Insert;
        self.insert_count = count.max(1);
        self.insert_lines = lines;
        self.session.clear();
        self.recording = !self.replaying;
    }

    fn leave_insert(&mut self, d: &mut Doc) {
        for _ in 1..self.insert_count {
            if self.insert_lines {
                d.newline();
            }
            for k in self.session.clone() {
                type_key(d, k);
            }
        }
        self.store_session();
        self.recording = false;
        self.insert_count = 1;
        self.mode = Mode::Normal;
        if d.col() > 0 {
            d.cursor -= 1;
        }
        d.set_want_col(None);
    }

    // ---- parsing --------------------------------------------------------------------------

    fn parse(&self) -> Parsed {
        let ks = &self.pending;
        let mut i = 0;
        let count = count_at(ks, &mut i);
        let Some(&k) = ks.get(i) else { return Parsed::More };
        let vis = self.mode.visual();
        let done = |act| Parsed::Done(Cmd { count, act });
        match k {
            K::Esc => return done(Act::Simple('\x1b')),
            K::CtrlR => return done(Act::Simple('\x12')),
            K::Del => return done(Act::Simple('x')),
            _ => {}
        }
        // Operators: d c y > < gu gU g~
        let op = match (k, ks.get(i + 1)) {
            (K::Ch('d'), _) => Some((Op::Delete, 1)),
            (K::Ch('c'), _) => Some((Op::Change, 1)),
            (K::Ch('y'), _) => Some((Op::Yank, 1)),
            (K::Ch('>'), _) => Some((Op::Indent, 1)),
            (K::Ch('<'), _) => Some((Op::Outdent, 1)),
            (K::Ch('g'), None) => return Parsed::More,
            (K::Ch('g'), Some(K::Ch('u'))) => Some((Op::Lower, 2)),
            (K::Ch('g'), Some(K::Ch('U'))) => Some((Op::Upper, 2)),
            (K::Ch('g'), Some(K::Ch('~'))) => Some((Op::Toggle, 2)),
            _ => None,
        };
        if let Some((op, n)) = op {
            if vis {
                return done(Act::VisOp(op));
            }
            let mut j = i + n;
            let count = mul(count, count_at(ks, &mut j));
            let done = |act| Parsed::Done(Cmd { count, act });
            let Some(&k2) = ks.get(j) else { return Parsed::More };
            if k2 == ks[i + n - 1] {
                return done(Act::Op(op, Tgt::Lines));
            }
            if let K::Ch(c @ ('i' | 'a')) = k2 {
                return match ks.get(j + 1) {
                    None => Parsed::More,
                    Some(K::Ch(o)) => obj_of(*o).map_or(Parsed::Bad, |o| done(Act::Op(op, Tgt::Obj(o, c == 'i')))),
                    _ => Parsed::Bad,
                };
            }
            return match parse_motion(ks, j) {
                Parsed::Done(Cmd { act: Act::Move(m), .. }) => done(Act::Op(op, Tgt::Motion(m))),
                Parsed::More => Parsed::More,
                _ => Parsed::Bad,
            };
        }
        if vis {
            match k {
                K::Ch(c @ ('i' | 'a')) => {
                    return match ks.get(i + 1) {
                        None => Parsed::More,
                        Some(K::Ch(o)) => obj_of(*o).map_or(Parsed::Bad, |o| done(Act::VisObj(o, c == 'i'))),
                        _ => Parsed::Bad,
                    }
                }
                K::Ch('u') => return done(Act::VisOp(Op::Lower)),
                K::Ch('U') => return done(Act::VisOp(Op::Upper)),
                K::Ch('~') => return done(Act::VisOp(Op::Toggle)),
                K::Ch('x') => return done(Act::VisOp(Op::Delete)),
                K::Ch('s') => return done(Act::VisOp(Op::Change)),
                _ => {}
            }
        }
        match k {
            K::Ch(
                c @ ('x' | 'X' | 's' | 'S' | 'D' | 'C' | 'Y' | 'J' | 'p' | 'P' | 'u' | '~' | '.' | 'i' | 'a' | 'I' | 'A'
                | 'o' | 'O' | 'v' | 'V' | ':' | '/' | '?'),
            ) => done(Act::Simple(c)),
            K::Ch('r') => match ks.get(i + 1) {
                None => Parsed::More,
                Some(&r @ (K::Ch(_) | K::Enter)) => done(Act::Replace(r)),
                _ => Parsed::Bad,
            },
            K::Ch('z') => match ks.get(i + 1) {
                None => Parsed::More,
                Some(K::Ch(c @ ('z' | 't' | 'b'))) => done(Act::Scroll(*c)),
                _ => Parsed::Bad,
            },
            _ => match parse_motion(ks, i) {
                Parsed::Done(Cmd { act, .. }) => done(act),
                other => other,
            },
        }
    }

    // ---- running commands -----------------------------------------------------------------

    fn exec(&mut self, d: &mut Doc, clip: &mut Clip, cmd: Cmd, keys: &[K]) -> Effect {
        let n = cmd.count.unwrap_or(1);
        match cmd.act {
            Act::Move(m) => {
                self.do_move(d, m, cmd.count);
                Effect::None
            }
            Act::Op(op, tgt) => {
                if op != Op::Yank {
                    self.record(keys);
                }
                self.op_target(d, clip, op, tgt, cmd.count)
            }
            Act::VisOp(op) => self.visual_op(d, clip, op, keys),
            Act::VisObj(o, inner) => {
                if let Some((s, e)) = motion::object(d, d.cursor, o, inner) {
                    if s < e {
                        self.mode = Mode::Visual;
                        self.vstart = s;
                        d.cursor = e - 1;
                    }
                }
                Effect::None
            }
            Act::Replace(r) => {
                if self.mode.visual() {
                    return self.visual_replace(d, r);
                }
                let (cur, le) = (d.cursor, d.line_end(d.line()));
                if cur + n > le {
                    return Effect::None;
                }
                self.record(keys);
                match r {
                    K::Ch(c) => {
                        d.replace(cur, cur + n, &c.to_string().repeat(n));
                        d.cursor = cur + n - 1;
                    }
                    _ => {
                        d.replace(cur, cur + n, "\n");
                    }
                }
                Effect::None
            }
            Act::Scroll(c) => {
                let (line, h) = (d.line(), d.view_h.max(1));
                d.scroll_line = match c {
                    'z' => line.saturating_sub(h / 2),
                    't' => line,
                    _ => line.saturating_sub(h - 1),
                };
                Effect::None
            }
            Act::Simple(c) if self.mode.visual() => self.visual_simple(d, clip, c, keys),
            Act::Simple(c) => self.simple(d, clip, c, cmd.count, keys),
        }
    }

    fn simple(&mut self, d: &mut Doc, clip: &mut Clip, c: char, count: Option<usize>, keys: &[K]) -> Effect {
        let n = count.unwrap_or(1);
        let line = d.line();
        let short = |op, m| (op, Tgt::Motion(m));
        let op = match c {
            'x' => Some(short(Op::Delete, Motion::Right)),
            'X' => Some(short(Op::Delete, Motion::Left)),
            's' => Some(short(Op::Change, Motion::Right)),
            'S' => Some((Op::Change, Tgt::Lines)),
            'D' => Some(short(Op::Delete, Motion::LineEnd)),
            'C' => Some(short(Op::Change, Motion::LineEnd)),
            'Y' => Some((Op::Yank, Tgt::Lines)),
            _ => None,
        };
        if let Some((op, tgt)) = op {
            if op != Op::Yank {
                self.record(keys);
            }
            return self.op_target(d, clip, op, tgt, count);
        }
        match c {
            '\x1b' => self.pending.clear(),
            'u' => {
                for _ in 0..n {
                    if !d.undo() {
                        self.msg = Some("Already at oldest change".into());
                        break;
                    }
                }
            }
            '\x12' => {
                for _ in 0..n {
                    if !d.redo() {
                        self.msg = Some("Already at newest change".into());
                        break;
                    }
                }
            }
            'J' => {
                self.record(keys);
                join(d, line, n.saturating_sub(1).max(1));
            }
            'p' | 'P' => {
                self.record(keys);
                paste(d, clip, c == 'p', n);
            }
            '~' => {
                self.record(keys);
                let (cur, le) = (d.cursor, d.line_end(line));
                let e = (cur + n).min(le);
                if cur < e {
                    let t = recase(&d.rope.slice(cur..e).to_string(), Op::Toggle);
                    d.replace(cur, e, &t);
                }
                d.cursor = e;
            }
            '.' => self.repeat(d, clip, count),
            'i' | 'a' | 'I' | 'A' | 'o' | 'O' => {
                self.record(keys);
                let (ls, le) = (d.line_start(line), d.line_end(line));
                match c {
                    'a' => d.cursor = (d.cursor + 1).min(le),
                    'I' => d.cursor = first_non_blank(d, line),
                    'A' => d.cursor = le,
                    'o' => {
                        d.cursor = le;
                        d.newline();
                    }
                    'O' => {
                        let indent = d.rope.slice(ls..first_non_blank(d, line)).to_string();
                        let eol = d.eol();
                        d.replace(ls, ls, &format!("{indent}{eol}"));
                        d.cursor = ls + indent.chars().count();
                    }
                    _ => {}
                }
                self.start_insert(n, matches!(c, 'o' | 'O'));
            }
            'v' | 'V' => {
                self.mode = if c == 'v' { Mode::Visual } else { Mode::VisualLine };
                self.vstart = d.cursor;
            }
            ':' => self.cmdline = Some((':', count.map(|n| format!(".,.+{}", n - 1)).unwrap_or_default())),
            '/' | '?' => self.cmdline = Some((c, String::new())),
            _ => {}
        }
        Effect::None
    }

    fn visual_simple(&mut self, d: &mut Doc, clip: &mut Clip, c: char, keys: &[K]) -> Effect {
        let Some(range) = self.vis_range(d) else { return Effect::None };
        let lines = match range {
            Range::Chars(s, e) => Range::Lines(line_of(d, s), line_of(d, e.saturating_sub(1).max(s))),
            r => r,
        };
        match c {
            '\x1b' => self.mode = Mode::Normal,
            'v' | 'V' => {
                let m = if c == 'v' { Mode::Visual } else { Mode::VisualLine };
                self.mode = if self.mode == m { Mode::Normal } else { m };
            }
            'o' => std::mem::swap(&mut self.vstart, &mut d.cursor),
            'X' | 'D' => return self.visual_apply(d, clip, Op::Delete, lines, keys),
            'C' | 'S' => return self.visual_apply(d, clip, Op::Change, lines, keys),
            'Y' => return self.visual_apply(d, clip, Op::Yank, lines, keys),
            'J' => {
                if let Range::Lines(a, b) = lines {
                    self.mode = Mode::Normal;
                    let joins = (b - a).max(1);
                    self.last_change =
                        Some(Change { keys: vec![K::Ch('J')], count: Some(joins + 1), insert: Vec::new() });
                    join(d, a, joins);
                }
            }
            'p' | 'P' => {
                self.mode = Mode::Normal;
                let put = clip.clone();
                let (s, e) = match range {
                    Range::Chars(s, e) => (s, e),
                    Range::Lines(a, b) => (d.line_start(a), line_after(d, b)),
                };
                let old = d.rope.slice(s..e).to_string();
                d.replace(s, e, "");
                d.cursor = s;
                if put.whole_line && matches!(range, Range::Lines(..)) {
                    d.replace(s, s, &put.text);
                    d.cursor = first_non_blank(d, line_of(d, s));
                } else {
                    let text = if put.whole_line && !matches!(range, Range::Lines(..)) {
                        put.text.trim_end_matches(['\r', '\n']).to_string()
                    } else {
                        put.text.clone()
                    };
                    let k = d.replace(s, s, &text);
                    d.cursor = s + k.saturating_sub(1);
                }
                *clip = Clip { text: old, whole_line: matches!(range, Range::Lines(..)) };
            }
            ':' => {
                if let Range::Lines(a, b) = lines {
                    self.last_visual = (a, b);
                }
                self.mode = Mode::Normal;
                self.cmdline = Some((':', "'<,'>".into()));
            }
            '/' | '?' => self.cmdline = Some((c, String::new())),
            _ => {}
        }
        Effect::None
    }

    fn visual_op(&mut self, d: &mut Doc, clip: &mut Clip, op: Op, keys: &[K]) -> Effect {
        match self.vis_range(d) {
            Some(r) => self.visual_apply(d, clip, op, r, keys),
            None => Effect::None,
        }
    }

    fn visual_apply(&mut self, d: &mut Doc, clip: &mut Clip, op: Op, r: Range, _keys: &[K]) -> Effect {
        if let Range::Lines(a, b) = r {
            self.last_visual = (a, b);
        }
        self.mode = Mode::Normal;
        // `.` after a visual change repeats it on as many lines / characters from the cursor.
        if op != Op::Yank && !self.replaying {
            let mut keys = op.keys();
            let change = match r {
                Range::Lines(a, b) if a == b => {
                    keys.push(*keys.last().unwrap());
                    Some((keys, None))
                }
                Range::Lines(a, b) => {
                    keys.push(K::Ch('j'));
                    Some((keys, Some(b - a)))
                }
                Range::Chars(s, e) if line_of(d, s) == line_of(d, e.saturating_sub(1).max(s)) => {
                    keys.push(K::Ch('l'));
                    Some((keys, Some(e - s)))
                }
                _ => None,
            };
            if let Some((keys, count)) = change {
                self.last_change = Some(Change { keys, count, insert: Vec::new() });
            }
        }
        self.apply(d, clip, op, r)
    }

    fn visual_replace(&mut self, d: &mut Doc, r: K) -> Effect {
        let (Some((s, e)), K::Ch(c)) = (self.visual_range(d), r) else { return Effect::None };
        let text: String = d.rope.slice(s..e).chars().map(|x| if x == '\n' || x == '\r' { x } else { c }).collect();
        d.replace(s, e, &text);
        d.cursor = s;
        self.mode = Mode::Normal;
        Effect::None
    }

    fn record(&mut self, keys: &[K]) {
        if self.replaying {
            return;
        }
        let mut i = 0;
        let count = count_at(keys, &mut i);
        self.last_change = Some(Change { keys: keys[i..].to_vec(), count, insert: Vec::new() });
    }

    /// `.`: runs the last change again (a new count replaces the old one), as one undo step.
    fn repeat(&mut self, d: &mut Doc, clip: &mut Clip, count: Option<usize>) {
        let Some(ch) = self.last_change.clone() else { return };
        let mut keys: Vec<K> = count.or(ch.count).map(|n| n.to_string().chars().map(K::Ch).collect()).unwrap_or_default();
        keys.extend(&ch.keys);
        self.replaying = true;
        self.pending = keys;
        if let Parsed::Done(cmd) = self.parse() {
            let ks = std::mem::take(&mut self.pending);
            self.exec(d, clip, cmd, &ks);
            if self.mode == Mode::Insert {
                for k in &ch.insert {
                    type_key(d, *k);
                    self.session.push(*k);
                }
                self.leave_insert(d);
            }
        }
        self.pending.clear();
        self.replaying = false;
    }

    fn do_move(&mut self, d: &mut Doc, m: Motion, count: Option<usize>) {
        let n = count.unwrap_or(1);
        if let Motion::Up | Motion::Down = m {
            let line = line_of(d, d.cursor);
            let target = if m == Motion::Up { line.saturating_sub(n) } else { (line + n).min(last_line(d)) };
            if target != line {
                d.vertical(target as isize - line as isize, false);
            }
            return;
        }
        if let Some(t) = self.eval(d, m, count) {
            let pos = if t.kind == Kind::Linewise { first_non_blank(d, line_of(d, t.pos)) } else { t.pos };
            d.cursor = pos;
            d.set_want_col(if m == Motion::LineEnd { Some(usize::MAX) } else { None });
        }
    }

    /// Where motion `m` goes from the cursor (None = it can't move).
    fn eval(&mut self, d: &mut Doc, m: Motion, count: Option<usize>) -> Option<Target> {
        use Kind::*;
        let n = count.unwrap_or(1);
        let cur = d.cursor;
        let line = line_of(d, cur);
        let (ls, le) = (d.line_start(line), d.line_end(line));
        let last = last_line(d);
        let t = Target::new;
        Some(match m {
            Motion::Left => {
                if cur <= ls {
                    return None;
                }
                t(cur.saturating_sub(n).max(ls), Exclusive)
            }
            Motion::Right => {
                if cur >= le {
                    return None;
                }
                t((cur + n).min(le), Exclusive)
            }
            Motion::Up | Motion::Down => {
                let target = if m == Motion::Up { line.checked_sub(n)? } else { line + n };
                if target > last {
                    return None;
                }
                t(d.line_start(target), Linewise)
            }
            Motion::NextLine | Motion::PrevLine => {
                let target = if m == Motion::PrevLine { line.checked_sub(n)? } else { line + n };
                if target > last {
                    return None;
                }
                t(d.line_start(target), Linewise)
            }
            Motion::WordFwd(big) => t((0..n).fold(cur, |p, _| motion::word_fwd(d, p, big)), Exclusive),
            Motion::WordBack(big) => t((0..n).fold(cur, |p, _| motion::word_back(d, p, big)), Exclusive),
            Motion::WordEnd(big) => t((0..n).fold(cur, |p, _| motion::word_end(d, p, big)), Inclusive),
            Motion::LineStart => t(ls, Exclusive),
            Motion::FirstNonBlank => t(first_non_blank(d, line), Exclusive),
            Motion::LineEnd => t(d.line_end((line + n - 1).min(last)), Exclusive),
            Motion::Top | Motion::Bottom => {
                let target = match count {
                    Some(c) => c.saturating_sub(1).min(last),
                    None if m == Motion::Top => 0,
                    None => last,
                };
                t(d.line_start(target), Linewise)
            }
            Motion::Find { ch, fwd, till } => {
                self.last_find = Some((ch, fwd, till));
                let p = motion::find_char(d, cur, ch, fwd, till, n, false)?;
                t(p, if fwd { Inclusive } else { Exclusive })
            }
            Motion::RepeatFind(back) => {
                let (ch, fwd, till) = self.last_find?;
                let fwd = fwd != back;
                let p = motion::find_char(d, cur, ch, fwd, till, n, true)?;
                t(p, if fwd { Inclusive } else { Exclusive })
            }
            Motion::Match => t(motion::match_pair(d, cur)?, Inclusive),
            Motion::ParaFwd => t(motion::para_fwd(d, cur, n), Exclusive),
            Motion::ParaBack => t(motion::para_back(d, cur, n), Exclusive),
            Motion::SearchNext(back) => {
                let Some((q, fwd, whole)) = self.last_search.clone() else {
                    self.msg = Some("E35: No previous regular expression".into());
                    return None;
                };
                t(self.search(d, &q, fwd != back, n, whole, cur)?, Exclusive)
            }
            Motion::Star(fwd) => {
                let Some((s, e)) = (cur..le)
                    .find(|p| motion::cls(d.rope.char(*p), false) == 1)
                    .and_then(|p| motion::object(d, p, Obj::Word(false), true))
                else {
                    self.msg = Some("E348: No string under cursor".into());
                    return None;
                };
                let word = d.rope.slice(s..e).to_string();
                self.last_search = Some((word.clone(), fwd, true));
                t(self.search(d, &word, fwd, n, true, s)?, Exclusive)
            }
        })
    }

    /// Finds the `n`th match of `q` from `from` with `Doc::find` (wrapping, smart case) and returns
    /// its start; the doc's cursor and selection are left alone.
    fn search(&mut self, d: &mut Doc, q: &str, fwd: bool, n: usize, whole: bool, from: usize) -> Option<usize> {
        let (c0, a0) = (d.cursor, d.anchor);
        let qn = q.chars().count().max(1);
        let total = len(d);
        let mut p = from;
        let mut wrapped = false;
        let mut found = Some(());
        'outer: for _ in 0..n {
            let start = p;
            for tries in 0.. {
                d.anchor = None;
                d.cursor = if fwd { (p + 1).min(total) } else { (p + qn - 1).min(total) };
                if !d.find(q, !fwd) {
                    found = None;
                    break 'outer;
                }
                let s = d.selection().map_or(d.cursor, |(s, _)| s);
                if (fwd && s <= p) || (!fwd && s >= p) {
                    wrapped = true;
                }
                p = s;
                if !whole || is_whole_word(d, s, qn) {
                    break;
                }
                if p == start || tries > 10_000 {
                    found = None;
                    break 'outer;
                }
            }
        }
        d.cursor = c0;
        d.anchor = a0;
        if found.is_none() {
            self.msg = Some(format!("E486: Pattern not found: {q}"));
            return None;
        }
        if wrapped {
            self.msg = Some(if fwd { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" }.into());
        }
        Some(p)
    }

    fn op_target(&mut self, d: &mut Doc, clip: &mut Clip, op: Op, tgt: Tgt, count: Option<usize>) -> Effect {
        let n = count.unwrap_or(1);
        let cur = d.cursor;
        let line = line_of(d, cur);
        let range = match tgt {
            Tgt::Lines => Range::Lines(line, (line + n - 1).min(last_line(d))),
            Tgt::Obj(o, inner) => match motion::object(d, cur, o, inner) {
                Some((s, e)) => Range::Chars(s, e),
                None => return Effect::None,
            },
            Tgt::Motion(Motion::WordFwd(big))
                if op == Op::Change && cur < len(d) && !d.rope.char(cur).is_whitespace() =>
            {
                // cw on a word changes to the end of the word, like ce (but stays in a 1-char word).
                let c = motion::cls(d.rope.char(cur), big);
                let mut q = cur;
                while q + 1 < d.line_end(line) && motion::cls(d.rope.char(q + 1), big) == c {
                    q += 1;
                }
                for _ in 1..n {
                    q = motion::word_end(d, q, big);
                }
                Range::Chars(cur, (q + 1).min(len(d)))
            }
            Tgt::Motion(m) => {
                let Some(t) = self.eval(d, m, count) else { return Effect::None };
                let (s, e) = (cur.min(t.pos), cur.max(t.pos));
                match t.kind {
                    Kind::Linewise => Range::Lines(line_of(d, s), line_of(d, e)),
                    Kind::Inclusive => Range::Chars(s, (e + 1).min(len(d))),
                    Kind::Exclusive => exclusive_range(d, s, e, m),
                }
            }
        };
        self.apply(d, clip, op, range)
    }

    fn apply(&mut self, d: &mut Doc, clip: &mut Clip, op: Op, r: Range) -> Effect {
        let col = d.col();
        match r {
            Range::Chars(s, e) => {
                if s >= e {
                    return Effect::None;
                }
                let text = d.rope.slice(s..e).to_string();
                match op {
                    Op::Yank => {
                        *clip = Clip { text: text.clone(), whole_line: false };
                        d.cursor = s;
                        return Effect::Yank(text);
                    }
                    Op::Delete | Op::Change => {
                        *clip = Clip { text, whole_line: false };
                        d.replace(s, e, "");
                        d.cursor = s;
                        if op == Op::Change {
                            self.start_insert(1, false);
                        }
                    }
                    Op::Indent | Op::Outdent => {
                        let (a, b) = (line_of(d, s), line_of(d, e - 1));
                        d.indent(a, b, op == Op::Indent);
                        d.cursor = first_non_blank(d, a);
                    }
                    Op::Lower | Op::Upper | Op::Toggle => {
                        d.replace(s, e, &recase(&text, op));
                        d.cursor = s;
                    }
                }
            }
            Range::Lines(a, b) => {
                let (s, e) = (d.line_start(a), line_after(d, b));
                let mut text = d.rope.slice(s..e).to_string();
                if !text.ends_with('\n') {
                    text.push_str(d.eol());
                }
                match op {
                    Op::Yank => {
                        *clip = Clip { text: text.clone(), whole_line: true };
                        if a < line_of(d, d.cursor) {
                            d.cursor = s + col.min(d.line_len(a));
                        }
                        return Effect::Yank(text);
                    }
                    Op::Delete => {
                        *clip = Clip { text, whole_line: true };
                        // The last line has no line break of its own: take the one before it.
                        let s = if e == len(d) && a > 0 && (e == s || d.rope.char(e - 1) != '\n') {
                            d.line_end(a - 1)
                        } else {
                            s
                        };
                        d.replace(s, e, "");
                        d.cursor = first_non_blank(d, a.min(last_line(d)));
                    }
                    Op::Change => {
                        *clip = Clip { text, whole_line: true };
                        let keep = s + motion::indent_len(d, a);
                        d.replace(keep, d.line_end(b), "");
                        d.cursor = keep;
                        self.start_insert(1, false);
                    }
                    Op::Indent | Op::Outdent => {
                        d.indent(a, b, op == Op::Indent);
                        d.cursor = first_non_blank(d, a);
                    }
                    Op::Lower | Op::Upper | Op::Toggle => {
                        let e = d.line_end(b);
                        let t = recase(&d.rope.slice(s..e).to_string(), op);
                        d.replace(s, e, &t);
                        d.cursor = s;
                    }
                }
            }
        }
        Effect::None
    }
}

/// Start of the line after `line` (or the end of the file).
fn line_after(d: &Doc, line: usize) -> usize {
    if line + 1 < d.rope.len_lines() {
        d.line_start(line + 1)
    } else {
        len(d)
    }
}

/// Vim's special cases for exclusive motions under an operator: `dw` at the end of a line stops
/// there, and a motion ending at column 0 of a later line stops at the end of the line before it
/// (whole lines when it started at the indent, e.g. `d}`).
fn exclusive_range(d: &Doc, s: usize, e: usize, m: Motion) -> Range {
    if e <= s || m == Motion::LineEnd {
        return Range::Chars(s, e);
    }
    let (ls, le) = (line_of(d, s), line_of(d, e));
    if le > ls && matches!(m, Motion::WordFwd(_)) {
        let end = d.line_end(le - 1);
        return Range::Chars(s, end.max(s));
    }
    if le > ls && e == d.line_start(le) {
        if s <= first_non_blank(d, ls) {
            return Range::Lines(ls, le - 1);
        }
        return Range::Chars(s, d.line_end(le - 1));
    }
    Range::Chars(s, e)
}

/// Normal-mode cursor: on a character, never past the last one of its line.
fn clamp(d: &mut Doc) {
    let n = len(d);
    if d.cursor > n {
        d.cursor = n;
    }
    let line = line_of(d, d.cursor);
    let max = d.line_start(line) + d.line_len(line).saturating_sub(1);
    if d.cursor > max {
        d.cursor = max;
    }
}

fn type_key(d: &mut Doc, k: K) {
    match k {
        K::Ch(c) => d.type_char(c),
        K::Enter => d.newline(),
        K::Back => d.backspace(),
        K::Del => d.delete(),
        K::Tab => d.tab(),
        _ => {}
    }
}

fn count_at(ks: &[K], i: &mut usize) -> Option<usize> {
    let mut n: Option<usize> = None;
    while let Some(K::Ch(c)) = ks.get(*i) {
        match c.to_digit(10) {
            Some(v) if v > 0 || n.is_some() => {
                n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(v as usize).min(100_000));
                *i += 1;
            }
            _ => break,
        }
    }
    n
}

fn mul(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1)).min(100_000)),
    }
}

fn parse_motion(ks: &[K], j: usize) -> Parsed {
    use Motion::*;
    let Some(&k) = ks.get(j) else { return Parsed::More };
    let m = match k {
        K::Ch('h') | K::Left | K::Back => Left,
        K::Ch('l') | K::Right | K::Ch(' ') => Right,
        K::Ch('j') | K::Down => Down,
        K::Ch('k') | K::Up => Up,
        K::Ch('w') => WordFwd(false),
        K::Ch('W') => WordFwd(true),
        K::Ch('b') => WordBack(false),
        K::Ch('B') => WordBack(true),
        K::Ch('e') => WordEnd(false),
        K::Ch('E') => WordEnd(true),
        K::Ch('0') | K::Home => LineStart,
        K::Ch('^') => FirstNonBlank,
        K::Ch('$') | K::End => LineEnd,
        K::Ch('+') | K::Enter => NextLine,
        K::Ch('-') => PrevLine,
        K::Ch('G') => Bottom,
        K::Ch('%') => Match,
        K::Ch('}') => ParaFwd,
        K::Ch('{') => ParaBack,
        K::Ch(';') => RepeatFind(false),
        K::Ch(',') => RepeatFind(true),
        K::Ch('n') => SearchNext(false),
        K::Ch('N') => SearchNext(true),
        K::Ch('*') => Star(true),
        K::Ch('#') => Star(false),
        K::Ch('g') => match ks.get(j + 1) {
            None => return Parsed::More,
            Some(K::Ch('g')) => Top,
            _ => return Parsed::Bad,
        },
        K::Ch(c @ ('f' | 'F' | 't' | 'T')) => match ks.get(j + 1) {
            None => return Parsed::More,
            Some(K::Ch(x)) => Find { ch: *x, fwd: c == 'f' || c == 't', till: c == 't' || c == 'T' },
            _ => return Parsed::Bad,
        },
        _ => return Parsed::Bad,
    };
    Parsed::Done(Cmd { count: None, act: Act::Move(m) })
}

fn is_whole_word(d: &Doc, s: usize, n: usize) -> bool {
    let word = |p: usize| motion::cls(d.rope.char(p), false) == 1;
    (s == 0 || !word(s - 1)) && (s + n >= len(d) || !word(s + n))
}

fn recase(text: &str, op: Op) -> String {
    match op {
        Op::Lower => text.to_lowercase(),
        Op::Upper => text.to_uppercase(),
        _ => text
            .chars()
            .flat_map(|c| -> Box<dyn Iterator<Item = char>> {
                if c.is_uppercase() {
                    Box::new(c.to_lowercase())
                } else {
                    Box::new(c.to_uppercase())
                }
            })
            .collect(),
    }
}

/// J: joins `line` with the `joins` lines after it, one space between (none before `)` or after
/// blanks), the next lines' indent removed. The cursor ends at the last join.
fn join(d: &mut Doc, line: usize, joins: usize) {
    for _ in 0..joins {
        if line + 1 > last_line(d) {
            break;
        }
        let e = d.line_end(line);
        let next = d.line_start(line + 1);
        let ind = motion::indent_len(d, line + 1);
        let rest_empty = d.line_len(line + 1) == ind;
        let cur_blank_end = d.line_len(line) == 0 || matches!(d.rope.char(e - 1), ' ' | '\t');
        let next_ch = d.rope.get_char(next + ind);
        let sep = if rest_empty || cur_blank_end || next_ch == Some(')') { "" } else { " " };
        d.replace(e, next + ind, sep);
        d.cursor = e;
    }
}

/// p / P: whole lines go below / above the cursor line, other text after / before the cursor.
fn paste(d: &mut Doc, clip: &Clip, after: bool, n: usize) {
    if clip.text.is_empty() {
        return;
    }
    let text = clip.text.repeat(n);
    let line = line_of(d, d.cursor);
    if clip.whole_line {
        let pos = if after { line_after(d, line) } else { d.line_start(line) };
        let total = len(d);
        if after && pos == total && (total == 0 || d.rope.char(total - 1) != '\n') {
            let body = text.strip_suffix('\n').map(|t| t.strip_suffix('\r').unwrap_or(t)).unwrap_or(&text);
            d.replace(total, total, &format!("\n{body}"));
            d.cursor = first_non_blank(d, line + 1);
        } else {
            d.replace(pos, pos, &text);
            d.cursor = first_non_blank(d, line_of(d, pos));
        }
    } else {
        let pos = if after && d.line_len(line) > 0 { (d.cursor + 1).min(d.line_end(line)) } else { d.cursor };
        let k = d.replace(pos, pos, &text);
        d.cursor = pos + k.saturating_sub(1);
    }
}
