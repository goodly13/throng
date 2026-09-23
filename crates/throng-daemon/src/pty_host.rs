//! A PTY host: a process of its own that owns one terminal's PTY and shell, and relays them to the
//! daemon over its standard input and output (`throng pty-host`).
//!
//! An elevated daemon starts one, without administrator rights, for each terminal that is not to
//! run as administrator. The host, not the daemon, must create the PTY: a program cannot use a
//! console host running at a higher integrity level than its own, so a de-elevated shell on the
//! daemon's own PTY would start and then fail.
//!
//! Frames go both ways in the protocol crate's framing. The daemon sends [`Start`] first; the host
//! answers that the shell started (with its pid) or why it did not, then streams its output, and
//! last its exit status. The daemon closing the host's input, or asking for a kill, ends the shell.
//! On the daemon's side a host looks like any other PTY and child, so a session drives both alike.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::Receiver;
use parking_lot::{Condvar, Mutex};
use portable_pty::{Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use throng_protocol::{read_frame, write_frame};

/// What a host starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The shell's complete environment.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

impl Start {
    /// The command that starts the shell, on whichever PTY.
    pub(crate) fn command(&self) -> CommandBuilder {
        let mut command = CommandBuilder::new(&self.program);
        command.args(&self.args);
        command.cwd(&self.cwd);
        command.env_clear();
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    pub(crate) fn size(&self) -> PtySize {
        size(self.cols, self.rows)
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 }
}

/// Daemon → host.
#[derive(Debug, Serialize, Deserialize)]
enum ToHost {
    Start(Start),
    Input(#[serde(with = "serde_bytes")] Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Kill,
}

/// Host → daemon.
#[derive(Debug, Serialize, Deserialize)]
enum FromHost {
    Started {
        pid: Option<u32>,
    },
    Failed(String),
    Output(#[serde(with = "serde_bytes")] Vec<u8>),
    /// As the OS reported it: a code, or the signal that ended it (Unix).
    Exited {
        code: u32,
        signal: Option<String>,
    },
}

/// One end of the pipe to the other side, shared by everything that writes to it.
type Sink = Arc<Mutex<Box<dyn Write + Send>>>;

fn send<T: Serialize>(sink: &Sink, message: &T) -> bool {
    write_frame(&mut *sink.lock(), message).is_ok()
}

/// Be a host on this process's standard input and output. Returns the process's exit code.
#[must_use]
pub fn run() -> i32 {
    serve(io::stdin(), io::stdout())
}

/// Serve one terminal over `input` and `output` until its shell exits.
pub fn serve(mut input: impl Read + Send + 'static, output: impl Write + Send + 'static) -> i32 {
    let sink: Sink = Arc::new(Mutex::new(Box::new(output)));
    let Ok(ToHost::Start(start)) = read_frame::<ToHost>(&mut input) else {
        return 2;
    };
    let (master, mut child, mut reader, mut writer) = match open(&start) {
        Ok(opened) => opened,
        Err(reason) => {
            send(&sink, &FromHost::Failed(reason));
            return 1;
        }
    };
    if !send(&sink, &FromHost::Started { pid: child.process_id() }) {
        let _ = child.kill();
        return 1;
    }

    let (drained_tx, drained_rx) = crossbeam_channel::bounded::<()>(1);
    {
        let sink = Arc::clone(&sink);
        let _ = thread::Builder::new().name("host-output".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if !send(&sink, &FromHost::Output(buf[..n].to_vec())) {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = drained_tx.send(());
        });
    }
    let master = Arc::new(Mutex::new(Some(master)));
    {
        let master = Arc::clone(&master);
        let mut killer = child.clone_killer();
        let _ = thread::Builder::new().name("host-input".into()).spawn(move || {
            loop {
                match read_frame::<ToHost>(&mut input) {
                    Ok(ToHost::Input(data)) => {
                        let _ = writer.write_all(&data).and_then(|()| writer.flush());
                    }
                    Ok(ToHost::Resize { cols, rows }) => {
                        if let Some(master) = &*master.lock() {
                            let _ = master.resize(size(cols, rows));
                        }
                    }
                    Ok(ToHost::Start(_)) => {}
                    // Asked to, or the daemon is gone: nobody may be left running unseen.
                    Ok(ToHost::Kill) | Err(_) => {
                        let _ = killer.kill();
                        break;
                    }
                }
            }
        });
    }

    let status = child.wait();
    // Closing the PTY lets its last output drain (a Windows pseudo console never ends its output
    // otherwise). Bounded, since a background process can hold a Unix PTY open.
    drop(master.lock().take());
    let _ = drained_rx.recv_timeout(Duration::from_secs(1));
    let (code, signal) = match status {
        Ok(status) => (status.exit_code(), status.signal().map(str::to_owned)),
        Err(e) => (1, Some(e.to_string())),
    };
    send(&sink, &FromHost::Exited { code, signal });
    i32::try_from(code).unwrap_or(1)
}

type Opened =
    (Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>, Box<dyn Read + Send>, Box<dyn Write + Send>);

fn open(start: &Start) -> Result<Opened, String> {
    let pair = native_pty_system().openpty(start.size()).map_err(|e| e.to_string())?;
    let child = pair.slave.spawn_command(start.command()).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    Ok((pair.master, child, reader, writer))
}

/// A terminal whose PTY and shell live in a host: the two halves a session drives.
pub struct Hosted {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

/// Start `start` in the host at the other end of `to_host` and `from_host`. Returns once the host
/// has started the shell, or with its reason for not starting it.
pub fn connect(
    to_host: impl Write + Send + 'static,
    mut from_host: impl Read + Send + 'static,
    start: &Start,
) -> Result<Hosted, String> {
    const ENDED: &str = "its host ended before starting it";
    let sink: Sink = Arc::new(Mutex::new(Box::new(to_host)));
    if !send(&sink, &ToHost::Start(start.clone())) {
        return Err(ENDED.into());
    }
    let pid = match read_frame::<FromHost>(&mut from_host) {
        Ok(FromHost::Started { pid }) => pid,
        Ok(FromHost::Failed(reason)) => return Err(reason),
        Ok(_) | Err(_) => return Err(ENDED.into()),
    };

    let (output_tx, output_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    let exit = Arc::new(Exit::default());
    {
        let exit = Arc::clone(&exit);
        thread::Builder::new()
            .name("host-link".into())
            .spawn(move || {
                let status = loop {
                    match read_frame::<FromHost>(&mut from_host) {
                        Ok(FromHost::Output(data)) => {
                            let _ = output_tx.send(data);
                        }
                        Ok(FromHost::Exited { code, signal: None }) => {
                            break ExitStatus::with_exit_code(code);
                        }
                        Ok(FromHost::Exited { signal: Some(signal), .. }) => {
                            break ExitStatus::with_signal(&signal);
                        }
                        Ok(FromHost::Started { .. } | FromHost::Failed(_)) => {}
                        Err(_) => break ExitStatus::with_signal("its host ended"),
                    }
                };
                // Every output chunk is queued before the exit is announced.
                drop(output_tx);
                *exit.status.lock() = Some(status);
                exit.done.notify_all();
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(Hosted {
        master: Box::new(HostMaster {
            sink: Arc::clone(&sink),
            size: Mutex::new(start.size()),
            output: Mutex::new(Some(output_rx)),
        }),
        child: Box::new(HostChild { pid, exit, sink }),
    })
}

#[derive(Default)]
struct Exit {
    status: Mutex<Option<ExitStatus>>,
    done: Condvar,
}

struct HostMaster {
    sink: Sink,
    size: Mutex<PtySize>,
    output: Mutex<Option<Receiver<Vec<u8>>>>,
}

impl MasterPty for HostMaster {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        *self.size.lock() = size;
        send(&self.sink, &ToHost::Resize { cols: size.cols, rows: size.rows });
        Ok(())
    }

    fn get_size(&self) -> anyhow::Result<PtySize> {
        Ok(*self.size.lock())
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        let output =
            self.output.lock().take().ok_or_else(|| anyhow::anyhow!("a host's output has one reader"))?;
        Ok(Box::new(OutputReader { output, chunk: Vec::new(), at: 0 }))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        Ok(Box::new(InputWriter(Arc::clone(&self.sink))))
    }

    // A host's process group is not this process's to read: a hosted shell is always "maybe busy".
    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<libc::pid_t> {
        None
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<portable_pty::unix::RawFd> {
        None
    }

    #[cfg(unix)]
    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

/// The host's output, as the bytes a PTY would give.
struct OutputReader {
    output: Receiver<Vec<u8>>,
    chunk: Vec<u8>,
    at: usize,
}

impl Read for OutputReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.chunk.len() {
            match self.output.recv() {
                Ok(chunk) => {
                    self.chunk = chunk;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.chunk.len() - self.at);
        buf[..n].copy_from_slice(&self.chunk[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

struct InputWriter(Sink);

impl Write for InputWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if send(&self.0, &ToHost::Input(buf.to_vec())) {
            Ok(buf.len())
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "the terminal's host has ended"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct HostChild {
    pid: Option<u32>,
    exit: Arc<Exit>,
    sink: Sink,
}

impl std::fmt::Debug for HostChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostChild").field("pid", &self.pid).finish_non_exhaustive()
    }
}

impl ChildKiller for HostChild {
    fn kill(&mut self) -> io::Result<()> {
        send(&self.sink, &ToHost::Kill);
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(HostKiller(Arc::clone(&self.sink)))
    }
}

impl Child for HostChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(self.exit.status.lock().clone())
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        let mut status = self.exit.status.lock();
        loop {
            if let Some(status) = &*status {
                return Ok(status.clone());
            }
            self.exit.done.wait(&mut status);
        }
    }

    /// The shell's pid (not the host's), so its directory and commands are read as for any shell.
    fn process_id(&self) -> Option<u32> {
        self.pid
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

struct HostKiller(Sink);

impl std::fmt::Debug for HostKiller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostKiller")
    }
}

impl ChildKiller for HostKiller {
    fn kill(&mut self) -> io::Result<()> {
        send(&self.0, &ToHost::Kill);
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self(Arc::clone(&self.0)))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    /// A host serving on a thread of its own, joined through in-process pipes.
    fn host(start: &Start) -> (Result<Hosted, String>, thread::JoinHandle<i32>) {
        let (host_in, to_host) = io::pipe().unwrap();
        let (from_host, host_out) = io::pipe().unwrap();
        let served = thread::spawn(move || serve(host_in, host_out));
        (connect(to_host, from_host, start), served)
    }

    fn start(program: &str, args: &[&str]) -> Start {
        Start {
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: std::env::temp_dir(),
            env: vec![
                ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
                ("TERM".into(), "xterm".into()),
            ],
            cols: 80,
            rows: 24,
        }
    }

    /// Everything the host's shell writes, gathered on a thread.
    fn gather(master: &dyn MasterPty) -> Arc<Mutex<Vec<u8>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone_reader().unwrap();
        let sink = Arc::clone(&seen);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().extend_from_slice(&buf[..n]);
            }
        });
        seen
    }

    fn wait_for(seen: &Mutex<Vec<u8>>, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !String::from_utf8_lossy(&seen.lock()).contains(needle) {
            assert!(
                Instant::now() < deadline,
                "never saw {needle:?} in {:?}",
                String::from_utf8_lossy(&seen.lock())
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_within(child: Box<dyn Child + Send + Sync>, limit: Duration) -> ExitStatus {
        let mut child = child;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(child.wait());
        });
        rx.recv_timeout(limit).expect("the hosted shell never ended").unwrap()
    }

    #[test]
    fn a_hosted_shell_takes_input_and_resizes_and_reports_its_exit() {
        let (hosted, served) = host(&start("/bin/sh", &[]));
        let hosted = hosted.unwrap();
        assert!(hosted.child.process_id().is_some(), "the shell's pid comes back");
        let seen = gather(&*hosted.master);
        let mut writer = hosted.master.take_writer().unwrap();
        hosted.master.resize(size(100, 30)).unwrap();
        writer.write_all(b"stty size; echo out-$((6*7)); exit 7\n").unwrap();
        wait_for(&seen, "30 100");
        wait_for(&seen, "out-42");
        let status = wait_within(hosted.child, Duration::from_secs(10));
        assert_eq!(status.exit_code(), 7);
        assert_eq!(served.join().unwrap(), 7);
    }

    #[test]
    fn a_hosted_shell_ends_when_killed() {
        let (hosted, _served) = host(&start("/bin/sh", &["-c", "sleep 30"]));
        let hosted = hosted.unwrap();
        let _seen = gather(&*hosted.master);
        hosted.child.clone_killer().kill().unwrap();
        let status = wait_within(hosted.child, Duration::from_secs(10));
        assert!(!status.success());
    }

    #[test]
    fn a_hosted_shell_ends_when_the_daemon_lets_go() {
        let (hosted, served) = host(&start("/bin/sh", &["-c", "sleep 30"]));
        let hosted = hosted.unwrap();
        let _seen = gather(&*hosted.master);
        drop(hosted);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(served.join());
        });
        rx.recv_timeout(Duration::from_secs(10)).expect("the host outlived its daemon").unwrap();
    }

    #[test]
    fn a_host_says_why_a_shell_did_not_start() {
        let (hosted, served) = host(&start("/no/such/shell", &[]));
        assert!(hosted.is_err());
        assert_eq!(served.join().unwrap(), 1);
    }
}
