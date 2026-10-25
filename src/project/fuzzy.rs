//! A small fuzzy matcher for file paths (fzf-like): the query's characters must appear in order,
//! case-insensitively. Matches at word starts (after `/ _ - .`, camelCase humps), runs of
//! consecutive characters and matches inside the file name score higher; gaps cost a little.

const BOUNDARY: i32 = 8;
const AFTER_SLASH: i32 = 10;
const CONSECUTIVE: i32 = 6;
const IN_FILENAME: i32 = 3;
const MATCH: i32 = 16;
const GAP_START: i32 = 3;
const GAP_EXTEND: i32 = 1;
/// Longer candidates are cut off here (they still match, just on their first part).
const MAX_LEN: usize = 512;

/// Scores `candidate` against `query`. Returns the score (higher is better) and the char indices
/// of the matched characters, or `None` when the query isn't a subsequence. An empty query
/// matches everything with score 0.
pub fn score(query: &str, candidate: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let orig: Vec<char> = candidate.chars().take(MAX_LEN).collect();
    let low: Vec<char> = orig.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let (n, m) = (orig.len(), q.len());
    if m > n || !is_subsequence(&q, &low) {
        return None;
    }
    let name_start = orig.iter().rposition(|&c| c == '/' || c == '\\').map_or(0, |i| i + 1);
    let bonus: Vec<i32> = (0..n)
        .map(|j| {
            let prev = if j == 0 { '/' } else { orig[j - 1] };
            let b = if prev == '/' || prev == '\\' {
                AFTER_SLASH
            } else if matches!(prev, '_' | '-' | '.' | ' ') || (prev.is_lowercase() && orig[j].is_uppercase()) {
                BOUNDARY
            } else if !prev.is_alphanumeric() && orig[j].is_alphanumeric() {
                BOUNDARY / 2
            } else {
                0
            };
            b + if j >= name_start { IN_FILENAME } else { 0 }
        })
        .collect();

    // dp[i][j]: best score with q[i] matched at position j; from[i][j]: where q[i-1] was matched.
    const NONE: i32 = i32::MIN / 2;
    let mut dp = vec![NONE; m * n];
    let mut from = vec![usize::MAX; m * n];
    for j in 0..n {
        if low[j] == q[0] {
            // A gap before the first match costs a little, so earlier matches win ties.
            dp[j] = MATCH + bonus[j] - (j.min(20) as i32) / 4;
        }
    }
    for i in 1..m {
        // Running best of dp[i-1][k] - gap(k, j) over k < j - 1.
        let mut best = NONE;
        let mut best_k = usize::MAX;
        for j in i..n {
            // Extend the running best by one more skipped char, then consider k = j - 2.
            if best > NONE {
                best -= GAP_EXTEND;
            }
            if j >= 2 {
                let k = j - 2;
                let v = dp[(i - 1) * n + k];
                if v > NONE && v - GAP_START > best {
                    best = v - GAP_START;
                    best_k = k;
                }
            }
            if low[j] != q[i] {
                continue;
            }
            let here = MATCH + bonus[j];
            let adj = dp[(i - 1) * n + j - 1];
            let (mut v, mut k) = (NONE, usize::MAX);
            if adj > NONE {
                v = adj + here + CONSECUTIVE;
                k = j - 1;
            }
            if best > NONE && best + here > v {
                v = best + here;
                k = best_k;
            }
            dp[i * n + j] = v;
            from[i * n + j] = k;
        }
    }
    let row = (m - 1) * n;
    let (mut j, best) = (0..n).map(|j| (j, dp[row + j])).max_by_key(|&(j, v)| (v, usize::MAX - j))?;
    if best <= NONE {
        return None;
    }
    let mut pos = vec![0; m];
    for i in (0..m).rev() {
        pos[i] = j;
        if i > 0 {
            j = from[i * n + j];
        }
    }
    // Shorter paths win ties.
    Some((best - (n as i32) / 8, pos))
}

fn is_subsequence(q: &[char], s: &[char]) -> bool {
    let mut it = s.iter();
    q.iter().all(|c| it.any(|x| x == c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rank<'a>(q: &str, items: &[&'a str]) -> Vec<&'a str> {
        let mut v: Vec<(i32, &str)> = items.iter().filter_map(|s| score(q, s).map(|(sc, _)| (sc, *s))).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn subsequence_required() {
        assert!(score("abc", "a/b/c").is_some());
        assert!(score("acb", "abc").is_none());
        assert!(score("xyz", "src/app.rs").is_none());
        assert!(score("", "anything").is_some());
    }

    #[test]
    fn case_insensitive_and_spaces_ignored() {
        assert!(score("APP", "src/app.rs").is_some());
        assert!(score("src app", "src/app.rs").is_some());
    }

    #[test]
    fn positions_are_the_matched_chars() {
        let (_, pos) = score("app", "src/app.rs").unwrap();
        assert_eq!(pos, vec![4, 5, 6]);
        // Prefers the word start / file name over the first occurrence.
        let (_, pos) = score("ui", "build/quick/ui.rs").unwrap();
        assert_eq!(pos, vec![12, 13]);
    }

    #[test]
    fn file_name_beats_directory() {
        let r = rank("doc", &["docs/ROADMAP.md", "src/doc.rs"]);
        assert_eq!(r[0], "src/doc.rs");
    }

    #[test]
    fn word_starts_and_runs_win() {
        let r = rank("pm", &["src/upsample.rs", "src/project/mod.rs"]);
        assert_eq!(r[0], "src/project/mod.rs");
        let r = rank("main", &["src/m_a_i_n.rs", "src/main.rs"]);
        assert_eq!(r[0], "src/main.rs");
        // camelCase humps
        let r = rank("fb", &["src/FooBar.java", "src/fabric.java"]);
        assert_eq!(r[0], "src/FooBar.java");
    }

    #[test]
    fn shorter_path_wins_ties() {
        let r = rank("app.rs", &["very/deep/nested/dir/app.rs", "src/app.rs"]);
        assert_eq!(r[0], "src/app.rs");
    }
}
