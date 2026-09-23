//! Installed-shell detection (Principle IV): throng hosts the user's real shells.

use std::path::{Path, PathBuf};

use throng_core::terminal::ShellInfo;

/// Shells found on this machine, the user's login shell first.
#[must_use]
pub fn detect() -> Vec<ShellInfo> {
    let mut found: Vec<ShellInfo> = Vec::new();
    let mut push = |program: PathBuf| {
        if !is_executable(&program) {
            return;
        }
        let Some(info) = describe(&program) else { return };
        let canonical = std::fs::canonicalize(&program).unwrap_or_else(|_| program.clone());
        let duplicate = found.iter().any(|s| {
            s.id == info.id
                || std::fs::canonicalize(&s.program).unwrap_or_else(|_| s.program.clone()) == canonical
        });
        if !duplicate {
            found.push(info);
        }
    };
    for candidate in candidates() {
        push(candidate);
    }
    // Each WSL distribution is a shell of its own, beside WSL's default one.
    if let Some(wsl) = found.iter().find(|s| s.id == "wsl").map(|s| s.program.clone()) {
        for name in wsl_distributions(&list_wsl(&wsl)) {
            found.push(ShellInfo {
                id: format!("wsl:{name}"),
                label: format!("WSL: {name}"),
                program: wsl.clone(),
                args: vec!["-d".to_owned(), name],
            });
        }
    }
    found
}

/// What `wsl.exe -l -q` prints: the installed distributions' names. Nothing where it cannot run.
fn list_wsl(wsl: &Path) -> Vec<u8> {
    let mut command = std::process::Command::new(wsl);
    command.args(["-l", "-q"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.output().map(|o| o.stdout).unwrap_or_default()
}

/// The distribution names in `wsl.exe -l -q`'s output (UTF-16, as wsl.exe writes it, or UTF-8),
/// leaving out Docker Desktop's own, which are not shells.
#[must_use]
pub fn wsl_distributions(output: &[u8]) -> Vec<String> {
    let (pairs, rest) = output.as_chunks::<2>();
    let utf16 = rest.is_empty() && pairs.iter().any(|pair| pair[1] == 0);
    let text = if utf16 {
        let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(output).into_owned()
    };
    text.lines()
        .map(|line| line.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0'))
        .filter(|name| !name.is_empty() && !name.starts_with("docker-desktop"))
        .map(str::to_owned)
        .collect()
}

/// The shell a new terminal uses: the configured id when it is installed, else the first detected.
#[must_use]
pub fn choose<'a>(shells: &'a [ShellInfo], preferred: Option<&str>) -> Option<&'a ShellInfo> {
    preferred.and_then(|id| shells.iter().find(|s| s.id == id)).or_else(|| shells.first())
}

#[cfg(unix)]
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(login) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        out.push(PathBuf::from(login));
    }
    if let Ok(text) = std::fs::read_to_string("/etc/shells") {
        out.extend(
            text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(PathBuf::from),
        );
    }
    for name in ["bash", "zsh", "fish", "pwsh", "nu", "sh"] {
        if let Some(path) = which(name) {
            out.push(path);
        }
    }
    out
}

#[cfg(windows)]
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = which("pwsh.exe") {
        out.push(path);
    }
    let system_root =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    out.push(system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    out.push(
        std::env::var_os("ComSpec").map_or_else(|| system_root.join(r"System32\cmd.exe"), PathBuf::from),
    );
    for base in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(base) {
            out.push(PathBuf::from(dir).join(r"Git\bin\bash.exe"));
        }
    }
    out.push(system_root.join(r"System32\wsl.exe"));
    out
}

/// Identify a shell from its executable name.
fn describe(program: &Path) -> Option<ShellInfo> {
    let stem = program.file_stem()?.to_string_lossy().to_lowercase();
    let login = cfg!(target_os = "macos");
    let (id, label, args): (&str, &str, Vec<&str>) = match stem.as_str() {
        "bash" if cfg!(windows) => ("git-bash", "Git Bash", vec!["--login", "-i"]),
        "bash" => ("bash", "Bash", if login { vec!["-l"] } else { vec![] }),
        "zsh" => ("zsh", "Zsh", if login { vec!["-l"] } else { vec![] }),
        "fish" => ("fish", "Fish", if login { vec!["-l"] } else { vec![] }),
        "sh" => ("sh", "sh", vec![]),
        "dash" => ("dash", "Dash", vec![]),
        "ksh" | "mksh" => ("ksh", "KornShell", vec![]),
        "tcsh" => ("tcsh", "tcsh", vec![]),
        "csh" => ("csh", "csh", vec![]),
        "nu" => ("nu", "Nushell", vec![]),
        "elvish" => ("elvish", "Elvish", vec![]),
        "xonsh" => ("xonsh", "Xonsh", vec![]),
        "pwsh" => ("pwsh", "PowerShell", vec!["-NoLogo"]),
        "powershell" => ("powershell", "Windows PowerShell", vec!["-NoLogo"]),
        "cmd" => ("cmd", "Command Prompt", vec![]),
        "wsl" => ("wsl", "WSL", vec![]),
        _ => return None,
    };
    Some(ShellInfo {
        id: id.to_owned(),
        label: label.to_owned(),
        program: program.to_path_buf(),
        args: args.into_iter().map(str::to_owned).collect(),
    })
}

/// Find an executable on `PATH`.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_shells_are_described() {
        let bash = describe(Path::new("/bin/bash")).unwrap();
        assert_eq!(bash.label, if cfg!(windows) { "Git Bash" } else { "Bash" });
        assert_eq!(describe(Path::new("/usr/bin/pwsh")).unwrap().args, vec!["-NoLogo".to_string()]);
        assert!(describe(Path::new("/usr/bin/python3")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn detection_finds_sh_and_never_duplicates() {
        let shells = detect();
        assert!(shells.iter().any(|s| s.id == "sh" || s.id == "bash" || s.id == "dash"), "{shells:?}");
        let mut ids: Vec<_> = shells.iter().map(|s| s.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), shells.len());
    }

    #[test]
    fn wsl_distributions_are_read_from_its_utf16_listing() {
        let listing: Vec<u8> = "Ubuntu-24.04\r\ndocker-desktop\r\nDebian\r\n\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(wsl_distributions(&listing), ["Ubuntu-24.04", "Debian"]);
        assert_eq!(wsl_distributions(b"Arch\n"), ["Arch"], "UTF-8 too");
        assert!(wsl_distributions(b"").is_empty());
    }

    #[test]
    fn choose_prefers_the_configured_shell() {
        let shells = vec![
            ShellInfo { id: "bash".into(), label: "Bash".into(), program: "/bin/bash".into(), args: vec![] },
            ShellInfo { id: "zsh".into(), label: "Zsh".into(), program: "/bin/zsh".into(), args: vec![] },
        ];
        assert_eq!(choose(&shells, Some("zsh")).unwrap().id, "zsh");
        assert_eq!(choose(&shells, Some("fish")).unwrap().id, "bash");
        assert_eq!(choose(&shells, None).unwrap().id, "bash");
        assert!(choose(&[], None).is_none());
    }
}
