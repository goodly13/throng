//! The project file tree (Principle I): lazy, cached per directory, refreshed by watching only the
//! directories that are expanded — a recursive watch of a large tree exhausts Linux's inotify limit.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use egui::{Color32, Key, Modifiers, Rect, RichText, Sense, Ui};
use throng_core::keymap::Scope;
use throng_core::paths::{PathRules, relative_to};

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
}

#[derive(Clone, Debug, Default)]
struct Listing {
    entries: Vec<Entry>,
    error: Option<String>,
}

/// An inline name edit in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Rename { path: PathBuf, name: String },
    Create { dir: PathBuf, folder: bool, name: String },
}

/// What the user asked the tree to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Open(PathBuf),
    OpenTerminal(PathBuf),
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Create {
        path: PathBuf,
        folder: bool,
    },
    Delete(PathBuf),
    CopyPath(String),
    Reveal(PathBuf),
    Hide(String),
    Unhide(String),
    /// Find in Files, scoped to this folder or file.
    FindIn(PathBuf),
    /// Move `from` into the folder `into`, or copy it there (a drag, or cut or copy then paste).
    Transfer {
        from: PathBuf,
        into: PathBuf,
        copy: bool,
    },
    /// Open a file's preview.
    Preview(PathBuf),
    /// Undo or redo the last file operation made from the tree.
    Undo,
    Redo,
}

/// A tree item being dragged (egui's drag-and-drop payload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeDrag(pub PathBuf);

/// Whether a drag with these modifiers copies rather than moves: Ctrl, or Option on macOS
/// (no modifier, or Shift, moves).
#[must_use]
pub fn copies(modifiers: Modifiers) -> bool {
    if cfg!(target_os = "macos") { modifiers.alt } else { modifiers.ctrl }
}

/// A tree item released over `rect` (a workspace panel), when that panel accepts one. Draws the
/// drop highlight while one hovers there.
pub fn dropped_on(ui: &Ui, rect: Rect, accepts: bool) -> Option<PathBuf> {
    let ctx = ui.ctx();
    if !accepts || !egui::DragAndDrop::has_payload_of_type::<TreeDrag>(ctx) {
        return None;
    }
    let over = ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| rect.contains(p));
    if !over {
        return None;
    }
    let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
    ui.painter().rect_stroke(rect.shrink(1.0), 2.0, stroke, egui::StrokeKind::Inside);
    if ctx.input(|i| i.pointer.any_released()) {
        return egui::DragAndDrop::take_payload::<TreeDrag>(ctx).map(|p| p.0.clone());
    }
    None
}

/// Whether the tree's history has anything to undo or redo, for its menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct History {
    pub can_undo: bool,
    pub can_redo: bool,
}

/// One project's tree.
pub struct Explorer {
    pub root: PathBuf,
    pub expanded: BTreeSet<PathBuf>,
    listings: HashMap<PathBuf, Listing>,
    pub selected: Option<PathBuf>,
    pub edit: Option<Edit>,
    edit_error: Option<String>,
    focus_edit: bool,
    /// The tree has the keyboard: it was clicked last (the `explorer` scope).
    pub focused: bool,
    /// Cut (true) or copied, waiting for Paste.
    pub clipboard: Option<(PathBuf, bool)>,
    /// The folder a drag would drop into, highlighted.
    drop_target: Option<PathBuf>,
    /// This frame's undo state, for the menus.
    history: History,
}

impl Explorer {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let mut expanded = BTreeSet::new();
        expanded.insert(root.clone());
        Self {
            root,
            expanded,
            listings: HashMap::new(),
            selected: None,
            edit: None,
            edit_error: None,
            focus_edit: false,
            focused: false,
            clipboard: None,
            drop_target: None,
            history: History::default(),
        }
    }

    /// An item moved from `from` to `to` (a rename, a move, an undo): what was expanded or
    /// selected under it follows it (a moved folder keeps its expansion).
    pub fn follow_move(&mut self, from: &Path, to: &Path) {
        let rebase = |p: &Path| -> Option<PathBuf> {
            let rest = p.strip_prefix(from).ok()?;
            Some(if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) })
        };
        self.expanded = self.expanded.iter().map(|p| rebase(p).unwrap_or_else(|| p.clone())).collect();
        if let Some(selected) = self.selected.as_deref().and_then(rebase) {
            self.selected = Some(selected);
        }
        if let Some((path, cut)) = self.clipboard.take() {
            self.clipboard = Some((rebase(&path).unwrap_or(path), cut));
        }
        self.invalidate(from.parent().unwrap_or(from));
        self.invalidate(to.parent().unwrap_or(to));
    }

    /// Forget a directory's cached listing (after a change inside it).
    pub fn invalidate(&mut self, dir: &Path) {
        self.listings.remove(dir);
    }

    pub fn invalidate_all(&mut self) {
        self.listings.clear();
    }

    /// The directories currently expanded (to watch).
    #[must_use]
    pub fn watched_dirs(&self) -> Vec<PathBuf> {
        self.expanded.iter().filter(|d| d.is_dir()).cloned().collect()
    }

    fn listing(
        &mut self,
        dir: &Path,
        exclude: &[String],
        hidden: &BTreeSet<String>,
        rules: &PathRules,
    ) -> &Listing {
        if !self.listings.contains_key(dir) {
            let listing = read_listing(&self.root, dir, exclude, hidden, rules);
            self.listings.insert(dir.to_path_buf(), listing);
        }
        &self.listings[dir]
    }

    /// Start creating a file or folder inside `dir`.
    pub fn begin_create(&mut self, dir: PathBuf, folder: bool) {
        self.expanded.insert(dir.clone());
        self.edit = Some(Edit::Create { dir, folder, name: String::new() });
        self.edit_error = None;
        self.focus_edit = true;
    }

    /// Start renaming `path`.
    pub fn begin_rename(&mut self, path: PathBuf) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.edit = Some(Edit::Rename { path, name });
        self.edit_error = None;
        self.focus_edit = true;
    }

    /// Show the tree.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        rules: &PathRules,
        exclude: &[String],
        hidden_paths: &[String],
        accent: Color32,
        history: History,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        self.history = history;
        let area = ui.max_rect();
        // Registered before the rows, so a row wins every click it is under: egui gives a tie to
        // the widget registered last, and this covers them all.
        let background = ui.interact(area, ui.id().with("explorer-bg"), Sense::click());
        if let Some(pressed) =
            ui.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten())
        {
            self.focused = area.contains(pressed);
        }
        self.keys(ui, &mut actions);
        let dragging = egui::DragAndDrop::payload::<TreeDrag>(ui.ctx());
        if dragging.is_none() {
            self.drop_target = None;
        } else {
            let copy = ui.input(|i| copies(i.modifiers));
            ui.ctx().set_cursor_icon(if copy { egui::CursorIcon::Copy } else { egui::CursorIcon::Grabbing });
        }
        let hidden: BTreeSet<String> = hidden_paths.iter().cloned().collect();
        let rows = self.visible_rows(rules, exclude, &hidden);
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;

        if rows.is_empty() {
            if let Some(error) = self.listings.get(&self.root).and_then(|l| l.error.clone()) {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            } else {
                ui.weak("This folder is empty.");
            }
        }

        egui::ScrollArea::vertical().id_salt(("explorer", &self.root)).auto_shrink([false, false]).show_rows(
            ui,
            row_h,
            rows.len() + usize::from(self.creating_at_end(&rows)),
            |ui, range| {
                for index in range {
                    if let Some(row) = rows.get(index) {
                        self.row_ui(ui, row, rules, accent, &mut actions);
                    }
                }
            },
        );
        // Right-clicking empty space offers creation at the root.
        background.context_menu(|ui| {
            if ui.button("New File").clicked() {
                self.begin_create(self.root.clone(), false);
                ui.close();
            }
            if ui.button("New Folder").clicked() {
                self.begin_create(self.root.clone(), true);
                ui.close();
            }
            let root = self.root.clone();
            self.paste_item(ui, &root, &mut actions);
            ui.separator();
            history_items(ui, history, &mut actions);
        });
        // A drag released below the rows lands in the root.
        if dragging.is_some()
            && ui.input(|i| i.pointer.any_released())
            && ui.input(|i| i.pointer.latest_pos()).is_some_and(|p| area.contains(p))
            && let Some(payload) = egui::DragAndDrop::take_payload::<TreeDrag>(ui.ctx())
        {
            let copy = ui.input(|i| copies(i.modifiers));
            actions.push(Action::Transfer { from: payload.0.clone(), into: self.root.clone(), copy });
        }
        actions
    }

    /// The tree's own keys, when it has the keyboard and nothing in it is being typed into.
    fn keys(&mut self, ui: &Ui, actions: &mut Vec<Action>) {
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if !self.focused || self.edit.is_some() || typing {
            return;
        }
        let take = |id| crate::keymap::take(ui.ctx(), id, Some(Scope::Explorer));
        if take("file.redo") {
            actions.push(Action::Redo);
        } else if take("file.undo") {
            actions.push(Action::Undo);
        }
        let Some(selected) = self.selected.clone() else { return };
        if take("file.rename") && selected != self.root {
            self.begin_rename(selected.clone());
            return;
        }
        // The platform turns Ctrl/Cmd+X, C and V into clipboard events rather than keys, which the
        // keymap answers as those chords; Paste only comes when the system clipboard holds text,
        // which Cut and Copy see to by putting the item's path there.
        let cut = take("file.cut");
        let copy = !cut && take("file.copy");
        let paste = !cut && !copy && take("file.paste");
        if cut || copy {
            self.clipboard = Some((selected.clone(), cut));
            actions.push(Action::CopyPath(selected.display().to_string()));
        } else if paste && let Some((from, cut)) = self.clipboard.clone() {
            let into = crate::file_ops::drop_folder(&selected, selected.is_dir());
            actions.push(Action::Transfer { from, into, copy: !cut });
            if cut {
                self.clipboard = None;
            }
        } else if take("file.delete") && selected != self.root {
            actions.push(Action::Delete(selected));
        }
    }

    /// "Paste" into `dir`, when something was cut or copied.
    fn paste_item(&mut self, ui: &mut Ui, dir: &Path, actions: &mut Vec<Action>) {
        let label = match &self.clipboard {
            Some((_, true)) => "Paste (move here)",
            _ => "Paste",
        };
        if ui
            .add_enabled(
                self.clipboard.is_some(),
                egui::Button::new(label).shortcut_text(chord(ui, "file.paste")),
            )
            .clicked()
            && let Some((from, cut)) = self.clipboard.clone()
        {
            actions.push(Action::Transfer { from, into: dir.to_path_buf(), copy: !cut });
            if cut {
                self.clipboard = None;
            }
            ui.close();
        }
    }

    fn creating_at_end(&self, rows: &[Row]) -> bool {
        matches!(&self.edit, Some(Edit::Create { dir, .. }) if !rows.iter().any(|r| &r.entry.path == dir) && dir != &self.root)
    }

    fn visible_rows(&mut self, rules: &PathRules, exclude: &[String], hidden: &BTreeSet<String>) -> Vec<Row> {
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.collect_rows(&root, 0, rules, exclude, hidden, &mut rows);
        rows
    }

    fn collect_rows(
        &mut self,
        dir: &Path,
        depth: usize,
        rules: &PathRules,
        exclude: &[String],
        hidden: &BTreeSet<String>,
        rows: &mut Vec<Row>,
    ) {
        if let Some(Edit::Create { dir: target, folder, .. }) = &self.edit
            && target == dir
        {
            rows.push(Row {
                depth,
                entry: Entry {
                    name: String::new(),
                    path: dir.to_path_buf(),
                    is_dir: *folder,
                    is_symlink: false,
                },
                creating: true,
            });
        }
        let entries = self.listing(dir, exclude, hidden, rules).entries.clone();
        for entry in entries {
            let expand = entry.is_dir && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            rows.push(Row { depth, entry, creating: false });
            if expand {
                self.collect_rows(&path, depth + 1, rules, exclude, hidden, rows);
            }
        }
    }

    fn row_ui(
        &mut self,
        ui: &mut Ui,
        row: &Row,
        rules: &PathRules,
        accent: Color32,
        actions: &mut Vec<Action>,
    ) {
        let indent = 14.0 * row.depth as f32;
        ui.horizontal(|ui| {
            ui.add_space(indent + 4.0);
            let editing_this = match &self.edit {
                Some(Edit::Rename { path, .. }) => !row.creating && path == &row.entry.path,
                Some(Edit::Create { dir, .. }) => row.creating && dir == &row.entry.path,
                None => false,
            };
            if editing_this {
                self.edit_ui(ui, rules, actions);
                return;
            }
            let entry = &row.entry;
            let expanded = self.expanded.contains(&entry.path);
            let (chevron, icon) = match (entry.is_dir, expanded) {
                (true, true) => ("chevronOpen", "folderOpen"),
                (true, false) => ("chevron", "folder"),
                (false, _) => ("", "file"),
            };
            let chevron = if chevron.is_empty() {
                egui::Atom::from("")
            } else {
                crate::icons::atom_with(ui.ctx(), chevron, RichText::weak)
            };
            // Named as before: the icon's glyph and the name, whether the icon draws as an image.
            let spoken = format!("{} {}", crate::icons::glyph(ui.ctx(), icon), entry.name);
            let icon = crate::icons::atom(ui.ctx(), icon);
            ui.add_sized(
                egui::vec2(12.0, ui.spacing().interact_size.y),
                egui::Button::new(chevron).frame(false).sense(Sense::hover()),
            );
            let selected = self.selected.as_ref() == Some(&entry.path);
            let mut text = RichText::new(format!(" {}", entry.name));
            if entry.is_symlink {
                text = text.italics();
            }
            let label = egui::Button::selectable(selected, (icon, text))
                .frame_when_inactive(false)
                .sense(Sense::click_and_drag());
            let response = ui.add(label);
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, &spoken)
            });
            if selected {
                ui.painter().vline(
                    response.rect.left() - 2.0,
                    response.rect.y_range(),
                    egui::Stroke::new(2.0, accent),
                );
            }
            if self.drop_target.as_ref() == Some(&entry.path) {
                let stroke = egui::Stroke::new(1.5, ui.visuals().selection.stroke.color);
                ui.painter().rect_stroke(response.rect.expand(1.0), 2.0, stroke, egui::StrokeKind::Outside);
            }
            response.dnd_set_drag_payload(TreeDrag(entry.path.clone()));
            let into = crate::file_ops::drop_folder(&entry.path, entry.is_dir);
            if response.dnd_hover_payload::<TreeDrag>().is_some() {
                self.drop_target = Some(into.clone());
            }
            if let Some(payload) = response.dnd_release_payload::<TreeDrag>() {
                let copy = ui.input(|i| copies(i.modifiers));
                actions.push(Action::Transfer { from: payload.0.clone(), into: into.clone(), copy });
            }
            if response.clicked() {
                self.selected = Some(entry.path.clone());
                if entry.is_dir {
                    if expanded {
                        self.expanded.remove(&entry.path);
                    } else {
                        self.expanded.insert(entry.path.clone());
                    }
                }
            }
            if response.double_clicked() && !entry.is_dir {
                actions.push(Action::Open(entry.path.clone()));
            }
            response.on_hover_text(entry.path.display().to_string()).context_menu(|ui| {
                self.selected = Some(entry.path.clone());
                let dir = if entry.is_dir {
                    entry.path.clone()
                } else {
                    entry.path.parent().unwrap_or(&self.root).to_path_buf()
                };
                if !entry.is_dir && ui.button("Open").clicked() {
                    actions.push(Action::Open(entry.path.clone()));
                    ui.close();
                }
                if crate::preview::previewable(&entry.path) && ui.button("Open Preview").clicked() {
                    actions.push(Action::Preview(entry.path.clone()));
                    ui.close();
                }
                if ui.button("Open Terminal Here").clicked() {
                    actions.push(Action::OpenTerminal(dir.clone()));
                    ui.close();
                }
                let find = if entry.is_dir { "Find in Folder…" } else { "Find in File…" };
                if ui.button(find).clicked() {
                    actions.push(Action::FindIn(entry.path.clone()));
                    ui.close();
                }
                ui.separator();
                if ui.button("New File").clicked() {
                    self.begin_create(dir.clone(), false);
                    ui.close();
                }
                if ui.button("New Folder").clicked() {
                    self.begin_create(dir, true);
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new("Cut").shortcut_text(chord(ui, "file.cut"))).clicked() {
                    self.clipboard = Some((entry.path.clone(), true));
                    actions.push(Action::CopyPath(entry.path.display().to_string()));
                    ui.close();
                }
                if ui.add(egui::Button::new("Copy").shortcut_text(chord(ui, "file.copy"))).clicked() {
                    self.clipboard = Some((entry.path.clone(), false));
                    actions.push(Action::CopyPath(entry.path.display().to_string()));
                    ui.close();
                }
                self.paste_item(ui, &into, actions);
                ui.separator();
                if ui.add(egui::Button::new("Rename").shortcut_text(chord(ui, "file.rename"))).clicked() {
                    self.begin_rename(entry.path.clone());
                    ui.close();
                }
                if ui
                    .add(egui::Button::new("Move to Trash").shortcut_text(chord(ui, "file.delete")))
                    .clicked()
                {
                    actions.push(Action::Delete(entry.path.clone()));
                    ui.close();
                }
                ui.separator();
                history_items(ui, self.history, actions);
                ui.separator();
                if ui.button("Copy Path").clicked() {
                    actions.push(Action::CopyPath(entry.path.display().to_string()));
                    ui.close();
                }
                if let Some(rel) = relative_to(rules, &self.root, &entry.path)
                    && ui.button("Copy Relative Path").clicked()
                {
                    actions.push(Action::CopyPath(rel));
                    ui.close();
                }
                if ui.button("Reveal in File Manager").clicked() {
                    actions.push(Action::Reveal(entry.path.clone()));
                    ui.close();
                }
                if let Some(rel) = relative_to(rules, &self.root, &entry.path)
                    && ui.button("Hide in This Project").clicked()
                {
                    actions.push(Action::Hide(rel));
                    ui.close();
                }
            });
        });
    }

    fn edit_ui(&mut self, ui: &mut Ui, rules: &PathRules, actions: &mut Vec<Action>) {
        let Some(edit) = self.edit.as_mut() else { return };
        let (name, placeholder) = match edit {
            Edit::Rename { name, .. } => (name, "New name"),
            Edit::Create { name, folder: true, .. } => (name, "Folder name"),
            Edit::Create { name, folder: false, .. } => (name, "File name"),
        };
        let response = ui.add(egui::TextEdit::singleline(name).hint_text(placeholder).desired_width(160.0));
        crate::find_bar::name_input(ui, &response, placeholder);
        if self.focus_edit {
            response.request_focus();
            self.focus_edit = false;
        }
        if let Some(error) = &self.edit_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let cancelled = ui.input(|i| i.key_pressed(Key::Escape)) || (response.lost_focus() && !submitted);
        if cancelled {
            self.edit = None;
            self.edit_error = None;
            return;
        }
        if !submitted {
            return;
        }
        let name = name.trim().to_owned();
        if let Some(reason) = rules.invalid_name_reason(&name) {
            self.edit_error = Some(reason.to_owned());
            self.focus_edit = true;
            return;
        }
        match self.edit.take() {
            Some(Edit::Rename { path, .. }) => {
                let to = path.with_file_name(&name);
                if to != path {
                    actions.push(Action::Rename { from: path, to });
                }
            }
            Some(Edit::Create { dir, folder, .. }) => {
                actions.push(Action::Create { path: dir.join(&name), folder })
            }
            None => {}
        }
        self.edit_error = None;
    }
}

/// A command's chord as a menu shows it.
fn chord(ui: &Ui, id: &str) -> String {
    crate::keymap::label(ui.ctx(), id)
}

/// Undo and Redo, disabled when there is nothing to undo or redo.
fn history_items(ui: &mut Ui, history: History, actions: &mut Vec<Action>) {
    let (undo, redo) = (chord(ui, "file.undo"), chord(ui, "file.redo"));
    if ui.add_enabled(history.can_undo, egui::Button::new("Undo").shortcut_text(undo)).clicked() {
        actions.push(Action::Undo);
        ui.close();
    }
    if ui.add_enabled(history.can_redo, egui::Button::new("Redo").shortcut_text(redo)).clicked() {
        actions.push(Action::Redo);
        ui.close();
    }
}

struct Row {
    depth: usize,
    entry: Entry,
    creating: bool,
}

fn read_listing(
    root: &Path,
    dir: &Path,
    exclude: &[String],
    hidden: &BTreeSet<String>,
    rules: &PathRules,
) -> Listing {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) => {
            return Listing {
                entries: Vec::new(),
                error: Some(throng_core::failure::describe(&e, throng_core::failure::Operation::List, dir)),
            };
        }
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if exclude.iter().any(|x| x == &name) {
                return None;
            }
            let path = e.path();
            if let Some(rel) = relative_to(rules, root, &path)
                && hidden.contains(&rel)
            {
                return None;
            }
            let file_type = e.file_type().ok()?;
            let is_symlink = file_type.is_symlink();
            let is_dir = if is_symlink { path.is_dir() } else { file_type.is_dir() };
            Some(Entry { name, path, is_dir, is_symlink })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name)));
    Listing { entries, error: None }
}

/// Case-insensitive, with runs of digits compared by value ("file2" before "file10").
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let ordering = na
                    .trim_start_matches('0')
                    .len()
                    .cmp(&nb.trim_start_matches('0').len())
                    .then_with(|| na.trim_start_matches('0').cmp(nb.trim_start_matches('0')));
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                let ordering = x.to_lowercase().cmp(y.to_lowercase());
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "alpha", "Beta"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["alpha", "Beta", "file1", "File2", "file10"]);
    }

    #[test]
    fn listings_put_folders_first_and_honour_excludes_and_hidden_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("a.txt"), b"").unwrap();
        let hidden: BTreeSet<String> = ["secret".to_string()].into();
        // A real directory, so the host's rules: under Linux's, `\` is not a separator on Windows.
        let listing = read_listing(root, root, &[".git".into()], &hidden, &throng_platform::path_rules());
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "a.txt"]);
    }

    #[test]
    fn unreadable_directories_report_in_words() {
        let listing = read_listing(
            Path::new("/"),
            Path::new("/definitely/not/here"),
            &[],
            &BTreeSet::new(),
            &PathRules::LINUX,
        );
        assert!(listing.error.unwrap().contains("no longer exists"));
    }
}
