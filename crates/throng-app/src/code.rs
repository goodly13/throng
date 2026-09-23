//! The code editor widget: draws a `throng-editor` document and turns input into its commands.
//!
//! Only the visible rows are laid out and highlighted, so a large file costs what the screen
//! shows. Caret, selection and hit-testing positions come from each row's laid-out
//! glyphs, so wide characters and fallback fonts line up; display columns are used only for
//! wrapping and for the column vertical moves aim at.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{
    Color32, CursorIcon, Event, EventFilter, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, Vec2,
    pos2, vec2,
};
use throng_core::keymap::Scope;
use throng_core::links::{Link, Target};
use throng_editor::commands::{self, Clip, ClipMode, Edited, IndentStyle, Unit};
use throng_editor::find;
use throng_editor::highlight::{Highlighter, Span};
use throng_editor::{Applied, Range, Selection, TextDoc, Wrap, lines};

use crate::links::LinkAction;

/// How long highlighting may take per frame before lines draw plain and fill in later.
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(6);
const BLINK: Duration = Duration::from_millis(530);
const PAD_X: f32 = 6.0;
/// Room the scroll bars take.
const BAR: f32 = 14.0;

/// Colours an editor draws with (the Editor theme tokens).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CodeColours {
    pub background: Color32,
    pub foreground: Color32,
    pub gutter_bg: Color32,
    pub gutter_fg: Color32,
    pub gutter_active: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub current_line: Color32,
    pub search_match: Color32,
    pub search_current: Color32,
    pub search_current_border: Color32,
    /// Link underlines: drawn faint at rest, solid under the pointer.
    pub link: Color32,
}

/// How to draw and edit.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeStyle {
    pub font_size: f32,
    pub tab: usize,
    pub wrap: bool,
    pub indent: IndentStyle,
    pub colours: CodeColours,
    /// A label for assistive technology ("Editor: main.rs").
    pub label: String,
    /// Find links in the text (the `editor.links.detectInEditors` setting).
    pub links: bool,
    /// The file has a preview: the menu offers Open Preview.
    pub previewable: bool,
}

/// Find state for one panel (sessions are per panel).
#[derive(Clone, Debug, Default)]
pub struct FindSession {
    pub query: find::Query,
    pub replacement: String,
    pub replace_open: bool,
    pub matches: Vec<(usize, usize)>,
    pub current: Option<usize>,
    /// The document version and query the matches were computed for.
    computed: Option<(u64, find::Query)>,
    /// When the term last changed (as-you-type debounce).
    pub edited_at: Option<Instant>,
    /// Put the current match at the caret (or `anchor`) on the next refresh.
    pub reseat: bool,
    pub anchor: Option<usize>,
    /// Focus the find (or replace) input on the next frame.
    pub focus_input: bool,
    pub focus_replace: bool,
}

impl FindSession {
    /// Recompute matches if the text or query changed and the debounce has passed. Returns how
    /// long to wait when still debouncing.
    pub fn refresh(&mut self, buf: &TextDoc, caret: usize, debounce: Duration) -> Option<Duration> {
        if let Some(at) = self.edited_at {
            let elapsed = at.elapsed();
            if elapsed < debounce {
                return Some(debounce - elapsed);
            }
            self.edited_at = None;
            self.reseat = true;
        }
        let key = (buf.version(), self.query.clone());
        if self.computed.as_ref() == Some(&key) && !self.reseat {
            return None;
        }
        let previous = self.current_match();
        self.matches = find::find_all(buf.rope(), &self.query, usize::MAX);
        let from =
            if self.reseat { self.anchor.take().unwrap_or(caret) } else { previous.map_or(caret, |m| m.0) };
        self.current = find::first_from(&self.matches, from);
        self.reseat = false;
        self.computed = Some(key);
        None
    }

    /// Step to the next or previous match, wrapping.
    pub fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.current = Some(match self.current {
            None => 0,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        });
    }

    #[must_use]
    pub fn current_match(&self) -> Option<(usize, usize)> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }
}

/// One panel's view of a document.
#[derive(Debug, Default)]
pub struct CodeView {
    pub selection: Selection,
    wrap: Option<Wrap>,
    /// The document version `wrap` reflects.
    wrap_version: u64,
    /// The widest line, in display columns (for the horizontal scroll range).
    widest: usize,
    /// The selection is a column block (for copy and paste).
    pub column: bool,
    /// Column selection's corners (line, display column).
    column_anchor: Option<(usize, usize)>,
    column_head: Option<(usize, usize)>,
    drag: Option<Drag>,
    pub scroll_to_caret: bool,
    /// Where the view was scrolled last frame.
    scroll: Vec2,
    preedit: Option<String>,
    activity: Option<Instant>,
    pub find: Option<FindSession>,
    /// Bring this position into view without moving the caret (a find match).
    pub reveal: Option<usize>,
    /// The link the context menu was opened on.
    menu_link: Option<Target>,
}

/// A selection being made with the mouse.
#[derive(Clone, Copy, Debug)]
struct Drag {
    /// Where the press landed.
    anchor: usize,
    /// For a column drag (Alt), the rectangle's fixed corner.
    column: Option<(usize, usize)>,
}

impl CodeView {
    /// Carry this view across an edit another view (or replace-all) made.
    pub fn follow(&mut self, buf: &TextDoc, applied: &Applied) {
        self.selection = self.selection.map(&applied.tx).clamp(buf.len_chars());
        match &mut self.wrap {
            Some(wrap) if self.wrap_version + 1 == applied.version && applied.version == buf.version() => {
                wrap.apply(buf.rope(), applied);
                self.wrap_version = applied.version;
                self.widest =
                    self.widest.max(widest_in(buf, applied.first_line..=applied.last_line, wrap.tab()));
            }
            _ => self.wrap = None,
        }
    }

    /// The text was replaced wholesale (reload): keep carets in range, rebuild layout.
    pub fn reset(&mut self, buf: &TextDoc) {
        self.selection = self.selection.clamp(buf.len_chars());
        self.wrap = None;
        self.column = false;
        self.drag = None;
    }

    /// Put the caret at `pos` and bring it into view.
    pub fn set_caret(&mut self, pos: usize) {
        self.select(pos, pos);
    }

    /// Select `from..to` and bring it into view.
    pub fn select(&mut self, from: usize, to: usize) {
        self.selection = Selection::single(from, to);
        self.column = false;
        self.scroll_to_caret = true;
        self.activity = Some(Instant::now());
    }

    fn touch(&mut self) {
        self.activity = Some(Instant::now());
        self.scroll_to_caret = true;
    }
}

fn widest_in(buf: &TextDoc, lines_range: std::ops::RangeInclusive<usize>, tab: usize) -> usize {
    let rope = buf.rope();
    let last = rope.len_lines().saturating_sub(1);
    let (a, b) = (*lines_range.start(), (*lines_range.end()).min(last));
    (a..=b)
        .map(|l| rope.line(l).chars().map(|c| if c == '\t' { tab.max(1) } else { 1 }).sum::<usize>())
        .max()
        .unwrap_or(0)
}

/// What a frame of the editor asked for.
#[derive(Debug, Default)]
pub struct CodeOutput {
    pub focused: bool,
    pub save: bool,
    /// Edits made, in order, for the caller to carry other views across.
    pub edits: Vec<Applied>,
    /// Text for the system clipboard.
    pub copy: Option<String>,
    /// 1-based line, and 1-based column in UTF-16 units.
    pub caret: (usize, usize),
    pub selected_chars: usize,
    pub carets: usize,
    /// Open find (`false`) or find-and-replace (`true`).
    pub open_find: Option<bool>,
    pub goto_line: bool,
    pub toggle_wrap: bool,
    pub set_language: bool,
    /// A link to follow, copy or reveal.
    pub link: Option<(Target, LinkAction)>,
    pub open_preview: bool,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn mac() -> bool {
    cfg!(target_os = "macos")
}

/// Metrics of the monospace font at the style's size.
struct Metrics {
    font: FontId,
    row_h: f32,
    char_w: f32,
}

/// One laid-out visual row.
struct RowLayout {
    line: usize,
    /// The characters of the line this row shows.
    start: usize,
    end: usize,
    /// `display[i]` is the galley character index of the row's `i`th buffer character (tabs expand
    /// to several spaces); one extra entry for the end.
    display: Vec<usize>,
    galley: Arc<egui::Galley>,
    /// Screen position of the row's text origin.
    origin: Pos2,
    last_of_line: bool,
}

impl RowLayout {
    fn x_of(&self, index_in_row: usize) -> f32 {
        let i = self.display[index_in_row.min(self.display.len() - 1)];
        self.origin.x + self.galley.pos_from_cursor(CCursor::new(i)).min.x
    }

    fn index_at_x(&self, x: f32) -> usize {
        let display = self.galley.cursor_from_pos(vec2(x - self.origin.x, 0.0)).index.0;
        match self.display.binary_search(&display) {
            Ok(i) => i,
            Err(i) => i.min(self.display.len() - 1),
        }
    }
}

/// Where the scrolled content sits on screen this frame.
struct Frame {
    rows: Vec<RowLayout>,
    /// Screen position of the content's top-left (row 0, before padding).
    origin: Pos2,
    visible: Rect,
}

/// Show the editor. `clip` is the last thing this app copied, so pasting the same text keeps its
/// full-line or column shape.
pub fn show(
    ui: &mut Ui,
    id: Id,
    buf: &mut TextDoc,
    highlighter: &mut Highlighter,
    view: &mut CodeView,
    style: &CodeStyle,
    clip: &mut Option<Clip>,
) -> CodeOutput {
    let mut out = CodeOutput::default();
    let outer = ui.available_rect_before_wrap();
    ui.allocate_rect(outer, Sense::hover());
    let metrics = {
        let font = FontId::monospace(style.font_size);
        let (row_h, char_w) = ui.ctx().fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
        Metrics { font, row_h, char_w }
    };
    let c = style.colours;
    let digits = buf.len_lines().to_string().len().max(2);
    let gutter = Rect::from_min_size(outer.min, vec2(metrics.char_w * digits as f32 + 18.0, outer.height()));
    let text_rect = Rect::from_min_max(pos2(gutter.right(), outer.top()), outer.max);
    ui.painter().rect_filled(outer, 0.0, c.background);
    ui.painter().rect_filled(gutter, 0.0, c.gutter_bg);

    // The wrap cache follows the text area's width and the document's version.
    let wrap_cols = style
        .wrap
        .then(|| ((text_rect.width() - 2.0 * PAD_X - BAR) / metrics.char_w).floor().max(8.0) as usize);
    let mut wrap = match view.wrap.take() {
        Some(mut w) if view.wrap_version == buf.version() => {
            w.set_width(buf.rope(), wrap_cols, style.tab);
            w
        }
        _ => {
            view.wrap_version = buf.version();
            view.widest = widest_in(buf, 0..=usize::MAX, style.tab);
            Wrap::new(buf.rope(), wrap_cols, style.tab)
        }
    };

    let focused = ui.memory(|m| m.has_focus(id));
    out.focused = focused;
    let mut ctx_repaint = false;
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false },
            );
            // The lock takes effect from the second focused frame; request it now, or a Tab typed
            // straight after clicking in would move focus away instead of indenting.
            if !m.had_focus_last_frame(id) {
                ctx_repaint = true;
            }
        });
        if ctx_repaint {
            ui.ctx().request_repaint();
        }
        let page = ((text_rect.height() / metrics.row_h).floor() as isize - 1).max(1);
        keyboard(ui, buf, highlighter, view, &mut wrap, style, clip, page, &mut out);
    }

    let offset = scroll_target(buf, view, &mut wrap, &metrics, style, text_rect);
    let content = vec2(
        if style.wrap {
            text_rect.width() - BAR
        } else {
            view.widest as f32 * metrics.char_w + 2.0 * PAD_X + metrics.char_w * 8.0
        },
        wrap.total_rows() as f32 * metrics.row_h + text_rect.height() * 0.5,
    );
    // Wrapped text never scrolls sideways.
    let area = if style.wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
    let mut area = area.id_salt(("code-scroll", id)).auto_shrink([false, false]);
    if let Some(offset) = offset {
        area = area.scroll_offset(offset);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
    let scrolled = area.show_viewport(&mut child, |ui, viewport| {
        ui.set_min_size(content);
        let origin = ui.min_rect().min;
        let visible = viewport.translate(origin.to_vec2());
        let response = ui.interact(visible, id, Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, &style.label));
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(egui::accesskit::Role::MultilineTextInput);
            node.set_label(style.label.clone());
        });
        let rows = layout_rows(ui, buf, highlighter, &mut wrap, &metrics, style, origin, viewport);
        (response, Frame { rows, origin, visible })
    });
    view.scroll = scrolled.state.offset;
    let (response, frame) = scrolled.inner;
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    if response.clicked() || response.drag_started() || response.secondary_clicked() {
        response.request_focus();
    }

    let links = if style.links { visible_links(buf, &frame) } else { Vec::new() };
    let hovered = response.hover_pos().and_then(|p| link_at(&frame, &links, p, metrics.row_h));
    // Ctrl, or Cmd on macOS.
    let command = ui.input(|i| i.modifiers.command);
    if let Some((_, link)) = hovered {
        if command {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response.clone().on_hover_text_at_pointer(crate::links::hover_text(&link.target));
    }
    // Ctrl+click (Cmd+click) on a link follows it; off a link it adds a caret.
    if let Some((_, link)) = hovered.filter(|_| command && response.clicked_by(egui::PointerButton::Primary))
    {
        out.link = Some((link.target.clone(), LinkAction::Follow));
    } else {
        mouse(ui, buf, view, &mut wrap, &response, &frame, &metrics, style);
    }
    if response.secondary_clicked() {
        // The link menu is for a right-click with nothing selected.
        view.menu_link = hovered.filter(|_| view.selection.all_empty()).map(|(_, l)| l.target.clone());
    }
    context_menu(&response, buf, highlighter, view, &mut wrap, clip, style, &mut out);
    paint(&ui.painter_at(frame.visible), buf, view, &frame, &metrics, style, focused, &links, hovered);
    paint_gutter(ui, gutter, buf, view, &frame, &metrics, style);

    if focused {
        // IME: the candidate window goes at the caret.
        if let Some(caret) = caret_rect(&frame, &metrics, view.selection.primary().head, buf) {
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: frame.visible,
                    cursor_rect: caret,
                    should_interrupt_composition: false,
                });
            });
        }
        let since = view.activity.map_or(0, |a| a.elapsed().as_millis() as u64);
        let phase = BLINK.as_millis() as u64;
        ui.ctx().request_repaint_after(Duration::from_millis((phase - since % phase).max(16)));
    }
    view.wrap = Some(wrap);

    let primary = view.selection.primary();
    let rope = buf.rope();
    out.caret = (lines::line_of(rope, primary.head) + 1, lines::utf16_column(rope, primary.head));
    out.selected_chars = view.selection.ranges().iter().map(Range::len).sum();
    out.carets = view.selection.len();
    out
}

/// The scroll offset that keeps the caret (or a revealed line) in view, when it must change.
fn scroll_target(
    buf: &TextDoc,
    view: &mut CodeView,
    wrap: &mut Wrap,
    metrics: &Metrics,
    style: &CodeStyle,
    text_rect: Rect,
) -> Option<Vec2> {
    let mut target = view.scroll;
    let revealing = view.reveal.take();
    let caret = std::mem::take(&mut view.scroll_to_caret);
    let pos = revealing.or_else(|| caret.then(|| view.selection.primary().head))?;
    let (row, col) = wrap.locate(buf.rope(), pos.min(buf.len_chars()));
    let (y, x) = (row as f32 * metrics.row_h, col as f32 * metrics.char_w + PAD_X);
    let (vh, vw) = (text_rect.height() - BAR, text_rect.width() - BAR);
    // A revealed match keeps a few rows of context around it.
    let context = if revealing.is_some() { metrics.row_h * 3.0 } else { 0.0 };
    if y - context < target.y {
        target.y = (y - context).max(0.0);
    } else if y + metrics.row_h + context > target.y + vh {
        target.y = y + metrics.row_h + context - vh;
    }
    let margin = metrics.char_w * 4.0;
    if style.wrap {
        target.x = 0.0;
    } else if x < target.x + margin {
        target.x = (x - margin).max(0.0);
    } else if x > target.x + vw - margin {
        target.x = x - vw + margin;
    }
    (target != view.scroll).then_some(target)
}

/// Record an edit this view made: caches follow, and the caller hears of it.
fn record(
    buf: &TextDoc,
    highlighter: &mut Highlighter,
    view: &mut CodeView,
    wrap: &mut Wrap,
    edited: Edited,
    out: &mut CodeOutput,
) {
    if let Some(applied) = edited.applied {
        highlighter.invalidate_from(applied.first_line);
        wrap.apply(buf.rope(), &applied);
        view.wrap_version = applied.version;
        view.widest = view.widest.max(widest_in(buf, applied.first_line..=applied.last_line, wrap.tab()));
        out.edits.push(applied);
    }
    view.selection = edited.selection;
    view.column = false;
    view.column_anchor = None;
    view.column_head = None;
    view.touch();
}

#[allow(clippy::too_many_arguments)]
fn keyboard(
    ui: &Ui,
    buf: &mut TextDoc,
    highlighter: &mut Highlighter,
    view: &mut CodeView,
    wrap: &mut Wrap,
    style: &CodeStyle,
    clip: &mut Option<Clip>,
    page: isize,
    out: &mut CodeOutput,
) {
    let events = ui.input(|i| i.events.clone());
    let now = now_ms();
    let keymap = crate::keymap::get(ui.ctx());
    for event in events {
        let before = view.selection.clone();
        let edited = match event {
            Event::Text(text) => {
                if view.preedit.is_some() || text.chars().any(char::is_control) {
                    continue;
                }
                Some(commands::insert_text(buf, &view.selection, &text, now))
            }
            Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                view.preedit = (!text.is_empty()).then_some(text);
                continue;
            }
            Event::Ime(egui::ImeEvent::Commit(text)) => {
                view.preedit = None;
                if text.is_empty() {
                    continue;
                }
                Some(commands::insert_text(buf, &view.selection, &text, now))
            }
            Event::Copy => {
                let copied = commands::copy(buf.rope(), &view.selection, view.column);
                out.copy = Some(copied.text.clone());
                *clip = Some(copied);
                continue;
            }
            Event::Cut => {
                let (copied, edited) = commands::cut(buf, &view.selection, view.column, now);
                out.copy = Some(copied.text.clone());
                *clip = Some(copied);
                Some(edited)
            }
            Event::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                let pasted = match clip.as_ref() {
                    Some(c) if c.text == text => c.clone(),
                    _ => Clip { text, mode: ClipMode::Plain },
                };
                let pad = if style.indent.tabs { '\t' } else { ' ' };
                Some(commands::paste(buf, &view.selection, &pasted, pad, now))
            }
            Event::Key { key, pressed: true, modifiers, .. } => {
                if view.preedit.is_some() {
                    continue;
                }
                let bound =
                    crate::keymap::chord(key, modifiers).and_then(|c| keymap.lookup(&c, Scope::Editor));
                if let Some(id) = bound
                    && editor_command(id, buf, view, style, out)
                {
                    continue;
                }
                key_command(buf, view, wrap, style, key, modifiers, page, now)
            }
            _ => continue,
        };
        match edited {
            Some(edited) => record(buf, highlighter, view, wrap, edited, out),
            None if view.selection != before => {
                buf.seal();
                view.touch();
            }
            None => {}
        }
    }
}

/// Run a bound editor command (the key bindings file decides which chord). Returns false when the
/// command does not apply here, so the key keeps its editing meaning.
fn editor_command(id: &str, buf: &TextDoc, view: &CodeView, style: &CodeStyle, out: &mut CodeOutput) -> bool {
    match id {
        "editor.save" => out.save = true,
        "search.find" => out.open_find = Some(false),
        "search.replace" => out.open_find = Some(true),
        "navigate.gotoLine" => out.goto_line = true,
        "editor.toggleWordWrap" => out.toggle_wrap = true,
        // Open Link: the keyboard's Ctrl+click; with no link at the caret the key is the
        // editor's.
        "preview.followLink" => match link_at_caret(buf, view.selection.primary().head) {
            Some(target) if style.links => out.link = Some((target, LinkAction::Follow)),
            _ => return false,
        },
        _ => return false,
    }
    true
}

fn redo_or_undo(buf: &mut TextDoc, redo: bool) -> Option<Edited> {
    let result = if redo { buf.redo() } else { buf.undo() };
    result.map(|(applied, selection)| Edited { applied: Some(applied), selection })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn key_command(
    buf: &mut TextDoc,
    view: &mut CodeView,
    wrap: &mut Wrap,
    style: &CodeStyle,
    key: Key,
    m: Modifiers,
    page: isize,
    now: u64,
) -> Option<Edited> {
    let rope = buf.rope().clone();
    let sel = view.selection.clone();
    let word = if mac() { m.alt } else { m.ctrl };
    // Column selection: Shift+Alt+Arrow (Shift+Alt+Cmd on macOS, where Alt+Arrow moves by word).
    let column_chord = m.shift && m.alt && (!mac() || m.mac_cmd);
    let set = |view: &mut CodeView, s: Selection| {
        view.selection = s;
        view.column = false;
        view.column_anchor = None;
        view.column_head = None;
    };
    match key {
        Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown if column_chord => {
            let anchor =
                *view.column_anchor.get_or_insert_with(|| display_at(&rope, sel.primary().anchor, style.tab));
            // The head corner is remembered, so crossing a short line keeps its column.
            let (mut line, mut col) =
                view.column_head.unwrap_or_else(|| display_at(&rope, sel.primary().head, style.tab));
            match key {
                Key::ArrowLeft => col = col.saturating_sub(1),
                Key::ArrowRight => col += 1,
                Key::ArrowUp => line = line.saturating_sub(1),
                _ => line = (line + 1).min(rope.len_lines().saturating_sub(1)),
            }
            view.column_head = Some((line, col));
            view.selection = commands::column_rect(&rope, style.tab, anchor, (line, col));
            view.column = true;
            None
        }
        Key::ArrowLeft | Key::ArrowRight => {
            let forward = key == Key::ArrowRight;
            let s = if mac() && m.mac_cmd {
                if forward {
                    commands::move_line_end(&rope, &sel, m.shift)
                } else {
                    commands::move_line_start(&rope, &sel, m.shift)
                }
            } else {
                let unit = if word { Unit::Word } else { Unit::Char };
                commands::move_horizontal(&rope, &sel, forward, unit, m.shift)
            };
            set(view, s);
            None
        }
        Key::ArrowUp | Key::ArrowDown => {
            let down = key == Key::ArrowDown;
            let s = if mac() && m.mac_cmd {
                commands::move_doc_edge(&rope, &sel, down, m.shift)
            } else {
                commands::move_vertical(&rope, wrap, &sel, if down { 1 } else { -1 }, m.shift)
            };
            set(view, s);
            None
        }
        Key::PageUp | Key::PageDown => {
            let rows = if key == Key::PageUp { -page } else { page };
            set(view, commands::move_vertical(&rope, wrap, &sel, rows, m.shift));
            None
        }
        Key::Home | Key::End => {
            let end = key == Key::End;
            let s = if m.command {
                commands::move_doc_edge(&rope, &sel, end, m.shift)
            } else if end {
                commands::move_line_end(&rope, &sel, m.shift)
            } else {
                commands::move_line_start(&rope, &sel, m.shift)
            };
            set(view, s);
            None
        }
        Key::Backspace => {
            let unit = if word { Unit::Word } else { Unit::Char };
            Some(commands::delete_backward(buf, &sel, unit, style.indent, now))
        }
        Key::Delete => {
            let unit = if word { Unit::Word } else { Unit::Char };
            Some(commands::delete_forward(buf, &sel, unit, now))
        }
        Key::Enter if !m.command && !m.alt => Some(commands::insert_newline(buf, &sel, style.indent, now)),
        Key::Tab if !m.command && !m.alt => Some(if m.shift {
            commands::outdent_lines(buf, &sel, style.indent, now)
        } else {
            commands::insert_tab(buf, &sel, style.indent, now)
        }),
        Key::Escape if sel.len() > 1 || !sel.all_empty() => {
            let collapsed =
                if sel.len() > 1 { sel.only_primary() } else { Selection::point(sel.primary().head) };
            set(view, collapsed);
            None
        }
        Key::A if m.command && !m.shift => {
            set(view, commands::select_all(&rope));
            None
        }
        Key::Z if m.command => redo_or_undo(buf, m.shift),
        Key::Y if m.command && !mac() => redo_or_undo(buf, true),
        _ => None,
    }
}

/// Lay out the visible rows, highlighting them within the frame budget.
#[allow(clippy::too_many_arguments)]
fn layout_rows(
    ui: &Ui,
    buf: &TextDoc,
    highlighter: &mut Highlighter,
    wrap: &mut Wrap,
    metrics: &Metrics,
    style: &CodeStyle,
    origin: Pos2,
    viewport: Rect,
) -> Vec<RowLayout> {
    let rope = buf.rope();
    let first_row = (viewport.top() / metrics.row_h).floor().max(0.0) as usize;
    let last_row = ((viewport.bottom() / metrics.row_h).ceil() as usize + 1).min(wrap.total_rows());
    if first_row >= last_row {
        return Vec::new();
    }
    let (first_line, first_sub) = wrap.line_at_row(first_row);
    let (last_line, _) = wrap.line_at_row(last_row - 1);
    let spans = highlighter.spans(rope, first_line..last_line + 1, Instant::now() + HIGHLIGHT_BUDGET);
    if spans.iter().any(Option::is_none) {
        ui.ctx().request_repaint();
    }
    let mut out = Vec::new();
    let mut row = first_row;
    'lines: for (i, line) in (first_line..=last_line).enumerate() {
        let text = lines::line_text(rope, line);
        let starts = throng_editor::wrap::row_starts(&text, wrap.width(), style.tab);
        let chars: Vec<char> = text.chars().collect();
        let line_spans = spans.get(i).cloned().flatten();
        let skip = if line == first_line { first_sub } else { 0 };
        for (sub, &start) in starts.iter().enumerate().skip(skip) {
            if row >= last_row {
                break 'lines;
            }
            let end = starts.get(sub + 1).copied().unwrap_or(chars.len());
            let (job, display) =
                row_job(&text, &chars[start..end], start, line_spans.as_deref(), metrics, style);
            out.push(RowLayout {
                line,
                start,
                end,
                display,
                galley: ui.ctx().fonts_mut(|f| f.layout_job(job)),
                origin: origin + vec2(PAD_X, row as f32 * metrics.row_h),
                last_of_line: sub + 1 == starts.len(),
            });
            row += 1;
        }
    }
    out
}

/// A row's layout job: highlighted runs, tabs expanded to spaces.
fn row_job(
    line_text: &str,
    chars: &[char],
    start_char: usize,
    spans: Option<&[Span]>,
    metrics: &Metrics,
    style: &CodeStyle,
) -> (LayoutJob, Vec<usize>) {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let fg = style.colours.foreground;
    let mut display = Vec::with_capacity(chars.len() + 1);
    let mut col = 0usize;
    let mut byte: usize = line_text.chars().take(start_char).map(char::len_utf8).sum();
    let mut span_index = 0usize;
    let mut run = String::new();
    let mut run_format: Option<TextFormat> = None;
    for &ch in chars {
        display.push(col);
        if let Some(spans) = spans {
            while span_index < spans.len() && spans[span_index].end <= byte {
                span_index += 1;
            }
        }
        let token = spans.and_then(|s| s.get(span_index)).filter(|s| s.start <= byte && byte < s.end);
        let colour = token.map_or(fg, |t| {
            Color32::from_rgba_unmultiplied(t.style.fg[0], t.style.fg[1], t.style.fg[2], t.style.fg[3])
        });
        let format = TextFormat {
            font_id: metrics.font.clone(),
            color: colour,
            italics: token.is_some_and(|t| t.style.italic),
            underline: if token.is_some_and(|t| t.style.underline) {
                Stroke::new(1.0, colour)
            } else {
                Stroke::NONE
            },
            ..TextFormat::default()
        };
        if run_format.as_ref() != Some(&format) {
            if let Some(previous) = run_format.take() {
                job.append(&run, 0.0, previous);
                run.clear();
            }
            run_format = Some(format);
        }
        if ch == '\t' {
            let n = style.tab.max(1) - col % style.tab.max(1);
            run.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            run.push(ch);
            col += 1;
        }
        byte += ch.len_utf8();
    }
    display.push(col);
    match run_format {
        Some(format) => job.append(&run, 0.0, format),
        None => job.append(
            "",
            0.0,
            TextFormat { font_id: metrics.font.clone(), color: fg, ..TextFormat::default() },
        ),
    }
    (job, display)
}

/// The links on the visible lines: (line, link), in line order. Very long lines are skipped, as
/// they are for highlighting.
fn visible_links(buf: &TextDoc, frame: &Frame) -> Vec<(usize, Link)> {
    let rope = buf.rope();
    let mut out = Vec::new();
    let mut last = None;
    for row in &frame.rows {
        if last == Some(row.line) {
            continue;
        }
        last = Some(row.line);
        if rope.line(row.line).len_chars() > 10_000 {
            continue;
        }
        let text = lines::line_text(rope, row.line);
        out.extend(throng_core::links::find(&text).into_iter().map(|l| (row.line, l)));
    }
    out
}

/// The link the caret is in or just after.
fn link_at_caret(buf: &TextDoc, pos: usize) -> Option<Target> {
    let rope = buf.rope();
    let line = lines::line_of(rope, pos);
    let index = pos - lines::line_start(rope, line);
    let text = lines::line_text(rope, line);
    throng_core::links::find(&text).into_iter().find(|l| l.start <= index && index <= l.end).map(|l| l.target)
}

/// The link under a screen point: over its glyphs, not merely nearest to it.
fn link_at<'a>(frame: &Frame, links: &'a [(usize, Link)], p: Pos2, row_h: f32) -> Option<(usize, &'a Link)> {
    let row = frame.rows.iter().find(|r| p.y >= r.origin.y && p.y < r.origin.y + row_h)?;
    links.iter().filter(|(line, _)| *line == row.line).find_map(|(line, link)| {
        let (a, b) = (link.start.max(row.start), link.end.min(row.end));
        (a < b && p.x >= row.x_of(a - row.start) && p.x < row.x_of(b - row.start)).then_some((*line, link))
    })
}

/// The visible row showing character `pos`, and the index within that row.
fn row_of(frame: &Frame, buf: &TextDoc, pos: usize) -> Option<(usize, usize)> {
    let rope = buf.rope();
    let line = lines::line_of(rope, pos);
    let index = pos - lines::line_start(rope, line);
    frame.rows.iter().enumerate().find_map(|(i, r)| {
        // `then`, not `then_some`: the index is only meaningful (and only non-negative) for the
        // row that holds it.
        (r.line == line && index >= r.start && (index < r.end || (index == r.end && r.last_of_line)))
            .then(|| (i, index - r.start))
    })
}

fn caret_rect(frame: &Frame, metrics: &Metrics, pos: usize, buf: &TextDoc) -> Option<Rect> {
    let (i, index) = row_of(frame, buf, pos)?;
    let row = &frame.rows[i];
    Some(Rect::from_min_size(pos2(row.x_of(index), row.origin.y), vec2(2.0, metrics.row_h)))
}

/// The character position under a screen point. Rows outside the visible ones (a drag past the
/// edge) fall back to display columns.
fn position_at(frame: &Frame, buf: &TextDoc, wrap: &mut Wrap, metrics: &Metrics, p: Pos2) -> usize {
    let rope = buf.rope();
    if let Some(row) = frame.rows.iter().find(|r| p.y >= r.origin.y && p.y < r.origin.y + metrics.row_h) {
        let mut index = row.index_at_x(p.x).min(row.end - row.start);
        // The end of a wrapped row belongs to the next row.
        if !row.last_of_line && index == row.end - row.start && index > 0 {
            index -= 1;
        }
        return lines::line_start(rope, row.line) + row.start + index;
    }
    let row = ((p.y - frame.origin.y) / metrics.row_h).floor();
    if row < 0.0 {
        return 0;
    }
    if row as usize >= wrap.total_rows() {
        return rope.len_chars();
    }
    let col = ((p.x - frame.origin.x - PAD_X) / metrics.char_w).round().max(0.0) as usize;
    wrap.position(rope, row as usize, col)
}

fn display_at(rope: &ropey::Rope, pos: usize, tab: usize) -> (usize, usize) {
    let line = lines::line_of(rope, pos);
    let text = lines::line_text(rope, line);
    (line, lines::display_col(&text, pos - lines::line_start(rope, line), tab))
}

#[allow(clippy::too_many_arguments)]
fn mouse(
    ui: &Ui,
    buf: &TextDoc,
    view: &mut CodeView,
    wrap: &mut Wrap,
    response: &egui::Response,
    frame: &Frame,
    metrics: &Metrics,
    style: &CodeStyle,
) {
    let Some(pointer) = response.interact_pointer_pos() else {
        return;
    };
    let rope = buf.rope();
    let pos = position_at(frame, buf, wrap, metrics, pointer);
    let m = ui.input(|i| i.modifiers);
    let add_cursor = if mac() { m.mac_cmd } else { m.ctrl };
    let column_drag = m.alt && !add_cursor;
    let primary = egui::PointerButton::Primary;

    let place = |view: &mut CodeView, at: usize| {
        let anchor = if column_drag {
            let corner = display_at(rope, at, style.tab);
            view.selection = commands::column_rect(rope, style.tab, corner, corner);
            view.column = true;
            return Drag { anchor: at, column: Some(corner) };
        } else if add_cursor {
            view.selection = view.selection.with(Range::point(at));
            at
        } else if m.shift {
            let anchor = view.selection.primary().anchor;
            view.selection = Selection::single(anchor, at);
            anchor
        } else {
            view.selection = Selection::point(at);
            at
        };
        view.column = false;
        Drag { anchor, column: None }
    };

    if response.triple_clicked() {
        let r = commands::line_range(rope, pos);
        view.selection = Selection::single(r.anchor, r.head);
        view.column = false;
        view.drag = None;
    } else if response.double_clicked() {
        let r = commands::word_at(rope, pos);
        view.selection = Selection::single(r.anchor, r.head);
        view.column = false;
        view.drag = None;
    } else if response.drag_started_by(primary) {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pointer);
        let start = position_at(frame, buf, wrap, metrics, origin);
        view.drag = Some(place(view, start));
    } else if response.clicked_by(primary) {
        place(view, pos);
        view.drag = None;
    } else if response.dragged_by(primary)
        && let Some(drag) = view.drag
    {
        if let Some(corner) = drag.column {
            let head = display_at(rope, pos, style.tab);
            view.selection = commands::column_rect(rope, style.tab, corner, head);
            view.column = true;
            view.column_anchor = Some(corner);
            view.column_head = Some(head);
        } else {
            let (from, to) = (drag.anchor, pos);
            let mut ranges: Vec<Range> = view.selection.ranges().to_vec();
            let primary_index = view.selection.primary_index();
            ranges[primary_index] = Range::new(from, to);
            view.selection = Selection::new(ranges, primary_index);
        }
        if !frame.visible.y_range().contains(pointer.y) {
            // Scroll while dragging past the top or bottom.
            view.scroll_to_caret = true;
            ui.ctx().request_repaint();
        }
    } else if response.secondary_clicked() {
        // Right-click inside a selection keeps it; outside moves the caret.
        let inside =
            view.selection.ranges().iter().any(|r| !r.is_empty() && r.from() <= pos && pos <= r.to());
        if !inside {
            view.selection = Selection::point(pos);
            view.column = false;
        }
    } else {
        return;
    }
    if response.drag_stopped() {
        view.drag = None;
    }
    view.activity = Some(Instant::now());
}

#[allow(clippy::too_many_arguments)]
fn context_menu(
    response: &egui::Response,
    buf: &mut TextDoc,
    highlighter: &mut Highlighter,
    view: &mut CodeView,
    wrap: &mut Wrap,
    clip: &mut Option<Clip>,
    style: &CodeStyle,
    out: &mut CodeOutput,
) {
    let open = response.context_menu(|ui| {
        let now = now_ms();
        let mut edited = None;
        if let Some(target) = view.menu_link.clone() {
            link_items(ui, &target, out);
            ui.separator();
        }
        if ui.button("Cut").clicked() {
            let (copied, e) = commands::cut(buf, &view.selection, view.column, now);
            out.copy = Some(copied.text.clone());
            *clip = Some(copied);
            edited = Some(e);
        }
        if ui.button("Copy").clicked() {
            let copied = commands::copy(buf.rope(), &view.selection, view.column);
            out.copy = Some(copied.text.clone());
            *clip = Some(copied);
            ui.close();
        }
        if ui.button("Paste").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }
        if ui.button("Select All").clicked() {
            view.selection = commands::select_all(buf.rope());
            view.column = false;
            ui.close();
        }
        ui.separator();
        if ui.add_enabled(buf.history().can_undo(), egui::Button::new("Undo")).clicked() {
            edited = redo_or_undo(buf, false);
        }
        if ui.add_enabled(buf.history().can_redo(), egui::Button::new("Redo")).clicked() {
            edited = redo_or_undo(buf, true);
        }
        ui.separator();
        if ui.button("Find").clicked() {
            out.open_find = Some(false);
            ui.close();
        }
        if ui.button("Replace").clicked() {
            out.open_find = Some(true);
            ui.close();
        }
        if ui.button("Go to Line…").clicked() {
            out.goto_line = true;
            ui.close();
        }
        ui.separator();
        let mut wrapped = style.wrap;
        if ui.checkbox(&mut wrapped, "Word Wrap").clicked() {
            out.toggle_wrap = true;
            ui.close();
        }
        if ui.button("Set Language…").clicked() {
            out.set_language = true;
            ui.close();
        }
        if style.previewable && ui.button("Open Preview").clicked() {
            out.open_preview = true;
            ui.close();
        }
        if let Some(edited) = edited {
            record(buf, highlighter, view, wrap, edited, out);
            ui.close();
        }
    });
    if open.is_none() {
        view.menu_link = None;
    }
}

/// The link menu's items: follow it, copy its address, and for a file, show it in the
/// file manager or open it with the system's program.
pub fn link_items(ui: &mut Ui, target: &Target, out_link: &mut impl LinkSink) {
    let file = matches!(target, Target::File { .. });
    let mut item = |ui: &mut Ui, label: &str, action: LinkAction| {
        if ui.button(label).clicked() {
            out_link.link(target.clone(), action);
            ui.close();
        }
    };
    item(ui, "Open Link", LinkAction::Follow);
    item(ui, "Copy Link Address", LinkAction::Copy);
    if file {
        item(ui, "Reveal in File Manager", LinkAction::Reveal);
        item(ui, "Open with Default Program", LinkAction::OpenDefault);
    }
}

/// Where a link menu's choice goes.
pub trait LinkSink {
    fn link(&mut self, target: Target, action: LinkAction);
}

impl LinkSink for CodeOutput {
    fn link(&mut self, target: Target, action: LinkAction) {
        self.link = Some((target, action));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(
    painter: &egui::Painter,
    buf: &TextDoc,
    view: &CodeView,
    frame: &Frame,
    metrics: &Metrics,
    style: &CodeStyle,
    focused: bool,
    links: &[(usize, Link)],
    hovered: Option<(usize, &Link)>,
) {
    let c = style.colours;
    let rope = buf.rope();
    let primary_line = lines::line_of(rope, view.selection.primary().head);
    let span_rect = |r: &RowLayout, from: usize, to: usize, eol: bool| {
        let x0 = r.x_of(from);
        let x1 = r.x_of(to) + if eol { metrics.char_w * 0.6 } else { 0.0 };
        Rect::from_min_max(pos2(x0, r.origin.y), pos2(x1.max(x0 + 1.0), r.origin.y + metrics.row_h))
    };
    if view.selection.all_empty() {
        for r in frame.rows.iter().filter(|r| r.line == primary_line) {
            let band = Rect::from_min_max(
                pos2(frame.visible.left(), r.origin.y),
                pos2(frame.visible.right(), r.origin.y + metrics.row_h),
            );
            painter.rect_filled(band, 0.0, c.current_line);
        }
    }
    for r in &frame.rows {
        let line_start = lines::line_start(rope, r.line);
        let (row_from, row_to) = (line_start + r.start, line_start + r.end);
        if let Some(find) = &view.find {
            let current = find.current_match();
            let first = find.matches.partition_point(|m| m.1 <= row_from);
            for &(mf, mt) in find.matches[first..].iter().take_while(|m| m.0 < row_to) {
                let rect = span_rect(r, mf.max(row_from) - row_from, mt.min(row_to) - row_from, false);
                if current == Some((mf, mt)) {
                    painter.rect_filled(rect, 2.0, c.search_current);
                    painter.rect_stroke(
                        rect,
                        2.0,
                        Stroke::new(1.0, c.search_current_border),
                        egui::StrokeKind::Inside,
                    );
                } else {
                    painter.rect_filled(rect, 2.0, c.search_match);
                }
            }
        }
        for s in view.selection.ranges().iter().filter(|s| !s.is_empty()) {
            if s.to() < row_from || s.from() > row_to {
                continue;
            }
            let (a, b) = (s.from().max(row_from) - row_from, s.to().min(row_to) - row_from);
            let eol = s.to() > row_to && r.last_of_line;
            if a < b || eol {
                painter.rect_filled(span_rect(r, a, b, eol), 0.0, c.selection);
            }
        }
        painter.galley(r.origin, r.galley.clone(), c.foreground);
        for (line, link) in links.iter().filter(|(line, _)| *line == r.line) {
            let (a, b) = (link.start.max(r.start), link.end.min(r.end));
            if a >= b {
                continue;
            }
            let hot = hovered.is_some_and(|(l, h)| l == *line && std::ptr::eq(h, link));
            let colour = if hot { c.link } else { c.link.gamma_multiply(0.45) };
            let y = r.origin.y + metrics.row_h - 1.5;
            painter.hline(r.x_of(a - r.start)..=r.x_of(b - r.start), y, Stroke::new(1.0, colour));
        }
    }
    let blink_on =
        view.activity.is_none_or(|a| (a.elapsed().as_millis() / BLINK.as_millis()).is_multiple_of(2));
    if focused && blink_on {
        for s in view.selection.ranges() {
            if let Some(rect) = caret_rect(frame, metrics, s.head, buf) {
                painter.rect_filled(rect, 0.0, c.cursor);
            }
        }
    }
    // An IME composition, drawn at the caret and underlined.
    if let Some(preedit) = &view.preedit
        && let Some(caret) = caret_rect(frame, metrics, view.selection.primary().head, buf)
    {
        let galley = painter.layout_no_wrap(preedit.clone(), metrics.font.clone(), c.foreground);
        let rect = Rect::from_min_size(caret.min, vec2(galley.size().x, metrics.row_h));
        painter.rect_filled(rect, 0.0, c.background);
        painter.galley(caret.min, galley, c.foreground);
        painter.hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, c.foreground));
    }
}

fn paint_gutter(
    ui: &Ui,
    gutter: Rect,
    buf: &TextDoc,
    view: &CodeView,
    frame: &Frame,
    metrics: &Metrics,
    style: &CodeStyle,
) {
    let painter = ui.painter_at(gutter);
    let rope = buf.rope();
    let caret_lines: Vec<usize> =
        view.selection.ranges().iter().map(|r| lines::line_of(rope, r.head)).collect();
    let size = style.font_size * 0.92;
    let font = FontId::monospace(size);
    for r in frame.rows.iter().filter(|r| r.start == 0) {
        let colour =
            if caret_lines.contains(&r.line) { style.colours.gutter_active } else { style.colours.gutter_fg };
        painter.text(
            pos2(gutter.right() - 8.0, r.origin.y + (metrics.row_h - size) * 0.5),
            egui::Align2::RIGHT_TOP,
            (r.line + 1).to_string(),
            font.clone(),
            colour,
        );
    }
}
