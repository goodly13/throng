//! Undo and redo of file operations made from the tree.
//!
//! A move or rename, and a delete to the system trash, are recorded per project, newest last, at
//! most [`LIMIT`] of them. The history outlives the app, so an entry may describe a world throng
//! was not watching: whoever applies one checks the disk first and, when it no longer matches,
//! changes nothing. This module only keeps the record; it never touches the disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Entries kept per project.
pub const LIMIT: usize = 50;
const FORMAT: u32 = 1;

/// One file operation that can be reversed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum FileOp {
    /// A rename or a move: the item went from `from` to `to`.
    Move { from: PathBuf, to: PathBuf },
    /// The item at `path` went to the system trash, where `token` finds it again.
    Trash { path: PathBuf, token: String },
}

impl FileOp {
    /// What the user did, as a verb: `rename`, `move` or `delete`.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Move { from, to } if from.parent() == to.parent() => "rename",
            Self::Move { .. } => "move",
            Self::Trash { .. } => "delete",
        }
    }

    /// The item as the user knows it: where it is when the operation stands.
    #[must_use]
    pub fn item(&self) -> &Path {
        match self {
            Self::Move { to, .. } => to,
            Self::Trash { path, .. } => path,
        }
    }
}

/// A project's undo and redo stacks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHistory {
    format: u32,
    undo: Vec<FileOp>,
    redo: Vec<FileOp>,
}

impl Default for FileHistory {
    fn default() -> Self {
        Self { format: FORMAT, undo: Vec::new(), redo: Vec::new() }
    }
}

impl FileHistory {
    /// Read a stored history. Missing, unreadable or of an unknown format, it is empty: a bad
    /// record never stops a project from loading.
    #[must_use]
    pub fn parse(text: Option<&str>) -> Self {
        text.and_then(|t| serde_json::from_str::<Self>(t).ok())
            .filter(|h| h.format == FORMAT)
            .map(|mut h| {
                h.trim();
                h
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a file history serialises")
    }

    /// A new operation: it can be undone, and what was undone before it can no longer be redone.
    pub fn record(&mut self, op: FileOp) {
        self.undo.push(op);
        self.redo.clear();
        self.trim();
    }

    fn trim(&mut self) {
        let over = self.undo.len().saturating_sub(LIMIT);
        self.undo.drain(..over);
        let over = self.redo.len().saturating_sub(LIMIT);
        self.redo.drain(..over);
    }

    #[must_use]
    pub fn next_undo(&self) -> Option<&FileOp> {
        self.undo.last()
    }

    #[must_use]
    pub fn next_redo(&self) -> Option<&FileOp> {
        self.redo.last()
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The newest operation was reversed on disk: it can now be redone.
    pub fn undone(&mut self) {
        if let Some(op) = self.undo.pop() {
            self.redo.push(op);
        }
    }

    /// The newest undone operation was applied again, as `op` (a delete's trash token changes).
    /// `None`: it was applied but cannot be undone again (the trash here cannot restore).
    pub fn redone(&mut self, op: Option<FileOp>) {
        self.redo.pop();
        if let Some(op) = op {
            self.undo.push(op);
            self.trim();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(from: &str, to: &str) -> FileOp {
        FileOp::Move { from: from.into(), to: to.into() }
    }

    #[test]
    fn a_new_operation_clears_redo_and_the_oldest_drops_past_the_limit() {
        let mut h = FileHistory::default();
        h.record(mv("/p/a", "/p/b"));
        h.undone();
        assert!(h.can_redo());
        h.record(mv("/p/c", "/p/d"));
        assert!(!h.can_redo(), "a new operation ends the redo branch");
        for i in 0..LIMIT + 5 {
            h.record(mv(&format!("/p/{i}"), &format!("/p/x{i}")));
        }
        assert_eq!(h.undo.len(), LIMIT);
        assert_eq!(h.undo[0], mv("/p/5", "/p/x5"), "the oldest went first");
    }

    #[test]
    fn undo_and_redo_move_entries_between_the_stacks() {
        let mut h = FileHistory::default();
        h.record(mv("/p/a", "/p/b"));
        h.record(FileOp::Trash { path: "/p/c".into(), token: "t1".into() });
        assert_eq!(h.next_undo().map(FileOp::verb), Some("delete"));
        h.undone();
        assert_eq!(h.next_undo().map(FileOp::verb), Some("rename"));
        h.redone(Some(FileOp::Trash { path: "/p/c".into(), token: "t2".into() }));
        assert_eq!(h.next_undo(), Some(&FileOp::Trash { path: "/p/c".into(), token: "t2".into() }));
        h.undone();
        h.redone(None);
        assert!(!h.can_redo());
        assert_eq!(h.next_undo().map(FileOp::verb), Some("rename"), "a delete that cannot be restored");
    }

    #[test]
    fn a_stored_history_round_trips_and_a_bad_one_is_empty() {
        let mut h = FileHistory::default();
        h.record(mv("/p/a", "/p/q/a"));
        assert_eq!(FileHistory::parse(Some(&h.to_json())), h);
        assert_eq!(h.next_undo().map(FileOp::verb), Some("move"));
        for bad in [None, Some("not json"), Some(r#"{"format":99,"undo":[],"redo":[]}"#)] {
            assert_eq!(FileHistory::parse(bad), FileHistory::default());
        }
    }
}
