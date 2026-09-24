//! Update check: is a newer throng published than the one running?
//!
//! throng does not install updates. It asks GitHub for the latest release now and then and, when
//! that release is newer, says so once with a link to download it (FR-052).

use std::time::Duration;

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

/// The `tag_name` of a GitHub release, from the API's JSON.
#[must_use]
pub fn release_tag(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.get("tag_name")?.as_str().map(str::to_owned)
}

/// The version `latest_tag` names, when it is newer than `running`.
#[must_use]
pub fn newer_release(latest_tag: &str, running: &str) -> Option<Version> {
    let latest = Version::parse(latest_tag)?;
    let running = Version::parse(running)?;
    (latest > running).then_some(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(release_tag(json).as_deref(), Some("v0.1.2"));
        assert_eq!(release_tag(r#"{"message":"Not Found"}"#), None);
        assert_eq!(release_tag("<html>rate limited</html>"), None);
    }
}
