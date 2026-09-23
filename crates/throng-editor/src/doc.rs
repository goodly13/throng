//! A document's text and history. Views (panels) hold their own selections and pass them in.

use std::cell::Cell;

use ropey::Rope;

use crate::change::Transaction;
use crate::history::{EditKind, History, Revision};
use crate::selection::Selection;

/// What an edit did, for the caller to carry other views and caches across it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub tx: Transaction,
    /// Lines `first_line..=last_line` (numbered after the edit) are the only ones whose text may
    /// differ; lines before are untouched, lines after are untouched but shifted by `line_delta`.
    pub first_line: usize,
    pub last_line: usize,
    pub line_delta: isize,
    /// The document version this edit produced.
    pub version: u64,
}

/// Text plus undo history. Line endings inside are always `\n`; the file's own endings are
/// restored on save by the codec in `throng-core`.
#[derive(Debug)]
pub struct TextDoc {
    rope: Rope,
    history: History,
    version: u64,
    /// The text as last loaded or saved. Cloning a rope shares its storage, so keeping it is cheap.
    saved: Rope,
    dirty: Cell<Option<(u64, bool)>>,
}

impl Default for TextDoc {
    fn default() -> Self {
        Self::new("")
    }
}

impl TextDoc {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let rope = Rope::from_str(text);
        Self { saved: rope.clone(), rope, history: History::default(), version: 0, dirty: Cell::new(None) }
    }

    #[must_use]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    #[must_use]
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Increases with every change to the text.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Replace the history (restoring it after a crash).
    pub fn set_history(&mut self, history: History) {
        self.history = history;
        self.history.seal();
    }

    /// Whether the text differs from what was last loaded or saved. Undoing back to the saved text
    /// is clean (a comparison, not a counter); the comparison runs once per change.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        if let Some((version, dirty)) = self.dirty.get()
            && version == self.version
        {
            return dirty;
        }
        let dirty = self.rope.len_chars() != self.saved.len_chars() || self.rope != self.saved;
        self.dirty.set(Some((self.version, dirty)));
        dirty
    }

    /// The text is now what the disk holds.
    pub fn mark_saved(&mut self, text: &Rope) {
        self.saved = text.clone();
        self.dirty.set(None);
        self.history.seal();
    }

    /// The text that was last loaded or saved.
    #[must_use]
    pub fn saved(&self) -> &Rope {
        &self.saved
    }

    /// Replace everything with `text` from disk: history cleared, clean.
    pub fn reset(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.saved = self.rope.clone();
        self.history.clear();
        self.version += 1;
        self.dirty.set(None);
    }

    /// Replace the text but keep it dirty against the disk copy (recovering unsaved edits).
    pub fn restore_unsaved(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.version += 1;
        self.dirty.set(None);
    }

    /// Apply an edit made by a view whose selection was `before` and becomes `after`.
    pub fn edit(
        &mut self,
        tx: Transaction,
        before: Selection,
        after: Selection,
        kind: EditKind,
        now_ms: u64,
    ) -> Option<Applied> {
        if tx.is_empty() {
            return None;
        }
        let applied = self.apply(&tx);
        self.history.record(Revision { tx, before, after, kind, time_ms: now_ms });
        Some(applied)
    }

    /// Undo one step; returns what changed and the selection to put back.
    pub fn undo(&mut self) -> Option<(Applied, Selection)> {
        let revision = self.history.undo()?;
        let applied = self.apply(&revision.tx.invert());
        Some((applied, revision.before.clamp(self.len_chars())))
    }

    /// Redo one step.
    pub fn redo(&mut self) -> Option<(Applied, Selection)> {
        let revision = self.history.redo()?;
        let applied = self.apply(&revision.tx);
        Some((applied, revision.after.clamp(self.len_chars())))
    }

    /// Start a new undo step with the next edit (the caret moved, a save happened).
    pub fn seal(&mut self) {
        self.history.seal();
    }

    fn apply(&mut self, tx: &Transaction) -> Applied {
        // Track the touched line span through each change in turn (a change can shift the lines
        // an earlier one touched).
        let mut span: Option<(usize, usize)> = None;
        let lines_before = self.rope.len_lines();
        for change in &tx.changes {
            let removed = change.removed.chars().count();
            let inserted = change.inserted.chars().count();
            let start = self.rope.char_to_line(change.at);
            let end_old = self.rope.char_to_line(change.at + removed);
            let one = crate::change::Transaction { changes: vec![change.clone()] };
            one.apply(&mut self.rope);
            let end_new = self.rope.char_to_line(change.at + inserted);
            let delta = end_new as isize - end_old as isize;
            let shift = |line: usize, high: bool| {
                if line < start {
                    line
                } else if line > end_old {
                    (line as isize + delta) as usize
                } else if high {
                    end_new
                } else {
                    start
                }
            };
            span = Some(match span {
                None => (start, end_new),
                Some((lo, hi)) => (shift(lo, false).min(start), shift(hi, true).max(end_new)),
            });
        }
        self.version += 1;
        let (first_line, last_line) = span.unwrap_or((0, 0));
        Applied {
            tx: tx.clone(),
            first_line,
            last_line,
            line_delta: self.rope.len_lines() as isize - lines_before as isize,
            version: self.version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_back_to_the_saved_text_is_clean_and_redo_is_dirty_again() {
        let mut doc = TextDoc::new("abc");
        let tx = Transaction::replace(doc.rope(), vec![(3, 3, "d".into())]);
        doc.edit(tx, Selection::point(3), Selection::point(4), EditKind::Insert, 0);
        assert!(doc.is_dirty());
        let (_, selection) = doc.undo().unwrap();
        assert_eq!(selection, Selection::point(3));
        assert!(!doc.is_dirty());
        doc.redo();
        assert_eq!(doc.text(), "abcd");
        assert!(doc.is_dirty());
    }

    #[test]
    fn edits_report_the_lines_they_touched() {
        let mut doc = TextDoc::new("a\nb\nc\nd\n");
        // Replace line 1 ("b") with two lines, and edit line 3 ("d").
        let tx = Transaction::replace(doc.rope(), vec![(2, 3, "x\ny".into()), (6, 7, "D".into())]);
        let applied = doc.edit(tx, Selection::point(0), Selection::point(0), EditKind::Other, 0).unwrap();
        assert_eq!(doc.text(), "a\nx\ny\nc\nD\n");
        assert_eq!((applied.first_line, applied.last_line, applied.line_delta), (1, 4, 1));
    }

    #[test]
    fn undo_survives_a_save_and_undoing_past_it_is_dirty() {
        let mut doc = TextDoc::new("");
        let tx = Transaction::replace(doc.rope(), vec![(0, 0, "x".into())]);
        doc.edit(tx, Selection::point(0), Selection::point(1), EditKind::Insert, 0);
        let rope = doc.rope().clone();
        doc.mark_saved(&rope);
        assert!(!doc.is_dirty());
        doc.undo();
        assert!(doc.is_dirty());
    }
}
