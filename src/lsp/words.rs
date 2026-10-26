//! Word completion without a language server (decision 0011): the words of the open buffers,
//! ranked by the project's fuzzy scorer and by how close they are to the cursor. Used on its own
//! for files without a (running) server — XML, Markdown, plain text, a language whose server
//! isn't installed — and under a server's items when the server has few matches.

use std::collections::HashMap;

use crate::doc::Doc;
use crate::project::fuzzy;

/// Words longer than this are noise (hashes, base64, minified code).
const MAX_WORD: usize = 64;
/// Characters scanned per buffer (the current one, then each other open one).
const SCAN_CURRENT: usize = 1_000_000;
const SCAN_OTHER: usize = 250_000;
/// Other buffers' words rank after every word of the current file.
const OTHER_FILE: u32 = 1 << 30;
/// At most this many words are offered.
pub const LIMIT: usize = 50;

/// One candidate: the word, its distance from the cursor (chars; other files rank after the
/// current one) and the other file's name, if it came from there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub prox: u32,
    pub from: Option<String>,
}

/// Collects the words of `text` (`is_word` decides what a word character is) into `out` with
/// their smallest distance from `cursor`; the occurrence that spans `skip` (the word being typed)
/// is left out.
fn scan(text: impl Iterator<Item = char>, is_word: &dyn Fn(char) -> bool, cursor: usize, skip: Option<usize>, base: u32, out: &mut HashMap<String, u32>) {
    let mut cur = String::new();
    let mut start = 0;
    let mut flush = |cur: &mut String, start: usize, end: usize| {
        let n = cur.chars().count();
        let spans_skip = skip.is_some_and(|s| s >= start && s <= end);
        let first_digit = cur.chars().next().is_some_and(|c| c.is_ascii_digit());
        if n >= 2 && n <= MAX_WORD && !spans_skip && !first_digit {
            let d = base.saturating_add(cursor.abs_diff(start).min((OTHER_FILE - 1) as usize) as u32);
            let e = out.entry(std::mem::take(cur)).or_insert(u32::MAX);
            *e = (*e).min(d);
        }
        cur.clear();
    };
    let mut i = 0;
    for c in text {
        if is_word(c) {
            if cur.is_empty() {
                start = i;
            }
            cur.push(c);
        } else if !cur.is_empty() {
            flush(&mut cur, start, i);
        }
        i += 1;
    }
    if !cur.is_empty() {
        flush(&mut cur, start, i);
    }
}

/// Ranks `found` against `prefix`: words starting with it (case-insensitive) first, nearest
/// first; then the other fuzzy matches by score, then closeness. Words equal to the prefix are
/// dropped (nothing to complete).
pub fn rank(prefix: &str, found: HashMap<String, u32>, names: &HashMap<u32, String>) -> Vec<Word> {
    let lower = prefix.to_lowercase();
    let mut v: Vec<(bool, i32, Word)> = found
        .into_iter()
        .filter(|(w, _)| w != prefix)
        .filter_map(|(w, prox)| {
            let (score, _) = fuzzy::score(prefix, &w)?;
            let from = (prox >= OTHER_FILE).then(|| names.get(&((prox - OTHER_FILE) >> 20)).cloned()).flatten();
            let starts = w.to_lowercase().starts_with(&lower);
            Some((!starts, if starts { 0 } else { score }, Word { text: w, prox, from }))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.prox.cmp(&b.2.prox)).then(a.2.text.cmp(&b.2.text)));
    v.into_iter().take(LIMIT).map(|(_, _, w)| w).collect()
}

/// The words for completing `prefix` at the cursor of `docs[active]` (the word being typed starts
/// at `start`): the current file's words by distance, then the other open files' words.
pub fn complete(docs: &[Doc], active: usize, start: usize, prefix: &str, is_word: &dyn Fn(char) -> bool) -> Vec<Word> {
    let mut found: HashMap<String, u32> = HashMap::new();
    let mut names = HashMap::new();
    let d = &docs[active];
    scan(d.rope.chars().take(SCAN_CURRENT), is_word, d.cursor, Some(start), 0, &mut found);
    for (i, o) in docs.iter().enumerate().filter(|(i, _)| *i != active) {
        // Each other file gets its own band (file index in the high bits), so names can be told.
        let base = OTHER_FILE + ((i as u32).min(1023) << 20);
        let mut theirs = HashMap::new();
        scan(o.rope.chars().take(SCAN_OTHER), is_word, 0, None, base, &mut theirs);
        for (w, p) in theirs {
            found.entry(w).or_insert(p);
        }
        names.insert(i as u32, o.name());
    }
    rank(prefix, found, &names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn doc(text: &str, cursor: usize) -> Doc {
        let mut d = Doc::empty();
        d.insert_str(text);
        d.move_to(cursor, false);
        d
    }

    fn texts(v: &[Word]) -> Vec<&str> {
        v.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn words_rank_prefix_then_score_then_closeness() {
        // Cursor after "pr" on the last line; "print_report" is closer than "prepare".
        let text = "prepare the data\nreport = 1\nprint_report(x)\npr";
        let d = doc(text, text.chars().count());
        let start = text.chars().count() - 2;
        let got = complete(&[d], 0, start, "pr", &w);
        assert_eq!(texts(&got), ["print_report", "prepare", "report"], "prefix matches first, the closer one first; fuzzy 'report' last");
        assert!(got.iter().all(|w| w.from.is_none()));
    }

    #[test]
    fn the_typed_word_numbers_and_short_words_are_left_out() {
        let text = "ab 12abc x abcdef ab";
        let d = doc(text, text.chars().count());
        let got = complete(&[d], 0, text.chars().count() - 2, "ab", &w);
        // "ab" twice: once being typed, once equal to the prefix; "12abc" starts with a digit; "x" too short.
        assert_eq!(texts(&got), ["abcdef"]);
    }

    #[test]
    fn other_buffers_come_after_the_current_one_with_their_name() {
        let here = doc("alpha_local\nal", 14);
        let mut there = doc("alpha_remote alpha_local", 0);
        there.path = Some("/x/other.txt".into());
        let got = complete(&[here, there], 0, 12, "al", &w);
        assert_eq!(texts(&got), ["alpha_local", "alpha_remote"]);
        assert_eq!(got[0].from, None, "found in the current file first");
        assert_eq!(got[1].from.as_deref(), Some("other.txt"));
    }

    #[test]
    fn empty_prefix_gives_the_nearest_words() {
        let text = "far_away\n\n\n\n\nnear_word \n";
        let d = doc(text, text.chars().count() - 1);
        let got = complete(&[d], 0, text.chars().count() - 1, "", &w);
        assert_eq!(texts(&got), ["near_word", "far_away"]);
    }

    #[test]
    fn many_words_are_capped_and_fast() {
        let text: String = (0..20_000).map(|i| format!("word_{i} ")).collect();
        let d = doc(&text, 0);
        let t = std::time::Instant::now();
        let got = complete(&[d], 0, 0, "wd", &w);
        assert_eq!(got.len(), LIMIT);
        assert!(t.elapsed() < std::time::Duration::from_millis(if cfg!(debug_assertions) { 1500 } else { 150 }), "{:?}", t.elapsed());
    }
}
