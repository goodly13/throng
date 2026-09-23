//! Filesystem change notifications for the directories the UI shows or edits in.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// Watches a changing set of directories, non-recursively. Changes are reported in the spelling
/// the directory was watched under, even where the platform reports its canonical path (macOS
/// reports `/private/var/…` for a folder watched as `/var/…`).
pub struct DirWatcher {
    watcher: Option<RecommendedWatcher>,
    watched: BTreeSet<PathBuf>,
    /// Watched directories whose canonical path differs: `(canonical, as watched)`.
    aliases: Vec<(PathBuf, PathBuf)>,
    events: Receiver<PathBuf>,
    pub error: Option<String>,
}

impl DirWatcher {
    /// `wake` is called from the watcher thread whenever something changes.
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (tx, events) = crossbeam_channel::unbounded();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                for path in event.paths {
                    let _ = tx.send(path);
                }
                wake();
            }
        });
        match watcher {
            Ok(watcher) => Self {
                watcher: Some(watcher),
                watched: BTreeSet::new(),
                aliases: Vec::new(),
                events,
                error: None,
            },
            Err(e) => Self {
                watcher: None,
                watched: BTreeSet::new(),
                aliases: Vec::new(),
                events,
                error: Some(format!("File changes will not be noticed automatically: {e}")),
            },
        }
    }

    /// Watch exactly `dirs`.
    pub fn sync(&mut self, dirs: impl IntoIterator<Item = PathBuf>) {
        let Some(watcher) = self.watcher.as_mut() else { return };
        let wanted: BTreeSet<PathBuf> = dirs.into_iter().collect();
        for gone in self.watched.difference(&wanted).cloned().collect::<Vec<_>>() {
            let _ = watcher.unwatch(&gone);
            self.aliases.retain(|(_, spelled)| spelled != &gone);
            self.watched.remove(&gone);
        }
        for new in wanted.difference(&self.watched).cloned().collect::<Vec<_>>() {
            match watcher.watch(&new, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    if let Ok(canonical) = new.canonicalize()
                        && canonical != new
                    {
                        self.aliases.push((canonical, new.clone()));
                    }
                    self.watched.insert(new);
                }
                Err(e) => {
                    tracing::debug!(dir = %new.display(), error = %e, "could not watch");
                    if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) {
                        self.error = Some(
                            "The system limit on watched folders was reached; some folders will not refresh on their own."
                                .to_owned(),
                        );
                    }
                }
            }
        }
    }

    /// Paths that changed since the last call, deduplicated, in the watched spelling.
    pub fn drain(&self) -> BTreeSet<PathBuf> {
        self.events.try_iter().map(|path| self.as_watched(path)).collect()
    }

    fn as_watched(&self, path: PathBuf) -> PathBuf {
        for (canonical, spelled) in &self.aliases {
            if let Ok(rest) = path.strip_prefix(canonical) {
                return if rest.as_os_str().is_empty() { spelled.clone() } else { spelled.join(rest) };
            }
        }
        path
    }

    #[must_use]
    pub fn is_watching(&self, dir: &Path) -> bool {
        self.watched.contains(dir)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_change_is_reported_under_the_name_its_folder_was_watched_by() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        #[cfg(unix)]
        let watched = {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            link
        };
        #[cfg(not(unix))]
        let watched = real.clone();
        let mut watcher = DirWatcher::new(|| {});
        watcher.sync([watched.clone()]);
        assert!(watcher.is_watching(&watched));
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(real.join("a.json"), "{}").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = BTreeSet::new();
        while !seen.contains(&watched.join("a.json")) {
            assert!(Instant::now() < deadline, "saw {seen:?}");
            std::thread::sleep(Duration::from_millis(20));
            seen.extend(watcher.drain());
        }
    }
}
