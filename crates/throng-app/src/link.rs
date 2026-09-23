//! The UI's connection to the terminal daemon, and how its loss is presented.
//!
//! A dropped connection is not yet death: the link reconnects in the background (starting a fresh
//! daemon if none is listening), and only after a grace period does it raise one notice. Retries
//! back off, so a daemon that cannot start is not respawned in a tight loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use throng_daemon::{Client, ClientEvent, ConnectError};
use throng_platform::dirs::AppDirs;

/// How long a lost connection may stay lost before the user is told.
pub const GRACE: Duration = Duration::from_millis(1200);

/// Where the link stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Connected,
    /// Lost and retrying.
    Lost {
        since: Instant,
        reason: String,
    },
    /// Needs the user: a daemon from another build holds the endpoint.
    Blocked {
        reason: String,
        daemon_pid: u32,
    },
}

/// What happened since the last poll.
#[derive(Debug)]
pub enum LinkEvent {
    Connected,
    Lost,
    Daemon(ClientEvent),
}

type Notify = Arc<dyn Fn() + Send + Sync>;

/// The link.
pub struct DaemonLink {
    dirs: AppDirs,
    exe: PathBuf,
    notify: Notify,
    client: Option<Client>,
    pub state: LinkState,
    attempt: Option<Receiver<Result<Client, ConnectError>>>,
    next_try: Instant,
    backoff: Duration,
}

impl DaemonLink {
    /// Start connecting in the background.
    pub fn start(dirs: AppDirs, exe: PathBuf, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let mut link = Self {
            dirs,
            exe,
            notify: Arc::new(notify),
            client: None,
            state: LinkState::Connecting,
            attempt: None,
            next_try: Instant::now(),
            backoff: Duration::from_millis(250),
        };
        link.try_connect();
        link
    }

    fn try_connect(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let dirs = self.dirs.clone();
        let exe = self.exe.clone();
        let notify = Arc::clone(&self.notify);
        let spawned = thread::Builder::new().name("throng-connect".into()).spawn(move || {
            let wake = Arc::clone(&notify);
            let result = Client::connect_or_spawn(&dirs, &exe, move || wake());
            let _ = tx.send(result);
            notify();
        });
        if spawned.is_ok() {
            self.attempt = Some(rx);
        }
    }

    #[must_use]
    pub fn client(&self) -> Option<&Client> {
        self.client.as_ref().filter(|c| c.is_connected())
    }

    /// Whether the user should currently be told the terminal host is unavailable.
    #[must_use]
    pub fn outage(&self) -> Option<String> {
        match &self.state {
            LinkState::Lost { since, reason } if since.elapsed() >= GRACE => {
                Some(format!("The terminal host is not responding ({reason}). Reconnecting…"))
            }
            LinkState::Blocked { reason, .. } => Some(reason.clone()),
            _ => None,
        }
    }

    /// Stop the daemon that is blocking this build and start ours. Its terminals end.
    pub fn replace_blocking_daemon(&mut self) {
        if let LinkState::Blocked { daemon_pid, .. } = self.state {
            throng_platform::process::terminate(daemon_pid);
            self.state = LinkState::Connecting;
            self.next_try = Instant::now() + Duration::from_millis(300);
        }
    }

    /// Drain connection changes and daemon events.
    pub fn poll(&mut self) -> Vec<LinkEvent> {
        let mut events = Vec::new();
        if let Some(rx) = &self.attempt
            && let Ok(result) = rx.try_recv()
        {
            self.attempt = None;
            match result {
                Ok(client) => {
                    tracing::info!(pid = client.daemon_pid, build = %client.daemon_build, "connected to daemon");
                    self.client = Some(client);
                    self.state = LinkState::Connected;
                    self.backoff = Duration::from_millis(250);
                    events.push(LinkEvent::Connected);
                }
                Err(ConnectError::VersionMismatch { daemon, pid }) => {
                    self.state = LinkState::Blocked {
                        reason: format!(
                            "A terminal host from a different throng version is running (pid {pid}, protocol {daemon}). \
                             Its terminals keep running; restart it to use terminals here."
                        ),
                        daemon_pid: pid,
                    };
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not connect to daemon");
                    let since = match &self.state {
                        LinkState::Lost { since, .. } => *since,
                        _ => Instant::now(),
                    };
                    self.state = LinkState::Lost { since, reason: e.to_string() };
                    self.next_try = Instant::now() + self.backoff;
                    self.backoff = (self.backoff * 2).min(Duration::from_secs(15));
                }
            }
        }

        if let Some(client) = &self.client {
            let mut lost = None;
            for event in client.events().try_iter() {
                match event {
                    ClientEvent::Disconnected { reason } => lost = Some(reason),
                    other => events.push(LinkEvent::Daemon(other)),
                }
            }
            if let Some(reason) = lost {
                tracing::warn!(%reason, "daemon connection lost");
                self.client = None;
                self.state = LinkState::Lost { since: Instant::now(), reason };
                self.next_try = Instant::now() + Duration::from_millis(100);
                events.push(LinkEvent::Lost);
            }
        }

        let waiting = matches!(self.state, LinkState::Lost { .. } | LinkState::Connecting);
        if waiting && self.attempt.is_none() && Instant::now() >= self.next_try {
            self.try_connect();
        }
        events
    }

    /// When the UI should wake up next without input (retry timers, grace expiry).
    #[must_use]
    pub fn next_wake(&self) -> Option<Duration> {
        match &self.state {
            LinkState::Lost { since, .. } => {
                let grace_left = GRACE.saturating_sub(since.elapsed());
                let retry_left = self.next_try.saturating_duration_since(Instant::now());
                Some(grace_left.max(Duration::from_millis(50)).min(retry_left.max(Duration::from_millis(50))))
            }
            LinkState::Connecting => Some(Duration::from_millis(100)),
            _ => None,
        }
    }
}
