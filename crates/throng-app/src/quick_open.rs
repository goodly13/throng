//! Quick Open: type part of a path, pick a file.
//!
//! Every whitespace-separated term must be a case-insensitive substring of the file's path, in
//! any order. A hit in the file name ranks above a hit only in its folders, an earlier hit above a
//! later one, and ties keep the index's order so the list never reshuffles under the arrow keys.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{Key, RichText, TextFormat, Ui};
use throng_core::ids::PanelId;

use crate::project_files::ProjectFile;

/// Rows drawn at most.
pub const SHOWN: usize = 200;

/// One result: the file's index and the character spans that matched, for highlighting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub spans: Vec<Range<usize>>,
}

/// Length-preserving case folding (a lower-cased copy can change length).
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn find_chars(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Rank `files` against `query`, keeping at most [`SHOWN`]; the flag says more matched.
#[must_use]
pub fn rank(files: &[ProjectFile], query: &str, include_hidden: bool) -> (Vec<Hit>, bool) {
    let terms: Vec<Vec<char>> = query.split_whitespace().map(|t| t.chars().map(fold).collect()).collect();
    let visible = files.iter().enumerate().filter(|(_, f)| include_hidden || !f.hidden);
    if terms.is_empty() {
        let hits: Vec<Hit> =
            visible.map(|(index, _)| Hit { index, spans: Vec::new() }).take(SHOWN + 1).collect();
        let more = hits.len() > SHOWN;
        return (hits.into_iter().take(SHOWN).collect(), more);
    }
    let mut scored: Vec<((usize, usize, usize), Hit)> = Vec::new();
    for (index, file) in visible {
        let path: Vec<char> = file.rel.chars().map(fold).collect();
        let name_start = path.iter().rposition(|c| *c == '/').map_or(0, |i| i + 1);
        let mut spans = Vec::with_capacity(terms.len());
        let mut in_name = 0usize;
        let mut earliest = usize::MAX;
        let mut all = true;
        for term in &terms {
            // Prefer the hit in the file name; fall back to anywhere in the path.
            let hit = find_chars(&path[name_start..], term)
                .map(|i| (i + name_start, true))
                .or_else(|| find_chars(&path, term).map(|i| (i, false)));
            let Some((at, named)) = hit else {
                all = false;
                break;
            };
            if named {
                in_name += 1;
                earliest = earliest.min(at - name_start);
            } else {
                earliest = earliest.min(path.len() + at);
            }
            spans.push(at..at + term.len());
        }
        if all {
            spans.sort_by_key(|s| s.start);
            scored.push(((usize::MAX - in_name, earliest, index), Hit { index, spans }));
        }
    }
    scored.sort_by_key(|(key, _)| *key);
    let more = scored.len() > SHOWN;
    (scored.into_iter().take(SHOWN).map(|(_, hit)| hit).collect(), more)
}

/// Quick Open's state while it is on screen.
pub struct QuickOpen {
    pub query: String,
    pub selected: usize,
    pub include_hidden: bool,
    pub files: Arc<Vec<ProjectFile>>,
    pub building: bool,
    /// The editor it was opened from, and whether to open there instead of in a new editor.
    pub target: Option<(PanelId, String)>,
    pub into_active: bool,
    cache: Option<(String, bool, usize, Vec<Hit>, bool)>,
    focus_target: bool,
}

impl QuickOpen {
    #[must_use]
    pub fn new(
        files: Arc<Vec<ProjectFile>>,
        include_hidden: bool,
        target: Option<(PanelId, String)>,
    ) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            include_hidden,
            files,
            building: false,
            target,
            into_active: false,
            cache: None,
            focus_target: false,
        }
    }

    fn hits(&mut self) -> (Vec<Hit>, bool) {
        let key = (self.query.clone(), self.include_hidden, Arc::as_ptr(&self.files) as usize);
        match &self.cache {
            Some((q, h, f, hits, more)) if (q, h, f) == (&key.0, &key.1, &key.2) => (hits.clone(), *more),
            _ => {
                let (hits, more) = rank(&self.files, &self.query, self.include_hidden);
                self.cache = Some((key.0, key.1, key.2, hits.clone(), more));
                (hits, more)
            }
        }
    }
}

/// The file chosen, and the editor to open it in (`None`: a new one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub path: PathBuf,
    pub into: Option<PanelId>,
}

/// Draw Quick Open; returns the choice when the user picks a file.
pub fn show(ui: &mut Ui, state: &mut QuickOpen) -> Option<Choice> {
    ui.heading("Quick Open");
    let (hits, more) = state.hits();
    let mut chosen = None;
    let response = ui.add(
        egui::TextEdit::singleline(&mut state.query)
            .hint_text("File name or part of a path")
            .desired_width(f32::INFINITY),
    );
    crate::find_bar::name_input(ui, &response, "Quick Open");
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
    if !submitted && !state.focus_target {
        response.request_focus();
    }
    ui.horizontal(|ui| {
        let hidden = ui.checkbox(&mut state.include_hidden, "Include files hidden in this project");
        if hidden.changed() {
            state.selected = 0;
        }
        if let Some((_, name)) = &state.target {
            let label = if state.into_active {
                format!("Will open in the active editor ({name})")
            } else {
                "Will open in a new editor".to_owned()
            };
            let toggle = ui.selectable_label(state.into_active, label);
            if toggle.clicked() {
                state.into_active = !state.into_active;
            }
            state.focus_target = toggle.has_focus();
        }
    });
    if state.building && state.files.is_empty() {
        ui.weak("Still listing the project's files…");
    }
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show_rows(
        ui,
        row_h,
        hits.len(),
        |ui, rows| {
            for i in rows {
                let hit = &hits[i];
                let Some(file) = state.files.get(hit.index) else { continue };
                let text = highlighted(ui, &file.rel, &hit.spans);
                let row =
                    ui.add(egui::Button::selectable(i == state.selected, text).frame_when_inactive(false));
                if i == state.selected && (down || up) {
                    row.scroll_to_me(None);
                }
                if row.clicked() {
                    chosen = Some(i);
                }
            }
        },
    );
    if more {
        ui.weak(format!("Showing the first {SHOWN}. Type more to narrow the list."));
    } else if hits.is_empty() && !state.query.trim().is_empty() {
        ui.weak("No files match.");
    }
    if submitted && !hits.is_empty() {
        chosen = Some(state.selected.min(hits.len() - 1));
    }
    let into = if state.into_active { state.target.as_ref().map(|(p, _)| *p) } else { None };
    chosen
        .and_then(|i| hits.get(i))
        .and_then(|h| state.files.get(h.index))
        .map(|f| Choice { path: f.path.clone(), into })
}

/// The path with its matched spans in bold.
fn highlighted(ui: &Ui, rel: &str, spans: &[Range<usize>]) -> LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let normal =
        TextFormat { font_id: font.clone(), color: ui.visuals().text_color(), ..TextFormat::default() };
    let strong = TextFormat {
        font_id: font,
        color: ui.visuals().strong_text_color(),
        underline: egui::Stroke::new(1.0, ui.visuals().hyperlink_color),
        ..TextFormat::default()
    };
    let mut job = LayoutJob::default();
    let chars: Vec<char> = rel.chars().collect();
    let mut i = 0;
    for span in spans {
        if span.start > i {
            job.append(&chars[i..span.start].iter().collect::<String>(), 0.0, normal.clone());
        }
        let from = span.start.max(i);
        if span.end > from {
            job.append(&chars[from..span.end].iter().collect::<String>(), 0.0, strong.clone());
        }
        i = i.max(span.end);
    }
    if i < chars.len() {
        job.append(&chars[i..].iter().collect::<String>(), 0.0, normal);
    }
    let _ = RichText::new("");
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<ProjectFile> {
        paths
            .iter()
            .map(|p| ProjectFile { rel: (*p).into(), path: PathBuf::from(p), hidden: false })
            .collect()
    }

    fn ranked(list: &[ProjectFile], q: &str) -> Vec<String> {
        rank(list, q, false).0.into_iter().map(|h| list[h.index].rel.clone()).collect()
    }

    #[test]
    fn every_term_must_match_somewhere_in_any_order() {
        let list = files(&["src/app/main.rs", "src/lib.rs", "docs/main.md"]);
        assert_eq!(ranked(&list, "main src"), vec!["src/app/main.rs"]);
        // Both hit in the file name; `lib.rs` hits earlier in it.
        assert_eq!(ranked(&list, "RS"), vec!["src/lib.rs", "src/app/main.rs"]);
        assert!(ranked(&list, "nothing").is_empty());
    }

    #[test]
    fn a_name_hit_beats_a_folder_hit_and_earlier_beats_later() {
        let list = files(&["app/other.rs", "src/app.rs", "x/zapp.rs"]);
        // "app" is in the file name of the last two; the first only has it in a folder.
        assert_eq!(ranked(&list, "app"), vec!["src/app.rs", "x/zapp.rs", "app/other.rs"]);
    }

    #[test]
    fn ties_keep_the_index_order() {
        let list = files(&["b/test.rs", "a/test.rs"]);
        assert_eq!(ranked(&list, "test"), vec!["b/test.rs", "a/test.rs"]);
    }

    #[test]
    fn spans_point_at_the_real_characters() {
        let list = files(&["İx/ix.txt"]);
        let (hits, _) = rank(&list, "ix", false);
        assert_eq!(hits[0].spans, vec![3..5], "the name hit, in characters of the original path");
    }

    #[test]
    fn hidden_files_only_on_request_and_the_list_is_capped() {
        let mut list = files(&["a.txt"]);
        list.push(ProjectFile { rel: "gen/b.txt".into(), path: "gen/b.txt".into(), hidden: true });
        assert_eq!(rank(&list, "txt", false).0.len(), 1);
        assert_eq!(rank(&list, "txt", true).0.len(), 2);
        let many: Vec<ProjectFile> = (0..500)
            .map(|i| ProjectFile { rel: format!("f{i}.rs"), path: PathBuf::new(), hidden: false })
            .collect();
        let (hits, more) = rank(&many, "", false);
        assert_eq!((hits.len(), more), (SHOWN, true));
    }
}
