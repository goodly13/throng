//! The daemon against real shells on real PTYs: `/bin/sh` on Linux and macOS, `cmd.exe` under
//! ConPTY on Windows. A few tests are about Unix itself (process groups, the socket file) and run
//! there only.

use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use throng_core::ids::{ProjectId, TerminalId};
use throng_core::terminal::ExitStatus;
use throng_daemon::{Client, ClientEvent, DaemonConfig, Endpoint, RequestError, RunError, run};
use throng_platform::dirs::AppDirs;
use throng_protocol::{
    ClientMsg, FrameError, Reply, Request, ServerMsg, Snapshot, SpawnSpec, read_frame, unseen, write_frame,
};

const WAIT: Duration = Duration::from_secs(10);

struct Harness {
    _dir: tempfile::TempDir,
    dirs: AppDirs,
    daemon: Option<JoinHandle<Result<(), RunError>>>,
}

impl Harness {
    fn start() -> Self {
        Self::start_with(|_| {})
    }

    fn start_with(tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::under(dir.path());
        let mut config = DaemonConfig::new(dirs.clone());
        config.idle_exit = Duration::from_secs(30);
        tweak(&mut config);
        let daemon = thread::spawn(move || run(config));
        let endpoint = Endpoint::for_dirs(&dirs);
        let deadline = Instant::now() + WAIT;
        while endpoint.connect().is_err() {
            assert!(Instant::now() < deadline, "daemon never started listening");
            thread::sleep(Duration::from_millis(10));
        }
        Self { _dir: dir, dirs, daemon: Some(daemon) }
    }

    fn client(&self) -> Client {
        Client::connect(&Endpoint::for_dirs(&self.dirs), || {}).unwrap()
    }

    fn cwd(&self) -> PathBuf {
        self._dir.path().to_path_buf()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            if let Ok(client) = Client::connect(&Endpoint::for_dirs(&self.dirs), || {}) {
                let _ = client.request(Request::Shutdown { kill_all: true }, WAIT);
            }
            let _ = daemon.join();
        }
    }
}

fn env() -> Vec<(String, String)> {
    // Windows programs need much of the environment (SystemRoot, PATHEXT…); a Unix shell only this.
    let keep = |k: &str| cfg!(windows) || k == "PATH" || k == "HOME" || k == "LANG";
    let mut env: Vec<(String, String)> =
        std::env::vars().filter(|(k, _)| keep(k) && !k.starts_with("THRONG_")).collect();
    env.push(("PS1".into(), "$ ".into()));
    env.push(("THRONG_SECRET".into(), "must-not-leak".into()));
    env
}

/// A shell that runs `unix` (with `/bin/sh -c`) or `windows` (with `cmd.exe /c`) and exits.
fn script(cwd: &Path, unix: &str, windows: &str) -> SpawnSpec {
    if cfg!(windows) {
        spec(cwd, "cmd.exe", &["/d", "/q", "/c", windows])
    } else {
        spec(cwd, "/bin/sh", &["-c", unix])
    }
}

/// An interactive shell.
fn interactive(cwd: &Path) -> SpawnSpec {
    if cfg!(windows) { spec(cwd, "cmd.exe", &["/d"]) } else { spec(cwd, "/bin/sh", &[]) }
}

/// Terminal output as text, without its escape sequences (ConPTY writes many).
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: to BEL or ST.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn spec(cwd: &Path, program: &str, args: &[&str]) -> SpawnSpec {
    SpawnSpec {
        terminal: TerminalId::new(),
        project: ProjectId::new(),
        label: "test".into(),
        program: program.into(),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        cwd: cwd.to_path_buf(),
        env: env(),
        cols: 80,
        rows: 24,
        startup_command: None,
    }
}

fn snapshot(reply: Result<Reply, RequestError>) -> Snapshot {
    match reply {
        Ok(Reply::Snapshot(s)) => s,
        other => panic!("expected a snapshot, got {other:?}"),
    }
}

/// Everything a view has shown for `terminal`: the snapshot, then unseen output, until `done`.
struct View {
    terminal: TerminalId,
    seen_until: u64,
    text: Vec<u8>,
    exited: Option<ExitStatus>,
}

impl View {
    fn new(snapshot: &Snapshot) -> Self {
        Self {
            terminal: snapshot.terminal,
            seen_until: snapshot.end_offset,
            text: snapshot.tail.clone(),
            exited: snapshot.exited,
        }
    }

    fn pump(&mut self, client: &Client, until: impl Fn(&Self) -> bool) {
        pump_all(client, &mut [self], until);
    }

    /// Take one event, if it is for this view's terminal.
    fn take(&mut self, event: &ClientEvent) {
        match event {
            ClientEvent::Output { terminal, offset, data } if *terminal == self.terminal => {
                let fresh = unseen(*offset, data, self.seen_until);
                self.text.extend_from_slice(fresh);
                self.seen_until = self.seen_until.max(offset + data.len() as u64);
            }
            ClientEvent::Exited { terminal, status } if *terminal == self.terminal => {
                self.exited = Some(*status)
            }
            _ => {}
        }
    }

    fn contains(&self, needle: &str) -> bool {
        plain(&self.text).contains(needle)
    }

    fn count(&self, needle: &str) -> usize {
        plain(&self.text).matches(needle).count()
    }

    /// The text with every run of whitespace made one space (for output a console lays out).
    fn squeezed(&self) -> String {
        plain(&self.text).split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// Pump several views from one client until `until` holds for each. One client's events carry every
/// terminal's output, so waiting on one view alone would drop what the others were sent meanwhile.
fn pump_all(client: &Client, views: &mut [&mut View], until: impl Fn(&View) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !views.iter().all(|v| until(v)) {
        let left = deadline.saturating_duration_since(Instant::now());
        let shown: Vec<String> =
            views.iter().map(|v| String::from_utf8_lossy(&v.text).into_owned()).collect();
        assert!(!left.is_zero(), "timed out; output so far: {shown:?}");
        if let Ok(event) = client.events().recv_timeout(left.min(Duration::from_millis(100))) {
            for view in views.iter_mut() {
                view.take(&event);
            }
        }
    }
}

#[test]
fn spawn_streams_output_and_reports_the_exit_code() {
    let h = Harness::start();
    let client = h.client();
    let spec = script(
        &h.cwd(),
        "echo hello from $0; echo secret=${THRONG_SECRET:-none}; exit 3",
        "echo hello from cmd& if defined THRONG_SECRET (echo secret=%THRONG_SECRET%) else (echo secret=none)& exit 3",
    );
    let mut view = View::new(&snapshot(client.request(Request::Spawn(spec), WAIT)));
    view.pump(&client, |v| v.exited.is_some());
    assert!(view.contains(if cfg!(windows) { "hello from cmd" } else { "hello from /bin/sh" }));
    assert!(view.contains("secret=none"), "THRONG_* variables must not reach the shell");
    assert_eq!(view.exited, Some(ExitStatus { code: Some(3), user_killed: false }));
}

#[test]
fn spawning_into_a_missing_folder_is_refused_in_words() {
    let h = Harness::start();
    let client = h.client();
    let spec = interactive(&h.cwd().join("gone"));
    match client.request(Request::Spawn(spec), WAIT) {
        Err(RequestError::Refused(message)) => assert!(message.contains("does not exist"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_exit_while_nobody_watches_is_kept_for_the_next_attach() {
    let h = Harness::start();
    let client = h.client();
    let spec = script(
        &h.cwd(),
        "sleep 0.3; echo last words; exit 7",
        "ping -n 2 127.0.0.1 >NUL& echo last words& exit 7",
    );
    let terminal = spec.terminal;
    snapshot(client.request(Request::Spawn(spec), WAIT));
    client.request(Request::Detach { terminal }, WAIT).unwrap();
    thread::sleep(Duration::from_millis(if cfg!(windows) { 3000 } else { 1200 }));
    let snap = snapshot(client.request(Request::Attach { terminal, cols: 80, rows: 24 }, WAIT));
    assert_eq!(snap.exited, Some(ExitStatus { code: Some(7), user_killed: false }));
    assert!(plain(&snap.tail).contains("last words"));
    client.request(Request::Forget { terminal }, WAIT).unwrap();
    assert!(client.request(Request::Attach { terminal, cols: 80, rows: 24 }, WAIT).is_err());
}

#[test]
fn reattaching_never_duplicates_or_loses_output() {
    let h = Harness::start();
    let client = h.client();
    let spec = script(
        &h.cwd(),
        "i=0; while [ $i -lt 400 ]; do echo line$i; i=$((i+1)); sleep 0.002; done; sleep 0.3",
        "for /L %i in (0,1,399) do @(echo line%i& ping -n 1 -w 1 127.0.0.1 >NUL)",
    );
    let terminal = spec.terminal;
    let mut first = View::new(&snapshot(client.request(Request::Spawn(spec), WAIT)));
    first.pump(&client, |v| v.contains("line50"));
    for _ in 0..5 {
        client.post(Request::Detach { terminal });
        thread::sleep(Duration::from_millis(40));
        let _ = snapshot(client.request(Request::Attach { terminal, cols: 80, rows: 24 }, WAIT));
    }
    let mut view =
        View::new(&snapshot(client.request(Request::Attach { terminal, cols: 80, rows: 24 }, WAIT)));
    view.pump(&client, |v| v.exited.is_some());
    // Every line once, in order: read as the `lineN` words, since ConPTY may move the cursor
    // rather than write a newline.
    let text = plain(&view.text);
    let seen: Vec<&str> =
        text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| w.starts_with("line")).collect();
    let expected: Vec<String> = (0..400).map(|i| format!("line{i}")).collect();
    assert_eq!(seen, expected);
}

#[cfg(unix)]
#[test]
fn kill_ends_the_whole_process_group() {
    let h = Harness::start();
    let client = h.client();
    let spec = spec(&h.cwd(), "/bin/sh", &["-c", "sleep 300 & echo BG=$!; wait"]);
    let terminal = spec.terminal;
    let mut view = View::new(&snapshot(client.request(Request::Spawn(spec), WAIT)));
    view.pump(&client, |v| v.contains("BG=") && String::from_utf8_lossy(&v.text).contains('\n'));
    let text = String::from_utf8_lossy(&view.text).into_owned();
    let bg: u32 = text.split("BG=").nth(1).unwrap().trim().lines().next().unwrap().trim().parse().unwrap();
    let Reply::Terminals(list) = client.request(Request::List, WAIT).unwrap() else { panic!() };
    let shell = list.iter().find(|t| t.terminal == terminal).unwrap().pid.unwrap();

    client.request(Request::Kill { terminal }, WAIT).unwrap();
    view.pump(&client, |v| v.exited.is_some());
    assert!(view.exited.unwrap().user_killed);
    let deadline = Instant::now() + WAIT;
    while !(throng_daemon::unix::process_gone(bg) && throng_daemon::unix::process_gone(shell)) {
        assert!(Instant::now() < deadline, "shell {shell} or background job {bg} outlived the kill");
        thread::sleep(Duration::from_millis(20));
    }
    let Reply::Terminals(list) = client.request(Request::List, WAIT).unwrap() else { panic!() };
    assert!(list.iter().all(|t| t.terminal != terminal), "a user-killed session is forgotten");
}

/// An interactive shell whose prompt is known, and a command that keeps it busy.
fn busy_shell(cwd: &Path) -> Option<(SpawnSpec, &'static str, &'static [u8])> {
    if cfg!(windows) {
        return Some((spec(cwd, "cmd.exe", &["/d"]), ">", b"ping -n 30 127.0.0.1\r"));
    }
    let bash = ["/bin/bash", "/usr/bin/bash"].into_iter().find(|p| Path::new(p).exists())?;
    Some((spec(cwd, bash, &["--norc", "--noprofile", "-i"]), "$ ", b"sleep 30\r"))
}

fn busy(client: &Client, terminal: TerminalId) -> bool {
    let Reply::Terminals(list) = client.request(Request::List, WAIT).unwrap() else { panic!() };
    list.iter().find(|t| t.terminal == terminal).is_some_and(|t| t.busy)
}

fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn busy_means_a_command_holds_the_foreground_and_close_idle_spares_it() {
    let h = Harness::start();
    let Some((busy_spec, prompt, command)) = busy_shell(&h.cwd()) else { return };
    let Some((idle_spec, _, _)) = busy_shell(&h.cwd()) else { return };
    let client = h.client();
    let (busy_id, idle_id) = (busy_spec.terminal, idle_spec.terminal);
    let mut busy_view = View::new(&snapshot(client.request(Request::Spawn(busy_spec), WAIT)));
    let mut idle_view = View::new(&snapshot(client.request(Request::Spawn(idle_spec), WAIT)));
    // Both prompts are waited on together: the idle shell's may come first.
    pump_all(&client, &mut [&mut busy_view, &mut idle_view], |v| v.contains(prompt));
    assert!(!busy(&client, busy_id));

    client.write(busy_id, command);
    wait_for(|| busy(&client, busy_id), "the sleep to take the foreground");
    assert!(!busy(&client, idle_id));

    let Reply::Closed(closed) =
        client.request(Request::CloseIdle { terminals: vec![busy_id, idle_id] }, WAIT).unwrap()
    else {
        panic!()
    };
    assert_eq!(closed, vec![idle_id]);
    assert!(busy(&client, busy_id), "the busy terminal keeps running");

    client.write(busy_id, b"\x03");
    wait_for(|| !busy(&client, busy_id), "Ctrl+C to return the shell to its prompt");
}

#[test]
fn startup_command_runs_once_after_first_output_and_never_on_reattach() {
    let h = Harness::start();
    let client = h.client();
    let mut spec = interactive(&h.cwd());
    // The shell works it out, so the command as typed never reads "started-42".
    spec.startup_command =
        Some(if cfg!(windows) { "echo started-4^2" } else { "echo started-$((20+22))" }.into());
    let terminal = spec.terminal;
    let mut view = View::new(&snapshot(client.request(Request::Spawn(spec), WAIT)));
    view.pump(&client, |v| v.contains("started-42"));
    thread::sleep(Duration::from_millis(300));
    view.pump(&client, |_| true);
    assert_eq!(view.count("started-42"), 1);

    client.request(Request::Detach { terminal }, WAIT).unwrap();
    let mut again =
        View::new(&snapshot(client.request(Request::Attach { terminal, cols: 80, rows: 24 }, WAIT)));
    thread::sleep(Duration::from_millis(300));
    again.pump(&client, |_| true);
    assert_eq!(again.count("started-42"), 1, "a reattach must not re-run the startup command");
    client.request(Request::Kill { terminal }, WAIT).unwrap();
}

#[test]
fn keystrokes_arrive_in_the_order_typed() {
    let h = Harness::start();
    let client = h.client();
    // cat echoes the line back; cmd.exe names it in the error for an unknown command. On Windows
    // the line stays short enough not to wrap.
    let (spec, length) =
        if cfg!(windows) { (interactive(&h.cwd()), 50) } else { (spec(&h.cwd(), "/bin/cat", &[]), 400) };
    let terminal = spec.terminal;
    let mut view = View::new(&snapshot(client.request(Request::Spawn(spec), WAIT)));
    let typed: String = (0..length).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    for byte in typed.bytes() {
        client.write(terminal, &[byte]);
    }
    client.write(terminal, if cfg!(windows) { b"\r" } else { b"\n" });
    view.pump(&client, |v| v.count(&typed) >= 2);
    client.request(Request::Kill { terminal }, WAIT).unwrap();
}

/// What asks a shell for its terminal's size: `stty size` prints "rows cols"; cmd.exe's
/// `mode con` prints "Lines: rows Columns: cols".
const SIZE_QUERY: &[u8] = if cfg!(windows) { b"mode con\r" } else { b"stty size\n" };

/// What [`SIZE_QUERY`] prints for a size, whitespace squeezed.
fn size_report(rows: u16, cols: u16) -> String {
    if cfg!(windows) { format!("Lines: {rows} Columns: {cols}") } else { format!("{rows} {cols}") }
}

#[test]
fn the_pty_takes_the_smallest_attached_view() {
    let h = Harness::start();
    let a = h.client();
    let b = h.client();
    let mut spec = interactive(&h.cwd());
    spec.cols = 100;
    spec.rows = 30;
    let terminal = spec.terminal;
    let (ask, size) = (SIZE_QUERY, size_report);
    let mut view = View::new(&snapshot(a.request(Request::Spawn(spec), WAIT)));
    snapshot(b.request(Request::Attach { terminal, cols: 70, rows: 20 }, WAIT));
    a.write(terminal, ask);
    view.pump(&a, |v| v.squeezed().contains(&size(20, 70)));
    b.request(Request::Detach { terminal }, WAIT).unwrap();
    a.write(terminal, ask);
    view.pump(&a, |v| v.squeezed().contains(&size(30, 100)));
    // A resize from a view that is no longer attached must not pin the size.
    b.post(Request::Resize { terminal, cols: 10, rows: 5 });
    a.write(terminal, ask);
    view.pump(&a, |v| v.squeezed().matches(&size(30, 100)).count() >= 2);
    assert!(!view.squeezed().contains(&size(5, 10)));
    a.request(Request::Kill { terminal }, WAIT).unwrap();
}

#[test]
fn a_bad_frame_closes_only_that_client() {
    let h = Harness::start();
    let endpoint = Endpoint::for_dirs(&h.dirs);
    let raw = endpoint.connect().unwrap();
    use interprocess::local_socket::traits::Stream as _;
    let (mut recv, mut send) = raw.split();
    write_frame(
        &mut send,
        &ClientMsg::Hello { protocol: throng_protocol::PROTOCOL_VERSION, build: "t".into(), pid: 1 },
    )
    .unwrap();
    assert!(matches!(read_frame::<ServerMsg>(&mut recv).unwrap(), ServerMsg::Welcome { .. }));
    use std::io::Write as _;
    send.write_all(&[3, 0, 0, 0, 0xFF, 0xFF, 0xFF]).unwrap();
    assert!(matches!(read_frame::<ServerMsg>(&mut recv), Err(FrameError::Closed)));

    let client = h.client();
    assert_eq!(client.request(Request::Ping, WAIT).unwrap(), Reply::Pong);
}

#[test]
fn a_different_protocol_is_told_the_version_then_closed() {
    let h = Harness::start();
    let raw = Endpoint::for_dirs(&h.dirs).connect().unwrap();
    use interprocess::local_socket::traits::Stream as _;
    let (mut recv, mut send) = raw.split();
    write_frame(&mut send, &ClientMsg::Hello { protocol: 999, build: "future".into(), pid: 1 }).unwrap();
    match read_frame::<ServerMsg>(&mut recv).unwrap() {
        ServerMsg::Welcome { protocol, .. } => assert_eq!(protocol, throng_protocol::PROTOCOL_VERSION),
        other => panic!("{other:?}"),
    }
    assert!(matches!(read_frame::<ServerMsg>(&mut recv), Err(FrameError::Closed)));
}

#[test]
fn pending_requests_fail_fast_when_the_daemon_goes() {
    let h = Harness::start();
    let client = h.client();
    client.request(Request::Shutdown { kill_all: true }, WAIT).unwrap();
    let deadline = Instant::now() + WAIT;
    while client.is_connected() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    assert_eq!(client.request(Request::Ping, Duration::from_secs(30)), Err(RequestError::Disconnected));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn the_daemon_exits_when_idle_and_refuses_a_second_instance() {
    let mut h = Harness::start_with(|c| c.idle_exit = Duration::from_millis(300));
    {
        let dirs = h.dirs.clone();
        let second = run(DaemonConfig::new(dirs));
        assert!(matches!(second, Err(RunError::AlreadyRunning)));
    }
    let client = h.client();
    assert_eq!(client.request(Request::Ping, WAIT).unwrap(), Reply::Pong);
    drop(client);
    let daemon = h.daemon.take().unwrap();
    let deadline = Instant::now() + WAIT;
    while !daemon.is_finished() {
        assert!(Instant::now() < deadline, "idle daemon never exited");
        thread::sleep(Duration::from_millis(20));
    }
    daemon.join().unwrap().unwrap();
    #[cfg(unix)]
    assert!(!h.dirs.daemon_socket().exists(), "the socket file is removed on exit");
}

#[test]
fn shutdown_without_kill_all_refuses_while_terminals_run() {
    let h = Harness::start();
    let client = h.client();
    let spec = script(&h.cwd(), "sleep 30", "ping -n 30 127.0.0.1 >NUL");
    let terminal = spec.terminal;
    snapshot(client.request(Request::Spawn(spec), WAIT));
    assert!(matches!(
        client.request(Request::Shutdown { kill_all: false }, WAIT),
        Err(RequestError::Refused(_))
    ));
    client.request(Request::Kill { terminal }, WAIT).unwrap();
}

#[cfg(unix)]
#[test]
fn a_daemon_whose_socket_disappears_ends_its_terminals_and_exits() {
    let mut h = Harness::start();
    let client = h.client();
    let spec = spec(&h.cwd(), "/bin/sh", &["-c", "sleep 300"]);
    let terminal = spec.terminal;
    snapshot(client.request(Request::Spawn(spec), WAIT));
    let Reply::Terminals(list) = client.request(Request::List, WAIT).unwrap() else { panic!() };
    let shell = list.iter().find(|t| t.terminal == terminal).unwrap().pid.unwrap();
    drop(client);

    std::fs::remove_file(h.dirs.daemon_socket()).unwrap();
    let daemon = h.daemon.take().unwrap();
    let deadline = Instant::now() + WAIT;
    while !daemon.is_finished() {
        assert!(Instant::now() < deadline, "an unreachable daemon must not linger");
        thread::sleep(Duration::from_millis(20));
    }
    daemon.join().unwrap().unwrap();
    wait_for(|| throng_daemon::unix::process_gone(shell), "the orphaned shell to be ended");
}
