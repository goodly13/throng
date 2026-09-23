//! The Find in Files panel: a query that survives a restart, results that do not.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use egui::text::LayoutJob;
use egui::{Color32, Key, RichText, Stroke, TextFormat, Ui};
use throng_core::ids::PanelId;
use throng_core::workspace::SearchPanelConfig;
use throng_editor::find::Query;

use crate::file_search::{FileHits, Scan, Spec, Update};
use crate::find_bar::grouped;
use crate::project_files::Exclusions;

/// Where a scan stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Scanning,
    Done {
        files: usize,
        skipped: usize,
        capped: bool,
    },
    Refused(String),
}

/// Which replacements to commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Commit {
    All,
    File(usize),
    Match(usize, usize),
}

/// A panel's live state: results and the scan producing them (never persisted).
#[derive(Default)]
pub struct SearchState {
    pub scan: Scan,
    pub results: Vec<FileHits>,
    pub status: Status,
    /// When the query last changed (search as you type once it settles).
    pub edited_at: Option<Instant>,
    /// When the current burst of typing began (a scan is forced at 4× the settle time).
    burst_from: Option<Instant>,
    pub collapsed: HashSet<String>,
    pub selected: Option<(usize, usize)>,
    /// Focus the find (`false`) or replace (`true`) input on the next frame.
    pub focus: Option<bool>,
    /// The last commit's outcome, shown under the query.
    pub summary: Option<String>,
}

impl SearchState {
    /// Search again now.
    pub fn run(
        &mut self,
        config: &SearchPanelConfig,
        root: PathBuf,
        exclusions: Exclusions,
        max_bytes: u64,
        wake: impl Fn() + Send + 'static,
    ) {
        self.edited_at = None;
        self.burst_from = None;
        self.results.clear();
        self.selected = None;
        self.summary = None;
        if config.term.is_empty() {
            // An empty term matches nothing and starts no scan.
            self.scan.cancel();
            self.status = Status::Idle;
            return;
        }
        self.status = Status::Scanning;
        let query = Query {
            term: config.term.clone(),
            case_sensitive: config.case_sensitive,
            whole_word: config.whole_word,
        };
        self.scan.start(Spec { root, scope: config.scope.clone(), query, exclusions, max_bytes }, wake);
    }

    /// The query changed: search once typing settles.
    pub fn edited(&mut self) {
        let now = Instant::now();
        self.edited_at = Some(now);
        self.burst_from.get_or_insert(now);
    }

    /// Whether a settled (or long-running) edit is due for a scan, or how long to wait.
    fn due(&self, settle: Duration) -> Result<bool, Duration> {
        let Some(at) = self.edited_at else { return Ok(false) };
        let quiet = at.elapsed();
        let burst = self.burst_from.map_or(Duration::ZERO, |b| b.elapsed());
        if quiet >= settle || burst >= settle * 4 {
            Ok(true)
        } else {
            Err((settle - quiet).min((settle * 4).saturating_sub(burst)))
        }
    }

    /// Take updates from the scan.
    pub fn poll(&mut self) {
        for update in self.scan.poll() {
            match update {
                Update::Found(hits) => self.results.extend(hits),
                Update::Done { files, skipped, capped } => {
                    self.status = Status::Done { files, skipped, capped }
                }
                Update::Refused(reason) => self.status = Status::Refused(reason),
            }
        }
    }

    #[must_use]
    pub fn match_count(&self) -> usize {
        self.results.iter().map(|f| f.matches.len()).sum()
    }
}

/// What the panel asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchAction {
    /// Open a result in an editor, with the match selected.
    Open {
        path: PathBuf,
        from: usize,
        to: usize,
    },
    Commit(Commit),
    /// Run with the current query (after the app has what it needs).
    Run,
}

/// The panel's title: kind plus term.
#[must_use]
pub fn title(config: &SearchPanelConfig) -> String {
    let kind = if config.replace { "Find & Replace in Files" } else { "Find in Files" };
    if config.term.is_empty() { kind.to_owned() } else { format!("{kind} — {}", config.term) }
}

/// The id of a panel's search input.
#[must_use]
pub fn input_id(panel: PanelId) -> egui::Id {
    egui::Id::new(("search-input", panel))
}

enum Row {
    Folder(String),
    File(usize),
    Match(usize, usize),
}

fn rows(state: &SearchState, by_folder: bool) -> Vec<Row> {
    let mut out = Vec::new();
    let mut folder: Option<String> = None;
    for (fi, file) in state.results.iter().enumerate() {
        if by_folder {
            let dir = file.rel.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_owned());
            if folder.as_ref() != Some(&dir) {
                out.push(Row::Folder(dir.clone()));
                folder = Some(dir.clone());
            }
            if state.collapsed.contains(&format!("dir:{dir}")) {
                continue;
            }
        }
        out.push(Row::File(fi));
        if !state.collapsed.contains(&file.rel) {
            out.extend((0..file.matches.len()).map(|mi| Row::Match(fi, mi)));
        }
    }
    out
}

fn stale(file: &FileHits) -> bool {
    let now = std::fs::metadata(&file.path).ok().map(|m| (m.len(), m.modified().ok()));
    now != file.stamp && !(now.is_none() && file.stamp.is_none())
}

/// Draw the panel. Edits to the query go into `config`; the caller saves it with the layout.
#[allow(clippy::too_many_lines)]
pub fn show(
    ui: &mut Ui,
    panel: PanelId,
    config: &mut SearchPanelConfig,
    state: &mut SearchState,
    settle: Duration,
) -> Vec<SearchAction> {
    let mut actions = Vec::new();
    state.poll();
    match state.due(settle) {
        Ok(true) => actions.push(SearchAction::Run),
        Ok(false) => {}
        Err(wait) => ui.ctx().request_repaint_after(wait),
    }
    if state.status == Status::Scanning {
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
    let mut edited = false;
    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 6)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let glyph = crate::icons::atom(ui.ctx(), if config.replace { "chevronOpen" } else { "chevron" });
            let toggle = ui.small_button(glyph).on_hover_text(if config.replace {
                "Hide replace"
            } else {
                "Show replace"
            });
            toggle.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    if config.replace { "Hide replace" } else { "Show replace" },
                )
            });
            if toggle.clicked() {
                config.replace = !config.replace;
            }
            let input = ui.add(
                egui::TextEdit::singleline(&mut config.term)
                    .id(input_id(panel))
                    .hint_text("Find in files")
                    .desired_width(260.0),
            );
            crate::find_bar::name_input(ui, &input, "Find in files");
            if state.focus == Some(false) {
                input.request_focus();
                state.focus = None;
            }
            if input.changed() {
                edited = true;
            }
            if input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                actions.push(SearchAction::Run);
                input.request_focus();
            }
            let case = ui
                .selectable_label(config.case_sensitive, RichText::new("Aa").monospace())
                .on_hover_text("Match case");
            case.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    config.case_sensitive,
                    "Match case",
                )
            });
            if case.clicked() {
                config.case_sensitive = !config.case_sensitive;
                edited = true;
            }
            let word = ui
                .selectable_label(config.whole_word, RichText::new("ab").monospace().underline())
                .on_hover_text("Match whole word");
            word.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    config.whole_word,
                    "Match whole word",
                )
            });
            if word.clicked() {
                config.whole_word = !config.whole_word;
                edited = true;
            }
        });
        if config.replace {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add_space(22.0);
                let id = egui::Id::new(("search-replace", panel));
                let input = ui.add(
                    egui::TextEdit::singleline(&mut config.replacement)
                        .id(id)
                        .hint_text("Replace with")
                        .desired_width(260.0),
                );
                crate::find_bar::name_input(ui, &input, "Replace with");
                if state.focus == Some(true) {
                    input.request_focus();
                    state.focus = None;
                }
                let can = state.match_count() > 0 && state.status != Status::Scanning;
                if ui.add_enabled(can, egui::Button::new("Replace All")).clicked() {
                    actions.push(SearchAction::Commit(Commit::All));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_space(22.0);
            ui.weak("in");
            let scope = ui.add(
                egui::TextEdit::singleline(&mut config.scope)
                    .hint_text("the whole project")
                    .desired_width(236.0),
            );
            crate::find_bar::name_input(ui, &scope, "Search in");
            if scope.changed() {
                edited = true;
            }
            if scope.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                actions.push(SearchAction::Run);
            }
        });
        ui.horizontal(|ui| {
            let status = match &state.status {
                Status::Idle => String::new(),
                Status::Scanning => format!("Searching… {} so far", grouped(state.match_count())),
                Status::Refused(reason) => reason.clone(),
                Status::Done { files, skipped, capped } => {
                    let n = state.match_count();
                    let mut s = if n == 0 {
                        format!("No results in {} files", grouped(*files))
                    } else {
                        format!("{} results in {} files", grouped(n), grouped(state.results.len()))
                    };
                    if *skipped > 0 {
                        s.push_str(&format!(" · {} skipped", grouped(*skipped)));
                    }
                    if *capped {
                        s.push_str(" · stopped at the first 20,000");
                    }
                    s
                }
            };
            let colour = if matches!(state.status, Status::Refused(_)) {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.colored_label(colour, status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut config.group_by_folder, "Group by folder");
            });
        });
        if let Some(summary) = &state.summary {
            ui.weak(summary);
        }
    });
    if edited {
        state.edited();
    }
    ui.separator();

    let list = rows(state, config.group_by_folder);
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let scanning = state.status == Status::Scanning;
    let mut toggle: Option<String> = None;
    egui::ScrollArea::vertical().id_salt(("search-results", panel)).auto_shrink([false, false]).show_rows(
        ui,
        row_h,
        list.len(),
        |ui, range| {
            for row in &list[range] {
                match *row {
                    Row::Folder(ref dir) => {
                        let key = format!("dir:{dir}");
                        let open = !state.collapsed.contains(&key);
                        let chevron = crate::icons::atom_with(
                            ui.ctx(),
                            if open { "chevronOpen" } else { "chevron" },
                            RichText::strong,
                        );
                        let name = if dir.is_empty() { "(project root)" } else { dir };
                        let token = if open { "chevronOpen" } else { "chevron" };
                        let spoken = format!("{} {name}", crate::icons::glyph(ui.ctx(), token));
                        let label = (chevron, RichText::new(format!(" {name}")).strong());
                        let response = ui.add(egui::Button::new(label).frame(false));
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &spoken)
                        });
                        if response.clicked() {
                            toggle = Some(key);
                        }
                    }
                    Row::File(fi) => {
                        let file = &state.results[fi];
                        let open = !state.collapsed.contains(&file.rel);
                        ui.horizontal(|ui| {
                            let shown = if config.group_by_folder {
                                file.rel.rsplit('/').next().unwrap_or(&file.rel)
                            } else {
                                &file.rel
                            };
                            let chevron = crate::icons::atom_with(
                                ui.ctx(),
                                if open { "chevronOpen" } else { "chevron" },
                                RichText::strong,
                            );
                            let token = if open { "chevronOpen" } else { "chevron" };
                            let spoken = format!("{} {shown}", crate::icons::glyph(ui.ctx(), token));
                            let label = (chevron, RichText::new(format!(" {shown}")).strong());
                            let response = ui.add(egui::Button::new(label).frame(false));
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &spoken)
                            });
                            if response.clicked() {
                                toggle = Some(file.rel.clone());
                            }
                            ui.weak(format!("({})", grouped(file.matches.len())));
                            if stale(file) {
                                ui.colored_label(ui.visuals().warn_fg_color, "changed since the search")
                                    .on_hover_text("Search again to refresh these results.");
                            }
                            if config.replace && !scanning && ui.small_button("Replace in File").clicked() {
                                actions.push(SearchAction::Commit(Commit::File(fi)));
                            }
                        });
                    }
                    Row::Match(fi, mi) => {
                        let file = &state.results[fi];
                        let m = &file.matches[mi];
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            ui.weak(format!("{:>5}", m.line + 1));
                            let job = snippet_job(
                                ui,
                                &m.snippet,
                                m.span,
                                config.replace.then_some(config.replacement.as_str()),
                            );
                            let selected = state.selected == Some((fi, mi));
                            let response = ui.add_enabled(
                                !scanning,
                                egui::Button::selectable(selected, job).frame_when_inactive(false),
                            );
                            if response.clicked() {
                                state.selected = Some((fi, mi));
                            }
                            if response.double_clicked()
                                || (selected
                                    && response.has_focus()
                                    && ui.input(|i| i.key_pressed(Key::Enter)))
                            {
                                actions.push(SearchAction::Open {
                                    path: file.path.clone(),
                                    from: m.from,
                                    to: m.to,
                                });
                            }
                            if config.replace && !scanning && ui.small_button("Replace").clicked() {
                                actions.push(SearchAction::Commit(Commit::Match(fi, mi)));
                            }
                        });
                    }
                }
            }
        },
    );
    if let Some(key) = toggle
        && !state.collapsed.remove(&key)
    {
        state.collapsed.insert(key);
    }
    actions
}

/// A result row: the match highlighted; in replace mode, struck through with its replacement
/// after it.
fn snippet_job(ui: &Ui, snippet: &str, span: (usize, usize), replacement: Option<&str>) -> LayoutJob {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let text = ui.visuals().text_color();
    let plain = TextFormat { font_id: font.clone(), color: text, ..TextFormat::default() };
    let hit = TextFormat {
        font_id: font.clone(),
        color: ui.visuals().strong_text_color(),
        background: Color32::from_rgba_unmultiplied(0xff, 0xc8, 0x00, 60),
        strikethrough: if replacement.is_some() { Stroke::new(1.0, text) } else { Stroke::NONE },
        ..TextFormat::default()
    };
    let chars: Vec<char> = snippet.chars().collect();
    let (a, b) = (span.0.min(chars.len()), span.1.min(chars.len()));
    let mut job = LayoutJob::default();
    job.append(&chars[..a].iter().collect::<String>(), 0.0, plain.clone());
    job.append(&chars[a..b].iter().collect::<String>(), 0.0, hit);
    if let Some(replacement) = replacement {
        let new = TextFormat {
            font_id: font,
            color: ui.visuals().strong_text_color(),
            background: Color32::from_rgba_unmultiplied(0x57, 0xab, 0x5a, 70),
            ..TextFormat::default()
        };
        job.append(if replacement.is_empty() { "(delete)" } else { replacement }, 0.0, new);
    }
    job.append(&chars[b..].iter().collect::<String>(), 0.0, plain);
    job
}

/// Whether a file was touched since `stamp` (for the app to decide after its own writes).
#[must_use]
pub fn stamp_of(path: &std::path::Path) -> Option<(u64, Option<SystemTime>)> {
    std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_name_the_kind_and_term() {
        let mut config = SearchPanelConfig::default();
        assert_eq!(title(&config), "Find in Files");
        config.term = "todo".into();
        config.replace = true;
        assert_eq!(title(&config), "Find & Replace in Files — todo");
    }

    #[test]
    fn a_scan_waits_for_typing_to_settle_but_not_forever() {
        let mut state = SearchState::default();
        assert_eq!(state.due(Duration::from_millis(500)), Ok(false));
        state.edited();
        assert!(state.due(Duration::from_millis(500)).is_err(), "still typing");
        state.burst_from = Some(Instant::now() - Duration::from_secs(3));
        assert_eq!(state.due(Duration::from_millis(500)), Ok(true), "forced at 4x the settle time");
    }
}
