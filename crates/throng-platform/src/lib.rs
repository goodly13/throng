//! Operating-system implementations behind throng's platform abstractions (Principle II).
//!
//! Everything here answers a question the domain cannot answer for itself: where this user's files
//! live, which shells are installed, how to write a file so a crash cannot truncate it, how to put a
//! file in the trash. Linux and macOS are first-class; Windows gets its own rules
//! where they differ.

pub mod dirs;
pub mod fs;
pub mod process;
pub mod shells;

use throng_core::paths::PathRules;

/// The path rules of the operating system this build targets.
#[must_use]
pub fn path_rules() -> PathRules {
    if cfg!(target_os = "macos") {
        PathRules::MACOS
    } else if cfg!(windows) {
        PathRules::WINDOWS
    } else {
        PathRules::LINUX
    }
}

/// A short name for the host OS, for diagnostics.
#[must_use]
pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Unix"
    }
}
