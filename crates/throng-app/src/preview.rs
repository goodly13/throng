//! Markdown previews: what a preview panel shows, how it follows its source, and how it is
//! drawn. A preview is bound to a file, never to an editor: while the file is open in an
//! editor it follows that document, unsaved edits included; otherwise it follows the disk
//! and reads the file without opening it as a document.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, CursorIcon, FontId, RichText, Sense, Stroke, Ui, vec2};
use syntect::highlighting::Theme;
use throng_core::paths::PathRules;
use throng_editor::highlight::Highlighter;

use crate::links::LinkAction;
use crate::markdown::{self, Block, Document, Inline, Item};

/// Whether a file has a preview: the Markdown provider accepts `.md` and `.markdown`.
#[must_use]
pub fn previewable(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// How often a standalone preview looks at its file on disk.
const DISK_CHECK: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Document(u64),
    Disk(Option<(u64, Option<SystemTime>)>),
}

/// One code block's highlighted layout, and what it was made from.
struct CodeCache {
    text: String,
    language: String,
    size: f32,
    job: LayoutJob,
}

/// A preview panel's state.
pub struct PreviewState {
    /// The file it shows.
    pub path: PathBuf,
    pub document: Option<Document>,
    shown: Option<Shown>,
    /// An unshown document version, when it was first seen, and when it last changed.
    pending: Option<(u64, Instant, Instant)>,
    checked: Option<Instant>,
    /// What is wrong, shown as one inline notice.
    pub problem: Option<String>,
    /// Scroll to this heading next frame (a `#heading` link).
    pub anchor: Option<String>,
    /// Re-read now, ignoring the delay (Refresh).
    pub refresh: bool,
    code: Vec<CodeCache>,
    theme_key: usize,
    /// Each image source's loadable address, or `None` when it stays alt text: worked out once per
    /// showing, since it asks the disk.
    images: HashMap<String, Option<String>>,
}

impl PreviewState {
    #[must_use]
    pub fn new(path: PathBuf, anchor: Option<String>) -> Self {
        Self {
            path,
            document: None,
            shown: None,
            pending: None,
            checked: None,
            problem: None,
            anchor,
            refresh: false,
            code: Vec::new(),
            theme_key: 0,
            images: HashMap::new(),
        }
    }

    fn show_text(&mut self, text: &str, shown: Shown) {
        self.document = Some(markdown::parse(text));
        self.images.clear();
        self.shown = Some(shown);
        self.pending = None;
        self.problem = None;
        self.refresh = false;
    }

    /// Follow the open document at `version`: shown once `delay` has passed since the last change,
    /// and no later than `wait` after the first change not yet shown. Returns
    /// how long until it should look again.
    pub fn follow_document(
        &mut self,
        version: u64,
        text: impl FnOnce() -> String,
        now: Instant,
        delay: Duration,
        wait: Duration,
    ) -> Option<Duration> {
        if self.shown == Some(Shown::Document(version)) && !self.refresh {
            self.pending = None;
            return None;
        }
        // The first showing, and Refresh, do not wait.
        let first_showing = !matches!(self.shown, Some(Shown::Document(_)));
        if first_showing || self.refresh {
            self.show_text(&text(), Shown::Document(version));
            return None;
        }
        let (first, last) = match self.pending {
            Some((v, first, last)) if v == version => (first, last),
            Some((_, first, _)) => (first, now),
            None => (now, now),
        };
        self.pending = Some((version, first, last));
        let quiet = now.duration_since(last);
        let waited = now.duration_since(first);
        if quiet >= delay || waited >= wait {
            self.show_text(&text(), Shown::Document(version));
            return None;
        }
        Some((delay - quiet).min(wait - waited))
    }

    /// Follow the file on disk, looking every [`DISK_CHECK`].
    pub fn follow_disk(&mut self, max_bytes: u64, now: Instant) -> Option<Duration> {
        let due = self.refresh
            || !matches!(self.shown, Some(Shown::Disk(_)))
            || self.checked.is_none_or(|at| now.duration_since(at) >= DISK_CHECK);
        if !due {
            return Some(DISK_CHECK);
        }
        self.checked = Some(now);
        let stamp = std::fs::metadata(&self.path).ok().map(|m| (m.len(), m.modified().ok()));
        if self.shown == Some(Shown::Disk(stamp)) && !self.refresh {
            return Some(DISK_CHECK);
        }
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match crate::editor::read_text(&self.path, max_bytes) {
            Ok(text) => self.show_text(&text, Shown::Disk(stamp)),
            Err(_) if stamp.is_none() => {
                self.problem = Some(format!("\"{name}\" no longer exists."));
                self.shown = Some(Shown::Disk(None));
                self.refresh = false;
            }
            Err(e) => {
                self.problem = Some(format!("\"{name}\" cannot be previewed: {e}"));
                self.shown = Some(Shown::Disk(stamp));
                self.refresh = false;
            }
        }
        Some(DISK_CHECK)
    }
}

/// How a preview draws.
pub struct PreviewStyle {
    pub body_size: f32,
    pub code_size: f32,
    /// The editor's syntax colours, for code blocks.
    pub theme: Arc<Theme>,
    pub link: Color32,
    pub images: ImagePolicy,
}

/// Where a preview's images may come from.
#[derive(Clone, Debug)]
pub struct ImagePolicy {
    /// The project the previewed file belongs to: a local image loads only from inside it.
    pub root: Option<PathBuf>,
    pub rules: PathRules,
    /// Whether `https:` images load (`editor.previews.loadRemoteImages`). No other remote image
    /// ever does.
    pub remote: bool,
}

/// The largest local image a preview decodes.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

impl ImagePolicy {
    /// The address an image written as `src` in a document in `dir` loads from, or `None` when it
    /// stays alt text: an `https:` image while remote images are on, or a file inside the project
    /// (resolved against the document's folder, links followed) no larger than
    /// [`MAX_IMAGE_BYTES`]. `http:`, `data:`, `file:` and every other scheme never load.
    #[must_use]
    pub fn address(&self, src: &str, dir: Option<&Path>) -> Option<String> {
        let src = src.trim();
        let lower = src.to_ascii_lowercase();
        if lower.starts_with("https://") {
            return self.remote.then(|| src.to_owned());
        }
        let scheme = lower.split_once(':').is_some_and(|(s, _)| {
            s.len() > 1 && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        });
        if src.is_empty() || scheme {
            return None;
        }
        let written = src.replace("%20", " ");
        let written = Path::new(&written);
        let path = if written.is_absolute() { written.to_path_buf() } else { dir?.join(written) };
        let path = std::fs::canonicalize(path).ok()?;
        let root = std::fs::canonicalize(self.root.as_ref()?).ok()?;
        let meta = std::fs::metadata(&path).ok()?;
        (self.rules.is_within(&root, &path) && meta.is_file() && meta.len() <= MAX_IMAGE_BYTES)
            .then(|| format!("file://{}", path.display()))
    }
}

/// A link the reader followed or asked about: its written target and what they asked.
pub type LinkRequest = (String, LinkAction);

struct Render<'a> {
    style: &'a PreviewStyle,
    /// The previewed file's folder, which relative images are read from.
    dir: Option<&'a Path>,
    images: &'a mut HashMap<String, Option<String>>,
    code: &'a mut Vec<CodeCache>,
    code_index: usize,
    anchor: Option<String>,
    links: Vec<LinkRequest>,
}

/// Draw the document; returns the links the reader followed or used a menu on.
pub fn show(ui: &mut Ui, state: &mut PreviewState, style: &PreviewStyle) -> Vec<LinkRequest> {
    let theme_key = Arc::as_ptr(&style.theme) as usize;
    if state.theme_key != theme_key {
        state.code.clear();
        state.theme_key = theme_key;
    }
    let Some(document) = state.document.as_ref() else { return Vec::new() };
    let mut render = Render {
        style,
        dir: state.path.parent(),
        images: &mut state.images,
        code: &mut state.code,
        code_index: 0,
        anchor: state.anchor.take(),
        links: Vec::new(),
    };
    ui.spacing_mut().item_spacing.y = 6.0;
    render.blocks(ui, &document.blocks);
    render.links
}

impl Render<'_> {
    fn blocks(&mut self, ui: &mut Ui, blocks: &[Block]) {
        for block in blocks {
            self.block(ui, block);
        }
    }

    fn block(&mut self, ui: &mut Ui, block: &Block) {
        match block {
            Block::Heading { level, inlines, anchor } => {
                let scale = match level {
                    1 => 1.9,
                    2 => 1.55,
                    3 => 1.3,
                    4 => 1.12,
                    _ => 1.0,
                };
                ui.add_space(if *level <= 2 { 8.0 } else { 4.0 });
                let response = self.inlines(ui, inlines, self.style.body_size * scale, true);
                if *level <= 2 {
                    ui.separator();
                }
                if self.anchor.as_deref() == Some(anchor.as_str()) {
                    response.scroll_to_me(Some(egui::Align::TOP));
                }
            }
            Block::Paragraph(inlines) => {
                self.inlines(ui, inlines, self.style.body_size, false);
            }
            Block::List { start, items } => self.list(ui, *start, items),
            Block::Quote(blocks) => {
                let bar = ui.visuals().weak_text_color();
                let response = egui::Frame::new()
                    .inner_margin(egui::Margin { left: 12, right: 0, top: 2, bottom: 2 })
                    .show(ui, |ui| self.blocks(ui, blocks))
                    .response;
                ui.painter().vline(
                    response.rect.left() + 3.0,
                    response.rect.y_range(),
                    Stroke::new(3.0, bar),
                );
            }
            Block::Code { language, text } => self.code(ui, language, text),
            Block::Table { head, rows } => {
                let id = ui.id().with(("table", self.code_index, rows.len()));
                egui::Frame::new().inner_margin(2).show(ui, |ui| {
                    egui::Grid::new(id).striped(true).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                        for cell in head {
                            self.inlines(ui, cell, self.style.body_size, true);
                        }
                        ui.end_row();
                        for row in rows {
                            for cell in row {
                                self.inlines(ui, cell, self.style.body_size, false);
                            }
                            ui.end_row();
                        }
                    });
                });
            }
            Block::Rule => {
                ui.separator();
            }
            Block::FrontMatter(pairs) => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::Grid::new(ui.id().with("front-matter")).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                        for (key, value) in pairs {
                            ui.label(RichText::new(key).strong());
                            ui.label(RichText::new(value).monospace());
                            ui.end_row();
                        }
                    });
                });
            }
        }
    }

    fn list(&mut self, ui: &mut Ui, start: Option<u64>, items: &[Item]) {
        for (i, item) in items.iter().enumerate() {
            ui.horizontal_top(|ui| {
                ui.add_space(6.0);
                let marker_w = 22.0;
                match (item.task, start) {
                    (Some(done), _) => {
                        let mut done = done;
                        // Read-only.
                        ui.add_enabled(false, egui::Checkbox::without_text(&mut done));
                    }
                    (None, Some(n)) => {
                        ui.add_sized(vec2(marker_w, 0.0), egui::Label::new(format!("{}.", n + i as u64)));
                    }
                    (None, None) => {
                        ui.add_sized(vec2(marker_w * 0.6, 0.0), egui::Label::new("•"));
                    }
                }
                ui.vertical(|ui| self.blocks(ui, &item.blocks));
            });
        }
    }

    fn code(&mut self, ui: &mut Ui, language: &str, text: &str) {
        let index = self.code_index;
        self.code_index += 1;
        let size = self.style.code_size;
        let stale =
            self.code.get(index).is_none_or(|c| c.text != text || c.language != language || c.size != size);
        if stale {
            let job = highlight(text, language, size, &self.style.theme, ui.visuals().text_color());
            let entry = CodeCache { text: text.to_owned(), language: language.to_owned(), size, job };
            if index < self.code.len() {
                self.code[index] = entry;
            } else {
                self.code.push(entry);
            }
        }
        let job = self.code[index].job.clone();
        egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(4).inner_margin(8).show(
            ui,
            |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::horizontal().id_salt(("preview-code", index)).show(ui, |ui| {
                    ui.add(egui::Label::new(job).extend());
                });
            },
        );
    }

    /// A run of inline content, wrapped. `strong` sets it all bold (a heading, a table header).
    fn inlines(&mut self, ui: &mut Ui, inlines: &[Inline], size: f32, strong: bool) -> egui::Response {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for inline in inlines {
                match inline {
                    Inline::Text { text, style, link } => {
                        let mut rich =
                            RichText::new(text).size(if style.sup || style.sub { size * 0.75 } else { size });
                        if strong || style.strong {
                            rich = rich.strong();
                        }
                        if style.emphasis {
                            rich = rich.italics();
                        }
                        if style.strike {
                            rich = rich.strikethrough();
                        }
                        if style.code || style.kbd {
                            rich = rich.code();
                        }
                        if style.sup {
                            rich = rich.raised();
                        }
                        match link {
                            Some(href) => self.link(ui, rich, href),
                            None => {
                                ui.add(egui::Label::new(rich).wrap());
                            }
                        }
                    }
                    Inline::Image { alt, src, title, link } => {
                        self.image(ui, (alt, src, title), size, link.as_deref());
                    }
                    Inline::Break => ui.end_row(),
                }
            }
        })
        .response
    }

    /// An image, drawn when it loads and shown as its alternative text when it cannot. Its tooltip
    /// names its source as written, and its title; inside a link it is the link, and the link's
    /// target is what its tooltip names.
    fn image(&mut self, ui: &mut Ui, (alt, src, title): (&str, &str, &str), size: f32, link: Option<&str>) {
        let address = self
            .images
            .entry(src.to_owned())
            .or_insert_with(|| self.style.images.address(src, self.dir))
            .clone();
        let width = ui.available_width().max(16.0);
        let image = address
            .map(|uri| egui::Image::new(uri).max_width(width).fit_to_original_size(1.0).alt_text(alt))
            .filter(|image| image.load_for_size(ui.ctx(), vec2(width, f32::INFINITY)).is_ok());
        let response = match image {
            Some(image) => ui.add(image.sense(Sense::click())),
            None => {
                let label = if alt.is_empty() { format!("[image: {src}]") } else { format!("[{alt}]") };
                let mut rich = RichText::new(label).italics().size(size);
                if link.is_some() {
                    rich = rich.color(self.style.link).underline();
                }
                ui.add(egui::Label::new(rich).sense(Sense::click()))
            }
        };
        match link {
            Some(href) => self.link_response(ui, &response, href),
            None => {
                let tip = if title.is_empty() { src.to_owned() } else { format!("{src}\n{title}") };
                response.on_hover_text(tip);
            }
        }
    }

    /// A link: Ctrl+click (Cmd+click) follows it; a plain click does nothing; right-click
    /// offers Open Link and Copy Link Address.
    fn link(&mut self, ui: &mut Ui, rich: RichText, href: &str) {
        let response =
            ui.add(egui::Label::new(rich.color(self.style.link).underline()).sense(Sense::click()));
        self.link_response(ui, &response, href);
    }

    fn link_response(&mut self, ui: &mut Ui, response: &egui::Response, href: &str) {
        let response = response.clone();
        let command = ui.input(|i| i.modifiers.command);
        if response.hovered() && command {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let chord = if cfg!(target_os = "macos") { "Cmd+click" } else { "Ctrl+click" };
        let response = response.on_hover_text_at_pointer(format!("{href}\n{chord} to follow"));
        if response.clicked() && command {
            self.links.push((href.to_owned(), LinkAction::Follow));
        }
        response.context_menu(|ui| {
            if ui.button("Open Link").clicked() {
                self.links.push((href.to_owned(), LinkAction::Follow));
                ui.close();
            }
            if ui.button("Copy Link Address").clicked() {
                self.links.push((href.to_owned(), LinkAction::Copy));
                ui.close();
            }
        });
    }
}

/// A code block's text, highlighted with the editor's syntax colours when its language is known.
fn highlight(text: &str, language: &str, size: f32, theme: &Arc<Theme>, plain: Color32) -> LayoutJob {
    let font = FontId::monospace(size);
    let mut job = LayoutJob::default();
    let syntax = (!language.is_empty())
        .then(|| {
            throng_editor::lang::by_name(language)
                .or_else(|| throng_editor::highlight::syntax_set().find_syntax_by_token(language))
        })
        .flatten();
    let Some(syntax) = syntax else {
        job.append(text, 0.0, TextFormat { font_id: font, color: plain, ..TextFormat::default() });
        return job;
    };
    let rope = ropey::Rope::from_str(text);
    let mut highlighter = Highlighter::new(syntax, Arc::clone(theme));
    let lines = rope.len_lines();
    let spans = highlighter.spans(&rope, 0..lines, Instant::now() + Duration::from_millis(50));
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            job.append(
                "\n",
                0.0,
                TextFormat { font_id: font.clone(), color: plain, ..TextFormat::default() },
            );
        }
        let line_spans = spans.get(i).cloned().flatten();
        let mut at = 0;
        if let Some(line_spans) = line_spans {
            for span in line_spans.iter() {
                let (start, end) = (span.start.min(line.len()), span.end.min(line.len()));
                if start > at {
                    job.append(
                        &line[at..start],
                        0.0,
                        TextFormat { font_id: font.clone(), color: plain, ..TextFormat::default() },
                    );
                }
                if end > start && line.is_char_boundary(start) && line.is_char_boundary(end) {
                    let [r, g, b, a] = span.style.fg;
                    let colour = Color32::from_rgba_unmultiplied(r, g, b, a);
                    job.append(
                        &line[start..end],
                        0.0,
                        TextFormat {
                            font_id: font.clone(),
                            color: colour,
                            italics: span.style.italic,
                            ..TextFormat::default()
                        },
                    );
                    at = end;
                }
            }
        }
        if at < line.len() {
            job.append(
                &line[at..],
                0.0,
                TextFormat { font_id: font.clone(), color: plain, ..TextFormat::default() },
            );
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_load_from_inside_the_project_and_https_only_while_remote_images_are_on() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(root.join("docs/img")).unwrap();
        std::fs::write(root.join("docs/img/a b.png"), b"png").unwrap();
        std::fs::write(dir.path().join("outside.png"), b"png").unwrap();
        let docs = root.join("docs");
        let mut policy =
            ImagePolicy { root: Some(root.clone()), rules: throng_platform::path_rules(), remote: true };
        let local = |p: &Path| format!("file://{}", std::fs::canonicalize(p).unwrap().display());
        assert_eq!(policy.address("img/a b.png", Some(&docs)), Some(local(&root.join("docs/img/a b.png"))));
        assert_eq!(policy.address("img/a%20b.png", Some(&docs)), Some(local(&root.join("docs/img/a b.png"))));
        assert_eq!(policy.address("../../outside.png", Some(&docs)), None, "outside the project");
        assert_eq!(policy.address("img/missing.png", Some(&docs)), None);
        assert_eq!(policy.address("https://x.dev/b.svg", None).as_deref(), Some("https://x.dev/b.svg"));
        for never in ["http://x.dev/a.png", "data:image/png;base64,AA", "file:///etc/x.png", "ftp://x/a.png"]
        {
            assert_eq!(policy.address(never, Some(&docs)), None, "{never}");
        }
        policy.remote = false;
        assert_eq!(policy.address("https://x.dev/b.svg", None), None, "remote images off");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("outside.png"), root.join("docs/sneaky.png")).unwrap();
            assert_eq!(policy.address("sneaky.png", Some(&docs)), None, "a link out of the project");
        }
        let unbound = ImagePolicy { root: None, ..policy };
        assert_eq!(unbound.address("img/a b.png", Some(&docs)), None, "no project, no local images");
    }

    #[test]
    fn a_parented_preview_waits_for_typing_to_settle_but_never_longer_than_the_maximum() {
        let mut state = PreviewState::new("a.md".into(), None);
        let t0 = Instant::now();
        let (delay, wait) = (Duration::from_millis(300), Duration::from_millis(1000));
        assert_eq!(state.follow_document(1, || "# one".into(), t0, delay, wait), None, "shown at once");
        assert!(state.document.is_some());
        // Typing every 200 ms: nothing shows until the maximum wait has passed.
        let mut shown_at = None;
        for step in 1..=8u64 {
            let now = t0 + Duration::from_millis(200 * step);
            state.follow_document(1 + step, || format!("# v{step}"), now, delay, wait);
            if shown_at.is_none() && state.shown == Some(Shown::Document(1 + step)) {
                shown_at = Some(step);
            }
        }
        assert_eq!(shown_at, Some(6), "at 1200 ms, the first check past the 1000 ms wait");
        // Once typing stops, the delay shows the rest.
        let later = t0 + Duration::from_millis(1600 + 300);
        state.follow_document(9, || "# last".into(), later, delay, wait);
        assert_eq!(state.follow_document(9, || "# last".into(), later + delay, delay, wait), None);
        assert_eq!(state.shown, Some(Shown::Document(9)));
    }

    #[test]
    fn a_standalone_preview_follows_the_disk_and_reports_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "# Hello").unwrap();
        let mut state = PreviewState::new(path.clone(), None);
        let t0 = Instant::now();
        state.follow_disk(1 << 20, t0);
        assert!(
            matches!(&state.document.as_ref().unwrap().blocks[0], Block::Heading { anchor, .. } if anchor == "hello")
        );
        std::fs::remove_file(&path).unwrap();
        state.follow_disk(1 << 20, t0 + DISK_CHECK);
        assert_eq!(state.problem.as_deref(), Some("\"a.md\" no longer exists."));
    }
}
