//! The find bar: one adaptive bar on a panel. It searches as you type, counts
//! "N of M", steps both ways, and offers replace only where the content can change.

use egui::text::{CCursor, CCursorRange};
use egui::{Id, Key, Modifiers, RichText, Ui};
use throng_core::ids::PanelId;
use throng_editor::find::Query;

/// What the bar edits.
pub struct Bar<'a> {
    pub panel: PanelId,
    pub query: &'a mut Query,
    /// The replacement text and whether its row is open; `None` where nothing can be replaced (a
    /// terminal).
    pub replace: Option<(&'a mut String, &'a mut bool)>,
    pub current: Option<usize>,
    pub total: usize,
    /// Focus (and select) the find input this frame.
    pub focus: &'a mut bool,
    pub focus_replace: &'a mut bool,
}

/// What the user did in the bar.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BarOut {
    /// The term or a mode changed: search again.
    pub changed: bool,
    /// Next (`true`) or previous match.
    pub step: Option<bool>,
    pub close: bool,
    pub replace_one: bool,
    pub replace_all: bool,
}

/// `12345` → `12,345`, for display only.
#[must_use]
pub fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Give an icon control a spoken name (the glyph alone reads as noise to a screen reader).
fn named(response: egui::Response, name: &str, selected: Option<bool>) -> egui::Response {
    response.widget_info(|| match selected {
        Some(on) => egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, name),
        None => egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name),
    });
    response
}

/// Give a text input an accessible name (a hint is not a name).
pub fn name_input(ui: &Ui, response: &egui::Response, name: &str) {
    ui.ctx().accesskit_node_builder(response.id, |node| node.set_label(name));
}

/// The id of a panel's find input, so callers can tell whether the bar has focus.
#[must_use]
pub fn input_id(panel: PanelId) -> Id {
    Id::new(("find-input", panel))
}

fn replace_id(panel: PanelId) -> Id {
    Id::new(("find-replace", panel))
}

/// Whether keyboard focus is in this panel's bar.
#[must_use]
pub fn has_focus(ui: &Ui, panel: PanelId) -> bool {
    ui.memory(|m| m.has_focus(input_id(panel)) || m.has_focus(replace_id(panel)))
}

fn select_all(ui: &Ui, id: Id, len: usize) {
    let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(len))));
    state.store(ui.ctx(), id);
}

/// Draw the bar.
pub fn show(ui: &mut Ui, bar: Bar<'_>) -> BarOut {
    let mut out = BarOut::default();
    let Bar { panel, query, mut replace, current, total, focus, focus_replace } = bar;
    let input = input_id(panel);
    // Alt+Enter replaces the current match, Ctrl+Alt+Enter all of them, from anywhere in the bar.
    if replace.as_ref().is_some_and(|(_, open)| **open) && has_focus(ui, panel) {
        if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::Enter)) {
            out.replace_all = true;
        } else if ui.input_mut(|i| i.consume_key(Modifiers::ALT, Key::Enter)) {
            out.replace_one = true;
        }
    }
    egui::Frame::new().fill(ui.visuals().faint_bg_color).inner_margin(egui::Margin::symmetric(6, 4)).show(
        ui,
        |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if let Some((_, open)) = replace.as_mut() {
                    let glyph = crate::icons::glyph(ui.ctx(), if **open { "chevronOpen" } else { "chevron" });
                    let hint = if **open {
                        "Hide replace".to_owned()
                    } else {
                        match crate::keymap::label(ui.ctx(), "search.replace") {
                            chord if chord.is_empty() => "Show replace".to_owned(),
                            chord => format!("Show replace ({chord})"),
                        }
                    };
                    let name = if **open { "Hide replace" } else { "Show replace" };
                    if named(ui.small_button(glyph), name, None).on_hover_text(hint).clicked() {
                        **open = !**open;
                    }
                }
                if *focus {
                    select_all(ui, input, query.term.chars().count());
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut query.term)
                        .id(input)
                        .hint_text("Find")
                        .desired_width(220.0),
                );
                if std::mem::take(focus) {
                    response.request_focus();
                }
                if response.changed() {
                    out.changed = true;
                }
                if response.lost_focus() {
                    let (enter, shift, escape) = ui.input(|i| {
                        (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape))
                    });
                    if enter {
                        out.step = Some(!shift);
                        response.request_focus();
                    } else if escape {
                        out.close = true;
                    }
                }
                let case = named(
                    ui.selectable_label(query.case_sensitive, RichText::new("Aa").monospace()),
                    "Match case",
                    Some(query.case_sensitive),
                )
                .on_hover_text("Match case");
                if case.clicked() {
                    query.case_sensitive = !query.case_sensitive;
                    out.changed = true;
                }
                let word = named(
                    ui.selectable_label(query.whole_word, RichText::new("ab").monospace().underline()),
                    "Match whole word",
                    Some(query.whole_word),
                )
                .on_hover_text("Match whole word");
                if word.clicked() {
                    query.whole_word = !query.whole_word;
                    out.changed = true;
                }
                let count = if query.term.is_empty() {
                    String::new()
                } else if total == 0 {
                    "No results".to_owned()
                } else {
                    match current {
                        Some(i) => format!("{} of {}", grouped(i + 1), grouped(total)),
                        None => format!("{} found", grouped(total)),
                    }
                };
                ui.add_sized([96.0, 18.0], egui::Label::new(RichText::new(count).small()));
                let previous = named(
                    ui.add_enabled(
                        total > 0,
                        egui::Button::new(crate::icons::glyph(ui.ctx(), "findPrevious")).small(),
                    ),
                    "Previous match",
                    None,
                );
                if previous.on_hover_text("Previous (Shift+F3)").clicked() {
                    out.step = Some(false);
                }
                let next = named(
                    ui.add_enabled(
                        total > 0,
                        egui::Button::new(crate::icons::glyph(ui.ctx(), "findNext")).small(),
                    ),
                    "Next match",
                    None,
                );
                if next.on_hover_text("Next (F3)").clicked() {
                    out.step = Some(true);
                }
                if named(ui.small_button(crate::icons::glyph(ui.ctx(), "dismiss")), "Close find", None)
                    .on_hover_text("Close (Escape)")
                    .clicked()
                {
                    out.close = true;
                }
            });
            if let Some((text, open)) = replace
                && *open
            {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.add_space(22.0);
                    let id = replace_id(panel);
                    let response = ui.add(
                        egui::TextEdit::singleline(text).id(id).hint_text("Replace").desired_width(220.0),
                    );
                    name_input(ui, &response, "Replace with");
                    if std::mem::take(focus_replace) {
                        response.request_focus();
                    }
                    if response.lost_focus() {
                        let (enter, escape) =
                            ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                        if enter {
                            out.replace_one = true;
                            response.request_focus();
                        } else if escape {
                            out.close = true;
                        }
                    }
                    if ui
                        .add_enabled(total > 0, egui::Button::new("Replace"))
                        .on_hover_text("Alt+Enter")
                        .clicked()
                    {
                        out.replace_one = true;
                    }
                    if ui
                        .add_enabled(total > 0, egui::Button::new("Replace All"))
                        .on_hover_text("Ctrl+Alt+Enter")
                        .clicked()
                    {
                        out.replace_all = true;
                    }
                });
            }
        },
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_grouped_for_display() {
        assert_eq!(grouped(7), "7");
        assert_eq!(grouped(1234), "1,234");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
