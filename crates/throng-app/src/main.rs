//! `throng` — the UI, and (as `throng daemon`) the terminal daemon it starts.

// A release build on Windows is a windowed program: launching it opens no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use throng_app::remote::{self, Command, Reply};
use throng_app::{Services, ThrongApp, logging};
use throng_core::ids::TerminalId;
use throng_platform::dirs::AppDirs;

const USAGE: &str = "\
throng — project-first terminal & agent workspace

USAGE:
    throng [FOLDER]        Open throng, and the project for FOLDER (created if needed).
                           While throng is open, the folder is handed to it instead.
    throng open PATH       Open a file in the open throng, in the project that holds it;
                           PATH may end in :LINE or :LINE:COL. A folder opens its project.
    throng focus           Bring the terminal this is typed in to the front
    throng notify WORDS..  Leave a notice in throng that points back to this terminal
    throng status          Print every project and terminal as JSON
    throng daemon          Run the terminal daemon (started automatically; not needed by hand)
    throng pty-host        Host one terminal for the daemon, over standard input and output
                           (started by an elevated daemon; not needed by hand)
    throng --version       Print the version
";

/// How long the command line waits for the open throng to answer.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => run_daemon(),
        Some("pty-host") => ExitCode::from(u8::try_from(throng_daemon::pty_host::run()).unwrap_or(1)),
        Some("open" | "focus" | "notify" | "status") => run_remote(&args),
        Some("--version" | "-V") => {
            println!("throng {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(flag) if flag.starts_with('-') => {
            eprintln!("throng: unknown option {flag}\n\n{USAGE}");
            ExitCode::from(2)
        }
        folder => run_ui(folder.map(PathBuf::from)),
    }
}

/// The window icon (the packages carry the same picture).
fn icon() -> egui::IconData {
    let png = include_bytes!("../../../packaging/throng.png");
    match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(image) => {
            let image = image.into_rgba8();
            let (width, height) = image.dimensions();
            egui::IconData { rgba: image.into_raw(), width, height }
        }
        Err(e) => {
            tracing::warn!(error = %e, "the window icon could not be read");
            egui::IconData::default()
        }
    }
}

/// `path` as the open throng must read it: absolute, against this process's folder.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    }
}

/// `throng open|focus|notify|status`: one command to the open throng (FR-056). The terminal this
/// is typed in is named by `THRONG_TERMINAL`, which every throng shell carries.
fn run_remote(args: &[String]) -> ExitCode {
    let dirs = match dirs() {
        Ok(dirs) => dirs,
        Err(code) => return code,
    };
    let terminal = std::env::var("THRONG_TERMINAL").ok().and_then(|s| s.parse::<TerminalId>().ok());
    let command = match (args[0].as_str(), &args[1..]) {
        ("open", [path]) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let (path, line, column) = remote::target(path, &cwd);
            Command::Open { path, line, column }
        }
        ("focus", []) => Command::Focus { terminal },
        ("notify", words) if !words.is_empty() => Command::Notify { message: words.join(" "), terminal },
        ("status", []) => Command::Status,
        (what, _) => {
            eprintln!("throng: wrong arguments for `{what}`\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match remote::send(&dirs, &command, REMOTE_TIMEOUT) {
        Ok(Reply::Ok) => ExitCode::SUCCESS,
        Ok(Reply::Status(projects)) => {
            println!("{}", serde_json::to_string_pretty(&projects).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Ok(Reply::Error(message)) | Err(message) => {
            eprintln!("throng: {message}");
            ExitCode::FAILURE
        }
    }
}

fn dirs() -> Result<AppDirs, ExitCode> {
    AppDirs::resolve().map_err(|e| {
        eprintln!("throng: cannot find a home for its files: {e}");
        ExitCode::FAILURE
    })
}

fn run_daemon() -> ExitCode {
    let dirs = match dirs() {
        Ok(dirs) => dirs,
        Err(code) => return code,
    };
    logging::init(&dirs.logs, "daemon.log");
    let mut config = throng_daemon::DaemonConfig::new(dirs);
    // An elevated daemon starts terminals without its rights through a PTY host: this program.
    config.pty_host = std::env::current_exe().ok();
    match throng_daemon::run(config) {
        Ok(()) | Err(throng_daemon::RunError::AlreadyRunning) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "daemon failed");
            ExitCode::FAILURE
        }
    }
}

fn run_ui(folder: Option<PathBuf>) -> ExitCode {
    let dirs = match dirs() {
        Ok(dirs) => dirs,
        Err(code) => return code,
    };
    let log = logging::init(&dirs.logs, "ui.log");
    if let Err(e) = dirs.ensure() {
        eprintln!("throng: cannot create its folders: {e}");
        return ExitCode::FAILURE;
    }

    // One UI per instance: a second one would fight the first over the database.
    let lock = std::fs::File::options().create(true).truncate(false).write(true).open(dirs.ui_lock());
    let _lock = match lock {
        Ok(file) => match file.try_lock() {
            Ok(()) => file,
            Err(_) => {
                // Another throng has this instance: hand it the folder, or bring it forward.
                let command = match &folder {
                    Some(folder) => Command::Open { path: absolute(folder), line: None, column: None },
                    None => Command::Focus { terminal: None },
                };
                return match remote::send(&dirs, &command, REMOTE_TIMEOUT) {
                    Ok(Reply::Ok | Reply::Status(_)) => ExitCode::SUCCESS,
                    Ok(Reply::Error(message)) => {
                        eprintln!("throng: {message}");
                        ExitCode::FAILURE
                    }
                    Err(_) => {
                        eprintln!("throng is already running, but its window could not be reached.");
                        ExitCode::FAILURE
                    }
                };
            }
        },
        Err(e) => {
            eprintln!("throng: cannot open its lock file: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Inside an AppImage the binary lives on a mount that goes when this process ends; the daemon
    // outlives it, so it is started from the AppImage itself and holds a mount of its own.
    let exe = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("throng"));
    let screenshot = std::env::var_os("THRONG_SCREENSHOT").map(|path| {
        let delay =
            std::env::var("THRONG_SCREENSHOT_DELAY_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(2500);
        (PathBuf::from(path), Duration::from_millis(delay))
    });
    let services = Services {
        dirs,
        exe,
        open: folder,
        screenshot,
        pick_folder: throng_app::native_folder_picker(),
        releases: throng_app::github_releases(),
        installer: std::sync::Arc::new(throng_app::PlatformInstaller::new(
            std::env::current_exe().unwrap_or_default(),
            std::env::var_os("APPIMAGE"),
        )),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("throng")
            .with_app_id("throng")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(std::sync::Arc::new(icon())),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let result = eframe::run_native(
        "throng",
        options,
        Box::new(move |cc| {
            ThrongApp::new(&cc.egui_ctx, services)
                .map(|app| Box::new(app) as Box<dyn eframe::App>)
                .map_err(|e| e.to_string().into())
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // A startup failure is never silent.
            tracing::error!(error = %e, "throng failed to start");
            eprintln!("throng could not start: {e}");
            if let Some(log) = log {
                eprintln!("Details are in {}", log.display());
            }
            ExitCode::FAILURE
        }
    }
}
