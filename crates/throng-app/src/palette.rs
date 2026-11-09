//! The command palette (FR-057): every command live where the keyboard is, with its chord, and
//! every project and tab by name. Typing filters; Enter runs the one selected.

use egui::{Key, RichText, Ui};
use throng_core::ids::{ProjectId, TabId};

use crate::quick_open::{find_chars, fold};

/// What a row stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemId {
    Command(&'static str),
    Project(ProjectId),
    Tab(TabId),
}

/// One row: what it reads and, at its right, its chord or kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    pub label: String,
    pub detail: String,
}

/// Rank `items` against `query`: every whitespace-separated term must be a case-blind substring of
/// the label, in any order. An earlier first hit ranks higher; ties keep the items' order.
#[must_use]
pub fn rank(items: &[Item], query: &str) -> Vec<usize> {
    let terms: Vec<Vec<char>> = query.split_whitespace().map(|t| t.chars().map(fold).collect()).collect();
    if terms.is_empty() {
        return (0..items.len()).collect();
    }
    let mut scored: Vec<(usize, usize)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let label: Vec<char> = item.label.chars().map(fold).collect();
        let mut earliest = usize::MAX;
        let mut all = true;
        for term in &terms {
            match find_chars(&label, term) {
                Some(at) => earliest = earliest.min(at),
                None => {
                    all = false;
                    break;
                }
            }
        }
        if all {
            scored.push((earliest, index));
        }
    }
    scored.sort_unstable();
    scored.into_iter().map(|(_, index)| index).collect()
}

/// The palette while it is on screen.
pub struct Palette {
    pub query: String,
    pub selected: usize,
    items: Vec<Item>,
}

impl Palette {
    #[must_use]
    pub fn new(items: Vec<Item>) -> Self {
        Self { query: String::new(), selected: 0, items }
    }
}

/// Draw the palette; the item chosen, if any.
pub fn show(ui: &mut Ui, state: &mut Palette) -> Option<ItemId> {
    ui.heading("Command Palette");
    let hits = rank(&state.items, &state.query);
    let response = ui.add(
        egui::TextEdit::singleline(&mut state.query)
            .hint_text("A command, a project or a tab")
            .desired_width(f32::INFINITY),
    );
    crate::find_bar::name_input(ui, &response, "Command Palette");
    if response.changed() {
        state.selected = 0;
    }
    // Arrows move through the list while the input keeps focus.
    let (down, up, enter) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.key_pressed(Key::Enter),
        )
    });
    if down {
        state.selected = (state.selected + 1).min(hits.len().saturating_sub(1));
    }
    if up {
        state.selected = state.selected.saturating_sub(1);
    }
    let submitted = response.lost_focus() && enter;
    if !submitted {
        response.request_focus();
    }
    let mut chosen = None;
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show_rows(
        ui,
        row_h,
        hits.len(),
        |ui, rows| {
            for i in rows {
                let item = &state.items[hits[i]];
                let atoms =
                    (RichText::new(&item.label), egui::Atom::grow(), RichText::new(&item.detail).weak());
                let row =
                    ui.add(egui::Button::selectable(i == state.selected, atoms).frame_when_inactive(false));
                if i == state.selected && (down || up) {
                    row.scroll_to_me(None);
                }
                if row.clicked() {
                    chosen = Some(i);
                }
            }
        },
    );
    if hits.is_empty() && !state.query.trim().is_empty() {
        ui.weak("Nothing matches.");
    }
    if submitted && !hits.is_empty() {
        chosen = Some(state.selected.min(hits.len() - 1));
    }
    chosen.and_then(|i| hits.get(i)).map(|&i| state.items[i].id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Item {
        Item { id: ItemId::Command("x"), label: label.into(), detail: String::new() }
    }

    #[test]
    fn rank_prefers_an_earlier_hit_and_needs_every_term() {
        let items = vec![
            item("Editor: Save"),
            item("Editor: Save all"),
            item("Alpha project"),
            item("Project: Beta"),
            item("View: Toggle File Explorer"),
        ];
        assert_eq!(rank(&items, ""), vec![0, 1, 2, 3, 4], "no query: everything, in order");
        assert_eq!(rank(&items, "save"), vec![0, 1]);
        assert_eq!(rank(&items, "all save"), vec![1], "every term, in any order");
        assert_eq!(rank(&items, "PROJECT"), vec![3, 2], "case-blind; the earlier hit first");
        assert_eq!(rank(&items, "toggle expl"), vec![4]);
        assert_eq!(rank(&items, "zzz"), Vec::<usize>::new());
    }
}
