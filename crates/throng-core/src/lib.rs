//! OS-agnostic domain model for throng (Principle II).
//!
//! Nothing in this crate touches the filesystem, spawns a process, or asks which operating system it
//! is running on. Where a rule genuinely differs by platform — whether `Foo` and `foo` name the same
//! file, which characters a file name may contain — the rule is a value ([`paths::PathRules`]) that
//! the platform layer hands in, never a `cfg!` inside the domain.

pub mod failure;
pub mod file_history;
pub mod icons;
pub mod ids;
pub mod keymap;
pub mod links;
pub mod notice;
pub mod paths;
pub mod project;
pub mod settings;
pub mod subworkspace;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod workspace;

pub use ids::{PanelId, ProjectId, TabId, TerminalId};
