//! The update check (FR-052): at start and once a day, ask where releases are published for the
//! latest one. What counts as newer, and what the user is told, is `throng_core::update`'s and the
//! app's; this only fetches, off the UI thread.

use std::time::Instant;

use crossbeam_channel::{Receiver, TryRecvError};
use egui::Context;
use throng_core::update::{CHECK_INTERVAL, LATEST_RELEASE_API, Release, parse_release};

/// Called with the latest release, or `None` when it could not be learned.
pub type ReleaseCallback = Box<dyn FnOnce(Option<Release>) + Send>;

/// Looks up the latest release and hands it to the callback, from any thread.
pub type ReleaseSource = Box<dyn Fn(ReleaseCallback)>;

/// The latest release on GitHub. The request carries throng's name and version and nothing else.
#[must_use]
pub fn github_releases() -> ReleaseSource {
    Box::new(|done| {
        let mut request = ehttp::Request::get(LATEST_RELEASE_API);
        request.headers.insert("Accept", "application/vnd.github+json");
        request.headers.insert("User-Agent", concat!("throng/", env!("CARGO_PKG_VERSION")));
        ehttp::fetch(request, move |result| {
            let release = match result {
                Ok(response) if response.ok => response.text().and_then(parse_release),
                Ok(response) => {
                    tracing::info!(status = response.status, "update check: no release answer");
                    None
                }
                Err(error) => {
                    tracing::info!(%error, "update check: request failed");
                    None
                }
            };
            done(release);
        });
    })
}

/// When the next check is due, and the one in flight.
pub struct UpdateCheck {
    source: ReleaseSource,
    due: Instant,
    pending: Option<Receiver<Option<Release>>>,
}

impl UpdateCheck {
    #[must_use]
    pub fn new(source: ReleaseSource) -> Self {
        Self { source, due: Instant::now(), pending: None }
    }

    /// Start a check when one is due and `enabled`; returns the latest release once an answer
    /// arrives.
    pub fn poll(&mut self, ctx: &Context, enabled: bool) -> Option<Release> {
        if let Some(pending) = &self.pending {
            return match pending.try_recv() {
                Ok(release) => {
                    self.pending = None;
                    release
                }
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    None
                }
            };
        }
        if !enabled {
            return None;
        }
        let now = Instant::now();
        if now < self.due {
            ctx.request_repaint_after(self.due - now);
            return None;
        }
        self.due = now + CHECK_INTERVAL;
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = ctx.clone();
        (self.source)(Box::new(move |release| {
            let _ = tx.send(release);
            ctx.request_repaint();
        }));
        self.pending = Some(rx);
        None
    }
}
