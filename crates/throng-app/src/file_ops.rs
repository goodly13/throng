//! File operations from the tree that can be undone, and the checks that make undo safe.
//! Every undo and redo is checked against the disk first; when the disk no longer
//! matches what the entry describes, nothing changes and the reason is returned.

use std::path::{Path, PathBuf};

use throng_core::file_history::{FileHistory, FileOp};
use throng_core::paths::{PathRules, copy_name};
use throng_platform::fs::RestoreError;

/// The filesystem as undo sees it (Principle II's seam; tests use a fake).
pub trait Seam {
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn rename(&self, from: &Path, to: &Path) -> Result<(), String>;
    /// Trash `path`; `Some(token)` when it can be restored later.
    fn trash(&self, path: &Path) -> Result<Option<String>, String>;
    fn restore(&self, token: &str, original: &Path) -> Result<(), RestoreError>;
}

/// The real disk.
pub struct Disk;

impl Seam for Disk {
    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
        throng_platform::fs::rename_no_clobber(from, to).map_err(|e| e.to_string())
    }

    fn trash(&self, path: &Path) -> Result<Option<String>, String> {
        throng_platform::fs::trash_restorable(path)
    }

    fn restore(&self, token: &str, original: &Path) -> Result<(), RestoreError> {
        throng_platform::fs::restore_from_trash(token, original)
    }
}

/// What changed on disk, for open editors and the tree to follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Changed {
    Moved {
        from: PathBuf,
        to: PathBuf,
    },
    /// Back from the trash.
    Restored(PathBuf),
    Trashed(PathBuf),
}

/// An undo or redo that was not carried out: a title naming what failed, the item, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub title: String,
    pub item: PathBuf,
    pub reason: String,
}

fn name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn refuse(undo: bool, op: &FileOp, reason: String) -> Refusal {
    Refusal {
        title: format!("Can't {} {}", if undo { "undo" } else { "redo" }, op.verb()),
        item: op.item().to_path_buf(),
        reason,
    }
}

/// Undo the newest operation. `Ok(None)`: there was nothing to undo.
pub fn undo(history: &mut FileHistory, seam: &dyn Seam) -> Result<Option<Changed>, Refusal> {
    let Some(op) = history.next_undo().cloned() else { return Ok(None) };
    let changed = match &op {
        FileOp::Move { from, to } => {
            check_move(seam, to, from).map_err(|reason| refuse(true, &op, reason))?;
            seam.rename(to, from).map_err(|reason| refuse(true, &op, reason))?;
            Changed::Moved { from: to.clone(), to: from.clone() }
        }
        FileOp::Trash { path, token } => {
            if let Some(parent) = path.parent().filter(|p| !seam.is_dir(p)) {
                return Err(refuse(true, &op, format!("Its folder {} no longer exists.", parent.display())));
            }
            seam.restore(token, path).map_err(|e| {
                let reason = match e {
                    RestoreError::Gone => "It is no longer in the trash.".to_owned(),
                    RestoreError::Taken => format!("Something else is now at {}.", path.display()),
                    RestoreError::Unsupported => "This system's trash cannot put items back.".to_owned(),
                    RestoreError::Failed(reason) => reason,
                };
                refuse(true, &op, reason)
            })?;
            Changed::Restored(path.clone())
        }
    };
    history.undone();
    Ok(Some(changed))
}

/// Apply the newest undone operation again. `Ok(None)`: there was nothing to redo.
pub fn redo(history: &mut FileHistory, seam: &dyn Seam) -> Result<Option<Changed>, Refusal> {
    let Some(op) = history.next_redo().cloned() else { return Ok(None) };
    let (changed, again) = match &op {
        FileOp::Move { from, to } => {
            check_move(seam, from, to).map_err(|reason| refuse(false, &op, reason))?;
            seam.rename(from, to).map_err(|reason| refuse(false, &op, reason))?;
            (Changed::Moved { from: from.clone(), to: to.clone() }, Some(op.clone()))
        }
        FileOp::Trash { path, .. } => {
            if !seam.exists(path) {
                return Err(refuse(false, &op, format!("Nothing is at {} any more.", path.display())));
            }
            let token = seam.trash(path).map_err(|reason| refuse(false, &op, reason))?;
            let again = token.map(|token| FileOp::Trash { path: path.clone(), token });
            (Changed::Trashed(path.clone()), again)
        }
    };
    history.redone(again);
    Ok(Some(changed))
}

/// Whether the item at `from` can go back to `to`: it must still be there, and nothing may be in
/// its way.
fn check_move(seam: &dyn Seam, from: &Path, to: &Path) -> Result<(), String> {
    if !seam.exists(from) {
        return Err(format!("\"{}\" is no longer at {}.", name(from), from.display()));
    }
    if seam.exists(to) {
        return Err(format!("Something else is now at {}.", to.display()));
    }
    if let Some(parent) = to.parent().filter(|p| !seam.is_dir(p)) {
        return Err(format!("The folder {} no longer exists.", parent.display()));
    }
    Ok(())
}

/// Where a drop or paste onto `onto` puts things: into a folder, or beside a file.
#[must_use]
pub fn drop_folder(onto: &Path, onto_is_dir: bool) -> PathBuf {
    if onto_is_dir {
        onto.to_path_buf()
    } else {
        onto.parent().map_or_else(|| onto.to_path_buf(), Path::to_path_buf)
    }
}

/// A move or copy the tree was asked for, checked against the drag-and-drop rules. `Ok(None)`: nothing to do (a
/// move into the folder it is already in).
pub fn plan_transfer(
    rules: &PathRules,
    root: &Path,
    from: &Path,
    into: &Path,
    copy: bool,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<Option<PathBuf>, String> {
    if rules.same(from, root) {
        return Err("The project's root folder cannot be moved or copied.".into());
    }
    if !rules.is_within(root, into) {
        return Err("That folder is outside this project.".into());
    }
    if !rules.transfer_allowed(from, into) {
        return Err(format!("\"{}\" cannot go inside itself.", name(from)));
    }
    let Some(file_name) = from.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Err("That item has no name.".into());
    };
    let same_folder = from.parent().is_some_and(|p| rules.same(p, into));
    if copy {
        let unique = copy_name(&file_name, |candidate| exists(&into.join(candidate)));
        return Ok(Some(into.join(unique)));
    }
    if same_folder {
        return Ok(None);
    }
    let to = into.join(&file_name);
    if exists(&to) {
        return Err(format!("\"{file_name}\" already exists in {}.", into.display()));
    }
    Ok(Some(to))
}

/// Paths as a terminal should receive them when dropped: absolute, quoted when they need it,
/// space-separated, with a trailing space and never a newline (so nothing runs by itself).
#[must_use]
pub fn for_terminal(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for path in paths {
        let text = path.display().to_string().replace(['\n', '\r'], "");
        out.push_str(&quote(&text));
        out.push(' ');
    }
    out
}

#[cfg(windows)]
fn quote(text: &str) -> String {
    if text.contains([' ', '\t', '&', '(', ')', '^', ';', ',']) {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    }
}

#[cfg(not(windows))]
fn quote(text: &str) -> String {
    let plain = text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c));
    if plain { text.to_owned() } else { format!("'{}'", text.replace('\'', r"'\''")) }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    /// Files and folders in memory, and a trash.
    #[derive(Default)]
    struct Fake {
        entries: RefCell<BTreeSet<PathBuf>>,
        trash: RefCell<BTreeMap<String, PathBuf>>,
        restorable: bool,
    }

    impl Fake {
        fn with(paths: &[&str]) -> Self {
            let fake = Self { restorable: true, ..Self::default() };
            fake.entries.borrow_mut().extend(paths.iter().map(PathBuf::from));
            fake
        }
    }

    impl Seam for Fake {
        fn exists(&self, path: &Path) -> bool {
            self.entries.borrow().contains(path)
        }
        fn is_dir(&self, path: &Path) -> bool {
            path == Path::new("/p")
                || self.entries.borrow().iter().any(|e| e.parent() == Some(path))
                || self.entries.borrow().contains(path)
        }
        fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
            let mut entries = self.entries.borrow_mut();
            assert!(entries.remove(from), "rename of a missing entry");
            assert!(entries.insert(to.to_path_buf()), "rename over an entry");
            Ok(())
        }
        fn trash(&self, path: &Path) -> Result<Option<String>, String> {
            self.entries.borrow_mut().remove(path);
            let token = format!("t{}", self.trash.borrow().len());
            self.trash.borrow_mut().insert(token.clone(), path.to_path_buf());
            Ok(self.restorable.then_some(token))
        }
        fn restore(&self, token: &str, original: &Path) -> Result<(), RestoreError> {
            if self.exists(original) {
                return Err(RestoreError::Taken);
            }
            let path = self.trash.borrow_mut().remove(token).ok_or(RestoreError::Gone)?;
            self.entries.borrow_mut().insert(path);
            Ok(())
        }
    }

    #[test]
    fn a_rename_undoes_and_redoes_and_editors_are_told_where_it_went() {
        let disk = Fake::with(&["/p/b.txt"]);
        let mut h = FileHistory::default();
        h.record(FileOp::Move { from: "/p/a.txt".into(), to: "/p/b.txt".into() });
        let back = undo(&mut h, &disk).unwrap();
        assert_eq!(back, Some(Changed::Moved { from: "/p/b.txt".into(), to: "/p/a.txt".into() }));
        assert!(disk.exists(Path::new("/p/a.txt")));
        redo(&mut h, &disk).unwrap();
        assert!(disk.exists(Path::new("/p/b.txt")));
        assert_eq!(undo(&mut FileHistory::default(), &disk), Ok(None));
    }

    #[test]
    fn an_undo_whose_world_changed_is_refused_and_changes_nothing() {
        // Someone put a new a.txt where the rename would go back to.
        let disk = Fake::with(&["/p/a.txt", "/p/b.txt"]);
        let mut h = FileHistory::default();
        h.record(FileOp::Move { from: "/p/a.txt".into(), to: "/p/b.txt".into() });
        let refusal = undo(&mut h, &disk).unwrap_err();
        assert_eq!(refusal.title, "Can't undo rename");
        assert_eq!(refusal.item, PathBuf::from("/p/b.txt"));
        assert!(refusal.reason.contains("Something else is now at /p/a.txt"), "{}", refusal.reason);
        assert!(h.can_undo() && !h.can_redo(), "the history is untouched");
        assert!(disk.exists(Path::new("/p/b.txt")));

        // The renamed item itself went.
        let disk = Fake::with(&["/p/x"]);
        let refusal = undo(&mut h, &disk).unwrap_err();
        assert!(refusal.reason.contains("no longer at /p/b.txt"), "{}", refusal.reason);
    }

    #[test]
    fn a_delete_is_undone_from_the_trash_and_a_purged_one_is_refused() {
        let disk = Fake::with(&["/p/a.txt"]);
        let mut h = FileHistory::default();
        let token = disk.trash(Path::new("/p/a.txt")).unwrap().unwrap();
        h.record(FileOp::Trash { path: "/p/a.txt".into(), token });
        assert_eq!(undo(&mut h, &disk).unwrap(), Some(Changed::Restored("/p/a.txt".into())));
        assert!(disk.exists(Path::new("/p/a.txt")));
        assert_eq!(redo(&mut h, &disk).unwrap(), Some(Changed::Trashed("/p/a.txt".into())));
        disk.trash.borrow_mut().clear();
        let refusal = undo(&mut h, &disk).unwrap_err();
        assert_eq!(refusal.title, "Can't undo delete");
        assert_eq!(refusal.reason, "It is no longer in the trash.");
    }

    #[test]
    fn transfers_follow_the_drop_rules() {
        let rules = PathRules::LINUX;
        let root = Path::new("/p");
        let taken = |p: &Path| p == Path::new("/p/dst/a.txt") || p == Path::new("/p/src/a.txt");
        let plan = |from: &str, into: &str, copy: bool| {
            plan_transfer(&rules, root, Path::new(from), Path::new(into), copy, &taken)
        };
        assert_eq!(plan("/p/src/a.txt", "/p/other", false), Ok(Some("/p/other/a.txt".into())));
        assert_eq!(plan("/p/src/a.txt", "/p/src", false), Ok(None), "its own folder: nothing to do");
        assert!(plan("/p/src/a.txt", "/p/dst", false).unwrap_err().contains("already exists"));
        assert_eq!(plan("/p/src/a.txt", "/p/src", true), Ok(Some("/p/src/a copy.txt".into())));
        assert_eq!(plan("/p/src/a.txt", "/p/dst", true), Ok(Some("/p/dst/a copy.txt".into())));
        assert!(plan("/p/src", "/p/src/deep", false).unwrap_err().contains("inside itself"));
        assert!(plan("/p", "/p/x", false).unwrap_err().contains("root folder"));
        assert!(plan("/p/src/a.txt", "/q", false).unwrap_err().contains("outside"));
        assert_eq!(drop_folder(Path::new("/p/src/a.txt"), false), PathBuf::from("/p/src"));
        assert_eq!(drop_folder(Path::new("/p/src"), true), PathBuf::from("/p/src"));
    }

    #[cfg(not(windows))]
    #[test]
    fn paths_for_a_terminal_are_quoted_and_never_end_a_line() {
        let paths = [PathBuf::from("/p/a.txt"), PathBuf::from("/p/my file's.txt")];
        assert_eq!(for_terminal(&paths), r"/p/a.txt '/p/my file'\''s.txt' ");
    }
}
