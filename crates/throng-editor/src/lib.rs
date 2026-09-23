//! The text model behind throng's editor, free of any UI: a rope buffer, per-view selections with
//! many carets, transactions with one undo history per document, find and replace, soft wrapping,
//! and syntax highlighting whose cost follows the visible area.

pub mod change;
pub mod commands;
pub mod doc;
pub mod find;
pub mod highlight;
pub mod history;
pub mod lang;
pub mod lines;
pub mod selection;
pub mod wrap;

pub use change::{Assoc, Change, Transaction};
pub use doc::{Applied, TextDoc};
pub use history::{EditKind, History};
pub use selection::{Range, Selection};
pub use wrap::Wrap;
