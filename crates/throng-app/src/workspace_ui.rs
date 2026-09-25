//! The Workspace Pane (Principle XI): a strip of tabs, each a dock of panels.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use egui::{Color32, Id, RichText, Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, NodePath, TabViewer};
use throng_core::ids::{PanelId, ProjectId, TabId};
use throng_core::paths::PathRules;
use throng_core::project::Project;
use throng_core::settings::Settings;
use throng_core::terminal::TerminalPanelConfig;
use throng_core::workspace::{
    EditorPanelConfig, Layout, Panel, PanelKind, Placement, PreviewPanelConfig, SearchPanelConfig,
};
use throng_daemon::Client;
use throng_protocol::Request;

use crate::dock::{from_dock, read_dock, to_dock};
use crate::editor::{self, Disk, Documents, EditorStyle};
use crate::find_bar;
use crate::search_panel::{self, SearchAction, SearchState};
use crate::term::hub::{SpawnPlan, TerminalHub};
use crate::term::view::{self, TermStyle};
use crate::term::{Status, TerminalView};

/// A loaded project workspace: the layout plus one dock per tab.
pub struct ProjectWorkspace {
    pub layout: Layout,
    pub docks: HashMap<TabId, DockState<PanelId>>,
    pub dirty_at: Option<Instant>,
}

impl ProjectWorkspace {
    #[must_use]
    pub fn new(layout: Layout) -> Self {
        let mut ws = Self { layout, docks: HashMap::new(), dirty_at: None };
        ws.rebuild_all();
        ws
    }

    /// Rebuild every dock from the layout (after a structural change made in the model).
    pub fn rebuild_all(&mut self) {
        self.docks = self.layout.tabs.iter().map(|t| (t.id, to_dock(&t.root))).collect();
    }

    pub fn mark_dirty(&mut self) {
        self.dirty_at.get_or_insert_with(Instant::now);
    }
}

/// A request raised while drawing, applied once the frame's widgets are done.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelAction {
    Close(PanelId),
    Split(PanelId, Placement, PanelKind),
    SetKind(PanelId, PanelKind),
    Rename(PanelId),
    KillTerminal(PanelId),
    RestartTerminal(PanelId),
    Save(PanelId),
    SaveAs(PanelId),
    Reload(PanelId),
    KeepMine(PanelId),
    RetryOpen(PanelId),
    NewTab,
    CloseTab(TabId),
    RenameTab(TabId),
    Copy(String),
    /// Open Go To Line for this editor.
    GotoLine(PanelId),
    /// Open the language picker for this editor.
    PickLanguage(PanelId),
    /// A Find in Files panel's query changed (it is saved with the layout).
    UpdateSearch(PanelId, SearchPanelConfig),
    /// Something a Find in Files panel asked for.
    Search(PanelId, SearchAction),
    /// A file from the tree was dropped on this empty panel: show it here.
    DropFile(PanelId, PathBuf),
    /// Follow, copy or reveal a link; relative paths are tried against `base`, then the root.
    Link {
        target: throng_core::links::Target,
        action: crate::links::LinkAction,
        base: Option<PathBuf>,
    },
    /// Open a file's preview, or focus the one it has.
    OpenPreview(PathBuf),
    /// Open a file in an editor (a preview's route back to its source).
    OpenInEditor(PathBuf),
    /// Back or forward in a preview's history.
    PreviewBack(PanelId),
    PreviewForward(PanelId),
    /// A heading in the document a preview shows: scrolled to, as a place of its own in history.
    PreviewJump {
        panel: PanelId,
        anchor: String,
    },
    /// A link in a preview.
    PreviewLink {
        panel: PanelId,
        href: String,
        action: crate::links::LinkAction,
    },
    /// Move this project panel into a sub-workspace, out of its project's tabs until it returns.
    MoveTo {
        panel: PanelId,
        sub: Option<ProjectId>,
        tab: Option<TabId>,
    },
    /// A panel dragged out of the dock: moved into a new sub-workspace window.
    TearOff(PanelId),
    /// Move a tab's panels into a new sub-workspace.
    MoveTab(TabId),
    /// Show this project panel in a sub-workspace (`None`: a new one), in a tab of it (`None`: a
    /// new tab).
    SyncTo {
        panel: PanelId,
        sub: Option<ProjectId>,
        tab: Option<TabId>,
    },
}

/// What a mirror shows: a project's panel, resolved before drawing.
#[derive(Clone, Debug)]
pub struct MirrorSource {
    pub project: Project,
    pub panel: PanelId,
    pub kind: PanelKind,
    pub title: String,
    /// The panel was moved here: it is in none of its project's tabs until it returns.
    pub away: bool,
}

/// A sub-workspace as the Sync to Sub-workspace menu offers it.
#[derive(Clone, Debug)]
pub struct SubTarget {
    pub id: ProjectId,
    pub name: String,
    pub tabs: Vec<(TabId, String)>,
}

/// The type picker's state for one untyped panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picker {
    pub shell: Option<String>,
    /// Shell arguments, as typed.
    pub args: String,
    pub cwd: String,
    pub startup: String,
    /// The panel's own "reopen in the last directory" choice; `None` follows the preference.
    pub remember_directory: Option<bool>,
    /// Where its last terminal was working, carried into the next one.
    pub last_directory: Option<PathBuf>,
    /// Whether a command running when it ends becomes its startup command.
    pub remember_command: bool,
    /// Keep throng's administrator rights (when it has them).
    pub run_as_admin: bool,
    pub open_path: String,
    pub error: Option<String>,
}

impl Picker {
    /// Pre-filled from what the panel's last terminal was started with.
    #[must_use]
    pub fn remembering(config: &TerminalPanelConfig) -> Self {
        Self {
            shell: config.shell.clone(),
            args: throng_core::terminal::join_args(&config.args),
            cwd: config.cwd.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            startup: config.startup_command.clone().unwrap_or_default(),
            remember_directory: config.remember_directory,
            last_directory: config.last_directory.clone(),
            remember_command: config.remember_command,
            run_as_admin: config.run_as_admin,
            ..Self::default()
        }
    }
}

/// "Run as administrator": a choice only while throng itself runs as administrator. Otherwise it is
/// shown off and disabled, saying why, while the panel keeps its choice for an elevated run.
fn admin_checkbox(ui: &mut Ui, run_as_admin: &mut bool, elevated: bool) {
    if elevated {
        ui.checkbox(run_as_admin, "Run as administrator").on_hover_text(
            "Keep throng's administrator rights. Otherwise this terminal runs with a normal user's rights.",
        );
    } else {
        let mut off = false;
        ui.add_enabled(false, egui::Checkbox::new(&mut off, "Run as administrator"))
            .on_disabled_hover_text("Start throng as administrator to run a terminal as administrator.");
    }
}

/// The mark on an elevated terminal's tab and on an elevated throng's status bar.
pub const ADMIN_MARK: &str = "ADMIN";

/// [`ADMIN_MARK`] as drawn: red, on a faint red ground.
#[must_use]
pub fn admin_mark() -> RichText {
    let red = Color32::from_rgb(0xe5, 0x53, 0x4b);
    RichText::new(ADMIN_MARK).small().strong().color(red).background_color(red.gamma_multiply(0.18))
}

/// The panel the keyboard was last in (each panel's own strip reports on it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Focus {
    pub panel: Option<PanelId>,
}

/// Everything a panel needs to draw itself.
pub struct PanelCtx<'a> {
    pub project: &'a Project,
    pub hub: &'a mut TerminalHub,
    pub client: Option<&'a Client>,
    pub docs: &'a mut Documents,
    pub rules: &'a PathRules,
    pub settings: &'a Settings,
    pub look: &'a crate::theme::Look,
    pub actions: &'a mut Vec<PanelAction>,
    pub focus: &'a mut Focus,
    pub pickers: &'a mut HashMap<PanelId, Picker>,
    pub open_errors: &'a mut HashMap<PanelId, String>,
    pub searches: &'a mut HashMap<PanelId, SearchState>,
    pub previews: &'a mut HashMap<PanelId, crate::preview::PreviewState>,
    /// What each mirror in this workspace shows (empty outside sub-workspaces).
    pub mirrors: &'a HashMap<PanelId, MirrorSource>,
    /// The sub-workspaces a panel can be synced into (empty inside one).
    pub subs: &'a [SubTarget],
    /// Terminals already drawn this frame by the drawing that sizes them.
    pub drawn: &'a mut HashSet<PanelId>,
    /// The workspace being drawn is a sub-workspace, not a project's.
    pub sub_workspace: bool,
}

/// `<label>` ▸ New Sub-workspace | <sub-workspace> ▸ New Tab | <tab>: Sync to Sub-workspace (shown
/// in both places) and Move to Sub-workspace (shown there only) share it.
fn sub_menu(
    ui: &mut Ui,
    label: &str,
    subs: &[SubTarget],
    actions: &mut Vec<PanelAction>,
    action: impl Fn(Option<ProjectId>, Option<TabId>) -> PanelAction,
) {
    ui.menu_button(label, |ui| {
        if ui.button("New Sub-workspace").clicked() {
            actions.push(action(None, None));
            ui.close();
        }
        if !subs.is_empty() {
            ui.separator();
        }
        for target in subs {
            ui.menu_button(&target.name, |ui| {
                if ui.button("New Tab").clicked() {
                    actions.push(action(Some(target.id), None));
                    ui.close();
                }
                for (tab, title) in &target.tabs {
                    if ui.button(title).clicked() {
                        actions.push(action(Some(target.id), Some(*tab)));
                        ui.close();
                    }
                }
            });
        }
    });
}

/// A mirror: the project panel it shows, drawn here with its own caret and widgets.
fn mirror_ui(ui: &mut Ui, panel: PanelId, tab_title: &str, ctx: &mut PanelCtx<'_>) {
    let Some(source) = ctx.mirrors.get(&panel).cloned() else {
        banner(
            ui,
            ui.visuals().warn_fg_color,
            "The panel this showed is gone from its project.",
            &[("Close Here", PanelAction::Close(panel))],
            ctx.actions,
        );
        return;
    };
    match source.kind {
        PanelKind::Terminal(config) => {
            let label = format!("{} › {} › {}", source.project.name, tab_title, source.title);
            // Spawned (if it must be) as its project's: in its root, under its id.
            terminal_ui(ui, &source.project, source.panel, panel, config, label, ctx);
        }
        PanelKind::Editor(config) => editor_ui(ui, source.panel, panel, &config, ctx),
        PanelKind::Preview(config) => preview_ui(ui, panel, &config, ctx),
        _ => {
            ui.weak("This kind of panel is not shown in sub-workspaces.");
        }
    }
}

struct Viewer<'a, 'b> {
    ctx: &'b mut PanelCtx<'a>,
    /// For tab titles made of parts.
    style: std::sync::Arc<egui::Style>,
    panels: &'b BTreeMap<PanelId, Panel>,
    tab_title: String,
    add_requests: Vec<NodePath>,
}

impl TabViewer for Viewer<'_, '_> {
    type Tab = PanelId;

    fn id(&mut self, tab: &mut PanelId) -> Id {
        Id::new(("panel", *tab))
    }

    fn title(&mut self, tab: &mut PanelId) -> WidgetText {
        let Some(panel) = self.panels.get(tab) else { return "?".into() };
        match &panel.kind {
            PanelKind::Editor(config) => {
                let key = Documents::panel_key(self.ctx.rules, *tab, config.path.as_deref());
                let doc = self.ctx.docs.get(&key);
                let name = if panel.title_is_custom {
                    panel.title.clone()
                } else {
                    doc.map_or_else(|| file_name(config.path.as_ref()), editor::Document::name)
                };
                let dirty = doc.is_some_and(editor::Document::is_dirty);
                if dirty { format!("{name} •").into() } else { name.into() }
            }
            PanelKind::Terminal(_) => {
                let view = self.ctx.hub.views.get(tab);
                let failed = view.is_some_and(|v| matches!(v.status, Status::Exited(_) | Status::Failed(_)));
                let bell = view.is_some_and(|v| v.bell);
                let admin = view.is_some_and(|v| v.elevated && v.status == Status::Running);
                if admin {
                    let mut job = egui::text::LayoutJob::default();
                    let font = egui::FontSelection::Style(egui::TextStyle::Button);
                    let title =
                        if bell { format!("• {} ", panel.title) } else { format!("{} ", panel.title) };
                    RichText::new(title).append_to(&mut job, &self.style, font.clone(), egui::Align::Center);
                    admin_mark().append_to(&mut job, &self.style, font, egui::Align::Center);
                    job.into()
                } else if failed {
                    RichText::new(&panel.title).color(Color32::from_rgb(0xe5, 0x53, 0x4b)).into()
                } else if bell {
                    format!("• {}", panel.title).into()
                } else {
                    panel.title.clone().into()
                }
            }
            PanelKind::Untyped => RichText::new(&panel.title).italics().into(),
            PanelKind::Mirror(_) => match self.ctx.mirrors.get(tab) {
                Some(source) => format!("{} · {}", source.project.name, source.title).into(),
                None => RichText::new("Gone").italics().into(),
            },
            PanelKind::Search(config) => search_panel::title(config).into(),
            PanelKind::Preview(config) => {
                // `<name> - Preview`, dirty exactly while its source document is.
                let key = Documents::key_for(self.ctx.rules, &config.path);
                let doc = self.ctx.docs.get(&key);
                let name = doc.map_or_else(|| file_name(Some(&config.path)), editor::Document::name);
                let dirty = doc.is_some_and(editor::Document::is_dirty);
                if dirty {
                    format!("{name} - Preview •").into()
                } else {
                    format!("{name} - Preview").into()
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut PanelId) {
        let panel = *tab;
        let Some(record) = self.panels.get(&panel) else { return };
        match record.kind.clone() {
            PanelKind::Untyped => {
                let rect = ui.max_rect();
                picker_ui(ui, panel, self.ctx);
                if let Some(path) = crate::explorer::dropped_on(ui, rect, true) {
                    self.ctx.actions.push(PanelAction::DropFile(panel, path));
                }
            }
            PanelKind::Terminal(config) => {
                let label = format!("{} › {} › {}", self.ctx.project.name, self.tab_title, record.title);
                let project = self.ctx.project;
                terminal_ui(ui, project, panel, panel, config, label, self.ctx);
            }
            PanelKind::Editor(config) => editor_ui(ui, panel, panel, &config, self.ctx),
            PanelKind::Search(config) => search_ui(ui, panel, config, self.ctx),
            PanelKind::Preview(config) => preview_ui(ui, panel, &config, self.ctx),
            PanelKind::Mirror(_) => mirror_ui(ui, panel, &self.tab_title, self.ctx),
        }
    }

    fn context_menu(&mut self, ui: &mut Ui, tab: &mut PanelId, _path: NodePath) {
        let panel = *tab;
        let kind = self.panels.get(&panel).map(|p| p.kind.clone()).unwrap_or_default();
        let syncable = matches!(kind, PanelKind::Terminal(_) | PanelKind::Editor(_) | PanelKind::Preview(_));
        let mirror = matches!(kind, PanelKind::Mirror(_));
        // A Find in Files panel is named by its query, and a preview by its file: neither can be
        // renamed.
        if !matches!(kind, PanelKind::Search(_) | PanelKind::Preview(_) | PanelKind::Mirror(_)) {
            if ui.button("Rename…").clicked() {
                self.ctx.actions.push(PanelAction::Rename(panel));
                ui.close();
            }
            ui.separator();
        }
        let new_terminal = PanelKind::Terminal(TerminalPanelConfig::default());
        if ui.button("Split Right (Terminal)").clicked() {
            self.ctx.actions.push(PanelAction::Split(panel, Placement::Right, new_terminal.clone()));
            ui.close();
        }
        if ui.button("Split Down (Terminal)").clicked() {
            self.ctx.actions.push(PanelAction::Split(panel, Placement::Below, new_terminal));
            ui.close();
        }
        if ui.button("Split Right (Empty)").clicked() {
            self.ctx.actions.push(PanelAction::Split(panel, Placement::Right, PanelKind::Untyped));
            ui.close();
        }
        match kind {
            PanelKind::Terminal(_) => {
                ui.separator();
                let restart =
                    if self.ctx.hub.is_dormant(panel) { "Reload Terminal" } else { "Restart Terminal" };
                if ui.button(restart).clicked() {
                    self.ctx.actions.push(PanelAction::RestartTerminal(panel));
                    ui.close();
                }
                if ui.button("Kill Terminal (keep panel)").clicked() {
                    self.ctx.actions.push(PanelAction::KillTerminal(panel));
                    ui.close();
                }
                if let Some(view) = self.ctx.hub.views.get(&panel)
                    && ui.button("Copy All Output").clicked()
                {
                    self.ctx.actions.push(PanelAction::Copy(view.text()));
                    ui.close();
                }
            }
            PanelKind::Editor(config) => {
                ui.separator();
                if ui.button("Save").clicked() {
                    self.ctx.actions.push(PanelAction::Save(panel));
                    ui.close();
                }
                if ui.button("Save As…").clicked() {
                    self.ctx.actions.push(PanelAction::SaveAs(panel));
                    ui.close();
                }
                if let Some(path) = config.path.filter(|p| crate::preview::previewable(p))
                    && ui.button("Open Preview").clicked()
                {
                    self.ctx.actions.push(PanelAction::OpenPreview(path));
                    ui.close();
                }
            }
            PanelKind::Preview(config) => {
                ui.separator();
                let ctx = ui.ctx().clone();
                let back =
                    egui::Button::new("Back").shortcut_text(crate::keymap::label(&ctx, "navigate.back"));
                if ui.add_enabled(config.can_back(), back).clicked() {
                    self.ctx.actions.push(PanelAction::PreviewBack(panel));
                    ui.close();
                }
                let forward = egui::Button::new("Forward")
                    .shortcut_text(crate::keymap::label(&ctx, "navigate.forward"));
                if ui.add_enabled(config.can_forward(), forward).clicked() {
                    self.ctx.actions.push(PanelAction::PreviewForward(panel));
                    ui.close();
                }
                if ui.button("Refresh").clicked() {
                    if let Some(state) = self.ctx.previews.get_mut(&panel) {
                        state.refresh = true;
                    }
                    ui.close();
                }
                if ui.button("Open in Editor").clicked() {
                    self.ctx.actions.push(PanelAction::OpenInEditor(config.path));
                    ui.close();
                }
            }
            PanelKind::Untyped | PanelKind::Search(_) | PanelKind::Mirror(_) => {}
        }
        // A project's terminal, editor or preview can be shown in a sub-workspace too.
        if syncable && !self.ctx.sub_workspace {
            ui.separator();
            sub_menu(ui, "Sync to Sub-workspace", self.ctx.subs, self.ctx.actions, |sub, tab| {
                PanelAction::SyncTo { panel, sub, tab }
            });
            sub_menu(ui, "Move to Sub-workspace", self.ctx.subs, self.ctx.actions, |sub, tab| {
                PanelAction::MoveTo { panel, sub, tab }
            });
        }
        ui.separator();
        // A mirror is closed here and lives on in its project ("Close", not "Destroy"); one moved
        // here goes back to its project.
        let away = self.ctx.mirrors.get(&panel).filter(|m| m.away);
        let close = match away {
            Some(source) => format!("Return to {}", source.project.name),
            None if mirror => "Close Here".to_owned(),
            None => "Close Panel".to_owned(),
        };
        if ui.button(close).clicked() {
            self.ctx.actions.push(PanelAction::Close(panel));
            ui.close();
        }
    }

    fn on_tab_button(&mut self, tab: &mut PanelId, response: &egui::Response) {
        // The dock paints tab titles itself; name the button so screen readers (and tests) can
        // find it.
        let title = self.title(tab).text().to_owned();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &title));
    }

    fn on_close(&mut self, tab: &mut PanelId) -> OnCloseResponse {
        // The app decides (unsaved edits, running terminals); the dock never removes it itself.
        self.ctx.actions.push(PanelAction::Close(*tab));
        OnCloseResponse::Ignore
    }

    fn on_add(&mut self, path: NodePath) {
        self.add_requests.push(path);
    }
}

fn file_name(path: Option<&PathBuf>) -> String {
    path.and_then(|p| p.file_name()).map_or_else(|| "Untitled".into(), |n| n.to_string_lossy().into_owned())
}

/// Draw the tab strip and the active tab's dock.
pub fn show(
    ui: &mut Ui,
    ws: &mut ProjectWorkspace,
    ctx: &mut PanelCtx<'_>,
    renaming: &mut Option<(TabId, String)>,
) {
    tab_strip(ui, ws, ctx, renaming);
    let Some(tab) = ws.layout.active_tab().map(|t| (t.id, t.title.clone())) else {
        ui.centered_and_justified(|ui| {
            if ui.button("New Tab").clicked() {
                ctx.actions.push(PanelAction::NewTab);
            }
        });
        return;
    };
    let (tab_id, tab_title) = tab;
    let ProjectWorkspace { layout, docks, .. } = ws;
    let dock = docks.entry(tab_id).or_insert_with(|| {
        let root = layout
            .tab(tab_id)
            .map(|t| t.root.clone())
            .unwrap_or_else(|| throng_core::workspace::SplitTree::Leaf { panels: vec![], active: 0 });
        to_dock(&root)
    });
    let mut viewer = Viewer {
        ctx,
        style: ui.style().clone(),
        panels: &layout.panels,
        tab_title,
        add_requests: Vec::new(),
    };
    let mut style = egui_dock::Style::from_egui(ui.style().as_ref());
    style.tab_bar.fill_tab_bar = false;
    style.tab.tab_body.inner_margin = egui::Margin::ZERO;
    dock_look(&mut style, ui.visuals(), crate::theme::to_color32(viewer.ctx.project.colour));
    DockArea::new(dock)
        .id(Id::new(("dock", tab_id)))
        .style(style)
        .show_add_buttons(true)
        .show_close_buttons(true)
        .show_leaf_collapse_buttons(false)
        .show_leaf_close_all_buttons(false)
        .draggable_tabs(true)
        .show_inside(ui, &mut viewer);

    let add_requests = std::mem::take(&mut viewer.add_requests);
    for path in add_requests {
        if let Ok(leaf) = dock.leaf(path)
            && let Some(anchor) = leaf.tabs.get(leaf.active.0).or_else(|| leaf.tabs.first())
        {
            viewer.ctx.actions.push(PanelAction::Split(*anchor, Placement::Stack, PanelKind::Untyped));
        }
    }
    let focused = dock.find_active_focused().map(|(_, panel)| *panel);
    // A panel of a project dragged out of the dock is torn off into a sub-workspace window of its
    // own (in a sub-workspace it just folds back). Until the move takes it out, the layout keeps it
    // folded back, so it can never be lost between the two.
    if !viewer.ctx.sub_workspace {
        for panel in read_dock(dock).1 {
            viewer.ctx.actions.push(PanelAction::TearOff(panel));
        }
    }
    let tree = from_dock(dock);
    if let Some(tab) = layout.tab_mut(tab_id) {
        if let Some(tree) = tree
            && tree != tab.root
        {
            tab.root = tree;
            ws.dirty_at.get_or_insert_with(Instant::now);
        }
        if let Some(panel) = focused {
            tab.active_panel = Some(panel);
        }
    }
}

fn tab_strip(
    ui: &mut Ui,
    ws: &mut ProjectWorkspace,
    ctx: &mut PanelCtx<'_>,
    renaming: &mut Option<(TabId, String)>,
) {
    let accent = crate::theme::to_color32(ctx.project.colour);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let active = ws.layout.active_tab().map(|t| t.id);
        let tabs: Vec<(TabId, String)> = ws.layout.tabs.iter().map(|t| (t.id, t.title.clone())).collect();
        for (id, title) in tabs {
            if let Some((renaming_id, text)) = renaming.as_mut()
                && *renaming_id == id
            {
                let response = ui.add(egui::TextEdit::singleline(text).desired_width(120.0));
                // Ask `lost_focus` before focusing: egui answers it when asked, so focusing first
                // hides the Enter, Escape or click that just left the field. Focus is taken once,
                // when the field appears, so a click elsewhere is free to end the rename.
                if response.lost_focus() {
                    if ui.input(|i| !i.key_pressed(egui::Key::Escape)) {
                        ws.layout.rename_tab(id, text);
                        ws.mark_dirty();
                    }
                    *renaming = None;
                } else if !response.has_focus() {
                    response.request_focus();
                }
                continue;
            }
            let selected = Some(id) == active;
            // The open tab reads strong over the project's colour; the rest are muted, with a soft
            // ground only under the pointer.
            let text = if selected {
                RichText::new(&title).color(ui.visuals().strong_text_color())
            } else {
                RichText::new(&title).color(ui.visuals().weak_text_color())
            };
            let response = ui.add(egui::Button::new(text).frame_when_inactive(false));
            if selected {
                let r = response.rect;
                ui.painter().hline(r.x_range().shrink(4.0), r.bottom() - 1.0, egui::Stroke::new(2.0, accent));
            }
            if response.clicked() {
                ws.layout.active_tab = Some(id);
                ws.mark_dirty();
            }
            if response.double_clicked() {
                *renaming = Some((id, title.clone()));
            }
            response.context_menu(|ui| {
                if ui.button("Rename…").clicked() {
                    *renaming = Some((id, title.clone()));
                    ui.close();
                }
                if !ctx.sub_workspace && ui.button("Move Tab to Sub-workspace").clicked() {
                    ctx.actions.push(PanelAction::MoveTab(id));
                    ui.close();
                }
                if ui.button("Close Tab").clicked() {
                    ctx.actions.push(PanelAction::CloseTab(id));
                    ui.close();
                }
            });
        }
        if crate::icons::token_button(ui, "add", "New Tab", true)
            .on_hover_text("New tab with a terminal")
            .clicked()
        {
            ctx.actions.push(PanelAction::NewTab);
        }
    });
    ui.add_space(2.0);
}

/// The panels' tabs: open ones merge with their panel, the rest are muted on the bar, and the focused
/// panel's tab is outlined in the project's colour.
fn dock_look(style: &mut egui_dock::Style, visuals: &egui::Visuals, accent: Color32) {
    let (text, muted, strong) =
        (visuals.text_color(), visuals.weak_text_color(), visuals.strong_text_color());
    let body = visuals.extreme_bg_color;
    let border = visuals.widgets.noninteractive.bg_stroke.color;
    let top = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
    style.tab_bar.bg_fill = visuals.panel_fill;
    style.tab_bar.hline_color = border;
    style.tab_bar.height = 30.0;
    let t = &mut style.tab;
    for s in [&mut t.inactive, &mut t.inactive_with_kb_focus] {
        s.bg_fill = visuals.panel_fill;
        s.text_color = muted;
        s.outline_color = Color32::TRANSPARENT;
        s.corner_radius = top;
    }
    t.hovered.bg_fill = visuals.widgets.hovered.weak_bg_fill;
    t.hovered.text_color = text;
    t.hovered.corner_radius = top;
    for s in [&mut t.active, &mut t.active_with_kb_focus] {
        s.bg_fill = body;
        s.text_color = text;
        s.outline_color = border;
        s.corner_radius = top;
    }
    for s in [&mut t.focused, &mut t.focused_with_kb_focus] {
        s.bg_fill = body;
        s.text_color = strong;
        s.outline_color = accent;
        s.corner_radius = top;
    }
    style.buttons.close_tab_color = muted;
    style.buttons.close_tab_active_color = text;
    style.buttons.add_tab_color = muted;
    style.buttons.add_tab_active_color = text;
}

fn picker_ui(ui: &mut Ui, panel: PanelId, ctx: &mut PanelCtx<'_>) {
    let picker = ctx.pickers.entry(panel).or_default();
    egui::ScrollArea::vertical().id_salt(("picker", panel)).show(ui, |ui| {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.heading("What should this panel show?");
        });
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("Terminal");
            egui::Grid::new(("picker-grid", panel)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Shell");
                let selected_label = picker
                    .shell
                    .as_deref()
                    .and_then(|id| ctx.hub.shells.iter().find(|s| s.id == id))
                    .map_or_else(|| "Default".to_owned(), |s| s.label.clone());
                egui::ComboBox::from_id_salt(("picker-shell", panel)).selected_text(selected_label).show_ui(
                    ui,
                    |ui| {
                        ui.selectable_value(&mut picker.shell, None, "Default");
                        for shell in &ctx.hub.shells {
                            ui.selectable_value(&mut picker.shell, Some(shell.id.clone()), &shell.label)
                                .on_hover_text(shell.program.display().to_string());
                        }
                    },
                );
                ui.end_row();
                ui.label("Shell arguments");
                ui.add(
                    egui::TextEdit::singleline(&mut picker.args).hint_text("optional").desired_width(320.0),
                );
                ui.end_row();
                ui.label("Folder");
                ui.add(
                    egui::TextEdit::singleline(&mut picker.cwd)
                        .hint_text(ctx.project.root.display().to_string())
                        .desired_width(320.0),
                );
                ui.end_row();
                ui.label("Startup command");
                ui.add(
                    egui::TextEdit::singleline(&mut picker.startup)
                        .hint_text("optional")
                        .desired_width(320.0),
                );
                ui.end_row();
                ui.label("");
                let mut remember = picker.remember_directory.unwrap_or(ctx.settings.remember_directory());
                if ui
                    .checkbox(&mut remember, "Reopen in the last directory")
                    .on_hover_text("Start again in the folder this terminal was last working in.")
                    .changed()
                {
                    picker.remember_directory = Some(remember);
                }
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut picker.remember_command, "Remember the running command").on_hover_text(
                    "When this terminal ends with a command running, that command becomes its \
                         startup command.",
                );
                ui.end_row();
                // Only an elevated throng on Windows has rights to keep or drop; elsewhere a
                // terminal has its user's.
                if cfg!(windows) {
                    ui.label("");
                    admin_checkbox(ui, &mut picker.run_as_admin, ctx.hub.elevated);
                    ui.end_row();
                }
            });
            if ui.button("Start Terminal").clicked() {
                let cwd = picker.cwd.trim();
                let cwd = if cwd.is_empty() { None } else { Some(resolve(&ctx.project.root, cwd)) };
                match cwd {
                    Some(dir) if !dir.is_dir() => {
                        picker.error = Some(format!("The folder \"{}\" does not exist.", dir.display()));
                    }
                    _ => {
                        let startup = picker.startup.trim();
                        let config = TerminalPanelConfig {
                            shell: picker.shell.clone(),
                            args: throng_core::terminal::split_args(&picker.args),
                            cwd,
                            startup_command: if startup.is_empty() { None } else { Some(startup.to_owned()) },
                            remember_directory: picker.remember_directory,
                            last_directory: picker.last_directory.clone(),
                            remember_command: picker.remember_command,
                            running_command: None,
                            run_as_admin: picker.run_as_admin,
                        };
                        ctx.actions.push(PanelAction::SetKind(panel, PanelKind::Terminal(config)));
                    }
                }
            }
        });
        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("Editor");
            ui.horizontal(|ui| {
                ui.label("File");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut picker.open_path)
                        .hint_text("path relative to the project, or absolute")
                        .desired_width(300.0),
                );
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Open").clicked() || enter {
                    let path = resolve(&ctx.project.root, picker.open_path.trim());
                    if path.is_file() {
                        ctx.actions.push(PanelAction::SetKind(
                            panel,
                            PanelKind::Editor(EditorPanelConfig { path: Some(path) }),
                        ));
                    } else {
                        picker.error = Some(format!("\"{}\" is not a file.", path.display()));
                    }
                }
            });
            if ui.button("New Untitled File").clicked() {
                ctx.actions
                    .push(PanelAction::SetKind(panel, PanelKind::Editor(EditorPanelConfig { path: None })));
            }
        });
        if let Some(error) = &picker.error {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    });
}

fn resolve(root: &std::path::Path, text: &str) -> PathBuf {
    let path = PathBuf::from(text);
    if path.is_absolute() { path } else { root.join(path) }
}

fn banner(
    ui: &mut Ui,
    color: Color32,
    text: &str,
    buttons: &[(&str, PanelAction)],
    actions: &mut Vec<PanelAction>,
) {
    egui::Frame::new().fill(color.gamma_multiply(0.18)).inner_margin(egui::Margin::symmetric(8, 6)).show(
        ui,
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, text);
                for (label, action) in buttons {
                    if ui.button(*label).clicked() {
                        actions.push(action.clone());
                    }
                }
            });
        },
    );
}

/// A terminal panel. `session` is the panel whose terminal it shows, and `panel` the one drawn: the
/// same, unless `panel` mirrors `session` in a sub-workspace. The session belongs to `owner`.
#[allow(clippy::too_many_lines)]
fn terminal_ui(
    ui: &mut Ui,
    owner: &Project,
    session: PanelId,
    panel: PanelId,
    config: TerminalPanelConfig,
    label: String,
    ctx: &mut PanelCtx<'_>,
) {
    let cwd = config.cwd.clone();
    let shell_id = config.shell.clone();
    let name = label.clone();
    let plan = SpawnPlan { project: owner.id, label, root: owner.root.clone(), config };
    let area = ui.max_rect();
    ctx.hub.prepare(session, plan);
    let widget =
        if session == panel { view::terminal_id(session) } else { Id::new(("throng-mirror", panel)) };
    // The first drawing this frame sizes it; the owner's panel is drawn before any mirror.
    let primary = ctx.drawn.insert(session);
    let Some(view) = ctx.hub.views.get_mut(&session) else { return };
    let warn = ui.visuals().warn_fg_color;
    let error = ui.visuals().error_fg_color;
    match view.status.clone() {
        Status::Connecting => {
            if ctx.client.is_none() {
                banner(ui, warn, "Waiting for the terminal host…", &[], ctx.actions);
            }
        }
        Status::Failed(message) => {
            banner(
                ui,
                error,
                &message,
                &[
                    ("Retry", PanelAction::RestartTerminal(panel)),
                    ("Change Type", PanelAction::SetKind(panel, PanelKind::Untyped)),
                ],
                ctx.actions,
            );
            return;
        }
        Status::Exited(status) => {
            let code = status.code.map_or_else(|| "—".to_owned(), |c| c.to_string());
            banner(
                ui,
                error,
                &format!("The process exited with code {code}."),
                &[
                    ("Restart", PanelAction::RestartTerminal(panel)),
                    ("Close Panel", PanelAction::Close(panel)),
                ],
                ctx.actions,
            );
        }
        Status::Dormant => {
            // Not started, by choice (manual reload): the panel says what it is and how to start it.
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.label(RichText::new(format!("\"{name}\" is not running.")).strong());
                ui.weak("Terminals start when you reload them (Preferences, Start terminals).");
                ui.add_space(6.0);
                if ui.button("Reload").clicked() {
                    ctx.actions.push(PanelAction::RestartTerminal(panel));
                }
            });
            return;
        }
        Status::Running => {}
    }
    find_in_terminal(ui, panel, widget, view);
    let colours = &ctx.look.code;
    let style = TermStyle {
        font_size: ctx.settings.terminal_font_size(),
        palette: ctx.look.palette.clone(),
        copy_on_select: ctx.settings.copy_on_select(),
        search_match: colours.search_match,
        search_current: colours.search_current,
        link: colours.link,
        links: ctx.settings.links_in_terminals(),
        widget,
        primary,
    };
    let (body, strip_rect) = split_strip(ui, ctx.settings.terminal_status_bar());
    let mut out =
        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| view::show(ui, view, &style)).inner;
    if let Some(strip_rect) = strip_rect {
        let shell = throng_platform::shells::choose(
            &ctx.hub.shells,
            shell_id.as_deref().or(ctx.hub.default_shell.as_deref()),
        )
        .map_or_else(|| "Shell".to_owned(), |s| s.label.clone());
        let directory = view.cwd.as_deref().map(|d| crate::status_strip::directory_label(&owner.root, d));
        let size = view.size;
        ui.scope_builder(egui::UiBuilder::new().max_rect(strip_rect), |ui| {
            crate::status_strip::terminal(ui, &shell, directory.as_deref(), size);
        });
    }
    if let Some((target, action)) = out.link.take() {
        // Relative paths: where the shell is working now, else where it started, then the root.
        let base = view.cwd.clone().or(cwd);
        ctx.actions.push(PanelAction::Link { target, action, base });
    }
    // A path dragged from the tree is typed in, never run: no newline follows it.
    if let Some(path) = crate::explorer::dropped_on(ui, area, view.status == Status::Running) {
        out.input.extend(view.paste_bytes(&crate::file_ops::for_terminal(&[path])));
        ui.memory_mut(|m| m.request_focus(widget));
    }
    if out.open_find {
        // Seeded from a one-line selection, like the editor's.
        let seed = view.term().selection_to_string().filter(|s| !s.is_empty() && !s.contains('\n'));
        let find = view.find.get_or_insert_with(Default::default);
        if let Some(seed) = seed {
            find.query.term = seed;
            find.edited_at = Some(std::time::Instant::now());
        }
        find.focus_input = true;
    }
    finish_terminal_frame(view, out, ctx.focus, ctx.actions, ctx.client);
    ctx.hub.connect(session, ctx.client);
}

/// The terminal's find bar: read-only, no replace row.
fn find_in_terminal(ui: &mut Ui, panel: PanelId, widget: Id, view: &mut TerminalView) {
    let Some(mut find) = view.find.take() else { return };
    if let Some(wait) = find.refresh(view.term(), view.version(), view.size, FIND_DEBOUNCE) {
        ui.ctx().request_repaint_after(wait);
    }
    let mut no_replace_focus = false;
    let bar = find_bar::show(
        ui,
        find_bar::Bar {
            panel,
            current: find.current,
            total: find.matches.len(),
            query: &mut find.query,
            replace: None,
            focus: &mut find.focus_input,
            focus_replace: &mut no_replace_focus,
        },
    );
    if bar.changed {
        find.edited_at = Some(std::time::Instant::now());
    }
    if let Some(forward) = bar.step {
        find.edited_at = None;
        let _ = find.refresh(view.term(), view.version(), view.size, std::time::Duration::ZERO);
        find.step(forward);
    }
    if bar.close {
        ui.memory_mut(|m| m.request_focus(widget));
    } else {
        view.find = Some(find);
    }
}

fn finish_terminal_frame(
    view: &mut TerminalView,
    out: view::TermOutput,
    focus: &mut Focus,
    actions: &mut Vec<PanelAction>,
    client: Option<&Client>,
) {
    if out.focused {
        view.bell = false;
        focus.panel = Some(view.panel);
    }
    if let Some(text) = out.copy {
        actions.push(PanelAction::Copy(text));
    }
    let Some(client) = client else { return };
    if view.status == Status::Running {
        if !out.input.is_empty() {
            client.write(view.panel.into(), &out.input);
        }
        if out.resized {
            client.post(Request::Resize {
                terminal: view.panel.into(),
                cols: view.size.0,
                rows: view.size.1,
            });
        }
    }
}

/// An editor panel. `doc_panel` is the panel whose document it shows (for an untitled one) and
/// `panel` the one drawn, with its own caret: the same, unless `panel` mirrors it.
fn editor_ui(
    ui: &mut Ui,
    doc_panel: PanelId,
    panel: PanelId,
    config: &EditorPanelConfig,
    ctx: &mut PanelCtx<'_>,
) {
    let eol = ctx.settings.default_line_ending();
    let max = ctx.settings.max_open_file_bytes();
    let key = match &config.path {
        Some(path) => {
            if let Some(error) = ctx.open_errors.get(&panel) {
                let error = error.clone();
                banner(
                    ui,
                    ui.visuals().error_fg_color,
                    &error,
                    &[("Retry", PanelAction::RetryOpen(panel)), ("Close Panel", PanelAction::Close(panel))],
                    ctx.actions,
                );
                return;
            }
            match ctx.docs.ensure_file(ctx.rules, path, max, eol) {
                Ok(key) => key,
                Err(e) => {
                    ctx.open_errors.insert(panel, e.to_string());
                    return;
                }
            }
        }
        None => ctx.docs.ensure_untitled(doc_panel, eol),
    };
    let Some(doc) = ctx.docs.get_mut(&key) else { return };
    let warn = ui.visuals().warn_fg_color;
    match doc.disk {
        Disk::Changed => banner(
            ui,
            warn,
            "This file changed on disk while you had unsaved edits.",
            &[
                ("Reload (discard mine)", PanelAction::Reload(panel)),
                ("Keep Mine", PanelAction::KeepMine(panel)),
            ],
            ctx.actions,
        ),
        Disk::Deleted => banner(
            ui,
            warn,
            "This file was deleted on disk. Your text is kept; saving recreates the file.",
            &[("Save", PanelAction::Save(panel))],
            ctx.actions,
        ),
        Disk::InSync => {}
    }
    if let Some(error) = doc.error.clone() {
        banner(
            ui,
            ui.visuals().error_fg_color,
            &error,
            &[("Retry Save", PanelAction::Save(panel))],
            ctx.actions,
        );
    }
    let style = EditorStyle {
        font_size: ctx.settings.editor_font_size(),
        word_wrap: ctx.settings.word_wrap(),
        tab_size: ctx.settings.tab_size(),
        colours: ctx.look.code,
        links: ctx.settings.links_in_editors(),
        previewable: config.path.as_deref().is_some_and(crate::preview::previewable),
        from: (doc_panel != panel).then(|| ctx.mirrors.get(&panel).map(|m| m.project.name.clone())).flatten(),
    };
    let (show_strip, show_position, show_counts) = ctx.settings.editor_status_bar();
    let preview_open = config.path.as_deref().is_some_and(|p| {
        let key = Documents::key_for(ctx.rules, p);
        ctx.previews.values().any(|s| Documents::key_for(ctx.rules, &s.path) == key)
    });
    let Some((doc, clip)) = ctx.docs.doc_and_clip(&key) else { return };
    find_in_editor(ui, panel, doc);
    let (body, strip_rect) = split_strip(ui, show_strip);
    let out = ui
        .scope_builder(egui::UiBuilder::new().max_rect(body), |ui| editor::show(ui, panel, doc, &style, clip))
        .inner;
    if let Some(strip_rect) = strip_rect {
        let rope = doc.buf.rope();
        let selected = doc.views.get(&panel).map_or(0, |v| {
            v.selection
                .ranges()
                .iter()
                .map(|r| throng_editor::lines::utf16_between(rope, r.from(), r.to()))
                .sum()
        });
        let counts = show_counts.then(|| doc.counts());
        let wrap = doc.wrap.unwrap_or(style.word_wrap);
        let format = doc.format.describe();
        let strip = crate::status_strip::EditorStrip {
            line: out.caret.0,
            column: out.caret.1,
            selected,
            carets: out.carets,
            counts,
            show_position,
            language: doc.language(),
            format: &format,
            wrap,
            preview: style.previewable.then_some(preview_open),
        };
        let strip_out = ui
            .scope_builder(egui::UiBuilder::new().max_rect(strip_rect), |ui| {
                crate::status_strip::editor(ui, &strip)
            })
            .inner;
        if strip_out.pick_language {
            ctx.actions.push(PanelAction::PickLanguage(panel));
        }
        if strip_out.toggle_wrap {
            doc.wrap = Some(!wrap);
        }
        if strip_out.open_preview
            && let Some(path) = doc.path.clone()
        {
            ctx.actions.push(PanelAction::OpenPreview(path));
        }
    }
    if out.save {
        ctx.actions.push(if doc.path.is_some() {
            PanelAction::Save(panel)
        } else {
            PanelAction::SaveAs(panel)
        });
    }
    if let Some(text) = out.copy {
        ctx.actions.push(PanelAction::Copy(text));
    }
    if let Some(replace) = out.open_find {
        open_find(doc, panel, replace);
    }
    if out.goto_line {
        ctx.actions.push(PanelAction::GotoLine(panel));
    }
    if out.set_language {
        ctx.actions.push(PanelAction::PickLanguage(panel));
    }
    if out.toggle_wrap {
        doc.wrap = Some(!doc.wrap.unwrap_or(style.word_wrap));
    }
    if out.open_preview
        && let Some(path) = doc.path.clone()
    {
        ctx.actions.push(PanelAction::OpenPreview(path));
    }
    if let Some((target, action)) = out.link {
        // Relative paths are tried beside the file first; an untitled one has only the root.
        let base = doc.path.as_deref().and_then(std::path::Path::parent).map(std::path::Path::to_path_buf);
        ctx.actions.push(PanelAction::Link { target, action, base });
    }
    if out.focused {
        ctx.focus.panel = Some(panel);
    }
}

/// Open (or re-focus) the panel's find bar, seeded from a single-line selection.
fn open_find(doc: &mut editor::Document, panel: PanelId, replace: bool) {
    let view = doc.views.entry(panel).or_default();
    let primary = view.selection.primary();
    let seed = (view.selection.len() == 1 && !primary.is_empty())
        .then(|| doc.buf.rope().slice(primary.from()..primary.to()).to_string())
        .filter(|text| !text.contains('\n'));
    let session = view.find.get_or_insert_with(Default::default);
    if let Some(seed) = seed {
        session.query.term = seed;
        session.reseat = true;
        session.anchor = Some(primary.from());
    }
    if replace {
        session.replace_open = true;
    }
    session.focus_input = true;
}

/// How long typing in the find input settles before searching (`search.asYouTypeDebounceMs`).
const FIND_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(120);

/// The editor's find bar: F3 steps, Escape closes, replace edits as one undo step each.
fn find_in_editor(ui: &mut Ui, panel: PanelId, doc: &mut editor::Document) {
    let Some(mut view) = doc.views.remove(&panel) else { return };
    let editor_focused = ui.memory(|m| m.has_focus(editor::editor_id(panel)));
    let here = editor_focused || find_bar::has_focus(ui, panel);
    if let Some(mut session) = view.find.take() {
        let caret = view.selection.primary().head;
        let before = session.current_match();
        if let Some(wait) = session.refresh(&doc.buf, caret, FIND_DEBOUNCE) {
            ui.ctx().request_repaint_after(wait);
        }
        if session.current_match() != before
            && let Some(m) = session.current_match()
        {
            view.reveal = Some(m.0);
        }
        let mut step = None;
        let mut close = false;
        if here {
            let take = |id| crate::keymap::take(ui.ctx(), id, Some(throng_core::keymap::Scope::Editor));
            if take("search.findPrevious") {
                step = Some(false);
            } else if take("search.findNext") {
                step = Some(true);
            }
            if editor_focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                close = true;
            }
        }
        let bar = find_bar::show(
            ui,
            find_bar::Bar {
                panel,
                current: session.current,
                total: session.matches.len(),
                query: &mut session.query,
                replace: Some((&mut session.replacement, &mut session.replace_open)),
                focus: &mut session.focus_input,
                focus_replace: &mut session.focus_replace,
            },
        );
        if bar.changed {
            session.edited_at = Some(std::time::Instant::now());
        }
        step = bar.step.or(step);
        if let Some(forward) = step {
            // Enter or F3 searches at once, without waiting for the debounce.
            session.edited_at = None;
            let _ = session.refresh(&doc.buf, caret, std::time::Duration::ZERO);
            session.step(forward);
            if let Some((from, to)) = session.current_match() {
                view.select(from, to);
            }
        }
        if bar.replace_one
            && let Some((from, to)) = session.current_match()
        {
            let replacement = session.replacement.clone();
            let tx =
                throng_editor::Transaction::replace(doc.buf.rope(), vec![(from, to, replacement.clone())]);
            if let Some(applied) = doc.transact(view.selection.clone(), tx) {
                view.follow(&doc.buf, &applied);
            }
            session.anchor = Some(from + replacement.chars().count());
            session.reseat = true;
            let _ = session.refresh(&doc.buf, caret, std::time::Duration::ZERO);
            if let Some((from, to)) = session.current_match() {
                view.select(from, to);
            }
        }
        if bar.replace_all && !session.matches.is_empty() {
            let tx = throng_editor::find::replace_all(doc.buf.rope(), &session.matches, &session.replacement);
            if let Some(applied) = doc.transact(view.selection.clone(), tx) {
                view.follow(&doc.buf, &applied);
            }
        }
        if close || bar.close {
            // Closing clears the highlights and returns to the text at the current match.
            ui.memory_mut(|m| m.request_focus(editor::editor_id(panel)));
        } else {
            view.find = Some(session);
        }
    }
    doc.views.insert(panel, view);
}

fn search_ui(ui: &mut Ui, panel: PanelId, mut config: SearchPanelConfig, ctx: &mut PanelCtx<'_>) {
    let settle = std::time::Duration::from_millis(ctx.settings.search_settle_ms());
    let state = ctx.searches.entry(panel).or_default();
    let before = config.clone();
    let actions = search_panel::show(ui, panel, &mut config, state, settle);
    if ui.memory(|m| m.has_focus(search_panel::input_id(panel))) {
        ctx.focus.panel = Some(panel);
    }
    if config != before {
        ctx.actions.push(PanelAction::UpdateSearch(panel, config));
    }
    ctx.actions.extend(actions.into_iter().map(|a| PanelAction::Search(panel, a)));
}

/// A preview panel: a small toolbar, one inline notice when the file cannot be shown,
/// and the rendered document. It follows the open document when there is one, else the
/// disk, and keeps its scroll position across updates.
fn preview_ui(ui: &mut Ui, panel: PanelId, config: &PreviewPanelConfig, ctx: &mut PanelCtx<'_>) {
    use std::time::Duration;
    let path = config.path.as_path();
    let state = ctx
        .previews
        .entry(panel)
        .or_insert_with(|| crate::preview::PreviewState::new(path.to_path_buf(), None));
    if state.path != path {
        *state = crate::preview::PreviewState::new(path.to_path_buf(), None);
    }
    let (delay, wait) = ctx.settings.preview_timing();
    let now = Instant::now();
    let key = Documents::key_for(ctx.rules, path);
    let again = match ctx.docs.get(&key) {
        Some(doc) => state.follow_document(
            doc.buf.version(),
            || doc.text(),
            now,
            Duration::from_millis(delay),
            Duration::from_millis(wait),
        ),
        None => state.follow_disk(ctx.settings.max_open_file_bytes(), now),
    };
    if let Some(again) = again {
        ui.ctx().request_repaint_after(again);
    }
    // A preview has the keyboard from a press inside it until a press anywhere else. It holds no
    // egui focus (it has no text to edit), so it keeps that itself.
    let hovered = ui.ui_contains_pointer();
    if ui.input(|i| i.pointer.any_pressed()) {
        state.has_keys = hovered;
    }
    let focused = state.has_keys;
    if focused {
        ctx.focus.panel = Some(panel);
    }
    let (key_back, key_forward) = if focused {
        let ctx = ui.ctx();
        (
            crate::keymap::take(ctx, "navigate.back", Some(throng_core::keymap::Scope::Preview)),
            crate::keymap::take(ctx, "navigate.forward", Some(throng_core::keymap::Scope::Preview)),
        )
    } else {
        (false, false)
    };
    // The mouse's own back and forward buttons, over the preview.
    let (mouse_back, mouse_forward) = ui.input(|i| {
        (
            hovered && i.pointer.button_pressed(egui::PointerButton::Extra1),
            hovered && i.pointer.button_pressed(egui::PointerButton::Extra2),
        )
    });
    ui.horizontal(|ui| {
        let ctx_ = ui.ctx().clone();
        let back = crate::icons::token_button(ui, "back", "Back", config.can_back())
            .on_hover_text(crate::app::hint(&ctx_, "Back", "navigate.back"));
        if (back.clicked() || key_back || mouse_back) && config.can_back() {
            ctx.actions.push(PanelAction::PreviewBack(panel));
        }
        let forward = crate::icons::token_button(ui, "forward", "Forward", config.can_forward())
            .on_hover_text(crate::app::hint(&ctx_, "Forward", "navigate.forward"));
        if (forward.clicked() || key_forward || mouse_forward) && config.can_forward() {
            ctx.actions.push(PanelAction::PreviewForward(panel));
        }
        if ui.small_button("Refresh").on_hover_text("Show the latest now").clicked() {
            state.refresh = true;
            ui.ctx().request_repaint();
        }
        if ui.small_button("Open in Editor").clicked() {
            ctx.actions.push(PanelAction::OpenInEditor(path.to_path_buf()));
        }
        if let Some(document) = &state.document
            && ui.small_button("Copy Text").clicked()
        {
            ctx.actions.push(PanelAction::Copy(document.plain_text()));
        }
    });
    if let Some(problem) = &state.problem {
        ui.colored_label(ui.visuals().warn_fg_color, problem);
    }
    ui.separator();
    let style = crate::preview::PreviewStyle {
        body_size: ctx.settings.editor_font_size().max(13.0),
        code_size: ctx.settings.editor_font_size(),
        theme: std::sync::Arc::clone(&ctx.docs.theme),
        link: ctx.look.code.link,
        images: crate::preview::ImagePolicy {
            root: ctx.rules.is_within(&ctx.project.root, path).then(|| ctx.project.root.clone()),
            rules: *ctx.rules,
            remote: ctx.settings.preview_remote_images(),
        },
    };
    let mut area = egui::ScrollArea::vertical().id_salt(("preview", panel)).auto_shrink([false, false]);
    // A step back or forward puts the reader where they were.
    if let Some(offset) = state.restore.take() {
        area = area.vertical_scroll_offset(offset);
    }
    let output = area.show(ui, |ui| {
        ui.set_max_width(ui.available_width().min(900.0));
        crate::preview::show(ui, state, &style)
    });
    state.scroll = output.state.offset.y;
    // Scroll sync with the file's editor in this window, both ways.
    if ctx.settings.preview_sync_scroll() {
        let key = Documents::key_for(ctx.rules, path);
        let (here, pass) = (ui.ctx().viewport_id(), ui.ctx().cumulative_pass_nr());
        let editor = ctx.docs.get_mut(&key).and_then(|doc| {
            doc.views
                .iter_mut()
                .filter(|(_, v)| v.drawn.is_some_and(|(window, n)| window == here && n + 1 >= pass))
                .min_by_key(|(id, _)| **id)
                .map(|(_, v)| v)
        });
        if let Some(view) = editor {
            if let Some(line) = state.sync(view.top_line()) {
                view.show_line_at_top(line);
            }
            if state.settling {
                ui.ctx().request_repaint();
            }
        }
    }
    for (href, action) in output.inner {
        // A heading in this document is a place of its own; anything else is the app's to follow.
        match href.strip_prefix('#') {
            Some(anchor) if action == crate::links::LinkAction::Follow => {
                ctx.actions.push(PanelAction::PreviewJump { panel, anchor: crate::markdown::slug(anchor) });
                ui.ctx().request_repaint();
            }
            _ => ctx.actions.push(PanelAction::PreviewLink { panel, href, action }),
        }
    }
}

/// Split the rest of the panel into its body and, when shown, a status strip along the bottom.
fn split_strip(ui: &Ui, show: bool) -> (egui::Rect, Option<egui::Rect>) {
    let full = ui.available_rect_before_wrap();
    if !show || full.height() < crate::status_strip::HEIGHT * 3.0 {
        return (full, None);
    }
    let split = full.bottom() - crate::status_strip::HEIGHT;
    let body = egui::Rect::from_min_max(full.min, egui::pos2(full.right(), split));
    let strip = egui::Rect::from_min_max(egui::pos2(full.left(), split), full.max);
    (body, Some(strip))
}
