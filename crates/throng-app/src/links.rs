//! Following a link: where a written path points, and what following it does. The grammar
//! that finds links is `throng_core::links`; this is the part that looks at the disk.

use std::path::{Component, Path, PathBuf};

use throng_core::links::Target;
use throng_core::paths::PathRules;

/// Where relative paths are tried, in order: an editor's own folder or a terminal's working
/// directory, then the project root.
#[derive(Clone, Debug, Default)]
pub struct Bases {
    pub first: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// Whether `/c/…` and `/mnt/c/…` name Windows drives (Git Bash, MSYS, Cygwin and WSL output
    /// on Windows).
    pub drive_forms: bool,
}

impl Bases {
    /// Bases for this platform: home from the environment, drive forms on Windows.
    #[must_use]
    pub fn here(first: Option<PathBuf>, root: Option<PathBuf>) -> Self {
        Self { first, root, home: std::env::home_dir(), drive_forms: cfg!(windows) }
    }
}

/// What following a link does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Follow {
    /// Hand it to the system: a web address or an allowed scheme.
    Url(String),
    /// Open this file of the project in an editor, at a position.
    Open { path: PathBuf, line: Option<u32>, column: Option<u32> },
    /// Show it in the system's file manager: a folder, or anything outside the project.
    Reveal(PathBuf),
    /// Nothing is there.
    Missing(PathBuf),
}

/// What the user asked of a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAction {
    /// Ctrl+click or Open Link.
    Follow,
    /// Copy Link Address: the resolved path, or the address.
    Copy,
    /// Open in the system's file manager.
    Reveal,
    /// Open with the system's default program for it.
    OpenDefault,
}

/// `path` with `.` and `..` worked out on the text, so the same file always has the same spelling.
#[must_use]
pub fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The one location a written path names: the first candidate that exists, or the
/// first candidate when none does.
#[must_use]
pub fn resolve(written: &str, bases: &Bases) -> PathBuf {
    let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(rest) = written.strip_prefix("~/").or_else(|| written.strip_prefix("~\\")) {
        if let Some(home) = &bases.home {
            candidates.push(home.join(rest));
        }
    } else if written.starts_with('/') && !written.starts_with("//") {
        // On Windows, `/c/…` or `/mnt/c/…` is a Unix-like shell's spelling of a drive path, tried
        // first. Otherwise a leading `/` is tried against the project root first, then as itself.
        let drive = throng_core::links::windows_drive_form(written).filter(|_| bases.drive_forms);
        let as_itself = drive.is_none();
        candidates.extend(drive.map(PathBuf::from));
        if let Some(root) = &bases.root {
            candidates.push(root.join(written.trim_start_matches('/')));
        }
        if as_itself {
            candidates.push(PathBuf::from(written));
        }
    } else if Path::new(written).is_absolute() || written.starts_with("//") || written.starts_with("\\\\") {
        candidates.push(PathBuf::from(written));
    } else {
        candidates.extend(bases.first.iter().chain(&bases.root).map(|base| base.join(written)));
    }
    let candidates: Vec<PathBuf> = candidates.iter().map(|p| normalise(p)).collect();
    candidates
        .iter()
        .find(|p| exists(p))
        .or_else(|| candidates.first())
        .cloned()
        .unwrap_or_else(|| PathBuf::from(written))
}

/// What following `target` does: a file in the project opens in an
/// editor; a folder, or anything outside the project, is revealed; nothing there is reported.
#[must_use]
pub fn follow(target: &Target, bases: &Bases, rules: &PathRules) -> Follow {
    match target {
        Target::Web(url) | Target::Scheme(url) => Follow::Url(url.clone()),
        Target::File { path, line, column } => {
            let resolved = resolve(path, bases);
            let Ok(meta) = std::fs::metadata(&resolved) else { return Follow::Missing(resolved) };
            let in_project = bases.root.as_deref().is_some_and(|root| rules.is_within(root, &resolved));
            if meta.is_file() && in_project {
                Follow::Open { path: resolved, line: *line, column: *column }
            } else {
                Follow::Reveal(resolved)
            }
        }
    }
}

/// The text Copy Link Address puts on the clipboard: the resolved absolute path, with its position,
/// or the address.
#[must_use]
pub fn address(target: &Target, bases: &Bases) -> String {
    match target {
        Target::Web(url) | Target::Scheme(url) => url.clone(),
        Target::File { path, line, column } => {
            let mut text = resolve(path, bases).display().to_string();
            if let Some(line) = line {
                text.push_str(&format!(":{line}"));
                if let Some(column) = column {
                    text.push_str(&format!(":{column}"));
                }
            }
            text
        }
    }
}

/// A short description for a hover: what the link names and how to follow it.
#[must_use]
pub fn hover_text(target: &Target) -> String {
    let chord = if cfg!(target_os = "macos") { "Cmd+click" } else { "Ctrl+click" };
    let what = match target {
        Target::Web(url) | Target::Scheme(url) => url.clone(),
        Target::File { path, line: Some(line), column: Some(column) } => {
            format!("{path}, line {line}, column {column}")
        }
        Target::File { path, line: Some(line), column: None } => format!("{path}, line {line}"),
        Target::File { path, .. } => path.clone(),
    };
    format!("{what}\n{chord} to follow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_try_the_first_base_then_the_root_and_the_first_existing_wins() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        let bases = Bases {
            first: Some(root.join("src")),
            root: Some(root.clone()),
            home: Some(dir.path().into()),
            ..Bases::default()
        };
        assert_eq!(resolve("lib.rs", &bases), root.join("src/lib.rs"));
        assert_eq!(resolve("README.md", &bases), root.join("README.md"), "not beside the file: the root");
        assert_eq!(resolve("../README.md", &bases), root.join("README.md"));
        assert_eq!(
            resolve("/src/lib.rs", &bases),
            root.join("src/lib.rs"),
            "a leading / tries the root first"
        );
        assert_eq!(resolve("~/p/README.md", &bases), root.join("README.md"));
        assert_eq!(
            resolve("gone.txt", &bases),
            root.join("src/gone.txt"),
            "none exists: the first candidate"
        );
    }

    #[test]
    fn following_opens_project_files_reveals_the_rest_and_reports_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "").unwrap();
        let rules = throng_platform::path_rules();
        let bases = Bases { root: Some(root.clone()), ..Bases::default() };
        let a_rs = root.join("src").join("a.rs");
        let file = |path: &str, line| Target::File { path: path.into(), line, column: None };
        assert_eq!(
            follow(&file("src/a.rs", Some(3)), &bases, &rules),
            Follow::Open { path: a_rs.clone(), line: Some(3), column: None }
        );
        assert_eq!(follow(&file("src", None), &bases, &rules), Follow::Reveal(root.join("src")));
        let outside = dir.path().join("outside.txt");
        assert_eq!(follow(&file(outside.to_str().unwrap(), None), &bases, &rules), Follow::Reveal(outside));
        assert_eq!(follow(&file("nope.rs", None), &bases, &rules), Follow::Missing(root.join("nope.rs")));
        assert_eq!(
            follow(&Target::Web("https://x.dev".into()), &bases, &rules),
            Follow::Url("https://x.dev".into())
        );
        assert_eq!(address(&file("src/a.rs", Some(3)), &bases), format!("{}:3", a_rs.display()));
    }

    #[test]
    fn a_unix_shells_drive_path_names_the_windows_drive_only_where_drives_are() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(root.join("c")).unwrap();
        std::fs::write(root.join("c/in-project.rs"), "").unwrap();
        let windows = Bases { root: Some(root.clone()), drive_forms: true, ..Bases::default() };
        assert_eq!(resolve("/c/Users/me/a.rs", &windows), PathBuf::from(r"C:\Users\me\a.rs"));
        assert_eq!(resolve("/mnt/c/Users/me/a.rs", &windows), PathBuf::from(r"C:\Users\me\a.rs"));
        assert_eq!(
            resolve("/c/in-project.rs", &windows),
            root.join("c/in-project.rs"),
            "a project file that exists still wins over a drive path that does not"
        );
        let unix = Bases { root: Some(root.clone()), ..Bases::default() };
        assert_eq!(resolve("/mnt/c/x.rs", &unix), root.join("mnt/c/x.rs"), "no drives: the usual rule");
    }
}
