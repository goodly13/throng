//! Undo and redo: one history per document, shared by every view of it.

use serde::{Deserialize, Serialize};

use crate::change::Transaction;
use crate::selection::Selection;

/// Entries kept. The requirement is at least 500.
pub const MAX_ENTRIES: usize = 1000;
/// Typing or deleting within this long of the previous edit, without moving the caret, extends the
/// same undo step.
pub const COALESCE_MS: u64 = 1000;

/// What kind of edit a step was, for coalescing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    /// Typing.
    Insert,
    /// Backspace or Delete.
    Delete,
    /// Anything else (paste, replace, indent, …): always its own step.
    Other,
}

/// One undo step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub tx: Transaction,
    /// The editing view's selection before and after, restored by undo and redo.
    pub before: Selection,
    pub after: Selection,
    pub kind: EditKind,
    pub time_ms: u64,
}

/// A document's undo and redo stacks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    undo: Vec<Revision>,
    redo: Vec<Revision>,
    /// The next edit starts a new step (after a save, a caret move, an undo).
    #[serde(skip)]
    sealed: bool,
}

impl History {
    /// Record an applied edit. Any redo is lost: history is linear.
    pub fn record(&mut self, revision: Revision) {
        self.redo.clear();
        let merged = !self.sealed
            && self.undo.last_mut().is_some_and(|last| {
                if !can_merge(last, &revision) {
                    return false;
                }
                last.tx.changes.extend(revision.tx.changes.iter().cloned());
                last.after = revision.after.clone();
                last.time_ms = revision.time_ms;
                true
            });
        if !merged {
            self.undo.push(revision);
        }
        self.sealed = false;
        if self.undo.len() > MAX_ENTRIES {
            let excess = self.undo.len() - MAX_ENTRIES;
            self.undo.drain(..excess);
        }
    }

    /// Make the next edit its own step.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Take the step to undo; the caller applies its inverse.
    pub fn undo(&mut self) -> Option<Revision> {
        let revision = self.undo.pop()?;
        self.redo.push(revision.clone());
        self.sealed = true;
        Some(revision)
    }

    /// Take the step to redo; the caller applies it.
    pub fn redo(&mut self) -> Option<Revision> {
        let revision = self.redo.pop()?;
        self.undo.push(revision.clone());
        self.sealed = true;
        Some(revision)
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[must_use]
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.sealed = true;
    }

    /// A copy whose serialised size stays under `max_bytes`, dropping the oldest steps first
    /// (1 MiB per document). Redo goes first, being the least likely to be wanted.
    #[must_use]
    pub fn trimmed_to(&self, max_bytes: usize) -> Self {
        let mut out = self.clone();
        let size = |r: &Revision| r.tx.weight() + 32 * (r.before.len() + r.after.len()) + 32;
        let mut total: usize = out.undo.iter().chain(&out.redo).map(size).sum();
        while total > max_bytes && !out.redo.is_empty() {
            total -= size(&out.redo.remove(0));
        }
        let mut drop = 0;
        while total > max_bytes && drop < out.undo.len() {
            total -= size(&out.undo[drop]);
            drop += 1;
        }
        out.undo.drain(..drop);
        out
    }
}

fn can_merge(last: &Revision, next: &Revision) -> bool {
    if last.kind != next.kind || last.kind == EditKind::Other {
        return false;
    }
    if next.time_ms.saturating_sub(last.time_ms) > COALESCE_MS {
        return false;
    }
    // The caret must not have moved in between, and a new line starts a new step.
    let same_place = last.after.len() == next.before.len()
        && last.after.ranges().iter().zip(next.before.ranges()).all(|(a, b)| a.same_span(b));
    let new_line = next.tx.changes.iter().any(|c| c.inserted.contains('\n'));
    same_place && !new_line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::Change;

    fn typed(at: usize, text: &str, time_ms: u64) -> Revision {
        Revision {
            tx: Transaction { changes: vec![Change { at, removed: String::new(), inserted: text.into() }] },
            before: Selection::point(at),
            after: Selection::point(at + text.chars().count()),
            kind: EditKind::Insert,
            time_ms,
        }
    }

    #[test]
    fn typing_in_one_place_is_one_step() {
        let mut history = History::default();
        history.record(typed(0, "a", 0));
        history.record(typed(1, "b", 100));
        history.record(typed(2, "c", 200));
        assert_eq!(history.undo_len(), 1);
    }

    #[test]
    fn a_pause_a_move_or_a_seal_starts_a_new_step() {
        let mut history = History::default();
        history.record(typed(0, "a", 0));
        history.record(typed(1, "b", 5_000));
        assert_eq!(history.undo_len(), 2, "pause");
        history.record(typed(10, "c", 5_100));
        assert_eq!(history.undo_len(), 3, "caret moved");
        history.seal();
        history.record(typed(11, "d", 5_200));
        assert_eq!(history.undo_len(), 4, "sealed");
    }

    #[test]
    fn a_new_edit_drops_redo() {
        let mut history = History::default();
        history.record(typed(0, "a", 0));
        history.undo();
        assert!(history.can_redo());
        history.record(typed(0, "b", 10));
        assert!(!history.can_redo());
    }

    #[test]
    fn the_saved_history_fits_its_budget_by_dropping_the_oldest() {
        let mut history = History::default();
        for i in 0..100 {
            history.seal();
            history.record(typed(i, &"x".repeat(100), i as u64));
        }
        let trimmed = history.trimmed_to(2_000);
        assert!(trimmed.undo_len() < 100 && trimmed.undo_len() > 0);
        assert_eq!(trimmed.undo.last(), history.undo.last(), "the newest step survives");
    }
}
