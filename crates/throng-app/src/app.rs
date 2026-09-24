//! The application: state, the frame loop, and how user actions are applied.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Color32, Context, RichText, Ui, ViewportCommand};
use throng_core::file_history::{FileHistory, FileOp};
use throng_core::ids::{PanelId, ProjectId, TabId};
use throng_core::links::Target;
use throng_core::notice::{Notice, NoticeCenter, Severity};
use throng_core::paths::PathRules;
use throng_core::project::{Project, ProjectBook, ProjectError, ProjectField, ProjectInput};
use throng_core::settings::{ReadOutcome, Settings};
use throng_core::subworkspace::{Place, SubWorkspaces};
use throng_core::terminal::TerminalPanelConfig;
use throng_core::workspace::{
    EditorPanelConfig, Layout, MirrorPanelConfig, PanelKind, Placement, PreviewPanelConfig, SearchPanelConfig,
};
use throng_persistence::{
    ACTIVE_PROJECT_KEY, FILE_HISTORY_KEY_PREFIX, LANGUAGE_KEY_PREFIX, LayoutLoad, SUB_WORKSPACES_KEY, Store,
};
use throng_platform::dirs::AppDirs;
use throng_protocol::{Reply, Request};

use crate::dialogs::{self, Answer, Dialog, ProjectForm};
use crate::editor::{Disk, DocKey, Documents};
use crate::explorer::{self, Explorer};
use crate::file_ops::{self, Changed};
use crate::file_search;
use crate::link::{DaemonLink, LinkEvent, LinkState};
use crate::links::{Follow, LinkAction};
use crate::project_files::{Exclusions, FileIndex};
use crate::quick_open::{Choice, QuickOpen};
use crate::recovery::Recovery;
use crate::search_panel::{self, Commit, SearchAction, SearchState};
use crate::term::hub::{HubEvent, TerminalHub};
use crate::watch::DirWatcher;
use crate::workspace_ui::{
    self, Focus, MirrorSource, PanelAction, PanelCtx, Picker, ProjectWorkspace, SubTarget,
};

/// Save a changed layout this long after the last change.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Startup inputs.
pub struct Services {
    pub dirs: AppDirs,
    /// The executable that runs `throng daemon`.
    pub exe: PathBuf,
    /// `throng <folder>`: open (or create) the project for this folder.
    pub open: Option<PathBuf>,
    /// Take a screenshot to this path after a delay, then quit (verification and CI smoke tests).
    pub screenshot: Option<(PathBuf, Duration)>,
    /// Ask the user for a folder, starting in the given one.
    pub pick_folder: FolderPicker,
}

/// Asks the user for a folder, starting in the given one; `None` when they cancel.
pub type FolderPicker = Box<dyn FnMut(Option<&Path>) -> Option<PathBuf>>;

/// The platform's own folder picker.
#[must_use]
pub fn native_folder_picker() -> FolderPicker {
    Box::new(|start| {
        let dialog = rfd::FileDialog::new().set_title("Choose the project's root folder");
        let dialog = match start {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        };
        dialog.pick_folder()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closing {
    No,
    /// The user answered every question; let the window close.
    Confirmed,
}

/// The application.
pub struct ThrongApp {
    dirs: AppDirs,
    rules: PathRules,
    settings: Settings,
    store: Store,
    book: ProjectBook,
    workspaces: HashMap<ProjectId, ProjectWorkspace>,
    explorers: HashMap<ProjectId, Explorer>,
    hub: TerminalHub,
    link: DaemonLink,
    docs: Documents,
    watcher: DirWatcher,
    notices: NoticeCenter,
    dialog: Option<Dialog>,
    closing: Closing,
    pickers: HashMap<PanelId, Picker>,
    open_errors: HashMap<PanelId, String>,
    focus: Focus,
    renaming_tab: Option<(TabId, String)>,
    show_explorer: bool,
    prefs: crate::prefs::Prefs,
    screenshot: Option<(PathBuf, Instant, bool)>,
    pick_folder: FolderPicker,
    started: Instant,
    /// The themes on offer, built in and the user's own.
    themes: crate::themes::ThemeStore,
    /// The active theme's colours, and the theme and system mode they were made from.
    look: crate::theme::Look,
    look_source: Option<(throng_core::theme::Theme, f32)>,
    /// The key bindings: the defaults with `keybindings.json` over them.
    keymap: std::sync::Arc<throng_core::keymap::Keymap>,
    /// The icon packs in `<config>/icon-packs`, the folders that could not be read as one, and the
    /// pack last put in force (`None` when it must be worked out again).
    icon_packs: Vec<throng_core::icons::IconPack>,
    /// The sub-workspaces, each a window of its own.
    subs: SubWorkspaces,
    /// Whether the sub-workspace list changed since it was stored (window moves, mostly).
    subs_dirty: bool,
    /// Which of throng's windows had the keyboard this frame, as each reported it.
    window_focus: Vec<(egui::ViewportId, bool)>,
    focus_group: crate::focus_group::FocusGroup,
    /// Terminals drawn this frame by the drawing that sizes them.
    drawn: HashSet<PanelId>,
    icon_pack_problems: Vec<(String, String)>,
    icons_applied: Option<String>,
    /// Find in Files panels' results and scans (not persisted).
    searches: HashMap<PanelId, SearchState>,
    /// Each project's file list, for Quick Open.
    indexes: HashMap<ProjectId, FileIndex>,
    /// A widget to focus once no dialog is open, and how many dialog-free frames to wait first.
    /// egui keeps the previous frame's modal layer for one more frame and refuses focus beneath
    /// it, so a dialog's answer cannot hand focus back until the frame after the modal is gone.
    focus_next: Option<(egui::Id, u8)>,
    /// Keeps unsaved edits on disk so a crash or a quit loses none of them.
    /// `None` when its folder could not be made; that was reported at launch.
    recovery: Option<Recovery>,
    /// Whether recovery files carry undo history, as last applied.
    recovery_history: bool,
    /// Auto-save's view of each document: the version it last saw change, when, and whether a save
    /// of that version was already tried.
    autosave: HashMap<DocKey, (u64, Instant, bool)>,
    /// Each project's undoable file operations, loaded when first needed.
    file_histories: HashMap<ProjectId, FileHistory>,
    /// What each preview panel shows; not persisted beyond its file's path.
    previews: HashMap<PanelId, crate::preview::PreviewState>,
}

/// Whether following a link to `path` would run a program: an executable file.
fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        matches!(ext.as_str(), "exe" | "bat" | "cmd" | "com" | "ps1" | "msi" | "vbs" | "js" | "lnk" | "scr")
    }
}

/// `action` aimed at `panel` instead (a mirror's request, carried to the panel it shows).
fn retarget(action: PanelAction, panel: PanelId) -> PanelAction {
    match action {
        PanelAction::KillTerminal(_) => PanelAction::KillTerminal(panel),
        PanelAction::RestartTerminal(_) => PanelAction::RestartTerminal(panel),
        PanelAction::Save(_) => PanelAction::Save(panel),
        PanelAction::SaveAs(_) => PanelAction::SaveAs(panel),
        PanelAction::Reload(_) => PanelAction::Reload(panel),
        PanelAction::KeepMine(_) => PanelAction::KeepMine(panel),
        PanelAction::RetryOpen(_) => PanelAction::RetryOpen(panel),
        PanelAction::GotoLine(_) => PanelAction::GotoLine(panel),
        PanelAction::PickLanguage(_) => PanelAction::PickLanguage(panel),
        PanelAction::PreviewLink { href, action, .. } => PanelAction::PreviewLink { panel, href, action },
        PanelAction::PreviewBack(_) => PanelAction::PreviewBack(panel),
        PanelAction::PreviewForward(_) => PanelAction::PreviewForward(panel),
        PanelAction::PreviewJump { anchor, .. } => PanelAction::PreviewJump { panel, anchor },
        other => other,
    }
}

/// A tooltip naming what a control does and, when it has one, its chord.
pub(crate) fn hint(ctx: &Context, what: &str, id: &str) -> String {
    match crate::keymap::label(ctx, id) {
        chord if chord.is_empty() => what.to_owned(),
        chord => format!("{what} ({chord})"),
    }
}

/// A scroll offset as history keeps it: whole points, never negative.
fn scroll_points(offset: f32) -> u32 {
    // Offsets are small and non-negative; the cast only drops the fraction.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let points = offset.max(0.0).round() as u32;
    points
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn read_settings(path: &Path) -> (Settings, ReadOutcome) {
    match std::fs::read_to_string(path) {
        Ok(text) => Settings::read(Some(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::read(None),
        Err(e) => (Settings::default(), ReadOutcome::Malformed { reason: e.to_string() }),
    }
}

impl ThrongApp {
    /// Build the app. Any error here is a startup failure the caller reports and exits on.
    pub fn new(ctx: &Context, services: Services) -> anyhow::Result<Self> {
        let Services { dirs, exe, open, screenshot, pick_folder } = services;
        dirs.ensure()?;
        // Images for previews and icon packs: files, https (a preview decides what it asks for),
        // decoded formats and SVG.
        egui_extras::install_image_loaders(ctx);
        let rules = throng_platform::path_rules();
        let mut notices = NoticeCenter::default();

        let (settings, outcome) = read_settings(&dirs.settings_file());
        match &outcome {
            ReadOutcome::Malformed { reason } => {
                notices.raise(settings_malformed(reason));
            }
            _ if outcome.needs_write() => {
                if let Err(e) =
                    throng_platform::fs::atomic_write(&dirs.settings_file(), settings.to_json().as_bytes())
                {
                    tracing::warn!(error = %e, "could not write settings");
                }
            }
            _ => {}
        }

        let store = Store::open(&dirs.database())?;
        let (projects, bad) = store.projects()?;
        if !bad.is_empty() {
            notices.raise(
                Notice::new(
                    "store:bad-projects",
                    Severity::Warning,
                    format!(
                        "{} saved project(s) could not be read. They were left untouched in the database.",
                        bad.len()
                    ),
                )
                .with_detail(
                    bad.iter().map(|b| format!("{}: {}", b.id, b.reason)).collect::<Vec<_>>().join("\n"),
                ),
            );
        }
        let active = store.state(ACTIVE_PROJECT_KEY)?.and_then(|s| s.parse().ok());
        let book = ProjectBook::new(projects, active);

        let shells = throng_platform::shells::detect();
        tracing::info!(shells = shells.len(), "detected shells");
        let hub = TerminalHub::new(
            shells,
            settings.default_shell().map(str::to_owned),
            settings.scrollback_lines(),
        );
        let repaint = ctx.clone();
        let link = DaemonLink::start(dirs.clone(), exe, move || repaint.request_repaint());
        let repaint = ctx.clone();
        let watcher = DirWatcher::new(move || repaint.request_repaint());
        ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let themes_dir = dirs.config.join("themes");
        let mut app = Self {
            dirs,
            rules,
            settings,
            store,
            book,
            workspaces: HashMap::new(),
            explorers: HashMap::new(),
            hub,
            link,
            docs: Documents::default(),
            watcher,
            notices,
            dialog: None,
            closing: Closing::No,
            pickers: HashMap::new(),
            open_errors: HashMap::new(),
            focus: Focus::default(),
            renaming_tab: None,
            show_explorer: true,
            prefs: crate::prefs::Prefs::default(),
            screenshot: screenshot.map(|(path, delay)| (path, Instant::now() + delay, false)),
            pick_folder,
            started: Instant::now(),
            themes: crate::themes::ThemeStore::load(themes_dir),
            look: crate::theme::Look::default(),
            look_source: None,
            keymap: std::sync::Arc::new(throng_core::keymap::Keymap::defaults(crate::keymap::mac())),
            icon_packs: Vec::new(),
            subs: SubWorkspaces::default(),
            subs_dirty: false,
            window_focus: Vec::new(),
            focus_group: crate::focus_group::FocusGroup::default(),
            drawn: HashSet::new(),
            icon_pack_problems: Vec::new(),
            icons_applied: None,
            searches: HashMap::new(),
            indexes: HashMap::new(),
            focus_next: None,
            recovery: None,
            recovery_history: true,
            autosave: HashMap::new(),
            file_histories: HashMap::new(),
            previews: HashMap::new(),
        };
        match app.store.states_with_prefix(LANGUAGE_KEY_PREFIX) {
            Ok(entries) => {
                for (path, language) in entries {
                    let key = Documents::key_for(&app.rules, Path::new(&path));
                    app.docs.languages.insert(key, language);
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not read saved languages"),
        }
        app.start_recovery();
        app.report_unusable_themes();
        app.reload_keymap(ctx);
        app.rescan_icon_packs();
        app.load_subs();
        if let Some(folder) = open {
            app.open_folder(&folder);
        }
        if let Some(id) = app.book.active_id() {
            app.load_workspace(id);
        }
        app.apply_theme(ctx);
        Ok(app)
    }

    /// Load last session's unsaved edits, to be put back as their editors open, and delete the
    /// ones no saved layout still shows.
    fn start_recovery(&mut self) {
        self.recovery_history = self.settings.persist_undo_history();
        let mut recovery = match Recovery::new(self.dirs.data.join("recovery")) {
            Ok(recovery) => recovery,
            Err(e) => {
                let dir = self.dirs.data.join("recovery");
                self.notices.raise(Notice::new(
                    "recovery:dir",
                    Severity::Warning,
                    format!(
                        "{} Unsaved edits will not survive a crash until it is fixed.",
                        throng_core::failure::describe(&e, throng_core::failure::Operation::Write, &dir)
                    ),
                ));
                return;
            }
        };
        let shown = self.documents_in_saved_layouts();
        let records = recovery.load_all();
        recovery.reconcile(&records, |r| r.key(&self.rules).is_some_and(|k| shown.contains(&k)));
        for mut record in records {
            if let Some(key) = record.key(&self.rules).filter(|k| shown.contains(k)) {
                if !self.recovery_history {
                    record.history = None;
                }
                recovery.adopt(key.clone());
                self.docs.recovered.insert(key, record);
            }
        }
        self.recovery = Some(recovery);
    }

    /// Every document an editor panel shows in any project's saved layout.
    fn documents_in_saved_layouts(&self) -> HashSet<DocKey> {
        let mut shown = HashSet::new();
        let ids = self.book.projects().iter().map(|p| p.id).chain(self.subs.list.iter().map(|s| s.id));
        for id in ids {
            let Ok(LayoutLoad::Loaded(layout)) = self.store.layout(id) else { continue };
            for panel in layout.panels.values() {
                if let PanelKind::Editor(config) = &panel.kind {
                    shown.insert(Documents::panel_key(&self.rules, panel.id, config.path.as_deref()));
                }
            }
        }
        shown
    }

    /// Bring recovery files up to date; `flush` writes everything now and waits (quitting).
    fn tick_recovery(&mut self, ctx: Option<&Context>, flush: bool) {
        let persist = self.settings.persist_undo_history();
        let Some(recovery) = self.recovery.as_mut() else { return };
        if persist != self.recovery_history {
            self.recovery_history = persist;
            if !persist {
                // Turning it off purges what was already kept.
                recovery.drop_histories();
                for record in self.docs.recovered.values_mut() {
                    record.history = None;
                }
            }
        }
        if let Some(wait) = recovery.tick(&self.docs, persist, flush)
            && let Some(ctx) = ctx
        {
            ctx.request_repaint_after(wait);
        }
    }

    /// Save documents whose edits have settled, when auto-save is on. Only a
    /// document with a path whose disk copy is still the one it loaded is written: an untitled one
    /// waits for Save As, and one changed or deleted elsewhere waits for the user to choose.
    fn auto_save(&mut self, ctx: &Context) {
        if !self.settings.auto_save() {
            self.autosave.clear();
            return;
        }
        let debounce = Duration::from_millis(self.settings.auto_save_debounce_ms());
        let now = Instant::now();
        let mut live = HashSet::new();
        let mut next: Option<Duration> = None;
        for (key, doc) in self.docs.iter_mut() {
            if doc.path.is_none() || doc.disk != Disk::InSync || !doc.is_dirty() {
                continue;
            }
            live.insert(key.clone());
            let version = doc.buf.version();
            let entry = self.autosave.entry(key.clone()).or_insert((version, now, false));
            if entry.0 != version {
                *entry = (version, now, false);
            }
            if entry.2 {
                continue;
            }
            let quiet = now.duration_since(entry.1);
            if quiet < debounce {
                let wait = debounce - quiet;
                next = Some(next.map_or(wait, |n| n.min(wait)));
                continue;
            }
            entry.2 = true;
            match doc.save_to(None) {
                Ok(()) => doc.error = None,
                Err(e) => doc.error = Some(e),
            }
        }
        self.autosave.retain(|k, _| live.contains(k));
        if let Some(wait) = next {
            ctx.request_repaint_after(wait);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Read-only views, for tests and diagnostics

    #[must_use]
    pub fn projects(&self) -> &[Project] {
        self.book.projects()
    }

    #[must_use]
    pub fn active_layout(&self) -> Option<&Layout> {
        self.workspaces.get(&self.book.active_id()?).map(|ws| &ws.layout)
    }

    /// How far down a preview panel is scrolled, and the source line at its top.
    #[must_use]
    pub fn preview_position(&self, panel: PanelId) -> Option<(f32, Option<usize>)> {
        let state = self.previews.get(&panel)?;
        let lines = state.document.as_ref().map(|d| d.lines.as_slice()).unwrap_or_default();
        Some((state.scroll, crate::preview::line_for_offset(lines, &state.block_tops, state.scroll)))
    }

    /// The first line in view in an editor panel.
    #[must_use]
    pub fn editor_top_line(&mut self, panel: PanelId) -> Option<usize> {
        self.docs
            .iter_mut()
            .find_map(|(_, doc)| doc.views.get_mut(&panel))
            .and_then(crate::code::CodeView::top_line)
    }

    /// The text a terminal panel shows (scrollback and screen).
    #[must_use]
    pub fn terminal_text(&self, panel: PanelId) -> Option<String> {
        self.hub.views.get(&panel).map(crate::term::TerminalView::text)
    }

    /// Columns × rows of a terminal panel's grid.
    #[must_use]
    pub fn terminal_size(&self, panel: PanelId) -> Option<(u16, u16)> {
        self.hub.views.get(&panel).map(|v| v.size)
    }

    #[must_use]
    pub fn terminal_directory(&self, panel: PanelId) -> Option<PathBuf> {
        self.hub.views.get(&panel).and_then(|v| v.cwd.clone())
    }

    #[must_use]
    pub fn terminal_status(&self, panel: PanelId) -> Option<crate::term::Status> {
        self.hub.views.get(&panel).map(|v| v.status.clone())
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.link.client().is_some()
    }

    /// The theme the app is drawn in.
    #[must_use]
    pub fn theme_name(&self) -> &str {
        &self.look.name
    }

    #[must_use]
    pub fn notices(&self) -> &[Notice] {
        self.notices.all()
    }

    /// An editor panel's text.
    /// The sub-workspaces, by name, and whether each one's window is open.
    #[must_use]
    pub fn sub_workspaces(&self) -> Vec<(String, bool)> {
        self.subs.list.iter().map(|s| (s.name.clone(), s.open)).collect()
    }

    #[must_use]
    pub fn editor_text(&self, panel: PanelId) -> Option<String> {
        let key = self.editor_key(self.book.active_id()?, panel)?;
        self.docs.get(&key).map(crate::editor::Document::text)
    }

    /// An editor panel's caret: 1-based line and column (UTF-16 units).
    #[must_use]
    pub fn editor_caret(&self, panel: PanelId) -> Option<(usize, usize)> {
        let key = self.editor_key(self.book.active_id()?, panel)?;
        let doc = self.docs.get(&key)?;
        let head = doc.views.get(&panel)?.selection.primary().head;
        let rope = doc.buf.rope();
        Some((throng_editor::lines::line_of(rope, head) + 1, throng_editor::lines::utf16_column(rope, head)))
    }

    /// An editor panel's find state: the current match's index and the match count.
    #[must_use]
    pub fn editor_find(&self, panel: PanelId) -> Option<(Option<usize>, usize)> {
        let key = self.editor_key(self.book.active_id()?, panel)?;
        let find = self.docs.get(&key)?.views.get(&panel)?.find.as_ref()?;
        Some((find.current, find.matches.len()))
    }

    /// A terminal panel's find state: the current match's index and the match count.
    #[must_use]
    pub fn terminal_find(&self, panel: PanelId) -> Option<(Option<usize>, usize)> {
        let find = self.hub.views.get(&panel)?.find.as_ref()?;
        Some((find.current, find.matches.len()))
    }

    /// The active tab's Find in Files panel: files with results, matches, and whether it is done.
    #[must_use]
    pub fn search_results(&self) -> Option<(usize, usize, bool)> {
        let ws = self.workspaces.get(&self.book.active_id()?)?;
        let tab = ws.layout.active_tab()?;
        let panel = tab
            .root
            .panels()
            .into_iter()
            .find(|p| matches!(ws.layout.panels.get(p).map(|x| &x.kind), Some(PanelKind::Search(_))))?;
        let state = self.searches.get(&panel)?;
        let done = matches!(state.status, search_panel::Status::Done { .. });
        Some((state.results.len(), state.match_count(), done))
    }

    /// Begin closing as the window's close button would.
    pub fn request_close(&mut self) {
        if self.dialog.is_none() && self.closing != Closing::Confirmed {
            self.begin_close();
        }
    }

    #[must_use]
    pub fn close_confirmed(&self) -> bool {
        self.closing == Closing::Confirmed
    }

    /// Draw in the theme the settings name (and, for `system`, the platform's mode) at the
    /// interface scale; the work is done only when one of those changed.
    fn apply_theme(&mut self, ctx: &Context) {
        let system_dark = ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark);
        let theme = self.themes.resolve(self.settings.theme(), system_dark);
        let scale = self.settings.ui_scale();
        if self.look_source.as_ref().is_some_and(|(t, s)| t == theme && (*s - scale).abs() < f32::EPSILON) {
            return;
        }
        self.look = crate::theme::Look::new(theme);
        self.look_source = Some((theme.clone(), scale));
        crate::theme::apply(ctx, &self.look, scale);
        self.docs.set_theme(self.look.syntax_theme());
    }

    fn active_project(&self) -> Option<&Project> {
        self.book.active()
    }

    fn default_terminal() -> PanelKind {
        PanelKind::Terminal(TerminalPanelConfig::default())
    }

    // ---------------------------------------------------------------------------------------------
    // Projects

    /// `throng <folder>`: activate the project owning `folder`, or create one for it.
    fn open_folder(&mut self, folder: &Path) {
        let folder = throng_platform::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        if let Some(existing) = self.book.owner_of(&self.rules, &folder).map(|p| p.id) {
            self.switch_project(existing);
            return;
        }
        let mut form = ProjectForm::new(self.book.projects().len(), folder.display().to_string());
        if self.save_project(&mut form).is_err() {
            // The New Project dialog, filled in, with the problem beside its field.
            self.dialog = Some(Dialog::Project(form));
        }
    }

    fn save_project(&mut self, form: &mut ProjectForm) -> Result<(), ProjectError> {
        let input: ProjectInput = form.input();
        let root = PathBuf::from(input.root.to_string_lossy().trim());
        if !root.as_os_str().is_empty() && root.is_absolute() && !root.is_dir() {
            form.error = Some((ProjectField::Root, "That folder does not exist.".into()));
            return Err(ProjectError::EmptyRoot);
        }
        let result = match form.editing {
            None => self.book.create(&self.rules, &input, now_ms()).map(|p| p.id),
            Some(id) => self.book.update(&self.rules, id, &input, now_ms()).map(|p| p.id),
        };
        let id = match result {
            Ok(id) => id,
            Err(e) => {
                form.fail(&e);
                return Err(e);
            }
        };
        self.persist_project(id);
        if form.editing.is_some() {
            if let Some(project) = self.book.get(id) {
                let root = project.root.clone();
                if self.explorers.get(&id).is_some_and(|e| e.root != root) {
                    self.explorers.insert(id, Explorer::new(root));
                }
            }
        } else {
            self.switch_project(id);
        }
        Ok(())
    }

    fn persist_project(&mut self, id: ProjectId) {
        let position = self.book.projects().iter().position(|p| p.id == id).unwrap_or(0);
        if let Some(project) = self.book.get(id)
            && let Err(e) = self.store.upsert_project(project, position)
        {
            self.store_failed("save the project", &e);
        }
    }

    fn store_failed(&mut self, what: &str, error: &dyn std::fmt::Display) {
        tracing::error!(%error, what, "store write failed");
        self.notices.raise(Notice::new("store:write", Severity::Error, format!("Could not {what}: {error}")));
    }

    fn switch_project(&mut self, id: ProjectId) {
        self.flush_layouts(true);
        if self.book.set_active(id).is_err() {
            return;
        }
        if let Err(e) = self.store.set_state(ACTIVE_PROJECT_KEY, Some(&id.to_string())) {
            self.store_failed("remember the active project", &e);
        }
        self.load_workspace(id);
        self.focus = Focus::default();
    }

    // ---------------------------------------------------------------------------------------------
    // Sub-workspaces

    fn load_subs(&mut self) {
        match self.store.state(SUB_WORKSPACES_KEY) {
            Ok(Some(text)) => match SubWorkspaces::parse(&text) {
                Ok(subs) => self.subs = subs,
                Err(e) => {
                    self.notices.raise(Notice::new(
                        "sub-workspaces:unreadable",
                        Severity::Warning,
                        format!("The list of sub-workspaces could not be read ({e}); it starts empty."),
                    ));
                }
            },
            Ok(None) => {}
            Err(e) => self.store_failed("read the sub-workspaces", &e),
        }
        let ids: Vec<ProjectId> = self.subs.list.iter().map(|s| s.id).collect();
        for id in ids {
            self.load_workspace(id);
        }
    }

    fn save_subs(&mut self) {
        self.subs_dirty = false;
        if let Err(e) = self.store.set_state(SUB_WORKSPACES_KEY, Some(&self.subs.to_json())) {
            self.store_failed("save the sub-workspaces", &e);
        }
    }

    /// A sub-workspace as its own panels see their owner: named for it, working in the home folder.
    fn sub_project(&self, id: ProjectId) -> Option<Project> {
        let sub = self.subs.get(id)?;
        Some(Project {
            id,
            name: sub.name.clone(),
            colour: throng_core::project::Colour { r: 0x8b, g: 0x94, b: 0x9e },
            root: throng_platform::dirs::home().unwrap_or_else(|| PathBuf::from("/")),
            hidden_paths: Vec::new(),
            created_at: 0,
            updated_at: 0,
        })
    }

    /// The project, or the sub-workspace, a workspace belongs to.
    fn owner(&self, id: ProjectId) -> Option<Project> {
        self.book.get(id).cloned().or_else(|| self.sub_project(id))
    }

    /// The workspace (project or sub-workspace) holding `panel`.
    fn workspace_of(&self, panel: PanelId) -> Option<ProjectId> {
        self.workspaces.iter().find(|(_, ws)| ws.layout.panels.contains_key(&panel)).map(|(id, _)| *id)
    }

    /// A project's file is never opened into a sub-workspace's own editor: true, and
    /// said once, when `path` would be.
    fn refuse_project_file(&mut self, pid: ProjectId, path: &Path) -> bool {
        if !self.subs.contains(pid) {
            return false;
        }
        let Some(project) = self.book.owner_of(&self.rules, path) else { return false };
        let message = format!(
            "{} belongs to \"{}\". Open it there, and use Sync to Sub-workspace on its editor to show it here.",
            path.display(),
            project.name
        );
        self.notices.raise(Notice::new("sub-workspace:project-file", Severity::Warning, message));
        true
    }

    /// The sub-workspaces as the Sync to Sub-workspace menu lists them.
    fn sub_targets(&self) -> Vec<SubTarget> {
        self.subs
            .list
            .iter()
            .map(|sub| SubTarget {
                id: sub.id,
                name: sub.name.clone(),
                tabs: self
                    .workspaces
                    .get(&sub.id)
                    .map(|ws| ws.layout.tabs.iter().map(|t| (t.id, t.title.clone())).collect())
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Show project `pid`'s `panel` in a sub-workspace (a new one when `sub` is `None`), in a new
    /// tab or beside the active panel of `tab`, and open its window. `moved`: the panel leaves its
    /// project's tabs to live there until it returns, rather than showing in both. Returns the
    /// sub-workspace.
    fn sync_to(
        &mut self,
        pid: ProjectId,
        panel: PanelId,
        sub: Option<ProjectId>,
        tab: Option<TabId>,
        moved: bool,
    ) -> Option<ProjectId> {
        if self.subs.contains(pid) {
            return None;
        }
        let showable =
            self.workspaces.get(&pid).and_then(|ws| ws.layout.panels.get(&panel)).is_some_and(|p| {
                matches!(p.kind, PanelKind::Terminal(_) | PanelKind::Editor(_) | PanelKind::Preview(_))
            });
        if !showable {
            return None;
        }
        let mirror = PanelKind::Mirror(MirrorPanelConfig { project: pid, panel });
        let id = match sub.filter(|s| self.subs.contains(*s)) {
            Some(id) => {
                self.load_workspace(id);
                let ws = self.workspaces.get_mut(&id)?;
                let anchor = tab
                    .and_then(|t| ws.layout.tab(t))
                    .and_then(|t| t.active_panel.or_else(|| t.root.panels().first().copied()));
                match anchor {
                    Some(anchor) => {
                        ws.layout.add_panel(id, Some(anchor), Placement::Right, mirror);
                    }
                    None => {
                        ws.layout.add_tab(id, mirror);
                    }
                }
                ws.rebuild_all();
                ws.mark_dirty();
                id
            }
            None => {
                let id = self.subs.create();
                let mut layout = Layout::new_default(id);
                if let Some(first) = layout.panels.keys().next().copied() {
                    layout.set_kind(first, mirror);
                }
                let mut ws = ProjectWorkspace::new(layout);
                ws.mark_dirty();
                self.workspaces.insert(id, ws);
                id
            }
        };
        if let Some(sub) = self.subs.get_mut(id) {
            sub.open = true;
        }
        self.save_subs();
        if moved && let Some(ws) = self.workspaces.get_mut(&pid) {
            ws.layout.send_away(panel);
            ws.rebuild_all();
            ws.mark_dirty();
        }
        Some(id)
    }

    /// Move every panel of a project's tab that a sub-workspace can show into a new one, in one tab.
    fn move_tab(&mut self, pid: ProjectId, tab: TabId) {
        let panels = self.workspaces.get(&pid).and_then(|ws| ws.layout.tab(tab)).map(|t| t.root.panels());
        let mut target: Option<(ProjectId, TabId)> = None;
        for panel in panels.unwrap_or_default() {
            let Some(sub) = self.sync_to(pid, panel, target.map(|t| t.0), target.map(|t| t.1), true) else {
                continue;
            };
            if target.is_none() {
                target = self.workspaces.get(&sub).and_then(|ws| ws.layout.tabs.first()).map(|t| (sub, t.id));
            }
        }
    }

    /// A moved panel comes back to its project's tabs.
    fn return_home(&mut self, project: ProjectId, panel: PanelId) {
        self.load_workspace(project);
        if let Some(ws) = self.workspaces.get_mut(&project)
            && ws.layout.bring_back(panel)
        {
            ws.rebuild_all();
            ws.mark_dirty();
        }
    }

    /// Take away every mirror of `project`'s panels (only `panel`'s, when given); a sub-workspace
    /// left with no tab goes.
    fn drop_mirrors(&mut self, project: ProjectId, panel: Option<PanelId>) {
        let ids: Vec<ProjectId> = self.subs.list.iter().map(|s| s.id).collect();
        for id in ids {
            self.load_workspace(id);
            let Some(ws) = self.workspaces.get_mut(&id) else { continue };
            let mirrors = ws.layout.mirrors_of(project, panel);
            if mirrors.is_empty() {
                continue;
            }
            for mirror in &mirrors {
                ws.layout.remove_panel(*mirror);
            }
            ws.rebuild_all();
            ws.mark_dirty();
            let empty = ws.layout.tabs.is_empty();
            for mirror in mirrors {
                self.docs.forget_view(mirror);
                self.previews.remove(&mirror);
            }
            if empty {
                self.remove_sub(id);
            }
        }
    }

    /// Destroy a sub-workspace: its own panels end with it (their terminals too), the project
    /// panels it showed carry on in their projects, and those moved there go back to them.
    fn remove_sub(&mut self, id: ProjectId) {
        self.load_workspace(id);
        if let Some(ws) = self.workspaces.remove(&id) {
            for panel in ws.layout.panels.values() {
                if let PanelKind::Mirror(m) = &panel.kind {
                    self.return_home(m.project, m.panel);
                }
            }
            let client = self.link.client();
            for panel in ws.layout.panels.values() {
                match &panel.kind {
                    PanelKind::Terminal(_) => self.hub.kill(panel.id, client),
                    PanelKind::Editor(config) => {
                        let key = Documents::panel_key(&self.rules, panel.id, config.path.as_deref());
                        if self.panels_showing(&key) == 0 {
                            self.docs.remove(&key);
                        } else {
                            self.docs.drop_view(&key, panel.id);
                        }
                    }
                    _ => self.docs.forget_view(panel.id),
                }
                self.previews.remove(&panel.id);
                self.pickers.remove(&panel.id);
            }
        }
        if let Err(e) = self.store.delete_project(id) {
            self.store_failed("delete the sub-workspace", &e);
        }
        self.subs.remove(id);
        self.save_subs();
    }

    /// What each mirror in sub-workspace `id` shows, loading the projects they come from.
    fn resolve_mirrors(&mut self, id: ProjectId) -> HashMap<PanelId, MirrorSource> {
        let mirrors: Vec<(PanelId, MirrorPanelConfig)> = self
            .workspaces
            .get(&id)
            .map(|ws| {
                ws.layout
                    .panels
                    .values()
                    .filter_map(|p| match &p.kind {
                        PanelKind::Mirror(m) => Some((p.id, *m)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut out = HashMap::new();
        for (mirror, source) in mirrors {
            let Some(project) = self.book.get(source.project).cloned() else { continue };
            self.load_workspace(source.project);
            let Some(record) =
                self.workspaces.get(&source.project).and_then(|ws| ws.layout.panels.get(&source.panel))
            else {
                continue;
            };
            let title = match &record.kind {
                PanelKind::Editor(config) if !record.title_is_custom => config
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map_or_else(|| "Untitled".to_owned(), |n| n.to_string_lossy().into_owned()),
                _ => record.title.clone(),
            };
            let away = self.workspaces.get(&source.project).is_some_and(|ws| ws.layout.is_away(source.panel));
            out.insert(
                mirror,
                MirrorSource { project, panel: source.panel, kind: record.kind.clone(), title, away },
            );
        }
        out
    }

    /// Draw every open sub-workspace in a window of its own.
    fn sub_windows(&mut self, ctx: &Context) {
        let screen = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1920.0, 1080.0));
        let open: Vec<ProjectId> = self.subs.list.iter().filter(|s| s.open).map(|s| s.id).collect();
        for id in open {
            let Some(sub) = self.subs.get(id).cloned() else { continue };
            // A window saved on a monitor that is gone opens on one that is here.
            let place = sub
                .place
                .unwrap_or(Place { x: 160.0, y: 120.0, width: 900.0, height: 600.0 })
                .on_screen((screen.x, screen.y));
            let builder = egui::ViewportBuilder::default()
                .with_title(format!("{} — throng", sub.name))
                .with_app_id("throng")
                .with_inner_size([place.width, place.height])
                .with_position([place.x, place.y]);
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("throng-sub", id)),
                builder,
                |ui, class| {
                    self.sub_window(ui, id, class);
                },
            );
        }
        if self.subs_dirty {
            self.save_subs();
        }
    }

    /// Bring every throng window forward when one is brought forward from another app, ending
    /// with the one the user chose so it keeps the keyboard.
    fn raise_together(&mut self, ctx: &Context) {
        let seen = std::mem::take(&mut self.window_focus);
        let windows: Vec<egui::ViewportId> = seen.iter().map(|(id, _)| *id).collect();
        let focused = seen.iter().find(|(_, f)| *f).map(|(id, _)| *id);
        for window in self.focus_group.update(focused, &windows, Instant::now()) {
            ctx.send_viewport_cmd_to(window, ViewportCommand::Focus);
        }
    }

    fn sub_window(&mut self, ui: &mut Ui, id: ProjectId, class: egui::ViewportClass) {
        let ctx = ui.ctx().clone();
        let Some(name) = self.subs.get(id).map(|s| s.name.clone()) else { return };
        if class == egui::ViewportClass::Immediate {
            self.window_focus.push((ctx.viewport_id(), ctx.input(|i| i.viewport().focused.unwrap_or(false))));
            if ctx.input(|i| i.viewport().close_requested()) {
                // Closing the window keeps the sub-workspace; the sidebar opens it again.
                if let Some(sub) = self.subs.get_mut(id) {
                    sub.open = false;
                }
                self.subs_dirty = true;
                return;
            }
            let (outer, inner) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
            if let (Some(outer), Some(inner)) = (outer, inner) {
                let now =
                    Place { x: outer.min.x, y: outer.min.y, width: inner.width(), height: inner.height() };
                if let Some(sub) = self.subs.get_mut(id)
                    && sub.place.is_none_or(|p| {
                        (p.x - now.x).abs() > 1.0
                            || (p.y - now.y).abs() > 1.0
                            || (p.width - now.width).abs() > 1.0
                            || (p.height - now.height).abs() > 1.0
                    })
                {
                    sub.place = Some(now);
                    self.subs_dirty = true;
                }
            }
        } else {
            // Shown inside the main window (a backend without windows of its own, or tests).
            ui.set_min_size(egui::vec2(760.0, 460.0));
        }
        ui.horizontal(|ui| {
            ui.strong(&name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close Window").on_hover_text("The sub-workspace stays in the sidebar").clicked()
                {
                    if let Some(sub) = self.subs.get_mut(id) {
                        sub.open = false;
                    }
                    self.subs_dirty = true;
                }
            });
        });
        ui.separator();
        self.load_workspace(id);
        let mirrors = self.resolve_mirrors(id);
        let Some(project) = self.sub_project(id) else { return };
        let Some(ws) = self.workspaces.get_mut(&id) else { return };
        let mut actions = Vec::new();
        let mut focus = Focus::default();
        let mut panels = PanelCtx {
            project: &project,
            hub: &mut self.hub,
            client: self.link.client(),
            docs: &mut self.docs,
            rules: &self.rules,
            settings: &self.settings,
            look: &self.look,
            actions: &mut actions,
            focus: &mut focus,
            pickers: &mut self.pickers,
            open_errors: &mut self.open_errors,
            searches: &mut self.searches,
            previews: &mut self.previews,
            mirrors: &mirrors,
            subs: &[],
            drawn: &mut self.drawn,
            sub_workspace: true,
        };
        workspace_ui::show(ui, ws, &mut panels, &mut self.renaming_tab);
        self.apply_sub_actions(&ctx, id, actions);
    }

    /// Carry out what a sub-workspace's panels asked for. What a mirror asks of its content (save,
    /// restart, reload…) goes to the project panel it shows; what it asks of its place (close,
    /// split, rename) stays here. A file a link or preview opens goes to the project that owns it.
    fn apply_sub_actions(&mut self, ctx: &Context, id: ProjectId, actions: Vec<PanelAction>) {
        let mut here = Vec::new();
        let mut there: Vec<(ProjectId, PanelAction)> = Vec::new();
        for action in actions {
            let target = match &action {
                PanelAction::KillTerminal(p)
                | PanelAction::RestartTerminal(p)
                | PanelAction::Save(p)
                | PanelAction::SaveAs(p)
                | PanelAction::Reload(p)
                | PanelAction::KeepMine(p)
                | PanelAction::RetryOpen(p)
                | PanelAction::GotoLine(p)
                | PanelAction::PickLanguage(p)
                | PanelAction::PreviewBack(p)
                | PanelAction::PreviewForward(p) => Some(*p),
                PanelAction::PreviewLink { panel, .. } | PanelAction::PreviewJump { panel, .. } => {
                    Some(*panel)
                }
                _ => None,
            };
            let mirror = target.and_then(|p| {
                self.workspaces.get(&id).and_then(|ws| ws.layout.panels.get(&p)).and_then(|r| match r.kind {
                    PanelKind::Mirror(m) => Some(m),
                    _ => None,
                })
            });
            let file = match &action {
                PanelAction::OpenPreview(path) | PanelAction::OpenInEditor(path) => Some(path.clone()),
                PanelAction::Link { target: Target::File { path, .. }, base, .. } => {
                    Some(base.as_ref().map_or_else(|| PathBuf::from(path), |b| b.join(path)))
                }
                _ => None,
            };
            if let Some(m) = mirror {
                there.push((m.project, retarget(action, m.panel)));
            } else if let Some(owner) = file.and_then(|f| self.book.owner_of(&self.rules, &f).map(|p| p.id)) {
                there.push((owner, action));
            } else {
                here.push(action);
            }
        }
        self.apply_actions_in(ctx, id, here);
        for (project, action) in there {
            if self.book.active_id() != Some(project) && !self.subs.contains(project) {
                // Whatever opens, opens where it can be seen.
                if matches!(
                    action,
                    PanelAction::OpenPreview(_) | PanelAction::OpenInEditor(_) | PanelAction::Link { .. }
                ) {
                    self.switch_project(project);
                }
            }
            self.apply_actions_in(ctx, project, vec![action]);
        }
    }

    /// Load a project's (or a sub-workspace's) layout. An unreadable layout is quarantined before a
    /// fresh one replaces it, and the user is told once.
    fn load_workspace(&mut self, id: ProjectId) {
        if self.workspaces.contains_key(&id) {
            return;
        }
        let Some(project) = self.owner(id) else { return };
        // A project starts with a terminal; a sub-workspace with an empty panel, starting nothing.
        let is_project = self.book.get(id).is_some();
        let fresh = || {
            let mut layout = Layout::new_default(id);
            if let Some(panel) = layout.panels.keys().next().copied()
                && is_project
            {
                layout.set_kind(panel, Self::default_terminal());
            }
            layout
        };
        let (layout, save) = match self.store.layout(id) {
            Ok(LayoutLoad::Loaded(mut layout)) => {
                let orphans = layout.prune_orphans();
                // With manual reload, the terminals a saved layout holds wait to be reloaded; one
                // still running reattaches all the same.
                if self.settings.manual_reload() {
                    self.hub.keep_dormant(layout.terminal_panels().map(|(panel, _)| panel.id));
                }
                (layout, !orphans.is_empty())
            }
            Ok(LayoutLoad::Missing) => (fresh(), true),
            Ok(LayoutLoad::Unreadable { raw, error }) => {
                let kept = self.store.quarantine_layout(id, &raw, &error.to_string(), now_ms()).is_ok();
                self.notices.raise(Notice::new(
                    format!("layout:{id}"),
                    Severity::Warning,
                    format!(
                        "The saved layout of \"{}\" could not be read ({error}). {}A fresh layout was started.",
                        project.name,
                        if kept { "The old one was kept aside in the database. " } else { "" }
                    ),
                ));
                (fresh(), kept)
            }
            Err(e) => {
                self.store_failed("read the layout", &e);
                (fresh(), false)
            }
        };
        let mut ws = ProjectWorkspace::new(layout);
        if save {
            ws.mark_dirty();
        }
        self.workspaces.insert(id, ws);
        self.explorers.entry(id).or_insert_with(|| Explorer::new(project.root.clone()));
    }

    fn delete_project(&mut self, id: ProjectId) {
        // Sub-workspaces showing its panels show them no longer.
        self.drop_mirrors(id, None);
        self.load_workspace(id);
        if let Some(ws) = self.workspaces.remove(&id) {
            let client = self.link.client();
            for (panel, _) in ws.layout.terminal_panels() {
                self.hub.kill(panel.id, client);
            }
        }
        self.explorers.remove(&id);
        if let Err(e) = self.store.delete_project(id) {
            self.store_failed("delete the project", &e);
        }
        let was_active = self.book.active_id() == Some(id);
        let _ = self.book.delete(id);
        let order: Vec<ProjectId> = self.book.projects().iter().map(|p| p.id).collect();
        let _ = self.store.set_positions(&order);
        if was_active {
            match self.book.active_id() {
                Some(next) => self.switch_project(next),
                None => {
                    let _ = self.store.set_state(ACTIVE_PROJECT_KEY, None);
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Background events

    fn process_link(&mut self, ctx: &Context) {
        for event in self.link.poll() {
            match event {
                LinkEvent::Connected => self.hub.reconnected(),
                LinkEvent::Lost => self.hub.disconnected(),
                LinkEvent::Daemon(event) => {
                    let out = self.hub.on_daemon_event(event, self.link.client());
                    self.hub_events(ctx, out);
                }
            }
        }
        let queued = self.hub.take_queued();
        self.hub_events(ctx, queued);
        match self.link.outage() {
            Some(message) => {
                let mut notice = Notice::new("daemon", Severity::Error, message);
                if matches!(self.link.state, LinkState::Blocked { .. }) {
                    notice = notice.with_action("replace-daemon", "Restart Terminal Host");
                }
                self.notices.raise_quietly(notice);
            }
            None => {
                self.notices.dismiss("daemon");
            }
        }
        if let Some(wake) = self.link.next_wake() {
            ctx.request_repaint_after(wake);
        }
    }

    fn hub_events(&mut self, ctx: &Context, events: Vec<HubEvent>) {
        for event in events {
            match event {
                // Quitting closed the idle shells itself; their ends are not the user's, and the
                // panels must come back as terminals that start fresh (Principle III), not as
                // empty panels saved on the way out.
                HubEvent::Ended { .. } if self.closing == Closing::Confirmed => {}
                HubEvent::Ended { panel, config } => {
                    // A deliberate or clean end: the panel stays, empty and ready (Principle III),
                    // with the picker remembering what ran there.
                    for ws in self.workspaces.values_mut() {
                        if ws.layout.panels.contains_key(&panel) {
                            ws.layout.set_kind(panel, PanelKind::Untyped);
                            ws.mark_dirty();
                        }
                    }
                    self.pickers.insert(panel, Picker::remembering(&config));
                }
                HubEvent::Clipboard(text) => ctx.copy_text(text),
                HubEvent::Directory { panel, dir } => self.remember_directory(panel, dir),
                HubEvent::Observed { panel, running } => {
                    self.update_terminal(panel, |config| config.observe(running));
                }
                HubEvent::Captured(panel) => {
                    self.update_terminal(panel, TerminalPanelConfig::capture);
                }
                HubEvent::Failed { .. } | HubEvent::TitleChanged(_) => {}
            }
        }
    }

    /// Change a terminal panel's saved configuration wherever its layout is; `change` says whether
    /// it changed anything, and only then is the layout saved.
    fn update_terminal(&mut self, panel: PanelId, change: impl FnOnce(&mut TerminalPanelConfig) -> bool) {
        let Some(ws) = self.workspaces.values_mut().find(|ws| ws.layout.panels.contains_key(&panel)) else {
            return;
        };
        if let Some(PanelKind::Terminal(config)) = ws.layout.panels.get_mut(&panel).map(|r| &mut r.kind)
            && change(config)
        {
            ws.mark_dirty();
        }
    }

    /// A terminal the app is about to end: while it remembers commands, what it runs this moment
    /// becomes its startup command (nothing running leaves that as it was).
    fn capture_before_ending(&mut self, panel: PanelId, running: Option<Option<String>>) {
        self.update_terminal(panel, |config| {
            let observed = running.is_some_and(|now| config.observe(now));
            config.capture() || observed
        });
    }

    /// A terminal's shell moved: while the panel remembers its directory, keep the new
    /// one with the layout, so a cold start goes back there. Only folders inside the project count.
    fn remember_directory(&mut self, panel: PanelId, dir: PathBuf) {
        let remember_default = self.settings.remember_directory();
        for (pid, ws) in &mut self.workspaces {
            let Some(root) = self.book.get(*pid).map(|p| p.root.clone()) else { continue };
            let Some(record) = ws.layout.panels.get_mut(&panel) else { continue };
            if let PanelKind::Terminal(config) = &mut record.kind
                && config.remember_directory.unwrap_or(remember_default)
                && self.rules.is_within(&root, &dir)
                && config.last_directory.as_ref() != Some(&dir)
            {
                config.last_directory = Some(dir.clone());
                ws.mark_dirty();
            }
        }
    }

    fn process_watch(&mut self, ctx: &Context) {
        let changed = self.watcher.drain();
        if !changed.is_empty() {
            let settings_file = self.dirs.settings_file();
            let max = self.settings.max_open_file_bytes();
            let packs = self.icon_packs_dir();
            if changed.iter().any(|p| p.starts_with(&packs)) {
                self.rescan_icon_packs();
            }
            if changed.iter().any(|p| self.themes.owns(p)) {
                self.themes.reload();
                self.prefs.reapply(&mut self.themes);
                self.report_unusable_themes();
            }
            let keybindings_file = self.dirs.keybindings_file();
            for path in &changed {
                if *path == settings_file {
                    self.reload_settings();
                }
                if *path == keybindings_file {
                    self.reload_keymap(ctx);
                }
                for explorer in self.explorers.values_mut() {
                    explorer.invalidate(path);
                    if let Some(parent) = path.parent() {
                        explorer.invalidate(parent);
                    }
                }
            }
            for (_, doc) in self.docs.iter_mut() {
                let Some(path) = doc.path.clone() else { continue };
                if changed.contains(&path) || path.parent().is_some_and(|p| changed.contains(p)) {
                    doc.check_disk(max);
                }
            }
        }
        let mut wanted = self.docs.watched_dirs();
        if let Some(id) = self.book.active_id()
            && let Some(explorer) = self.explorers.get(&id)
        {
            wanted.extend(explorer.watched_dirs());
        }
        wanted.push(self.dirs.config.clone());
        if self.themes.dir().is_dir() {
            wanted.push(self.themes.dir().to_path_buf());
        }
        // The packs folder and each pack in it, so a pack.json edit is seen.
        let packs = self.icon_packs_dir();
        if let Ok(entries) = std::fs::read_dir(&packs) {
            wanted.extend(entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()));
            wanted.push(packs);
        }
        self.watcher.sync(wanted);
        if let Some(error) = self.watcher.error.take() {
            self.notices.raise(Notice::new("watcher", Severity::Warning, error));
        }
    }

    /// One notice for every theme file that could not be used (read, parsed, or named apart from
    /// the others); none once they all can.
    fn report_unusable_themes(&mut self) {
        let unusable = &self.themes.unusable;
        if unusable.is_empty() {
            self.notices.dismiss("themes:unusable");
            return;
        }
        let names: Vec<&str> = unusable.iter().map(|u| u.file.as_str()).collect();
        let detail =
            unusable.iter().map(|u| format!("{}: {}", u.file, u.reason)).collect::<Vec<_>>().join("\n");
        self.notices.raise(
            Notice::new(
                "themes:unusable",
                Severity::Warning,
                format!("Some theme files were left out: {}.", names.join(", ")),
            )
            .with_detail(detail),
        );
    }

    fn icon_packs_dir(&self) -> PathBuf {
        self.dirs.config.join("icon-packs")
    }

    fn rescan_icon_packs(&mut self) {
        let (packs, problems) = crate::icons::scan_packs(&self.icon_packs_dir());
        self.icon_packs = packs;
        self.icon_pack_problems = problems;
        self.icons_applied = None;
    }

    /// Put the icon pack the settings name in force, when that changed. Whatever of it cannot be
    /// drawn keeps throng's icon, and one notice says what.
    fn apply_icons(&mut self, ctx: &Context) {
        let wanted = self.settings.icon_pack().trim().to_owned();
        if self.icons_applied.as_deref() == Some(wanted.as_str()) {
            return;
        }
        let mut problems: Vec<String> =
            self.icon_pack_problems.iter().map(|(name, reason)| format!("{name}: {reason}")).collect();
        let set = if wanted.is_empty() {
            throng_core::icons::IconSet::default()
        } else if let Some(pack) = self.icon_packs.iter().find(|p| p.name.eq_ignore_ascii_case(&wanted)) {
            let folder = self.icon_packs_dir().join(&pack.name);
            let (set, kept) = throng_core::icons::IconSet::with_pack(
                pack,
                |g| crate::icons::drawable(ctx, g),
                |written| crate::icons::pack_image(&folder, written),
            );
            if !kept.is_empty() {
                problems.push(format!(
                    "{}: {} kept throng's icon (a glyph the fonts cannot draw, or an image that is \
                     missing, outside the pack, too large, or not SVG or PNG).",
                    pack.name,
                    kept.join(", ")
                ));
            }
            set
        } else {
            problems.push(format!(
                "There is no icon pack called \"{wanted}\" in {}.",
                self.icon_packs_dir().display()
            ));
            throng_core::icons::IconSet::default()
        };
        crate::icons::install(ctx, std::sync::Arc::new(set));
        self.icons_applied = Some(wanted);
        if problems.is_empty() {
            self.notices.dismiss("icons:problems");
        } else {
            self.notices.raise(
                Notice::new(
                    "icons:problems",
                    Severity::Warning,
                    "Some icons could not come from the icon pack; throng's own stand in.",
                )
                .with_detail(problems.join("\n")),
            );
        }
    }

    /// Read `keybindings.json` (absent means the defaults) and put it in force. Anything in it that
    /// cannot be used is listed in one notice; the rest applies.
    fn reload_keymap(&mut self, ctx: &Context) {
        let path = self.dirs.keybindings_file();
        let (map, problems) = match std::fs::read_to_string(&path) {
            Ok(text) => throng_core::keymap::Keymap::parse(&text, crate::keymap::mac()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (throng_core::keymap::Keymap::defaults(crate::keymap::mac()), Vec::new())
            }
            Err(e) => (
                throng_core::keymap::Keymap::defaults(crate::keymap::mac()),
                vec![throng_core::failure::describe(&e, throng_core::failure::Operation::Read, &path)],
            ),
        };
        let unknown: Vec<String> = throng_core::keymap::COMMANDS
            .iter()
            .flat_map(|c| {
                map.chords(c.id).iter().filter(|k| !crate::keymap::known(k)).map(move |k| (c.id, k))
            })
            .map(|(id, k)| format!("\"{id}\": {k} names a key throng does not know."))
            .collect();
        let problems: Vec<String> = problems.into_iter().chain(unknown).collect();
        if problems.is_empty() {
            self.notices.dismiss("keybindings:problems");
        } else {
            self.notices.raise(
                Notice::new(
                    "keybindings:problems",
                    Severity::Warning,
                    format!(
                        "Some key bindings in keybindings.json were left out ({}); the rest apply.",
                        problems.len()
                    ),
                )
                .with_detail(problems.join("\n")),
            );
        }
        self.keymap = std::sync::Arc::new(map);
        crate::keymap::install(ctx, std::sync::Arc::clone(&self.keymap));
    }

    fn reload_settings(&mut self) {
        let (settings, outcome) = read_settings(&self.dirs.settings_file());
        match outcome {
            ReadOutcome::Malformed { reason } => {
                // Keep running on what we had; never write over a file the user is fixing.
                self.notices.raise(settings_malformed(&reason));
                return;
            }
            ReadOutcome::Loaded { ref corrected } if !corrected.is_empty() => {
                self.settings = settings;
                self.write_settings();
            }
            _ => self.settings = settings,
        }
        self.notices.dismiss("settings:malformed");
        self.hub.default_shell = self.settings.default_shell().map(str::to_owned);
        self.hub.scrollback = self.settings.scrollback_lines();
        for explorer in self.explorers.values_mut() {
            explorer.invalidate_all();
        }
    }

    fn write_settings(&mut self) {
        let path = self.dirs.settings_file();
        match throng_platform::fs::atomic_write(&path, self.settings.to_json().as_bytes()) {
            Ok(()) => {
                self.hub.default_shell = self.settings.default_shell().map(str::to_owned);
                self.hub.scrollback = self.settings.scrollback_lines();
            }
            Err(e) => {
                self.notices.raise(Notice::new(
                    "settings:write",
                    Severity::Error,
                    throng_core::failure::describe(&e, throng_core::failure::Operation::Write, &path),
                ));
            }
        }
    }

    fn flush_layouts(&mut self, force: bool) {
        let now = now_ms();
        let mut failures = Vec::new();
        for (id, ws) in &mut self.workspaces {
            let Some(since) = ws.dirty_at else { continue };
            if !force && since.elapsed() < SAVE_DEBOUNCE {
                continue;
            }
            ws.layout.prune_orphans();
            if let Err(e) = ws.layout.check() {
                tracing::error!(project = %id, error = %e, "refusing to save an inconsistent layout");
                ws.dirty_at = None;
                continue;
            }
            match self.store.save_layout(*id, &ws.layout, now) {
                Ok(()) => ws.dirty_at = None,
                Err(e) => failures.push(e.to_string()),
            }
        }
        if let Some(error) = failures.first() {
            self.notices.raise(Notice::new(
                "layout:save",
                Severity::Error,
                format!("Could not save the layout: {error}"),
            ));
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Actions

    fn active_panel(&self) -> Option<PanelId> {
        let ws = self.workspaces.get(&self.book.active_id()?)?;
        self.focus.panel.filter(|p| ws.layout.panels.contains_key(p)).or_else(|| {
            ws.layout.active_tab().and_then(|t| t.active_panel.or_else(|| t.root.panels().first().copied()))
        })
    }

    fn capture_chord(&mut self, ctx: &Context) {
        let path = self.dirs.settings_file().display().to_string();
        let keybindings_path = self.dirs.keybindings_file();
        let before = std::sync::Arc::clone(&self.keymap);
        let mut p = crate::prefs::PrefsCtx {
            settings: &mut self.settings,
            themes: &mut self.themes,
            shells: &self.hub.shells,
            icon_packs: &[],
            path: &path,
            system_dark: true,
            keymap: &mut self.keymap,
            keybindings_path: &keybindings_path,
        };
        self.prefs.capture(ctx, &mut p);
        if !std::sync::Arc::ptr_eq(&before, &self.keymap) {
            crate::keymap::install(ctx, std::sync::Arc::clone(&self.keymap));
        }
    }

    /// The commands live everywhere, taken before any panel sees the keys (the keymap decides
    /// which chords).
    fn shortcuts(&mut self, ctx: &Context) -> Vec<PanelAction> {
        let mut actions = Vec::new();
        let take = |id| crate::keymap::take(ctx, id, None);
        if take("navigate.quickOpen") {
            self.quick_open(ctx);
        }
        if take("search.findInFiles") {
            self.find_in_files(ctx, false, true, None);
        }
        if take("search.replaceInFiles") {
            self.find_in_files(ctx, true, true, None);
        }
        if take("panel.splitRight")
            && let Some(panel) = self.active_panel()
        {
            actions.push(PanelAction::Split(panel, Placement::Right, Self::default_terminal()));
        }
        if take("panel.splitDown")
            && let Some(panel) = self.active_panel()
        {
            actions.push(PanelAction::Split(panel, Placement::Below, Self::default_terminal()));
        }
        if take("panel.close")
            && let Some(panel) = self.active_panel()
        {
            actions.push(PanelAction::Close(panel));
        }
        if take("project.new") {
            self.dialog = Some(Dialog::Project(ProjectForm::new(self.book.projects().len(), String::new())));
        }
        if take("app.preferences") {
            self.prefs.open = true;
        }
        if take("view.toggleExplorer") {
            self.show_explorer = !self.show_explorer;
        }
        if take("view.fullscreen") {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!full));
        }
        let zoom = if take("zoom.in") {
            Some(self.settings.ui_scale() + 0.1)
        } else if take("zoom.out") {
            Some(self.settings.ui_scale() - 0.1)
        } else if take("zoom.reset") {
            Some(1.0)
        } else {
            None
        };
        if let Some(scale) = zoom {
            // Rounded to the setting's step, and clamped by it.
            let scale = f64::from((scale * 20.0).round() / 20.0);
            if self.settings.set("appearance.uiScale", throng_core::settings::SettingValue::Float(scale)) {
                self.write_settings();
            }
        }
        let next = take("tabs.next");
        let previous = take("tabs.previous");
        if (next || previous)
            && let Some(ws) = self.book.active_id().and_then(|id| self.workspaces.get_mut(&id))
            && !ws.layout.tabs.is_empty()
        {
            let count = ws.layout.tabs.len();
            let current = ws
                .layout
                .active_tab()
                .and_then(|t| ws.layout.tabs.iter().position(|x| x.id == t.id))
                .unwrap_or(0);
            let target = if next { (current + 1) % count } else { (current + count - 1) % count };
            ws.layout.active_tab = Some(ws.layout.tabs[target].id);
            ws.mark_dirty();
        }
        actions
    }

    fn apply_actions(&mut self, ctx: &Context, actions: Vec<PanelAction>) {
        let Some(pid) = self.book.active_id() else { return };
        self.apply_actions_in(ctx, pid, actions);
    }

    /// Carry out what panels in the workspace `pid` (a project's or a sub-workspace's) asked for.
    #[allow(clippy::too_many_lines)]
    fn apply_actions_in(&mut self, ctx: &Context, pid: ProjectId, actions: Vec<PanelAction>) {
        for action in actions {
            match action {
                PanelAction::NewTab => {
                    if let Some(ws) = self.workspaces.get_mut(&pid) {
                        ws.layout.add_tab(pid, Self::default_terminal());
                        ws.rebuild_all();
                        ws.mark_dirty();
                    }
                }
                PanelAction::CloseTab(tab) => self.close_tab(pid, tab),
                PanelAction::Close(panel) => self.close_panel(pid, panel, false),
                PanelAction::Split(anchor, placement, kind) => {
                    if let Some(ws) = self.workspaces.get_mut(&pid) {
                        ws.layout.add_panel(pid, Some(anchor), placement, kind);
                        ws.rebuild_all();
                        ws.mark_dirty();
                    }
                }
                PanelAction::SetKind(panel, kind) => {
                    if let PanelKind::Editor(EditorPanelConfig { path: Some(path) }) = &kind
                        && self.refuse_project_file(pid, path)
                    {
                        continue;
                    }
                    let client = self.link.client();
                    if let Some(ws) = self.workspaces.get_mut(&pid) {
                        let was_terminal = ws
                            .layout
                            .panels
                            .get(&panel)
                            .is_some_and(|p| matches!(p.kind, PanelKind::Terminal(_)));
                        if was_terminal && !matches!(kind, PanelKind::Terminal(_)) {
                            self.hub.kill(panel, client);
                        }
                        ws.layout.set_kind(panel, kind);
                        ws.mark_dirty();
                    }
                    self.pickers.remove(&panel);
                    self.open_errors.remove(&panel);
                }
                PanelAction::Rename(panel) => {
                    if let Some(record) =
                        self.workspaces.get(&pid).and_then(|ws| ws.layout.panels.get(&panel))
                    {
                        self.dialog =
                            Some(Dialog::RenamePanel { panel, project: pid, name: record.title.clone() });
                    }
                }
                PanelAction::RenameTab(tab) => {
                    if let Some(t) = self.workspaces.get(&pid).and_then(|ws| ws.layout.tab(tab)) {
                        self.renaming_tab = Some((tab, t.title.clone()));
                    }
                }
                PanelAction::KillTerminal(panel) => {
                    self.capture_before_ending(panel, self.hub.observe_now(panel));
                    let config = self.terminal_config(pid, panel).unwrap_or_default();
                    self.hub.kill(panel, self.link.client());
                    if let Some(ws) = self.workspaces.get_mut(&pid) {
                        ws.layout.set_kind(panel, PanelKind::Untyped);
                        ws.mark_dirty();
                    }
                    self.pickers.insert(panel, Picker::remembering(&config));
                }
                PanelAction::RestartTerminal(panel) => self.hub.restart(panel, self.link.client()),
                PanelAction::Save(panel) => self.save_panel(pid, panel),
                PanelAction::SaveAs(panel) => {
                    let suggestion = self
                        .editor_key(pid, panel)
                        .and_then(|k| self.docs.get(&k))
                        .and_then(|d| d.path.clone())
                        .or_else(|| self.owner(pid).map(|p| p.root.join("untitled.txt")))
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    self.dialog = Some(Dialog::SaveAs { panel, path: suggestion, error: None });
                }
                PanelAction::Reload(panel) => {
                    let max = self.settings.max_open_file_bytes();
                    if let Some(key) = self.editor_key(pid, panel)
                        && let Some(doc) = self.docs.get_mut(&key)
                        && let Err(e) = doc.reload(max)
                    {
                        doc.error = Some(e);
                    }
                }
                PanelAction::KeepMine(panel) => {
                    if let Some(key) = self.editor_key(pid, panel)
                        && let Some(doc) = self.docs.get_mut(&key)
                    {
                        doc.keep_mine();
                    }
                }
                PanelAction::RetryOpen(panel) => {
                    self.open_errors.remove(&panel);
                }
                PanelAction::Copy(text) => ctx.copy_text(text),
                PanelAction::GotoLine(panel) => {
                    if let Some(doc) = self.editor_key(pid, panel).and_then(|k| self.docs.get(&k)) {
                        let lines = doc.buf.len_lines();
                        self.dialog = Some(Dialog::GotoLine { panel, input: String::new(), lines });
                    }
                }
                PanelAction::PickLanguage(panel) => self.pick_language(panel),
                PanelAction::DropFile(panel, path) => self.drop_file(pid, panel, path),
                PanelAction::SyncTo { panel, sub, tab } => {
                    self.sync_to(pid, panel, sub, tab, false);
                }
                PanelAction::MoveTo { panel, sub, tab } => {
                    self.sync_to(pid, panel, sub, tab, true);
                }
                PanelAction::TearOff(panel) => {
                    // A panel no sub-workspace can show stays where it was dragged from.
                    if self.sync_to(pid, panel, None, None, true).is_none()
                        && let Some(ws) = self.workspaces.get_mut(&pid)
                    {
                        ws.rebuild_all();
                    }
                }
                PanelAction::MoveTab(tab) => self.move_tab(pid, tab),
                PanelAction::Link { target, action, base } => self.link(ctx, pid, &target, action, base),
                PanelAction::OpenPreview(path) => self.open_preview(pid, path),
                PanelAction::OpenInEditor(path) => {
                    self.open_in_editor(pid, path);
                }
                PanelAction::PreviewLink { panel, href, action } => {
                    self.preview_link(ctx, pid, panel, &href, action);
                }
                PanelAction::PreviewBack(panel) => self.preview_step(pid, panel, false),
                PanelAction::PreviewForward(panel) => self.preview_step(pid, panel, true),
                PanelAction::PreviewJump { panel, anchor } => {
                    let Some(path) = self.previews.get(&panel).map(|s| s.path.clone()) else { continue };
                    self.preview_open(pid, panel, path, Some(anchor));
                }
                PanelAction::UpdateSearch(panel, config) => {
                    if let Some(ws) = self.workspaces.get_mut(&pid) {
                        ws.layout.set_kind(panel, PanelKind::Search(config));
                        ws.mark_dirty();
                    }
                }
                PanelAction::Search(panel, SearchAction::Run) => self.run_search(ctx, pid, panel),
                PanelAction::Search(_, SearchAction::Open { path, from, to }) => {
                    self.open_at(pid, &path, from, to)
                }
                PanelAction::Search(panel, SearchAction::Commit(commit)) => {
                    self.commit(pid, panel, commit, false)
                }
            }
        }
    }

    /// What Quick Open and Find in Files leave out for a project.
    fn exclusions(&self, pid: ProjectId) -> Exclusions {
        Exclusions {
            names: self.settings.explorer_exclude(),
            hidden: self.book.get(pid).map(|p| p.hidden_paths.clone()).unwrap_or_default(),
            keep_hidden: false,
        }
    }

    /// The focused editor's selection, when it is one non-empty line (it seeds the query).
    fn selection_seed(&self, pid: ProjectId) -> Option<String> {
        let panel = self.focus.panel?;
        let doc = self.docs.get(&self.editor_key(pid, panel)?)?;
        let view = doc.views.get(&panel)?;
        let range = view.selection.primary();
        (view.selection.len() == 1 && !range.is_empty())
            .then(|| doc.buf.rope().slice(range.from()..range.to()).to_string())
            .filter(|text| !text.contains('\n'))
    }

    /// Find (or Replace) in Files: reuse the current tab's search panel or add one.
    /// `seed` takes the focused editor's selection; `scope` is the tree's "Find in Folder…".
    fn find_in_files(&mut self, ctx: &Context, replace: bool, seed: bool, scope: Option<PathBuf>) {
        // With no project open the chords do nothing, silently.
        let Some(pid) = self.book.active_id() else { return };
        let seed = if seed { self.selection_seed(pid) } else { None };
        let anchor = self.active_panel();
        let Some(root) = self.book.get(pid).map(|p| p.root.clone()) else { return };
        let Some(ws) = self.workspaces.get_mut(&pid) else { return };
        let existing = ws.layout.active_tab().and_then(|t| {
            t.root
                .panels()
                .into_iter()
                .find(|p| matches!(ws.layout.panels.get(p).map(|x| &x.kind), Some(PanelKind::Search(_))))
        });
        let panel = match existing {
            Some(panel) => panel,
            None => ws.layout.add_panel(
                pid,
                anchor,
                Placement::Right,
                PanelKind::Search(SearchPanelConfig::default()),
            ),
        };
        let mut config = match ws.layout.panels.get(&panel).map(|p| &p.kind) {
            Some(PanelKind::Search(config)) => config.clone(),
            _ => SearchPanelConfig::default(),
        };
        config.replace = replace;
        let state = self.searches.entry(panel).or_default();
        let mut run = false;
        if let Some(scope) = scope {
            // From the tree: the scope changes, the old query and results go, nothing starts.
            config.scope = crate::project_files::relative(&root, &scope).unwrap_or_default();
            config.term.clear();
            config.replacement.clear();
            state.scan.cancel();
            state.results.clear();
            state.status = search_panel::Status::Idle;
        } else if let Some(seed) = seed {
            config.term = seed;
            run = true;
        }
        state.focus = Some(replace);
        ws.layout.set_kind(panel, PanelKind::Search(config));
        ws.layout.focus_panel(panel);
        ws.rebuild_all();
        ws.mark_dirty();
        if run {
            self.run_search(ctx, pid, panel);
        }
    }

    fn run_search(&mut self, ctx: &Context, pid: ProjectId, panel: PanelId) {
        let Some(root) = self.book.get(pid).map(|p| p.root.clone()) else { return };
        let config =
            match self.workspaces.get(&pid).and_then(|ws| ws.layout.panels.get(&panel)).map(|p| &p.kind) {
                Some(PanelKind::Search(config)) => config.clone(),
                _ => return,
            };
        let exclusions = self.exclusions(pid);
        let max = self.settings.max_open_file_bytes();
        let repaint = ctx.clone();
        self.searches
            .entry(panel)
            .or_default()
            .run(&config, root, exclusions, max, move || repaint.request_repaint());
    }

    /// Follow, copy or reveal a link. A file in the project opens at its position; a folder,
    /// or anything outside the project, is shown in the file manager; a missing file is reported.
    /// A program is never run from a link: it is revealed instead.
    fn link(
        &mut self,
        ctx: &Context,
        pid: ProjectId,
        target: &Target,
        action: LinkAction,
        base: Option<PathBuf>,
    ) {
        let root = self.book.get(pid).map(|p| p.root.clone());
        let base = base.map(|b| match &root {
            Some(root) if b.is_relative() => root.join(b),
            _ => b,
        });
        let bases = crate::links::Bases::here(base, root);
        let resolved = || match target {
            Target::File { path, .. } => Some(crate::links::resolve(path, &bases)),
            _ => None,
        };
        let report = |notices: &mut NoticeCenter, message: String| {
            notices.raise(Notice::new("link", Severity::Warning, message));
        };
        match action {
            LinkAction::Copy => ctx.copy_text(crate::links::address(target, &bases)),
            LinkAction::Reveal => {
                if let Some(path) = resolved()
                    && let Err(e) = throng_platform::fs::reveal(&path)
                {
                    report(&mut self.notices, format!("Could not open the file manager: {e}"));
                }
            }
            LinkAction::OpenDefault => {
                let Some(path) = resolved() else { return };
                if !path.exists() {
                    report(&mut self.notices, format!("{} does not exist.", path.display()));
                } else if is_program(&path) {
                    let _ = throng_platform::fs::reveal(&path);
                    report(
                        &mut self.notices,
                        format!("{} is a program; throng shows it rather than running it.", path.display()),
                    );
                } else if let Err(e) = throng_platform::fs::open_with_default_app(&path.display().to_string())
                {
                    report(&mut self.notices, format!("Could not open {}: {e}", path.display()));
                }
            }
            LinkAction::Follow => match crate::links::follow(target, &bases, &self.rules) {
                Follow::Url(url) => {
                    if let Err(e) = throng_platform::fs::open_with_default_app(&url) {
                        report(&mut self.notices, format!("Could not open {url}: {e}"));
                    }
                }
                Follow::Open { path, line, column } => self.open_at_line(pid, &path, line, column),
                Follow::Reveal(path) => {
                    if let Err(e) = throng_platform::fs::reveal(&path) {
                        report(&mut self.notices, format!("Could not open the file manager: {e}"));
                    }
                }
                Follow::Missing(path) => {
                    report(&mut self.notices, format!("{} does not exist.", path.display()));
                }
            },
        }
    }

    /// Open `path`'s preview: focus the one it has; beside its editor, on the right,
    /// when the file is open in one; otherwise standalone, where a new editor would go.
    /// Nothing outside the project is previewed.
    fn open_preview(&mut self, pid: ProjectId, path: PathBuf) {
        let inside = self.book.get(pid).is_some_and(|p| self.rules.is_within(&p.root, &path));
        if !crate::preview::previewable(&path) || !inside {
            return;
        }
        let key = Documents::key_for(&self.rules, &path);
        let anchor = self.active_panel();
        let rules = &self.rules;
        let Some(ws) = self.workspaces.get_mut(&pid) else { return };
        let same = |p: &Path| Documents::key_for(rules, p) == key;
        let existing = ws
            .layout
            .panels
            .values()
            .find(|p| matches!(&p.kind, PanelKind::Preview(c) if same(&c.path)))
            .map(|p| p.id);
        if let Some(panel) = existing {
            ws.layout.focus_panel(panel);
        } else {
            let editor = ws
                .layout
                .panels
                .values()
                .find(
                    |p| matches!(&p.kind, PanelKind::Editor(EditorPanelConfig { path: Some(e) }) if same(e)),
                )
                .map(|p| p.id);
            let kind = PanelKind::Preview(PreviewPanelConfig::new(path));
            match editor {
                Some(editor) => ws.layout.add_panel(pid, Some(editor), Placement::Right, kind),
                None => ws.layout.add_panel(pid, anchor, Placement::Stack, kind),
            };
        }
        ws.rebuild_all();
        ws.mark_dirty();
    }

    /// A link followed in a preview: web and mail links go to the system; a
    /// Markdown file in the project opens in this same preview, at its heading when one is named;
    /// another project file opens in an editor; anything else is one inline notice.
    fn preview_link(
        &mut self,
        ctx: &Context,
        pid: ProjectId,
        panel: PanelId,
        href: &str,
        action: LinkAction,
    ) {
        let Some(source) = self.previews.get(&panel).map(|s| s.path.clone()) else { return };
        match throng_core::links::classify(href) {
            Some(target @ (Target::Web(_) | Target::Scheme(_))) => {
                self.link(ctx, pid, &target, action, None);
                return;
            }
            Some(Target::File { .. }) | None => {}
        }
        let (file, anchor) = match href.split_once('#') {
            Some((file, anchor)) => (file, Some(crate::markdown::slug(anchor))),
            None => (href, None),
        };
        let file = file.replace("%20", " ");
        let file = file.strip_prefix("file://").unwrap_or(&file);
        let root = self.book.get(pid).map(|p| p.root.clone());
        let bases = crate::links::Bases::here(source.parent().map(Path::to_path_buf), root.clone());
        let resolved = crate::links::resolve(file, &bases);
        if action == LinkAction::Copy {
            ctx.copy_text(resolved.display().to_string());
            return;
        }
        let inside = root.as_deref().is_some_and(|r| self.rules.is_within(r, &resolved));
        let problem = if !resolved.exists() {
            Some(format!("{} does not exist.", resolved.display()))
        } else if !inside {
            Some(format!("{} is outside this project.", resolved.display()))
        } else {
            None
        };
        if let Some(problem) = problem {
            if let Some(state) = self.previews.get_mut(&panel) {
                state.problem = Some(problem);
            }
            return;
        }
        if resolved.is_dir() {
            let _ = throng_platform::fs::reveal(&resolved);
        } else if crate::preview::previewable(&resolved) {
            // The same preview shows the other file from now on.
            self.preview_open(pid, panel, resolved, anchor);
        } else {
            self.open_in_editor(pid, resolved);
        }
    }

    /// A preview shows `path` next (at `anchor`, a heading), as a new place in its history.
    fn preview_open(&mut self, pid: ProjectId, panel: PanelId, path: PathBuf, anchor: Option<String>) {
        let cap = self.settings.navigation_history_size();
        let Some(state) = self.previews.get_mut(&panel) else { return };
        let Some(ws) = self.workspaces.get_mut(&pid) else { return };
        let Some(PanelKind::Preview(config)) = ws.layout.panels.get(&panel).map(|p| p.kind.clone()) else {
            return;
        };
        let mut config = config;
        config.open(path.clone(), scroll_points(state.scroll), cap);
        if state.path == path {
            state.anchor = anchor;
        } else {
            let has_keys = state.has_keys;
            *state = crate::preview::PreviewState::new(path, anchor);
            state.has_keys = has_keys;
        }
        ws.layout.set_kind(panel, PanelKind::Preview(config));
        ws.mark_dirty();
    }

    /// Back (or forward) in a preview's history, to where the reader was.
    fn preview_step(&mut self, pid: ProjectId, panel: PanelId, forward: bool) {
        let cap = self.settings.navigation_history_size();
        let Some(state) = self.previews.get_mut(&panel) else { return };
        let Some(ws) = self.workspaces.get_mut(&pid) else { return };
        let Some(PanelKind::Preview(mut config)) = ws.layout.panels.get(&panel).map(|p| p.kind.clone())
        else {
            return;
        };
        config.cap(cap);
        let scroll = scroll_points(state.scroll);
        let Some(entry) = (if forward { config.forward(scroll) } else { config.back(scroll) }) else {
            return;
        };
        if state.path != entry.path {
            let has_keys = state.has_keys;
            *state = crate::preview::PreviewState::new(entry.path.clone(), None);
            state.has_keys = has_keys;
        }
        state.restore = Some(entry.scroll as f32);
        ws.layout.set_kind(panel, PanelKind::Preview(config));
        ws.mark_dirty();
    }

    /// Open `path` with the caret at a 1-based line and column (clamped to the text).
    fn open_at_line(&mut self, pid: ProjectId, path: &Path, line: Option<u32>, column: Option<u32>) {
        let max = self.settings.max_open_file_bytes();
        let eol = self.settings.default_line_ending();
        let pos = match self.docs.ensure_file(&self.rules, path, max, eol) {
            Ok(key) => self.docs.get(&key).map_or(0, |doc| {
                let rope = doc.buf.rope();
                let line = (line.unwrap_or(1).max(1) as usize - 1).min(rope.len_lines().saturating_sub(1));
                let start = throng_editor::lines::line_start(rope, line);
                let len = throng_editor::lines::line_text(rope, line).chars().count();
                start + (column.unwrap_or(1).max(1) as usize - 1).min(len)
            }),
            Err(_) => 0,
        };
        self.open_at(pid, path, pos, pos);
    }

    /// Open `path` in an editor with `from..to` selected (a search result).
    fn open_at(&mut self, pid: ProjectId, path: &Path, from: usize, to: usize) {
        let Some(panel) = self.open_in_editor(pid, path.to_path_buf()) else { return };
        let max = self.settings.max_open_file_bytes();
        let eol = self.settings.default_line_ending();
        if let Ok(key) = self.docs.ensure_file(&self.rules, path, max, eol)
            && let Some(doc) = self.docs.get_mut(&key)
        {
            let len = doc.buf.len_chars();
            doc.views.entry(panel).or_default().select(from.min(len), to.min(len));
        }
        self.focus_next = Some((crate::editor::editor_id(panel), 0));
    }

    /// Commit replacements from a search panel. Open documents change through
    /// their buffer (one undo step each); files that are not open are written straight to disk,
    /// after a warning. Every match is checked against the current text first.
    fn commit(&mut self, pid: ProjectId, panel: PanelId, commit: Commit, confirmed: bool) {
        let replacement =
            match self.workspaces.get(&pid).and_then(|ws| ws.layout.panels.get(&panel)).map(|p| &p.kind) {
                Some(PanelKind::Search(config)) => config.replacement.clone(),
                _ => return,
            };
        let Some(state) = self.searches.get(&panel) else { return };
        let targets: Vec<(usize, Vec<file_search::FileMatch>)> = match commit {
            Commit::All => state.results.iter().enumerate().map(|(i, f)| (i, f.matches.clone())).collect(),
            Commit::File(fi) => {
                state.results.get(fi).map(|f| vec![(fi, f.matches.clone())]).unwrap_or_default()
            }
            Commit::Match(fi, mi) => state
                .results
                .get(fi)
                .and_then(|f| f.matches.get(mi))
                .map(|m| vec![(fi, vec![m.clone()])])
                .unwrap_or_default(),
        };
        let unopened: Vec<usize> = targets
            .iter()
            .filter(|(fi, _)| {
                self.docs.get(&Documents::key_for(&self.rules, &state.results[*fi].path)).is_none()
            })
            .map(|(fi, _)| *fi)
            .collect();
        if !confirmed && !unopened.is_empty() && self.settings.warn_irreversible_commit() {
            let matches = targets.iter().filter(|(fi, _)| unopened.contains(fi)).map(|(_, m)| m.len()).sum();
            self.dialog = Some(Dialog::ConfirmReplace { panel, commit, files: unopened.len(), matches });
            return;
        }
        let max = self.settings.max_open_file_bytes();
        let (mut replaced, mut refused, mut files) = (0usize, 0usize, 0usize);
        let mut failures = Vec::new();
        let mut committed: Vec<(usize, Vec<file_search::FileMatch>)> = Vec::new();
        for (fi, matches) in targets {
            let Some(file) = self.searches.get(&panel).and_then(|s| s.results.get(fi)).cloned() else {
                continue;
            };
            let key = Documents::key_for(&self.rules, &file.path);
            let outcome = if let Some(doc) = self.docs.get_mut(&key) {
                let was_clean = !doc.is_dirty();
                let (edits, r) = file_search::verified_edits(doc.buf.rope(), &matches, &replacement);
                let n = edits.len();
                if n > 0 {
                    let tx = throng_editor::Transaction::replace(doc.buf.rope(), edits);
                    doc.transact(throng_editor::Selection::point(0), tx);
                    // A clean document is saved; one with edits of its own stays dirty.
                    if was_clean && let Err(e) = doc.save_to(None) {
                        failures.push(format!("{}: {e}", file.rel));
                    }
                }
                Ok((n, r))
            } else {
                file_search::replace_on_disk(&file.path, &matches, &replacement, max)
            };
            match outcome {
                Ok((n, r)) => {
                    replaced += n;
                    refused += r;
                    files += usize::from(n > 0);
                    committed.push((fi, matches));
                }
                Err(e) => failures.push(format!("{}: {e}", file.rel)),
            }
        }
        if let Some(state) = self.searches.get_mut(&panel) {
            // Committed rows leave the list; our own writes do not make a file stale.
            for (fi, done) in &committed {
                if let Some(file) = state.results.get_mut(*fi) {
                    file.matches.retain(|m| !done.contains(m));
                    file.stamp = search_panel::stamp_of(&file.path);
                }
            }
            state.results.retain(|f| !f.matches.is_empty());
            state.selected = None;
            state.summary = Some(format!("Replaced {replaced} in {files} file(s)."));
        }
        if let Some(explorer) = self.explorers.get_mut(&pid) {
            explorer.invalidate_all();
        }
        if refused > 0 || !failures.is_empty() {
            // One summary for a failed or partial commit.
            let mut message = String::new();
            if refused > 0 {
                message.push_str(&format!(
                    "{refused} match(es) had changed since the search and were left alone. "
                ));
            }
            if !failures.is_empty() {
                message.push_str(&format!("{} file(s) could not be written.", failures.len()));
            }
            let mut notice = Notice::new("search:commit", Severity::Warning, message.trim().to_owned());
            if !failures.is_empty() {
                notice = notice.with_detail(failures.join("\n"));
            }
            self.notices.raise(notice);
        }
    }

    /// Quick Open: the project's files, listed in the background.
    fn quick_open(&mut self, ctx: &Context) {
        let Some(pid) = self.book.active_id() else { return };
        let Some(root) = self.book.get(pid).map(|p| p.root.clone()) else { return };
        let exclusions = self.exclusions(pid);
        // Quick Open offers "open in the active editor" only when invoked from one.
        let target = self.focus.panel.and_then(|panel| {
            let doc = self.docs.get(&self.editor_key(pid, panel)?)?;
            Some((panel, doc.name()))
        });
        let index = self.indexes.entry(pid).or_insert_with(|| FileIndex::new(root));
        let repaint = ctx.clone();
        index.refresh(&exclusions, Duration::from_secs(2), move || repaint.request_repaint());
        let mut state =
            QuickOpen::new(index.files.clone(), !self.settings.quick_open_exclude_hidden(), target);
        state.building = index.building;
        self.dialog = Some(Dialog::QuickOpen(state));
    }

    /// Keep an open Quick Open's list current while the index builds.
    fn refresh_quick_open(&mut self, ctx: &Context) {
        let Some(Dialog::QuickOpen(state)) = self.dialog.as_mut() else { return };
        let Some(pid) = self.book.active_id() else { return };
        let exclusions = Exclusions {
            names: self.settings.explorer_exclude(),
            hidden: self.book.get(pid).map(|p| p.hidden_paths.clone()).unwrap_or_default(),
            keep_hidden: false,
        };
        if let Some(index) = self.indexes.get_mut(&pid) {
            let repaint = ctx.clone();
            index.refresh(&exclusions, Duration::from_secs(2), move || repaint.request_repaint());
            if !std::sync::Arc::ptr_eq(&state.files, &index.files) {
                state.files = index.files.clone();
            }
            state.building = index.building;
        }
    }

    fn pick_language(&mut self, panel: PanelId) {
        let Some(pid) = self.book.active_id() else { return };
        if let Some(doc) = self.editor_key(pid, panel).and_then(|k| self.docs.get(&k)) {
            self.dialog = Some(Dialog::Language {
                panel,
                filter: String::new(),
                current: doc.language().to_owned(),
                overridden: doc.language_override().is_some(),
            });
        }
    }

    /// Choose a document's language by hand, remembered against its file.
    fn set_language(&mut self, panel: PanelId, language: Option<String>) {
        let Some(pid) = self.book.active_id() else { return };
        let Some(key) = self.editor_key(pid, panel) else { return };
        let Some(doc) = self.docs.get_mut(&key) else { return };
        doc.set_language(language.clone());
        if let Some(path) = doc.path.clone() {
            let state_key = format!("{LANGUAGE_KEY_PREFIX}{}", path.display());
            if let Err(e) = self.store.set_state(&state_key, language.as_deref()) {
                self.store_failed("remember the language", &e);
            }
            match language {
                Some(language) => {
                    self.docs.languages.insert(key, language);
                }
                None => {
                    self.docs.languages.remove(&key);
                }
            }
        }
    }

    fn terminal_config(&self, pid: ProjectId, panel: PanelId) -> Option<TerminalPanelConfig> {
        match &self.workspaces.get(&pid)?.layout.panels.get(&panel)?.kind {
            PanelKind::Terminal(config) => Some(config.clone()),
            _ => None,
        }
    }

    fn editor_key(&self, pid: ProjectId, panel: PanelId) -> Option<DocKey> {
        match &self.workspaces.get(&pid)?.layout.panels.get(&panel)?.kind {
            PanelKind::Editor(config) => {
                Some(Documents::panel_key(&self.rules, panel, config.path.as_deref()))
            }
            _ => None,
        }
    }

    /// How many editor panels, across every loaded project, show `key`.
    fn panels_showing(&self, key: &DocKey) -> usize {
        self.workspaces
            .values()
            .flat_map(|ws| ws.layout.panels.values())
            .filter(|p| match &p.kind {
                PanelKind::Editor(config) => {
                    &Documents::panel_key(&self.rules, p.id, config.path.as_deref()) == key
                }
                _ => false,
            })
            .count()
    }

    fn save_panel(&mut self, pid: ProjectId, panel: PanelId) {
        let Some(key) = self.editor_key(pid, panel) else { return };
        let Some(doc) = self.docs.get_mut(&key) else { return };
        if doc.path.is_none() {
            let root = self.book.get(pid).map(|p| p.root.join("untitled.txt")).unwrap_or_default();
            self.dialog = Some(Dialog::SaveAs { panel, path: root.display().to_string(), error: None });
            return;
        }
        if let Err(e) = doc.save_to(None) {
            doc.error = Some(e);
        }
    }

    fn close_panel(&mut self, pid: ProjectId, panel: PanelId, force: bool) {
        let Some(kind) =
            self.workspaces.get(&pid).and_then(|ws| ws.layout.panels.get(&panel)).map(|p| p.kind.clone())
        else {
            return;
        };
        match kind {
            PanelKind::Editor(config) => {
                let key = Documents::panel_key(&self.rules, panel, config.path.as_deref());
                let last_view = self.panels_showing(&key) <= 1;
                let dirty = self.docs.get(&key).is_some_and(crate::editor::Document::is_dirty);
                if dirty && last_view && !force {
                    let name = self.docs.get(&key).map(crate::editor::Document::name).unwrap_or_default();
                    self.dialog = Some(Dialog::ClosePanel {
                        panel,
                        reason: format!("\"{name}\" has unsaved changes. Closing this panel discards them."),
                    });
                    return;
                }
                if last_view {
                    self.docs.remove(&key);
                } else {
                    self.docs.drop_view(&key, panel);
                }
            }
            // Destroying a panel ends its terminal (Principle III): no orphaned process or view.
            PanelKind::Terminal(_) => self.hub.kill(panel, self.link.client()),
            // Closing a preview never prompts.
            PanelKind::Preview(_) => {
                self.previews.remove(&panel);
            }
            PanelKind::Search(_) => {
                // Closing a Find in Files panel discards its results and stops its scan.
                if let Some(state) = self.searches.remove(&panel) {
                    state.scan.cancel();
                }
            }
            PanelKind::Untyped => {}
            // Closing a mirror closes it here only: its panel lives on in its project, and one moved
            // here goes back there.
            PanelKind::Mirror(m) => {
                self.docs.forget_view(panel);
                self.previews.remove(&panel);
                self.return_home(m.project, m.panel);
            }
        }
        if let Some(ws) = self.workspaces.get_mut(&pid) {
            ws.layout.remove_panel(panel);
            ws.rebuild_all();
            ws.mark_dirty();
        }
        if self.subs.contains(pid) {
            // A sub-workspace holds at least one tab: emptied, it goes.
            if self.workspaces.get(&pid).is_some_and(|ws| ws.layout.tabs.is_empty()) {
                self.remove_sub(pid);
            }
        } else {
            // A project's panel destroyed is gone everywhere, sub-workspaces included.
            self.drop_mirrors(pid, Some(panel));
        }
        self.pickers.remove(&panel);
        self.open_errors.remove(&panel);
        if self.focus.panel == Some(panel) {
            self.focus = Focus::default();
        }
    }

    fn close_tab(&mut self, pid: ProjectId, tab: TabId) {
        let panels = self
            .workspaces
            .get(&pid)
            .and_then(|ws| ws.layout.tab(tab))
            .map(|t| t.root.panels())
            .unwrap_or_default();
        let dirty: Vec<String> = panels
            .iter()
            .filter_map(|p| self.editor_key(pid, *p))
            .filter(|k| self.panels_showing(k) <= 1)
            .filter_map(|k| self.docs.get(&k).filter(|d| d.is_dirty()).map(crate::editor::Document::name))
            .collect();
        if !dirty.is_empty() {
            self.notices.raise(Notice::new(
                format!("close-tab:{tab}"),
                Severity::Warning,
                format!(
                    "This tab has unsaved changes in {}. Save or close those panels first.",
                    dirty.join(", ")
                ),
            ));
            return;
        }
        for panel in panels {
            self.close_panel(pid, panel, true);
        }
    }

    fn open_in_editor(&mut self, pid: ProjectId, path: PathBuf) -> Option<PanelId> {
        let key = Documents::key_for(&self.rules, &path);
        let anchor = self.active_panel();
        let ws = self.workspaces.get_mut(&pid)?;
        let existing = ws.layout.panels.values().find_map(|p| match &p.kind {
            PanelKind::Editor(EditorPanelConfig { path: Some(other) })
                if Documents::key_for(&self.rules, other) == key =>
            {
                Some(p.id)
            }
            _ => None,
        });
        let panel = match existing {
            Some(panel) => {
                ws.layout.focus_panel(panel);
                panel
            }
            None => ws.layout.add_panel(
                pid,
                anchor,
                Placement::Stack,
                PanelKind::Editor(EditorPanelConfig { path: Some(path) }),
            ),
        };
        ws.rebuild_all();
        ws.mark_dirty();
        Some(panel)
    }

    /// Show `path` in the editor panel `panel` (Quick Open's "open in the active editor").
    /// A document with unsaved edits and no other view is not displaced: a new editor opens.
    fn open_here(&mut self, pid: ProjectId, panel: PanelId, path: PathBuf) -> Option<PanelId> {
        if let Some(key) = self.editor_key(pid, panel)
            && self.docs.get(&key).is_some_and(crate::editor::Document::is_dirty)
            && self.panels_showing(&key) <= 1
        {
            return self.open_in_editor(pid, path);
        }
        if let Some(key) = self.editor_key(pid, panel) {
            if self.panels_showing(&key) <= 1 {
                self.docs.remove(&key);
            } else {
                self.docs.drop_view(&key, panel);
            }
        }
        let ws = self.workspaces.get_mut(&pid)?;
        ws.layout.set_kind(panel, PanelKind::Editor(EditorPanelConfig { path: Some(path) }));
        ws.layout.focus_panel(panel);
        ws.rebuild_all();
        ws.mark_dirty();
        Some(panel)
    }

    /// A file dropped on an empty panel becomes its editor; one already open elsewhere is focused
    /// instead, and a folder is refused.
    fn drop_file(&mut self, pid: ProjectId, panel: PanelId, path: PathBuf) {
        if self.refuse_project_file(pid, &path) {
            return;
        }
        if path.is_dir() {
            self.notices.raise(Notice::new(
                "drop",
                Severity::Warning,
                "A folder cannot be shown in a panel. Drop a file, or open a terminal in the folder.",
            ));
            return;
        }
        let key = Documents::key_for(&self.rules, &path);
        let open_elsewhere = self.workspaces.get(&pid).is_some_and(|ws| {
            ws.layout.panels.values().any(|p| {
                matches!(&p.kind, PanelKind::Editor(EditorPanelConfig { path: Some(other) })
                    if Documents::key_for(&self.rules, other) == key)
            })
        });
        if open_elsewhere {
            self.open_in_editor(pid, path);
            return;
        }
        if let Some(ws) = self.workspaces.get_mut(&pid) {
            ws.layout.set_kind(panel, PanelKind::Editor(EditorPanelConfig { path: Some(path) }));
            ws.layout.focus_panel(panel);
            ws.rebuild_all();
            ws.mark_dirty();
        }
    }

    /// Files dropped on the window from the system open in editors of the active project, one
    /// each; an open one is focused. Folders and files outside the project are refused, and the
    /// notice says why.
    fn open_dropped(&mut self, files: Vec<PathBuf>) {
        let Some(project) = self.active_project().cloned() else { return };
        let mut refused = Vec::new();
        for path in files {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if path.is_dir() {
                refused.push(format!("\"{name}\" is a folder"));
            } else if !self.rules.is_within(&project.root, &path) {
                refused.push(format!("\"{name}\" is outside {}", project.name));
            } else {
                self.open_in_editor(project.id, path);
            }
        }
        if !refused.is_empty() {
            self.notices.raise(Notice::new(
                "drop",
                Severity::Warning,
                format!("Not opened: {}. Only files inside the project open here.", refused.join("; ")),
            ));
        }
    }

    fn explorer_actions(&mut self, ctx: &Context, actions: Vec<explorer::Action>) {
        let Some(pid) = self.book.active_id() else { return };
        for action in actions {
            match action {
                explorer::Action::Open(path) => {
                    self.open_in_editor(pid, path);
                }
                explorer::Action::FindIn(path) => self.find_in_files(ctx, false, false, Some(path)),
                explorer::Action::OpenTerminal(dir) => {
                    let anchor = self.active_panel();
                    if let Some(ws) = self.workspaces.get_mut(&pid) {
                        let config = TerminalPanelConfig { cwd: Some(dir), ..TerminalPanelConfig::default() };
                        ws.layout.add_panel(pid, anchor, Placement::Stack, PanelKind::Terminal(config));
                        ws.rebuild_all();
                        ws.mark_dirty();
                    }
                }
                explorer::Action::Rename { from, to } => {
                    if self.rename_path(pid, &from, &to) {
                        self.record_file_op(pid, FileOp::Move { from, to });
                    }
                }
                explorer::Action::Transfer { from, into, copy } => self.transfer(pid, &from, &into, copy),
                explorer::Action::Undo => self.undo_file_op(pid, true),
                explorer::Action::Preview(path) => self.open_preview(pid, path),
                explorer::Action::Redo => self.undo_file_op(pid, false),
                explorer::Action::Create { path, folder } => {
                    let result = if folder {
                        std::fs::create_dir(&path)
                    } else {
                        std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ())
                    };
                    match result {
                        Ok(()) => {
                            if let Some(explorer) = self.explorers.get_mut(&pid) {
                                explorer.invalidate(path.parent().unwrap_or(&path));
                                explorer.selected = Some(path.clone());
                            }
                            if !folder {
                                self.open_in_editor(pid, path);
                            }
                        }
                        Err(e) => self.file_op_failed(&e, throng_core::failure::Operation::Create, &path),
                    }
                }
                explorer::Action::Delete(path) => self.dialog = Some(Dialog::Trash { path }),
                explorer::Action::CopyPath(text) => ctx.copy_text(text),
                explorer::Action::Reveal(path) => {
                    if let Err(e) = throng_platform::fs::reveal(&path) {
                        self.notices.raise(Notice::new(
                            "reveal",
                            Severity::Warning,
                            format!("Could not open the file manager: {e}"),
                        ));
                    }
                }
                explorer::Action::Hide(rel) => {
                    if let Some(project) = self.book.get(pid) {
                        let mut hidden = project.hidden_paths.clone();
                        hidden.push(rel);
                        let _ = self.book.set_hidden(pid, hidden, now_ms());
                        self.persist_project(pid);
                        if let Some(explorer) = self.explorers.get_mut(&pid) {
                            explorer.invalidate_all();
                        }
                    }
                }
                explorer::Action::Unhide(rel) => {
                    if let Some(project) = self.book.get(pid) {
                        let hidden = project.hidden_paths.iter().filter(|h| **h != rel).cloned().collect();
                        let _ = self.book.set_hidden(pid, hidden, now_ms());
                        self.persist_project(pid);
                        if let Some(explorer) = self.explorers.get_mut(&pid) {
                            explorer.invalidate_all();
                        }
                    }
                }
            }
        }
    }

    fn file_op_failed(&mut self, error: &std::io::Error, op: throng_core::failure::Operation, path: &Path) {
        self.notices.raise(Notice::new(
            "file-op",
            Severity::Error,
            throng_core::failure::describe(error, op, path),
        ));
    }

    /// Rename or move inside throng; `false` when it failed (and the user was told).
    fn rename_path(&mut self, pid: ProjectId, from: &Path, to: &Path) -> bool {
        if let Err(e) = throng_platform::fs::rename_no_clobber(from, to) {
            self.file_op_failed(&e, throng_core::failure::Operation::Rename, from);
            return false;
        }
        self.follow_move(pid, from, to);
        true
    }

    /// Something moved from `from` to `to`: open documents follow it, keeping their buffer, dirty
    /// state and history — a move is not a delete — and so does the tree.
    fn follow_move(&mut self, pid: ProjectId, from: &Path, to: &Path) {
        let moved: Vec<(DocKey, PathBuf)> = self
            .docs
            .iter()
            .filter_map(|(key, doc)| {
                let path = doc.path.as_ref()?;
                let rest = path.strip_prefix(from).ok()?;
                Some((
                    key.clone(),
                    if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) },
                ))
            })
            .collect();
        for (key, new_path) in moved {
            if let Some(doc) = self.docs.get_mut(&key) {
                doc.moved_to(new_path.clone());
            }
            let new_key = Documents::key_for(&self.rules, &new_path);
            self.docs.rekey(&key, new_key);
        }
        // Every editor and preview on the item follows it, whether or not its document is loaded
        // yet; a preview follows its file.
        let rebase = |p: &mut PathBuf| {
            let Ok(rest) = p.strip_prefix(from) else { return false };
            *p = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
            true
        };
        for ws in self.workspaces.values_mut() {
            let mut changed = false;
            for panel in ws.layout.panels.values_mut() {
                changed |= match &mut panel.kind {
                    PanelKind::Editor(EditorPanelConfig { path: Some(p) }) => rebase(p),
                    PanelKind::Preview(config) => {
                        let before = config.clone();
                        config.rebase(|p| {
                            let mut p = p.to_path_buf();
                            rebase(&mut p).then_some(p)
                        });
                        *config != before
                    }
                    _ => false,
                };
            }
            if changed {
                ws.dirty_at.get_or_insert_with(Instant::now);
            }
        }
        for state in self.previews.values_mut() {
            let mut path = state.path.clone();
            if rebase(&mut path) {
                state.path = path;
            }
        }
        if let Some(explorer) = self.explorers.get_mut(&pid) {
            explorer.follow_move(from, to);
            explorer.selected = Some(to.to_path_buf());
        }
    }

    /// Move to the trash; the delete can be undone where the trash can put things back.
    fn trash(&mut self, path: PathBuf) {
        match throng_platform::fs::trash_restorable(&path) {
            Ok(token) => {
                if let (Some(token), Some(pid)) = (token, self.book.active_id()) {
                    self.record_file_op(pid, FileOp::Trash { path: path.clone(), token });
                }
            }
            Err(reason) => {
                self.notices.raise(Notice::new(
                    "file-op",
                    Severity::Error,
                    format!("Could not move \"{}\" to the trash: {reason}", path.display()),
                ));
                return;
            }
        }
        self.disk_changed_at(&path);
    }

    /// Something appeared or went at `path` (a trash, a restore): editors under it check the
    /// disk, and the tree re-reads its folder.
    fn disk_changed_at(&mut self, path: &Path) {
        let max = self.settings.max_open_file_bytes();
        for (_, doc) in self.docs.iter_mut() {
            if doc.path.as_ref().is_some_and(|p| p.starts_with(path)) {
                doc.check_disk(max);
            }
        }
        if let Some(explorer) = self.book.active_id().and_then(|id| self.explorers.get_mut(&id)) {
            explorer.invalidate(path.parent().unwrap_or(path));
            explorer.invalidate(path);
        }
    }

    fn file_history(&mut self, pid: ProjectId) -> &mut FileHistory {
        let store = &self.store;
        self.file_histories.entry(pid).or_insert_with(|| {
            let stored = store.state(&format!("{FILE_HISTORY_KEY_PREFIX}{pid}")).ok().flatten();
            FileHistory::parse(stored.as_deref())
        })
    }

    fn save_file_history(&mut self, pid: ProjectId) {
        let json = self.file_history(pid).to_json();
        if let Err(e) = self.store.set_state(&format!("{FILE_HISTORY_KEY_PREFIX}{pid}"), Some(&json)) {
            self.store_failed("save the file history", &e);
        }
    }

    fn record_file_op(&mut self, pid: ProjectId, op: FileOp) {
        self.file_history(pid).record(op);
        self.save_file_history(pid);
    }

    /// Move or copy from the tree (a drag, or a paste): moves can be undone, copies cannot.
    fn transfer(&mut self, pid: ProjectId, from: &Path, into: &Path, copy: bool) {
        let Some(root) = self.book.get(pid).map(|p| p.root.clone()) else { return };
        let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
        let to = match file_ops::plan_transfer(&self.rules, &root, from, into, copy, &exists) {
            Ok(Some(to)) => to,
            Ok(None) => return,
            Err(reason) => {
                let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let verb = if copy { "copy" } else { "move" };
                self.notices.raise(Notice::new(
                    "file-op",
                    Severity::Error,
                    format!("Can't {verb} \"{name}\": {reason}"),
                ));
                return;
            }
        };
        if !copy {
            if self.rename_path(pid, from, &to) {
                self.record_file_op(pid, FileOp::Move { from: from.to_path_buf(), to });
            }
            return;
        }
        match throng_platform::fs::copy_recursive(from, &to) {
            Ok(()) => {
                if let Some(explorer) = self.explorers.get_mut(&pid) {
                    explorer.invalidate(into);
                    explorer.expanded.insert(into.to_path_buf());
                    explorer.selected = Some(to);
                }
            }
            Err(e) => self.file_op_failed(&e, throng_core::failure::Operation::Write, &to),
        }
    }

    /// Undo (or redo) the project's last file operation, after checking the disk still matches it;
    /// a refusal is an error notice that stays until dismissed.
    fn undo_file_op(&mut self, pid: ProjectId, undo: bool) {
        let history = self.file_history(pid);
        let outcome = if undo {
            file_ops::undo(history, &file_ops::Disk)
        } else {
            file_ops::redo(history, &file_ops::Disk)
        };
        match outcome {
            Ok(None) => {}
            Ok(Some(changed)) => {
                self.save_file_history(pid);
                match changed {
                    Changed::Moved { from, to } => self.follow_move(pid, &from, &to),
                    Changed::Restored(path) | Changed::Trashed(path) => self.disk_changed_at(&path),
                }
            }
            Err(refusal) => {
                let name =
                    refusal.item.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.notices.raise(
                    Notice::new(
                        format!("file-undo:{}", refusal.item.display()),
                        Severity::Error,
                        format!("{}: \"{name}\". {}", refusal.title, refusal.reason),
                    )
                    .with_detail(refusal.item.display().to_string()),
                );
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Closing (Principle III)

    /// Unsaved editors are not asked about: their recovery files bring them back.
    /// Only running terminals are.
    fn begin_close(&mut self) {
        self.check_running_terminals();
    }

    fn check_running_terminals(&mut self) {
        let busy: Vec<String> =
            match self.link.client().map(|c| c.request(Request::List, Duration::from_secs(2))) {
                Some(Ok(Reply::Terminals(list))) => {
                    list.into_iter().filter(|t| t.busy && t.exited.is_none()).map(|t| t.label).collect()
                }
                _ => Vec::new(),
            };
        if busy.is_empty() {
            self.finish_close(Answer::LeaveRunning);
        } else {
            self.dialog = Some(Dialog::RunningTerminals { labels: busy });
        }
    }

    fn finish_close(&mut self, choice: Answer) {
        if !matches!(choice, Answer::LeaveRunning | Answer::TerminateAll) {
            return;
        }
        // From here on nothing starts a terminal: the ones quitting closes stay closed.
        self.hub.closing = true;
        let timeout = Duration::from_secs(3);
        let list = match self.link.client().map(|c| c.request(Request::List, timeout)) {
            Some(Ok(Reply::Terminals(list))) => list,
            _ => Vec::new(),
        };
        let live: Vec<_> = list.into_iter().filter(|t| t.exited.is_none()).collect();
        // Command memory, before anything ends. Busy terminals left running have not ended: their
        // memory waits for an end. Idle ones close with nothing running.
        let table = throng_platform::process::ProcessTable::snapshot();
        for info in &live {
            let running = match choice {
                Answer::LeaveRunning if info.busy => continue,
                Answer::LeaveRunning => Some(None),
                _ => info.pid.map(|pid| table.running_command(pid)),
            };
            self.capture_before_ending(PanelId::from(info.terminal), running);
        }
        if let Some(client) = self.link.client() {
            let ids: Vec<_> = live.iter().map(|t| t.terminal).collect();
            match choice {
                // Idle shells close now and are re-created on reopen; busy ones keep running.
                Answer::LeaveRunning => {
                    let _ = client.request(Request::CloseIdle { terminals: ids }, timeout);
                }
                _ => {
                    for terminal in ids {
                        let _ = client.request(Request::Kill { terminal }, timeout);
                    }
                }
            }
        }
        self.tick_recovery(None, true);
        self.flush_layouts(true);
        self.closing = Closing::Confirmed;
    }

    fn answer(&mut self, ctx: &Context, answer: Answer) {
        let Some(dialog) = self.dialog.take() else { return };
        match (dialog, answer) {
            (dialog, Answer::None) => self.dialog = Some(dialog),
            (Dialog::GotoLine { panel, .. } | Dialog::Language { panel, .. }, Answer::Cancel) => {
                // Both return to the editor they came from.
                self.focus_next = Some((crate::editor::editor_id(panel), 1));
            }
            (_, Answer::Cancel) => {}
            (Dialog::GotoLine { panel, .. }, Answer::GotoLine(line)) => {
                if let Some(pid) = self.book.active_id()
                    && let Some(doc) = self.editor_key(pid, panel).and_then(|k| self.docs.get_mut(&k))
                {
                    let pos = throng_editor::lines::line_start(doc.buf.rope(), line.saturating_sub(1));
                    doc.views.entry(panel).or_default().set_caret(pos);
                }
                self.focus_next = Some((crate::editor::editor_id(panel), 1));
            }
            (Dialog::QuickOpen(_), Answer::QuickOpen(Choice { path, into })) => {
                if let Some(pid) = self.book.active_id() {
                    let panel = match into {
                        Some(panel) => self.open_here(pid, panel, path),
                        None => self.open_in_editor(pid, path),
                    };
                    if let Some(panel) = panel {
                        self.focus_next = Some((crate::editor::editor_id(panel), 1));
                    }
                }
            }
            (Dialog::ConfirmReplace { panel, commit, .. }, Answer::ConfirmReplace) => {
                if let Some(pid) = self.book.active_id() {
                    self.commit(pid, panel, commit, true);
                }
            }
            (Dialog::Language { panel, .. }, Answer::Language(language)) => {
                self.set_language(panel, language);
                self.focus_next = Some((crate::editor::editor_id(panel), 1));
            }
            (Dialog::Project(mut form), Answer::BrowseRoot) => {
                let current = PathBuf::from(form.root.trim());
                let start = current.is_dir().then_some(current.as_path());
                if let Some(folder) = (self.pick_folder)(start) {
                    form.choose_root(&folder);
                }
                self.dialog = Some(Dialog::Project(form));
            }
            (Dialog::Project(mut form), Answer::SaveProject) => {
                if self.save_project(&mut form).is_err() {
                    self.dialog = Some(Dialog::Project(form));
                } else {
                    self.apply_theme(ctx);
                }
            }
            (_, Answer::DeleteProject(id)) => {
                self.delete_project(id);
                self.apply_theme(ctx);
            }
            (Dialog::Trash { path }, Answer::Trash) => self.trash(path),
            (Dialog::RenamePanel { panel, project, name }, Answer::RenamePanel) => {
                if let Some(ws) = self.workspaces.get_mut(&project) {
                    ws.layout.rename_panel(panel, &name);
                    ws.mark_dirty();
                }
            }
            (Dialog::RenameSubWorkspace { id, name }, Answer::RenameSubWorkspace) => {
                if self.subs.rename(id, &name) {
                    self.save_subs();
                }
            }
            (_, Answer::DestroySubWorkspace(id)) => self.remove_sub(id),
            (Dialog::SaveAs { panel, path, .. }, Answer::SaveAs) => self.save_as(panel, path),
            (_, Answer::ClosePanelAnyway(panel)) => {
                if let Some(pid) = self.workspace_of(panel) {
                    self.close_panel(pid, panel, true);
                }
            }
            (Dialog::RunningTerminals { .. }, choice @ (Answer::LeaveRunning | Answer::TerminateAll)) => {
                self.finish_close(choice);
            }
            (_, _) => {}
        }
        if self.closing == Closing::Confirmed {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn save_as(&mut self, panel: PanelId, path: String) {
        let Some(pid) = self.workspace_of(panel) else { return };
        let target = PathBuf::from(path.trim());
        let retry = |error: String| Dialog::SaveAs { panel, path: path.clone(), error: Some(error) };
        if !target.is_absolute() {
            self.dialog = Some(retry("Enter a full path.".into()));
            return;
        }
        // A sub-workspace's own editor writes only outside every project.
        if self.subs.contains(pid)
            && let Some(project) = self.book.owner_of(&self.rules, &target)
        {
            let name = project.name.clone();
            self.dialog = Some(retry(format!(
                "That is inside \"{name}\". This editor belongs to a sub-workspace, which saves only outside projects."
            )));
            return;
        }
        if target.parent().is_some_and(|p| !p.is_dir()) {
            self.dialog = Some(retry("That folder does not exist.".into()));
            return;
        }
        let Some(old_key) = self.editor_key(pid, panel) else { return };
        let new_key = Documents::key_for(&self.rules, &target);
        if target.exists() && new_key != old_key {
            self.dialog = Some(retry("A file already exists there.".into()));
            return;
        }
        let Some(doc) = self.docs.get_mut(&old_key) else { return };
        if let Err(e) = doc.save_to(Some(&target)) {
            self.dialog = Some(retry(e));
            return;
        }
        let new_key = Documents::key_for(&self.rules, &target);
        self.docs.rekey(&old_key, new_key);
        if let Some(ws) = self.workspaces.get_mut(&pid) {
            ws.layout.set_kind(panel, PanelKind::Editor(EditorPanelConfig { path: Some(target.clone()) }));
            ws.mark_dirty();
        }
        if let Some(explorer) = self.explorers.get_mut(&pid) {
            explorer.invalidate(target.parent().unwrap_or(&target));
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Drawing

    fn menu_bar(&mut self, ui: &mut Ui, actions: &mut Vec<PanelAction>) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New Project…").clicked() {
                    self.dialog =
                        Some(Dialog::Project(ProjectForm::new(self.book.projects().len(), String::new())));
                    ui.close();
                }
                ui.add_enabled_ui(self.book.active_id().is_some(), |ui| {
                    if ui.button("New Terminal Tab").clicked() {
                        actions.push(PanelAction::NewTab);
                        ui.close();
                    }
                    if ui.button("New Untitled File").clicked() {
                        if let Some(panel) = self.active_panel() {
                            actions.push(PanelAction::Split(
                                panel,
                                Placement::Stack,
                                PanelKind::Editor(EditorPanelConfig { path: None }),
                            ));
                        }
                        ui.close();
                    }
                });
                ui.separator();
                if ui.button("Preferences…").clicked() {
                    self.prefs.open = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                    ui.close();
                }
            });
            ui.menu_button("Go", |ui| {
                let has_project = self.book.active_id().is_some();
                ui.add_enabled_ui(has_project, |ui| {
                    if ui
                        .add(
                            egui::Button::new("Quick Open…")
                                .shortcut_text(crate::keymap::label(ui.ctx(), "navigate.quickOpen")),
                        )
                        .clicked()
                    {
                        self.quick_open(ui.ctx());
                        ui.close();
                    }
                    if ui
                        .add(
                            egui::Button::new("Find in Files…")
                                .shortcut_text(crate::keymap::label(ui.ctx(), "search.findInFiles")),
                        )
                        .clicked()
                    {
                        self.find_in_files(ui.ctx(), false, true, None);
                        ui.close();
                    }
                    if ui
                        .add(
                            egui::Button::new("Replace in Files…")
                                .shortcut_text(crate::keymap::label(ui.ctx(), "search.replaceInFiles")),
                        )
                        .clicked()
                    {
                        self.find_in_files(ui.ctx(), true, true, None);
                        ui.close();
                    }
                });
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.show_explorer, "File Explorer");
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Open Logs Folder").clicked() {
                    let _ = throng_platform::fs::open_with_default_app(&self.dirs.logs.display().to_string());
                    ui.close();
                }
                if ui.button("About throng").clicked() {
                    self.dialog = Some(Dialog::About);
                    ui.close();
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui, ctx: &Context) {
        ui.horizontal(|ui| {
            ui.strong("PROJECTS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(crate::icons::atom(ui.ctx(), "add"))
                    .on_hover_text(hint(ui.ctx(), "New project", "project.new"))
                    .clicked()
                {
                    self.dialog =
                        Some(Dialog::Project(ProjectForm::new(self.book.projects().len(), String::new())));
                }
            });
        });
        ui.separator();
        let active = self.book.active_id();
        let projects: Vec<Project> = self.book.projects().to_vec();
        let mut switch_to = None;
        egui::ScrollArea::vertical().id_salt("projects").show(ui, |ui| {
            for (index, project) in projects.iter().enumerate() {
                let colour = crate::theme::to_color32(project.colour);
                let selected = Some(project.id) == active;
                let response = ui
                    .horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 5.0, colour);
                        let text = if selected {
                            RichText::new(&project.name).strong()
                        } else {
                            RichText::new(&project.name)
                        };
                        ui.add(egui::Button::selectable(selected, text).frame_when_inactive(false))
                    })
                    .inner;
                let response = response.on_hover_text(project.root.display().to_string());
                if response.clicked() && !selected {
                    switch_to = Some(project.id);
                }
                response.context_menu(|ui| {
                    if ui.button("Edit…").clicked() {
                        self.dialog = Some(Dialog::Project(ProjectForm {
                            editing: Some(project.id),
                            name: project.name.clone(),
                            colour: [project.colour.r, project.colour.g, project.colour.b],
                            root: project.root.display().to_string(),
                            error: None,
                        }));
                        ui.close();
                    }
                    if index > 0 && ui.button("Move Up").clicked() {
                        let _ = self.book.reorder(project.id, index - 1);
                        let order: Vec<ProjectId> = self.book.projects().iter().map(|p| p.id).collect();
                        let _ = self.store.set_positions(&order);
                        ui.close();
                    }
                    if index + 1 < projects.len() && ui.button("Move Down").clicked() {
                        let _ = self.book.reorder(project.id, index + 1);
                        let order: Vec<ProjectId> = self.book.projects().iter().map(|p| p.id).collect();
                        let _ = self.store.set_positions(&order);
                        ui.close();
                    }
                    if ui.button("Reveal Root Folder").clicked() {
                        let _ = throng_platform::fs::reveal(&project.root);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Delete Project…").clicked() {
                        self.load_workspace(project.id);
                        let terminals = self
                            .workspaces
                            .get(&project.id)
                            .map_or(0, |ws| ws.layout.terminal_panels().count());
                        self.dialog = Some(Dialog::DeleteProject {
                            id: project.id,
                            name: project.name.clone(),
                            terminals,
                        });
                        ui.close();
                    }
                });
            }
        });
        if let Some(id) = switch_to {
            self.switch_project(id);
            self.apply_theme(ctx);
        }
        self.sub_workspace_list(ui);
    }

    /// The sidebar's sub-workspaces: open one's window, rename or destroy it.
    fn sub_workspace_list(&mut self, ui: &mut Ui) {
        if self.subs.list.is_empty() {
            return;
        }
        ui.add_space(10.0);
        ui.strong("SUB-WORKSPACES");
        ui.separator();
        let subs = self.subs.list.clone();
        for sub in subs {
            let response = ui
                .add(egui::Button::selectable(sub.open, RichText::new(&sub.name)).frame_when_inactive(false));
            let response =
                response.on_hover_text(if sub.open { "Its window is open" } else { "Open its window" });
            if response.clicked() {
                if let Some(s) = self.subs.get_mut(sub.id) {
                    s.open = true;
                }
                ui.ctx().send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of(("throng-sub", sub.id)),
                    ViewportCommand::Focus,
                );
                self.save_subs();
            }
            response.context_menu(|ui| {
                if ui.button(if sub.open { "Close Window" } else { "Open Window" }).clicked() {
                    if let Some(s) = self.subs.get_mut(sub.id) {
                        s.open = !sub.open;
                    }
                    self.save_subs();
                    ui.close();
                }
                if ui.button("Rename…").clicked() {
                    self.dialog = Some(Dialog::RenameSubWorkspace { id: sub.id, name: sub.name.clone() });
                    ui.close();
                }
                ui.separator();
                if ui.button("Destroy Sub-workspace…").clicked() {
                    let (terminals, mirrors) = self.workspaces.get(&sub.id).map_or((0, 0), |ws| {
                        let panels = ws.layout.panels.values();
                        let terminals =
                            panels.clone().filter(|p| matches!(p.kind, PanelKind::Terminal(_))).count();
                        let mirrors = panels.filter(|p| matches!(p.kind, PanelKind::Mirror(_))).count();
                        (terminals, mirrors)
                    });
                    self.dialog = Some(Dialog::DestroySubWorkspace {
                        id: sub.id,
                        name: sub.name.clone(),
                        terminals,
                        mirrors,
                    });
                    ui.close();
                }
            });
        }
    }

    fn explorer_panel(&mut self, ui: &mut Ui, ctx: &Context) {
        let Some(project) = self.active_project().cloned() else { return };
        ui.horizontal(|ui| {
            ui.strong(project.name.to_uppercase());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let search = crate::icons::button(ui, crate::icons::Icon::Search, "Find in Files")
                    .on_hover_text(hint(ui.ctx(), "Find in Files", "search.findInFiles"));
                if search.clicked() {
                    // The toolbar never seeds from a selection.
                    self.find_in_files(ctx, false, false, None);
                }
                if let Some(explorer) = self.explorers.get_mut(&project.id) {
                    if ui
                        .small_button(crate::icons::atom(ui.ctx(), "refresh"))
                        .on_hover_text("Refresh")
                        .clicked()
                    {
                        explorer.invalidate_all();
                    }
                    if ui
                        .small_button(crate::icons::atom(ui.ctx(), "newFolder"))
                        .on_hover_text("New folder")
                        .clicked()
                    {
                        explorer.begin_create(project.root.clone(), true);
                    }
                    if ui
                        .small_button(crate::icons::atom(ui.ctx(), "newFile"))
                        .on_hover_text("New file")
                        .clicked()
                    {
                        explorer.begin_create(project.root.clone(), false);
                    }
                }
            });
        });
        ui.separator();
        let exclude = self.settings.explorer_exclude();
        let accent = crate::theme::to_color32(project.colour);
        let actions = match self.explorers.get_mut(&project.id) {
            Some(explorer) => {
                let history = self.file_histories.get(&project.id);
                let history = explorer::History {
                    can_undo: history.is_some_and(FileHistory::can_undo),
                    can_redo: history.is_some_and(FileHistory::can_redo),
                };
                explorer.ui(ui, &self.rules, &exclude, &project.hidden_paths, accent, history)
            }
            None => Vec::new(),
        };
        if !project.hidden_paths.is_empty() {
            ui.separator();
            let mut unhide = None;
            egui::CollapsingHeader::new(format!("Hidden in this project ({})", project.hidden_paths.len()))
                .id_salt("hidden-paths")
                .show(ui, |ui| {
                    for rel in &project.hidden_paths {
                        ui.horizontal(|ui| {
                            ui.label(rel);
                            if ui.small_button("Show").clicked() {
                                unhide = Some(rel.clone());
                            }
                        });
                    }
                });
            if let Some(rel) = unhide {
                self.explorer_actions(ctx, vec![explorer::Action::Unhide(rel)]);
            }
        }
        self.explorer_actions(ctx, actions);
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(project) = self.active_project() {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, crate::theme::to_color32(project.colour));
                ui.label(&project.name);
                ui.weak(project.root.display().to_string());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.hub.elevated {
                    ui.label(crate::workspace_ui::admin_mark())
                        .on_hover_text("throng is running as administrator");
                    ui.separator();
                }
                let (text, color) = match &self.link.state {
                    LinkState::Connected => ("terminal host", Color32::from_rgb(0x57, 0xab, 0x5a)),
                    LinkState::Connecting => ("starting terminal host", ui.visuals().weak_text_color()),
                    LinkState::Lost { .. } => ("reconnecting", ui.visuals().warn_fg_color),
                    LinkState::Blocked { .. } => ("terminal host blocked", ui.visuals().error_fg_color),
                };
                ui.colored_label(color, text);
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                ui.separator();
                let running = self.hub.views.len();
                ui.weak(format!("{running} terminal{}", if running == 1 { "" } else { "s" }));
            });
        });
    }

    fn notices_ui(&mut self, ui: &mut Ui) {
        let mut clicked: Vec<(String, String)> = Vec::new();
        for notice in self.notices.all() {
            let color = match notice.severity {
                Severity::Error => ui.visuals().error_fg_color,
                Severity::Warning => ui.visuals().warn_fg_color,
                Severity::Info => ui.visuals().hyperlink_color,
            };
            egui::Frame::new()
                .fill(color.gamma_multiply(0.15))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(color, &notice.message);
                        if notice.repeats > 0 {
                            ui.weak(format!("(×{})", notice.repeats + 1));
                        }
                        for action in &notice.actions {
                            if ui.button(&action.label).clicked() {
                                clicked.push((notice.key.clone(), action.id.clone()));
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(crate::icons::atom(ui.ctx(), "dismiss"))
                                .on_hover_text("Dismiss")
                                .clicked()
                            {
                                clicked.push((notice.key.clone(), "dismiss".into()));
                            }
                        });
                    });
                    if let Some(detail) = &notice.detail {
                        ui.weak(detail);
                    }
                });
        }
        for (key, action) in clicked {
            match action.as_str() {
                "replace-daemon" => self.link.replace_blocking_daemon(),
                "open-settings" => self.prefs.open = true,
                _ => {}
            }
            self.notices.dismiss(&key);
        }
    }

    fn welcome(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.heading(RichText::new("throng").size(34.0));
            ui.label("Project-first terminals, editors and files — each project in its own clean context.");
            ui.add_space(16.0);
            if ui.button(RichText::new("Create a project").size(16.0)).clicked() {
                self.dialog =
                    Some(Dialog::Project(ProjectForm::new(self.book.projects().len(), String::new())));
            }
            if ui.button("Open a Folder\u{2026}").clicked()
                && let Some(folder) = (self.pick_folder)(None)
            {
                self.open_folder(&folder);
                self.apply_theme(ui.ctx());
            }
            // Started from a shell in a project, offer that folder; started from a desktop launcher
            // the working directory is `/` or similar, which names no project.
            if let Ok(cwd) = std::env::current_dir()
                && cwd.file_name().is_some()
                && ui.button(format!("Open {}", cwd.display())).clicked()
            {
                self.open_folder(&cwd);
                self.apply_theme(ui.ctx());
            }
        });
    }

    fn central(&mut self, ui: &mut Ui, actions: &mut Vec<PanelAction>) {
        self.notices_ui(ui);
        let Some(pid) = self.book.active_id() else {
            self.welcome(ui);
            return;
        };
        self.load_workspace(pid);
        let Some(project) = self.book.get(pid).cloned() else { return };
        let subs = self.sub_targets();
        let no_mirrors = HashMap::new();
        let Some(ws) = self.workspaces.get_mut(&pid) else { return };
        self.focus = Focus::default();
        let mut ctx = PanelCtx {
            project: &project,
            hub: &mut self.hub,
            client: self.link.client(),
            docs: &mut self.docs,
            rules: &self.rules,
            settings: &self.settings,
            look: &self.look,
            actions,
            focus: &mut self.focus,
            pickers: &mut self.pickers,
            open_errors: &mut self.open_errors,
            searches: &mut self.searches,
            previews: &mut self.previews,
            mirrors: &no_mirrors,
            subs: &subs,
            drawn: &mut self.drawn,
            sub_workspace: false,
        };
        workspace_ui::show(ui, ws, &mut ctx, &mut self.renaming_tab);
    }

    fn screenshot(&mut self, ctx: &Context) {
        let Some((path, at, requested)) = &mut self.screenshot else { return };
        if !*requested && Instant::now() >= *at {
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
            *requested = true;
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, pixels).map(|img| img.save(&*path)) {
                Some(Ok(())) => tracing::info!(path = %path.display(), "screenshot saved"),
                other => tracing::error!(?other, "screenshot failed"),
            }
            // Quit the way a user's quit does (idle shells close, busy ones keep running), so a
            // screenshot run leaves no idle shell holding the daemon open.
            self.finish_close(Answer::LeaveRunning);
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else if !*requested || Instant::now() < *at + Duration::from_secs(10) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn settings_malformed(reason: &str) -> Notice {
    Notice::new(
        "settings:malformed",
        Severity::Warning,
        format!("settings.json could not be read ({reason}). throng is using defaults until it is fixed; the file was not changed."),
    )
    .with_action("open-settings", "Preferences")
}

impl eframe::App for ThrongApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.process_link(ctx);
        self.hub.remember_directory = self.settings.remember_directory();
        if let Some(again) = self.hub.poll_directories(self.link.client()) {
            ctx.request_repaint_after(again);
        }
        self.process_watch(ctx);
        self.auto_save(ctx);
        self.tick_recovery(Some(ctx), false);
        self.flush_layouts(false);
        if ctx.input(|i| i.viewport().close_requested()) && self.closing != Closing::Confirmed {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if self.dialog.is_none() {
                self.begin_close();
                if self.closing == Closing::Confirmed {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        if self.workspaces.values().any(|ws| ws.dirty_at.is_some()) {
            ctx.request_repaint_after(SAVE_DEBOUNCE);
        }
        let _ = self.started;
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.dialog.is_none() {
            match self.focus_next.take() {
                Some((id, 0)) => ctx.memory_mut(|m| m.request_focus(id)),
                Some((id, wait)) => {
                    self.focus_next = Some((id, wait - 1));
                    ctx.request_repaint();
                }
                None => {}
            }
        }
        self.apply_theme(&ctx);
        self.apply_icons(&ctx);
        // A chord being captured for a key binding is nobody else's.
        let mut actions = if self.prefs.capturing() {
            self.capture_chord(&ctx);
            Vec::new()
        } else {
            self.shortcuts(&ctx)
        };

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &mut actions));
        egui::Panel::bottom("status").exact_size(24.0).show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("sidebar").resizable(true).default_size(190.0).size_range(120.0..=360.0).show(
            ui,
            |ui| {
                self.sidebar(ui, &ctx);
            },
        );
        if self.show_explorer && self.book.active_id().is_some() {
            egui::Panel::left("explorer").resizable(true).default_size(260.0).size_range(150.0..=600.0).show(
                ui,
                |ui| {
                    self.explorer_panel(ui, &ctx);
                },
            );
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(4)))
            .show(ui, |ui| {
                self.central(ui, &mut actions);
            });

        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.open_dropped(dropped);
        }
        self.refresh_quick_open(&ctx);
        if let Some(dialog) = self.dialog.as_mut() {
            let answer = dialogs::show(&ctx, dialog);
            self.answer(&ctx, answer);
        }
        if self.prefs.open {
            let pack_names: Vec<String> = self.icon_packs.iter().map(|p| p.name.clone()).collect();
            let path = self.dirs.settings_file().display().to_string();
            let keybindings_path = self.dirs.keybindings_file();
            let before = std::sync::Arc::clone(&self.keymap);
            let mut p = crate::prefs::PrefsCtx {
                settings: &mut self.settings,
                themes: &mut self.themes,
                shells: &self.hub.shells,
                icon_packs: &pack_names,
                path: &path,
                system_dark: ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark),
                keymap: &mut self.keymap,
                keybindings_path: &keybindings_path,
            };
            if self.prefs.show(&ctx, &mut p) {
                self.write_settings();
            }
            if !std::sync::Arc::ptr_eq(&before, &self.keymap) {
                crate::keymap::install(&ctx, std::sync::Arc::clone(&self.keymap));
            }
            if self.prefs.pending() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            self.apply_theme(&ctx);
        }
        self.apply_actions(&ctx, actions);
        // Sub-workspace windows draw after the main one, so a terminal on screen in both is sized
        // by its own project's panel.
        self.window_focus
            .push((egui::ViewportId::ROOT, ctx.input(|i| i.viewport().focused.unwrap_or(false))));
        self.sub_windows(&ctx);
        self.raise_together(&ctx);
        self.drawn.clear();
        self.screenshot(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.tick_recovery(None, true);
        self.flush_layouts(true);
    }
}
