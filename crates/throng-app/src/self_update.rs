//! Installing a newer release from inside throng (FR-054): *Install and Restart* on the update
//! notice. The app asks an [`Installer`] whether this install can replace itself, and has it
//! download, check and install the release off the UI thread. The real one is
//! [`PlatformInstaller`]; tests hand the app one that touches nothing.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crossbeam_channel::{Receiver, TryRecvError};
use egui::Context;
use throng_core::update::{Release, SHA256SUMS, Version};
use throng_platform::self_update::Target;

/// The largest file the updater downloads. A package is tens of MB; this only stops a runaway.
const DOWNLOAD_LIMIT: u64 = 512 * 1024 * 1024;

/// What an install in progress is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Downloading,
    Verifying,
}

/// Replaces this install with a newer release.
pub trait Installer: Send + Sync {
    /// Whether this install can replace itself with `release`, which is `version`: the install is
    /// of a kind that can, and the release carries the files it needs.
    fn can_install(&self, release: &Release, version: Version) -> bool;

    /// Download, check and install. Runs off the UI thread and reports each step as it starts.
    fn install(&self, release: &Release, version: Version, step: &dyn Fn(Step)) -> Result<(), String>;

    /// Open the installed throng once this process has exited.
    fn relaunch(&self) -> Result<(), String>;
}

/// An install that never replaces itself: *Download* is all it offers.
pub struct NoInstaller;

impl Installer for NoInstaller {
    fn can_install(&self, _: &Release, _: Version) -> bool {
        false
    }

    fn install(&self, _: &Release, _: Version, _: &dyn Fn(Step)) -> Result<(), String> {
        Err("This install does not update itself.".into())
    }

    fn relaunch(&self) -> Result<(), String> {
        Err("This install does not update itself.".into())
    }
}

/// The installer for the install this process runs from, worked out the first time it is asked.
pub struct PlatformInstaller {
    exe: PathBuf,
    appimage: Option<std::ffi::OsString>,
    target: OnceLock<Option<Target>>,
}

impl PlatformInstaller {
    /// `exe` is this program; `appimage` is `APPIMAGE`, set when it runs from an AppImage.
    #[must_use]
    pub fn new(exe: PathBuf, appimage: Option<std::ffi::OsString>) -> Self {
        Self { exe, appimage, target: OnceLock::new() }
    }

    fn target(&self) -> Option<&Target> {
        self.target
            .get_or_init(|| match Target::detect(&self.exe, self.appimage.as_deref()) {
                Ok(target) => Some(target),
                Err(reason) => {
                    tracing::info!(%reason, "updates are downloaded by hand");
                    None
                }
            })
            .as_ref()
    }
}

impl Installer for PlatformInstaller {
    fn can_install(&self, release: &Release, version: Version) -> bool {
        let Some(target) = self.target() else { return false };
        release.asset(&target.package_name(version)).is_some()
            && (!target.needs_checksums() || release.asset(SHA256SUMS).is_some())
    }

    fn install(&self, release: &Release, version: Version, step: &dyn Fn(Step)) -> Result<(), String> {
        let target = self.target().ok_or("This install does not update itself.")?;
        let name = target.package_name(version);
        let asset = release.asset(&name).ok_or_else(|| format!("The release has no {name}."))?;
        step(Step::Downloading);
        let package = download(&asset.url)?;
        let sums = if target.needs_checksums() {
            let asset =
                release.asset(SHA256SUMS).ok_or_else(|| format!("The release has no {SHA256SUMS}."))?;
            Some(String::from_utf8_lossy(&download(&asset.url)?).into_owned())
        } else {
            None
        };
        target.install(&package, sums.as_deref(), version, &|| step(Step::Verifying))
    }

    fn relaunch(&self) -> Result<(), String> {
        let target = self.target().ok_or("This install does not update itself.")?;
        target.relaunch(std::process::id())
    }
}

/// A release file, over HTTPS. The request carries throng's name and version and nothing else.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let failed = |e: ureq::Error| format!("The download failed: {e}");
    let mut response = ureq::get(url)
        .header("User-Agent", concat!("throng/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(failed)?;
    response.body_mut().with_config().limit(DOWNLOAD_LIMIT).read_to_vec().map_err(failed)
}

/// An install running on its own thread.
pub struct Installing {
    pub version: Version,
    step: Step,
    events: Receiver<Event>,
}

enum Event {
    Step(Step),
    Done(Result<(), String>),
}

impl Installing {
    /// Start installing `release` on a thread of its own.
    pub fn start(ctx: &Context, installer: Arc<dyn Installer>, release: Release, version: Version) -> Self {
        let (tx, events) = crossbeam_channel::unbounded();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("throng-update".into()).spawn(move || {
            let send = |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };
            let result = installer.install(&release, version, &|step| send(Event::Step(step)));
            send(Event::Done(result));
        });
        // A thread that could not start dropped its sender: `poll` reports that as a failure.
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the update thread could not start");
        }
        Self { version, step: Step::Downloading, events }
    }

    /// What it is doing now.
    #[must_use]
    pub fn step(&self) -> Step {
        self.step
    }

    /// The outcome, once it has one.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        loop {
            match self.events.try_recv() {
                Ok(Event::Step(step)) => self.step = step,
                Ok(Event::Done(result)) => return Some(result),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    return Some(Err("The install stopped without finishing.".into()));
                }
            }
        }
    }
}
