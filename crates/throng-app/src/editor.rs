//! Editor panels and the documents behind them: one buffer per file, however many panels show it.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use egui::Ui;
use syntect::highlighting::Theme;
use throng_core::failure::{Operation, describe};
use throng_core::ids::PanelId;
use throng_core::paths::PathRules;
use throng_core::text::{self, Decoded, Indent, LineEnding, TextFormat};
use throng_editor::commands::{Clip, IndentStyle};
use throng_editor::highlight::Highlighter;
use throng_editor::{Applied, EditKind, Selection, TextDoc, Transaction, lang};

use crate::code::{self, CodeColours, CodeOutput, CodeStyle, CodeView};

/// What we last saw on disk: enough to tell our own write from someone else's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self { len: meta.len(), modified: meta.modified().ok() })
    }
}

/// How the file on disk relates to the buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disk {
    InSync,
    /// Someone else changed the file while the buffer had unsaved edits.
    Changed,
    Deleted,
}

/// A document: its text and history, how it is encoded on disk, and each panel's view of it.
pub struct Document {
    pub path: Option<PathBuf>,
    pub buf: TextDoc,
    pub format: TextFormat,
    pub disk: Disk,
    stamp: Option<Stamp>,
    pub indent: Option<Indent>,
    pub error: Option<String>,
    highlighter: Highlighter,
    /// A language chosen by hand; it belongs to the document, in every panel.
    language: Option<String>,
    /// Word wrap for this document; `None` follows the setting.
    pub wrap: Option<bool>,
    /// Each panel's carets, scroll and find session.
    pub views: HashMap<PanelId, CodeView>,
    /// The last counts, the version and time they were taken at.
    counts: Option<(u64, std::time::Instant, (usize, usize))>,
}

/// Why a file could not be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    TooLarge { size: u64, max: u64 },
    NotText(String),
    Io(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { size, max } => write!(
                f,
                "This file is {} MB, larger than the {} MB editor limit (editor.maxOpenFileBytes).",
                size / 1_048_576,
                max / 1_048_576
            ),
            Self::NotText(reason) | Self::Io(reason) => f.write_str(reason),
        }
    }
}

/// Read and decode a text file as an editor would, without opening it as a document (a
/// standalone preview reads its file this way).
pub fn read_text(path: &Path, max_bytes: u64) -> Result<String, OpenError> {
    read(path, max_bytes, LineEnding::Lf).map(|(decoded, _)| decoded.text)
}

fn read(path: &Path, max_bytes: u64, default_eol: LineEnding) -> Result<(Decoded, Option<Stamp>), OpenError> {
    let size = std::fs::metadata(path).map_err(|e| OpenError::Io(describe(&e, Operation::Read, path)))?.len();
    if size > max_bytes {
        return Err(OpenError::TooLarge { size, max: max_bytes });
    }
    let bytes = std::fs::read(path).map_err(|e| OpenError::Io(describe(&e, Operation::Read, path)))?;
    let decoded = text::decode(&bytes, default_eol).map_err(|e| OpenError::NotText(e.to_string()))?;
    Ok((decoded, Stamp::of(path)))
}

impl Document {
    /// A new, never-saved document.
    #[must_use]
    pub fn untitled(eol: LineEnding, theme: Arc<Theme>) -> Self {
        Self {
            path: None,
            buf: TextDoc::default(),
            format: TextFormat::new_document(eol),
            disk: Disk::InSync,
            stamp: None,
            indent: None,
            error: None,
            highlighter: Highlighter::new(lang::plain_text(), theme),
            language: None,
            wrap: None,
            views: HashMap::new(),
            counts: None,
        }
    }

    /// Open a file.
    pub fn open(
        path: &Path,
        max_bytes: u64,
        default_eol: LineEnding,
        theme: Arc<Theme>,
    ) -> Result<Self, OpenError> {
        let (Decoded { text, format }, stamp) = read(path, max_bytes, default_eol)?;
        let mut doc = Self::untitled(default_eol, theme);
        doc.indent = text::infer_indent(&text);
        doc.buf = TextDoc::new(&text);
        doc.format = format;
        doc.stamp = stamp;
        doc.path = Some(path.to_path_buf());
        doc.detect_language();
        Ok(doc)
    }

    /// The whole text (tests, previews, search).
    #[must_use]
    pub fn text(&self) -> String {
        self.buf.text()
    }

    /// The document's character and word counts, recounted at most every 150 ms
    /// while it changes, so they settle within 200 ms of the last edit and typing never
    /// waits for a count.
    pub fn counts(&mut self) -> (usize, usize) {
        let version = self.buf.version();
        match self.counts {
            Some((v, _, counts)) if v == version => counts,
            Some((_, at, counts)) if at.elapsed() < std::time::Duration::from_millis(150) => counts,
            _ => {
                let counts = throng_editor::lines::counts(self.buf.rope());
                self.counts = Some((version, std::time::Instant::now(), counts));
                counts
            }
        }
    }

    /// Unsaved: the text differs from the disk copy, or the file was deleted.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.buf.is_dirty() || self.disk == Disk::Deleted
    }

    /// Put a recovery record's text, history and choices back.
    pub fn restore(&mut self, record: crate::recovery::Record) {
        self.restore_unsaved(&record.text);
        if let Some(history) = record.history {
            self.buf.set_history(history);
        }
        if record.language.is_some() {
            self.set_language(record.language);
        }
        self.wrap = record.wrap;
    }

    /// The file-name label, e.g. `main.rs` or `Untitled`.
    #[must_use]
    pub fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map_or_else(|| "Untitled".to_owned(), |n| n.to_string_lossy().into_owned())
    }

    /// The language's display name ("Rust", "Plain Text").
    #[must_use]
    pub fn language(&self) -> &'static str {
        &self.highlighter.syntax().name
    }

    /// The hand-picked language, if any.
    #[must_use]
    pub fn language_override(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Choose the language by hand, or `None` to detect it from the file name again.
    pub fn set_language(&mut self, name: Option<String>) {
        self.language = name;
        self.detect_language();
    }

    fn detect_language(&mut self) {
        let syntax = self
            .language
            .as_deref()
            .and_then(lang::by_name)
            .unwrap_or_else(|| lang::detect(&self.name(), &BTreeMap::new()));
        self.highlighter.set_syntax(syntax);
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.highlighter.set_theme(theme);
    }

    /// Save to `path` (or the document's own path). The text written is captured before the write
    /// and is exactly what becomes "saved": an edit made meanwhile stays dirty.
    pub fn save_to(&mut self, path: Option<&Path>) -> Result<(), String> {
        let target =
            path.map(Path::to_path_buf).or_else(|| self.path.clone()).ok_or("Choose where to save.")?;
        let snapshot = self.buf.rope().clone();
        let bytes = text::encode(&snapshot.to_string(), &self.format);
        throng_platform::fs::atomic_write(&target, &bytes)
            .map_err(|e| describe(&e, Operation::Write, &target))?;
        // Re-derive the format from what is now on disk, so a mixed-ending file's per-line map
        // matches the new lines.
        if let Ok(decoded) = text::decode(&bytes, self.format.eol) {
            self.format = decoded.format;
        }
        self.buf.mark_saved(&snapshot);
        let renamed = self.path.as_deref() != Some(target.as_path());
        self.path = Some(target.clone());
        self.stamp = Stamp::of(&target);
        self.disk = Disk::InSync;
        self.error = None;
        if renamed {
            self.detect_language();
        }
        Ok(())
    }

    /// Compare with the disk after a change notification. A clean buffer follows the disk; a dirty
    /// one is never overwritten: it is marked so the user decides.
    pub fn check_disk(&mut self, max_bytes: u64) {
        let Some(path) = self.path.clone() else { return };
        let Some(stamp) = Stamp::of(&path) else {
            if path.exists() {
                return;
            }
            self.disk = Disk::Deleted;
            self.stamp = None;
            return;
        };
        if Some(stamp) == self.stamp && self.disk != Disk::Deleted {
            return;
        }
        let Ok((fresh, stamp)) = read(&path, max_bytes, self.format.eol) else { return };
        self.stamp = stamp;
        if *self.buf.saved() == fresh.text.as_str() {
            // Our own save, or a touch that changed nothing.
            self.disk =
                if self.disk == Disk::Deleted && self.buf.is_dirty() { Disk::Changed } else { Disk::InSync };
            if self.disk == Disk::InSync {
                self.format = fresh.format;
            }
            return;
        }
        // The buffer's own edits, not `is_dirty`: a deleted file counts as unsaved until it is back.
        if self.buf.is_dirty() {
            self.disk = Disk::Changed;
        } else {
            self.reload_from(fresh);
        }
    }

    /// Replace the buffer with the disk's content, discarding edits and history.
    pub fn reload(&mut self, max_bytes: u64) -> Result<(), String> {
        let path = self.path.clone().ok_or("This document has never been saved.")?;
        let (fresh, stamp) = read(&path, max_bytes, self.format.eol).map_err(|e| e.to_string())?;
        self.stamp = stamp;
        self.reload_from(fresh);
        Ok(())
    }

    fn reload_from(&mut self, fresh: Decoded) {
        self.buf.reset(&fresh.text);
        self.format = fresh.format;
        self.indent = text::infer_indent(&fresh.text).or(self.indent);
        self.disk = Disk::InSync;
        self.highlighter.reset();
        for view in self.views.values_mut() {
            view.reset(&self.buf);
        }
    }

    /// Put recovered unsaved text back (the disk copy stays what `saved` compares against).
    /// Indentation is inferred from the recovered text, not the stale disk copy.
    pub fn restore_unsaved(&mut self, text: &str) {
        self.buf.restore_unsaved(text);
        self.indent = text::infer_indent(text).or(self.indent);
        self.highlighter.reset();
        for view in self.views.values_mut() {
            view.reset(&self.buf);
        }
    }

    /// Keep the edits and stop warning: the next save overwrites the disk.
    pub fn keep_mine(&mut self) {
        if let Some(path) = &self.path {
            self.stamp = Stamp::of(path);
        }
        self.disk = Disk::InSync;
    }

    /// The file was moved or renamed inside throng: follow it, keeping buffer, dirty state and
    /// undo.
    pub fn moved_to(&mut self, path: PathBuf) {
        self.stamp = Stamp::of(&path);
        self.path = Some(path);
        self.detect_language();
    }

    /// Apply an edit made outside the widget (find's replace, a committed search result) as one
    /// undo step. `before` is the editing view's selection; every view in `views` follows.
    pub fn transact(&mut self, before: Selection, tx: Transaction) -> Option<Applied> {
        let after = before.map(&tx);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let applied = self.buf.edit(tx, before, after, EditKind::Other, now)?;
        self.after_edits(None, std::slice::from_ref(&applied));
        Some(applied)
    }

    /// Carry every view except `from` (which already follows) across `edits`.
    fn after_edits(&mut self, from: Option<PanelId>, edits: &[Applied]) {
        if let Some(first) = edits.iter().map(|a| a.first_line).min() {
            self.highlighter.invalidate_from(first);
        }
        for applied in edits {
            for (id, view) in &mut self.views {
                if Some(*id) != from {
                    view.follow(&self.buf, applied);
                }
            }
        }
    }

    /// The indentation this document uses.
    #[must_use]
    pub fn indent_style(&self, tab_size: usize) -> IndentStyle {
        match self.indent {
            Some(Indent::Tabs) => IndentStyle { tabs: true, width: tab_size },
            Some(Indent::Spaces(n)) => IndentStyle { tabs: false, width: n },
            None => IndentStyle { tabs: false, width: tab_size },
        }
    }
}

/// Where a document lives in the store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DocKey {
    File(String),
    Untitled(PanelId),
}

/// Every open document.
pub struct Documents {
    docs: HashMap<DocKey, Document>,
    /// The syntax colours every highlighter uses (previews' code blocks too).
    pub theme: Arc<Theme>,
    /// The last thing an editor copied, so pasting it back keeps its shape.
    pub clip: Option<Clip>,
    /// Hand-picked languages, saved against their files.
    pub languages: HashMap<DocKey, String>,
    /// Unsaved work recovered at launch, put back when its editor opens.
    pub recovered: HashMap<DocKey, crate::recovery::Record>,
}

impl Default for Documents {
    fn default() -> Self {
        Self {
            docs: HashMap::new(),
            theme: Arc::new(crate::theme::default_syntax_theme()),
            clip: None,
            languages: HashMap::new(),
            recovered: HashMap::new(),
        }
    }
}

impl Documents {
    /// The key for a path: the comparison key of its resolved location, so two spellings or a
    /// symlink never open two buffers of one file.
    #[must_use]
    pub fn key_for(rules: &PathRules, path: &Path) -> DocKey {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        DocKey::File(rules.key(&resolved))
    }

    /// The key an editor panel uses.
    #[must_use]
    pub fn panel_key(rules: &PathRules, panel: PanelId, path: Option<&Path>) -> DocKey {
        path.map_or(DocKey::Untitled(panel), |p| Self::key_for(rules, p))
    }

    /// Use `theme` for highlighting everywhere.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = Arc::new(theme);
        for doc in self.docs.values_mut() {
            doc.set_theme(Arc::clone(&self.theme));
        }
    }

    /// Open (or find) the document for a path.
    pub fn ensure_file(
        &mut self,
        rules: &PathRules,
        path: &Path,
        max_bytes: u64,
        eol: LineEnding,
    ) -> Result<DocKey, OpenError> {
        let key = Self::key_for(rules, path);
        if !self.docs.contains_key(&key) {
            let record = self.recovered.remove(&key);
            let mut doc = match (Document::open(path, max_bytes, eol, Arc::clone(&self.theme)), &record) {
                (Ok(doc), _) => doc,
                // The file went while throng was closed, but its unsaved text did not.
                (Err(_), Some(_)) if !path.exists() => {
                    let mut doc = Document::untitled(eol, Arc::clone(&self.theme));
                    doc.path = Some(path.to_path_buf());
                    doc.disk = Disk::Deleted;
                    doc.detect_language();
                    doc
                }
                (Err(e), _) => return Err(e),
            };
            if let Some(language) = self.languages.get(&key) {
                doc.set_language(Some(language.clone()));
            }
            if let Some(record) = record {
                doc.restore(record);
            }
            self.docs.insert(key.clone(), doc);
        }
        Ok(key)
    }

    pub fn ensure_untitled(&mut self, panel: PanelId, eol: LineEnding) -> DocKey {
        let key = DocKey::Untitled(panel);
        if !self.docs.contains_key(&key) {
            let mut doc = Document::untitled(eol, Arc::clone(&self.theme));
            if let Some(record) = self.recovered.remove(&key) {
                doc.restore(record);
            }
            self.docs.insert(key.clone(), doc);
        }
        key
    }

    /// The document for `key` together with the shared clipboard shape.
    pub fn doc_and_clip(&mut self, key: &DocKey) -> Option<(&mut Document, &mut Option<Clip>)> {
        let doc = self.docs.get_mut(key)?;
        Some((doc, &mut self.clip))
    }

    /// Put a document in the store (crash recovery).
    pub fn insert(&mut self, key: DocKey, doc: Document) {
        self.docs.insert(key, doc);
    }

    #[must_use]
    pub fn theme(&self) -> Arc<Theme> {
        Arc::clone(&self.theme)
    }

    #[must_use]
    pub fn get(&self, key: &DocKey) -> Option<&Document> {
        self.docs.get(key)
    }

    pub fn get_mut(&mut self, key: &DocKey) -> Option<&mut Document> {
        self.docs.get_mut(key)
    }

    /// Move a document to a new key (Save As on an untitled document, a rename).
    pub fn rekey(&mut self, from: &DocKey, to: DocKey) {
        if let Some(doc) = self.docs.remove(from) {
            self.docs.insert(to, doc);
        }
    }

    pub fn remove(&mut self, key: &DocKey) -> Option<Document> {
        self.docs.remove(key)
    }

    /// A panel stopped showing its document.
    pub fn drop_view(&mut self, key: &DocKey, panel: PanelId) {
        if let Some(doc) = self.docs.get_mut(key) {
            doc.views.remove(&panel);
        }
    }

    /// Forget `panel`'s caret and view in every document (a mirror that closed).
    pub fn forget_view(&mut self, panel: PanelId) {
        for doc in self.docs.values_mut() {
            doc.views.remove(&panel);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&DocKey, &Document)> {
        self.docs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&DocKey, &mut Document)> {
        self.docs.iter_mut()
    }

    /// Directories holding open files (to watch).
    #[must_use]
    pub fn watched_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .docs
            .values()
            .filter_map(|d| d.path.as_deref().and_then(Path::parent).map(Path::to_path_buf))
            .collect();
        dirs.sort();
        dirs.dedup();
        dirs
    }
}

/// How to draw editors.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorStyle {
    pub font_size: f32,
    pub word_wrap: bool,
    pub tab_size: usize,
    pub colours: CodeColours,
    pub links: bool,
    pub previewable: bool,
    /// The project a mirrored editor's document belongs to, named for assistive technology.
    pub from: Option<String>,
}

/// The id of `panel`'s editor widget (global, so focus can be given from anywhere).
#[must_use]
pub fn editor_id(panel: PanelId) -> egui::Id {
    egui::Id::new(("throng-editor", panel))
}

/// Show `panel`'s editor for `doc`.
pub fn show(
    ui: &mut Ui,
    panel: PanelId,
    doc: &mut Document,
    style: &EditorStyle,
    clip: &mut Option<Clip>,
) -> CodeOutput {
    let mut view = doc.views.remove(&panel).unwrap_or_default();
    let code_style = CodeStyle {
        font_size: style.font_size,
        tab: style.tab_size,
        wrap: doc.wrap.unwrap_or(style.word_wrap),
        indent: doc.indent_style(style.tab_size),
        colours: style.colours,
        label: match &style.from {
            Some(project) => format!("Editor: {}, from {project}", doc.name()),
            None => format!("Editor: {}", doc.name()),
        },
        links: style.links,
        previewable: style.previewable,
    };
    let out =
        code::show(ui, editor_id(panel), &mut doc.buf, &mut doc.highlighter, &mut view, &code_style, clip);
    doc.views.insert(panel, view);
    doc.after_edits(Some(panel), &out.edits);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Arc<Theme> {
        Arc::new(crate::theme::default_syntax_theme())
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn open(path: &Path) -> Document {
        Document::open(path, 1 << 20, LineEnding::Lf, theme()).unwrap()
    }

    /// Type at the end, as a panel would.
    fn append(doc: &mut Document, text: &str) {
        let end = doc.buf.len_chars();
        throng_editor::commands::insert_text(&mut doc.buf, &Selection::point(end), text, 0);
    }

    #[test]
    fn saving_keeps_the_files_format_and_marks_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.txt", b"\xEF\xBB\xBFone\r\ntwo\r\n");
        let mut doc = open(&path);
        assert!(!doc.is_dirty());
        append(&mut doc, "three\n");
        assert!(doc.is_dirty());
        doc.save_to(None).unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(std::fs::read(&path).unwrap(), b"\xEF\xBB\xBFone\r\ntwo\r\nthree\r\n");
    }

    #[test]
    fn undoing_back_to_the_saved_text_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = open(&write(dir.path(), "a.txt", b"hello\n"));
        append(&mut doc, "x");
        assert!(doc.is_dirty());
        doc.buf.undo();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn a_clean_buffer_follows_the_disk_and_a_dirty_one_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.txt", b"v1\n");
        let mut clean = open(&path);
        let mut dirty = open(&path);
        append(&mut dirty, "mine\n");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"v2 from elsewhere\n").unwrap();
        clean.check_disk(1 << 20);
        dirty.check_disk(1 << 20);
        assert_eq!(clean.text(), "v2 from elsewhere\n");
        assert_eq!(clean.disk, Disk::InSync);
        assert!(!clean.buf.history().can_undo(), "an external reload clears history");
        assert_eq!(dirty.text(), "v1\nmine\n");
        assert_eq!(dirty.disk, Disk::Changed);
        dirty.reload(1 << 20).unwrap();
        assert_eq!(dirty.text(), "v2 from elsewhere\n");
        assert!(!dirty.is_dirty());
    }

    #[test]
    fn our_own_save_is_not_an_external_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = open(&write(dir.path(), "a.txt", b"x\n"));
        append(&mut doc, "y\n");
        doc.save_to(None).unwrap();
        append(&mut doc, "typing after save\n");
        doc.check_disk(1 << 20);
        assert_eq!(doc.disk, Disk::InSync);
        assert!(doc.text().ends_with("typing after save\n"), "unsaved typing must survive the save echo");
        assert!(doc.is_dirty());
    }

    #[test]
    fn deletion_keeps_the_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.txt", b"keep me\n");
        let mut doc = open(&path);
        std::fs::remove_file(&path).unwrap();
        doc.check_disk(1 << 20);
        assert_eq!(doc.disk, Disk::Deleted);
        assert_eq!(doc.text(), "keep me\n");
        assert!(doc.is_dirty(), "a deleted file's editor has unsaved content");
        doc.save_to(None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me\n");
        assert_eq!(doc.disk, Disk::InSync);
        assert!(!doc.is_dirty());
    }

    #[test]
    fn a_deleted_file_that_comes_back_different_is_followed_when_the_buffer_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.txt", b"old\n");
        let mut doc = open(&path);
        std::fs::remove_file(&path).unwrap();
        doc.check_disk(1 << 20);
        std::fs::write(&path, b"new\n").unwrap();
        doc.check_disk(1 << 20);
        assert_eq!(doc.disk, Disk::InSync);
        assert_eq!(doc.text(), "new\n");
    }

    #[test]
    fn non_utf8_and_oversized_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let latin1 = write(dir.path(), "l.txt", b"caf\xE9\n");
        assert!(matches!(
            Document::open(&latin1, 1 << 20, LineEnding::Lf, theme()),
            Err(OpenError::NotText(_))
        ));
        let big = write(dir.path(), "b.txt", &vec![b'a'; 2048]);
        assert!(matches!(
            Document::open(&big, 1024, LineEnding::Lf, theme()),
            Err(OpenError::TooLarge { .. })
        ));
    }

    #[test]
    fn one_buffer_per_file_across_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.rs", b"fn main() {}\n");
        let rules = PathRules::LINUX;
        let mut docs = Documents::default();
        let a = docs.ensure_file(&rules, &path, 1 << 20, LineEnding::Lf).unwrap();
        let other_spelling = dir.path().join(".").join("a.rs");
        let b = docs.ensure_file(&rules, &other_spelling, 1 << 20, LineEnding::Lf).unwrap();
        assert_eq!(a, b);
        assert_eq!(docs.get(&a).unwrap().language(), "Rust");
    }

    #[test]
    fn an_edit_in_one_panel_carries_the_others_carets() {
        let mut doc = Document::untitled(LineEnding::Lf, theme());
        append(&mut doc, "hello world");
        let b = PanelId::new();
        doc.views.insert(b, CodeView::default());
        doc.views.get_mut(&b).unwrap().selection = Selection::point(6);
        let tx = Transaction::replace(doc.buf.rope(), vec![(0, 0, ">> ".into())]);
        doc.transact(Selection::point(0), tx);
        assert_eq!(doc.views[&b].selection, Selection::point(9), "b's caret stays before \"world\"");
    }

    #[test]
    fn a_hand_picked_language_wins_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = open(&write(dir.path(), "notes.txt", b"x\n"));
        assert_eq!(doc.language(), "Plain Text");
        doc.set_language(Some("Markdown".into()));
        assert_eq!(doc.language(), "Markdown");
        doc.set_language(None);
        assert_eq!(doc.language(), "Plain Text");
    }
}
