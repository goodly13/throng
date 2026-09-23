//! Cursors and selections, as character offsets into the document.
//!
//! Selections belong to a view (a panel), not to the document: two panels on one file keep their
//! own carets, and an edit made in one moves the other's through the same change.

use serde::{Deserialize, Serialize};

use crate::change::{Assoc, Transaction};

/// One selection range. `anchor` stays put while `head` (the caret) moves; they are equal for a
/// bare caret. Offsets are in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
    /// The display column a run of vertical moves aims for, so passing a short line does not pull
    /// the caret left for good.
    #[serde(skip)]
    pub goal: Option<usize>,
}

impl Range {
    #[must_use]
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal: None }
    }

    #[must_use]
    pub fn point(at: usize) -> Self {
        Self::new(at, at)
    }

    /// The lower end.
    #[must_use]
    pub fn from(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The upper end.
    #[must_use]
    pub fn to(&self) -> usize {
        self.anchor.max(self.head)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.to() - self.from()
    }

    /// Same span, ignoring direction and goal.
    #[must_use]
    pub fn same_span(&self, other: &Self) -> bool {
        self.from() == other.from() && self.to() == other.to()
    }

    /// Move the caret, keeping the anchor when `extend`, collapsing otherwise.
    #[must_use]
    pub fn put_head(self, head: usize, extend: bool) -> Self {
        if extend { Self { anchor: self.anchor, head, goal: None } } else { Self::point(head) }
    }

    /// This range after `tx`: a caret sitting where text was inserted ends up after it.
    #[must_use]
    pub fn map(self, tx: &Transaction) -> Self {
        if self.is_empty() {
            let at = tx.map(self.head, Assoc::After);
            return Self { anchor: at, head: at, goal: self.goal };
        }
        // Text inserted exactly at a selection's edge stays outside it.
        let (from_assoc, to_assoc) = (Assoc::After, Assoc::Before);
        let from = tx.map(self.from(), from_assoc);
        let to = tx.map(self.to(), to_assoc).max(from);
        if self.anchor <= self.head {
            Self { anchor: from, head: to, goal: self.goal }
        } else {
            Self { anchor: to, head: from, goal: self.goal }
        }
    }
}

/// A view's selection: one or more ranges, sorted and never overlapping, one of them primary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    ranges: Vec<Range>,
    primary: usize,
}

impl Default for Selection {
    fn default() -> Self {
        Self::point(0)
    }
}

impl Selection {
    #[must_use]
    pub fn point(at: usize) -> Self {
        Self { ranges: vec![Range::point(at)], primary: 0 }
    }

    #[must_use]
    pub fn single(anchor: usize, head: usize) -> Self {
        Self { ranges: vec![Range::new(anchor, head)], primary: 0 }
    }

    /// Build from ranges in any order. Overlapping (or, for carets, coincident) ranges merge; the
    /// merged range takes the primary role if any part of it had it.
    #[must_use]
    pub fn new(ranges: Vec<Range>, primary: usize) -> Self {
        assert!(!ranges.is_empty(), "a selection has at least one range");
        let primary = primary.min(ranges.len() - 1);
        let mut indexed: Vec<(usize, Range)> = ranges.into_iter().enumerate().collect();
        indexed.sort_by_key(|(_, r)| (r.from(), r.to()));
        let mut merged: Vec<Range> = Vec::with_capacity(indexed.len());
        let mut new_primary = 0;
        for (index, range) in indexed {
            if let Some(last) = merged.last_mut() {
                let overlaps = range.from() < last.to()
                    || (range.from() == last.to() && (range.is_empty() || last.is_empty()));
                if overlaps {
                    let from = last.from().min(range.from());
                    let to = last.to().max(range.to());
                    let forward = last.anchor <= last.head;
                    *last = if forward { Range::new(from, to) } else { Range::new(to, from) };
                    if index == primary {
                        new_primary = merged.len() - 1;
                    }
                    continue;
                }
            }
            if index == primary {
                new_primary = merged.len();
            }
            merged.push(range);
        }
        Self { ranges: merged, primary: new_primary }
    }

    #[must_use]
    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    #[must_use]
    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    #[must_use]
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Every range is a bare caret.
    #[must_use]
    pub fn all_empty(&self) -> bool {
        self.ranges.iter().all(Range::is_empty)
    }

    /// Apply `f` to every range and re-normalise.
    #[must_use]
    pub fn transform(&self, mut f: impl FnMut(Range) -> Range) -> Self {
        Self::new(self.ranges.iter().map(|r| f(*r)).collect(), self.primary)
    }

    /// Add a range and make it primary.
    #[must_use]
    pub fn with(&self, range: Range) -> Self {
        let mut ranges = self.ranges.clone();
        ranges.push(range);
        let primary = ranges.len() - 1;
        Self::new(ranges, primary)
    }

    /// Only the primary range.
    #[must_use]
    pub fn only_primary(&self) -> Self {
        Self { ranges: vec![self.primary()], primary: 0 }
    }

    /// This selection after `tx`.
    #[must_use]
    pub fn map(&self, tx: &Transaction) -> Self {
        self.transform(|r| r.map(tx))
    }

    /// Clamp every offset into a document of `len` characters.
    #[must_use]
    pub fn clamp(&self, len: usize) -> Self {
        self.transform(|r| Range { anchor: r.anchor.min(len), head: r.head.min(len), goal: r.goal })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_ranges_merge_and_keep_the_primary() {
        let sel = Selection::new(vec![Range::new(10, 12), Range::new(0, 5), Range::new(4, 8)], 2);
        assert_eq!(sel.ranges(), &[Range::new(0, 8), Range::new(10, 12)]);
        assert_eq!(sel.primary(), Range::new(0, 8));
    }

    #[test]
    fn coincident_carets_merge_but_touching_selections_do_not() {
        let carets = Selection::new(vec![Range::point(3), Range::point(3)], 1);
        assert_eq!(carets.len(), 1);
        let touching = Selection::new(vec![Range::new(0, 3), Range::new(3, 6)], 0);
        assert_eq!(touching.len(), 2);
    }

    #[test]
    fn a_backward_selection_keeps_its_direction_when_merged() {
        let sel = Selection::new(vec![Range::new(8, 4), Range::new(5, 2)], 0);
        assert_eq!(sel.primary(), Range::new(8, 2));
    }
}
