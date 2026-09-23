//! Filesystem operations with the guarantees the domain relies on.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Write `bytes` to `path` so that a crash, full disk or power loss leaves either the old file or
/// the new one — never a truncated mix. The data goes to a sibling temp file, is synced,
/// takes the old file's permissions, and is renamed over the target. A symlink is followed, so the
/// link survives and its target is what changes.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::Builder::new().prefix(".throng-save-").suffix(".tmp").tempfile_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if let Ok(meta) = fs::metadata(&target) {
        fs::set_permissions(temp.path(), meta.permissions())?;
    }
    temp.persist(&target).map_err(|e| e.error)?;
    sync_dir(dir);
    Ok(())
}

/// `path` resolved (symlinks, `.` and `..`), written as people and shells write it. Windows'
/// own resolution gives a verbatim `\\?\C:\…` path, which Command Prompt cannot even start in;
/// the prefix is dropped where the plain form names the same place (a drive or a share).
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let resolved = fs::canonicalize(path)?;
    if cfg!(windows) {
        Ok(without_verbatim_prefix(&resolved.to_string_lossy()).map_or(resolved, PathBuf::from))
    } else {
        Ok(resolved)
    }
}

/// The plain spelling of a verbatim Windows path: `\\?\C:\x` is `C:\x`, and `\\?\UNC\srv\x` is
/// `\\srv\x`. `None` for a path with no such prefix, or one only the verbatim form can name (too
/// long for the plain one).
fn without_verbatim_prefix(path: &str) -> Option<String> {
    const MAX_PATH: usize = 260;
    let plain = if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{share}")
    } else {
        let local = path.strip_prefix(r"\\?\")?;
        let drive = local.as_bytes();
        if drive.len() < 3 || !drive[0].is_ascii_alphabetic() || drive[1] != b':' || drive[2] != b'\\' {
            return None;
        }
        local.to_owned()
    };
    (plain.len() < MAX_PATH).then_some(plain)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

/// Why a trashed item could not be put back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreError {
    /// It is no longer in the trash (emptied, or restored by hand).
    Gone,
    /// Something else is at its original path now.
    Taken,
    /// This system's trash cannot put items back (macOS: the Finder can, throng cannot).
    Unsupported,
    Failed(String),
}

/// The path the trash records for `path`: its folder resolved, its own name kept (a symlink is
/// trashed as the link, not its target).
fn trash_original(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    Ok(fs::canonicalize(parent)?.join(name))
}

/// Move `path` to the trash and return a token that finds it there again, where the system can
/// restore (Linux's freedesktop trash, Windows' recycle bin); `None` where it cannot. One item, one
/// outcome: a batch that half-succeeded could not say which half to undo.
pub fn trash_restorable(path: &Path) -> Result<Option<String>, String> {
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    {
        use std::collections::HashSet;
        let original = trash_original(path).map_err(|e| e.to_string())?;
        let ours = |item: &trash::TrashItem| item.original_path() == original;
        let before: HashSet<std::ffi::OsString> = trash::os_limited::list()
            .map(|items| items.into_iter().filter(|i| ours(i)).map(|i| i.id).collect())
            .unwrap_or_default();
        trash::delete(path).map_err(|e| e.to_string())?;
        let token = trash::os_limited::list().ok().and_then(|items| {
            items
                .into_iter()
                .filter(|i| ours(i) && !before.contains(&i.id))
                .max_by_key(|i| i.time_deleted)
                .map(|i| i.id.to_string_lossy().into_owned())
        });
        Ok(token)
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        let _ = trash_original;
        trash::delete(path).map(|()| None).map_err(|e| e.to_string())
    }
}

/// Put the trashed item `token` back at `original`, refusing to replace anything there.
pub fn restore_from_trash(token: &str, original: &Path) -> Result<(), RestoreError> {
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    {
        if fs::symlink_metadata(original).is_ok() {
            return Err(RestoreError::Taken);
        }
        let items = trash::os_limited::list().map_err(|e| RestoreError::Failed(e.to_string()))?;
        let Some(item) = items.into_iter().find(|i| i.id.to_string_lossy() == token) else {
            return Err(RestoreError::Gone);
        };
        if fs::symlink_metadata(item.original_path()).is_ok() {
            return Err(RestoreError::Taken);
        }
        trash::os_limited::restore_all([item]).map_err(|e| match e {
            trash::Error::RestoreCollision { .. } => RestoreError::Taken,
            other => RestoreError::Failed(other.to_string()),
        })
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        let _ = (token, original);
        Err(RestoreError::Unsupported)
    }
}

/// Copy a file or a whole directory tree. Refusing a copy into its own subtree is the caller's job
/// (`PathRules::transfer_allowed`); this also stops if it ever finds itself inside `from`.
pub fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.is_dir() {
        let canonical_from = fs::canonicalize(from)?;
        fs::create_dir(to)?;
        let canonical_to = fs::canonicalize(to)?;
        if canonical_to.starts_with(&canonical_from) {
            let _ = fs::remove_dir(to);
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot copy a folder into itself"));
        }
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else if meta.file_type().is_symlink() {
        let link = fs::read_link(from)?;
        make_symlink(&link, to)
    } else {
        if to.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
        }
        fs::copy(from, to).map(|_| ())
    }
}

#[cfg(unix)]
fn make_symlink(link: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link, to)
}

#[cfg(windows)]
fn make_symlink(link: &Path, to: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(link, to)
}

/// Move or rename, refusing to replace anything already at `to`.
pub fn rename_no_clobber(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(to).is_ok() && !same_entry(from, to) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
    }
    fs::rename(from, to)
}

/// True when two paths are the same directory entry (e.g. a case-only rename on macOS).
fn same_entry(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Show `path` in the system file manager.
pub fn reveal(path: &Path) -> io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("explorer");
        c.arg(format!("/select,{}", path.display()));
        c
    } else {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let mut c = std::process::Command::new("xdg-open");
        c.arg(dir);
        c
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.spawn().map(|_| ())
}

/// Open a file or URL with the system's default application.
pub fn open_with_default_app(target: &str) -> io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_windows_paths_lose_their_prefix_where_the_plain_form_names_the_same_place() {
        assert_eq!(without_verbatim_prefix(r"\\?\C:\work\proj").as_deref(), Some(r"C:\work\proj"));
        assert_eq!(without_verbatim_prefix(r"\\?\UNC\srv\share\p").as_deref(), Some(r"\\srv\share\p"));
        assert_eq!(without_verbatim_prefix(r"C:\work"), None, "already plain");
        assert_eq!(without_verbatim_prefix(r"\\?\Volume{0b1c}\x"), None, "no plain spelling");
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(without_verbatim_prefix(&long), None, "too long to be plain");
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, b"old").unwrap();
        atomic_write(&file, b"new content").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"new content");
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.sh");
        fs::write(&real, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        atomic_write(&link, b"#!/bin/sh\necho hi\n").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"#!/bin/sh\necho hi\n");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn copy_recursive_copies_trees_and_refuses_self_nesting() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::write(src.join("inner/a.txt"), b"a").unwrap();
        copy_recursive(&src, &dir.path().join("dst")).unwrap();
        assert_eq!(fs::read(dir.path().join("dst/inner/a.txt")).unwrap(), b"a");
        let err = copy_recursive(&src, &src.join("inner/nested")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!src.join("inner/nested").exists());
    }

    /// The real trash, on Linux only: where no trash can be made here (no home, a read-only
    /// mount), the test says so and stops rather than failing on the environment.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_trashed_file_comes_back_from_the_trash_and_never_over_another() {
        let dir =
            tempfile::tempdir_in(std::env::var_os("HOME").map_or_else(std::env::temp_dir, PathBuf::from))
                .unwrap();
        let file = dir.path().join("throng-trash-test.txt");
        fs::write(&file, b"keep").unwrap();
        let token = match trash_restorable(&file) {
            Ok(Some(token)) => token,
            other => {
                eprintln!("no restorable trash here ({other:?}); skipping");
                return;
            }
        };
        assert!(!file.exists());
        fs::write(&file, b"other").unwrap();
        assert_eq!(restore_from_trash(&token, &file), Err(RestoreError::Taken));
        fs::remove_file(&file).unwrap();
        restore_from_trash(&token, &file).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"keep");
        assert_eq!(restore_from_trash(&token, &file), Err(RestoreError::Taken));
        fs::remove_file(&file).unwrap();
        assert_eq!(restore_from_trash(&token, &file), Err(RestoreError::Gone));
    }

    #[test]
    fn rename_never_clobbers() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        assert_eq!(rename_no_clobber(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&b).unwrap(), b"b");
        rename_no_clobber(&a, &dir.path().join("c")).unwrap();
        assert!(!a.exists());
    }
}
