//! Completion as you type, like IntelliJ (decision 0011):
//! - typing a name (1+ word chars with a server, 2+ for buffer words) asks for completion after
//!   [`AUTO_DELAY`] without typing; the server's `triggerCharacters` (`.` by default; `::` in
//!   Rust/C++) ask at once. Not inside comments or strings (tree-sitter), not in prose (plain
//!   text, Markdown) unless asked, not while a snippet's placeholders are walked, never in Vim's
//!   normal mode, and not again for a word whose popup was closed with Esc.
//! - every completion request has a sequence number; an answer to an older one, or one whose
//!   word the cursor has left, is dropped. Requests are asynchronous: typing never waits.
//! - files without a server get words from the open buffers ([`super::words`]); a server answer
//!   with few matches gets them appended (deduplicated).
//! - Ctrl+Space, and where macOS takes Ctrl+Space (input sources): **Ctrl+J**, Ctrl+@ (NUL).
//!   `autocomplete = false` in `settings.toml` keeps only the keys.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lsp_types::CompletionResponse;
use serde_json::{json, Value};

use super::popup::{Item, Popup};
use super::server::Pending;
use super::text::{path_to_uri, to_lsp};
use super::{config, is_word_in, word_start, words};
use crate::app::App;
use crate::syntax::Lang;

/// Pause in typing before an automatic request goes out.
pub const AUTO_DELAY: Duration = Duration::from_millis(100);
/// Word characters typed before the popup opens by itself: with a server, and for buffer words.
pub const MIN_SERVER: usize = 1;
pub const MIN_WORDS: usize = 2;
/// A server answer with fewer matches than this gets buffer words appended.
pub const FEW: usize = 10;

/// An automatic request waiting for a pause in typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoReq {
    pub doc: usize,
    pub rev: u64,
    pub cursor: usize,
    pub at: Instant,
}

/// The completion keys: Ctrl+Space (also arrives as NUL = Ctrl+@ / Ctrl+Shift+2), and Ctrl+J
/// (macOS uses Ctrl+Space to switch input sources, so it often never arrives there).
pub fn is_complete_key(k: &KeyEvent, vim_cmd: bool) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Null => true,
        KeyCode::Char(' ') | KeyCode::Char('@') => ctrl && !alt,
        KeyCode::Char('2') => ctrl && shift && !alt,
        // Vim's normal mode keeps Ctrl+J (line down).
        KeyCode::Char('j') => ctrl && !alt && !shift && !vim_cmd,
        _ => false,
    }
}

/// The characters that open completion at once: the server's `triggerCharacters` (whitespace
/// and word characters left out: typescript-language-server lists " ", which would ask after
/// every space), else `.`.
pub fn trigger_chars(caps: &Value, lang: Lang) -> Vec<char> {
    let listed: Option<Vec<char>> = caps
        .pointer("/completionProvider/triggerCharacters")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter_map(|s| s.chars().next()).collect());
    let mut v: Vec<char> = listed.unwrap_or_else(|| vec!['.']).into_iter().filter(|&c| !c.is_whitespace() && !is_word_in(lang, c)).collect();
    v.dedup();
    v
}

/// Whether typing `c` (after `prev`) triggers completion: one of `triggers`; a `:` only as `::`
/// in Rust and C++ (a lone `:` is a type annotation or a label there).
pub fn is_trigger(lang: Lang, triggers: &[char], c: char, prev: Option<char>) -> bool {
    if !triggers.contains(&c) {
        return false;
    }
    !(c == ':' && matches!(lang, Lang::Rust | Lang::Cpp | Lang::C) && prev != Some(':'))
}

/// Prose files: buffer words don't pop up by themselves there (Enter would complete words in
/// running text); Ctrl+Space / Ctrl+J still offer them.
pub fn prose(lang: Lang) -> bool {
    matches!(lang, Lang::Plain | Lang::Markdown)
}

/// Whether a word typed so far (`typed`, from its start to the cursor) should open the popup by
/// itself: enough characters, not a number.
pub fn wants_auto(typed: &str, server: bool) -> bool {
    let n = typed.chars().count();
    let min = if server { MIN_SERVER } else { MIN_WORDS };
    n >= min && !typed.chars().next().is_some_and(|c| c.is_ascii_digit())
}

impl App {
    /// The main server of the active file and its capabilities, when it can complete (started,
    /// or still starting); `None` = complete from buffer words.
    fn completion_server(&mut self) -> Option<(usize, Value)> {
        let path = self.doc().path.clone()?;
        let lang = self.doc().syntax.lang;
        if !config::get().has_servers(lang.key()) || self.lsp.no_server.contains(&lang) {
            return None;
        }
        let key = self.lsp.key(&path);
        let sid = *self.lsp.open.get(&key)?.servers.first()?;
        let s = self.lsp.server(sid)?;
        let can = s.caps.get("completionProvider").is_some_and(|c| !c.is_null() && c != &json!(false));
        (!s.ready || can).then(|| (sid, s.caps.clone()))
    }

    /// Asks for completion at the cursor: the server, else the buffer words. `auto` = typed, not
    /// a key: no messages, and a list that only repeats what is typed isn't shown.
    pub(super) fn request_completion(&mut self, trigger: Option<char>, auto: bool) {
        let start = word_start(self.doc());
        self.lsp.comp_seq += 1;
        let seq = self.lsp.comp_seq;
        self.lsp.auto_at = None;
        self.lsp_sync(Some(self.active));
        let Some((sid, _)) = self.completion_server() else {
            if trigger.is_none() {
                self.word_popup(start, auto);
            }
            return;
        };
        let key = self.lsp.key(&self.doc().path.clone().unwrap_or_default());
        let uri = path_to_uri(&key);
        let ctx = match trigger {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            None => json!({ "triggerKind": if auto { 3 } else { 1 } }),
        };
        let req = to_lsp(&self.doc().rope, self.doc().cursor);
        let Some(s) = self.lsp.server(sid) else { return };
        if !s.ready && !auto {
            self.message = Some("Language server is still starting…".into());
        }
        s.request("textDocument/completion", json!({ "textDocument": { "uri": uri }, "position": req, "context": ctx }), Pending::Completion { path: key, start, req, seq, auto });
    }

    /// The word being typed (from `start` to the cursor) if the cursor is still on its line and
    /// it holds only word characters.
    fn typed_since(&self, start: usize) -> Option<String> {
        let d = self.doc();
        if d.cursor < start || d.rope.char_to_line(d.cursor) != d.rope.char_to_line(start) {
            return None;
        }
        let t = d.rope.slice(start..d.cursor).to_string();
        t.chars().all(|c| is_word_in(d.syntax.lang, c)).then_some(t)
    }

    /// Buffer words for the word at `start` as completion items.
    fn word_items(&self, start: usize, prefix: &str) -> Vec<Item> {
        let d = self.doc();
        let lang = d.syntax.lang;
        let is_word = move |c: char| is_word_in(lang, c);
        let found = words::complete(&self.docs, self.active, start, prefix, &is_word);
        // Tab replaces the rest of the word after the cursor.
        let rest: usize = d.rope.chars_at(d.cursor).take_while(|&c| is_word(c)).map(char::len_utf16).sum();
        let req = to_lsp(&d.rope, d.cursor);
        let end = (rest > 0).then(|| lsp_types::Position { line: req.line, character: req.character + rest as u32 });
        found.iter().map(|w| Item::word(w, end)).collect()
    }

    /// Completion from the buffers' words only (no server). Marked incomplete, so typing asks
    /// again (the best words change as the prefix grows).
    fn word_popup(&mut self, start: usize, auto: bool) {
        let Some(prefix) = self.typed_since(start) else { return };
        let items = self.word_items(start, &prefix);
        let req = to_lsp(&self.doc().rope, self.doc().cursor);
        let popup = Popup::completion(items, start, true, &prefix, req);
        self.show_completion(popup, &prefix, auto);
    }

    /// Shows a completion list, or nothing when it's empty (or, typed, only repeats the word).
    fn show_completion(&mut self, popup: Popup, prefix: &str, auto: bool) {
        let useless = match &popup {
            Popup::Completion { shown, items, .. } => shown.is_empty() || (auto && shown.iter().all(|&i| items[i].adds_nothing(prefix))),
            _ => true,
        };
        if useless {
            self.lsp.popup = None;
            if !auto {
                self.message = Some("No suggestions".into());
            }
        } else {
            self.lsp.popup = Some(popup);
        }
    }

    /// A server's completion answer (dropped when stale: a newer request went out, another file
    /// is active, or the cursor left the word).
    pub(super) fn on_completion(&mut self, path: std::path::PathBuf, start: usize, req: lsp_types::Position, seq: u64, auto: bool, v: Value) {
        if seq != self.lsp.comp_seq {
            return;
        }
        let here = self.doc().path.clone().map(|p| self.lsp.key(&p));
        if here.as_ref() != Some(&path) {
            return;
        }
        let Some(prefix) = self.typed_since(start) else { return };
        if auto && self.lsp.popup.as_ref().is_some_and(|p| !matches!(p, Popup::Completion { .. })) {
            return; // a picker / hover / quick fixes opened meanwhile
        }
        let (items, incomplete) = match serde_json::from_value::<Option<CompletionResponse>>(v).ok().flatten() {
            Some(CompletionResponse::Array(a)) => (a, false),
            Some(CompletionResponse::List(l)) => (l.items, l.is_incomplete),
            None => (Vec::new(), false),
        };
        let mut items: Vec<Item> = items.into_iter().map(Item::from_lsp).collect();
        if !prefix.is_empty() && items.iter().filter(|i| i.matches(&prefix)).count() < FEW {
            // A word the server offers already (by label, name or inserted text) isn't repeated.
            let have: std::collections::HashSet<String> = items.iter().flat_map(|i| [i.label.clone(), i.name().to_string(), i.insert.text.clone()]).collect();
            items.extend(self.word_items(start, &prefix).into_iter().filter(|w| !have.contains(&w.label)));
        }
        let popup = Popup::completion(items, start, incomplete, &prefix, req);
        self.show_completion(popup, &prefix, auto);
    }

    /// After an editor key (called by `smart_after_key`, with the popup already filtered): opens
    /// completion on a trigger character, or schedules it while a name is typed.
    pub(super) fn auto_after_key(&mut self, k: KeyEvent, typing: bool) {
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        let KeyCode::Char(c) = k.code else {
            if !matches!(k.code, KeyCode::Backspace) {
                self.lsp.auto_at = None;
            }
            return;
        };
        if !self.lsp.auto || !typing || !plain || self.lsp.snippet.is_some() || self.lsp.popup.is_some() {
            self.lsp.auto_at = None;
            return;
        }
        let d = self.doc();
        let lang = d.syntax.lang;
        let start = word_start(d);
        let prev = d.cursor.checked_sub(2).map(|p| d.rope.char(p));
        let caps = self.completion_server().map(|(_, caps)| caps);
        if let Some(caps) = &caps {
            if is_trigger(lang, &trigger_chars(caps, lang), c, prev) {
                if !self.in_comment() {
                    self.request_completion(Some(c), true);
                }
                return;
            }
        }
        if !is_word_in(lang, c) || self.lsp.auto_off == Some((self.active, start)) {
            self.lsp.auto_at = None;
            return;
        }
        let Some(typed) = self.typed_since(start) else { return };
        let server = caps.is_some();
        if wants_auto(&typed, server) && (server || !prose(lang)) {
            let d = self.doc();
            self.lsp.auto_at = Some(AutoReq { doc: self.active, rev: d.rev(), cursor: d.cursor, at: Instant::now() });
        }
    }

    /// Whether the character before the cursor sits in a comment or string (code files only).
    fn in_comment(&mut self) -> bool {
        let d = &mut self.docs[self.active];
        let rev = d.rev();
        let at = d.cursor;
        d.syntax.in_comment_or_string(&d.rope, rev, at)
    }

    /// From `lsp_tick`: sends the scheduled automatic request once typing paused. Returns
    /// whether the screen may have changed.
    pub(super) fn auto_tick(&mut self, now: Instant) -> bool {
        let Some(a) = self.lsp.auto_at else { return false };
        if now.duration_since(a.at) < AUTO_DELAY {
            return false;
        }
        self.lsp.auto_at = None;
        let d = self.doc();
        let elsewhere = self.prompt.is_some() || self.project.popup.is_some() || self.dock.has_focus() || (self.agents.visible && self.agents.focus);
        if a.doc != self.active || a.rev != d.rev() || a.cursor != d.cursor || self.lsp.popup.is_some() || elsewhere || self.in_comment() {
            return false;
        }
        self.request_completion(None, true);
        self.lsp.popup.is_some()
    }

    /// Esc on the completion list: close it, drop answers on their way, and don't open it again
    /// by itself for this word.
    pub(super) fn completion_dismissed(&mut self) {
        let start = match &self.lsp.popup {
            Some(Popup::Completion { start, .. }) => *start,
            _ => word_start(self.doc()),
        };
        self.lsp.auto_off = Some((self.active, start));
        self.lsp.auto_at = None;
        self.lsp.comp_seq += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::Event;
    use KeyModifiers as M;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn completion_keys() {
        assert!(is_complete_key(&k(KeyCode::Char(' '), M::CONTROL), false), "Ctrl+Space (also NUL in a legacy terminal)");
        assert!(is_complete_key(&k(KeyCode::Char('j'), M::CONTROL), false), "Ctrl+J: the Mac fallback");
        assert!(!is_complete_key(&k(KeyCode::Char('j'), M::CONTROL), true), "Vim normal mode keeps Ctrl+J");
        assert!(is_complete_key(&k(KeyCode::Char('@'), M::CONTROL | M::SHIFT), false), "Ctrl+@ (kitty protocol)");
        assert!(is_complete_key(&k(KeyCode::Char('2'), M::CONTROL | M::SHIFT), false));
        assert!(is_complete_key(&k(KeyCode::Null, M::NONE), false));
        assert!(!is_complete_key(&k(KeyCode::Char(' '), M::NONE), false));
        assert!(!is_complete_key(&k(KeyCode::Char(' '), M::ALT), false), "Alt+Space is Search Everywhere");
        assert!(!is_complete_key(&k(KeyCode::Char('j'), M::CONTROL | M::ALT), false));
        assert!(!is_complete_key(&k(KeyCode::Char('2'), M::CONTROL), false));
    }

    #[test]
    fn trigger_characters_come_from_the_server() {
        let ts = json!({ "completionProvider": { "triggerCharacters": [".", "\"", "'", "/", "@", "<", "#", " "] } });
        assert_eq!(trigger_chars(&ts, Lang::TypeScript), ['.', '"', '\'', '/', '@', '<', '#'], "no space");
        assert_eq!(trigger_chars(&json!({ "completionProvider": {} }), Lang::Python), ['.'], "default");
        assert_eq!(trigger_chars(&Value::Null, Lang::Python), ['.']);
        let ra = json!({ "completionProvider": { "triggerCharacters": [":", ".", "'", "("] } });
        let t = trigger_chars(&ra, Lang::Rust);
        assert!(is_trigger(Lang::Rust, &t, ':', Some(':')), "::");
        assert!(!is_trigger(Lang::Rust, &t, ':', Some('x')), "a lone : is a type annotation");
        assert!(is_trigger(Lang::Rust, &t, '.', Some('x')));
        assert!(!is_trigger(Lang::Rust, &t, ',', Some('x')));
        assert!(is_trigger(Lang::Python, &[':'], ':', None), "other languages take the server's word for it");
    }

    #[test]
    fn auto_needs_enough_letters_and_no_number() {
        assert!(wants_auto("g", true));
        assert!(!wants_auto("g", false), "buffer words need two");
        assert!(wants_auto("gr", false));
        assert!(!wants_auto("12", true), "numbers");
        assert!(!wants_auto("", true));
        assert!(prose(Lang::Plain) && prose(Lang::Markdown) && !prose(Lang::Xml));
    }

    fn app(name: &str, text: &str) -> App {
        let d = std::env::temp_dir().join(format!("catlas-auto-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("CATLAS_CONFIG_DIR", d.join(".cfg"));
        std::fs::write(d.join(name), text).unwrap();
        let mut app = App::new(vec![d.clone(), d.join(name)]).unwrap();
        app.active = 0;
        app.docs[0].doc_end(false);
        app
    }

    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle(Event::Key(k(KeyCode::Char(c), M::NONE)));
        }
    }

    fn shown(app: &App) -> Vec<String> {
        match &app.lsp.popup {
            Some(Popup::Completion { items, shown, .. }) => shown.iter().map(|&i| items[i].label.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Runs the loop's tick as if `ms` passed since the last key.
    fn after(app: &mut App, ms: u64) -> bool {
        let at = app.lsp.auto_at.map(|a| a.at).unwrap_or_else(Instant::now);
        app.auto_tick(at + Duration::from_millis(ms))
    }

    #[test]
    fn typing_opens_buffer_words_after_a_pause_only() {
        // XML has no server installed here (lemminx missing): buffer words.
        let mut a = app("pom.xml", "<project>\n  <dependency>\n  <dependencies/>\n</project>\n");
        typ(&mut a, "<dep");
        assert!(a.lsp.popup.is_none(), "nothing before the pause");
        assert!(a.lsp.auto_at.is_some(), "scheduled");
        assert!(!after(&mut a, 50), "still typing");
        assert!(a.lsp.popup.is_none());
        assert!(after(&mut a, 120));
        assert_eq!(shown(&a), ["dependencies", "dependency"], "the nearer one first");
        // Typing filters; Enter inserts.
        typ(&mut a, "endenc");
        assert_eq!(shown(&a), ["dependencies", "dependency"]);
        typ(&mut a, "ie");
        assert_eq!(shown(&a), ["dependencies"]);
        a.handle(Event::Key(k(KeyCode::Enter, M::NONE)));
        assert!(a.doc().rope.to_string().ends_with("<dependencies"), "{}", a.doc().rope);
        assert!(a.lsp.popup.is_none());
    }

    #[test]
    fn a_key_typed_meanwhile_cancels_the_pending_request() {
        let mut a = app("a.xml", "<alpha/>\n<al");
        typ(&mut a, "p");
        let first = a.lsp.auto_at.unwrap();
        typ(&mut a, "h");
        let second = a.lsp.auto_at.unwrap();
        assert!(second.rev != first.rev);
        // A stale request (older revision) doesn't fire.
        a.lsp.auto_at = Some(first);
        assert!(!after(&mut a, 200));
        assert!(a.lsp.popup.is_none());
        // Moving the cursor away cancels it too.
        a.lsp.auto_at = Some(AutoReq { at: Instant::now(), ..second });
        a.handle(Event::Key(k(KeyCode::Left, M::NONE)));
        assert!(a.lsp.auto_at.is_none());
    }

    #[test]
    fn prose_comments_and_the_setting_stay_quiet() {
        let mut a = app("notes.txt", "elephant\n");
        typ(&mut a, "ele");
        assert!(a.lsp.auto_at.is_none(), "plain text: no popup by itself");
        a.handle(Event::Key(k(KeyCode::Char('j'), M::CONTROL)));
        assert_eq!(shown(&a), ["elephant"], "Ctrl+J offers the words");
        a.handle(Event::Key(k(KeyCode::Esc, M::NONE)));
        assert!(a.lsp.popup.is_none());

        let mut a = app("x.xml", "<elephant/>\n<!--  -->\n");
        let at = a.doc().rope.to_string().find("<!-- ").unwrap() + 5;
        a.docs[0].move_to(at, false);
        typ(&mut a, "ele");
        assert!(a.lsp.auto_at.is_some());
        assert!(!after(&mut a, 150), "inside an XML comment");
        assert!(a.lsp.popup.is_none());

        let mut a = app("y.xml", "<elephant/>\n<");
        a.lsp.auto = false;
        typ(&mut a, "ele");
        assert!(a.lsp.auto_at.is_none(), "autocomplete = false");
        a.handle(Event::Key(k(KeyCode::Char(' '), M::CONTROL)));
        assert_eq!(shown(&a), ["elephant"], "the keys still work");
    }

    #[test]
    fn esc_keeps_the_word_quiet_until_the_next_one() {
        let mut a = app("e.xml", "<elephant/>\n<");
        typ(&mut a, "el");
        after(&mut a, 150);
        assert_eq!(shown(&a), ["elephant"]);
        a.handle(Event::Key(k(KeyCode::Esc, M::NONE)));
        assert!(a.lsp.popup.is_none());
        typ(&mut a, "e");
        assert!(a.lsp.auto_at.is_none(), "same word: stays closed");
        typ(&mut a, " el");
        assert!(a.lsp.auto_at.is_some(), "a new word may pop up again");
    }

    #[test]
    fn enter_on_a_word_that_is_typed_already_makes_a_new_line() {
        let mut a = app("f.xml", "<elephants/>\n<");
        typ(&mut a, "elephant");
        a.handle(Event::Key(k(KeyCode::Char('j'), M::CONTROL)));
        assert_eq!(shown(&a), ["elephants"]);
        // The popup filters to "elephants" only; Enter accepts it.
        a.handle(Event::Key(k(KeyCode::Enter, M::NONE)));
        assert!(a.doc().rope.to_string().ends_with("<elephants"));
        // A server list whose pick equals the typed word: Enter is a new line.
        let start = a.doc().cursor - "elephants".len();
        let req = to_lsp(&a.doc().rope, a.doc().cursor);
        let it = Item::from_lsp(lsp_types::CompletionItem { label: "elephants".into(), ..Default::default() });
        a.lsp.popup = Some(Popup::completion(vec![it], start, false, "elephants", req));
        a.handle(Event::Key(k(KeyCode::Enter, M::NONE)));
        assert!(a.lsp.popup.is_none());
        assert!(a.doc().rope.to_string().ends_with("<elephants\n"), "{:?}", a.doc().rope.to_string());
    }

    #[test]
    fn stale_server_answers_are_dropped() {
        let mut a = app("s.xml", "<some/>\n<so");
        let key = a.lsp.key(&a.doc().path.clone().unwrap());
        let start = a.doc().cursor - 2;
        let req = to_lsp(&a.doc().rope, a.doc().cursor);
        let answer = json!([{ "label": "something" }, { "label": "sonar" }]);
        a.lsp.comp_seq = 5;
        a.on_completion(key.clone(), start, req, 4, true, answer.clone());
        assert!(a.lsp.popup.is_none(), "an older request's answer");
        a.on_completion(key.clone(), start, req, 5, true, answer.clone());
        assert_eq!(shown(&a), ["something", "sonar", "some"], "server items first, buffer words appended");
        a.lsp.popup = None;
        typ(&mut a, " ");
        a.lsp.comp_seq = 6;
        a.on_completion(key.clone(), start, req, 6, true, answer.clone());
        assert!(a.lsp.popup.is_none(), "the cursor left the word");
        a.on_completion(std::path::PathBuf::from("/elsewhere.xml"), start, req, 6, true, answer);
        assert!(a.lsp.popup.is_none(), "another file");
    }

    #[test]
    fn duplicates_of_server_items_are_not_added_as_words() {
        let mut a = app("d.xml", "<some/>\n<so");
        let key = a.lsp.key(&a.doc().path.clone().unwrap());
        let start = a.doc().cursor - 2;
        let req = to_lsp(&a.doc().rope, a.doc().cursor);
        a.lsp.comp_seq = 1;
        a.on_completion(key.clone(), start, req, 1, false, json!({ "isIncomplete": false, "items": [{ "label": "some", "kind": 6 }] }));
        assert_eq!(shown(&a), ["some"]);
        // rust-analyzer labels methods "name(…)" and inserts a snippet: the word still counts as had.
        a.lsp.comp_seq = 2;
        let it = json!({ "label": "some(…)", "filterText": "some", "kind": 2, "insertTextFormat": 2, "insertText": "some(${1:x})" });
        a.on_completion(key, start, req, 2, false, json!([it]));
        assert_eq!(shown(&a), ["some(…)"]);
    }
}
