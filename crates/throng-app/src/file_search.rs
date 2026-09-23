//! Find in Files: the scan behind the panel, and the commit of replacements.
//!
//! A scan runs on its own thread and streams results back in batches (at most 250
//! rows, at least every 50 ms). A new scan cancels the old one through a generation counter.
//! Binary files, files over the editor's size limit and unreadable files are skipped and
//! counted. Matching is the find bar's, so the two agree.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use ropey::Rope;
use throng_core::text::{self, LineEnding};
use throng_editor::find::{self, Query};
use throng_editor::lines;

use crate::project_files::{self, Exclusions};

/// Matches listed at most.
pub const MATCH_CAP: usize = 20_000;
const BATCH_ROWS: usize = 250;
const BATCH_EVERY: Duration = Duration::from_millis(50);
/// Characters of context either side of a match in a result row.
const CONTEXT: usize = 40;

/// One match in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatch {
    /// Character offsets in the decoded text.
    pub from: usize,
    pub to: usize,
    /// 0-based line.
    pub line: usize,
    /// The matched text, as found (checked again before replacing it).
    pub found: String,
    /// A short excerpt of the line, and where the match sits in it (character offsets).
    pub snippet: String,
    pub span: (usize, usize),
}

/// A file with matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHits {
    pub rel: String,
    pub path: PathBuf,
    pub matches: Vec<FileMatch>,
    /// What the file looked like when scanned, to mark it stale later.
    pub stamp: Option<(u64, Option<SystemTime>)>,
}

/// What a scan reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Found(Vec<FileHits>),
    Done {
        files: usize,
        skipped: usize,
        capped: bool,
    },
    /// The scope is not in the project, or does not exist.
    Refused(String),
}

/// What to search.
#[derive(Clone, Debug)]
pub struct Spec {
    pub root: PathBuf,
    /// Folder or file inside the root; empty for the root.
    pub scope: String,
    pub query: Query,
    pub exclusions: Exclusions,
    pub max_bytes: u64,
}

/// Where a scope points, or why it cannot be searched.
///
/// The answer is spelled under `root` as given, not canonicalised: the walk strips `root` off
/// every path it finds, and a canonical spelling differs from the given one wherever the root
/// sits behind a symlink (`/var` on macOS) or is written without the `\\?\` prefix (Windows).
pub fn resolve_scope(root: &Path, scope: &str) -> Result<PathBuf, String> {
    let trimmed = scope.trim();
    if trimmed.is_empty() {
        return Ok(root.to_path_buf());
    }
    let unified = trimmed.replace('\\', "/");
    let unified = unified.strip_prefix("./").unwrap_or(&unified).trim_end_matches('/');
    let candidate =
        if Path::new(unified).is_absolute() { PathBuf::from(unified) } else { root.join(unified) };
    let resolved = std::fs::canonicalize(&candidate).map_err(|_| format!("Could not find `{trimmed}`."))?;
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    match resolved.strip_prefix(&canonical_root) {
        Ok(inside) => Ok(root.join(inside)),
        Err(_) => Err(format!("`{trimmed}` is outside this project.")),
    }
}

/// A running scan, owned by one panel.
pub struct Scan {
    generation: Arc<AtomicU64>,
    rx: Receiver<(u64, Update)>,
    tx: Sender<(u64, Update)>,
}

impl Default for Scan {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self { generation: Arc::new(AtomicU64::new(0)), rx, tx }
    }
}

impl Scan {
    /// Start a scan, cancelling any before it.
    pub fn start(&self, spec: Spec, wake: impl Fn() + Send + 'static) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current = Arc::clone(&self.generation);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            run(&spec, generation, &current, &tx, &wake);
        });
    }

    /// Stop the running scan (the panel closed, the query was cleared).
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Updates from the current scan; stale ones are dropped.
    pub fn poll(&self) -> Vec<Update> {
        let current = self.generation.load(Ordering::SeqCst);
        self.rx.try_iter().filter(|(g, _)| *g == current).map(|(_, u)| u).collect()
    }
}

fn run(spec: &Spec, generation: u64, current: &AtomicU64, tx: &Sender<(u64, Update)>, wake: &dyn Fn()) {
    let alive = || current.load(Ordering::SeqCst) == generation;
    let send = |update: Update| {
        let _ = tx.send((generation, update));
        wake();
    };
    let target = match resolve_scope(&spec.root, &spec.scope) {
        Ok(target) => target,
        Err(reason) => return send(Update::Refused(reason)),
    };
    let root = &spec.root;
    let (mut files, mut skipped, mut total) = (0usize, 0usize, 0usize);
    let mut batch: Vec<FileHits> = Vec::new();
    let mut rows = 0usize;
    let mut last_flush = Instant::now();
    let mut capped = false;
    let mut search_file = |path: &Path, rel: String| -> bool {
        if !alive() {
            return false;
        }
        files += 1;
        match search_one(path, &spec.query, spec.max_bytes, MATCH_CAP - total) {
            Some(matches) if !matches.is_empty() => {
                total += matches.len();
                rows += matches.len() + 1;
                let meta = std::fs::metadata(path).ok();
                batch.push(FileHits {
                    rel,
                    path: path.to_path_buf(),
                    stamp: meta.map(|m| (m.len(), m.modified().ok())),
                    matches,
                });
            }
            Some(_) => {}
            None => skipped += 1,
        }
        if rows >= BATCH_ROWS || (last_flush.elapsed() >= BATCH_EVERY && !batch.is_empty()) {
            send(Update::Found(std::mem::take(&mut batch)));
            rows = 0;
            last_flush = Instant::now();
        }
        if total >= MATCH_CAP {
            capped = true;
            return false;
        }
        true
    };
    if target.is_file() {
        // A single-file scope is searched whatever the exclusions say.
        let rel = project_files::relative(root, &target).unwrap_or_default();
        search_file(&target, rel);
    } else {
        project_files::walk(root, &target, &spec.exclusions, &mut |f| search_file(&f.path, f.rel));
    }
    if !alive() {
        return;
    }
    if !batch.is_empty() {
        send(Update::Found(batch));
    }
    send(Update::Done { files, skipped, capped });
}

/// Whether bytes look like a binary file: a NUL in the first 8000.
#[must_use]
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

/// The matches in one file, or `None` when it is skipped (binary, too large, unreadable, not text).
#[must_use]
pub fn search_one(path: &Path, query: &Query, max_bytes: u64, limit: usize) -> Option<Vec<FileMatch>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > max_bytes {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if is_binary(&bytes) {
        return None;
    }
    let decoded = text::decode(&bytes, LineEnding::Lf).ok()?;
    let rope = Rope::from_str(&decoded.text);
    Some(matches_in(&rope, query, limit))
}

/// Matches in a text, with snippets.
#[must_use]
pub fn matches_in(rope: &Rope, query: &Query, limit: usize) -> Vec<FileMatch> {
    find::find_all(rope, query, limit)
        .into_iter()
        .map(|(from, to)| {
            let line = lines::line_of(rope, from);
            let text = lines::line_text(rope, line);
            let start = from - lines::line_start(rope, line);
            let (snippet, span) = snippet(&text, start, start + (to - from));
            FileMatch { from, to, line, found: rope.slice(from..to).to_string(), snippet, span }
        })
        .collect()
}

/// About [`CONTEXT`] characters either side of `from..to`, snapped outward to a word boundary,
/// with an ellipsis wherever the line was cut.
#[must_use]
pub fn snippet(line: &str, from: usize, to: usize) -> (String, (usize, usize)) {
    let chars: Vec<char> = line.chars().collect();
    let mut start = from.saturating_sub(CONTEXT);
    while start > 0 && !chars[start - 1].is_whitespace() && start > from.saturating_sub(CONTEXT + 12) {
        start -= 1;
    }
    let mut end = (to + CONTEXT).min(chars.len());
    while end < chars.len() && !chars[end].is_whitespace() && end < to + CONTEXT + 12 {
        end += 1;
    }
    // Leading indentation is noise in a result row.
    while start < from && chars[start].is_whitespace() {
        start += 1;
    }
    let mut out = String::new();
    let mut offset = 0;
    // Only cut text earns an ellipsis; skipped indentation does not.
    if chars[..start].iter().any(|c| !c.is_whitespace()) {
        out.push('…');
        offset = 1;
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    (out, (from - start + offset, to - start + offset))
}

/// Replace `targets` in `rope`: each is checked against what the scan found, and only those
/// still there are replaced. Returns the edits and how many were refused.
#[must_use]
pub fn verified_edits(
    rope: &Rope,
    targets: &[FileMatch],
    replacement: &str,
) -> (Vec<(usize, usize, String)>, usize) {
    let mut edits = Vec::new();
    let mut refused = 0;
    for m in targets {
        let still = m.to <= rope.len_chars() && rope.slice(m.from..m.to) == m.found.as_str();
        if still {
            edits.push((m.from, m.to, replacement.to_owned()));
        } else {
            refused += 1;
        }
    }
    (edits, refused)
}

/// Replace matches in a file that is not open: read, verify, write atomically in its own encoding
/// and line endings. Returns how many were replaced and refused.
pub fn replace_on_disk(
    path: &Path,
    targets: &[FileMatch],
    replacement: &str,
    max_bytes: u64,
) -> Result<(usize, usize), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > max_bytes {
        return Err("the file is now larger than the editor's limit".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let decoded = text::decode(&bytes, LineEnding::Lf).map_err(|e| e.to_string())?;
    let mut rope = Rope::from_str(&decoded.text);
    let (edits, refused) = verified_edits(&rope, targets, replacement);
    if edits.is_empty() {
        return Ok((0, refused));
    }
    let count = edits.len();
    throng_editor::Transaction::replace(&rope, edits).apply(&mut rope);
    let out = text::encode(&rope.to_string(), &decoded.format);
    throng_platform::fs::atomic_write(path, &out).map_err(|e| e.to_string())?;
    Ok((count, refused))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: &str) -> Query {
        Query { term: t.into(), case_sensitive: false, whole_word: false }
    }

    #[test]
    fn snippets_keep_context_and_mark_cuts() {
        let line = format!("{} needle {}", "word ".repeat(20), "tail ".repeat(20));
        let at = line.find("needle").unwrap();
        let (text, (a, b)) = snippet(&line, at, at + 6);
        assert!(text.starts_with('…') && text.ends_with('…'), "{text}");
        assert_eq!(text.chars().skip(a).take(b - a).collect::<String>(), "needle");
        let (short, span) = snippet("    let x = needle;", 12, 18);
        assert_eq!(short, "let x = needle;");
        assert_eq!(span, (8, 14));
    }

    #[test]
    fn binary_large_and_undecodable_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("a.bin");
        std::fs::write(&bin, b"needle\0rest").unwrap();
        assert_eq!(search_one(&bin, &q("needle"), 1 << 20, 10), None);
        let big = dir.path().join("big.txt");
        std::fs::write(&big, b"needle").unwrap();
        assert_eq!(search_one(&big, &q("needle"), 3, 10), None);
        let latin = dir.path().join("l.txt");
        std::fs::write(&latin, b"needle caf\xe9").unwrap();
        assert_eq!(search_one(&latin, &q("needle"), 1 << 20, 10), None);
        let text = dir.path().join("t.txt");
        std::fs::write(&text, "one needle\r\ntwo NEEDLE\r\n").unwrap();
        let found = search_one(&text, &q("needle"), 1 << 20, 10).unwrap();
        assert_eq!(found.iter().map(|m| m.line).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn scopes_stay_inside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(root.join("src")).unwrap();
        assert!(resolve_scope(&root, "").is_ok());
        assert!(resolve_scope(&root, "./src/").is_ok());
        assert!(resolve_scope(&root, "src\\").is_ok());
        assert!(resolve_scope(&root, "nope").unwrap_err().contains("Could not find"));
        assert!(resolve_scope(&root, dir.path().to_str().unwrap()).unwrap_err().contains("outside"));
    }

    #[test]
    fn a_scan_streams_results_and_counts_what_it_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        // Reach the project through a symlink where the host allows one, as macOS's `/var` does:
        // the walk must not mix the canonical spelling with the given one.
        #[cfg(unix)]
        let root = {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            link
        };
        #[cfg(not(unix))]
        let root = real;
        std::fs::write(root.join("a.txt"), "needle\n").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/b.txt"), "no\nneedle needle\n").unwrap();
        std::fs::write(root.join("c.bin"), b"needle\0").unwrap();
        let scan = Scan::default();
        let spec = Spec {
            root: root.clone(),
            scope: String::new(),
            query: q("needle"),
            exclusions: Exclusions::default(),
            max_bytes: 1 << 20,
        };
        scan.start(spec, || {});
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = Vec::new();
        let mut done = None;
        while done.is_none() && Instant::now() < deadline {
            for update in scan.poll() {
                match update {
                    Update::Found(hits) => found.extend(hits),
                    other => done = Some(other),
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(done, Some(Update::Done { files: 3, skipped: 1, capped: false }));
        let counts: Vec<(String, usize)> = found.iter().map(|f| (f.rel.clone(), f.matches.len())).collect();
        assert_eq!(counts, vec![("a.txt".into(), 1), ("sub/b.txt".into(), 2)]);
    }

    #[test]
    fn replacing_on_disk_verifies_each_match_and_keeps_the_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "foo bar\r\nfoo\r\n").unwrap();
        let found = search_one(&path, &q("foo"), 1 << 20, 10).unwrap();
        // Someone changed the second line since the scan: that match is refused.
        std::fs::write(&path, "foo bar\r\nfob\r\n").unwrap();
        let (replaced, refused) = replace_on_disk(&path, &found, "qux", 1 << 20).unwrap();
        assert_eq!((replaced, refused), (1, 1));
        assert_eq!(std::fs::read(&path).unwrap(), b"qux bar\r\nfob\r\n");
    }
}
