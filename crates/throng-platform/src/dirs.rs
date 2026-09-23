//! Where throng keeps its files.
//!
//! Every instance gets its own directories and its own daemon endpoint: a debug build never opens
//! the installed build's database, and `THRONG_HOME` gives tests and side-by-side runs a
//! private tree of their own.

use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that relocates every throng directory under one root.
pub const HOME_ENV: &str = "THRONG_HOME";

/// throng's directories for one instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDirs {
    /// `throng` for a release build, `throng-dev` for a debug build, `custom` under `THRONG_HOME`.
    pub instance: String,
    /// `settings.json` and friends.
    pub config: PathBuf,
    /// The database and crash-recovery snapshots.
    pub data: PathBuf,
    pub logs: PathBuf,
    /// The daemon socket and lock files. Private to the user.
    pub runtime: PathBuf,
    /// The single root everything lives under, when there is one (`THRONG_HOME`, tests). A daemon
    /// this instance spawns is handed it explicitly, so it can never resolve a different home.
    pub home: Option<PathBuf>,
}

/// The user's home folder: where a sub-workspace's own terminals start.
#[must_use]
pub fn home() -> Option<PathBuf> {
    directories::UserDirs::new().map(|d| d.home_dir().to_path_buf())
}

impl AppDirs {
    /// Resolve from the environment: `THRONG_HOME` if set, else the OS conventions.
    pub fn resolve() -> io::Result<Self> {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::under(Path::new(&home)));
        }
        let instance = if cfg!(debug_assertions) { "throng-dev" } else { "throng" };
        let project = directories::ProjectDirs::from("", "", instance)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory for this user"))?;
        let data = project.data_dir().to_path_buf();
        let runtime = project.runtime_dir().map_or_else(|| fallback_runtime(instance), Path::to_path_buf);
        Ok(Self {
            instance: instance.to_owned(),
            config: project.config_dir().to_path_buf(),
            logs: data.join("logs"),
            data,
            runtime,
            home: None,
        })
    }

    /// Every directory under one root (tests, portable runs).
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            instance: "custom".to_owned(),
            config: root.join("config"),
            data: root.join("data"),
            logs: root.join("logs"),
            runtime: root.join("run"),
            home: Some(root.to_path_buf()),
        }
    }

    /// Create every directory; the runtime directory is made private to the user.
    pub fn ensure(&self) -> io::Result<()> {
        for dir in [&self.config, &self.data, &self.logs, &self.runtime] {
            std::fs::create_dir_all(dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.runtime, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn database(&self) -> PathBuf {
        self.data.join("throng.db")
    }

    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    #[must_use]
    pub fn keybindings_file(&self) -> PathBuf {
        self.config.join("keybindings.json")
    }

    #[must_use]
    pub fn recovery(&self) -> PathBuf {
        self.data.join("recovery")
    }

    /// The daemon's local socket (Unix) — on Windows the endpoint is a named pipe derived from
    /// [`Self::pipe_name`].
    #[must_use]
    pub fn daemon_socket(&self) -> PathBuf {
        self.runtime.join("daemon.sock")
    }

    /// A per-user, per-instance pipe name (Windows).
    #[must_use]
    pub fn pipe_name(&self) -> String {
        let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
        let mut hash: u32 = 2_166_136_261;
        for byte in self.runtime.to_string_lossy().bytes() {
            hash = (hash ^ u32::from(byte)).wrapping_mul(16_777_619);
        }
        format!("throng-{}-{}-{hash:08x}", self.instance, sanitize(&user))
    }

    /// The lock held for the lifetime of the daemon.
    #[must_use]
    pub fn daemon_lock(&self) -> PathBuf {
        self.runtime.join("daemon.lock")
    }

    /// The lock held for the lifetime of the UI (one UI per instance).
    #[must_use]
    pub fn ui_lock(&self) -> PathBuf {
        self.runtime.join("ui.lock")
    }
}

fn sanitize(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn fallback_runtime(instance: &str) -> PathBuf {
    // macOS: $TMPDIR is already per-user. Elsewhere, suffix the uid so users never share a folder.
    #[cfg(unix)]
    {
        #[allow(unsafe_code)]
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("{instance}-{uid}"))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join(instance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_override_puts_everything_under_one_root() {
        let dirs = AppDirs::under(Path::new("/tmp/t"));
        assert_eq!(dirs.database(), PathBuf::from("/tmp/t/data/throng.db"));
        assert_eq!(dirs.daemon_socket(), PathBuf::from("/tmp/t/run/daemon.sock"));
        assert_eq!(dirs.settings_file(), PathBuf::from("/tmp/t/config/settings.json"));
    }

    #[test]
    fn pipe_names_differ_per_root() {
        let a = AppDirs::under(Path::new("/tmp/a"));
        let b = AppDirs::under(Path::new("/tmp/b"));
        assert_ne!(a.pipe_name(), b.pipe_name());
        assert!(a.pipe_name().chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn ensure_creates_a_private_runtime_dir() {
        let root = tempfile::tempdir().unwrap();
        let dirs = AppDirs::under(root.path());
        dirs.ensure().unwrap();
        assert!(dirs.runtime.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dirs.runtime).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }
}
