//! The one-line status strips under editors and terminals. An editor's carries
//! readouts on the left — caret, selection, counts — and its actions on the right: language,
//! word wrap and, for a file that has one, its preview. A terminal's names its shell
//! and where it is working.

use egui::{Align, Layout, RichText, Sense, Ui, WidgetInfo, WidgetType};

/// The strip's height: one line of small text.
pub const HEIGHT: f32 = 22.0;

/// What an editor strip shows.
pub struct EditorStrip<'a> {
    pub line: usize,
    pub column: usize,
    /// Selected characters in UTF-16 units, 0 when nothing is selected.
    pub selected: usize,
    pub carets: usize,
    /// Characters and words; `None` when the preference hides them.
    pub counts: Option<(usize, usize)>,
    pub show_position: bool,
    pub language: &'a str,
    pub format: &'a str,
    pub wrap: bool,
    /// The file has a preview, and whether it is open.
    pub preview: Option<bool>,
}

/// What the user asked of an editor strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorStripOutput {
    pub pick_language: bool,
    pub toggle_wrap: bool,
    pub open_preview: bool,
}

/// A readout: an abbreviated label with its figure, named in full for assistive technology.
/// Readouts are not actions.
fn readout(ui: &mut Ui, text: String, spoken: String) {
    let response = ui.add(egui::Label::new(RichText::new(text).small().weak()).selectable(false));
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &spoken));
}

/// One readout segment: its label's two forms, its figure, and what it is called aloud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub full: &'static str,
    pub short: &'static str,
    pub figure: String,
    pub spoken: String,
}

/// The readouts to draw in `width`: labels shorten first, in the hide order,
/// then whole segments go in that order — words, characters, selection, column, line. A figure
/// never appears without its label, and the result depends only on the width.
#[must_use]
pub fn plan(
    segments: &[Segment],
    width: f32,
    gap: f32,
    measure: impl Fn(&str) -> f32,
) -> Vec<(String, String)> {
    // Segments arrive in display order; the hide order is the reverse of it.
    let mut short = vec![false; segments.len()];
    let mut shown = vec![true; segments.len()];
    let text = |i: usize, short: &[bool]| {
        let s = &segments[i];
        format!("{} {}", if short[i] { s.short } else { s.full }, s.figure)
    };
    let total = |short: &[bool], shown: &[bool]| {
        let widths: Vec<f32> =
            (0..segments.len()).filter(|i| shown[*i]).map(|i| measure(&text(i, short))).collect();
        widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32
    };
    for i in (0..segments.len()).rev() {
        if total(&short, &shown) <= width {
            break;
        }
        short[i] = segments[i].short != segments[i].full;
    }
    for i in (0..segments.len()).rev() {
        if total(&short, &shown) <= width {
            break;
        }
        shown[i] = false;
    }
    (0..segments.len()).filter(|i| shown[*i]).map(|i| (text(i, &short), segments[i].spoken.clone())).collect()
}

fn frame(ui: &mut Ui, contents: impl FnOnce(&mut Ui)) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    ui.painter().hline(rect.x_range(), rect.top(), ui.visuals().widgets.noninteractive.bg_stroke);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(6.0, 1.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.set_clip_rect(rect);
    child.spacing_mut().item_spacing.x = 10.0;
    contents(&mut child);
}

/// Draw an editor's strip in the current `ui` (which should be [`HEIGHT`] tall).
pub fn editor(ui: &mut Ui, strip: &EditorStrip<'_>) -> EditorStripOutput {
    let mut out = EditorStripOutput::default();
    frame(ui, |ui| {
        // Actions on the right, laid out first so the readouts give way when the panel is narrow.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(open) = strip.preview {
                let button = egui::Button::selectable(open, RichText::new("Preview").small())
                    .frame_when_inactive(false);
                let hint =
                    if open { "Show this file's preview" } else { "Open a preview beside this editor" };
                if ui.add(button).on_hover_text(hint).clicked() {
                    out.open_preview = true;
                }
            }
            let wrap = egui::Button::selectable(strip.wrap, RichText::new("Wrap").small())
                .frame_when_inactive(false);
            if ui
                .add(wrap)
                .on_hover_text(match crate::keymap::label(ui.ctx(), "editor.toggleWordWrap") {
                    chord if chord.is_empty() => "Word wrap".to_owned(),
                    chord => format!("Word wrap ({chord})"),
                })
                .clicked()
            {
                out.toggle_wrap = true;
            }
            ui.label(RichText::new(strip.format).small().weak());
            let language =
                egui::Button::new(RichText::new(strip.language).small()).frame_when_inactive(false);
            if ui.add(language).on_hover_text("Set the language").clicked() {
                out.pick_language = true;
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let g = crate::find_bar::grouped;
                let mut segments = Vec::new();
                if strip.show_position {
                    segments.push(Segment {
                        full: "Ln",
                        short: "Ln",
                        figure: g(strip.line),
                        spoken: format!("line {}", strip.line),
                    });
                    segments.push(Segment {
                        full: "Col",
                        short: "Col",
                        figure: g(strip.column),
                        spoken: format!("column {}", strip.column),
                    });
                    // Absent, not zero, when nothing is selected.
                    if strip.selected > 0 {
                        segments.push(Segment {
                            full: "selected",
                            short: "sel",
                            figure: g(strip.selected),
                            spoken: format!("{} characters selected", strip.selected),
                        });
                    }
                }
                if let Some((chars, words)) = strip.counts {
                    segments.push(Segment {
                        full: "chars",
                        short: "ch",
                        figure: g(chars),
                        spoken: format!("{chars} characters"),
                    });
                    segments.push(Segment {
                        full: "words",
                        short: "w",
                        figure: g(words),
                        spoken: format!("{words} words"),
                    });
                }
                let font = egui::TextStyle::Small.resolve(ui.style());
                let measure = |text: &str| {
                    ui.fonts_mut(|f| {
                        f.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE).size().x
                    })
                };
                let gap = ui.spacing().item_spacing.x;
                for (text, spoken) in plan(&segments, ui.available_width(), gap, measure) {
                    readout(ui, text, spoken);
                }
            });
        });
    });
    out
}

/// Draw a terminal's strip: its shell, where it is working, and its grid.
pub fn terminal(ui: &mut Ui, shell: &str, directory: Option<&str>, size: (u16, u16)) {
    frame(ui, |ui| {
        ui.label(RichText::new(shell).small());
        if let Some(directory) = directory {
            let response = ui
                .add(
                    egui::Label::new(RichText::new(directory).small().weak())
                        .truncate()
                        .sense(Sense::hover()),
                )
                .on_hover_text("Where the shell is working");
            response.widget_info(|| {
                WidgetInfo::labeled(WidgetType::Label, true, format!("working in {directory}"))
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            readout(ui, format!("{}×{}", size.0, size.1), format!("{} columns by {} rows", size.0, size.1));
        });
    });
}

/// A working directory as the strip shows it: relative to the project root when inside it.
#[must_use]
pub fn directory_label(root: &std::path::Path, dir: &std::path::Path) -> String {
    match dir.strip_prefix(root) {
        Ok(rest) if rest.as_os_str().is_empty() => "./".to_owned(),
        Ok(rest) => format!("./{}", rest.display()),
        Err(_) => dir.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn segment(full: &'static str, short: &'static str, figure: &str) -> Segment {
        Segment { full, short, figure: figure.into(), spoken: full.into() }
    }

    #[test]
    fn narrow_strips_shorten_labels_then_drop_segments_in_the_fixed_order() {
        let segments = [
            segment("Ln", "Ln", "12"),
            segment("Col", "Col", "4"),
            segment("selected", "sel", "3"),
            segment("chars", "ch", "1,234"),
            segment("words", "w", "200"),
        ];
        // Each character is 1 wide, with a gap of 1.
        let measure = |t: &str| t.chars().count() as f32;
        let texts = |w: f32| plan(&segments, w, 1.0, measure).into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        assert_eq!(texts(100.0), ["Ln 12", "Col 4", "selected 3", "chars 1,234", "words 200"]);
        // Shortening comes first, words first.
        assert_eq!(texts(40.0), ["Ln 12", "Col 4", "selected 3", "chars 1,234", "w 200"]);
        assert_eq!(texts(34.0), ["Ln 12", "Col 4", "sel 3", "ch 1,234", "w 200"]);
        // Then whole segments go: words, characters, selection, column, line.
        assert_eq!(texts(27.0), ["Ln 12", "Col 4", "sel 3", "ch 1,234"]);
        assert_eq!(texts(12.0), ["Ln 12", "Col 4"]);
        assert_eq!(texts(5.0), ["Ln 12"]);
        assert_eq!(texts(2.0), Vec::<String>::new());
    }

    #[test]
    fn directories_read_relative_to_the_project() {
        assert_eq!(directory_label(Path::new("/p"), Path::new("/p")), "./");
        assert_eq!(directory_label(Path::new("/p"), Path::new("/p/src")), "./src");
        assert_eq!(directory_label(Path::new("/p"), Path::new("/tmp")), "/tmp");
    }
}
