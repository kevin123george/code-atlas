//! A compact commit graph, one text row per commit (like `git log --graph`, drawn with box
//! characters): each lane is two cells, the commit `●` sits in its lane, lanes that end in it
//! join with `╯`/`╰`, a merge opens a lane for its other parent with `╮`/`╭`, and `─` connects.
//! Every lane keeps the colour it was given when it opened.

/// One drawn cell: the character and a colour index (the caller picks the palette).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub color: u8,
}

/// Lanes drawn at most; wider graphs end in `…`.
pub const MAX_LANES: usize = 12;

#[derive(Default)]
pub struct Layout {
    /// The commit each lane waits for, and its colour.
    lanes: Vec<Option<(String, u8)>>,
    next_color: u8,
}

impl Layout {
    fn color(&mut self) -> u8 {
        let c = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        c
    }

    fn free_lane(&mut self, taken: &[usize]) -> usize {
        match (0..self.lanes.len()).find(|i| self.lanes[*i].is_none() && !taken.contains(i)) {
            Some(i) => i,
            None => {
                self.lanes.push(None);
                self.lanes.len() - 1
            }
        }
    }

    /// The row for the next commit (in the order `git log` printed them) and its column.
    pub fn row(&mut self, sha: &str, parents: &[String]) -> (Vec<Cell>, usize) {
        let waiting: Vec<usize> = (0..self.lanes.len()).filter(|&i| self.lanes[i].as_ref().is_some_and(|(s, _)| s == sha)).collect();
        let (col, color) = match waiting.first() {
            Some(&c) => (c, self.lanes[c].as_ref().unwrap().1),
            None => {
                let c = self.free_lane(&[]);
                let color = self.color();
                (c, color)
            }
        };
        let merging: Vec<usize> = waiting.iter().copied().filter(|&i| i != col).collect();
        // Lanes for the other parents of a merge.
        let mut taken = merging.clone();
        taken.push(col);
        let mut opened: Vec<(usize, u8, bool)> = Vec::new(); // lane, colour, joins an existing lane
        for p in parents.iter().skip(1) {
            if let Some(j) = (0..self.lanes.len()).find(|&j| j != col && !merging.contains(&j) && self.lanes[j].as_ref().is_some_and(|(s, _)| s == p)) {
                opened.push((j, self.lanes[j].as_ref().unwrap().1, true));
                continue;
            }
            let j = self.free_lane(&taken);
            taken.push(j);
            let c = self.color();
            self.lanes[j] = Some((p.clone(), c));
            opened.push((j, c, false));
        }
        let width = self.lanes.len().max(col + 1);
        let mut cells = vec![Cell { ch: ' ', color: 0 }; width * 2];
        for (i, lane) in self.lanes.iter().enumerate() {
            if let Some((_, c)) = lane {
                if !opened.iter().any(|o| o.0 == i && !o.2) {
                    cells[i * 2] = Cell { ch: '│', color: *c };
                }
            }
        }
        // Horizontal links: from the commit to each joining / opened lane.
        let link = |cells: &mut Vec<Cell>, to: usize, end: char, c: u8| {
            let (a, b) = if to > col { (col, to) } else { (to, col) };
            for x in a * 2 + 1..b * 2 {
                if cells[x].ch == ' ' {
                    cells[x] = Cell { ch: '─', color: c };
                }
            }
            cells[to * 2] = Cell { ch: end, color: c };
        };
        for &m in &merging {
            let c = self.lanes[m].as_ref().map_or(color, |l| l.1);
            link(&mut cells, m, if m > col { '╯' } else { '╰' }, c);
        }
        for &(j, c, joins) in &opened {
            let end = match (j > col, joins) {
                (true, false) => '╮',
                (false, false) => '╭',
                (true, true) => '┤',
                (false, true) => '├',
            };
            link(&mut cells, j, end, c);
        }
        cells[col * 2] = Cell { ch: '●', color };
        // Next state.
        for &m in &merging {
            self.lanes[m] = None;
        }
        self.lanes[col] = parents.first().map(|p| (p.clone(), color));
        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        while cells.len() > 1 && cells.last().is_some_and(|c| c.ch == ' ') {
            cells.pop();
        }
        if cells.len() > MAX_LANES * 2 {
            cells.truncate(MAX_LANES * 2 - 1);
            cells.push(Cell { ch: '…', color });
        }
        (cells, col)
    }
}

/// Rows for a whole list of `(sha, parents)` in log order.
pub fn layout(commits: &[(String, Vec<String>)]) -> Vec<Vec<Cell>> {
    let mut l = Layout::default();
    commits.iter().map(|(s, p)| l.row(s, p).0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Vec<Cell>]) -> Vec<String> {
        rows.iter().map(|r| r.iter().map(|c| c.ch).collect::<String>()).collect()
    }

    fn c(sha: &str, parents: &[&str]) -> (String, Vec<String>) {
        (sha.into(), parents.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn a_straight_line() {
        let rows = layout(&[c("c", &["b"]), c("b", &["a"]), c("a", &[])]);
        assert_eq!(text(&rows), ["●", "●", "●"]);
    }

    #[test]
    fn branch_and_merge() {
        // m merges b into a; both come from x.
        let rows = layout(&[c("m", &["a", "b"]), c("b", &["x"]), c("a", &["x"]), c("x", &[])]);
        assert_eq!(text(&rows), ["●─╮", "│ ●", "● │", "●─╯"]);
        // The branch lane keeps one colour from where it opens to where it joins.
        assert_eq!(rows[0][2].color, rows[1][2].color);
        assert_eq!(rows[1][2].color, rows[3][2].color);
        assert_ne!(rows[0][0].color, rows[0][2].color);
    }

    #[test]
    fn two_branch_tips_and_an_octopus() {
        // Two tips (t1, t2) that meet at x, then an octopus merge with three parents.
        let rows = layout(&[c("t1", &["x"]), c("t2", &["x"]), c("x", &["o"]), c("o", &["p", "q", "r"]), c("q", &["p"]), c("r", &["p"]), c("p", &[])]);
        assert_eq!(text(&rows), ["●", "│ ●", "●─╯", "●─╮─╮", "│ ● │", "│ │ ●", "●─╯─╯"]);
    }

    #[test]
    fn a_merge_whose_parent_already_has_a_lane() {
        // m2's second parent b is already waited for by lane 1 (from m1).
        let rows = layout(&[c("m1", &["a", "b"]), c("m2", &["c", "b"]), c("b", &["c"]), c("a", &["c"]), c("c", &[])]);
        assert_eq!(text(&rows), ["●─╮", "│ ├─●", "│ ● │", "● │ │", "●─╯─╯"]);
    }

    #[test]
    fn wide_graphs_are_cut() {
        let mut commits = vec![c("top", &(0..20).map(|i| format!("p{i}")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>())];
        commits.extend((0..20).map(|i| c(&format!("p{i}"), &[])));
        let rows = layout(&commits);
        assert_eq!(rows[0].len(), MAX_LANES * 2);
        assert_eq!(rows[0].last().unwrap().ch, '…');
    }
}
