//! Seed a throng home with a demo project, for screenshots and manual checks:
//!
//! ```sh
//! cargo run -p throng-app --example seed -- <THRONG_HOME> <project-root> [file-to-edit]
//! THRONG_HOME=<THRONG_HOME> cargo run -p throng-app
//! ```
//!
//! With `THRONG_SEED_SUB=1`, the editor and the terminal are also shown in a sub-workspace window.

use std::path::PathBuf;

use throng_core::paths::PathRules;
use throng_core::project::{ProjectBook, ProjectInput};
use throng_core::subworkspace::{Place, SubWorkspaces};
use throng_core::terminal::TerminalPanelConfig;
use throng_core::workspace::{
    EditorPanelConfig, Layout, MirrorPanelConfig, PanelKind, Placement, PreviewPanelConfig,
};
use throng_persistence::{ACTIVE_PROJECT_KEY, SUB_WORKSPACES_KEY, Store};
use throng_platform::dirs::AppDirs;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(args.next().expect("usage: seed <THRONG_HOME> <project-root>"));
    let root = std::fs::canonicalize(args.next().expect("usage: seed <THRONG_HOME> <project-root>"))?;
    let dirs = AppDirs::under(&home);
    dirs.ensure()?;
    let store = Store::open(&dirs.database())?;
    let mut book = ProjectBook::default();
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "demo".into());
    let project = book
        .create(&PathRules::LINUX, &ProjectInput { name, colour: "#4cc38a".into(), root: root.clone() }, 0)?
        .clone();
    store.upsert_project(&project, 0)?;
    store.set_state(ACTIVE_PROJECT_KEY, Some(&project.id.to_string()))?;

    let mut layout = Layout::new_default(project.id);
    let first = layout.tabs[0].root.panels()[0];
    let demo = "printf '\\033[1;32mthrong\\033[0m — \\033[34mblue\\033[0m \\033[33myellow\\033[0m \\033[41;97m error \\033[0m\\n'; ls -la --color=always | head -12";
    layout.set_kind(
        first,
        PanelKind::Terminal(TerminalPanelConfig { startup_command: Some(demo.into()), ..Default::default() }),
    );
    let chosen = args.next().map(|f| root.join(f)).filter(|p| p.is_file());
    let readme = chosen.or_else(|| {
        ["README.md", "Cargo.toml", "package.json"].iter().map(|f| root.join(f)).find(|p| p.is_file())
    });
    let markdown = readme.clone().filter(|p| p.extension().is_some_and(|e| e == "md"));
    let editor = layout.add_panel(
        project.id,
        Some(first),
        Placement::Right,
        PanelKind::Editor(EditorPanelConfig { path: readme }),
    );
    // A Markdown file shows its preview beside its editor.
    if let Some(path) = markdown {
        layout.add_panel(
            project.id,
            Some(editor),
            Placement::Right,
            PanelKind::Preview(PreviewPanelConfig { path }),
        );
    }
    layout.rename_panel(first, "build");
    layout.add_tab(project.id, PanelKind::Untyped);
    layout.active_tab = Some(layout.tabs[0].id);
    store.save_layout(project.id, &layout, 0)?;
    if std::env::var_os("THRONG_SEED_SUB").is_some() {
        let mut subs = SubWorkspaces::default();
        let id = subs.create();
        subs.get_mut(id).expect("just made").place =
            Some(Place { x: 1290.0, y: 40.0, width: 700.0, height: 760.0 });
        let mut sub = Layout::new_default(id);
        let shown = sub.tabs[0].root.panels()[0];
        sub.set_kind(shown, PanelKind::Mirror(MirrorPanelConfig { project: project.id, panel: editor }));
        sub.add_panel(
            id,
            Some(shown),
            Placement::Below,
            PanelKind::Mirror(MirrorPanelConfig { project: project.id, panel: first }),
        );
        store.save_layout(id, &sub, 0)?;
        store.set_state(SUB_WORKSPACES_KEY, Some(&subs.to_json()))?;
    }
    println!("seeded {} with project {} ({})", home.display(), project.name, project.id);
    Ok(())
}
