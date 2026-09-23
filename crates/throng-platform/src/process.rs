//! Processes: detached spawning, liveness, elevation, working directories.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Start `program` fully detached from this process: its own session (Unix) or process group
/// (Windows), no inherited stdio, so closing the UI's terminal or the UI itself cannot take it down.
/// Returns its pid.
pub fn spawn_detached<I, S>(program: &Path, args: I, envs: &[(String, String)]) -> io::Result<u32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    detach(&mut command);
    let child = command.spawn()?;
    Ok(child.id())
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    #[allow(unsafe_code)]
    // SAFETY: the closure runs in the forked child before exec and only calls setsid, which is
    // async-signal-safe and touches no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

/// The working directory of process `pid`, read from outside it: a shell's current folder, with no
/// cooperation from the shell (its live working directory). `None` where it cannot be read.
#[must_use]
pub fn working_directory(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
    #[cfg(target_os = "macos")]
    {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
        #[allow(unsafe_code)]
        // SAFETY: the buffer is a zeroed `proc_vnodepathinfo` of exactly the size passed, which is
        // what this flavour writes; the call writes at most `size` bytes and reports how many.
        let written = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr().cast(), size)
        };
        if written != size {
            return None;
        }
        #[allow(unsafe_code)]
        // SAFETY: the call filled the whole struct (checked above); a zeroed struct is valid too.
        let info = unsafe { info.assume_init() };
        let bytes: Vec<u8> =
            info.pvi_cdir.vip_path.iter().flatten().map(|c| *c as u8).take_while(|b| *b != 0).collect();
        use std::os::unix::ffi::OsStringExt;
        (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// This machine's name, as a shell reporting its directory would write it (`file://<host>/…`).
#[must_use]
pub fn hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        #[allow(unsafe_code)]
        // SAFETY: the buffer is writable for its whole length, which is what is passed.
        let result = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if result != 0 {
            return None;
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        String::from_utf8(buf[..end].to_vec()).ok().filter(|h| !h.is_empty())
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").ok()
    }
}

/// Whether a process with this pid exists. A permission error means it exists but belongs to
/// someone else — reading that as "dead" once made helper processes kill themselves.
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
        #[allow(unsafe_code)]
        // SAFETY: kill with signal 0 performs only the existence and permission check.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        win::pid_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Ask a process to exit (SIGTERM on Unix).
pub fn terminate(pid: u32) {
    #[cfg(unix)]
    {
        if let Ok(pid) = libc::pid_t::try_from(pid) {
            #[allow(unsafe_code)]
            // SAFETY: kill only sends a signal; a stale pid fails with ESRCH.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status();
    }
}

/// Whether this process runs with administrator/root rights.
#[must_use]
pub fn is_elevated() -> bool {
    #[cfg(unix)]
    {
        #[allow(unsafe_code)]
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        euid == 0
    }
    #[cfg(windows)]
    {
        win::is_elevated()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// Whether the shell `pid` is running a command: it has a child process other than a console host.
/// `None` where that cannot be read. (On Unix the terminal's foreground process group answers
/// this instead; see the daemon.)
#[must_use]
pub fn runs_a_command(pid: u32) -> Option<bool> {
    #[cfg(windows)]
    {
        win::children(pid).map(|kids| kids.iter().any(|name| !is_console_host(name)))
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

/// The console hosts Windows starts beside a console program; they are not commands.
#[must_use]
pub fn is_console_host(exe: &str) -> bool {
    ["conhost.exe", "openconsole.exe"].iter().any(|h| exe.eq_ignore_ascii_case(h))
}

/// A process and everything it starts, ended together (Windows: a job object that kills its
/// processes when closed, so a shell's commands cannot outlive their terminal or the daemon; Unix
/// gets the same from sessions and SIGHUP, so this holds nothing there).
pub struct ProcessTree {
    #[cfg(windows)]
    job: win::Job,
}

impl ProcessTree {
    /// Take `pid` and whatever it starts from now on into one tree.
    pub fn adopt(pid: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self { job: win::Job::adopt(pid)? })
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            Ok(Self {})
        }
    }

    /// End every process in the tree now.
    pub fn terminate(&self) {
        #[cfg(windows)]
        self.job.terminate();
    }
}

/// The current user's name, for display only.
#[must_use]
pub fn user_name() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".to_owned())
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    //! The Win32 calls behind the process functions. Every handle opened here is closed here.

    use std::io;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE,
    };
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// A handle closed on drop.
    struct Owned(HANDLE);

    impl Owned {
        fn new(handle: HANDLE) -> io::Result<Self> {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(handle))
            }
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle was returned open by the call that made this value, and is closed
            // exactly once, here.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub fn pid_alive(pid: u32) -> bool {
        // SAFETY: OpenProcess takes no pointers; a failure returns null, handled below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        let Ok(process) = Owned::new(handle) else {
            // Someone else's process exists but refuses us (that is not "dead").
            // SAFETY: reads the calling thread's last error, set by the failed call above.
            return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
        };
        let mut code = 0u32;
        // SAFETY: the handle is open with query rights and `code` is a valid place to write.
        let ok = unsafe { GetExitCodeProcess(process.0, &raw mut code) };
        ok != 0 && code == STILL_ACTIVE as u32
    }

    pub fn is_elevated() -> bool {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing; `token` is a
        // valid place for the opened token handle.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
        if opened == 0 {
            return false;
        }
        let Ok(token) = Owned::new(token) else { return false };
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut written = 0u32;
        let size = u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>()).unwrap_or(0);
        // SAFETY: the buffer is a TOKEN_ELEVATION of exactly `size` bytes, which is what this
        // information class writes.
        let ok = unsafe {
            GetTokenInformation(token.0, TokenElevation, (&raw mut elevation).cast(), size, &raw mut written)
        };
        ok != 0 && elevation.TokenIsElevated != 0
    }

    /// The executable names of `pid`'s child processes.
    pub fn children(pid: u32) -> Option<Vec<String>> {
        // SAFETY: takes no pointers; failure returns INVALID_HANDLE_VALUE, handled by Owned::new.
        let snapshot = Owned::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).ok()?,
            ..PROCESSENTRY32W::default()
        };
        let mut out = Vec::new();
        // SAFETY: the snapshot is open and `entry` is a PROCESSENTRY32W whose dwSize is set, as
        // both calls require.
        let mut more = unsafe { Process32FirstW(snapshot.0, &raw mut entry) } != 0;
        while more {
            if entry.th32ParentProcessID == pid && entry.th32ProcessID != pid {
                let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
                out.push(String::from_utf16_lossy(&entry.szExeFile[..len]));
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.0, &raw mut entry) } != 0;
        }
        Some(out)
    }

    /// A job object that kills its processes when closed.
    pub struct Job(Owned);

    // SAFETY: a job handle is a kernel object reference, usable from any thread; this type only
    // ever terminates or closes it.
    unsafe impl Send for Job {}
    // SAFETY: as above; the calls made through a shared reference are thread-safe in Win32.
    unsafe impl Sync for Job {}

    impl Job {
        pub fn adopt(pid: u32) -> io::Result<Self> {
            // SAFETY: null attributes and name ask for an unnamed job with default security.
            let job = Owned::new(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .map_err(io::Error::other)?;
            // SAFETY: the job is open, and `limits` is the structure this class reads, of `size`.
            let set = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size,
                )
            };
            if set == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: takes no pointers; failure returns null, handled by Owned::new.
            let process = Owned::new(unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) })?;
            // SAFETY: both handles are open with the rights assignment needs.
            if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(job))
        }

        pub fn terminate(&self) {
            // SAFETY: the job handle is open for as long as self lives.
            unsafe {
                TerminateJobObject(self.0.0, 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_process_working_directory_is_read_from_outside() {
        let here = std::env::current_dir().unwrap();
        let found = super::working_directory(std::process::id()).expect("our own directory");
        assert_eq!(std::fs::canonicalize(found).unwrap(), std::fs::canonicalize(here).unwrap());
    }

    use super::*;

    #[test]
    fn this_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn console_hosts_are_not_commands() {
        assert!(is_console_host("conhost.exe") && is_console_host("OpenConsole.exe"));
        assert!(!is_console_host("node.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn a_shells_command_is_seen_and_ending_its_tree_ends_the_command_too() {
        // cmd runs ping as its child for some seconds: a command, as a shell would run one.
        let mut shell = Command::new("cmd.exe").args(["/c", "ping -n 30 127.0.0.1 >NUL"]).spawn().unwrap();
        let pid = shell.id();
        let tree = ProcessTree::adopt(pid).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while runs_a_command(pid) != Some(true) {
            assert!(std::time::Instant::now() < deadline, "ping never appeared under cmd");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let command = win::children(pid).unwrap();
        assert!(command.iter().any(|n| n.eq_ignore_ascii_case("PING.EXE")), "{command:?}");
        tree.terminate();
        shell.wait().unwrap();
        assert!(!pid_alive(pid));
        assert_eq!(runs_a_command(pid), Some(false), "nothing is left under it");
    }

    #[cfg(windows)]
    #[test]
    fn a_finished_process_is_dead() {
        let mut child = Command::new("cmd.exe").args(["/c", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // The handle `child` holds keeps the process object; drop it first.
        drop(child);
        assert!(!pid_alive(pid));
    }

    #[cfg(unix)]
    #[test]
    fn a_reaped_child_is_dead() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_alive(pid));
    }

    // `ps -o sid=` is procps; macOS's ps has no session-id column.
    #[cfg(target_os = "linux")]
    #[test]
    fn detached_children_lead_their_own_session() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("sid");
        let script = format!("ps -o sid= -p $$ > '{}'; echo $$ >> '{}'", out.display(), out.display());
        spawn_detached(Path::new("/bin/sh"), ["-c", &script], &[]).unwrap();
        let mut text = String::new();
        for _ in 0..100 {
            text = std::fs::read_to_string(&out).unwrap_or_default();
            if text.lines().count() >= 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut lines = text.lines().map(str::trim);
        let sid = lines.next().unwrap_or_default();
        let pid = lines.next().unwrap_or_default();
        assert!(!pid.is_empty(), "child never wrote its pid");
        assert_eq!(sid, pid, "the detached child should be its own session leader");
    }
}
