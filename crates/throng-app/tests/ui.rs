//! The real app, driven through its accessibility tree, against a real daemon process. Tests
//! that type POSIX shell commands into a terminal run on Linux and macOS; the rest run everywhere.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use throng_app::term::Status;
use throng_app::{Services, ThrongApp};
use throng_core::ids::{PanelId, ProjectId};
use throng_core::paths::PathRules;
use throng_core::project::{ProjectBook, ProjectInput};
#[cfg(unix)]
use throng_core::terminal::ExitStatus;
use throng_core::terminal::TerminalPanelConfig;
use throng_core::workspace::{EditorPanelConfig, Layout, PanelKind, Placement, PreviewPanelConfig};
use throng_daemon::{Client, Endpoint};
use throng_persistence::{ACTIVE_PROJECT_KEY, Store};
use throng_platform::dirs::AppDirs;
use throng_protocol::{Reply, Request};

const WAIT: Duration = Duration::from_secs(20);

/// A private throng home; its daemon is shut down when the test ends.
struct Env {
    dir: tempfile::TempDir,
    dirs: AppDirs,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::under(&dir.path().join("home"));
        Self { dir, dirs }
    }

    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    fn app(&self, open: Option<PathBuf>) -> Harness<'static, ThrongApp> {
        let services = Services {
            dirs: self.dirs.clone(),
            exe: PathBuf::from(env!("CARGO_BIN_EXE_throng")),
            open,
            screenshot: None,
        };
        // Frames a display's length apart (kittest's default is a quarter second), so two clicks in
        // successive frames are a double-click, as they are for a person.
        Harness::builder()
            .with_size([1280.0, 800.0])
            .with_step_dt(1.0 / 60.0)
            .build_eframe(|cc| ThrongApp::new(&cc.egui_ctx, services).expect("app starts"))
    }

    /// Seed a project with a layout built by `build`.
    fn seed(&self, root: &Path, build: impl FnOnce(ProjectId, &mut Layout)) -> ProjectId {
        self.dirs.ensure().unwrap();
        let store = Store::open(&self.dirs.database()).unwrap();
        let mut book = ProjectBook::default();
        let project = book
            .create(
                &PathRules::LINUX,
                &ProjectInput { name: "Seeded".into(), colour: "#6aa3ff".into(), root: root.to_path_buf() },
                0,
            )
            .unwrap()
            .clone();
        store.upsert_project(&project, 0).unwrap();
        store.set_state(ACTIVE_PROJECT_KEY, Some(&project.id.to_string())).unwrap();
        let mut layout = Layout::new_default(project.id);
        build(project.id, &mut layout);
        store.save_layout(project.id, &layout, 0).unwrap();
        project.id
    }

    fn client(&self) -> Client {
        Client::connect(&Endpoint::for_dirs(&self.dirs), || {}).expect("daemon is running")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Ok(client) = Client::connect(&Endpoint::for_dirs(&self.dirs), || {}) {
            let _ = client.request(Request::Shutdown { kill_all: true }, Duration::from_secs(5));
        }
    }
}

fn terminal(config: TerminalPanelConfig) -> PanelKind {
    PanelKind::Terminal(config)
}

#[cfg(unix)]
fn startup(command: &str) -> TerminalPanelConfig {
    TerminalPanelConfig { startup_command: Some(command.into()), ..TerminalPanelConfig::default() }
}

/// Step frames (in real time) until `done` holds.
fn wait(harness: &mut Harness<'static, ThrongApp>, what: &str, done: impl Fn(&ThrongApp) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        harness.step();
        if done(harness.state()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn steps(harness: &mut Harness<'static, ThrongApp>, n: usize) {
    for _ in 0..n {
        harness.step();
    }
}

fn first_panel(app: &ThrongApp) -> PanelId {
    app.active_layout().unwrap().tabs[0].root.panels()[0]
}

fn text_of(app: &ThrongApp, panel: PanelId) -> String {
    app.terminal_text(panel).unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn first_run_creates_a_project_and_its_terminal_takes_input() {
    let env = Env::new();
    let root = env.folder("demo");
    let mut harness = env.app(None);
    steps(&mut harness, 2);
    harness.get_by_label("Create a project").click();
    steps(&mut harness, 3);
    harness.get_by_label("Root folder").focus();
    steps(&mut harness, 1);
    harness.get_by_label("Root folder").type_text(&root.display().to_string());
    harness.get_by_label("Name").focus();
    steps(&mut harness, 1);
    harness.get_by_label("Name").type_text("Demo");
    steps(&mut harness, 1);
    harness.get_by_label("Create Project").click();
    steps(&mut harness, 3);

    let projects = harness.state().projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "Demo");
    let panel = first_panel(harness.state());
    wait(&mut harness, "the shell's prompt", |app| {
        app.terminal_status(panel) == Some(Status::Running) && !text_of(app, panel).trim().is_empty()
    });

    let label = "Terminal: Demo › Tab 1 › Panel 1";
    harness.get_by_label(label).click();
    steps(&mut harness, 2);
    harness.get_by_label(label).type_text("echo hello-$((6*7))");
    harness.key_press(Key::Enter);
    wait(&mut harness, "the command's output", |app| text_of(app, panel).contains("hello-42"));
}

#[cfg(unix)]
#[test]
fn a_busy_terminal_survives_closing_throng_and_reattaches_with_its_output() {
    let env = Env::new();
    let root = env.folder("svc");
    let mut idle = None;
    env.seed(&root, |project, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(startup("echo marker-$((40+2)); sleep 120")));
        idle = Some(layout.add_panel(
            project,
            Some(first),
            Placement::Right,
            terminal(TerminalPanelConfig::default()),
        ));
    });
    let idle = idle.unwrap();

    let mut first = env.app(None);
    let busy = first_panel(first.state());
    // The idle shell counts as idle once its prompt is up: while its startup files still run a
    // command, it is busy, and quitting then would rightly keep it.
    wait(&mut first, "both terminals to run", |app| {
        text_of(app, busy).contains("marker-42")
            && app.terminal_status(idle) == Some(Status::Running)
            && !text_of(app, idle).trim().is_empty()
    });
    wait(&mut first, "the sleep to hold the foreground", |_| {
        matches!(env.client().request(Request::List, WAIT), Ok(Reply::Terminals(list))
            if list.iter().any(|t| t.terminal == busy.into() && t.busy))
    });

    first.state_mut().request_close();
    steps(&mut first, 3);
    // Exactly three choices.
    first.get_by_label("Leave Running");
    first.get_by_label("Terminate All");
    first.get_by_label("Cancel");
    first.get_by_label("Leave Running").click();
    steps(&mut first, 3);
    assert!(first.state().close_confirmed());
    drop(first);

    // The idle shell was told to close; its exit is reaped a moment later, so wait for it.
    let deadline = Instant::now() + WAIT;
    let live = loop {
        let Ok(Reply::Terminals(list)) = env.client().request(Request::List, WAIT) else { panic!() };
        let live: Vec<_> = list.iter().filter(|t| t.exited.is_none()).map(|t| t.terminal).collect();
        if live.len() <= 1 || Instant::now() > deadline {
            break live;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(live, vec![busy.into()], "the idle shell closes with the app; the busy one keeps running");

    let mut second = env.app(None);
    wait(&mut second, "the busy terminal to reattach", |app| {
        app.terminal_status(busy) == Some(Status::Running) && text_of(app, busy).contains("marker-42")
    });
    assert_eq!(text_of(second.state(), busy).matches("marker-42").count(), 1, "reattached, not re-run");
    wait(&mut second, "the idle panel to get a fresh shell", |app| {
        app.terminal_status(idle) == Some(Status::Running)
    });
}

#[cfg(unix)]
#[test]
fn a_clean_exit_frees_the_panel_and_a_failed_one_is_shown_with_its_code() {
    let env = Env::new();
    let root = env.folder("exits");
    let mut failing = None;
    env.seed(&root, |project, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(startup("exit 0")));
        failing = Some(layout.add_panel(project, Some(first), Placement::Right, terminal(startup("exit 3"))));
    });
    let failing = failing.unwrap();
    let mut harness = env.app(None);
    let clean = first_panel(harness.state());
    wait(&mut harness, "both shells to exit", |app| {
        let layout = app.active_layout().unwrap();
        layout.panels[&clean].kind == PanelKind::Untyped
            && app.terminal_status(failing)
                == Some(Status::Exited(ExitStatus { code: Some(3), user_killed: false }))
    });
    steps(&mut harness, 2);
    harness.get_by_label("The process exited with code 3.");
    harness.get_by_label("What should this panel show?");
}

#[test]
fn an_unreadable_layout_is_kept_aside_and_reported_once() {
    let env = Env::new();
    let root = env.folder("broken");
    let project = env.seed(&root, |_, _| {});
    {
        let conn = rusqlite_open(&env.dirs.database());
        conn.execute("UPDATE layouts SET doc = '{\"version\": 99}'", []).unwrap();
    }
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    let notices: Vec<_> =
        harness.state().notices().iter().filter(|n| n.message.contains("could not be read")).collect();
    assert_eq!(notices.len(), 1);
    let store = Store::open(&env.dirs.database()).unwrap();
    assert_eq!(store.quarantined(project).unwrap(), 1, "the old layout is kept, not overwritten");
    assert!(harness.state().active_layout().is_some(), "a fresh layout replaced it");
}

fn rusqlite_open(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

#[test]
fn editing_and_saving_keeps_the_files_line_endings() {
    let env = Env::new();
    let root = env.folder("docs");
    let file = root.join("notes.txt");
    std::fs::write(&file, b"one\r\ntwo\r\n").unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Editor(EditorPanelConfig { path: Some(file.clone()) }));
    });
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    let editor = || egui::accesskit::Role::MultilineTextInput;
    harness.get_by_role(editor()).focus();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::End);
    steps(&mut harness, 1);
    harness.get_by_role(editor()).type_text("three");
    harness.key_press(Key::Enter);
    steps(&mut harness, 2);
    harness.get_by_label("notes.txt •");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    steps(&mut harness, 3);
    assert_eq!(std::fs::read(&file).unwrap(), b"one\r\ntwo\r\nthree\r\n");
    harness.get_by_label("notes.txt");
}

#[cfg(unix)]
#[test]
fn the_shell_starts_at_the_panels_real_size() {
    let env = Env::new();
    let root = env.folder("size");
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(startup("stty size")));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the shell to run", |app| app.terminal_status(panel) == Some(Status::Running));
    let (cols, rows) = harness.state().terminal_size(panel).unwrap();
    assert_ne!((cols, rows), (80, 24), "the panel should not be the placeholder size");
    let expected = format!("{rows} {cols}");
    wait(&mut harness, "stty to report the panel's size", |app| text_of(app, panel).contains(&expected));
}

fn editor_on(env: &Env, name: &str, contents: &str) -> (PathBuf, Harness<'static, ThrongApp>, PanelId) {
    let root = env.folder("edit");
    let file = root.join(name);
    std::fs::write(&file, contents).unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Editor(EditorPanelConfig { path: Some(file.clone()) }));
    });
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    let panel = first_panel(harness.state());
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    steps(&mut harness, 2);
    (file, harness, panel)
}

/// Step frames for `ms` of real time (debounces).
fn settle(harness: &mut Harness<'static, ThrongApp>, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        harness.step();
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
fn find_counts_steps_and_replace_all_is_one_undo_step() {
    let env = Env::new();
    let (_file, mut harness, panel) = editor_on(&env, "a.txt", "foo bar foo\nbaz foo\n");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    steps(&mut harness, 2);
    harness.get_by_role(egui::accesskit::Role::TextInput).type_text("foo");
    settle(&mut harness, 300);
    assert_eq!(harness.state().editor_find(panel), Some((Some(0), 3)));
    harness.get_by_label("1 of 3");
    harness.key_press(Key::Enter);
    steps(&mut harness, 2);
    assert_eq!(harness.state().editor_find(panel), Some((Some(1), 3)));
    assert_eq!(harness.state().editor_caret(panel), Some((1, 12)), "the match is selected, caret at its end");

    harness.get_by_label("Show replace").click();
    steps(&mut harness, 2);
    let inputs = harness.get_all_by_role(egui::accesskit::Role::TextInput).count();
    assert_eq!(inputs, 2, "the replace row is open");
    let replace = harness.get_all_by_role(egui::accesskit::Role::TextInput).nth(1).unwrap();
    replace.focus();
    steps(&mut harness, 1);
    harness.get_all_by_role(egui::accesskit::Role::TextInput).nth(1).unwrap().type_text("qux");
    steps(&mut harness, 1);
    harness.get_by_label("Replace All").click();
    steps(&mut harness, 2);
    assert_eq!(harness.state().editor_text(panel).as_deref(), Some("qux bar qux\nbaz qux\n"));

    // Undo in the editor takes the whole replace back.
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    steps(&mut harness, 1);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    steps(&mut harness, 2);
    assert_eq!(harness.state().editor_text(panel).as_deref(), Some("foo bar foo\nbaz foo\n"));
}

#[test]
fn go_to_line_puts_the_caret_at_the_start_of_that_line() {
    let env = Env::new();
    let text: String = (1..=50).map(|i| format!("line {i}\n")).collect();
    let (_file, mut harness, panel) = editor_on(&env, "lines.txt", &text);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::G);
    steps(&mut harness, 2);
    harness.get_by_role(egui::accesskit::Role::TextInput).type_text("30");
    steps(&mut harness, 1);
    harness.key_press(Key::Enter);
    steps(&mut harness, 3);
    assert_eq!(harness.state().editor_caret(panel), Some((30, 1)));
    // Typing lands there: focus went back to the editor.
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text(">");
    steps(&mut harness, 2);
    assert!(harness.state().editor_text(panel).unwrap().contains("\n>line 30\n"));
}

#[test]
fn a_large_file_opens_scrolls_and_edits_without_slow_frames() {
    let env = Env::new();
    let text = "fn value() -> u32 { let x = \"text\"; 42 } // comment\n".repeat(200_000);
    let (file, mut harness, panel) = editor_on(&env, "big.rs", &text);
    let mut slowest = Duration::ZERO;
    let mut frame = |harness: &mut Harness<'static, ThrongApp>| {
        let start = Instant::now();
        harness.step();
        slowest = slowest.max(start.elapsed());
    };
    for _ in 0..3 {
        frame(&mut harness);
    }
    harness.key_press_modifiers(Modifiers::COMMAND, Key::End);
    frame(&mut harness);
    frame(&mut harness);
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("end");
    frame(&mut harness);
    frame(&mut harness);
    assert_eq!(harness.state().editor_caret(panel).map(|c| c.0), Some(200_001));
    // Generous for an unoptimised build: what matters is that no frame pays for the whole file.
    assert!(slowest < Duration::from_secs(2), "slowest frame took {slowest:?}");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    steps(&mut harness, 2);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.ends_with("// comment\nend"));
}

#[cfg(unix)]
#[test]
fn find_in_a_terminal_counts_its_scrollback_and_escape_closes_it() {
    let env = Env::new();
    let root = env.folder("tfind");
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(startup("printf 'alpha\\nbeta error\\ngamma Error\\n'")));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the output", |app| text_of(app, panel).contains("gamma Error"));
    let label = "Terminal: Seeded › Tab 1 › Panel 1";
    harness.get_by_label(label).click();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    steps(&mut harness, 2);
    harness.get_by_role(egui::accesskit::Role::TextInput).type_text("error");
    settle(&mut harness, 300);
    // Both lines match (case-insensitive), plus the echoed command line itself.
    let (current, total) = harness.state().terminal_find(panel).unwrap();
    assert!(total >= 2, "found {total}");
    assert_eq!(current, Some(total - 1), "a new search starts at the newest match");
    harness.get_by_label("Match case").click();
    settle(&mut harness, 300);
    let (_, case_sensitive) = harness.state().terminal_find(panel).unwrap();
    assert!(case_sensitive < total, "Error no longer matches");
    // Escape in the terminal closes find; nothing reaches the shell.
    harness.get_by_label(label).click();
    // The click's frame, then the one the app asks for so the terminal's key lock is in place.
    steps(&mut harness, 2);
    harness.key_press(Key::Escape);
    steps(&mut harness, 2);
    assert_eq!(harness.state().terminal_find(panel), None);
}

fn project_with(env: &Env, files: &[(&str, &str)]) -> (PathBuf, Harness<'static, ThrongApp>) {
    let root = env.folder("proj");
    for (name, text) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Untyped);
    });
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    (root, harness)
}

#[test]
fn quick_open_finds_a_file_by_part_of_its_path_and_opens_it() {
    let env = Env::new();
    let (_root, mut harness) =
        project_with(&env, &[("src/main.rs", "fn main() {}\n"), ("docs/guide.md", "# Guide\n")]);
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::T);
    settle(&mut harness, 300);
    harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Quick Open").type_text("gui");
    steps(&mut harness, 2);
    harness.key_press(Key::Enter);
    steps(&mut harness, 4);
    let layout = harness.state().active_layout().unwrap();
    let opened: Vec<_> = layout
        .panels
        .values()
        .filter_map(|p| match &p.kind {
            PanelKind::Editor(EditorPanelConfig { path: Some(path) }) => {
                Some(path.file_name().unwrap().to_owned())
            }
            _ => None,
        })
        .collect();
    assert_eq!(opened, vec![std::ffi::OsString::from("guide.md")]);
    // Focus went to the new editor: typing lands in it.
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("x");
    steps(&mut harness, 2);
}

#[test]
fn find_in_files_lists_results_and_replace_all_writes_after_a_warning() {
    let env = Env::new();
    let (root, mut harness) = project_with(
        &env,
        &[
            ("a.txt", "alpha needle\n"),
            ("sub/b.txt", "needle one\nneedle two\n"),
            ("node_modules/x.txt", "needle\n"),
        ],
    );
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::H);
    steps(&mut harness, 3);
    let find = || (egui::accesskit::Role::TextInput, "Find in files");
    harness.get_by_role_and_label(find().0, find().1).focus();
    steps(&mut harness, 1);
    harness.get_by_role_and_label(find().0, find().1).type_text("needle");
    wait(&mut harness, "the search to finish", |app| app.search_results().is_some_and(|r| r.2));
    assert_eq!(harness.state().search_results(), Some((2, 3, true)), "node_modules is excluded");
    harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Replace with").focus();
    steps(&mut harness, 1);
    harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "Replace with").type_text("pin");
    steps(&mut harness, 2);
    harness.get_by_label("Replace All").click();
    // A modal sizes itself over its first frames; a click before it settles lands outside it.
    settle(&mut harness, 200);
    // The files are not open: throng warns before writing them.
    harness.get_by_label("Replace in Files").click();
    steps(&mut harness, 3);
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "alpha pin\n");
    assert_eq!(std::fs::read_to_string(root.join("sub/b.txt")).unwrap(), "pin one\npin two\n");
    assert_eq!(std::fs::read_to_string(root.join("node_modules/x.txt")).unwrap(), "needle\n");
    assert_eq!(harness.state().search_results().map(|r| r.1), Some(0), "committed rows leave the list");
}

#[test]
fn quitting_asks_nothing_about_unsaved_edits_and_the_next_launch_restores_them_with_undo() {
    let env = Env::new();
    let (file, mut first, panel) = editor_on(&env, "draft.txt", "one\n");
    let editor = || egui::accesskit::Role::MultilineTextInput;
    first.key_press_modifiers(Modifiers::COMMAND, Key::End);
    steps(&mut first, 1);
    first.get_by_role(editor()).type_text("two");
    steps(&mut first, 2);
    assert_eq!(first.state().editor_text(panel).as_deref(), Some("one\ntwo"));
    first.state_mut().request_close();
    steps(&mut first, 3);
    assert!(first.state().close_confirmed(), "no question about the unsaved editor");
    drop(first);
    assert_eq!(std::fs::read(&file).unwrap(), b"one\n", "quitting saved nothing");

    let mut second = env.app(None);
    steps(&mut second, 3);
    assert_eq!(second.state().editor_text(panel).as_deref(), Some("one\ntwo"), "the edit came back");
    second.get_by_label("draft.txt •");
    second.get_by_role(editor()).focus();
    steps(&mut second, 2);
    second.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    steps(&mut second, 2);
    assert_eq!(second.state().editor_text(panel).as_deref(), Some("one\n"), "and so did its undo history");
    second.get_by_label("draft.txt");
    second.state_mut().request_close();
    steps(&mut second, 3);
    assert!(second.state().close_confirmed());
    drop(second);

    // Clean again, so nothing is left to recover.
    let recovery = env.dirs.data.join("recovery");
    let left: Vec<String> = std::fs::read_dir(&recovery)
        .map(|d| d.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "a clean document keeps no recovery file: {left:?}");
}

#[test]
fn auto_save_writes_a_file_once_typing_stops() {
    let env = Env::new();
    env.dirs.ensure().unwrap();
    std::fs::write(env.dirs.settings_file(), r#"{"editor":{"autoSave":true,"autoSaveDebounceMs":200}}"#)
        .unwrap();
    let (file, mut harness, panel) = editor_on(&env, "a.txt", "x\n");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::End);
    steps(&mut harness, 1);
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("y");
    steps(&mut harness, 1);
    assert_eq!(harness.state().editor_text(panel).as_deref(), Some("x\ny"));
    settle(&mut harness, 600);
    assert_eq!(std::fs::read(&file).unwrap(), b"x\ny", "saved without being asked");
    harness.get_by_label("a.txt");
}

/// Click the node labelled `label` as a hand does: press and release in separate frames.
fn click_slowly(harness: &mut Harness<'static, ThrongApp>, label: &str) {
    let at = harness.get_by_label(label).rect().center();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    harness.event(egui::Event::PointerMoved(at));
    harness.event(button(true));
    steps(harness, 1);
    harness.event(button(false));
    steps(harness, 2);
}

/// The label of a folder's row in the tree, closed or open.
fn folder_row(harness: &Harness<'static, ThrongApp>, name: &str) -> String {
    let closed = format!("🗀 {name}");
    if harness.query_by_label(&closed).is_some() { closed } else { format!("🗁 {name}") }
}

/// Press on `from`, move to `to` over a few frames and release there: a real pointer drag.
fn drag(harness: &mut Harness<'static, ThrongApp>, from: egui::Pos2, to: egui::Pos2, modifiers: Modifiers) {
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    harness.event(egui::Event::PointerMoved(from));
    harness.event(button(from, true));
    steps(harness, 2);
    for t in [0.25, 0.5, 0.75, 1.0] {
        harness.event(egui::Event::PointerMoved(from + (to - from) * t));
        steps(harness, 1);
    }
    harness.event(button(to, false));
    steps(harness, 3);
}

#[test]
fn tree_moves_undo_redo_survive_a_restart_and_a_changed_world_is_refused() {
    let env = Env::new();
    let (root, mut harness) = project_with(&env, &[("a.txt", "a"), ("dir/keep.txt", "k")]);
    let from = harness.get_by_label("🗋 a.txt").rect().center();
    let onto = harness.get_by_label("🗀 dir").rect().center();
    drag(&mut harness, from, onto, Modifiers::NONE);
    assert!(root.join("dir/a.txt").exists() && !root.join("a.txt").exists(), "dragged into the folder");

    // The tree has the keyboard after a click in it.
    harness.get_by_label(&folder_row(&harness, "dir")).click();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    steps(&mut harness, 2);
    assert!(root.join("a.txt").exists() && !root.join("dir/a.txt").exists(), "undone");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Y);
    steps(&mut harness, 2);
    assert!(root.join("dir/a.txt").exists(), "redone");
    harness.state_mut().request_close();
    steps(&mut harness, 3);
    drop(harness);

    // The history outlives the app, and an entry the world has overtaken is refused
    // with a notice rather than acted on.
    std::fs::write(root.join("a.txt"), "someone else's").unwrap();
    let mut second = env.app(None);
    steps(&mut second, 3);
    second.get_by_label(&folder_row(&second, "dir")).click();
    steps(&mut second, 2);
    second.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    steps(&mut second, 2);
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"someone else's", "nothing overwritten");
    assert!(root.join("dir/a.txt").exists(), "nothing moved");
    let notice = second.state().notices().iter().find(|n| n.message.starts_with("Can't undo move"));
    assert!(
        notice.is_some_and(|n| n.message.contains("Something else is now at")),
        "{:?}",
        second.state().notices()
    );

    // Clear the way and the same entry goes through.
    std::fs::remove_file(root.join("a.txt")).unwrap();
    second.get_by_label(&folder_row(&second, "dir")).click();
    steps(&mut second, 2);
    second.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    steps(&mut second, 2);
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"a");
}

#[test]
fn a_rename_follows_an_open_editor_and_undo_takes_it_back_without_dirtying_it() {
    let env = Env::new();
    let (root, mut harness) = project_with(&env, &[("old.txt", "text\n")]);
    click_slowly(&mut harness, "🗋 old.txt");
    harness.key_press(Key::F2);
    steps(&mut harness, 3);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.get_by_role_and_label(egui::accesskit::Role::TextInput, "New name").type_text("new.txt");
    harness.key_press(Key::Enter);
    steps(&mut harness, 3);
    assert!(root.join("new.txt").exists() && !root.join("old.txt").exists());

    // Double-click opens it; a second of frames first, or egui counts a third click with the
    // earlier one on old.txt.
    steps(&mut harness, 60);
    harness.get_by_label("🗋 new.txt").click();
    harness.get_by_label("🗋 new.txt").click();
    steps(&mut harness, 4);
    harness.get_by_label("new.txt");
    harness.get_by_label("🗋 new.txt").click();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    steps(&mut harness, 3);
    assert!(root.join("old.txt").exists() && !root.join("new.txt").exists());
    // The editor followed the file back and is not dirty.
    harness.get_by_label("old.txt");
}

#[test]
fn a_file_dragged_from_the_tree_types_its_path_into_a_terminal_without_running_it() {
    let env = Env::new();
    let root = env.folder("drop");
    std::fs::write(root.join("my file.txt"), "x").unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(TerminalPanelConfig::default()));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the shell", |app| app.terminal_status(panel) == Some(Status::Running));
    let from = harness.get_by_label("🗋 my file.txt").rect().center();
    let term = harness.get_by_label_contains("Terminal: Seeded").rect().center();
    drag(&mut harness, from, term, Modifiers::NONE);
    let expected = format!("'{}' ", root.join("my file.txt").display());
    wait(&mut harness, "the path to be echoed", |app| text_of(app, panel).contains(expected.trim_end()));
    std::thread::sleep(Duration::from_millis(300));
    steps(&mut harness, 3);
    assert!(!text_of(harness.state(), panel).contains("No such file"), "nothing was run");
}

#[test]
fn a_file_dropped_on_an_empty_panel_opens_there_and_a_folder_is_refused() {
    let env = Env::new();
    let (root, mut harness) = project_with(&env, &[("a.txt", "a\n"), ("dir/b.txt", "b\n")]);
    let panel = first_panel(harness.state());
    let onto = harness.get_by_label("Start Terminal").rect().center();
    let folder = harness.get_by_label(&folder_row(&harness, "dir")).rect().center();
    drag(&mut harness, folder, onto, Modifiers::NONE);
    assert!(harness.state().notices().iter().any(|n| n.message.starts_with("A folder cannot be shown")));
    let file = harness.get_by_label("🗋 a.txt").rect().center();
    drag(&mut harness, file, onto, Modifiers::NONE);
    let kind = harness.state().active_layout().unwrap().panels[&panel].kind.clone();
    assert_eq!(kind, PanelKind::Editor(EditorPanelConfig { path: Some(root.join("a.txt")) }));
}

#[test]
fn a_path_in_an_editor_is_a_link_that_opens_its_file_at_the_line() {
    let env = Env::new();
    let root = env.folder("links");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/b.rs"), "one\ntwo\nthree\n").unwrap();
    let notes = root.join("notes.txt");
    std::fs::write(&notes, "see src/b.rs:3 and gone.rs:1\n").unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Editor(EditorPanelConfig { path: Some(notes.clone()) }));
    });
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    steps(&mut harness, 2);
    // The caret into the link, then Ctrl+Enter follows it.
    for _ in 0..6 {
        harness.key_press(Key::ArrowRight);
    }
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    steps(&mut harness, 4);
    let opened = harness.state().active_layout().unwrap().panels.values().find_map(|p| match &p.kind {
        PanelKind::Editor(EditorPanelConfig { path: Some(path) }) if path.ends_with("src/b.rs") => Some(p.id),
        _ => None,
    });
    let opened = opened.expect("src/b.rs opened in an editor");
    steps(&mut harness, 3);
    assert_eq!(harness.state().editor_caret(opened), Some((3, 1)), "at line 3");

    // A missing file is reported, not opened.
    harness.get_by_label("notes.txt").click();
    steps(&mut harness, 3);
    harness.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Editor: notes.txt").focus();
    steps(&mut harness, 2);
    harness.key_press(Key::End);
    for _ in 0..3 {
        harness.key_press(Key::ArrowLeft);
    }
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    steps(&mut harness, 3);
    assert!(
        harness.state().notices().iter().any(|n| n.message.ends_with("gone.rs does not exist.")),
        "{:?}",
        harness.state().notices()
    );
}

#[cfg(unix)]
#[test]
fn ctrl_click_on_a_path_in_terminal_output_opens_it_at_the_line() {
    let env = Env::new();
    let root = env.folder("termlinks");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/b.rs"), "one\ntwo\n").unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(startup(r"printf '\033[2J\033[Hsee src/b.rs:2\n'")));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the output", |app| {
        text_of(app, panel).lines().any(|l| l.trim_end() == "see src/b.rs:2")
    });
    steps(&mut harness, 3);
    let rect = harness.get_by_label_contains("Terminal: ").rect();
    let (cols, rows) = harness.state().terminal_size(panel).unwrap();
    let cell = egui::vec2((rect.width() - 8.0) / f32::from(cols), (rect.height() - 8.0) / f32::from(rows));
    // Row 0, column 6: inside "src/b.rs:2", which starts at column 4.
    let at = rect.min + egui::vec2(4.0 + cell.x * 6.5, 4.0 + cell.y * 0.5);
    // Ctrl held across the click, as a keyboard holds it.
    harness.event(egui::Event::ModifiersChanged(Modifiers::COMMAND));
    harness.event(egui::Event::PointerMoved(at));
    steps(&mut harness, 1);
    for pressed in [true, false] {
        let button = egui::PointerButton::Primary;
        harness.event(egui::Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::COMMAND });
        steps(&mut harness, 1);
    }
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    steps(&mut harness, 3);
    let opened = harness.state().active_layout().unwrap().panels.values().find_map(|p| match &p.kind {
        PanelKind::Editor(EditorPanelConfig { path: Some(path) }) if path.ends_with("src/b.rs") => Some(p.id),
        _ => None,
    });
    let opened = opened.expect("src/b.rs opened from the terminal");
    steps(&mut harness, 3);
    assert_eq!(harness.state().editor_caret(opened), Some((2, 1)));
}

#[test]
fn a_preview_opens_beside_its_editor_follows_unsaved_edits_and_follows_links_in_place() {
    let env = Env::new();
    let root = env.folder("docs");
    let plan = root.join("plan.md");
    std::fs::write(&plan, "# Plan\n\nSee [the other](other.md#part-two).\n").unwrap();
    std::fs::write(root.join("other.md"), "# Other\n\n## Part Two\n\nSecond.\n").unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Editor(EditorPanelConfig { path: Some(plan.clone()) }));
    });
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    let editor = first_panel(harness.state());

    // Open Preview from the editor's tab menu.
    harness.get_by_label("plan.md").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label("Open Preview").click();
    steps(&mut harness, 4);
    let layout = harness.state().active_layout().unwrap();
    let preview = layout
        .panels
        .values()
        .find(|p| matches!(p.kind, PanelKind::Preview(_)))
        .map(|p| p.id)
        .expect("a preview panel");
    assert_ne!(preview, editor);
    harness.get_by_label("plan.md - Preview");
    harness.get_by_label("Plan");

    // It follows the unsaved document, after the update delay.
    harness.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Editor: plan.md").focus();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::End);
    harness.key_press(Key::Enter);
    harness
        .get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Editor: plan.md")
        .type_text("Fresh words.");
    settle(&mut harness, 600);
    harness.get_by_label("Fresh words.");
    harness.get_by_label("plan.md - Preview •");

    // Ctrl+click on a link to another Markdown file shows it in the same preview.
    harness.get_by_label("the other").click_modifiers(Modifiers::COMMAND);
    steps(&mut harness, 4);
    let shown = |app: &ThrongApp| match &app.active_layout().unwrap().panels[&preview].kind {
        PanelKind::Preview(config) => (config.path.clone(), config.can_back(), config.can_forward()),
        other => panic!("not a preview: {other:?}"),
    };
    assert_eq!(shown(harness.state()), (root.join("other.md"), true, false));
    steps(&mut harness, 3);
    harness.get_by_label("Part Two");
    harness.get_by_label("other.md - Preview");

    // Back returns to the plan, Forward to the other file; Alt+Left goes back from the keyboard.
    harness.get_by_label("Back").click();
    steps(&mut harness, 4);
    assert_eq!(shown(harness.state()), (plan.clone(), false, true));
    harness.get_by_label("the other");
    harness.get_by_label("Forward").click();
    steps(&mut harness, 4);
    assert_eq!(shown(harness.state()), (root.join("other.md"), true, false));
    harness.get_by_label("Part Two").click();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    steps(&mut harness, 4);
    assert_eq!(shown(harness.state()).0, plan, "Alt+Left in the focused preview");
    harness.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    steps(&mut harness, 4);
    assert_eq!(shown(harness.state()).0, root.join("other.md"), "and it keeps the keyboard");
}

#[cfg(unix)]
#[test]
fn a_terminal_reopens_in_the_folder_it_was_last_working_in() {
    let env = Env::new();
    let root = env.folder("dirs");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(TerminalPanelConfig::default()));
    });
    let mut first = env.app(None);
    let panel = first_panel(first.state());
    wait(&mut first, "the prompt", |app| app.terminal_status(panel) == Some(Status::Running));
    wait(&mut first, "the root to be seen", |app| app.terminal_directory(panel) == Some(root.clone()));
    let label = "Terminal: Seeded › Tab 1 › Panel 1";
    first.get_by_label(label).click();
    steps(&mut first, 2);
    first.get_by_label(label).type_text("cd sub");
    first.key_press(Key::Enter);
    // The shell's own directory, read from outside it (no shell hooks).
    wait(&mut first, "the directory to be seen", |app| {
        app.terminal_directory(panel) == Some(root.join("sub"))
    });
    let remembered = |app: &ThrongApp| match &app.active_layout().unwrap().panels[&panel].kind {
        PanelKind::Terminal(config) => config.last_directory.clone(),
        _ => None,
    };
    assert_eq!(remembered(first.state()), Some(root.join("sub")), "kept with the layout");
    first.state_mut().request_close();
    steps(&mut first, 3);
    assert!(first.state().close_confirmed());
    drop(first);

    // The idle shell closed with the app; a cold start goes back to the folder.
    let mut second = env.app(None);
    wait(&mut second, "the new shell", |app| app.terminal_status(panel) == Some(Status::Running));
    wait(&mut second, "its directory", |app| app.terminal_directory(panel) == Some(root.join("sub")));
}

#[test]
fn an_editors_status_strip_reads_the_caret_and_counts_and_opens_the_language_picker() {
    let env = Env::new();
    let (_file, mut harness, _panel) = editor_on(&env, "a.txt", "one two\nthree 😀\n");
    // Readouts are named for what they are.
    harness.get_by_label("line 1");
    harness.get_by_label("column 1");
    settle(&mut harness, 250);
    harness.get_by_label("17 characters");
    harness.get_by_label("4 words");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::End);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::ArrowLeft);
    steps(&mut harness, 2);
    // The line break before the empty last line is selected; the caret is at line 2's end.
    harness.get_by_label("line 2");
    harness.get_by_label("1 characters selected");
    harness.key_press(Key::ArrowRight);
    harness.key_press(Key::ArrowUp);
    harness.key_press(Key::End);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::ArrowLeft);
    steps(&mut harness, 2);
    harness.get_by_label("2 characters selected");
    harness.get_by_label("Plain Text").click();
    steps(&mut harness, 4);
    harness.get_by_label("Set Language");
}

#[test]
fn an_idle_terminal_closed_by_quitting_comes_back_as_a_terminal_not_a_picker() {
    let env = Env::new();
    let root = env.folder("idle");
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(TerminalPanelConfig::default()));
    });
    let mut first = env.app(None);
    let panel = first_panel(first.state());
    wait(&mut first, "the shell", |app| app.terminal_status(panel) == Some(Status::Running));
    first.state_mut().request_close();
    steps(&mut first, 3);
    assert!(first.state().close_confirmed());
    // The frames a real window draws while it closes: the idle shell's exit arrives in them.
    settle(&mut first, 600);
    drop(first);
    // And nothing started it again: no shell outlives the app.
    let deadline = Instant::now() + WAIT;
    loop {
        let Ok(Reply::Terminals(list)) = env.client().request(Request::List, WAIT) else { panic!() };
        let live = list.iter().filter(|t| t.exited.is_none()).count();
        if live == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "{live} shell(s) still running after the app quit");
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut second = env.app(None);
    steps(&mut second, 3);
    let kind = second.state().active_layout().unwrap().panels[&panel].kind.clone();
    assert!(matches!(kind, PanelKind::Terminal(_)), "still a terminal panel (Principle III): {kind:?}");
    wait(&mut second, "a fresh shell", |app| app.terminal_status(panel) == Some(Status::Running));
}

#[test]
fn the_trees_clipboard_answers_the_platforms_copy_cut_and_paste_events() {
    let env = Env::new();
    let (root, mut harness) = project_with(&env, &[("a.txt", "a"), ("b.txt", "b"), ("dir/keep.txt", "k")]);
    // What egui-winit sends for Ctrl/Cmd+C and Ctrl/Cmd+V: events, not keys.
    harness.get_by_label("🗋 a.txt").click();
    steps(&mut harness, 2);
    harness.event(egui::Event::Copy);
    steps(&mut harness, 2);
    harness.get_by_label(&folder_row(&harness, "dir")).click();
    steps(&mut harness, 2);
    harness.event(egui::Event::Paste("ignored".into()));
    steps(&mut harness, 3);
    assert!(root.join("dir/a.txt").exists() && root.join("a.txt").exists(), "copied");
    harness.get_by_label("🗋 b.txt").click();
    steps(&mut harness, 2);
    harness.event(egui::Event::Cut);
    steps(&mut harness, 2);
    harness.get_by_label(&folder_row(&harness, "dir")).click();
    steps(&mut harness, 2);
    harness.event(egui::Event::Paste("ignored".into()));
    steps(&mut harness, 3);
    assert!(root.join("dir/b.txt").exists() && !root.join("b.txt").exists(), "moved");
}

#[test]
fn a_users_theme_file_is_drawn_reloads_when_edited_and_themes_are_picked_and_duplicated_in_preferences() {
    let env = Env::new();
    env.dirs.ensure().unwrap();
    let themes = env.dirs.config.join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    let mine = themes.join("mine.json");
    std::fs::write(&mine, r##"{"name":"Mine","colours":{"appBg":"#f0f0f0","text":"#101010"}}"##).unwrap();
    std::fs::write(env.dirs.settings_file(), r#"{"appearance":{"theme":"Mine"}}"#).unwrap();
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    assert_eq!(harness.state().theme_name(), "Mine");
    let fill = |h: &Harness<'static, ThrongApp>| h.ctx.style_of(h.ctx.theme()).visuals.panel_fill;
    assert_eq!(fill(&harness), egui::Color32::from_rgb(0xf0, 0xf0, 0xf0));
    assert!(
        !harness.ctx.style_of(harness.ctx.theme()).visuals.dark_mode,
        "a light ground draws as a light theme"
    );

    // An edit to the file is picked up while throng runs.
    std::fs::write(&mine, r##"{"name":"Mine","colours":{"appBg":"#202020"}}"##).unwrap();
    let deadline = Instant::now() + WAIT;
    while fill(&harness) != egui::Color32::from_rgb(0x20, 0x20, 0x20) {
        assert!(Instant::now() < deadline, "timed out waiting for the edited theme");
        harness.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(harness.ctx.style_of(harness.ctx.theme()).visuals.dark_mode);

    // Preferences: pick a built-in, then duplicate it into an editable theme of one's own.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    steps(&mut harness, 2);
    harness.get_by_label("Themes").click();
    steps(&mut harness, 2);
    harness.get_by_label("Active theme").click();
    steps(&mut harness, 2);
    harness.get_by_label("Snake").click();
    steps(&mut harness, 3);
    assert_eq!(harness.state().theme_name(), "Snake");
    let written = std::fs::read_to_string(env.dirs.settings_file()).unwrap();
    assert!(written.contains("\"Snake\""), "{written}");
    harness.get_by_label("Duplicate").click();
    steps(&mut harness, 3);
    assert_eq!(harness.state().theme_name(), "Snake copy");
    let copy = std::fs::read_to_string(themes.join("Snake copy.json")).unwrap();
    assert!(copy.contains("\"name\": \"Snake copy\""), "{copy}");
}

#[test]
fn key_bindings_come_from_the_file_and_are_captured_in_preferences_asking_before_taking_a_chord() {
    let env = Env::new();
    env.dirs.ensure().unwrap();
    let file = env.dirs.keybindings_file();
    std::fs::write(&file, r#"{"version":1,"note":"mine","bindings":{"navigate.quickOpen":["Ctrl+P"]}}"#)
        .unwrap();
    let (_root, mut harness) = project_with(&env, &[("a.txt", "a\n")]);
    let quick_open = |h: &Harness<'static, ThrongApp>| {
        h.query_by_role_and_label(egui::accesskit::Role::TextInput, "Quick Open").is_some()
    };
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::T);
    settle(&mut harness, 100);
    assert!(!quick_open(&harness), "the default chord was replaced");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::P);
    settle(&mut harness, 300);
    assert!(quick_open(&harness));
    harness.key_press(Key::Escape);
    settle(&mut harness, 100);
    assert!(!quick_open(&harness));

    harness.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    steps(&mut harness, 2);
    harness.get_by_label("Key Bindings").click();
    steps(&mut harness, 2);
    if !cfg!(target_os = "macos") {
        // Quick Open is live in terminals, and Ctrl+R is the shell's reverse search.
        harness.get_by_label("Add a chord for Quick Open").click();
        steps(&mut harness, 2);
        harness.key_press_modifiers(Modifiers::COMMAND, Key::R);
        steps(&mut harness, 3);
        harness.get_by_label_contains("is the terminal's");
    }
    // A chord another command runs is taken only when the user says so.
    harness.get_by_label("Add a chord for Quick Open").click();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
    steps(&mut harness, 3);
    harness.get_by_label_contains("already runs Find in Files");
    harness.get_by_label("Reassign").click();
    steps(&mut harness, 3);
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written["note"], "mine", "the rest of the file is kept");
    assert_eq!(written["bindings"]["navigate.quickOpen"], serde_json::json!(["Ctrl+P", "Ctrl+Shift+F"]));
    assert_eq!(written["bindings"]["search.findInFiles"], serde_json::json!([]));
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
    settle(&mut harness, 300);
    assert!(quick_open(&harness), "the chord now opens Quick Open");
}

#[test]
fn an_icon_pack_draws_the_tree_and_what_it_cannot_draw_keeps_throngs_icon() {
    let env = Env::new();
    env.dirs.ensure().unwrap();
    let pack = env.dirs.config.join("icon-packs/letters");
    std::fs::create_dir_all(&pack).unwrap();
    // U+E000 is a private-use character no bundled font draws.
    std::fs::write(
        pack.join("pack.json"),
        r#"{"name":"Letters","tokens":{"file":"F","folder":"D","refresh":"","newFile":"new.svg","newFolder":"img/plus.svg","dismiss":"../escape.svg"}}"#,
    )
    .unwrap();
    // An SVG in the pack draws; one outside it is refused.
    std::fs::create_dir_all(pack.join("img")).unwrap();
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path d="M7 2h2v5h5v2H9v5H7V9H2V7h5z" fill="#3a8"/></svg>"##;
    std::fs::write(pack.join("img/plus.svg"), svg).unwrap();
    std::fs::write(pack.parent().unwrap().join("escape.svg"), svg).unwrap();
    std::fs::write(env.dirs.settings_file(), r#"{"appearance":{"iconPack":"letters"}}"#).unwrap();
    let (_root, mut harness) = project_with(&env, &[("a.txt", "a\n"), ("dir/b.txt", "b\n")]);
    steps(&mut harness, 2);
    harness.get_by_label("F a.txt");
    harness.get_by_label("D dir");
    let notice = harness.state().notices().iter().find(|n| n.key == "icons:problems").cloned();
    let detail = notice.and_then(|n| n.detail).unwrap_or_default();
    assert!(detail.contains("refresh") && detail.contains("newFile"), "{detail}");
    assert!(detail.contains("dismiss") && !detail.contains("newFolder"), "{detail}");
    let uri = format!("file://{}", std::fs::canonicalize(pack.join("img/plus.svg")).unwrap().display());
    let deadline = Instant::now() + WAIT;
    loop {
        harness.step();
        let poll =
            harness.ctx.try_load_texture(&uri, egui::TextureOptions::default(), egui::SizeHint::default());
        if matches!(poll, Ok(egui::load::TexturePoll::Ready { .. })) {
            break;
        }
        assert!(Instant::now() < deadline, "the pack's SVG never drew: {:?}", poll.err());
        std::thread::sleep(Duration::from_millis(15));
    }

    // Back to throng's own icons once the setting is cleared.
    std::fs::write(env.dirs.settings_file(), r#"{"appearance":{"iconPack":""}}"#).unwrap();
    let deadline = Instant::now() + WAIT;
    while harness.query_by_label("🗋 a.txt").is_none() {
        assert!(Instant::now() < deadline, "timed out waiting for throng's icons");
        harness.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(harness.state().notices().iter().all(|n| n.key != "icons:problems"));
}

#[test]
fn a_project_editor_synced_into_a_sub_workspace_is_one_document_and_the_window_comes_back_after_a_restart() {
    let env = Env::new();
    let root = env.folder("sync");
    let notes = root.join("notes.txt");
    std::fs::write(&notes, "one\n").unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Editor(EditorPanelConfig { path: Some(notes.clone()) }));
    });
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    let editor = first_panel(harness.state());

    // Sync to Sub-workspace ▸ New Sub-workspace: a window of its own shows the editor.
    harness.get_by_label("notes.txt").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label_contains("Sync to Sub-workspace").click();
    steps(&mut harness, 3);
    harness.get_by_label("New Sub-workspace").click();
    steps(&mut harness, 4);
    assert_eq!(harness.state().sub_workspaces(), vec![("Sub-workspace 1".to_owned(), true)]);
    harness.get_by_label("Seeded · notes.txt");

    // Typing in the sub-workspace's view edits the project's one document.
    let mirror = || (egui::accesskit::Role::MultilineTextInput, "Editor: notes.txt, from Seeded");
    harness.get_by_role_and_label(mirror().0, mirror().1).focus();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::End);
    harness.get_by_role_and_label(mirror().0, mirror().1).type_text("two");
    steps(&mut harness, 3);
    assert_eq!(harness.state().editor_text(editor).as_deref(), Some("one\ntwo"));
    harness.get_by_label("notes.txt •");
    // Saving there saves the project's file.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::S);
    steps(&mut harness, 3);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "one\ntwo");

    // The sub-workspace and what it shows survive a restart.
    harness.state_mut().request_close();
    steps(&mut harness, 3);
    drop(harness);
    let mut harness = env.app(None);
    steps(&mut harness, 4);
    assert_eq!(harness.state().sub_workspaces(), vec![("Sub-workspace 1".to_owned(), true)]);
    harness.get_by_label("Seeded · notes.txt");

    // Closing the window keeps the sub-workspace; the sidebar opens it again.
    harness.get_by_label("Close Window").click();
    steps(&mut harness, 3);
    assert_eq!(harness.state().sub_workspaces(), vec![("Sub-workspace 1".to_owned(), false)]);
    assert!(harness.query_by_label("Seeded · notes.txt").is_none());
    harness.get_by_label("Sub-workspace 1").click();
    steps(&mut harness, 3);
    harness.get_by_label("Seeded · notes.txt");

    // Destroying the project's panel takes it out of the sub-workspace too, and a sub-workspace
    // left with nothing goes. (Here the window is drawn inside the main one, over
    // the tab strip: it is closed first so the tab can be reached.)
    harness.get_by_label("Close Window").click();
    steps(&mut harness, 3);
    harness.get_by_label("notes.txt").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label("Close Panel").click();
    steps(&mut harness, 4);
    assert!(harness.state().sub_workspaces().is_empty());
}

#[cfg(unix)]
#[test]
fn a_terminal_synced_into_a_sub_workspace_takes_input_there_and_closing_it_there_leaves_it_running() {
    let env = Env::new();
    let root = env.folder("shared");
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(TerminalPanelConfig::default()));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the shell", |app| app.terminal_status(panel) == Some(Status::Running));
    // Open the terminal tab's menu and sync it into a new sub-workspace.
    let title = "Panel 1";
    harness.get_by_label(title).click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label_contains("Sync to Sub-workspace").click();
    steps(&mut harness, 3);
    harness.get_by_label("New Sub-workspace").click();
    steps(&mut harness, 4);

    // Typed into the mirror, it reaches the project's shell: one session.
    harness.get_by_label_contains(", mirrored").click();
    steps(&mut harness, 2);
    harness.get_by_label_contains(", mirrored").type_text("echo via-$((40+2))");
    harness.key_press(Key::Enter);
    wait(&mut harness, "the output", |app| text_of(app, panel).contains("via-42"));

    // "Close Here" closes the mirror only: the terminal runs on in its project.
    harness.get_by_label(&format!("Seeded · {title}")).click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label("Close Here").click();
    steps(&mut harness, 4);
    assert!(harness.state().sub_workspaces().is_empty(), "emptied, the sub-workspace went");
    assert_eq!(harness.state().terminal_status(panel), Some(Status::Running));
}

#[cfg(unix)]
#[test]
fn a_terminal_that_remembers_its_command_keeps_what_ran_when_it_was_killed() {
    let env = Env::new();
    let root = env.folder("memory");
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout
            .set_kind(first, terminal(TerminalPanelConfig { remember_command: true, ..Default::default() }));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the shell", |app| app.terminal_status(panel) == Some(Status::Running));
    let label = "Terminal: Seeded › Tab 1 › Panel 1";
    harness.get_by_label(label).click();
    steps(&mut harness, 2);
    harness.get_by_label(label).type_text("sleep 300");
    harness.key_press(Key::Enter);
    let saved = |app: &ThrongApp| match &app.active_layout().unwrap().panels[&panel].kind {
        PanelKind::Terminal(config) => Some(config.clone()),
        _ => None,
    };
    // Seen running from outside the shell, and kept with the layout in case nobody sees the end.
    wait(&mut harness, "the command to be seen", |app| {
        saved(app).and_then(|c| c.running_command).as_deref() == Some("sleep 300")
    });

    harness.get_by_label("Panel 1").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label("Kill Terminal (keep panel)").click();
    steps(&mut harness, 3);
    // The picker offers it back as the startup command, and the choice to remember stays on.
    harness.get_by_label("Start Terminal").click();
    steps(&mut harness, 3);
    let config = saved(harness.state()).expect("a terminal again");
    assert_eq!(config.startup_command.as_deref(), Some("sleep 300"));
    assert!(config.remember_command && config.running_command.is_none());
    wait(&mut harness, "the new shell", |app| app.terminal_status(panel) == Some(Status::Running));
}

#[cfg(unix)]
#[test]
fn with_manual_reload_a_saved_terminal_waits_for_reload_and_a_crash_s_command_comes_back() {
    let env = Env::new();
    let root = env.folder("manual");
    std::fs::create_dir_all(env.dirs.settings_file().parent().unwrap()).unwrap();
    std::fs::write(env.dirs.settings_file(), r#"{"terminal":{"reloadMode":"manual"}}"#).unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        // What a crash leaves: a command seen running, and no end ever seen.
        let config = TerminalPanelConfig {
            remember_command: true,
            running_command: Some("echo remembered-$((6*7))".into()),
            ..Default::default()
        };
        layout.set_kind(first, terminal(config));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the panel to wait", |app| app.terminal_status(panel) == Some(Status::Dormant));
    harness.get_by_label("\"Seeded › Tab 1 › Panel 1\" is not running.");
    let listed = env.client().request(Request::List, WAIT).unwrap();
    assert!(matches!(listed, Reply::Terminals(ref list) if list.is_empty()), "no shell: {listed:?}");

    // The menu says Reload while it waits; the panel's own button does the same.
    harness.get_by_label("Panel 1").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label("Reload Terminal");
    harness.key_press(Key::Escape);
    steps(&mut harness, 2);
    harness.get_by_label("Reload").click();
    wait(&mut harness, "the remembered command's output", |app| {
        text_of(app, panel).contains("remembered-42")
    });
    match &harness.state().active_layout().unwrap().panels[&panel].kind {
        PanelKind::Terminal(config) => {
            assert_eq!(config.startup_command.as_deref(), Some("echo remembered-$((6*7))"));
            assert_eq!(config.running_command, None, "the capture is spent");
        }
        other => panic!("not a terminal: {other:?}"),
    }
}

#[test]
fn a_preview_draws_the_projects_images_and_shows_alt_text_for_what_it_may_not_load() {
    let env = Env::new();
    let root = env.folder("pictures");
    std::fs::create_dir_all(root.join("img")).unwrap();
    image::RgbaImage::from_pixel(24, 12, image::Rgba([200, 40, 40, 255]))
        .save(root.join("img/chart.png"))
        .unwrap();
    let outside = env.folder("elsewhere").join("secret.png");
    image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255])).save(&outside).unwrap();
    let readme = root.join("README.md");
    std::fs::write(
        &readme,
        format!(
            "# Pictures\n\n![a red chart](img/chart.png \"Q3\")\n\n![kept out]({})\n\n![plain web](http://x.invalid/a.png)\n",
            outside.display()
        ),
    )
    .unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Preview(PreviewPanelConfig::new(readme.clone())));
    });
    let mut harness = env.app(None);
    let uri = format!("file://{}", std::fs::canonicalize(root.join("img/chart.png")).unwrap().display());
    let deadline = Instant::now() + WAIT;
    loop {
        harness.step();
        let poll =
            harness.ctx.try_load_texture(&uri, egui::TextureOptions::default(), egui::SizeHint::default());
        if matches!(poll, Ok(egui::load::TexturePoll::Ready { .. })) {
            break;
        }
        assert!(Instant::now() < deadline, "the chart never decoded: {:?}", poll.err());
        std::thread::sleep(Duration::from_millis(15));
    }
    steps(&mut harness, 2);
    // Drawn, and named by its alt text; the ones it may not load stand as their alt text.
    harness.get_by_label("a red chart");
    assert!(harness.query_by_label("[a red chart]").is_none(), "drawn, not stood in for");
    harness.get_by_label("[kept out]");
    harness.get_by_label("[plain web]");
}

#[test]
fn a_preview_and_its_editor_scroll_together_both_ways() {
    let env = Env::new();
    let text: String = (1..=120).map(|i| format!("## Section {i}\n\nText for section {i}.\n\n")).collect();
    let (_file, mut harness, editor) = editor_on(&env, "long.md", &text);
    harness.get_by_label("long.md").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label("Open Preview").click();
    steps(&mut harness, 6);
    let preview = harness
        .state()
        .active_layout()
        .unwrap()
        .panels
        .values()
        .find(|p| matches!(p.kind, PanelKind::Preview(_)))
        .map(|p| p.id)
        .expect("a preview");
    let close = |a: usize, b: usize| a.abs_diff(b) <= 4;

    // The editor goes down the file: the preview follows it to the same section.
    harness.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    steps(&mut harness, 2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::G);
    steps(&mut harness, 2);
    harness.get_by_role(egui::accesskit::Role::TextInput).type_text("300");
    steps(&mut harness, 1);
    harness.key_press(Key::Enter);
    steps(&mut harness, 6);
    let top = harness.state_mut().editor_top_line(editor).expect("the editor's top line");
    assert!(top > 200, "the editor moved: {top}");
    let (offset, line) = harness.state().preview_position(preview).unwrap();
    assert!(offset > 0.0 && line.is_some_and(|l| close(l, top)), "preview at {line:?} for editor {top}");

    // The reader scrolls the preview back up: the editor follows it.
    let over = harness.get_by_label("long.md - Preview").rect().center() + egui::vec2(0.0, 200.0);
    harness.event(egui::Event::PointerMoved(over));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 2400.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    });
    steps(&mut harness, 8);
    let (_, line) = harness.state().preview_position(preview).unwrap();
    let line = line.expect("the preview's top line");
    let top = harness.state_mut().editor_top_line(editor).unwrap();
    assert!(top < 200, "the editor followed the preview up: {top}");
    assert!(close(line, top), "editor at {top} for preview {line}");
}

#[cfg(unix)]
#[test]
fn a_terminal_moved_to_a_sub_workspace_lives_only_there_and_returns_to_its_project() {
    let env = Env::new();
    let root = env.folder("moving");
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, terminal(TerminalPanelConfig::default()));
    });
    let mut harness = env.app(None);
    let panel = first_panel(harness.state());
    wait(&mut harness, "the shell", |app| app.terminal_status(panel) == Some(Status::Running));
    harness.get_by_label("Panel 1").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label_contains("Move to Sub-workspace").click();
    steps(&mut harness, 3);
    harness.get_by_label("New Sub-workspace").click();
    steps(&mut harness, 4);

    // Out of its project's tabs, its record kept there as away; shown in the sub-workspace only.
    let layout = harness.state().active_layout().unwrap();
    assert!(layout.is_away(panel) && layout.tab_of(panel).is_none(), "{:?}", layout.away);
    assert_eq!(harness.state().sub_workspaces().len(), 1);
    harness.get_by_label_contains(", mirrored").click();
    steps(&mut harness, 2);
    harness.get_by_label_contains(", mirrored").type_text("echo moved-$((6*7))");
    harness.key_press(Key::Enter);
    wait(&mut harness, "the output", |app| text_of(app, panel).contains("moved-42"));

    // "Return to Seeded" brings it back; the emptied sub-workspace goes.
    harness.get_by_label("Seeded · Panel 1").click_secondary();
    steps(&mut harness, 3);
    harness.get_by_label("Return to Seeded").click();
    steps(&mut harness, 4);
    let layout = harness.state().active_layout().unwrap();
    assert!(!layout.is_away(panel) && layout.tab_of(panel).is_some());
    assert!(harness.state().sub_workspaces().is_empty());
    assert_eq!(harness.state().terminal_status(panel), Some(Status::Running), "the same shell");
    assert!(text_of(harness.state(), panel).contains("moved-42"));
}

#[test]
fn a_tab_dragged_out_of_the_dock_tears_off_into_a_sub_workspace_window() {
    let env = Env::new();
    let root = env.folder("tearing");
    std::fs::write(root.join("notes.md"), "# Notes\n").unwrap();
    env.seed(&root, |_, layout| {
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(first, PanelKind::Editor(EditorPanelConfig { path: Some(root.join("notes.md")) }));
        let project = layout.panels[&first].origin_project;
        layout.add_panel(project, Some(first), Placement::Right, PanelKind::Untyped);
    });
    let mut harness = env.app(None);
    steps(&mut harness, 3);
    let editor = first_panel(harness.state());
    // Dropped on a panel but away from its split targets (they sit in its middle): the dock makes
    // it a floating window, which throng tears off into a window of its own.
    let from = harness.get_by_label("notes.md").rect().center();
    let to = egui::pos2(1240.0, 740.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    steps(&mut harness, 4);
    let layout = harness.state().active_layout().unwrap();
    assert!(layout.is_away(editor), "torn off: {:?}", layout.away);
    assert_eq!(harness.state().sub_workspaces().len(), 1);
    harness.get_by_label("Seeded · notes.md");
}
