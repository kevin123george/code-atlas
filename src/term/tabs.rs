//! Several shells in the dock (decision 0018): tabs, and each tab one or more panes side by side.
//! Generic over the shell so the rules are tested without starting processes.

use ratatui::layout::Rect;

/// Narrowest useful pane: a split that would go below this is refused.
pub const MIN_PANE_W: u16 = 20;

struct Group<T> {
    panes: Vec<T>,
    active: usize,
}

pub struct Tabs<T> {
    groups: Vec<Group<T>>,
    active: usize,
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        Tabs { groups: Vec::new(), active: 0 }
    }
}

impl<T> Tabs<T> {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub fn tab_count(&self) -> usize {
        self.groups.len()
    }

    pub fn pane_count(&self) -> usize {
        self.groups.iter().map(|g| g.panes.len()).sum()
    }

    pub fn active_tab(&self) -> usize {
        self.active
    }

    pub fn active_pane(&self) -> usize {
        self.groups.get(self.active).map_or(0, |g| g.active)
    }

    pub fn current(&self) -> Option<&T> {
        let g = self.groups.get(self.active)?;
        g.panes.get(g.active)
    }

    pub fn current_mut(&mut self) -> Option<&mut T> {
        let g = self.groups.get_mut(self.active)?;
        g.panes.get_mut(g.active)
    }

    /// The panes of the active tab.
    pub fn panes_mut(&mut self) -> &mut [T] {
        self.groups.get_mut(self.active).map_or(&mut [], |g| &mut g.panes)
    }

    pub fn tab_panes(&self, tab: usize) -> &[T] {
        self.groups.get(tab).map_or(&[], |g| &g.panes)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.groups.iter_mut().flat_map(|g| g.panes.iter_mut())
    }

    /// A new tab right after the active one, which it becomes.
    pub fn new_tab(&mut self, shell: T) {
        let at = if self.groups.is_empty() { 0 } else { self.active + 1 };
        self.groups.insert(at, Group { panes: vec![shell], active: 0 });
        self.active = at;
    }

    /// A new pane right of the active one, which it becomes.
    pub fn split(&mut self, shell: T) {
        match self.groups.get_mut(self.active) {
            Some(g) => {
                g.active += 1;
                g.panes.insert(g.active, shell);
            }
            None => self.new_tab(shell),
        }
    }

    pub fn select_tab(&mut self, tab: usize) {
        if tab < self.groups.len() {
            self.active = tab;
        }
    }

    pub fn select_pane(&mut self, pane: usize) {
        if let Some(g) = self.groups.get_mut(self.active) {
            if pane < g.panes.len() {
                g.active = pane;
            }
        }
    }

    /// The next or previous shell: through the panes of a tab, then on to the next tab, wrapping.
    pub fn step(&mut self, forward: bool) {
        let Some(g) = self.groups.get(self.active) else { return };
        if forward && g.active + 1 < g.panes.len() {
            self.groups[self.active].active += 1;
        } else if !forward && g.active > 0 {
            self.groups[self.active].active -= 1;
        } else {
            let n = self.groups.len();
            self.active = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
            let g = &mut self.groups[self.active];
            g.active = if forward { 0 } else { g.panes.len() - 1 };
        }
    }

    /// Removes the active shell. Its neighbour takes over; an emptied tab goes too.
    pub fn close_current(&mut self) -> Option<T> {
        let g = self.groups.get_mut(self.active)?;
        let shell = g.panes.remove(g.active);
        g.active = g.active.min(g.panes.len().saturating_sub(1));
        if g.panes.is_empty() {
            self.groups.remove(self.active);
            self.active = self.active.min(self.groups.len().saturating_sub(1));
        }
        Some(shell)
    }

    /// Removes shells matching `dead`, always leaving one so its message stays readable. Returns how many.
    pub fn remove_dead(&mut self, dead: impl Fn(&T) -> bool) -> usize {
        let mut removed = 0;
        while self.pane_count() > 1 {
            // Current shell first: focus only moves when the shell you look at has gone.
            let found = if self.current().is_some_and(&dead) {
                Some((self.active, self.active_pane()))
            } else {
                self.groups.iter().enumerate().find_map(|(t, g)| g.panes.iter().position(&dead).map(|p| (t, p)))
            };
            let Some((tab, pane)) = found else { break };
            let (keep_tab, keep_pane) = (self.active, self.active_pane());
            self.active = tab;
            self.groups[tab].active = pane;
            self.close_current();
            if tab != keep_tab {
                // Someone else's tab closed: stay on the shell in use.
                self.active = if tab < keep_tab { keep_tab - 1 } else { keep_tab };
            } else if pane != keep_pane {
                self.groups[tab].active = if pane < keep_pane { keep_pane - 1 } else { keep_pane };
            }
            removed += 1;
        }
        removed
    }
}

/// `n` side-by-side panes of equal width with one divider column between them.
pub fn pane_rects(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let n16 = n as u16;
    let free = area.width.saturating_sub(n16 - 1);
    let (w, extra) = (free / n16, free % n16);
    let mut x = area.x;
    (0..n16)
        .map(|i| {
            let width = w + u16::from(i < extra);
            let r = Rect::new(x, area.y, width, area.height);
            x += width + 1;
            r
        })
        .collect()
}

/// Whether one more pane still leaves every pane usable.
pub fn can_split(width: u16, panes: usize) -> bool {
    let n = panes as u16 + 1;
    width.saturating_sub(n - 1) / n >= MIN_PANE_W
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(t: &mut Tabs<i32>) -> Vec<i32> {
        let mut seen = Vec::new();
        for _ in 0..t.pane_count() {
            seen.push(*t.current().unwrap());
            t.step(true);
        }
        seen
    }

    #[test]
    fn tabs_and_splits_insert_next_to_the_active_one() {
        let mut t = Tabs::default();
        assert!(t.is_empty() && t.current().is_none());
        t.new_tab(1);
        t.new_tab(2);
        t.select_tab(0);
        t.new_tab(3);
        assert_eq!(t.active_tab(), 1);
        t.split(4);
        t.select_pane(0);
        t.split(5);
        assert_eq!((t.tab_count(), t.pane_count()), (3, 5));
        t.select_tab(0);
        assert_eq!(order(&mut t), [1, 3, 5, 4, 2]);
    }

    #[test]
    fn stepping_crosses_tabs_and_wraps_both_ways() {
        let mut t = Tabs::default();
        t.new_tab(1);
        t.split(2);
        t.new_tab(3);
        assert_eq!(*t.current().unwrap(), 3);
        t.step(true);
        assert_eq!(*t.current().unwrap(), 1);
        t.step(false);
        assert_eq!(*t.current().unwrap(), 3);
        t.step(false);
        assert_eq!(*t.current().unwrap(), 2, "backwards enters a tab at its last pane");
        let mut one = Tabs::default();
        one.new_tab(9);
        one.step(true);
        one.step(false);
        assert_eq!(*one.current().unwrap(), 9);
    }

    #[test]
    fn closing_moves_to_a_neighbour_and_empties_cleanly() {
        let mut t = Tabs::default();
        t.new_tab(1);
        t.split(2);
        t.new_tab(3);
        assert_eq!(t.close_current(), Some(3));
        assert_eq!(*t.current().unwrap(), 2);
        assert_eq!(t.close_current(), Some(2));
        assert_eq!(*t.current().unwrap(), 1);
        assert_eq!(t.close_current(), Some(1));
        assert!(t.is_empty());
        assert_eq!(t.close_current(), None);
    }

    #[test]
    fn dead_shells_go_but_the_last_one_stays_and_focus_holds() {
        let mut t = Tabs::default();
        t.new_tab(-1);
        t.new_tab(2);
        t.split(-3);
        t.split(4);
        t.new_tab(5);
        t.select_tab(1);
        t.select_pane(2);
        assert_eq!(t.remove_dead(|x| *x < 0), 2);
        assert_eq!(*t.current().unwrap(), 4, "still on the shell in use");
        assert_eq!(t.pane_count(), 3);
        let mut all = Tabs::default();
        all.new_tab(-1);
        all.split(-2);
        assert_eq!(all.remove_dead(|x| *x < 0), 1);
        assert_eq!(all.pane_count(), 1, "the last one stays, with its exit message");
    }

    #[test]
    fn panes_share_the_width() {
        let r = pane_rects(Rect::new(2, 5, 100, 10), 3);
        assert_eq!(r.iter().map(|p| p.width).collect::<Vec<_>>(), [33, 33, 32]);
        assert_eq!(r[0].x, 2);
        assert_eq!(r[1].x, 36, "one divider column between panes");
        assert_eq!(r[2].right(), 102, "the last pane ends at the edge");
        assert_eq!(pane_rects(Rect::new(0, 0, 80, 5), 1), [Rect::new(0, 0, 80, 5)]);
        assert!(pane_rects(Rect::new(0, 0, 80, 5), 0).is_empty());
        assert!(can_split(41, 1));
        assert!(!can_split(40, 1));
        assert!(!can_split(0, 1));
    }
}
