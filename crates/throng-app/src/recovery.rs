//! Crash recovery of unsaved edits.
//!
//! Every document with unsaved changes has a recovery file holding its text and, unless the user
//! turned it off, its undo history (at most 1 MiB of it). Files are written 400 ms after the last
//! change, on a background thread, atomically. A document that becomes clean, is closed or is
//! saved loses its file. Quitting does not ask about unsaved editors: they come back, silently,
//! on the next launch with their text and history. Files nothing refers to any more are
//! deleted at launch.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use ropey::Rope;
use serde::{Deserialize, Serialize};
use throng_core::ids::PanelId;
use throng_editor::History;

use crate::editor::{DocKey, Documents};

/// Quiet time after an edit before the recovery file is written.
pub const DEBOUNCE: Duration = Duration::from_millis(400);
/// Undo history kept per document in its recovery file.
pub const HISTORY_BYTES: usize = 1 << 20;
const FORMAT: u32 = 1;

/// What a recovery file holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub format: u32,
    /// The document's real path; `None` for a document never saved.
    pub path: Option<PathBuf>,
    /// The panel an untitled document belongs to.
    pub untitled: Option<PanelId>,
    pub text: String,
    #[serde(default)]
    pub history: Option<History>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub wrap: Option<bool>,
}

impl Record {
    /// The document key this record restores.
    #[must_use]
    pub fn key(&self, rules: &throng_core::paths::PathRules) -> Option<DocKey> {
        match (&self.path, self.untitled) {
            (Some(path), _) => Some(Documents::key_for(rules, path)),
            (None, Some(panel)) => Some(DocKey::Untitled(panel)),
            (None, None) => None,
        }
    }
}

/// A stable file name for a document key.
#[must_use]
pub fn file_name(key: &DocKey) -> String {
    let text = match key {
        DocKey::File(k) => format!("file:{k}"),
        DocKey::Untitled(panel) => format!("untitled:{panel}"),
    };
    // FNV-1a: stable across runs and platforms (unlike the standard hasher).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}.json")
}

enum Job {
    Write {
        target: PathBuf,
        record: Box<Pending>,
    },
    Delete(PathBuf),
    /// Rewrite every record in the folder without its history.
    StripHistories(PathBuf),
    Flush(Sender<()>),
}

/// A record whose text is still a rope: turned into a string on the writer thread, so the UI
/// thread never copies a large document.
struct Pending {
    path: Option<PathBuf>,
    untitled: Option<PanelId>,
    rope: Rope,
    history: Option<History>,
    language: Option<String>,
    wrap: Option<bool>,
}

/// Writes recovery files and remembers what it wrote.
pub struct Recovery {
    dir: PathBuf,
    tx: Sender<Job>,
    /// The version each document's file was last written at.
    written: HashMap<DocKey, u64>,
    /// When each document last changed, for the debounce.
    changed: HashMap<DocKey, (u64, Instant)>,
    /// Records left by the last session, whose documents may not be open yet: deleted once their
    /// document is clean, but never for merely not being open.
    adopted: HashSet<DocKey>,
}

impl Recovery {
    /// Recovery files live in `dir` (created private).
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("recovery".into()).spawn(move || writer(&rx))?;
        Ok(Self { dir, tx, written: HashMap::new(), changed: HashMap::new(), adopted: HashSet::new() })
    }

    fn target(&self, key: &DocKey) -> PathBuf {
        self.dir.join(file_name(key))
    }

    /// Every record on disk (at launch). Unreadable files are removed: they cannot be restored.
    #[must_use]
    pub fn load_all(&self) -> Vec<Record> {
        let Ok(read) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in read.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            match std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok()) {
                Some(record) if record.format == FORMAT => out.push(record),
                _ => {
                    tracing::warn!(file = %path.display(), "an unreadable recovery file was removed");
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        out
    }

    /// Delete the records whose documents nothing refers to (the reconcile at launch).
    pub fn reconcile(&self, records: &[Record], keep: impl Fn(&Record) -> bool) {
        for record in records {
            if !keep(record) {
                let key = match (&record.path, record.untitled) {
                    (Some(path), _) => {
                        // The record's own file name was computed from its key at write time; find
                        // it by content instead of re-deriving (the file may be gone now).
                        self.remove_matching(|r| r.path.as_ref() == Some(path));
                        continue;
                    }
                    (None, Some(panel)) => DocKey::Untitled(panel),
                    (None, None) => continue,
                };
                let _ = std::fs::remove_file(self.target(&key));
            }
        }
    }

    fn remove_matching(&self, pred: impl Fn(&Record) -> bool) {
        let Ok(read) = std::fs::read_dir(&self.dir) else { return };
        for entry in read.filter_map(Result::ok) {
            let path = entry.path();
            if let Some(record) =
                std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok())
                && pred(&record)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Bring the recovery files up to date with the documents: write dirty ones that have been
    /// quiet for [`DEBOUNCE`] (or all, when `flush`), delete clean or closed ones. Returns how long
    /// until a pending write is due.
    pub fn tick(&mut self, docs: &Documents, persist_history: bool, flush: bool) -> Option<Duration> {
        let now = Instant::now();
        let mut next: Option<Duration> = None;
        let mut present = HashSet::new();
        for (key, doc) in docs.iter() {
            present.insert(key.clone());
            let version = doc.buf.version() + u64::from(doc.disk == crate::editor::Disk::Deleted);
            if !doc.is_dirty() {
                let wrote = self.written.remove(key).is_some();
                if self.adopted.remove(key) || wrote {
                    let _ = self.tx.send(Job::Delete(self.target(key)));
                }
                self.changed.remove(key);
                continue;
            }
            if self.written.get(key) == Some(&version) {
                continue;
            }
            let since = match self.changed.get(key) {
                Some((v, at)) if *v == version => *at,
                _ => {
                    self.changed.insert(key.clone(), (version, now));
                    now
                }
            };
            let quiet = now.duration_since(since);
            if !flush && quiet < DEBOUNCE {
                let wait = DEBOUNCE - quiet;
                next = Some(next.map_or(wait, |n| n.min(wait)));
                continue;
            }
            let record = Pending {
                path: doc.path.clone(),
                untitled: match key {
                    DocKey::Untitled(panel) => Some(*panel),
                    DocKey::File(_) => None,
                },
                rope: doc.buf.rope().clone(),
                history: persist_history.then(|| doc.buf.history().trimmed_to(HISTORY_BYTES)),
                language: doc.language_override().map(str::to_owned),
                wrap: doc.wrap,
            };
            let _ = self.tx.send(Job::Write { target: self.target(key), record: Box::new(record) });
            self.written.insert(key.clone(), version);
            self.adopted.remove(key);
        }
        // Documents closed since the last tick lose their files.
        let gone: Vec<DocKey> = self.written.keys().filter(|k| !present.contains(k)).cloned().collect();
        for key in gone {
            self.written.remove(&key);
            self.changed.remove(&key);
            let _ = self.tx.send(Job::Delete(self.target(&key)));
        }
        if flush {
            let (done_tx, done_rx) = crossbeam_channel::bounded(1);
            let _ = self.tx.send(Job::Flush(done_tx));
            let _ = done_rx.recv_timeout(Duration::from_secs(10));
        }
        next
    }

    /// Take charge of a record the last session left, kept for its document (see [`Self::reconcile`]).
    pub fn adopt(&mut self, key: DocKey) {
        self.adopted.insert(key);
    }

    /// The history setting was turned off: every record on disk loses its history now, including
    /// those of documents not open yet, and open ones are rewritten without it.
    pub fn drop_histories(&mut self) {
        self.written.clear();
        // Queued behind any write already sent, so none of those can put a history back.
        let _ = self.tx.send(Job::StripHistories(self.dir.clone()));
    }

    /// Forget a document's record now (it was discarded).
    pub fn forget(&mut self, key: &DocKey) {
        self.written.remove(key);
        self.changed.remove(key);
        let _ = self.tx.send(Job::Delete(self.target(key)));
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn writer(rx: &Receiver<Job>) {
    for job in rx {
        match job {
            Job::Write { target, record } => {
                let Pending { path, untitled, rope, history, language, wrap } = *record;
                let record = Record {
                    format: FORMAT,
                    path,
                    untitled,
                    text: rope.to_string(),
                    history,
                    language,
                    wrap,
                };
                match serde_json::to_vec(&record) {
                    Ok(bytes) => {
                        if let Err(e) = throng_platform::fs::atomic_write(&target, &bytes) {
                            // Never log the content, only that writing failed.
                            tracing::warn!(error = %e, "could not write a recovery file");
                        }
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "could not encode a recovery file"),
                }
            }
            Job::Delete(target) => {
                let _ = std::fs::remove_file(target);
            }
            Job::StripHistories(dir) => strip_histories(&dir),
            Job::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

fn strip_histories(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.filter_map(Result::ok) {
        let path = entry.path();
        let Some(mut record) =
            std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok())
        else {
            continue;
        };
        if record.history.take().is_some()
            && let Ok(bytes) = serde_json::to_vec(&record)
            && let Err(e) = throng_platform::fs::atomic_write(&path, &bytes)
        {
            tracing::warn!(error = %e, "could not remove history from a recovery file");
        }
    }
}

#[cfg(test)]
mod tests {
    use throng_core::paths::PathRules;
    use throng_core::text::LineEnding;
    use throng_editor::Selection;

    use super::*;

    fn wait_for(path: &Path, present: bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while path.exists() != present && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_dirty_document_is_written_after_a_pause_and_removed_once_clean() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one\n").unwrap();
        let rules = PathRules::LINUX;
        let mut docs = Documents::default();
        let key = docs.ensure_file(&rules, &file, 1 << 20, LineEnding::Lf).unwrap();
        let mut recovery = Recovery::new(dir.path().join("recovery")).unwrap();
        let target = recovery.target(&key);

        assert_eq!(recovery.tick(&docs, true, false), None, "nothing to write while clean");
        let doc = docs.get_mut(&key).unwrap();
        throng_editor::commands::insert_text(&mut doc.buf, &Selection::point(4), "two\n", 0);
        assert!(recovery.tick(&docs, true, false).is_some(), "debouncing");
        assert!(!target.exists());
        recovery.tick(&docs, true, true);
        let record: Record = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(record.text, "one\ntwo\n");
        assert_eq!(record.path.as_deref(), Some(file.as_path()));
        assert!(record.history.as_ref().is_some_and(History::can_undo));

        docs.get_mut(&key).unwrap().save_to(None).unwrap();
        recovery.tick(&docs, true, true);
        wait_for(&target, false);
        assert!(!target.exists(), "a saved document has no recovery file");
    }

    #[test]
    fn a_closed_document_loses_its_file_and_history_can_be_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut docs = Documents::default();
        let panel = PanelId::new();
        let key = docs.ensure_untitled(panel, LineEnding::Lf);
        throng_editor::commands::insert_text(
            &mut docs.get_mut(&key).unwrap().buf,
            &Selection::point(0),
            "draft",
            0,
        );
        let mut recovery = Recovery::new(dir.path().join("r")).unwrap();
        recovery.tick(&docs, false, true);
        let target = recovery.target(&key);
        let record: Record = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!((record.untitled, record.history), (Some(panel), None));
        docs.remove(&key);
        recovery.tick(&docs, false, true);
        wait_for(&target, false);
        assert!(!target.exists());
    }

    #[test]
    fn turning_history_off_purges_it_from_every_record_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut docs = Documents::default();
        let key = docs.ensure_untitled(PanelId::new(), LineEnding::Lf);
        throng_editor::commands::insert_text(
            &mut docs.get_mut(&key).unwrap().buf,
            &Selection::point(0),
            "x",
            0,
        );
        let mut recovery = Recovery::new(dir.path().join("r")).unwrap();
        recovery.tick(&docs, true, true);
        // A record from last session whose editor has not opened yet.
        let waiting = DocKey::Untitled(PanelId::new());
        let history = docs.get(&key).unwrap().buf.history().clone();
        let old = Record {
            format: FORMAT,
            path: None,
            untitled: None,
            text: "y".into(),
            history: Some(history),
            language: None,
            wrap: None,
        };
        std::fs::write(recovery.target(&waiting), serde_json::to_vec(&old).unwrap()).unwrap();

        recovery.drop_histories();
        recovery.tick(&docs, false, true);
        let all = recovery.load_all();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|r| r.history.is_none()), "no history is left anywhere");
    }

    #[test]
    fn a_record_from_last_session_survives_until_its_document_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let mut recovery = Recovery::new(dir.path().join("r")).unwrap();
        let panel = PanelId::new();
        let key = DocKey::Untitled(panel);
        let old = Record {
            format: FORMAT,
            path: None,
            untitled: Some(panel),
            text: "draft".into(),
            history: None,
            language: None,
            wrap: None,
        };
        std::fs::write(recovery.target(&key), serde_json::to_vec(&old).unwrap()).unwrap();
        recovery.adopt(key.clone());

        let mut docs = Documents::default();
        recovery.tick(&docs, true, true);
        assert!(recovery.target(&key).exists(), "not open yet is not gone");
        docs.recovered.insert(key.clone(), old);
        docs.ensure_untitled(panel, LineEnding::Lf);
        recovery.tick(&docs, true, false);
        let doc = docs.get_mut(&key).unwrap();
        let all = Selection::single(0, doc.buf.len_chars());
        throng_editor::commands::insert_text(&mut doc.buf, &all, "", 0);
        recovery.tick(&docs, true, true);
        wait_for(&recovery.target(&key), false);
        assert!(!recovery.target(&key).exists(), "emptied back to clean before any rewrite");
    }

    #[test]
    fn records_round_trip_and_unreferenced_ones_are_reconciled_away() {
        let dir = tempfile::tempdir().unwrap();
        let recovery = Recovery::new(dir.path().join("r")).unwrap();
        let keep = Record {
            format: FORMAT,
            path: None,
            untitled: Some(PanelId::new()),
            text: "a".into(),
            history: None,
            language: None,
            wrap: None,
        };
        let stale = Record { untitled: Some(PanelId::new()), ..keep.clone() };
        for r in [&keep, &stale] {
            let key = DocKey::Untitled(r.untitled.unwrap());
            std::fs::write(recovery.target(&key), serde_json::to_vec(r).unwrap()).unwrap();
        }
        std::fs::write(recovery.dir().join("junk.json"), b"not json").unwrap();
        let all = recovery.load_all();
        assert_eq!(all.len(), 2);
        assert!(!recovery.dir().join("junk.json").exists(), "an unreadable record is dropped");
        recovery.reconcile(&all, |r| r.untitled == keep.untitled);
        assert_eq!(recovery.load_all(), vec![keep]);
    }
}
