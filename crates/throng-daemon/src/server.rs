//! The daemon: accepts clients, owns sessions, and exits when there is nothing left to own.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::{Listener as _, Stream as _};
use parking_lot::Mutex;
use throng_core::ids::TerminalId;
use throng_platform::dirs::AppDirs;
use throng_protocol::{
    ClientMsg, FrameError, NO_REPLY, PROTOCOL_VERSION, Reply, Request, ServerMsg, read_frame, write_frame,
};

use crate::endpoint::Endpoint;
use crate::registry::{ClientId, ClientQueue, Registry};
use crate::session::{DEFAULT_TAIL_BYTES, Session};

/// How often the accept loop looks for new clients and for a stop request.
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// How the daemon runs.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub dirs: AppDirs,
    /// Exit after this long with no live sessions and no clients.
    pub idle_exit: Duration,
    /// Exit after this long with only unread exit records left (they are then lost).
    pub unread_exit: Duration,
    pub tail_bytes: usize,
    /// Identifies the build, for the handshake.
    pub build: String,
}

impl DaemonConfig {
    #[must_use]
    pub fn new(dirs: AppDirs) -> Self {
        Self {
            dirs,
            idle_exit: Duration::from_secs(10),
            unread_exit: Duration::from_secs(3600),
            tail_bytes: DEFAULT_TAIL_BYTES,
            build: crate::BUILD.to_owned(),
        }
    }
}

/// Why the daemon did not run.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("another daemon already serves this instance")]
    AlreadyRunning,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

struct Daemon {
    config: DaemonConfig,
    endpoint: Endpoint,
    registry: Arc<Registry>,
    sessions: Mutex<HashMap<TerminalId, Arc<Session>>>,
    stopping: AtomicBool,
}

/// Run the daemon until it is told to stop or goes idle. Blocks.
pub fn run(config: DaemonConfig) -> Result<(), RunError> {
    config.dirs.ensure()?;
    let mut lock =
        File::options().create(true).truncate(false).write(true).open(config.dirs.daemon_lock())?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(RunError::AlreadyRunning),
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    lock.set_len(0)?;
    writeln!(lock, "{}", std::process::id())?;

    let endpoint = Endpoint::for_dirs(&config.dirs);
    let listener = endpoint.listen()?;
    tracing::info!(pid = std::process::id(), ?endpoint, "daemon listening");

    let daemon = Arc::new(Daemon {
        config,
        endpoint: endpoint.clone(),
        registry: Arc::new(Registry::default()),
        sessions: Mutex::new(HashMap::new()),
        stopping: AtomicBool::new(false),
    });

    {
        let daemon = Arc::clone(&daemon);
        thread::Builder::new().name("daemon-idle".into()).spawn(move || daemon.watch_idle())?;
    }

    while !daemon.stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(stream) => {
                // The listener is non-blocking; on macOS and the BSDs an accepted socket inherits
                // that, which would make every blocking read fail at once and drop the client.
                if let Err(e) = stream.set_nonblocking(false) {
                    tracing::warn!(error = %e, "could not make a client connection blocking");
                    continue;
                }
                let daemon = Arc::clone(&daemon);
                let spawned =
                    thread::Builder::new().name("daemon-client".into()).spawn(move || daemon.serve(stream));
                if let Err(e) = spawned {
                    tracing::error!(error = %e, "could not start a client thread");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                thread::sleep(ACCEPT_POLL);
            }
        }
    }

    daemon.end_all_sessions();
    daemon.registry.shutdown_all();
    drop(listener);
    endpoint.cleanup();
    tracing::info!("daemon stopped");
    drop(lock);
    Ok(())
}

impl Daemon {
    fn live_sessions(&self) -> usize {
        self.sessions.lock().values().filter(|s| !s.is_exited()).count()
    }

    fn watch_idle(&self) {
        let mut idle_since: Option<Instant> = None;
        while !self.stopping.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(200));
            if !self.endpoint.reachable() {
                // The socket file was deleted (a cleaned temp directory, a removed instance). No
                // client can ever reach these terminals again, so end them rather than orphan them.
                tracing::warn!("daemon socket removed; ending all terminals and exiting");
                self.stop();
                return;
            }
            let clients = self.registry.count();
            let live = self.live_sessions();
            let unread = self.sessions.lock().len() - live;
            if clients > 0 || live > 0 {
                idle_since = None;
                continue;
            }
            let since = *idle_since.get_or_insert_with(Instant::now);
            let limit = if unread > 0 { self.config.unread_exit } else { self.config.idle_exit };
            if since.elapsed() >= limit {
                tracing::info!(unread, "daemon idle; exiting");
                self.stop();
                return;
            }
        }
    }

    /// Stop accepting; the accept loop polls the flag.
    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    /// Kill every live session and wait (bounded) for each to be reaped.
    fn end_all_sessions(&self) {
        let sessions: Vec<Arc<Session>> = self.sessions.lock().values().cloned().collect();
        for session in &sessions {
            session.kill();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && sessions.iter().any(|s| !s.is_exited()) {
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn serve(self: Arc<Self>, stream: Stream) {
        let stream = Arc::new(stream);
        let mut recv: &Stream = &stream;
        let mut send: &Stream = &stream;
        let hello: Result<ClientMsg, FrameError> = read_frame(&mut recv);
        let welcome = ServerMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: self.config.build.clone(),
            pid: std::process::id(),
        };
        match hello {
            Ok(ClientMsg::Hello { protocol, .. }) => {
                if write_frame(&mut send, &welcome).is_err() || protocol != PROTOCOL_VERSION {
                    return;
                }
            }
            Ok(_) | Err(_) => return,
        }
        if self.stopping.load(Ordering::Acquire) {
            return;
        }

        let (client, queue, rx) = self.registry.add(Arc::clone(&stream));
        let writer = {
            let queue = Arc::clone(&queue);
            let stream = Arc::clone(&stream);
            thread::Builder::new().name("daemon-client-write".into()).spawn(move || {
                let mut send: &Stream = &stream;
                write_loop(&mut send, &queue, &rx);
                // A write failure means the client is gone; wake our reader too.
                crate::endpoint::shutdown(&stream);
            })
        };

        loop {
            match read_frame::<ClientMsg>(&mut recv) {
                Ok(ClientMsg::Request { id, request }) => {
                    let result = self.handle(client, id, request);
                    if let Some(result) = result {
                        if id != NO_REPLY {
                            self.registry.send(client, ServerMsg::Reply { id, result });
                        } else if let Err(message) = result {
                            tracing::debug!(%message, "unanswered request failed");
                        }
                    }
                }
                Ok(ClientMsg::Hello { .. }) => {}
                Err(FrameError::Closed) => break,
                Err(e) => {
                    // A malformed frame ends this client's connection, never the daemon.
                    tracing::warn!(error = %e, "closing a client that sent a bad frame");
                    break;
                }
            }
        }

        self.registry.remove(client);
        queue.close();
        let sessions: Vec<Arc<Session>> = self.sessions.lock().values().cloned().collect();
        for session in sessions {
            session.detach(client);
        }
        drop(queue);
        if let Ok(writer) = writer {
            let _ = writer.join();
        }
    }

    /// Handle one request. `None` means the reply was already sent (attach snapshots are sent under
    /// the session lock to order them against output).
    fn handle(
        self: &Arc<Self>,
        client: ClientId,
        id: u64,
        request: Request,
    ) -> Option<Result<Reply, String>> {
        let session = |terminal: TerminalId| self.sessions.lock().get(&terminal).cloned();
        match request {
            Request::Ping => Some(Ok(Reply::Pong)),
            Request::Spawn(spec) => {
                if session(spec.terminal).is_some_and(|existing| !existing.is_exited()) {
                    return Some(Err("A terminal with this id is already running.".into()));
                }
                let daemon = Arc::downgrade(self);
                let spawned =
                    Session::spawn(&spec, self.config.tail_bytes, Arc::clone(&self.registry), move |id| {
                        if let Some(daemon) = daemon.upgrade() {
                            daemon.session_ended(id);
                        }
                    });
                match spawned {
                    Ok(session) => {
                        self.sessions.lock().insert(spec.terminal, Arc::clone(&session));
                        tracing::info!(terminal = %spec.terminal, pid = ?session.pid, "spawned");
                        session.attach(client, id, spec.cols, spec.rows, &self.registry);
                        None
                    }
                    Err(e) => Some(Err(e.to_string())),
                }
            }
            Request::Attach { terminal, cols, rows } => match session(terminal) {
                Some(session) => {
                    let snapshot = session.attach(client, id, cols, rows, &self.registry);
                    if snapshot.alt_screen && snapshot.exited.is_none() {
                        session.nudge();
                    }
                    None
                }
                None => Some(Err(throng_protocol::NO_SUCH_TERMINAL.into())),
            },
            Request::Detach { terminal } => {
                if let Some(session) = session(terminal) {
                    session.detach(client);
                }
                Some(Ok(Reply::Ok))
            }
            Request::Write { terminal, data } => match session(terminal) {
                Some(session) => Some(session.write(&data).map(|()| Reply::Ok).map_err(|e| e.to_string())),
                None => Some(Err(throng_protocol::NO_SUCH_TERMINAL.into())),
            },
            Request::Resize { terminal, cols, rows } => {
                if let Some(session) = session(terminal) {
                    session.resize(client, cols, rows);
                }
                Some(Ok(Reply::Ok))
            }
            Request::Kill { terminal } => {
                if let Some(session) = session(terminal) {
                    session.kill();
                }
                Some(Ok(Reply::Ok))
            }
            Request::Forget { terminal } => {
                let mut sessions = self.sessions.lock();
                if sessions.get(&terminal).is_some_and(|s| s.is_exited()) {
                    sessions.remove(&terminal);
                }
                Some(Ok(Reply::Ok))
            }
            Request::List => {
                let infos = self.sessions.lock().values().map(|s| s.info()).collect();
                Some(Ok(Reply::Terminals(infos)))
            }
            Request::CloseIdle { terminals } => {
                let mut closed = Vec::new();
                for terminal in terminals {
                    if let Some(session) = session(terminal)
                        && !session.is_exited()
                        && !session.is_busy()
                    {
                        session.kill();
                        closed.push(terminal);
                    }
                }
                Some(Ok(Reply::Closed(closed)))
            }
            Request::Shutdown { kill_all } => {
                if !kill_all && self.live_sessions() > 0 {
                    return Some(Err("Terminals are still running.".into()));
                }
                let daemon = Arc::clone(self);
                let _ = thread::Builder::new().name("daemon-shutdown".into()).spawn(move || {
                    // Let the reply go out first.
                    thread::sleep(Duration::from_millis(50));
                    daemon.stop();
                });
                Some(Ok(Reply::Ok))
            }
        }
    }

    /// A session's shell exited. A user-ended session is forgotten at once; one that ended on its own
    /// is kept (exit status and tail) until a client has read it.
    fn session_ended(&self, terminal: TerminalId) {
        let mut sessions = self.sessions.lock();
        if let Some(session) = sessions.get(&terminal) {
            let status = session.exit_status();
            tracing::info!(%terminal, ?status, "session ended");
            if status.is_some_and(|s| s.user_killed) {
                sessions.remove(&terminal);
            }
        }
    }
}

fn write_loop(
    send: &mut impl std::io::Write,
    queue: &ClientQueue,
    rx: &crossbeam_channel::Receiver<ServerMsg>,
) {
    loop {
        let message = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(message) => message,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if queue.is_closed() {
                    return;
                }
                for terminal in queue.take_resyncs() {
                    if write_frame(send, &ServerMsg::Resync { terminal }).is_err() {
                        return;
                    }
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        let failed = write_frame(send, &message).is_err();
        queue.written(&message);
        if failed {
            return;
        }
        if rx.is_empty() {
            for terminal in queue.take_resyncs() {
                if write_frame(send, &ServerMsg::Resync { terminal }).is_err() {
                    return;
                }
            }
        }
    }
}
