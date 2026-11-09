//! Remote control (FR-056): the window listens on an endpoint of its own, beside the daemon's, and
//! answers one JSON line per connection. The `throng` command line speaks it from any shell, so a
//! program running in a terminal can open a file, bring its panel forward, or leave a notice.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::{Listener as _, Stream as _};
use serde::{Deserialize, Serialize};
use throng_core::ids::{ProjectId, TerminalId};
use throng_daemon::Endpoint;
use throng_platform::dirs::AppDirs;

/// How long the window has to answer a command before its connection says it did not.
const ANSWER: Duration = Duration::from_secs(5);
/// How often the accept loop looks for a connection and for a stop request.
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// What the command line can ask of the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Open a file in the project that holds it, at a position, or a folder as a project, and
    /// bring the window forward.
    Open { path: PathBuf, line: Option<u32>, column: Option<u32> },
    /// Bring a terminal's panel to the front, or the window alone when no terminal is named.
    Focus { terminal: Option<TerminalId> },
    /// Raise one notice, pointing back at the terminal it came from.
    Notify { message: String, terminal: Option<TerminalId> },
    /// Every project and terminal.
    Status,
}

/// The window's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Ok,
    Error(String),
    Status(Vec<ProjectStatus>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStatus {
    pub id: ProjectId,
    pub name: String,
    pub root: PathBuf,
    pub active: bool,
    pub terminals: Vec<TerminalStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalStatus {
    pub terminal: TerminalId,
    /// "Project › Tab › Panel".
    pub label: String,
    pub busy: bool,
    pub attention: bool,
    /// The title the program set, if any.
    pub title: Option<String>,
    pub directory: Option<PathBuf>,
}

/// The window's endpoint for an instance: beside the daemon's, never another instance's.
#[must_use]
pub fn endpoint(dirs: &AppDirs) -> Endpoint {
    if cfg!(windows) { Endpoint::Pipe(dirs.ui_pipe_name()) } else { Endpoint::Socket(dirs.ui_socket()) }
}

/// `path[:line[:col]]` as typed on the command line, made absolute against `cwd`.
#[must_use]
pub fn target(text: &str, cwd: &Path) -> (PathBuf, Option<u32>, Option<u32>) {
    let (path, line, column) =
        throng_core::links::split_position(text).unwrap_or_else(|| (text.to_owned(), None, None));
    let path = PathBuf::from(path);
    let path = if path.is_absolute() { path } else { cwd.join(path) };
    (path, line, column)
}

/// A command the window has yet to answer; the connection waits on `respond`.
pub struct Incoming {
    pub command: Command,
    pub respond: Sender<Reply>,
}

/// The window's end: accepts connections and queues their commands for the UI thread.
pub struct RemoteServer {
    incoming: Receiver<Incoming>,
    stop: Arc<AtomicBool>,
    endpoint: Endpoint,
}

impl RemoteServer {
    /// Listen for commands. `notify` is called (from a background thread) when one arrives, e.g.
    /// to wake the UI. The caller holds the UI lock, which is what makes replacing a stale socket
    /// file safe.
    pub fn start(dirs: &AppDirs, notify: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        let endpoint = endpoint(dirs);
        let listener = endpoint.listen()?;
        let (commands, incoming) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let stopping = Arc::clone(&stop);
        thread::Builder::new().name("throng-remote".into()).spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok(stream) => {
                        // The listener is non-blocking; on macOS an accepted socket inherits that.
                        if stream.set_nonblocking(false).is_err() {
                            continue;
                        }
                        let (commands, notify) = (commands.clone(), Arc::clone(&notify));
                        let spawned = thread::Builder::new()
                            .name("throng-remote-client".into())
                            .spawn(move || serve(&stream, &commands, &notify));
                        if let Err(e) = spawned {
                            tracing::warn!(error = %e, "could not start a remote command thread");
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
                    Err(e) => {
                        tracing::warn!(error = %e, "the command endpoint could not accept a connection");
                        thread::sleep(ACCEPT_POLL);
                    }
                }
            }
        })?;
        Ok(Self { incoming, stop, endpoint })
    }

    /// The commands that arrived since the last look.
    #[must_use]
    pub fn poll(&self) -> Vec<Incoming> {
        self.incoming.try_iter().collect()
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.endpoint.cleanup();
    }
}

/// Answer the one command a connection carries.
fn serve(stream: &Stream, commands: &Sender<Incoming>, notify: &Arc<dyn Fn() + Send + Sync>) {
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).is_err() {
        return;
    }
    let reply = match serde_json::from_str::<Command>(line.trim()) {
        Err(e) => Reply::Error(format!("Not a throng command: {e}")),
        Ok(command) => {
            let (respond, answer) = crossbeam_channel::bounded(1);
            if commands.send(Incoming { command, respond }).is_err() {
                Reply::Error("throng is closing.".into())
            } else {
                notify();
                answer.recv_timeout(ANSWER).unwrap_or_else(|_| Reply::Error("throng did not answer.".into()))
            }
        }
    };
    let mut send: &Stream = stream;
    let text = serde_json::to_string(&reply).unwrap_or_else(|_| "\"Ok\"".into());
    if writeln!(send, "{text}").is_ok() {
        let _ = send.flush();
    }
    throng_daemon::endpoint::disconnect(stream);
}

/// Send one command to the running window and wait up to `timeout` for its answer. The error is
/// a sentence for the person at the command line.
pub fn send(dirs: &AppDirs, command: &Command, timeout: Duration) -> Result<Reply, String> {
    let stream = endpoint(dirs).connect().map_err(|_| "throng is not running.".to_owned())?;
    let text = serde_json::to_string(command).map_err(|e| e.to_string())?;
    {
        let mut send: &Stream = &stream;
        writeln!(send, "{text}")
            .and_then(|()| send.flush())
            .map_err(|e| format!("throng could not be reached: {e}"))?;
    }
    // Read on a thread of its own, so a window that never answers cannot hold the caller.
    let (tx, rx) = crossbeam_channel::bounded(1);
    thread::Builder::new()
        .name("throng-remote-reply".into())
        .spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(&stream).read_line(&mut line).map(|_| line);
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(timeout) {
        Ok(Ok(line)) if !line.trim().is_empty() => serde_json::from_str(line.trim())
            .map_err(|e| format!("throng answered something unexpected: {e}")),
        Ok(Ok(_)) => Err("throng closed the connection without answering.".into()),
        Ok(Err(e)) => Err(format!("throng could not be read: {e}")),
        Err(_) => Err("throng did not answer in time.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn commands_round_trip_as_json() {
        let command = Command::Open { path: PathBuf::from("/a/b.rs"), line: Some(12), column: Some(3) };
        let text = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&text).unwrap(), command);
        let reply = Reply::Status(vec![ProjectStatus {
            id: ProjectId::new(),
            name: "Web".into(),
            root: PathBuf::from("/w"),
            active: true,
            terminals: vec![TerminalStatus {
                terminal: TerminalId::new(),
                label: "Web › Tab 1 › Panel 1".into(),
                busy: true,
                attention: false,
                title: Some("make".into()),
                directory: Some(PathBuf::from("/w/src")),
            }],
        }]);
        let text = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<Reply>(&text).unwrap(), reply);
    }

    #[test]
    fn a_target_splits_its_position_and_is_made_absolute() {
        let cwd = Path::new("/home/me/proj");
        assert_eq!(
            target("src/a.rs:12:3", cwd),
            (PathBuf::from("/home/me/proj/src/a.rs"), Some(12), Some(3))
        );
        assert_eq!(target("notes.md", cwd), (PathBuf::from("/home/me/proj/notes.md"), None, None));
        let absolute = if cfg!(windows) { "C:\\tmp\\x.txt" } else { "/tmp/x.txt" };
        assert_eq!(target(&format!("{absolute}:7"), cwd), (PathBuf::from(absolute), Some(7), None));
    }

    #[test]
    fn the_server_answers_one_command_per_connection() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::under(dir.path());
        dirs.ensure().unwrap();
        let server = RemoteServer::start(&dirs, || {}).unwrap();
        let client = {
            let dirs = dirs.clone();
            std::thread::spawn(move || send(&dirs, &Command::Status, Duration::from_secs(10)))
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let incoming = loop {
            if let Some(incoming) = server.poll().pop() {
                break incoming;
            }
            assert!(std::time::Instant::now() < deadline, "the command never arrived");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(incoming.command, Command::Status);
        incoming.respond.send(Reply::Status(Vec::new())).unwrap();
        assert_eq!(client.join().unwrap(), Ok(Reply::Status(Vec::new())));
        // Nothing answers once the window is gone.
        drop(server);
        assert!(send(&dirs, &Command::Status, Duration::from_secs(2)).is_err());
    }
}
