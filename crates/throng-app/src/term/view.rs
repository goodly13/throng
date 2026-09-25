//! Drawing a terminal and turning input into bytes.

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, CursorIcon, Event, EventFilter, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2,
};
use throng_core::keymap::Scope;

use super::colors::{Palette, dim};
use super::{TerminalView, keys};

/// How to draw terminals.
#[derive(Clone, Debug, PartialEq)]
pub struct TermStyle {
    pub font_size: f32,
    pub palette: Palette,
    pub copy_on_select: bool,
    pub search_match: Color32,
    pub search_current: Color32,
    /// Link underlines, faint at rest and solid under the pointer.
    pub link: Color32,
    /// Show links at all (the `editor.links.detectInTerminals` setting).
    pub links: bool,
    /// The widget's id: [`terminal_id`] for the panel that owns the session, another for each
    /// panel mirroring it.
    pub widget: egui::Id,
    /// Whether this drawing sizes the terminal and reports its focus to the program: the owner's
    /// panel when it is on screen, else the first mirror drawn. Others show the grid as it is.
    pub primary: bool,
}

/// What a frame of a terminal produced.
#[derive(Default, Debug)]
pub struct TermOutput {
    /// Bytes for the shell.
    pub input: Vec<u8>,
    /// The grid changed size; tell the daemon.
    pub resized: bool,
    /// Text for the clipboard.
    pub copy: Option<String>,
    pub focused: bool,
    /// Ctrl+F: open the find bar.
    pub open_find: bool,
    /// A link to follow, copy or reveal.
    pub link: Option<(throng_core::links::Target, crate::links::LinkAction)>,
}

impl crate::code::LinkSink for TermOutput {
    fn link(&mut self, target: throng_core::links::Target, action: crate::links::LinkAction) {
        self.link = Some((target, action));
    }
}

/// The id of a panel's terminal widget (global, so focus can be given from anywhere).
#[must_use]
pub fn terminal_id(panel: throng_core::ids::PanelId) -> egui::Id {
    egui::Id::new(("throng-terminal", panel))
}

const PAD: f32 = 4.0;

/// Show a terminal filling the available space.
pub fn show(ui: &mut Ui, view: &mut TerminalView, style: &TermStyle) -> TermOutput {
    let mut out = TermOutput::default();
    let rect = ui.available_rect_before_wrap();
    let id = style.widget;
    view.widgets.insert(id);
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let label = if id == terminal_id(view.panel) {
        format!("Terminal: {}", view.label)
    } else {
        format!("Terminal: {}, mirrored", view.label)
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &label));
    ui.allocate_rect(rect, Sense::hover());

    let font = FontId::monospace(style.font_size);
    let (cell_w, cell_h) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let (cols, rows) = if style.primary {
        let cols = ((rect.width() - 2.0 * PAD) / cell_w).floor().max(2.0) as u16;
        let rows = ((rect.height() - 2.0 * PAD) / cell_h).floor().max(1.0) as u16;
        out.resized = view.resize(cols, rows);
        (cols, rows)
    } else {
        view.size
    };
    let origin = rect.min + vec2(PAD, PAD);
    let grid = Grid { origin, cell: vec2(cell_w, cell_h), cols, rows };
    view.palette = Some(style.palette.clone());
    let ppp = ui.ctx().pixels_per_point();
    view.cell_px = ((cell_w * ppp).round() as u16, (cell_h * ppp).round() as u16);

    if response.clicked() || response.drag_started() || response.secondary_clicked() {
        response.request_focus();
    }
    let focused = response.has_focus();
    out.focused = focused;
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            );
        });
        // egui applies the lock only from the second focused frame. egui is reactive, so the next
        // frame is usually the one carrying the user's next key: an Escape there would drop focus
        // and never reach the shell (vim). Draw that frame now.
        if response.gained_focus() {
            ui.ctx().request_repaint();
        }
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }

    let mode = view.mode();
    // Focus in any window showing this terminal is focus for the program, reported once, by the
    // primary drawing, and only on a real change: a stray ESC [ I would read as Escape plus text.
    let focused_anywhere = ui.memory(|m| m.focused()).is_some_and(|f| view.widgets.contains(&f));
    if style.primary && focused_anywhere != view.had_focus {
        if mode.contains(TermMode::FOCUS_IN_OUT) {
            out.input.extend_from_slice(if focused_anywhere { b"\x1b[I" } else { b"\x1b[O" });
        }
        view.had_focus = focused_anywhere;
    }

    if focused {
        app_keys(ui, view, &mut out);
        keyboard(ui, view, mode, &mut out);
    }
    reveal_match(view);
    let links = if style.links { view.screen_links() } else { Vec::new() };
    let hovered = response.hover_pos().and_then(|p| {
        let (point, _) = grid.point(p, 0);
        links.iter().position(|l| l.contains(point.line.0, point.column.0))
    });
    // Ctrl, or Cmd on macOS.
    let command = ui.input(|i| i.modifiers.command);
    if let Some(i) = hovered {
        if command {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        response.clone().on_hover_text_at_pointer(crate::links::hover_text(&links[i].target));
    }
    // Ctrl+click (Cmd+click) follows a link, and is never also sent to the program as a mouse
    // report.
    let gesture = command && hovered.is_some();
    if gesture && response.clicked() {
        out.link = hovered.map(|i| (links[i].target.clone(), crate::links::LinkAction::Follow));
    } else {
        mouse(ui, view, &response, &grid, mode, style, gesture, &mut out);
    }
    if response.secondary_clicked() {
        let selected = view.term().selection_to_string().is_some_and(|s| !s.is_empty());
        view.menu_link = hovered.filter(|_| !selected).map(|i| links[i].target.clone());
    }
    if let Some(target) = view.menu_link.clone() {
        let open = response.context_menu(|ui| crate::code::link_items(ui, &target, &mut out));
        if open.is_none() {
            view.menu_link = None;
        }
    }

    if !out.input.is_empty() && view.term().grid().display_offset() != 0 {
        view.term_mut().scroll_display(Scroll::Bottom);
    }

    paint(ui, view, rect, &grid, &font, style, focused);
    if focused {
        ime(ui, view, rect, &grid, &font, style);
    }
    let painter = ui.painter_at(rect);
    for (i, link) in links.iter().enumerate() {
        let colour = if hovered == Some(i) { style.link } else { style.link.gamma_multiply(0.45) };
        for &(row, from, to) in &link.cells {
            let cells = grid.cell_rect(row, from, to - from);
            painter.hline(cells.x_range(), cells.bottom() - 1.0, Stroke::new(1.0, colour));
        }
    }
    out
}

struct Grid {
    origin: Pos2,
    cell: Vec2,
    cols: u16,
    rows: u16,
}

impl Grid {
    /// The cell under a screen position, and which half of it.
    fn point(&self, pos: Pos2, display_offset: usize) -> (Point, Side) {
        let x = ((pos.x - self.origin.x) / self.cell.x).max(0.0);
        let y = ((pos.y - self.origin.y) / self.cell.y).max(0.0);
        let col = (x.floor() as usize).min(usize::from(self.cols) - 1);
        let row = (y.floor() as usize).min(usize::from(self.rows) - 1);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (Point::new(Line(row as i32 - display_offset as i32), Column(col)), side)
    }

    fn cell_rect(&self, row: i32, col: usize, width: usize) -> Rect {
        Rect::from_min_size(
            self.origin + vec2(col as f32 * self.cell.x, row as f32 * self.cell.y),
            vec2(self.cell.x * width as f32, self.cell.y),
        )
    }
}

/// Keys throng takes from a focused terminal before the shell sees them: find (Ctrl+F is a
/// recorded exception to the reserved keys, Principle IV), and scrollback navigation, which
/// is never sent to the program.
fn app_keys(ui: &Ui, view: &mut TerminalView, out: &mut TermOutput) {
    use egui::{Key, Modifiers};
    let take = |id| crate::keymap::take(ui.ctx(), id, Some(Scope::Terminal));
    if take("search.find") {
        out.open_find = true;
    }
    if let Some(find) = view.find.as_mut() {
        if take("search.findPrevious") {
            find.step(false);
        } else if take("search.findNext") {
            find.step(true);
        }
        if ui.ctx().input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            view.find = None;
        }
    }
    let page = view.size.1.saturating_sub(1).max(1) as i32;
    let scroll = [
        ("terminal.scrollLineUp", Scroll::Delta(1)),
        ("terminal.scrollLineDown", Scroll::Delta(-1)),
        ("terminal.scrollPageUp", Scroll::Delta(page)),
        ("terminal.scrollPageDown", Scroll::Delta(-page)),
        ("terminal.scrollToTop", Scroll::Top),
        ("terminal.scrollToBottom", Scroll::Bottom),
    ];
    for (id, how) in scroll {
        if take(id) {
            view.term_mut().scroll_display(how);
        }
    }
}

/// Scroll so the current find match is on screen.
fn reveal_match(view: &mut TerminalView) {
    let Some(find) = view.find.as_mut() else { return };
    if !std::mem::take(&mut find.reveal) {
        return;
    }
    let Some(((line, _), _)) = find.current_match() else { return };
    let rows = i32::from(view.size.1);
    let offset = view.term().grid().display_offset() as i32;
    // Visible lines are -offset ..= rows - 1 - offset.
    if line < -offset || line > rows - 1 - offset {
        let wanted = (-line + rows / 2).max(0);
        view.term_mut().scroll_display(Scroll::Delta(wanted - offset));
    }
}

fn keyboard(ui: &Ui, view: &mut TerminalView, mode: TermMode, out: &mut TermOutput) {
    let events = ui.input(|i| i.events.clone());
    let modifiers = ui.input(|i| i.modifiers);
    let mac = cfg!(target_os = "macos");
    let kitty = keys::Kitty {
        disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
        all_keys: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
        associated_text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
    };
    let ctrl = egui::Modifiers { ctrl: true, ..egui::Modifiers::NONE };
    let mut events = events.into_iter().peekable();
    while let Some(event) = events.next() {
        match event {
            // While an input method composes, Enter and Backspace are its, not the program's.
            Event::Key { pressed: true, .. } if view.preedit.is_some() => {}
            Event::Key { key, pressed: true, modifiers, .. } => {
                // egui-winit sends the text a press typed straight after its key event; a chord
                // (Ctrl) types none, so the next event is then another key's.
                let text = match events.peek() {
                    Some(Event::Text(text)) => Some(text.clone()),
                    _ => None,
                };
                let reported = if let Some(bytes) =
                    keys::encode_kitty_typed(key, modifiers, kitty, text.as_deref())
                {
                    out.input.extend(bytes);
                    true
                } else if let Some(bytes) = keys::encode(key, modifiers, mode.contains(TermMode::APP_CURSOR))
                {
                    out.input.extend(bytes);
                    modifiers.ctrl || (modifiers.alt && keys::alt_is_meta())
                } else {
                    false
                };
                // A reported key's text is in its report, and is not typed a second time.
                if reported && text.is_some() {
                    events.next();
                }
            }
            Event::Text(text) => out.input.extend_from_slice(text.as_bytes()),
            Event::Copy => {
                let selection = view.term().selection_to_string().filter(|s| !s.is_empty());
                if let Some(text) = selection {
                    out.copy = Some(text);
                    view.term_mut().selection = None;
                } else if !mac && !modifiers.shift {
                    // Ctrl+C with nothing selected interrupts, as the shell expects.
                    if kitty.on() {
                        out.input.extend(keys::csi_u(u32::from(b'c'), ctrl));
                    } else {
                        out.input.push(0x03);
                    }
                }
            }
            Event::Cut if !mac => {
                if kitty.on() {
                    out.input.extend(keys::csi_u(u32::from(b'x'), ctrl));
                } else {
                    out.input.push(0x18);
                }
            }
            Event::Paste(text) => {
                out.input.extend(keys::paste(&text, mode.contains(TermMode::BRACKETED_PASTE)))
            }
            Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                view.preedit = (!text.is_empty()).then_some(text);
            }
            Event::Ime(egui::ImeEvent::Commit(text)) => {
                view.preedit = None;
                out.input.extend_from_slice(text.as_bytes());
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mouse(
    ui: &Ui,
    view: &mut TerminalView,
    response: &egui::Response,
    grid: &Grid,
    mode: TermMode,
    style: &TermStyle,
    link_gesture: bool,
    out: &mut TermOutput,
) {
    let display_offset = view.term().grid().display_offset();
    let shift = ui.input(|i| i.modifiers.shift);
    let reporting = mode.intersects(TermMode::MOUSE_MODE) && !shift;

    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            view.scroll_remainder += scroll / grid.cell.y;
            let lines = view.scroll_remainder.trunc() as i32;
            view.scroll_remainder -= lines as f32;
            if lines != 0 {
                if reporting {
                    if let Some(pos) = response.hover_pos() {
                        let (point, _) = grid.point(pos, 0);
                        let button = if lines > 0 { 64 } else { 65 };
                        for _ in 0..lines.unsigned_abs() {
                            out.input.extend(mouse_report(mode, button, point, true));
                        }
                    }
                } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                    let key: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                        (true, true) => b"\x1bOA",
                        (true, false) => b"\x1b[A",
                        (false, true) => b"\x1bOB",
                        (false, false) => b"\x1b[B",
                    };
                    for _ in 0..lines.unsigned_abs() {
                        out.input.extend_from_slice(key);
                    }
                } else {
                    view.term_mut().scroll_display(Scroll::Delta(lines));
                }
            }
        }
    }

    let Some(pos) = response.interact_pointer_pos() else { return };
    let (point, side) = grid.point(pos, display_offset);

    if reporting && link_gesture {
        return;
    }
    if reporting {
        let (report_point, _) = grid.point(pos, 0);
        let pressed = |b| ui.input(|i| i.pointer.button_pressed(b));
        let released = |b| ui.input(|i| i.pointer.button_released(b));
        for (button, code) in [
            (egui::PointerButton::Primary, 0),
            (egui::PointerButton::Middle, 1),
            (egui::PointerButton::Secondary, 2),
        ] {
            if pressed(button) {
                out.input.extend(mouse_report(mode, code, report_point, true));
            }
            if released(button) {
                out.input.extend(mouse_report(mode, code, report_point, false));
            }
        }
        if response.dragged() && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
            let delta = ui.input(|i| i.pointer.delta());
            if delta != Vec2::ZERO {
                out.input.extend(mouse_report(mode, 32, report_point, true));
            }
        }
        return;
    }

    if response.triple_clicked() {
        view.term_mut().selection = Some(Selection::new(SelectionType::Lines, point, side));
    } else if response.double_clicked() {
        view.term_mut().selection = Some(Selection::new(SelectionType::Semantic, point, side));
    } else if response.drag_started_by(egui::PointerButton::Primary) {
        view.term_mut().selection = Some(Selection::new(SelectionType::Simple, point, side));
        view.selecting = true;
    } else if response.dragged_by(egui::PointerButton::Primary) && view.selecting {
        if let Some(selection) = view.term_mut().selection.as_mut() {
            selection.update(point, side);
        }
    } else if response.clicked() {
        view.term_mut().selection = None;
    }
    if response.drag_stopped() {
        view.selecting = false;
        if style.copy_on_select {
            out.copy = view.term().selection_to_string().filter(|s| !s.is_empty());
        }
    }
}

/// Where an input method's candidate window goes, and the text it is composing: at the cursor,
/// underlined, over what is there.
fn ime(ui: &Ui, view: &TerminalView, rect: Rect, grid: &Grid, font: &FontId, style: &TermStyle) {
    let term = view.term();
    let point = term.grid().cursor.point;
    let row = point.line.0 + term.grid().display_offset() as i32;
    let cursor = grid.cell_rect(row, point.column.0, 1);
    ui.ctx().output_mut(|o| {
        o.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Terminal,
            rect,
            cursor_rect: cursor,
            should_interrupt_composition: false,
        });
    });
    if let Some(preedit) = &view.preedit {
        let painter = ui.painter_at(rect);
        let galley = painter.layout_no_wrap(preedit.clone(), font.clone(), style.palette.foreground);
        let area =
            Rect::from_min_size(cursor.min, vec2(galley.size().x.max(cursor.width()), cursor.height()));
        painter.rect_filled(area, 0.0, style.palette.background);
        painter.galley(cursor.min, galley, style.palette.foreground);
        painter.hline(area.x_range(), area.bottom() - 1.0, Stroke::new(1.0, style.palette.foreground));
    }
}

/// An xterm mouse report (SGR when the program asked for it).
fn mouse_report(mode: TermMode, button: u8, point: Point, press: bool) -> Vec<u8> {
    let (x, y) = (point.column.0 + 1, point.line.0.max(0) as usize + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        format!("\x1b[<{button};{x};{y}{}", if press { 'M' } else { 'm' }).into_bytes()
    } else {
        let code = if press { button } else { 3 };
        let clamp = |v: usize| (32 + v.min(223)) as u8;
        vec![0x1b, b'[', b'M', 32 + code, clamp(x), clamp(y)]
    }
}

struct Run {
    row: i32,
    col: usize,
    text: String,
    format: TextFormat,
}

fn paint(
    ui: &Ui,
    view: &TerminalView,
    rect: Rect,
    grid: &Grid,
    font: &FontId,
    style: &TermStyle,
    focused: bool,
) {
    let painter = ui.painter_at(rect);
    let palette = &style.palette;
    painter.rect_filled(rect, 0.0, palette.background);

    let term = view.term();
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let colors = content.colors;
    let mut run: Option<Run> = None;
    let flush = |run: &mut Option<Run>| {
        if let Some(r) = run.take() {
            let mut job = LayoutJob::default();
            job.append(&r.text, 0.0, r.format);
            let galley = painter.layout_job(job);
            painter.galley(grid.cell_rect(r.row, r.col, 1).min, galley, palette.foreground);
        }
    };

    for indexed in content.display_iter {
        let cell = indexed.cell;
        let row = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        let mut fg_color = cell.fg;
        if cell.flags.contains(Flags::BOLD) {
            fg_color = palette.brighten(fg_color);
        }
        let mut fg = palette.resolve(fg_color, colors);
        let mut bg = palette.resolve(cell.bg, colors);
        if cell.flags.contains(Flags::DIM) {
            fg = dim(fg, palette.background);
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let default_bg =
            matches!(cell.bg, Color::Named(NamedColor::Background)) && !cell.flags.contains(Flags::INVERSE);
        let selected = selection.is_some_and(|s| s.contains(indexed.point));
        let width = if wide { 2 } else { 1 };
        if !default_bg {
            painter.rect_filled(grid.cell_rect(row, col, width), 0.0, bg);
        }
        if selected {
            painter.rect_filled(grid.cell_rect(row, col, width), 0.0, palette.selection);
        }
        if let Some(current) = view.find.as_ref().and_then(|f| f.cell(indexed.point.line.0, col)) {
            let colour = if current { style.search_current } else { style.search_match };
            painter.rect_filled(grid.cell_rect(row, col, width), 0.0, colour);
        }

        let hidden = cell.flags.contains(Flags::HIDDEN);
        let format = TextFormat {
            font_id: font.clone(),
            color: fg,
            italics: cell.flags.contains(Flags::ITALIC),
            underline: if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                Stroke::new(1.0, fg)
            } else {
                Stroke::NONE
            },
            strikethrough: if cell.flags.contains(Flags::STRIKEOUT) {
                Stroke::new(1.0, fg)
            } else {
                Stroke::NONE
            },
            ..TextFormat::default()
        };
        let mut glyphs = String::new();
        glyphs.push(if hidden { ' ' } else { cell.c });
        if !hidden && let Some(zero_width) = cell.zerowidth() {
            glyphs.extend(zero_width.iter());
        }
        let extends = run.as_ref().is_some_and(|r| {
            !wide && r.row == row && r.col + r.text.chars().count() == col && r.format == format
        });
        if extends && glyphs.chars().count() == 1 {
            if let Some(r) = run.as_mut() {
                r.text.push_str(&glyphs);
            }
        } else {
            flush(&mut run);
            if glyphs.trim().is_empty()
                && format.underline == Stroke::NONE
                && format.strikethrough == Stroke::NONE
            {
                continue;
            }
            run = Some(Run { row, col, text: glyphs, format });
            if wide {
                flush(&mut run);
            }
        }
    }
    flush(&mut run);

    // The cursor.
    let cursor = content.cursor;
    let row = cursor.point.line.0 + offset;
    if content.mode.contains(TermMode::SHOW_CURSOR)
        && cursor.shape != CursorShape::Hidden
        && (0..i32::from(grid.rows)).contains(&row)
    {
        let cell = grid.cell_rect(row, cursor.point.column.0, 1);
        let color = palette.cursor;
        if !focused {
            painter.rect_stroke(cell.shrink(0.5), 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        } else {
            match cursor.shape {
                CursorShape::Beam => {
                    painter.rect_filled(Rect::from_min_size(cell.min, vec2(2.0, cell.height())), 0.0, color);
                }
                CursorShape::Underline => {
                    painter.rect_filled(
                        Rect::from_min_size(pos2(cell.min.x, cell.max.y - 2.0), vec2(cell.width(), 2.0)),
                        0.0,
                        color,
                    );
                }
                CursorShape::HollowBlock => {
                    painter.rect_stroke(
                        cell.shrink(0.5),
                        0.0,
                        Stroke::new(1.0, color),
                        egui::StrokeKind::Inside,
                    );
                }
                _ => {
                    painter.rect_filled(cell, 0.0, color);
                    let under = &term.grid()[cursor.point];
                    if under.c != ' ' {
                        let mut job = LayoutJob::default();
                        job.append(
                            &under.c.to_string(),
                            0.0,
                            TextFormat {
                                font_id: font.clone(),
                                color: palette.background,
                                ..TextFormat::default()
                            },
                        );
                        painter.galley(cell.min, painter.layout_job(job), palette.background);
                    }
                }
            }
        }
    }

    // Where in the scrollback the view is.
    if offset > 0 {
        let history = term.grid().history_size().max(1);
        let visible = f32::from(grid.rows);
        let total = history as f32 + visible;
        let track = rect.shrink2(vec2(0.0, 2.0));
        let thumb_h = (visible / total * track.height()).max(12.0);
        let top =
            track.top() + (1.0 - (offset as f32 + visible) / total) * (track.height() - thumb_h).max(0.0);
        let thumb = Rect::from_min_size(pos2(track.right() - 5.0, top.max(track.top())), vec2(3.0, thumb_h));
        painter.rect_filled(thumb, 1.5, palette.foreground.gamma_multiply(0.35));
        let label = format!("⏶ {offset}");
        let galley = painter.layout_no_wrap(label, FontId::proportional(11.0), palette.background);
        let size = galley.size() + vec2(8.0, 4.0);
        let badge = Rect::from_min_size(pos2(rect.right() - size.x - 12.0, rect.top() + 6.0), size);
        painter.rect_filled(badge, 3.0, palette.foreground.gamma_multiply(0.75));
        painter.galley(badge.min + vec2(4.0, 2.0), galley, palette.background);
    }
    let _ = Color32::TRANSPARENT;
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use egui::{Key, Modifiers};
    use throng_core::ids::PanelId;

    fn press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// The bytes a frame's key and text events send once the program wrote `program`.
    fn typed(program: &[u8], events: Vec<Event>) -> Vec<u8> {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        view.feed(0, program);
        let mode = view.mode();
        let mut out = TermOutput::default();
        let ctx = egui::Context::default();
        let input = egui::RawInput { events, ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| keyboard(ui, &mut view, mode, &mut out));
        output.textures_delta.clear();
        out.input
    }

    // Textual (OpenHands CLI and others) pushes disambiguate | report all keys | associated text
    // and takes a key's character from the text parameter: `CSI 32u` is a Space that types
    // nothing, so without the text the Space bar and capitals are dead.
    #[test]
    fn keys_reported_as_escape_codes_carry_the_text_they_typed() {
        const TEXTUAL: &[u8] = b"\x1b[>25u";
        let none = Modifiers::NONE;
        let shift = Modifiers { shift: true, ..none };
        let ctrl = Modifiers { ctrl: true, ..none };
        let space = vec![press(Key::Space, none), Event::Text(" ".into())];
        assert_eq!(typed(TEXTUAL, space.clone()), b"\x1b[32;;32u", "Space");
        let a = vec![press(Key::A, shift), Event::Text("A".into())];
        assert_eq!(typed(TEXTUAL, a), b"\x1b[97;2;65u", "Shift+A");
        // A chord types nothing (egui sends no text with Ctrl), so it has no text parameter.
        assert_eq!(typed(TEXTUAL, vec![press(Key::C, ctrl)]), b"\x1b[99;5u", "Ctrl+C");
        // Every key as an escape code but no text asked for: none is sent.
        assert_eq!(typed(b"\x1b[>9u", space.clone()), b"\x1b[32u");
        // Disambiguation alone leaves typing as text.
        assert_eq!(typed(b"\x1b[>1u", space.clone()), b" ");
        assert_eq!(typed(b"", space), b" ");
    }

    #[test]
    fn an_encoded_key_without_text_does_not_swallow_the_next_keys_text() {
        let none = Modifiers::NONE;
        let ctrl = Modifiers { ctrl: true, ..none };
        // Ctrl+U then "x" in one frame: egui sends no text for the chord.
        let events = vec![press(Key::U, ctrl), press(Key::X, none), Event::Text("x".into())];
        assert_eq!(typed(b"", events.clone()), b"\x15x");
        assert_eq!(typed(b"\x1b[>1u", events), b"\x1b[117;5ux");
        let events = vec![press(Key::Escape, none), press(Key::Space, none), Event::Text(" ".into())];
        assert_eq!(typed(b"\x1b[>1u", events), b"\x1b[27u ");
    }
}
