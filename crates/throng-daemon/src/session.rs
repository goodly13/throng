//! One terminal session: a shell on a PTY, its retained output, and the views attached to it.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use portable_pty::{ChildKiller, MasterPty, PtySize, native_pty_system};
use throng_core::ids::{ProjectId, TerminalId};
use throng_core::terminal::{ExitStatus, is_private_env_var};
use throng_platform::process::ProcessTree;
use throng_protocol::{ServerMsg, Snapshot, SpawnSpec, TerminalInfo};

use crate::pty_host::{self, Start};
use crate::registry::{ClientId, Registry};

/// Retained output per session.
pub const DEFAULT_TAIL_BYTES: usize = 4 * 1024 * 1024;

/// A session.
pub struct Session {
    pub id: TerminalId,
    pub project: ProjectId,
    pub label: String,
    pub pid: Option<u32>,
    /// The shell runs as administrator (Windows).
    pub elevated: bool,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// The shell and everything it starts (Windows: a job that ends them all when this session is
    /// dropped, so a command cannot outlive its terminal or the daemon). Unix ends a session's
    /// processes through its process groups instead, and never reads this.
    #[cfg_attr(unix, allow(dead_code))]
    tree: Option<ProcessTree>,
    state: Mutex<State>,
}

struct State {
    tail: VecDeque<u8>,
    capacity: usize,
    /// Bytes produced over the session's life; the offset just past the last byte.
    end_offset: u64,
    exited: Option<ExitStatus>,
    user_killed: bool,
    views: HashMap<ClientId, (u16, u16)>,
    size: (u16, u16),
    alt: AltScreenScanner,
    seen_output: bool,
    startup_command: Option<String>,
}

impl State {
    fn push(&mut self, chunk: &[u8]) {
        self.alt.scan(chunk);
        self.end_offset += chunk.len() as u64;
        self.tail.extend(chunk);
        if self.tail.len() > self.capacity {
            // Trim to a line boundary so a replay never starts inside an escape sequence or a
            // multi-byte character.
            let excess = self.tail.len() - self.capacity;
            let window = self.tail.len().min(excess + 64 * 1024);
            let cut = (excess..window).find(|i| self.tail[*i] == b'\n').map_or(excess, |i| i + 1);
            self.tail.drain(..cut);
        }
    }
}

/// Tracks whether the program is on the alternate screen by watching for `CSI ? 1049/1047/47 h|l`.
#[derive(Default)]
struct AltScreenScanner {
    active: bool,
    state: ScanState,
    params: Vec<u8>,
}

#[derive(Default, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    Escape,
    Csi,
    Private,
}

impl AltScreenScanner {
    fn scan(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state = match (&self.state, b) {
                (_, 0x1b) => ScanState::Escape,
                (ScanState::Escape, b'[') => ScanState::Csi,
                (ScanState::Csi, b'?') => {
                    self.params.clear();
                    ScanState::Private
                }
                (ScanState::Private, b'0'..=b'9' | b';') if self.params.len() < 32 => {
                    self.params.push(b);
                    ScanState::Private
                }
                (ScanState::Private, b'h' | b'l') => {
                    let set = b == b'h';
                    let hit =
                        self.params.split(|c| *c == b';').any(|p| p == b"1049" || p == b"1047" || p == b"47");
                    if hit {
                        self.active = set;
                    }
                    ScanState::Ground
                }
                _ => ScanState::Ground,
            };
        }
    }
}

/// Why a spawn failed, in words for the user.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("The folder \"{0}\" does not exist.")]
    MissingFolder(String),
    #[error("Could not start {program}: {reason}")]
    Failed { program: String, reason: String },
}

/// How a session's shell starts.
#[derive(Clone, Copy, Debug)]
pub enum Launch<'a> {
    /// On a PTY of the daemon's own, with the daemon's rights.
    Native,
    /// In a PTY host (`<exe> pty-host`) started without the daemon's administrator rights.
    Deelevated { exe: &'a Path },
}

/// Hold `pid` and everything it starts from now on together, so none of it outlives the session.
fn hold(pid: u32) -> Option<ProcessTree> {
    match ProcessTree::adopt(pid) {
        Ok(tree) => Some(tree),
        Err(e) => {
            tracing::warn!(pid, error = %e, "could not hold the shell's processes together");
            None
        }
    }
}

impl Session {
    /// Start a shell. Output flows to attached views through `registry`; `on_exit` runs once the
    /// shell has exited and its output has drained.
    pub fn spawn(
        spec: &SpawnSpec,
        launch: Launch<'_>,
        tail_bytes: usize,
        registry: Arc<Registry>,
        on_exit: impl FnOnce(TerminalId) + Send + 'static,
    ) -> Result<Arc<Self>, SpawnError> {
        if !spec.cwd.is_dir() {
            return Err(SpawnError::MissingFolder(spec.cwd.display().to_string()));
        }
        let failed =
            |reason: String| SpawnError::Failed { program: spec.program.display().to_string(), reason };
        // Later entries win, so throng's own terminal variables override the UI's.
        let mut env: Vec<(String, String)> =
            spec.env.iter().filter(|(key, _)| !is_private_env_var(key)).cloned().collect();
        env.extend([
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ("TERM_PROGRAM".to_owned(), "throng".to_owned()),
            ("TERM_PROGRAM_VERSION".to_owned(), env!("CARGO_PKG_VERSION").to_owned()),
        ]);
        let start = Start {
            program: spec.program.clone(),
            args: spec.args.clone(),
            cwd: spec.cwd.clone(),
            env,
            cols: spec.cols,
            rows: spec.rows,
        };
        let size = start.size();

        let (master, mut child, tree, elevated) = match launch {
            Launch::Native => {
                let pair = native_pty_system().openpty(size).map_err(|e| failed(e.to_string()))?;
                let child = pair.slave.spawn_command(start.command()).map_err(|e| failed(e.to_string()))?;
                drop(pair.slave);
                let tree = child.process_id().and_then(hold);
                // Marked only where running as administrator is a choice (Windows): a Unix
                // terminal has its user's rights, root's included, like any other program.
                (pair.master, child, tree, cfg!(windows) && throng_platform::process::is_elevated())
            }
            Launch::Deelevated { exe } => {
                let host = throng_platform::process::spawn_deelevated(exe, &["pty-host"]).map_err(|e| {
                    failed(format!("it could not be started without administrator rights ({e})"))
                })?;
                // Held before the shell exists, so the shell and all it starts are held with it.
                let tree = hold(host.pid);
                let hosted = pty_host::connect(host.stdin, host.stdout, &start).map_err(failed)?;
                (hosted.master, hosted.child, tree, false)
            }
        };
        let pid = child.process_id();
        let killer = child.clone_killer();
        let reader = master.try_clone_reader().map_err(|e| failed(e.to_string()))?;
        let writer = master.take_writer().map_err(|e| failed(e.to_string()))?;

        let session = Arc::new(Self {
            id: spec.terminal,
            project: spec.project,
            label: spec.label.clone(),
            pid,
            elevated,
            master: Mutex::new(master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            tree,
            state: Mutex::new(State {
                tail: VecDeque::new(),
                capacity: tail_bytes.max(4096),
                end_offset: 0,
                exited: None,
                user_killed: false,
                views: HashMap::new(),
                size: (size.cols, size.rows),
                alt: AltScreenScanner::default(),
                seen_output: false,
                startup_command: spec.startup_command.clone().filter(|c| !c.trim().is_empty()),
            }),
        });

        let (drained_tx, drained_rx) = crossbeam_channel::bounded::<()>(1);
        {
            let session = Arc::clone(&session);
            let registry = Arc::clone(&registry);
            thread::Builder::new()
                .name(format!("pty-read-{}", spec.terminal))
                .spawn(move || {
                    session.pump_output(reader, &registry);
                    let _ = drained_tx.send(());
                })
                .map_err(|e| failed(e.to_string()))?;
        }
        {
            let session = Arc::clone(&session);
            thread::Builder::new()
                .name(format!("pty-wait-{}", spec.terminal))
                .spawn(move || {
                    let status = child.wait();
                    // Deliver every byte the shell wrote before announcing its exit. A background
                    // process can hold the PTY open forever, so the wait is bounded.
                    let _ = drained_rx.recv_timeout(Duration::from_secs(1));
                    let code = match &status {
                        Ok(s) if s.signal().is_none() => i32::try_from(s.exit_code()).ok(),
                        _ => None,
                    };
                    session.finish(code, &registry);
                    on_exit(session.id);
                })
                .map_err(|e| failed(e.to_string()))?;
        }
        Ok(session)
    }

    fn pump_output(&self, mut reader: Box<dyn Read + Send>, registry: &Registry) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let chunk = &buf[..n];
            let startup = {
                let mut state = self.state.lock();
                let offset = state.end_offset;
                state.push(chunk);
                let startup = if state.seen_output { None } else { state.startup_command.take() };
                state.seen_output = true;
                // Sent while holding the state lock: an attach snapshot taken under the same lock is
                // therefore either entirely before or entirely after this chunk.
                for client in state.views.keys() {
                    registry.send_output(*client, self.id, offset, chunk);
                }
                startup
            };
            if let Some(command) = startup {
                let mut line = command.into_bytes();
                line.push(b'\r');
                let _ = self.writer.lock().write_all(&line);
            }
        }
    }

    fn finish(&self, code: Option<i32>, registry: &Registry) {
        let mut state = self.state.lock();
        let status = ExitStatus { code, user_killed: state.user_killed };
        state.exited = Some(status);
        for client in state.views.keys() {
            registry.send(*client, ServerMsg::Exited { terminal: self.id, status });
        }
        state.views.clear();
    }

    /// Attach a view and send its snapshot through `registry`, under the lock that orders output.
    pub fn attach(
        &self,
        client: ClientId,
        reply_id: u64,
        cols: u16,
        rows: u16,
        registry: &Registry,
    ) -> Snapshot {
        let mut state = self.state.lock();
        let snapshot = Snapshot {
            terminal: self.id,
            tail: state.tail.iter().copied().collect(),
            end_offset: state.end_offset,
            exited: state.exited,
            alt_screen: state.alt.active,
            elevated: self.elevated,
        };
        if state.exited.is_none() {
            state.views.insert(client, (cols.max(1), rows.max(1)));
            self.apply_size(&mut state);
        }
        registry.send(
            client,
            ServerMsg::Reply { id: reply_id, result: Ok(throng_protocol::Reply::Snapshot(snapshot.clone())) },
        );
        snapshot
    }

    /// Make a program on the alternate screen repaint a rebuilt view: shrink by one row, then
    /// restore, far enough apart that the program sees two distinct sizes.
    pub fn nudge(self: &Arc<Self>) {
        let session = Arc::clone(self);
        let _ = thread::Builder::new().name("pty-nudge".into()).spawn(move || {
            let (cols, rows) = session.state.lock().size;
            if rows < 2 {
                return;
            }
            let resize = |rows: u16| {
                let _ = session.master.lock().resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            };
            resize(rows - 1);
            thread::sleep(Duration::from_millis(60));
            resize(session.state.lock().size.1);
        });
    }

    pub fn detach(&self, client: ClientId) {
        let mut state = self.state.lock();
        if state.views.remove(&client).is_some() {
            self.apply_size(&mut state);
        }
    }

    pub fn resize(&self, client: ClientId, cols: u16, rows: u16) {
        let mut state = self.state.lock();
        // A resize from a view that is not attached is ignored rather than re-adding a ghost view
        // that would pin the size.
        if let Some(view) = state.views.get_mut(&client) {
            *view = (cols.max(1), rows.max(1));
            self.apply_size(&mut state);
        }
    }

    /// The PTY takes the smallest size across attached views and is only resized on a change.
    fn apply_size(&self, state: &mut State) {
        let Some(cols) = state.views.values().map(|v| v.0).min() else { return };
        let Some(rows) = state.views.values().map(|v| v.1).min() else { return };
        if (cols, rows) != state.size {
            state.size = (cols, rows);
            let _ = self.master.lock().resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    pub fn write(&self, data: &[u8]) -> std::io::Result<()> {
        if self.state.lock().exited.is_some() {
            return Ok(());
        }
        let mut writer = self.writer.lock();
        writer.write_all(data)?;
        writer.flush()
    }

    /// End the session at the user's request: hang up its process groups, then escalate if anything
    /// ignores the hangup. No process the session started may outlive it (Principle III).
    pub fn kill(self: &Arc<Self>) {
        {
            let mut state = self.state.lock();
            if state.exited.is_some() {
                return;
            }
            state.user_killed = true;
        }
        self.signal_all(Signal::Hangup);
        let _ = self.killer.lock().kill();
        let session = Arc::clone(self);
        let _ = thread::Builder::new().name("pty-kill".into()).spawn(move || {
            for _ in 0..30 {
                if session.is_exited() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            session.signal_all(Signal::Kill);
        });
    }

    fn signal_all(&self, signal: Signal) {
        #[cfg(unix)]
        {
            let sig = match signal {
                Signal::Hangup => libc::SIGHUP,
                Signal::Kill => libc::SIGKILL,
            };
            let foreground = self.master.lock().process_group_leader();
            for group in foreground.into_iter().chain(self.pid.and_then(|p| libc::pid_t::try_from(p).ok())) {
                crate::unix::kill_group(group, sig);
            }
            if let Some(pid) = self.pid {
                crate::unix::kill_session(pid, sig);
            }
        }
        #[cfg(not(unix))]
        {
            // No hangup to send: the terminal was asked to end, so its whole tree ends.
            let _ = signal;
            if let Some(tree) = &self.tree {
                tree.terminate();
            }
        }
    }

    #[must_use]
    pub fn is_exited(&self) -> bool {
        self.state.lock().exited.is_some()
    }

    #[must_use]
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.state.lock().exited
    }

    /// Whether a command other than the shell holds the foreground. When that cannot be determined
    /// the answer is "busy": a possibly-running command is never treated as idle.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        if self.is_exited() {
            return false;
        }
        #[cfg(unix)]
        {
            match (self.master.lock().process_group_leader(), self.pid) {
                (Some(group), Some(pid)) => u32::try_from(group).ok() != Some(pid),
                _ => true,
            }
        }
        #[cfg(not(unix))]
        {
            self.pid.and_then(throng_platform::process::runs_a_command).unwrap_or(true)
        }
    }

    #[must_use]
    pub fn info(&self) -> TerminalInfo {
        let (exited, views) = {
            let state = self.state.lock();
            (state.exited, state.views.len())
        };
        TerminalInfo {
            terminal: self.id,
            project: self.project,
            label: self.label.clone(),
            pid: self.pid,
            busy: exited.is_none() && self.is_busy(),
            exited,
            views,
            elevated: self.elevated,
        }
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Hangup,
    Kill,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_screen_is_tracked_across_chunks() {
        let mut scanner = AltScreenScanner::default();
        scanner.scan(b"hello \x1b[?10");
        assert!(!scanner.active);
        scanner.scan(b"49h vim");
        assert!(scanner.active);
        scanner.scan(b"\x1b[?25l\x1b[?1049l");
        assert!(!scanner.active);
        scanner.scan(b"\x1b[?1;47h");
        assert!(scanner.active);
        scanner.scan(b"\x1b[?2004h");
        assert!(scanner.active, "unrelated private modes leave it alone");
    }

    #[test]
    fn tail_trims_at_a_line_boundary() {
        let mut state = State {
            tail: VecDeque::new(),
            capacity: 10,
            end_offset: 0,
            exited: None,
            user_killed: false,
            views: HashMap::new(),
            size: (80, 24),
            alt: AltScreenScanner::default(),
            seen_output: false,
            startup_command: None,
        };
        state.push(b"line1\nline2\nline3\n");
        let tail: Vec<u8> = state.tail.iter().copied().collect();
        assert_eq!(tail, b"line3\n");
        assert_eq!(state.end_offset, 18);
    }
}
