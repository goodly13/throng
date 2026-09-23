//! throng's user interface.

pub mod app;
pub mod code;
pub mod dialogs;
pub mod dock;
pub mod editor;
pub mod explorer;
pub mod file_ops;
pub mod file_search;
pub mod find_bar;
pub mod icons;
pub mod keymap;
pub mod link;
pub mod links;
pub mod logging;
pub mod markdown;
pub mod prefs;
pub mod preview;
pub mod project_files;
pub mod quick_open;
pub mod recovery;
pub mod search_panel;
pub mod status_strip;
pub mod term;
pub mod theme;
pub mod themes;
pub mod watch;
pub mod workspace_ui;

pub use app::{Services, ThrongApp};
