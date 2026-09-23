//! Diagnostics logging to a bounded file.
//!
//! One file per process kind (`ui.log`, `daemon.log`), rotated to a single `.1` generation when it
//! passes [`MAX_BYTES`] at startup — logs never grow without bound.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

/// A log file is rotated when it grows past this at startup.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Start logging to `dir/name`. Returns the log path, or `None` if the file could not be opened (the
/// app still runs; it just logs to stderr).
pub fn init(dir: &Path, name: &str) -> Option<PathBuf> {
    let filter = EnvFilter::try_from_env("THRONG_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let path = dir.join(name);
    let file = std::fs::create_dir_all(dir).ok().and_then(|()| open_rotated(&path).ok());
    match file {
        Some(file) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .try_init();
            Some(path)
        }
        None => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).try_init();
            None
        }
    }
}

fn open_rotated(path: &Path) -> std::io::Result<File> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let mut rotated = path.as_os_str().to_owned();
        rotated.push(".1");
        std::fs::rename(path, rotated)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_logs_rotate_to_one_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.log");
        std::fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        std::fs::write(dir.path().join("ui.log.1"), b"older").unwrap();
        let file = open_rotated(&path).unwrap();
        drop(file);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(std::fs::metadata(dir.path().join("ui.log.1")).unwrap().len(), MAX_BYTES + 1);
    }
}
