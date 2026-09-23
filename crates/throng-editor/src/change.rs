//! Edits as data: a transaction is a list of replacements that can be applied, inverted (undo) and
//! used to carry positions across it (other views' carets, find matches).

use ropey::Rope;
use serde::{Deserialize, Serialize};

/// One replacement. `at` is a character offset in the document as it stands after the previous
/// changes of the same transaction have been applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    fn removed_len(&self) -> usize {
        self.removed.chars().count()
    }

    fn inserted_len(&self) -> usize {
        self.inserted.chars().count()
    }
}

/// Which way a position at an insertion point goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assoc {
    /// Stays before inserted text.
    Before,
    /// Moves past inserted text (a caret that typed it).
    After,
}

/// An ordered list of changes applied as one step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub changes: Vec<Change>,
}

impl Transaction {
    /// Replace each `(from, to, text)` range of `rope`. The ranges are in `rope`'s coordinates and
    /// must not overlap; they may come in any order.
    #[must_use]
    pub fn replace(rope: &Rope, mut edits: Vec<(usize, usize, String)>) -> Self {
        // Applying from the end backwards keeps every earlier offset valid.
        edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let len = rope.len_chars();
        let changes = edits
            .into_iter()
            .filter_map(|(from, to, inserted)| {
                let (from, to) = (from.min(len), to.min(len));
                let removed = rope.slice(from..to).to_string();
                (removed != inserted).then_some(Change { at: from, removed, inserted })
            })
            .collect();
        Self { changes }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Apply to `rope`.
    pub fn apply(&self, rope: &mut Rope) {
        for change in &self.changes {
            let end = change.at + change.removed_len();
            rope.remove(change.at..end);
            rope.insert(change.at, &change.inserted);
        }
    }

    /// The transaction that undoes this one.
    #[must_use]
    pub fn invert(&self) -> Self {
        Self {
            changes: self
                .changes
                .iter()
                .rev()
                .map(|c| Change { at: c.at, removed: c.inserted.clone(), inserted: c.removed.clone() })
                .collect(),
        }
    }

    /// Where `pos` ends up after this transaction.
    #[must_use]
    pub fn map(&self, mut pos: usize, assoc: Assoc) -> usize {
        for change in &self.changes {
            let removed = change.removed_len();
            let inserted = change.inserted_len();
            let end = change.at + removed;
            pos = if pos < change.at {
                pos
            } else if pos > end {
                pos - removed + inserted
            } else if removed == 0 {
                // A pure insertion exactly here.
                if assoc == Assoc::After { change.at + inserted } else { change.at }
            } else if pos == change.at {
                change.at
            } else if pos == end || assoc == Assoc::After {
                // Just after the replaced span, or inside it and carried forward.
                change.at + inserted
            } else {
                change.at
            };
        }
        pos
    }

    /// The smallest character offset this transaction touches, in the coordinates of the document
    /// before it; `None` when empty. Used to invalidate caches from that point on.
    #[must_use]
    pub fn first_touched(&self) -> Option<usize> {
        // Changes are applied in order, so an earlier change can shift a later one; the minimum
        // over their positions is still a safe lower bound in the pre-transaction document because
        // a change never moves text that lies before it.
        self.changes.iter().map(|c| c.at).min()
    }

    /// Total characters inserted plus removed (a rough size, for history caps).
    #[must_use]
    pub fn weight(&self) -> usize {
        self.changes.iter().map(|c| c.removed.len() + c.inserted.len() + 16).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn replacements_apply_in_any_order_and_invert() {
        let mut doc = rope("hello world");
        let tx = Transaction::replace(&doc, vec![(0, 5, "HELLO".into()), (6, 11, "there".into())]);
        tx.apply(&mut doc);
        assert_eq!(doc.to_string(), "HELLO there");
        tx.invert().apply(&mut doc);
        assert_eq!(doc.to_string(), "hello world");
    }

    #[test]
    fn positions_follow_edits() {
        let doc = rope("abcdef");
        // Insert "XY" at 2, delete "e" (4..5).
        let tx = Transaction::replace(&doc, vec![(2, 2, "XY".into()), (4, 5, String::new())]);
        assert_eq!(tx.map(0, Assoc::After), 0);
        assert_eq!(tx.map(2, Assoc::Before), 2);
        assert_eq!(tx.map(2, Assoc::After), 4);
        assert_eq!(tx.map(3, Assoc::After), 5);
        assert_eq!(tx.map(6, Assoc::After), 7);
        // Inside the deleted "e".
        assert_eq!(tx.map(5, Assoc::Before), 6);
    }

    #[test]
    fn unchanged_replacements_are_dropped() {
        let doc = rope("same");
        assert!(Transaction::replace(&doc, vec![(0, 4, "same".into())]).is_empty());
    }
}
