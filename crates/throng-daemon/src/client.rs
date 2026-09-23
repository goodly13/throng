//! The client side of the daemon protocol, used by the UI.
//!
//! One connection, one writer thread, one reader thread. Requests go out in the order they are made.
//! Replies either wake a blocked caller or arrive as [`ClientEvent::Reply`] for the UI loop to pick
//! up. When the connection drops, every pending request fails at once with "disconnected" — nothing
//! waits out a timeout for a daemon that is already gone.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use interprocess::local_socket::Stream;
use parking_lot::Mutex;
use throng_core::ids::TerminalId;
use throng_core::terminal::ExitStatus;
use throng_platform::dirs::AppDirs;
use throng_protocol::{
    ClientMsg, FrameError, NO_REPLY, PROTOCOL_VERSION, Reply, Request, ServerMsg, chunked_writes, read_frame,
    write_frame,
};

use crate::endpoint::Endpoint;

/// Something the daemon told us.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientEvent {
    Output {
        terminal: TerminalId,
        offset: u64,
        data: Vec<u8>,
    },
    Exited {
        terminal: TerminalId,
        status: ExitStatus,
    },
    Resync {
        terminal: TerminalId,
    },
    /// The answer to a request made with [`Client::send`].
    Reply {
        id: u64,
        result: Result<Reply, String>,
    },
    /// The connection is gone. Pending requests have already failed.
    Disconnected {
        reason: String,
    },
}

/// Why connecting failed.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("no daemon is listening")]
    NotRunning,
    #[error("the terminal host (pid {pid}) speaks protocol {daemon}, this build speaks {PROTOCOL_VERSION}")]
    VersionMismatch { daemon: u32, pid: u32 },
    #[error("could not start the terminal host: {0}")]
    Spawn(std::io::Error),
    #[error("the terminal host did not answer: {0}")]
    Handshake(String),
}

/// Why a request failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RequestError {
    #[error("the terminal host is not connected")]
    Disconnected,
    #[error("the terminal host did not answer in time")]
    Timeout,
    #[error("{0}")]
    Refused(String),
}

type Waiters = Arc<Mutex<HashMap<u64, Sender<Result<Reply, String>>>>>;

/// A connection to the daemon. Dropping it closes the connection; the daemon then detaches its views.
pub struct Client {
    stream: Arc<Stream>,
    outgoing: Sender<ClientMsg>,
    events: Receiver<ClientEvent>,
    waiters: Waiters,
    next_id: AtomicU64,
    connected: Arc<std::sync::atomic::AtomicBool>,
    /// Dropped: the reader stops at its next look.
    closing: Arc<std::sync::atomic::AtomicBool>,
    pub daemon_pid: u32,
    pub daemon_build: String,
}

impl Client {
    /// Connect to a running daemon. `notify` is called (from a background thread) whenever an event
    /// is queued, e.g. to wake the UI.
    pub fn connect(
        endpoint: &Endpoint,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, ConnectError> {
        let stream = Arc::new(endpoint.connect().map_err(|_| ConnectError::NotRunning)?);
        let mut recv: &Stream = &stream;
        let mut send: &Stream = &stream;
        let hello = ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: crate::BUILD.into(),
            pid: std::process::id(),
        };
        write_frame(&mut send, &hello).map_err(|e| ConnectError::Handshake(e.to_string()))?;
        let welcome: ServerMsg = read_frame(&mut recv).map_err(|e| ConnectError::Handshake(e.to_string()))?;
        let ServerMsg::Welcome { protocol, build, pid } = welcome else {
            return Err(ConnectError::Handshake("unexpected first message".into()));
        };
        if protocol != PROTOCOL_VERSION {
            return Err(ConnectError::VersionMismatch { daemon: protocol, pid });
        }

        let (outgoing, outgoing_rx) = crossbeam_channel::unbounded::<ClientMsg>();
        let (events_tx, events) = crossbeam_channel::unbounded::<ClientEvent>();
        let waiters: Waiters = Arc::default();
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notify = Arc::new(notify);

        {
            let stream = Arc::clone(&stream);
            thread::Builder::new()
                .name("throng-client-write".into())
                .spawn(move || {
                    let mut send: &Stream = &stream;
                    for message in outgoing_rx {
                        if write_frame(&mut send, &message).is_err() {
                            break;
                        }
                    }
                })
                .map_err(ConnectError::Spawn)?;
        }

        {
            let waiters = Arc::clone(&waiters);
            let connected = Arc::clone(&connected);
            let closing = Arc::clone(&closing);
            let stream = Arc::clone(&stream);
            thread::Builder::new()
                .name("throng-client-read".into())
                .spawn(move || {
                    let mut recv: &Stream = &stream;
                    let reason = loop {
                        if closing.load(Ordering::Acquire) {
                            break "client dropped".to_owned();
                        }
                        let event = match read_frame::<ServerMsg>(&mut recv) {
                            Ok(ServerMsg::Output { terminal, offset, data }) => {
                                ClientEvent::Output { terminal, offset, data }
                            }
                            Ok(ServerMsg::Exited { terminal, status }) => {
                                ClientEvent::Exited { terminal, status }
                            }
                            Ok(ServerMsg::Resync { terminal }) => ClientEvent::Resync { terminal },
                            Ok(ServerMsg::Reply { id, result }) => {
                                if let Some(waiter) = waiters.lock().remove(&id) {
                                    let _ = waiter.send(result);
                                    continue;
                                }
                                ClientEvent::Reply { id, result }
                            }
                            Ok(ServerMsg::Welcome { .. }) => continue,
                            Err(FrameError::Closed) => {
                                break "the terminal host closed the connection".to_owned();
                            }
                            Err(e) => break e.to_string(),
                        };
                        if events_tx.send(event).is_err() {
                            break "client dropped".to_owned();
                        }
                        notify();
                    };
                    connected.store(false, Ordering::Release);
                    for (_, waiter) in waiters.lock().drain() {
                        let _ = waiter.send(Err(DISCONNECTED.into()));
                    }
                    let _ = events_tx.send(ClientEvent::Disconnected { reason });
                    notify();
                })
                .map_err(ConnectError::Spawn)?;
        }

        Ok(Self {
            stream,
            outgoing,
            events,
            waiters,
            next_id: AtomicU64::new(1),
            connected,
            closing,
            daemon_pid: pid,
            daemon_build: build,
        })
    }

    /// Connect, starting the daemon (`exe daemon`, detached) if none is listening.
    pub fn connect_or_spawn(
        dirs: &AppDirs,
        exe: &Path,
        notify: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<Self, ConnectError> {
        let endpoint = Endpoint::for_dirs(dirs);
        match Self::connect(&endpoint, notify.clone()) {
            Err(ConnectError::NotRunning) => {}
            other => return other,
        }
        dirs.ensure().map_err(ConnectError::Spawn)?;
        let mut env = Vec::new();
        if let Some(home) = &dirs.home {
            env.push((throng_platform::dirs::HOME_ENV.to_owned(), home.to_string_lossy().into_owned()));
        }
        throng_platform::process::spawn_detached(exe, ["daemon"], &env).map_err(ConnectError::Spawn)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Self::connect(&endpoint, notify.clone()) {
                Err(ConnectError::NotRunning) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25));
                }
                other => return other,
            }
        }
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Events received so far.
    #[must_use]
    pub fn events(&self) -> &Receiver<ClientEvent> {
        &self.events
    }

    /// Send a request whose answer arrives later as [`ClientEvent::Reply`]. Returns its id.
    pub fn send(&self, request: Request) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.outgoing.send(ClientMsg::Request { id, request });
        id
    }

    /// Send a request that needs no answer (keystrokes, resizes, detaches).
    pub fn post(&self, request: Request) {
        let _ = self.outgoing.send(ClientMsg::Request { id: NO_REPLY, request });
    }

    /// Send input, split so no frame is ever too large.
    pub fn write(&self, terminal: TerminalId, data: &[u8]) {
        for request in chunked_writes(terminal, data) {
            self.post(request);
        }
    }

    /// Send a request and wait for its answer.
    pub fn request(&self, request: Request, timeout: Duration) -> Result<Reply, RequestError> {
        if !self.is_connected() {
            return Err(RequestError::Disconnected);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.waiters.lock().insert(id, tx);
        if self.outgoing.send(ClientMsg::Request { id, request }).is_err() {
            self.waiters.lock().remove(&id);
            return Err(RequestError::Disconnected);
        }
        // The reader may have drained the waiters just before we registered.
        if !self.is_connected() {
            self.waiters.lock().remove(&id);
            return Err(RequestError::Disconnected);
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(message)) if message == DISCONNECTED => Err(RequestError::Disconnected),
            Ok(Err(message)) => Err(RequestError::Refused(message)),
            Err(_) => {
                self.waiters.lock().remove(&id);
                if self.is_connected() { Err(RequestError::Timeout) } else { Err(RequestError::Disconnected) }
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        crate::endpoint::shutdown(&self.stream);
        // On Windows waking a read does not stop the next one, and a read begun just before the
        // flag was seen would wait for the daemon. Wake it again until the reader has gone, so the
        // connection closes and the daemon learns at once (bounded).
        #[cfg(windows)]
        {
            let deadline = Instant::now() + Duration::from_millis(500);
            while self.connected.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
                crate::endpoint::shutdown(&self.stream);
            }
        }
    }
}

const DISCONNECTED: &str = "\u{0}disconnected";
