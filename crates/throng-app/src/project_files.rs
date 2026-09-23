//! The files of a project, as Quick Open and Find in Files see them: the explorer's exclusions
//! and the project's hidden paths apply, `.gitignore` is not read, and symlinked
//! folders are not followed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

/// A file under a project root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFile {
    /// Relative to the root, `/`-separated.
    pub rel: String,
    pub path: PathBuf,
    /// Under a path hidden in this project.
    pub hidden: bool,
}

/// What to leave out.
#[derive(Clone, Debug, Default)]
pub struct Exclusions {
    /// Entry names excluded everywhere (`explorer.exclude`).
    pub names: Vec<String>,
    /// Paths hidden in this project, relative to its root.
    pub hidden: Vec<String>,
    /// Keep hidden paths in the walk, flagged, rather than leaving them out.
    pub keep_hidden: bool,
}

impl Exclusions {
    fn is_hidden(&self, rel: &str) -> bool {
        self.hidden
            .iter()
            .any(|h| rel == h || rel.strip_prefix(h.as_str()).is_some_and(|rest| rest.starts_with('/')))
    }
}

/// Walk `dir` (inside `root`), calling `visit` for each file in a stable, natural order (folders'
/// contents after their files, as the explorer lists them). `visit` returns `false` to stop.
pub fn walk(root: &Path, dir: &Path, ex: &Exclusions, visit: &mut dyn FnMut(ProjectFile) -> bool) -> bool {
    let Ok(read) = std::fs::read_dir(dir) else { return true };
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in read.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ex.names.contains(&name) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        let Some(rel) = relative(root, &path) else { continue };
        let hidden = ex.is_hidden(&rel);
        if hidden && !ex.keep_hidden {
            continue;
        }
        if kind.is_dir() {
            dirs.push((name, path));
        } else if kind.is_file() || (kind.is_symlink() && path.is_file()) {
            files.push((name, ProjectFile { rel, path, hidden }));
        }
        // A symlinked folder is not followed: it may lead outside the project or loop.
    }
    files.sort_by(|a, b| crate::explorer::natural_cmp(&a.0, &b.0));
    dirs.sort_by(|a, b| crate::explorer::natural_cmp(&a.0, &b.0));
    for (_, file) in files {
        if !visit(file) {
            return false;
        }
    }
    for (_, sub) in dirs {
        if !walk(root, &sub, ex, visit) {
            return false;
        }
    }
    true
}

/// `path` relative to `root` with `/` separators.
#[must_use]
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Vec<String> =
        rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(parts.join("/"))
}

/// Files Quick Open offers at most.
pub const INDEX_LIMIT: usize = 50_000;

/// A project's file list, built in the background so no keystroke waits on the disk.
pub struct FileIndex {
    pub root: PathBuf,
    pub files: Arc<Vec<ProjectFile>>,
    pub built_at: Option<Instant>,
    pub building: bool,
    generation: Arc<AtomicU64>,
    rx: Receiver<(u64, Vec<ProjectFile>)>,
    tx: Sender<(u64, Vec<ProjectFile>)>,
}

impl FileIndex {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            root,
            files: Arc::new(Vec::new()),
            built_at: None,
            building: false,
            generation: Arc::new(AtomicU64::new(0)),
            rx,
            tx,
        }
    }

    /// Rebuild if the list is older than `max_age` (changes show within 2 s).
    pub fn refresh(&mut self, ex: &Exclusions, max_age: Duration, wake: impl Fn() + Send + 'static) {
        self.poll();
        let fresh = self.built_at.is_some_and(|at| at.elapsed() < max_age);
        if self.building || fresh {
            return;
        }
        self.building = true;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let root = self.root.clone();
        let mut ex = ex.clone();
        ex.keep_hidden = true;
        let tx = self.tx.clone();
        let current = Arc::clone(&self.generation);
        std::thread::spawn(move || {
            let mut files = Vec::new();
            walk(&root, &root, &ex, &mut |f| {
                files.push(f);
                files.len() < INDEX_LIMIT && current.load(Ordering::SeqCst) == generation
            });
            let _ = tx.send((generation, files));
            wake();
        });
    }

    /// Take a finished build.
    pub fn poll(&mut self) {
        while let Ok((generation, files)) = self.rx.try_recv() {
            if generation == self.generation.load(Ordering::SeqCst) {
                self.files = Arc::new(files);
                self.built_at = Some(Instant::now());
                self.building = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_in_order_leaving_out_exclusions_and_hidden_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for p in ["b.txt", "a10.txt", "a2.txt", "src/main.rs", "node_modules/x.js", "gen/out.txt"] {
            let path = root.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        let ex =
            Exclusions { names: vec!["node_modules".into()], hidden: vec!["gen".into()], keep_hidden: false };
        let mut seen = Vec::new();
        walk(root, root, &ex, &mut |f| {
            seen.push(f.rel);
            true
        });
        assert_eq!(seen, vec!["a2.txt", "a10.txt", "b.txt", "src/main.rs"]);
        let keep = Exclusions { keep_hidden: true, ..ex };
        let mut flagged = Vec::new();
        walk(root, root, &keep, &mut |f| {
            flagged.push((f.rel, f.hidden));
            true
        });
        assert!(flagged.contains(&("gen/out.txt".to_owned(), true)));
    }

    #[test]
    fn a_hidden_path_hides_only_itself_and_what_is_under_it() {
        let ex = Exclusions { hidden: vec!["gen".into()], ..Exclusions::default() };
        assert!(ex.is_hidden("gen"));
        assert!(ex.is_hidden("gen/a"));
        assert!(!ex.is_hidden("general.txt"));
    }
}
