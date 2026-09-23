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
    #[cfg(windows)]
    {
        win::current_directory(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = pid;
        None
    }
}

/// A process snapshot, taken once for every terminal that needs one: on Linux the parent and
/// start of every process; on Windows the parent and program of every process; on macOS children
/// are asked for one shell at a time, which is as cheap.
#[derive(Debug, Default)]
pub struct ProcessTable {
    /// (pid, parent, start) of every process.
    #[cfg(target_os = "linux")]
    entries: Vec<(u32, u32, u64)>,
    /// (pid, parent, program) of every process.
    #[cfg(windows)]
    entries: Vec<(u32, u32, String)>,
}

impl ProcessTable {
    #[must_use]
    pub fn snapshot() -> Self {
        #[cfg(target_os = "linux")]
        {
            let entries = std::fs::read_dir("/proc")
                .map(|dir| {
                    dir.filter_map(Result::ok)
                        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
                        .filter_map(|pid| {
                            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                            let (parent, started) = linux_stat(&stat)?;
                            Some((pid, parent, started))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Self { entries }
        }
        #[cfg(windows)]
        {
            Self { entries: win::processes().unwrap_or_default() }
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            Self::default()
        }
    }

    /// The command `shell` is running, as a startup command (see
    /// [`throng_core::terminal::running_command`]). `None` when nothing runs or it cannot be read.
    #[must_use]
    pub fn running_command(&self, shell: u32) -> Option<String> {
        #[cfg(windows)]
        {
            // A Windows program parses its own command line, so the line is kept as it was. The
            // console hosts Windows starts beside a console program are not commands.
            let shell_line = win::command_line(shell)?;
            let children: Vec<throng_core::terminal::ShellChild> = self
                .entries
                .iter()
                .filter(|(pid, parent, exe)| *parent == shell && *pid != shell && !is_console_host(exe))
                .filter_map(|(pid, _, _)| {
                    Some(throng_core::terminal::ShellChild {
                        pid: *pid,
                        started: win::started(*pid)?,
                        argv: vec![win::command_line(*pid)?],
                    })
                })
                .collect();
            throng_core::terminal::running_command_line(&shell_line, &children)
        }
        #[cfg(not(windows))]
        {
            self.running_argv(shell)
        }
    }

    #[cfg(not(windows))]
    fn running_argv(&self, shell: u32) -> Option<String> {
        let shell_argv = command_line(shell)?;
        let children: Vec<throng_core::terminal::ShellChild> = self
            .children(shell)
            .into_iter()
            .filter_map(|(pid, started)| {
                Some(throng_core::terminal::ShellChild { pid, started, argv: command_line(pid)? })
            })
            .collect();
        throng_core::terminal::running_command(&shell_argv, &children)
    }

    /// `shell`'s direct children, with when each started.
    #[cfg(not(windows))]
    fn children(&self, shell: u32) -> Vec<(u32, u64)> {
        #[cfg(target_os = "linux")]
        {
            self.entries
                .iter()
                .filter(|(_, parent, _)| *parent == shell)
                .map(|(pid, _, s)| (*pid, *s))
                .collect()
        }
        #[cfg(target_os = "macos")]
        {
            mac::children(shell)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = shell;
            Vec::new()
        }
    }
}

/// The parent pid and start time (clock ticks since boot) in a `/proc/<pid>/stat` line. The
/// command name is parenthesised and may itself hold spaces and parentheses, so fields are counted
/// from the last `)`.
#[cfg(any(target_os = "linux", test))]
fn linux_stat(stat: &str) -> Option<(u32, u64)> {
    let fields: Vec<&str> = stat.get(stat.rfind(')')? + 1..)?.split_whitespace().collect();
    // After the name: state, ppid, … and starttime is the 20th.
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

/// The arguments process `pid` was started with. `None` where they cannot be read.
#[must_use]
pub fn command_line(pid: u32) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let argv: Vec<String> = bytes
            .split(|b| *b == 0)
            .filter(|word| !word.is_empty())
            .map(|word| String::from_utf8_lossy(word).into_owned())
            .collect();
        (!argv.is_empty()).then_some(argv)
    }
    #[cfg(target_os = "macos")]
    {
        mac::command_line(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mac {
    //! macOS process introspection: libproc for children and start times, `KERN_PROCARGS2` for
    //! arguments.

    pub fn children(shell: u32) -> Vec<(u32, u64)> {
        let Ok(shell) = libc::pid_t::try_from(shell) else { return Vec::new() };
        let mut pids = vec![0 as libc::pid_t; 256];
        let bytes = libc::c_int::try_from(pids.len() * std::mem::size_of::<libc::pid_t>()).unwrap_or(0);
        // SAFETY: the buffer holds `bytes` bytes of pids, which is the size passed; the call writes
        // at most that and returns how many pids it wrote (libproc divides the byte count).
        let count = unsafe { libc::proc_listchildpids(shell, pids.as_mut_ptr().cast(), bytes) };
        // Read either way, a count is safe: unwritten slots stay zero and are dropped below.
        pids.truncate(usize::try_from(count).unwrap_or(0).min(pids.len()));
        pids.into_iter()
            .filter(|pid| *pid > 0)
            .filter_map(|pid| Some((u32::try_from(pid).ok()?, started(pid)?)))
            .collect()
    }

    /// When `pid` started, in microseconds since the epoch.
    fn started(pid: libc::pid_t) -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: the buffer is a zeroed `proc_bsdinfo` of exactly `size` bytes, which is what this
        // flavour writes; the call reports how many bytes it wrote.
        let written =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
        if written != size {
            return None;
        }
        // SAFETY: the call filled the whole struct (checked above); a zeroed one is valid too.
        let info = unsafe { info.assume_init() };
        Some(info.pbi_start_tvsec.saturating_mul(1_000_000).saturating_add(info.pbi_start_tvusec))
    }

    pub fn command_line(pid: u32) -> Option<Vec<String>> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, libc::c_int::try_from(pid).ok()?];
        let mut size: libc::size_t = 0;
        // SAFETY: a null buffer asks only for the size, written to `size`.
        let asked = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &raw mut size, std::ptr::null_mut(), 0)
        };
        if asked != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size];
        // SAFETY: the buffer is `size` bytes long, which is what is passed; the call writes at
        // most that and updates `size` to what it wrote.
        let read = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &raw mut size, std::ptr::null_mut(), 0)
        };
        if read != 0 {
            return None;
        }
        buf.truncate(size);
        parse_procargs(&buf)
    }

    /// `KERN_PROCARGS2`'s layout: argc as a native int, the executable path, NUL padding, then
    /// argc NUL-terminated arguments (the environment follows).
    fn parse_procargs(buf: &[u8]) -> Option<Vec<String>> {
        let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
        let rest = buf.get(4..)?;
        let exec_end = rest.iter().position(|b| *b == 0)?;
        let mut words = rest[exec_end..].split(|b| *b == 0).filter(|w| !w.is_empty());
        let argv: Vec<String> =
            (0..argc).map_while(|_| words.next()).map(|w| String::from_utf8_lossy(w).into_owned()).collect();
        (!argv.is_empty()).then_some(argv)
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

/// Whether this process can start others without its administrator rights. Only Windows can: an
/// elevated process there hands a child a normal user's rights instead. (A Unix process's rights
/// are its user's, and there is nothing to drop.)
#[must_use]
pub fn can_deelevate() -> bool {
    cfg!(windows) && is_elevated()
}

/// A process started by [`spawn_deelevated`], with its standard input and output piped to the
/// caller.
pub struct Piped {
    pub pid: u32,
    pub stdin: std::fs::File,
    pub stdout: std::fs::File,
}

/// Start `program` with `args` without this process's administrator rights: on Windows, with a
/// normal user's token at medium integrity (what a program started from Explorer gets), and with
/// no console. Its standard error goes nowhere. Fails where [`can_deelevate`] is false.
pub fn spawn_deelevated(program: &Path, args: &[&str]) -> io::Result<Piped> {
    #[cfg(windows)]
    {
        let mut line = quote_windows_arg(&program.to_string_lossy());
        for arg in args {
            line.push(' ');
            line.push_str(&quote_windows_arg(arg));
        }
        let (pid, stdin, stdout) = win::spawn_deelevated(&line)?;
        Ok(Piped { pid, stdin: stdin.into(), stdout: stdout.into() })
    }
    #[cfg(not(windows))]
    {
        let _ = (program, args);
        Err(io::Error::new(io::ErrorKind::Unsupported, "only Windows can start a process without its rights"))
    }
}

/// One argument, quoted for a Windows command line so that the C runtime splits it back out
/// unchanged.
#[cfg(any(windows, test))]
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are escapes: double them, then escape the quote.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Before the closing quote they would escape it, so they are doubled too.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
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

    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, FILETIME, GetLastError, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE,
        UNICODE_STRING,
    };
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation};
    use windows_sys::Win32::Security::AppLocker::{
        SAFER_LEVEL_OPEN, SAFER_LEVELID_NORMALUSER, SAFER_SCOPEID_USER, SaferCloseLevel,
        SaferComputeTokenFromLevel, SaferCreateLevel,
    };
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{
        GetLengthSid, GetTokenInformation, PSID, SAFER_LEVEL_HANDLE, SID_AND_ATTRIBUTES, SetTokenInformation,
        TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenIntegrityLevel,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
    use windows_sys::Win32::System::Threading::{
        CreateProcessAsUserW, DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        PROCESS_VM_READ,
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

    impl Owned {
        /// Hand the handle on to std, which closes it from then on.
        fn into_std(self) -> std::os::windows::io::OwnedHandle {
            use std::os::windows::io::FromRawHandle;
            let handle = self.0;
            std::mem::forget(self);
            // SAFETY: the handle is open and owned by this value alone, which no longer closes it.
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) }
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

    /// Every process: its pid, its parent's, and its program's file name.
    pub fn processes() -> Option<Vec<(u32, u32, String)>> {
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
            let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            out.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..len]),
            ));
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.0, &raw mut entry) } != 0;
        }
        Some(out)
    }

    /// When `pid` started (100 ns units since 1601).
    pub fn started(pid: u32) -> Option<u64> {
        // SAFETY: OpenProcess takes no pointers; a failure returns null, handled by Owned::new.
        let process = Owned::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }).ok()?;
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the handle is open with query rights, and each pointer is a FILETIME to write.
        let ok = unsafe {
            GetProcessTimes(process.0, &raw mut created, &raw mut exited, &raw mut kernel, &raw mut user)
        };
        (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    /// The command line `pid` was started with, as it was written.
    pub fn command_line(pid: u32) -> Option<String> {
        // SAFETY: OpenProcess takes no pointers; a failure returns null, handled by Owned::new.
        let process = Owned::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }).ok()?;
        let mut needed = 0u32;
        // SAFETY: a zero-length query only reports, through `needed`, the size the answer takes.
        unsafe {
            NtQueryInformationProcess(
                process.0,
                ProcessCommandLineInformation,
                std::ptr::null_mut(),
                0,
                &raw mut needed,
            );
        }
        if needed == 0 {
            return None;
        }
        // Aligned for the UNICODE_STRING the answer starts with; its text follows in the buffer.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let size = u32::try_from(buffer.len() * 8).ok()?;
        // SAFETY: the buffer is writable for `size` bytes, which is what is passed.
        let status = unsafe {
            NtQueryInformationProcess(
                process.0,
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                size,
                &raw mut needed,
            )
        };
        if status < 0 {
            return None;
        }
        // SAFETY: a successful query leaves a UNICODE_STRING at the start of the buffer, whose
        // text lies inside the same buffer for `Length` bytes.
        let text = unsafe {
            let header = &*buffer.as_ptr().cast::<UNICODE_STRING>();
            if header.Buffer.is_null() {
                return None;
            }
            std::slice::from_raw_parts(header.Buffer, usize::from(header.Length) / 2)
        };
        Some(String::from_utf16_lossy(text))
    }

    /// `NtQueryInformationProcess`'s basic information, as laid out on 64-bit Windows.
    #[repr(C)]
    #[derive(Default)]
    struct BasicInformation {
        exit_status: i32,
        peb: usize,
        affinity: usize,
        priority: i32,
        pid: usize,
        parent: usize,
    }

    /// A counted UTF-16 string in another process: its length in bytes, and where its text is.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RemoteString {
        length: u16,
        maximum: u16,
        text: usize,
    }

    /// Read a `T` from another process's memory.
    fn read<T: Copy + Default>(process: HANDLE, at: usize) -> Option<T> {
        let mut value = T::default();
        let mut done = 0usize;
        // SAFETY: `value` is a `T` to write, of the size passed; the call writes at most that and
        // reports how much.
        let ok = unsafe {
            ReadProcessMemory(
                process,
                at as *const _,
                (&raw mut value).cast(),
                std::mem::size_of::<T>(),
                &raw mut done,
            )
        };
        (ok != 0 && done == std::mem::size_of::<T>()).then_some(value)
    }

    /// A process's current directory, read from outside it: its process parameters block holds it
    /// as the shell keeps it (64-bit processes; the offsets are the 64-bit ones).
    pub fn current_directory(pid: u32) -> Option<std::path::PathBuf> {
        if cfg!(not(target_pointer_width = "64")) {
            return None;
        }
        let rights = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ;
        // SAFETY: OpenProcess takes no pointers; a failure returns null, handled by Owned::new.
        let process = Owned::new(unsafe { OpenProcess(rights, 0, pid) }).ok()?;
        let mut info = BasicInformation::default();
        let mut written = 0u32;
        let size = u32::try_from(std::mem::size_of::<BasicInformation>()).ok()?;
        // SAFETY: `info` is the structure this class writes, of exactly `size` bytes.
        let status = unsafe {
            NtQueryInformationProcess(
                process.0,
                ProcessBasicInformation,
                (&raw mut info).cast(),
                size,
                &raw mut written,
            )
        };
        if status < 0 || info.peb == 0 {
            return None;
        }
        // PEB.ProcessParameters is at 0x20; its CurrentDirectory.DosPath at 0x38.
        let parameters: usize = read(process.0, info.peb + 0x20)?;
        let path: RemoteString = read(process.0, parameters + 0x38)?;
        let units = usize::from(path.length) / 2;
        if path.text == 0 || units == 0 {
            return None;
        }
        let mut text = vec![0u16; units];
        let mut done = 0usize;
        // SAFETY: the buffer holds `units` UTF-16 units, which is the byte count passed.
        let ok = unsafe {
            ReadProcessMemory(
                process.0,
                path.text as *const _,
                text.as_mut_ptr().cast(),
                units * 2,
                &raw mut done,
            )
        };
        if ok == 0 || done != units * 2 {
            return None;
        }
        let text = String::from_utf16_lossy(&text);
        // Kept with a trailing backslash, which only a drive's root needs.
        let trimmed = if text.len() > 3 { text.trim_end_matches('\\') } else { text.as_str() };
        Some(std::path::PathBuf::from(trimmed))
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

    /// De-elevated spawns make their pipe ends inheritable for a moment; one at a time, so that no
    /// child is ever handed another's pipe.
    static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Start `command_line` with a normal user's token at medium integrity and no console, its
    /// standard input and output piped. Returns its pid, then the ends for writing its input and
    /// reading its output.
    pub fn spawn_deelevated(
        command_line: &str,
    ) -> io::Result<(u32, std::os::windows::io::OwnedHandle, std::os::windows::io::OwnedHandle)> {
        let token = normal_user_token()?;
        let _one_at_a_time = SPAWNING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (child_in, our_in) = pipe()?;
        let (our_out, child_out) = pipe()?;
        for end in [&child_in, &child_out] {
            // SAFETY: the handle is open; this only sets its inherit flag.
            if unsafe { SetHandleInformation(end.0, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }

        // Only the two pipe ends are inherited, whatever else this process holds inheritable.
        let mut size = 0usize;
        // SAFETY: a null list asks only for the size one needs; that call fails by design.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut size) };
        let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>()).max(1)];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = storage.as_mut_ptr().cast();
        // SAFETY: `storage` holds at least `size` bytes, aligned for the pointers the list holds.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &raw mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let list = AttributeList(list);
        let inherited = [child_in.0, child_out.0];
        // SAFETY: the list was made for one attribute, and `inherited` outlives every use of it
        // (the process is created below, before either is dropped).
        let updated = unsafe {
            UpdateProcThreadAttribute(
                list.0,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited.as_ptr().cast(),
                std::mem::size_of_val(&inherited),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb =
            u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).map_err(io::Error::other)?;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = child_in.0;
        startup.StartupInfo.hStdOutput = child_out.0;
        startup.lpAttributeList = list.0;
        let mut line: Vec<u16> = command_line.encode_utf16().chain([0]).collect();
        let mut process = PROCESS_INFORMATION::default();
        // SAFETY: the token is a primary token restricted from this process's own, which needs no
        // extra privilege to assign; `line` is a writable NUL-terminated string, as the call
        // requires; `startup` is a STARTUPINFOEXW whose size and attribute list are set (with
        // EXTENDED_STARTUPINFO_PRESENT); the environment and directory are this process's (null).
        let created = unsafe {
            CreateProcessAsUserW(
                token.0,
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | DETACHED_PROCESS,
                std::ptr::null(),
                std::ptr::null(),
                (&raw const startup).cast(),
                &raw mut process,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        drop(Owned(process.hThread));
        drop(Owned(process.hProcess));
        drop(list);
        drop(storage);
        Ok((process.dwProcessId, our_in.into_std(), our_out.into_std()))
    }

    /// An initialised attribute list, deleted on drop (before its storage is freed).
    struct AttributeList(LPPROC_THREAD_ATTRIBUTE_LIST);

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: the list was initialised and is deleted exactly once, here.
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }

    /// An anonymous pipe: its read end, then its write end. Neither is inheritable.
    fn pipe() -> io::Result<(Owned, Owned)> {
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        // SAFETY: both are valid places for the new handles; null attributes make them
        // non-inheritable; zero asks for the default buffer size.
        if unsafe { CreatePipe(&raw mut read, &raw mut write, std::ptr::null(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((Owned::new(read)?, Owned::new(write)?))
    }

    /// This process's token, restricted to a normal user's rights (Safer's normal-user level:
    /// administrator groups deny-only, administrator privileges gone) at medium integrity.
    fn normal_user_token() -> io::Result<Owned> {
        let mut level: SAFER_LEVEL_HANDLE = std::ptr::null_mut();
        // SAFETY: `level` is a valid place for the opened level; the reserved argument is null.
        let opened = unsafe {
            SaferCreateLevel(
                SAFER_SCOPEID_USER,
                SAFER_LEVELID_NORMALUSER,
                SAFER_LEVEL_OPEN,
                &raw mut level,
                std::ptr::null(),
            )
        };
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the level is open; a null input token means this process's own; the output is
        // a valid place for the new token; the reserved argument is null.
        let computed = unsafe {
            SaferComputeTokenFromLevel(level, std::ptr::null_mut(), &raw mut token, 0, std::ptr::null_mut())
        };
        let error = io::Error::last_os_error();
        // SAFETY: the level was opened above, and is closed once, here.
        unsafe { SaferCloseLevel(level) };
        if computed == 0 {
            return Err(error);
        }
        let token = Owned::new(token)?;

        // Safer keeps this process's own (high) integrity level; a normal program runs at medium.
        let medium: Vec<u16> = "S-1-16-8192".encode_utf16().chain([0]).collect();
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `medium` is NUL-terminated; `sid` receives a SID allocated with LocalAlloc,
        // freed below.
        if unsafe { ConvertStringSidToSidW(medium.as_ptr(), &raw mut sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES { Sid: sid, Attributes: SE_GROUP_INTEGRITY.cast_unsigned() },
        };
        // SAFETY: `sid` is a valid SID until it is freed below.
        let sid_len = unsafe { GetLengthSid(sid) };
        let size =
            u32::try_from(std::mem::size_of::<TOKEN_MANDATORY_LABEL>()).map_err(io::Error::other)? + sid_len;
        // SAFETY: Safer opens the new token with full access; `label` is the structure this class
        // reads, followed in memory by nothing it needs (the SID is reached through its pointer).
        let set =
            unsafe { SetTokenInformation(token.0, TokenIntegrityLevel, (&raw const label).cast(), size) };
        let error = io::Error::last_os_error();
        // SAFETY: allocated by ConvertStringSidToSidW with LocalAlloc, and freed once, here.
        unsafe { LocalFree(sid) };
        if set == 0 {
            return Err(error);
        }
        Ok(token)
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
    fn a_stat_line_is_read_past_a_name_holding_spaces_and_parentheses() {
        let stat = "4242 (my (odd) cmd) S 17 4242 4242 34816 4242 4194304 1 0 0 0 0 0 0 0 20 0 1 0 987654 0";
        assert_eq!(linux_stat(stat), Some((17, 987_654)));
        assert_eq!(linux_stat("garbage"), None);
    }

    /// A shell and its children, ended however the test ends.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct Reaped(std::process::Child);

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = Command::new("pkill").args(["-P", &self.0.id().to_string()]).status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_shells_newest_child_is_its_running_command() {
        let shell =
            Reaped(Command::new("/bin/sh").args(["-c", "sleep 30 & sleep 31; wait"]).spawn().unwrap());
        let pid = shell.0.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = None;
        while seen.as_deref() != Some("sleep 31") {
            assert!(std::time::Instant::now() < deadline, "never saw sleep 31: {seen:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
            seen = ProcessTable::snapshot().running_command(pid);
        }
        assert_eq!(command_line(pid).unwrap()[0], "/bin/sh");
        drop(shell);
        assert_eq!(ProcessTable::snapshot().running_command(pid), None, "a gone shell runs nothing");
    }

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
    fn a_windows_shells_directory_and_running_command_are_read_from_outside_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = Command::new("cmd.exe")
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >NUL"])
            .current_dir(dir.path())
            .spawn()
            .unwrap();
        let pid = shell.id();
        let tree = ProcessTree::adopt(pid).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let command = loop {
            if let Some(line) = ProcessTable::snapshot().running_command(pid) {
                break line;
            }
            assert!(std::time::Instant::now() < deadline, "ping never showed as cmd's command");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(command.to_ascii_lowercase().contains("ping") && command.contains("-n 30"), "{command}");
        let here = working_directory(pid).expect("cmd's working directory");
        assert_eq!(std::fs::canonicalize(here).unwrap(), std::fs::canonicalize(dir.path()).unwrap());
        tree.terminate();
        shell.wait().unwrap();
        assert_eq!(ProcessTable::snapshot().running_command(pid), None, "a gone shell runs nothing");
    }

    #[test]
    fn windows_arguments_are_quoted_as_the_c_runtime_splits_them() {
        assert_eq!(quote_windows_arg("pty-host"), "pty-host");
        assert_eq!(quote_windows_arg(""), r#""""#);
        assert_eq!(
            quote_windows_arg(r"C:\Program Files\throng\throng.exe"),
            r#""C:\Program Files\throng\throng.exe""#
        );
        assert_eq!(quote_windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_windows_arg(r"C:\a dir\"), r#""C:\a dir\\""#);
        assert_eq!(quote_windows_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_windows_arg(r"a\\b"), r"a\\b", "backslashes not before a quote are literal");
    }

    #[cfg(windows)]
    #[test]
    fn a_deelevated_process_runs_at_medium_integrity() {
        use std::io::Read;
        if !can_deelevate() {
            eprintln!("skipped: this process is not elevated, so there is nothing to drop");
            return;
        }
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        // By its full path: a `whoami` earlier on PATH (Git's, on CI) is another program.
        let whoami = Path::new(&root).join(r"System32\whoami.exe");
        let piped = spawn_deelevated(&whoami, &["/groups", "/fo", "list"]).unwrap();
        drop(piped.stdin);
        let mut out = String::new();
        let mut stdout = piped.stdout;
        stdout.read_to_string(&mut out).unwrap();
        assert!(out.contains("S-1-16-8192"), "medium integrity:\n{out}");
        assert!(!out.contains("S-1-16-12288"), "not high integrity:\n{out}");
    }

    #[test]
    fn only_windows_starts_processes_without_its_rights() {
        if cfg!(not(windows)) {
            assert!(!can_deelevate());
            assert!(spawn_deelevated(Path::new("/bin/true"), &[]).is_err());
        }
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
