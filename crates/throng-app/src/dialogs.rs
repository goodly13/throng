//! Modal dialogs. Each returns what the user chose; the app applies it.

use egui::{Color32, Context, Id, Key, Modal, RichText, Ui};
use throng_core::ids::{PanelId, ProjectId};
use throng_core::project::{Colour, PALETTE, ProjectError, ProjectField, ProjectInput};

/// A dialog on screen.
pub enum Dialog {
    Project(ProjectForm),
    DeleteProject {
        id: ProjectId,
        name: String,
        terminals: usize,
    },
    Trash {
        path: std::path::PathBuf,
    },
    RenamePanel {
        panel: PanelId,
        project: ProjectId,
        name: String,
    },
    RenameSubWorkspace {
        id: ProjectId,
        name: String,
    },
    /// Destroy a sub-workspace: its own panels end, mirrored ones carry on in their projects.
    DestroySubWorkspace {
        id: ProjectId,
        name: String,
        terminals: usize,
        mirrors: usize,
    },
    SaveAs {
        panel: PanelId,
        path: String,
        error: Option<String>,
    },
    ClosePanel {
        panel: PanelId,
        reason: String,
    },
    RunningTerminals {
        labels: Vec<String>,
    },
    /// Go To Line in an editor.
    GotoLine {
        panel: PanelId,
        input: String,
        lines: usize,
    },
    /// Choose an editor's language by hand.
    Language {
        panel: PanelId,
        filter: String,
        current: String,
        overridden: bool,
    },
    /// Quick Open.
    QuickOpen(crate::quick_open::QuickOpen),
    /// Replacing in files that are not open writes them straight to disk.
    ConfirmReplace {
        panel: PanelId,
        commit: crate::search_panel::Commit,
        files: usize,
        matches: usize,
    },
    About,
}

/// The new/edit project form.
pub struct ProjectForm {
    pub editing: Option<ProjectId>,
    pub name: String,
    pub colour: [u8; 3],
    pub root: String,
    pub error: Option<(ProjectField, String)>,
}

impl ProjectForm {
    #[must_use]
    pub fn new(index: usize, root: String) -> Self {
        let colour = Colour::parse(PALETTE[index % PALETTE.len()]).expect("palette colours parse");
        let name = std::path::Path::new(&root)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self { editing: None, name, colour: [colour.r, colour.g, colour.b], root, error: None }
    }

    #[must_use]
    pub fn input(&self) -> ProjectInput {
        let [r, g, b] = self.colour;
        ProjectInput {
            name: self.name.clone(),
            colour: Colour { r, g, b }.to_hex(),
            root: self.root.clone().into(),
        }
    }

    pub fn fail(&mut self, error: &ProjectError) {
        self.error = Some((error.field(), error.to_string()));
    }
}

/// The user's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    None,
    Cancel,
    SaveProject,
    DeleteProject(ProjectId),
    Trash,
    RenamePanel,
    RenameSubWorkspace,
    DestroySubWorkspace(ProjectId),
    SaveAs,
    ClosePanelAnyway(PanelId),
    LeaveRunning,
    TerminateAll,
    /// Go to this 1-based line.
    GotoLine(usize),
    /// Use this language, or `None` to detect it from the file name.
    Language(Option<String>),
    QuickOpen(crate::quick_open::Choice),
    ConfirmReplace,
}

/// Parse Go To Line's input: an optionally signed number, clamped to the document;
/// anything else is no answer.
#[must_use]
pub fn parse_line(input: &str, lines: usize) -> Option<usize> {
    let text = input.trim();
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let negative = text.starts_with('-');
    let value: usize = digits.parse().unwrap_or(usize::MAX);
    Some(if negative || value == 0 { 1 } else { value.min(lines.max(1)) })
}

/// Show the dialog and return the user's answer.
pub fn show(ctx: &Context, dialog: &mut Dialog) -> Answer {
    let mut answer = Answer::None;
    let modal = Modal::new(Id::new("throng-dialog")).show(ctx, |ui| {
        ui.set_min_width(420.0);
        answer = match dialog {
            Dialog::Project(form) => project_form(ui, form),
            Dialog::DeleteProject { id, name, terminals } => {
                ui.heading(format!("Delete \"{name}\"?"));
                ui.label("The project, its layout and its tabs are removed from throng. Files on disk are not touched.");
                if *terminals > 0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Its {terminals} terminal(s) will be ended."),
                    );
                }
                buttons(ui, &[("Delete Project", Answer::DeleteProject(*id)), ("Cancel", Answer::Cancel)])
            }
            Dialog::Trash { path } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.heading(format!("Move \"{name}\" to the trash?"));
                ui.label("You can restore it from the system trash.");
                buttons(ui, &[("Move to Trash", Answer::Trash), ("Cancel", Answer::Cancel)])
            }
            Dialog::RenamePanel { name, .. } => {
                ui.heading("Rename panel");
                let response = ui.text_edit_singleline(name);
                let submitted = submitted(ui, &response);
                ui.weak("Leave empty to go back to the default name.");
                if submitted {
                    Answer::RenamePanel
                } else {
                    buttons(ui, &[("Rename", Answer::RenamePanel), ("Cancel", Answer::Cancel)])
                }
            }
            Dialog::RenameSubWorkspace { name, .. } => {
                ui.heading("Rename sub-workspace");
                let response = ui.text_edit_singleline(name);
                if submitted(ui, &response) {
                    Answer::RenameSubWorkspace
                } else {
                    buttons(ui, &[("Rename", Answer::RenameSubWorkspace), ("Cancel", Answer::Cancel)])
                }
            }
            Dialog::DestroySubWorkspace { id, name, terminals, mirrors } => {
                ui.heading(format!("Destroy \"{name}\"?"));
                ui.label("The sub-workspace and its window go, with the panels it holds of its own.");
                if *terminals > 0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Its own {terminals} terminal(s) will be ended."),
                    );
                }
                if *mirrors > 0 {
                    ui.label(format!(
                        "The {mirrors} project panel(s) it shows are only closed here; they carry on in their projects."
                    ));
                }
                buttons(ui, &[("Destroy", Answer::DestroySubWorkspace(*id)), ("Cancel", Answer::Cancel)])
            }
            Dialog::SaveAs { path, error, .. } => {
                ui.heading("Save As");
                ui.label("Full path of the new file:");
                let response = ui.add(egui::TextEdit::singleline(path).desired_width(f32::INFINITY));
                let submitted = submitted(ui, &response);
                if let Some(error) = error {
                    ui.colored_label(ui.visuals().error_fg_color, error.as_str());
                }
                if submitted {
                    Answer::SaveAs
                } else {
                    buttons(ui, &[("Save", Answer::SaveAs), ("Cancel", Answer::Cancel)])
                }
            }
            Dialog::ClosePanel { panel, reason } => {
                ui.heading("Close this panel?");
                ui.label(reason.as_str());
                buttons(ui, &[("Close Panel", Answer::ClosePanelAnyway(*panel)), ("Cancel", Answer::Cancel)])
            }
            Dialog::RunningTerminals { labels } => {
                // Exactly three choices.
                ui.heading("Terminals are still running");
                ui.label("These terminals have a command running:");
                for label in labels.iter().take(8) {
                    ui.label(format!("  • {label}"));
                }
                if labels.len() > 8 {
                    ui.weak(format!("  … and {} more", labels.len() - 8));
                }
                ui.add_space(4.0);
                ui.label("Leave them running to reattach when you reopen throng, or end them now.");
                buttons(
                    ui,
                    &[
                        ("Leave Running", Answer::LeaveRunning),
                        ("Terminate All", Answer::TerminateAll),
                        ("Cancel", Answer::Cancel),
                    ],
                )
            }
            Dialog::GotoLine { input, lines, .. } => {
                ui.heading("Go to Line");
                let response = ui.add(
                    egui::TextEdit::singleline(input)
                        .hint_text(format!("1 – {lines}"))
                        .desired_width(f32::INFINITY),
                );
                let submitted = submitted(ui, &response);
                let parsed = parse_line(input, *lines);
                if submitted {
                    parsed.map_or(Answer::None, Answer::GotoLine)
                } else {
                    let go = parsed.map_or(Answer::None, Answer::GotoLine);
                    buttons(ui, &[("Go", go), ("Cancel", Answer::Cancel)])
                }
            }
            Dialog::Language { filter, current, overridden, .. } => language_picker(ui, filter, current, *overridden),
            Dialog::QuickOpen(state) => match crate::quick_open::show(ui, state) {
                Some(choice) => Answer::QuickOpen(choice),
                None => Answer::None,
            },
            Dialog::ConfirmReplace { files, matches, .. } => {
                ui.heading("Replace in files that are not open?");
                ui.label(format!(
                    "{matches} replacement(s) will be written straight to {files} file(s) that are not open. \
                     throng cannot undo those; open files can be undone as usual."
                ));
                buttons(ui, &[("Replace in Files", Answer::ConfirmReplace), ("Cancel", Answer::Cancel)])
            }
            Dialog::About => {
                ui.heading("throng");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("A project-first terminal & agent workspace.");
                ui.hyperlink_to("github.com/goodly13/throng", "https://github.com/goodly13/throng");
                ui.weak("Licensed under AGPL-3.0-only.");
                buttons(ui, &[("Close", Answer::Cancel)])
            }
        };
    });
    if modal.should_close() && answer == Answer::None {
        answer = Answer::Cancel;
    }
    answer
}

/// Keep a dialog's text field focused, and say whether Enter submitted it this frame. The check
/// must come before re-focusing: egui answers `lost_focus` when asked, so focusing first would
/// hide the Enter that just left the field.
fn submitted(ui: &Ui, response: &egui::Response) -> bool {
    let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    if !submitted {
        response.request_focus();
    }
    submitted
}

fn buttons(ui: &mut Ui, choices: &[(&str, Answer)]) -> Answer {
    let mut answer = Answer::None;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        for (i, (label, choice)) in choices.iter().enumerate() {
            let text = if i == 0 { RichText::new(*label).strong() } else { RichText::new(*label) };
            if ui.button(text).clicked() {
                answer = choice.clone();
            }
        }
    });
    answer
}

fn project_form(ui: &mut Ui, form: &mut ProjectForm) -> Answer {
    ui.heading(if form.editing.is_some() { "Edit Project" } else { "New Project" });
    ui.add_space(6.0);
    let error_for = |field: ProjectField, form: &ProjectForm| {
        form.error.as_ref().filter(|(f, _)| *f == field).map(|(_, message)| message.clone())
    };
    let red = ui.visuals().error_fg_color;
    egui::Grid::new("project-form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
        let label = ui.label("Root folder");
        ui.add(
            egui::TextEdit::singleline(&mut form.root)
                .hint_text("/home/me/code/my-project")
                .desired_width(320.0),
        )
        .labelled_by(label.id);
        ui.end_row();
        if let Some(message) = error_for(ProjectField::Root, form) {
            ui.label("");
            ui.colored_label(red, message);
            ui.end_row();
        }
        let label = ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(320.0)).labelled_by(label.id);
        ui.end_row();
        if let Some(message) = error_for(ProjectField::Name, form) {
            ui.label("");
            ui.colored_label(red, message);
            ui.end_row();
        }
        ui.label("Colour");
        ui.horizontal(|ui| {
            for hex in PALETTE {
                let c = Colour::parse(hex).expect("palette colours parse");
                let color = Color32::from_rgb(c.r, c.g, c.b);
                let selected = form.colour == [c.r, c.g, c.b];
                let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                ui.painter().circle_filled(rect.center(), if selected { 9.0 } else { 7.0 }, color);
                if selected {
                    ui.painter().circle_stroke(
                        rect.center(),
                        9.5,
                        egui::Stroke::new(1.5, ui.visuals().strong_text_color()),
                    );
                }
                if response.on_hover_text(hex).clicked() {
                    form.colour = [c.r, c.g, c.b];
                }
            }
            ui.color_edit_button_srgb(&mut form.colour);
        });
        ui.end_row();
    });
    let enter = ui.input(|i| i.key_pressed(Key::Enter));
    let answer = buttons(
        ui,
        &[
            (if form.editing.is_some() { "Save" } else { "Create Project" }, Answer::SaveProject),
            ("Cancel", Answer::Cancel),
        ],
    );
    if enter && answer == Answer::None { Answer::SaveProject } else { answer }
}

fn language_picker(ui: &mut Ui, filter: &mut String, current: &str, overridden: bool) -> Answer {
    ui.heading("Set Language");
    let response =
        ui.add(egui::TextEdit::singleline(filter).hint_text("Filter languages").desired_width(f32::INFINITY));
    let enter = submitted(ui, &response);
    let needle = filter.trim().to_lowercase();
    let names: Vec<&str> =
        throng_editor::lang::names().into_iter().filter(|n| n.to_lowercase().contains(&needle)).collect();
    let mut answer = Answer::None;
    if enter && let Some(first) = names.first() {
        answer = Answer::Language(Some((*first).to_owned()));
    }
    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
        if overridden && ui.button("Detect from the file name").clicked() {
            answer = Answer::Language(None);
        }
        for name in names {
            if ui.selectable_label(name == current, name).clicked() {
                answer = Answer::Language(Some(name.to_owned()));
            }
        }
    });
    if answer == Answer::None { buttons(ui, &[("Cancel", Answer::Cancel)]) } else { answer }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_to_line_clamps_and_ignores_what_is_not_a_number() {
        assert_eq!(parse_line(" 12 ", 100), Some(12));
        assert_eq!(parse_line("+7", 100), Some(7));
        assert_eq!(parse_line("0", 100), Some(1));
        assert_eq!(parse_line("-5", 100), Some(1));
        assert_eq!(parse_line("999", 100), Some(100));
        assert_eq!(parse_line("", 100), None);
        assert_eq!(parse_line("12a", 100), None);
        assert_eq!(parse_line("1:2", 100), None);
    }
}
