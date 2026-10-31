//! Go Back / Go Forward (⌃⌘← / ⌃⌘→): the tabs visited in a workspace, each with the view it
//! showed (a table's filters, sort and page), so a step back restores both.

#[derive(Debug, Clone)]
pub struct History<V> {
    entries: Vec<(u64, V)>,
    pos: usize,
}
impl<V: Clone + PartialEq> History<V> {
    pub fn new(tab: u64, view: V) -> Self {
        Self {
            entries: vec![(tab, view)],
            pos: 0,
        }
    }
    pub fn current(&self) -> (u64, &V) {
        let (tab, view) = &self.entries[self.pos];
        (*tab, view)
    }
    /// Records how the current entry looks now, before it is left: a step back returns to this.
    pub fn update_current(&mut self, view: V) {
        self.entries[self.pos].1 = view;
    }
    /// A new place: anything ahead of the current entry is dropped. Visiting the current place
    /// again adds nothing.
    pub fn visit(&mut self, tab: u64, view: V) {
        if self.entries[self.pos] == (tab, view.clone()) {
            return;
        }
        self.entries.truncate(self.pos + 1);
        self.entries.push((tab, view));
        self.pos = self.entries.len() - 1;
    }
    pub fn can_back(&self) -> bool {
        self.pos > 0
    }
    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.entries.len()
    }
    pub fn back(&mut self) -> Option<(u64, V)> {
        self.can_back().then(|| {
            self.pos -= 1;
            self.entries[self.pos].clone()
        })
    }
    pub fn forward(&mut self) -> Option<(u64, V)> {
        self.can_forward().then(|| {
            self.pos += 1;
            self.entries[self.pos].clone()
        })
    }
    /// Forgets a closed tab. `fallback` is the place to stand on when nothing else is left.
    pub fn remove_tab(&mut self, tab: u64, fallback: (u64, V)) {
        let current = self.entries[self.pos].clone();
        let mut kept: Vec<(u64, V)> = vec![];
        let mut pos = 0;
        for (i, entry) in self.entries.drain(..).enumerate() {
            if entry.0 == tab {
                continue;
            }
            if kept.last() == Some(&entry) {
                continue;
            }
            if i <= self.pos {
                pos = kept.len();
            }
            kept.push(entry);
        }
        if kept.is_empty() {
            kept.push(fallback);
            pos = 0;
        }
        if current.0 != tab {
            pos = kept.iter().rposition(|e| *e == current).unwrap_or(pos);
        }
        self.entries = kept;
        self.pos = pos.min(self.entries.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn back_and_forward_restore_tabs_and_views() {
        let mut h = History::new(1, "a");
        h.visit(2, "x");
        h.update_current("x2");
        h.visit(2, "y");
        assert!(h.can_back() && !h.can_forward());
        assert_eq!(h.back(), Some((2, "x2")));
        assert_eq!(h.back(), Some((1, "a")));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some((2, "x2")));
        h.visit(3, "z");
        assert!(!h.can_forward());
        assert_eq!(h.current(), (3, &"z"));
    }
    #[test]
    fn revisiting_the_same_place_adds_nothing() {
        let mut h = History::new(1, "a");
        h.visit(1, "a");
        assert!(!h.can_back());
        h.visit(1, "b");
        assert!(h.can_back());
    }
    #[test]
    fn closed_tabs_are_forgotten_without_leaving_a_dangling_position() {
        let mut h = History::new(1, "a");
        h.visit(2, "b");
        h.visit(1, "a");
        h.visit(3, "c");
        h.remove_tab(2, (9, "f"));
        assert_eq!(h.current(), (3, &"c"));
        assert_eq!(h.back(), Some((1, "a")));
        assert_eq!(h.back(), None);
        h.remove_tab(1, (9, "f"));
        assert_eq!(h.current(), (3, &"c"));
        h.remove_tab(3, (9, "f"));
        assert_eq!(h.current(), (9, &"f"));
        assert!(!h.can_back() && !h.can_forward());
    }
    #[test]
    fn removing_the_current_tab_stands_on_the_previous_entry() {
        let mut h = History::new(1, "a");
        h.visit(2, "b");
        h.visit(3, "c");
        assert_eq!(h.back(), Some((2, "b")));
        h.remove_tab(2, (9, "f"));
        assert_eq!(h.current(), (1, &"a"));
        assert_eq!(h.forward(), Some((3, "c")));
    }
}
