//! Replacing the running install with a newer release's package (FR-054).
//!
//! Only two kinds of install replace themselves: a Developer ID signed `throng.app` on macOS, and
//! an AppImage on Linux. Everything else (the Windows `.msi` and `.zip`, which need a code-signing
//! certificate first; the `.deb`; the `.tar.gz`; a development build) is updated by downloading the
//! new package by hand. Which files to fetch, and the checks that are pure rules, are
//! `throng_core::update`'s; running `codesign`, `spctl` and `ditto` and moving files is here.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use throng_core::update::{self, Version};

/// An install that can replace itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A `throng.app` signed by `team`, in a folder this user can write.
    AppBundle { bundle: PathBuf, team: String },
    /// An AppImage, in a folder this user can write.
    AppImage { file: PathBuf },
}

impl Target {
    /// What the install that `exe` runs from is, when it can replace itself; otherwise why not.
    /// `appimage` is the `APPIMAGE` variable an AppImage's runtime sets.
    pub fn detect(exe: &Path, appimage: Option<&OsStr>) -> Result<Self, String> {
        if cfg!(target_os = "macos") {
            detect_bundle(exe)
        } else if cfg!(target_os = "linux") {
            let file = appimage.map(PathBuf::from).ok_or("throng is not running from an AppImage.")?;
            detect_appimage(&file)
        } else {
            Err("On this system throng is updated from its release page: installing in place needs a \
                 code-signing certificate first."
                .into())
        }
    }

    /// The release file this install is replaced from.
    #[must_use]
    pub fn package_name(&self, version: Version) -> String {
        match self {
            Self::AppBundle { .. } => update::macos_zip_name(version),
            Self::AppImage { .. } => update::appimage_name(version, std::env::consts::ARCH),
        }
    }

    /// Whether the release's `SHA256SUMS` must be fetched too.
    #[must_use]
    pub fn needs_checksums(&self) -> bool {
        matches!(self, Self::AppImage { .. })
    }

    /// Check `package` and put it in place of this install. Nothing is changed unless every check
    /// passes; a failure after the old install was moved puts it back. `verifying` is called once
    /// the checks start.
    pub fn install(
        &self,
        package: &[u8],
        sums: Option<&str>,
        version: Version,
        verifying: &dyn Fn(),
    ) -> Result<(), String> {
        match self {
            Self::AppBundle { bundle, team } => install_bundle(bundle, team, package, version, verifying),
            Self::AppImage { file } => {
                verifying();
                let name = self.package_name(version);
                update::verify_checksum(package, sums.unwrap_or_default(), &name)?;
                install_appimage(file, package)
            }
        }
    }

    /// Open the installed throng once this process (`pid`) has exited, so the new one does not find
    /// this one still holding the instance lock.
    pub fn relaunch(&self, pid: u32) -> Result<(), String> {
        let (script, target) = match self {
            Self::AppBundle { bundle, .. } => ("exec open -n \"$2\"", bundle),
            Self::AppImage { file } => ("exec \"$2\"", file),
        };
        let script = format!("while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; {script}");
        let args = [OsStr::new("-c"), OsStr::new(&script), OsStr::new("sh")];
        let pid = pid.to_string();
        let args = args.into_iter().chain([OsStr::new(&pid), target.as_os_str()]);
        crate::process::spawn_detached(Path::new("/bin/sh"), args, &[])
            .map(|_| ())
            .map_err(|e| format!("throng could not be reopened: {e}"))
    }
}

fn detect_bundle(exe: &Path) -> Result<Target, String> {
    let bundle = update::app_bundle_of(exe)
        .ok_or("throng is not running from an installed throng.app (a build, or a translocated app).")?;
    writable_folder(&bundle)?;
    let team = signing_team(&bundle)?.ok_or(
        "This throng.app is not signed with a Developer ID, so no update can be checked against it.",
    )?;
    Ok(Target::AppBundle { bundle, team })
}

fn detect_appimage(file: &Path) -> Result<Target, String> {
    let meta = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?;
    if !meta.is_file() || meta.permissions().readonly() {
        return Err(format!("{} cannot be replaced.", file.display()));
    }
    writable_folder(file)?;
    Ok(Target::AppImage { file: file.to_path_buf() })
}

/// The folder holding `item`, when this user can create files in it (a trial folder, removed at
/// once): the swap is two renames within it.
fn writable_folder(item: &Path) -> Result<&Path, String> {
    let folder = item.parent().ok_or_else(|| format!("{} has no folder.", item.display()))?;
    tempfile::Builder::new().prefix(".throng-update-").tempdir_in(folder).map(|_| folder).map_err(|e| {
        format!("{} cannot be written to ({e}), so throng cannot replace itself.", folder.display())
    })
}

/// Run a tool; its output (stdout then stderr) when it succeeds, and a sentence when it does not.
fn run(program: &str, args: &[&OsStr]) -> Result<String, String> {
    let output = Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() { Ok(text) } else { Err(format!("{program} refused it: {}", text.trim())) }
}

/// The team that signed a bundle, `None` when it is ad-hoc signed or unsigned.
fn signing_team(bundle: &Path) -> Result<Option<String>, String> {
    let text = run("codesign", &[OsStr::new("-dv"), OsStr::new("--verbose=2"), bundle.as_os_str()])?;
    Ok(update::team_identifier(&text))
}

/// A bundle's `CFBundleShortVersionString`.
fn bundle_version(bundle: &Path) -> Result<String, String> {
    let plist = bundle.join("Contents/Info.plist");
    let text = run(
        "plutil",
        &[
            OsStr::new("-extract"),
            OsStr::new("CFBundleShortVersionString"),
            OsStr::new("raw"),
            OsStr::new("-o"),
            OsStr::new("-"),
            plist.as_os_str(),
        ],
    )?;
    Ok(text.trim().to_owned())
}

/// Every check the downloaded app must pass before the installed one is touched.
fn verify_bundle(new: &Path, team: &str, version: Version) -> Result<(), String> {
    let path = new.as_os_str();
    run("codesign", &[OsStr::new("--verify"), OsStr::new("--deep"), OsStr::new("--strict"), path])
        .map_err(|e| format!("The downloaded throng.app's signature is not intact. {e}"))?;
    match signing_team(new)? {
        Some(signed) if signed == team => {}
        Some(signed) => {
            return Err(format!("The downloaded throng.app is signed by team {signed}, not {team}."));
        }
        None => return Err("The downloaded throng.app is not signed with a Developer ID.".into()),
    }
    run("spctl", &[OsStr::new("--assess"), OsStr::new("--type"), OsStr::new("execute"), path])
        .map_err(|e| format!("Gatekeeper does not accept the downloaded throng.app. {e}"))?;
    let found = bundle_version(new)?;
    if found != version.to_string() {
        return Err(format!("The downloaded throng.app is version {found}, not {version}."));
    }
    Ok(())
}

fn install_bundle(
    bundle: &Path,
    team: &str,
    zip: &[u8],
    version: Version,
    verifying: &dyn Fn(),
) -> Result<(), String> {
    let folder = writable_folder(bundle)?;
    // Unpacked beside the installed app, so moving it into place is a rename on one file system.
    let staging = tempfile::Builder::new()
        .prefix(".throng-update-")
        .tempdir_in(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    let archive = staging.path().join("throng.zip");
    std::fs::write(&archive, zip).map_err(|e| format!("{}: {e}", archive.display()))?;
    let unpacked = staging.path().join("unpacked");
    run("ditto", &[OsStr::new("-x"), OsStr::new("-k"), archive.as_os_str(), unpacked.as_os_str()])
        .map_err(|e| format!("The download could not be unpacked. {e}"))?;
    let new = unpacked.join("throng.app");
    if !new.is_dir() {
        return Err("The download holds no throng.app.".into());
    }
    verifying();
    verify_bundle(&new, team, version)?;
    swap(bundle, &new, staging.path())
}

/// Put `new` where `installed` is. The old one waits in `aside` until the new one is in place, and
/// goes back if it cannot be.
fn swap(installed: &Path, new: &Path, aside: &Path) -> Result<(), String> {
    let old = aside.join("previous");
    std::fs::rename(installed, &old)
        .map_err(|e| format!("{} could not be moved: {e}", installed.display()))?;
    if let Err(e) = std::fs::rename(new, installed) {
        let restored = std::fs::rename(&old, installed);
        return Err(match restored {
            Ok(()) => format!("The new throng could not be put in place: {e}"),
            Err(r) => format!(
                "The new throng could not be put in place ({e}), and the old one could not be put back \
                 ({r}): it is at {}.",
                old.display()
            ),
        });
    }
    // The staging folder, old app included, is removed when it is dropped.
    Ok(())
}

fn install_appimage(file: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let folder = writable_folder(file)?;
    let mut new = tempfile::Builder::new()
        .prefix(".throng-update-")
        .tempfile_in(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    new.write_all(bytes).and_then(|()| new.flush()).map_err(|e| format!("{}: {e}", new.path().display()))?;
    make_executable(new.path())?;
    // A rename: the running AppImage keeps its own (now unlinked) file until it exits.
    new.persist(file)
        .map(|_| ())
        .map_err(|e| format!("{} could not be replaced: {}", file.display(), e.error))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn an_appimage_is_replaced_only_by_a_download_that_matches_its_checksum() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("throng.AppImage");
        std::fs::write(&file, b"old").unwrap();
        let target = Target::AppImage { file: file.clone() };
        let name = target.package_name(v("9.0.0"));
        let sums = format!("{}  {name}\n", update::sha256_hex(b"new"));

        let error = target.install(b"corrupt", Some(&sums), v("9.0.0"), &|| {}).unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        assert_eq!(std::fs::read(&file).unwrap(), b"old", "a failed check changes nothing");
        let error = target.install(b"new", None, v("9.0.0"), &|| {}).unwrap_err();
        assert!(error.contains("no checksum"), "{error}");

        target.install(b"new", Some(&sums), v("9.0.0"), &|| {}).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o755);
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no staging file is left beside it");
    }

    #[test]
    fn a_swap_that_cannot_place_the_new_app_puts_the_old_one_back() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("throng.app");
        std::fs::create_dir(&installed).unwrap();
        std::fs::write(installed.join("marker"), b"old").unwrap();
        let aside = tempfile::tempdir_in(dir.path()).unwrap();
        let error = swap(&installed, &dir.path().join("missing.app"), aside.path()).unwrap_err();
        assert!(error.contains("could not be put in place"), "{error}");
        assert_eq!(std::fs::read(installed.join("marker")).unwrap(), b"old");
    }

    #[test]
    fn a_swap_puts_the_new_app_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("throng.app");
        let new = dir.path().join("new.app");
        std::fs::create_dir(&installed).unwrap();
        std::fs::create_dir(&new).unwrap();
        std::fs::write(new.join("marker"), b"new").unwrap();
        let aside = tempfile::tempdir_in(dir.path()).unwrap();
        swap(&installed, &new, aside.path()).unwrap();
        assert_eq!(std::fs::read(installed.join("marker")).unwrap(), b"new");
        drop(aside);
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("throng.app")], "the old app is removed");
    }

    /// The real tools, on a bundle built here: an ad-hoc signed app is refused before anything
    /// installed is touched, and so is a running app with no Developer ID.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_download_not_signed_by_the_running_apps_team_is_refused_and_nothing_moves() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build");
        let app = build.join("throng.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/throng")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>throng</string>\
             <key>CFBundleIdentifier</key><string>com.throng.test</string>\
             <key>CFBundleShortVersionString</key><string>9.0.0</string></dict></plist>\n",
        )
        .unwrap();
        run("codesign", &[OsStr::new("--force"), OsStr::new("--sign"), OsStr::new("-"), app.as_os_str()])
            .unwrap();
        let zip = dir.path().join("throng.zip");
        let keep = [OsStr::new("-c"), OsStr::new("-k"), OsStr::new("--keepParent")];
        let args: Vec<&OsStr> = keep.into_iter().chain([app.as_os_str(), zip.as_os_str()]).collect();
        run("ditto", &args).unwrap();
        assert_eq!(signing_team(&app).unwrap(), None, "ad-hoc signed");
        assert_eq!(bundle_version(&app).unwrap(), "9.0.0");

        let installed = dir.path().join("Applications/throng.app");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("marker"), b"old").unwrap();
        let target = Target::AppBundle { bundle: installed.clone(), team: "ABCDE12345".into() };
        let verified = std::cell::Cell::new(false);
        let error = target
            .install(&std::fs::read(&zip).unwrap(), None, v("9.0.0"), &|| verified.set(true))
            .unwrap_err();
        assert!(verified.get());
        assert!(error.contains("not signed with a Developer ID"), "{error}");
        assert_eq!(std::fs::read(installed.join("marker")).unwrap(), b"old");
        let left: Vec<_> = std::fs::read_dir(installed.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "the staging folder is removed");

        let exe = app.join("Contents/MacOS/throng");
        let refused = Target::detect(&exe, None).unwrap_err();
        assert!(refused.contains("not signed with a Developer ID"), "{refused}");
    }
}
