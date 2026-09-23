//! Unix process-group and session signalling.

/// Signal a whole process group. Errors (the group is already gone) are ignored.
pub fn kill_group(group: libc::pid_t, signal: libc::c_int) {
    if group <= 1 {
        return;
    }
    #[allow(unsafe_code)]
    // SAFETY: killpg only sends a signal; a stale group id fails with ESRCH, which is ignored.
    unsafe {
        libc::killpg(group, signal);
    }
}

/// Signal every process in the session `leader` leads, including background jobs in their own
/// process groups. Linux reads session ids from `/proc`; elsewhere the process groups signalled by
/// the caller are what can be reached.
pub fn kill_session(leader: u32, signal: libc::c_int) {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else { return };
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<libc::pid_t>().ok()) else {
                continue;
            };
            if session_of(pid) == Some(leader) && pid > 1 {
                #[allow(unsafe_code)]
                // SAFETY: kill only sends a signal; a pid that exited meanwhile fails with ESRCH.
                unsafe {
                    libc::kill(pid, signal);
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (leader, signal);
    }
}

/// The session id of a process, from `/proc/<pid>/stat` (field 6).
#[cfg(target_os = "linux")]
fn session_of(pid: libc::pid_t) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name (field 2) is parenthesised and may contain spaces; parse after the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(3)?.parse().ok()
}

/// Whether `pid` has ended (a zombie counts as ended: it runs nothing and holds no terminal).
#[must_use]
pub fn process_gone(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat.rfind(')').and_then(|i| stat[i + 1..].split_whitespace().next()) == Some("Z"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        !throng_platform::process::pid_alive(pid)
    }
}
