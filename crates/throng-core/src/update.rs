//! Updates: is a newer throng published than the one running, and which of its files would
//! replace this install (FR-052)?
//!
//! throng asks GitHub for the latest release now and then and, when that release is newer, says so
//! once with a link to download it. Where an install can replace itself (a Developer ID signed
//! `throng.app`, or an AppImage) it also offers to install it. The rules for choosing and checking
//! the files are here; the operating system's part is `throng-platform`'s.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// The GitHub API's view of the latest published release (drafts and pre-releases excluded).
pub const LATEST_RELEASE_API: &str = "https://api.github.com/repos/goodly13/throng/releases/latest";

/// The page a user downloads the latest release from.
pub const LATEST_RELEASE_PAGE: &str = "https://github.com/goodly13/throng/releases/latest";

/// How often a running throng asks again.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// A release version: `major.minor.patch`, optionally written with a leading `v`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse `1.2.3` or `v1.2.3`. Pre-release and build suffixes (`1.2.3-rc.1`) are not releases a
    /// user is pointed at, so they parse to `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self { major: next()?, minor: next()?, patch: next()? };
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A published release: its tag and the files attached to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub assets: Vec<Asset>,
}

/// A file attached to a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    /// Where it is downloaded from.
    pub url: String,
}

impl Release {
    /// A release with a tag and no files.
    #[must_use]
    pub fn tagged(tag: impl Into<String>) -> Self {
        Self { tag: tag.into(), assets: Vec::new() }
    }

    /// The file named exactly `name`. A release is never searched for something that merely looks
    /// like the file wanted.
    #[must_use]
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// A GitHub release, from the API's JSON: its `tag_name`, and each asset's `name` and
/// `browser_download_url`. Assets missing either are left out.
#[must_use]
pub fn parse_release(json: &str) -> Option<Release> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = value.get("tag_name")?.as_str()?.to_owned();
    let assets = value
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let name = a.get("name")?.as_str()?.to_owned();
                    let url = a.get("browser_download_url")?.as_str()?.to_owned();
                    Some(Asset { name, url })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Release { tag, assets })
}

/// The version `latest_tag` names, when it is newer than `running`.
#[must_use]
pub fn newer_release(latest_tag: &str, running: &str) -> Option<Version> {
    let latest = Version::parse(latest_tag)?;
    let running = Version::parse(running)?;
    (latest > running).then_some(latest)
}

/// The macOS package the updater installs: the signed, notarised `throng.app`, zipped with `ditto`
/// (FR-045).
#[must_use]
pub fn macos_zip_name(version: Version) -> String {
    format!("throng-{version}-macos-universal.zip")
}

/// The AppImage for a machine: `x86_64` or `aarch64`, as `uname -m` says and as Rust's
/// `std::env::consts::ARCH` says on Linux.
#[must_use]
pub fn appimage_name(version: Version, machine: &str) -> String {
    format!("throng-{version}-{machine}.AppImage")
}

/// The checksum list published with every release (FR-048).
pub const SHA256SUMS: &str = "SHA256SUMS";

/// The SHA-256 that `sums` (the output of `sha256sum`) gives for `name`, as lowercase hex. Both of
/// `sha256sum`'s forms are read: `<hash>  <name>`, and `<hash> *<name>` for binary mode.
#[must_use]
pub fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, rest) = line.trim_end_matches('\r').split_once(' ')?;
        let file = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*'))?;
        let valid = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        (valid && file == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Check a download against its release's `SHA256SUMS`. The list comes from the same place as the
/// file, so this catches a corrupt or truncated download, not a forged one: the HTTPS download
/// from GitHub is what the file is trusted on.
pub fn verify_checksum(bytes: &[u8], sums: &str, name: &str) -> Result<(), String> {
    let Some(expected) = checksum_for(sums, name) else {
        return Err(format!("{SHA256SUMS} lists no checksum for {name}."));
    };
    let actual = sha256_hex(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{name} does not match its checksum (expected {expected}, got {actual})."))
    }
}

/// The signing team in `codesign -dv` output (its `TeamIdentifier=` line). An ad-hoc signed or
/// unsigned app has none: `TeamIdentifier=not set`, or no such line at all.
#[must_use]
pub fn team_identifier(codesign_output: &str) -> Option<String> {
    codesign_output.lines().find_map(|line| {
        let team = line.trim().strip_prefix("TeamIdentifier=")?.trim();
        (!team.is_empty() && team != "not set").then(|| team.to_owned())
    })
}

/// The `throng.app` bundle that `exe` runs from, when `exe` is the bundle's own executable
/// (`…/throng.app/Contents/MacOS/throng`). A build under a Cargo `target` directory, or an app
/// macOS has translocated to a private read-only mount, is not one the updater may replace.
#[must_use]
pub fn app_bundle_of(exe: &Path) -> Option<PathBuf> {
    let named = |path: &Path, name: &str| path.file_name().is_some_and(|n| n == name);
    if !named(exe, "throng") {
        return None;
    }
    let macos = exe.parent().filter(|p| named(p, "MacOS"))?;
    let contents = macos.parent().filter(|p| named(p, "Contents"))?;
    let bundle = contents.parent().filter(|p| named(p, "throng.app"))?;
    let refused = bundle
        .components()
        .any(|c| matches!(c, Component::Normal(n) if n == "target" || n == "AppTranslocation"));
    (!refused && bundle.is_absolute()).then(|| bundle.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_parse_with_or_without_a_v() {
        assert_eq!(Version::parse("v0.1.2"), Some(Version { major: 0, minor: 1, patch: 2 }));
        assert_eq!(Version::parse(" 1.10.0 "), Some(Version { major: 1, minor: 10, patch: 0 }));
    }

    #[test]
    fn anything_but_a_plain_release_version_is_refused() {
        for text in ["", "v", "1.2", "1.2.3.4", "1.2.3-rc.1", "1.2.x", "v1..3", "latest", "+1.2.3"] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
    }

    #[test]
    fn only_a_strictly_newer_release_is_reported() {
        assert_eq!(newer_release("v0.1.3", "0.1.2"), Version::parse("0.1.3"));
        assert_eq!(newer_release("v0.1.2", "0.1.2"), None, "the same version");
        assert_eq!(newer_release("v0.1.1", "0.1.2"), None, "an older release");
        assert_eq!(newer_release("nightly", "0.1.2"), None, "a tag that names no version");
    }

    #[test]
    fn the_tag_is_read_from_the_release_json() {
        let json = r#"{"url":"x","tag_name":"v0.1.2","name":"throng v0.1.2","draft":false}"#;
        assert_eq!(parse_release(json).map(|r| r.tag).as_deref(), Some("v0.1.2"));
        assert_eq!(parse_release(r#"{"message":"Not Found"}"#), None);
        assert_eq!(parse_release("<html>rate limited</html>"), None);
    }

    #[test]
    fn a_release_brings_its_tag_and_the_files_attached_to_it() {
        let json = r#"{"tag_name":"v0.2.0","assets":[
            {"name":"throng-0.2.0-macos-universal.zip","browser_download_url":"https://e/zip"},
            {"name":"SHA256SUMS","browser_download_url":"https://e/sums"},
            {"name":"no-url"}
        ]}"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.assets.len(), 2, "an asset without a download address is left out");
        let zip = release.asset(&macos_zip_name(v("0.2.0")));
        assert_eq!(zip.map(|a| a.url.as_str()), Some("https://e/zip"));
        assert_eq!(release.asset("throng-0.2.0-macos-universal.dmg"), None);
        assert_eq!(parse_release(r#"{"tag_name":"v1.0.0"}"#), Some(Release::tagged("v1.0.0")));
    }

    #[test]
    fn package_names_follow_the_packaging_script() {
        assert_eq!(macos_zip_name(v("1.2.3")), "throng-1.2.3-macos-universal.zip");
        assert_eq!(appimage_name(v("1.2.3"), "x86_64"), "throng-1.2.3-x86_64.AppImage");
    }

    /// SHA-256 of "test".
    const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn checksums_are_read_in_both_of_sha256sums_forms() {
        let upper = HASH.to_uppercase();
        let sums = format!("{HASH}  throng-1.0.0-x86_64.AppImage\n{upper} *other.deb\r\n");
        assert_eq!(checksum_for(&sums, "throng-1.0.0-x86_64.AppImage").as_deref(), Some(HASH));
        assert_eq!(checksum_for(&sums, "other.deb").as_deref(), Some(HASH), "binary mode, CRLF");
        assert_eq!(checksum_for(&sums, "throng-1.0.0"), None, "names match exactly");
        assert_eq!(checksum_for("nothex  a\n", "a"), None);
        assert_eq!(checksum_for(&format!("{HASH} a"), "a"), None, "one space and no star");
    }

    #[test]
    fn a_download_is_checked_against_its_listed_checksum() {
        let sums = format!("{HASH}  a.AppImage\n");
        assert_eq!(sha256_hex(b"test"), HASH);
        assert_eq!(verify_checksum(b"test", &sums, "a.AppImage"), Ok(()));
        let wrong = verify_checksum(b"tesT", &sums, "a.AppImage").unwrap_err();
        assert!(wrong.contains("does not match"), "{wrong}");
        let missing = verify_checksum(b"test", &sums, "b.AppImage").unwrap_err();
        assert!(missing.contains("no checksum for b.AppImage"), "{missing}");
    }

    #[test]
    fn the_signing_team_is_read_from_codesign_output() {
        let signed = "Executable=/Applications/throng.app/Contents/MacOS/throng\n\
                      Identifier=com.throng.throng\nTeamIdentifier=ABCDE12345\nSealed Resources version=2\n";
        assert_eq!(team_identifier(signed).as_deref(), Some("ABCDE12345"));
        assert_eq!(team_identifier("Signature=adhoc\nTeamIdentifier=not set\n"), None);
        assert_eq!(team_identifier("code object is not signed at all\n"), None);
    }

    #[test]
    fn only_an_installed_bundles_own_executable_names_a_bundle() {
        let bundle = |p: &str| app_bundle_of(Path::new(p));
        assert_eq!(
            bundle("/Applications/throng.app/Contents/MacOS/throng"),
            Some(PathBuf::from("/Applications/throng.app"))
        );
        assert_eq!(
            bundle("/Users/me/Apps/throng.app/Contents/MacOS/throng"),
            Some(PathBuf::from("/Users/me/Apps/throng.app"))
        );
        for refused in [
            "/Users/me/throng/target/release/throng",
            "/Users/me/throng/target/dist/throng.app/Contents/MacOS/throng",
            "/private/var/folders/x/T/AppTranslocation/ABC/d/throng.app/Contents/MacOS/throng",
            "/Applications/throng.app/Contents/Resources/throng",
            "/Applications/Other.app/Contents/MacOS/throng",
            "/Applications/throng.app/Contents/MacOS/other",
            "throng.app/Contents/MacOS/throng",
        ] {
            assert_eq!(bundle(refused), None, "{refused}");
        }
    }
}
