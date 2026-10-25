//! Turning a run's entries into wrapped, styled lines for the panel. Tool calls take one line
//! (`⏺ Edit src/app.rs`), text is word-wrapped with a hanging indent.

use ratatui::style::{Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::run::{fmt_ms, Entry, ToolStatus};
use crate::ui::theme::*;

pub type Span = (String, Style);

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.0.as_str()).collect()
    }
}

fn line(spans: Vec<Span>) -> Line {
    Line { spans }
}

/// Word-wraps `text` to `width` columns (long words are cut); keeps blank lines.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.trim_end();
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        // Keep the leading indentation of the line (code, lists) on its first row.
        let indent = raw.chars().take_while(|c| *c == ' ').count();
        let indent = if indent < width / 2 { indent } else { 0 };
        let mut cur = " ".repeat(indent);
        let mut cur_w = indent;
        let mut has_word = false;
        for word in raw.split(' ').filter(|w| !w.is_empty()) {
            let ww = word.width();
            let sep = usize::from(has_word);
            if cur_w + sep + ww <= width {
                if has_word {
                    cur.push(' ');
                    cur_w += 1;
                }
                cur.push_str(word);
                cur_w += ww;
                has_word = true;
                continue;
            }
            if has_word {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            // The word alone: cut it if it's wider than a line.
            for ch in word.chars() {
                let w = ch.width().unwrap_or(0);
                if cur_w + w > width {
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += w;
            }
            has_word = true;
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// Cuts `s` to `width` columns with a trailing `…`.
pub fn ellipsize(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// A first line with `prefix`, continuation lines indented by its width.
fn hanging(out: &mut Vec<Line>, prefix: &str, pre_style: Style, text: &str, style: Style, width: usize) {
    let pw = prefix.width();
    let rows = wrap(text, width.saturating_sub(pw).max(8));
    for (i, r) in rows.into_iter().enumerate() {
        let p = if i == 0 { prefix.to_string() } else { " ".repeat(pw) };
        out.push(line(vec![(p, pre_style), (r, style)]));
    }
}

/// Renders the transcript at `width` columns. `spin` is the spinner frame for a running tool.
pub fn render(entries: &[Entry], width: usize, spin: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let muted = Style::new().fg(MUTED);
    for (i, e) in entries.iter().enumerate() {
        match e {
            Entry::User { text, ctx } => {
                if i > 0 {
                    out.push(line(vec![]));
                }
                hanging(&mut out, "› ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD), text, Style::new().fg(TEXT).add_modifier(Modifier::BOLD), width);
                if let Some(c) = ctx {
                    out.push(line(vec![("  + ".into(), muted), (ellipsize(c, width.saturating_sub(4)), muted)]));
                }
            }
            Entry::Text { text } => hanging(&mut out, "⏺ ", Style::new().fg(TEXT), text, Style::new().fg(TEXT), width),
            Entry::Tool { name, arg, status, note, .. } => {
                let (dot, c) = match status {
                    ToolStatus::Pending => (spin, WARNING),
                    ToolStatus::Ok => ("⏺", GOOD),
                    ToolStatus::Failed => ("⏺", CRITICAL),
                };
                let head = format!("{name} ");
                let room = width.saturating_sub(2 + head.width());
                out.push(line(vec![
                    (format!("{dot} "), Style::new().fg(c)),
                    (head, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                    (ellipsize(arg, room), Style::new().fg(TEXT_2)),
                ]));
                if *status == ToolStatus::Failed && !note.is_empty() {
                    out.push(line(vec![("  ⎿ ".into(), muted), (ellipsize(note, width.saturating_sub(4)), Style::new().fg(CRITICAL))]));
                }
            }
            Entry::Info { text } => hanging(&mut out, "  ", muted, text, muted, width),
            Entry::Done { ok, subtype, ms, cost, turns, denials } => {
                let mut t = if *ok { format!("✓ done in {}", fmt_ms(*ms)) } else { format!("✗ {subtype} after {}", fmt_ms(*ms)) };
                t.push_str(&format!(" · ${cost:.2} · {turns} turn{}", if *turns == 1 { "" } else { "s" }));
                let mut spans = vec![(t, Style::new().fg(if *ok { GOOD } else { CRITICAL }))];
                if *denials > 0 {
                    spans.push((format!(" · {denials} denied"), Style::new().fg(WARNING)));
                }
                let l = line(spans);
                if l.text().width() > width {
                    hanging(&mut out, "", Style::new(), &l.text(), Style::new().fg(if *ok { GOOD } else { CRITICAL }), width);
                } else {
                    out.push(l);
                }
            }
            Entry::Error { text } => hanging(&mut out, "✗ ", Style::new().fg(CRITICAL), text, Style::new().fg(CRITICAL), width),
            Entry::Stopped => out.push(line(vec![("■ stopped".into(), Style::new().fg(WARNING))])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::run::Run;
    use std::path::PathBuf;

    #[test]
    fn wrapping() {
        assert_eq!(wrap("the quick brown fox", 9), ["the quick", "brown fox"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("  - item one two", 10), ["  - item", "one two"]);
        assert_eq!(wrap("日本語のテキスト", 6), ["日本語", "のテキ", "スト"]);
        for l in wrap("some words and averyveryverylongwordhere too", 12) {
            assert!(l.width() <= 12, "{l:?}");
        }
    }

    #[test]
    fn ellipsis() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("src/agents/transcript.rs", 10), "src/agent…");
        assert!(ellipsize("src/agents/transcript.rs", 10).width() <= 10);
    }

    #[test]
    fn recorded_run_renders_as_a_readable_transcript() {
        let mut r = Run::new(1, PathBuf::from("/tmp/scratch"), "create hello.txt containing hi");
        r.begin_turn(&crate::agents::run::Turn {
            prompt: "create hello.txt containing hi".into(),
            shown: "create hello.txt containing hi".into(),
            ctx: Some("src/a.rs:3-4".into()),
            mode: Some("acceptEdits".into()),
            agent: None,
        });
        for l in include_str!("testdata/write-run.jsonl").lines() {
            r.feed_line(l);
        }
        let lines: Vec<String> = render(&r.entries, 40, "⠋").iter().map(Line::text).collect();
        assert_eq!(
            lines,
            [
                "› create hello.txt containing hi",
                "  + src/a.rs:3-4",
                "  mode acceptEdits · claude-opus-5-5 ·",
                "  session b0eef9d3",
                "⏺ Write hello.txt",
                "⏺ I created `hello.txt` containing \"hi\".",
                "  Done.",
                "✓ done in 7.4s · $0.11 · 2 turns",
            ]
        );
        for l in render(&r.entries, 24, "⠋") {
            assert!(l.text().width() <= 24, "{:?}", l.text());
        }
    }

    #[test]
    fn failed_tools_show_their_reason_and_turns_are_spaced() {
        let entries = vec![
            Entry::User { text: "a".into(), ctx: None },
            Entry::Tool { id: "1".into(), name: "Write".into(), arg: "x.txt".into(), status: ToolStatus::Failed, note: "permission denied".into() },
            Entry::Tool { id: "2".into(), name: "Bash".into(), arg: "sleep 30".into(), status: ToolStatus::Pending, note: String::new() },
            Entry::Stopped,
            Entry::User { text: "b".into(), ctx: None },
        ];
        let lines: Vec<String> = render(&entries, 40, "⠙").iter().map(Line::text).collect();
        assert_eq!(lines, ["› a", "⏺ Write x.txt", "  ⎿ permission denied", "⠙ Bash sleep 30", "■ stopped", "", "› b"]);
    }
}
