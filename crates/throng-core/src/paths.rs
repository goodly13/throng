//! Path identity and naming rules, as values rather than `cfg!`s.
//!
//! Lower-casing every path before comparing it suits Windows, which is case-insensitive.
//! Applied everywhere, that rule fuses two genuinely different directories on Linux (`~/Proj` and
//! `~/proj`); dropped, it lets two spellings of one directory on macOS pass the root-exclusivity
//! check. Neither answer is right everywhere, so the platform layer chooses a [`PathRules`] and the
//! domain only ever asks it.

use std::path::{Component, Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// How the host filesystem decides whether two spellings name the same entry, and which names it
/// accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathRules {
    /// `Foo` and `foo` name the same entry (Windows, default macOS volumes).
    pub case_insensitive: bool,
    /// Composed and decomposed Unicode spellings name the same entry (macOS).
    pub unicode_insensitive: bool,
    /// Backslash is a separator, and the Windows reserved names and characters apply.
    pub windows_names: bool,
}

impl PathRules {
    /// Linux: byte-exact names; only `/` and NUL are forbidden in a name.
    pub const LINUX: Self =
        Self { case_insensitive: false, unicode_insensitive: false, windows_names: false };
    /// macOS default (APFS, case-insensitive, normalisation-insensitive).
    pub const MACOS: Self = Self { case_insensitive: true, unicode_insensitive: true, windows_names: false };
    /// Windows (NTFS).
    pub const WINDOWS: Self =
        Self { case_insensitive: true, unicode_insensitive: false, windows_names: true };

    /// A comparison key for `path`: two paths name the same entry iff their keys are equal.
    ///
    /// Lexical only — it never touches the disk, so symlinks must be resolved by the caller first
    /// when that matters. Separators are unified, `.` segments dropped, `..` applied, repeated and
    /// trailing separators removed.
    #[must_use]
    pub fn key(&self, path: &Path) -> String {
        let raw = path.to_string_lossy();
        let unified: String = if self.windows_names { raw.replace('\\', "/") } else { raw.into_owned() };
        let absolute = unified.starts_with('/');
        let mut segments: Vec<&str> = Vec::new();
        for segment in unified.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                other => segments.push(other),
            }
        }
        let mut key = segments.join("/");
        if absolute {
            key.insert(0, '/');
        }
        if self.unicode_insensitive {
            key = key.nfc().collect();
        }
        if self.case_insensitive {
            key = key.to_lowercase();
        }
        key
    }

    /// Whether `a` and `b` name the same entry.
    #[must_use]
    pub fn same(&self, a: &Path, b: &Path) -> bool {
        self.key(a) == self.key(b)
    }

    /// Whether `path` is `ancestor` itself or somewhere beneath it.
    #[must_use]
    pub fn is_within(&self, ancestor: &Path, path: &Path) -> bool {
        let a = self.key(ancestor);
        let p = self.key(path);
        if a.is_empty() || p.is_empty() {
            return false;
        }
        if a == p {
            return true;
        }
        let prefix = if a.ends_with('/') { a } else { format!("{a}/") };
        p.starts_with(&prefix)
    }

    /// Whether two folders overlap exclusively: identical, or one inside the other.
    #[must_use]
    pub fn overlaps(&self, a: &Path, b: &Path) -> bool {
        self.is_within(a, b) || self.is_within(b, a)
    }

    /// Why `name` is not an acceptable single file-name segment here, or `None` if it is.
    ///
    /// Windows forbids `\ / : * ? " < > |`; on Linux and macOS only `/` and NUL are forbidden, and
    /// over-rejecting takes names away from users.
    #[must_use]
    pub fn invalid_name_reason(&self, name: &str) -> Option<&'static str> {
        if name.is_empty() {
            return Some("A name cannot be empty.");
        }
        if name == "." || name == ".." {
            return Some("\".\" and \"..\" are reserved names.");
        }
        if name.contains('/') || name.contains('\0') {
            return Some("A name cannot contain \"/\".");
        }
        if self.windows_names {
            if name
                .chars()
                .any(|c| matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32)
            {
                return Some("A name cannot contain any of \\ : * ? \" < > |.");
            }
            if name.ends_with('.') || name.ends_with(' ') {
                return Some("A name cannot end with a dot or a space.");
            }
            let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
            const RESERVED: [&str; 22] = [
                "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
                "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
            ];
            if RESERVED.contains(&stem.as_str()) {
                return Some("That name is reserved by Windows.");
            }
        }
        None
    }

    /// Whether moving or copying `source` into the folder `destination_dir` is allowed: never into
    /// itself or its own subtree. Move and copy share this one rule, so a copy can never recurse
    /// into itself.
    #[must_use]
    pub fn transfer_allowed(&self, source: &Path, destination_dir: &Path) -> bool {
        !self.is_within(source, destination_dir)
    }
}

/// `path` relative to `root`, with `/` separators, or `None` when it is not inside `root`.
#[must_use]
pub fn relative_to(rules: &PathRules, root: &Path, path: &Path) -> Option<String> {
    if !rules.is_within(root, path) {
        return None;
    }
    let root_depth = normal_components(root).len();
    let rel: Vec<String> = normal_components(path).into_iter().skip(root_depth).collect();
    Some(rel.join("/"))
}

fn normal_components(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                out.pop();
            }
            _ => {}
        }
    }
    out
}

/// A name for a copy of `name` that does not collide with anything `exists` reports:
/// `a.txt` → `a copy.txt` → `a copy 2.txt`.
#[must_use]
pub fn copy_name(name: &str, mut exists: impl FnMut(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i..]),
    };
    let mut n = 1u32;
    loop {
        let candidate = if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") };
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Join a root-relative, `/`-separated path onto `root`.
#[must_use]
pub fn join_relative(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for segment in rel.split('/').filter(|s| !s.is_empty()) {
        out.push(segment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    #[test]
    fn linux_is_case_sensitive() {
        let r = PathRules::LINUX;
        assert!(!r.same(p("/home/a/Proj"), p("/home/a/proj")));
        assert!(!r.overlaps(p("/home/a/Proj"), p("/home/a/proj")));
        assert!(r.same(p("/home/a/proj/"), p("/home/a//proj")));
    }

    #[test]
    fn macos_folds_case_and_normalisation() {
        let r = PathRules::MACOS;
        assert!(r.same(p("/Users/a/Proj"), p("/Users/a/proj")));
        // "é" composed vs decomposed
        assert!(r.same(p("/Users/a/caf\u{e9}"), p("/Users/a/cafe\u{301}")));
    }

    #[test]
    fn windows_unifies_separators_and_case() {
        let r = PathRules::WINDOWS;
        assert!(r.same(p("C:\\Work\\Proj"), p("c:/work/proj/")));
        assert!(r.is_within(p("C:\\Work"), p("c:/work/proj/src")));
    }

    #[test]
    fn overlap_is_ancestor_descendant_or_same_but_not_prefix() {
        let r = PathRules::LINUX;
        assert!(r.overlaps(p("/a/b"), p("/a/b")));
        assert!(r.overlaps(p("/a/b"), p("/a/b/c")));
        assert!(r.overlaps(p("/a/b/c"), p("/a/b")));
        assert!(!r.overlaps(p("/a/b"), p("/a/bc")));
        assert!(r.overlaps(p("/"), p("/anything")));
    }

    #[test]
    fn dot_segments_are_applied() {
        let r = PathRules::LINUX;
        assert!(r.same(p("/a/./b/../c"), p("/a/c")));
    }

    #[test]
    fn names_follow_the_platform() {
        assert_eq!(PathRules::LINUX.invalid_name_reason("a:b?c"), None);
        assert!(PathRules::WINDOWS.invalid_name_reason("a:b").is_some());
        assert!(PathRules::WINDOWS.invalid_name_reason("con.txt").is_some());
        assert!(PathRules::LINUX.invalid_name_reason("a/b").is_some());
        assert!(PathRules::LINUX.invalid_name_reason("..").is_some());
        assert!(PathRules::LINUX.invalid_name_reason("").is_some());
    }

    #[test]
    fn transfer_into_own_subtree_is_refused() {
        let r = PathRules::LINUX;
        assert!(!r.transfer_allowed(p("/p/src"), p("/p/src/sub")));
        assert!(!r.transfer_allowed(p("/p/src"), p("/p/src")));
        assert!(r.transfer_allowed(p("/p/src"), p("/p/other")));
        assert!(r.transfer_allowed(p("/p/src"), p("/p/src2")));
    }

    #[test]
    fn relative_paths() {
        let r = PathRules::LINUX;
        assert_eq!(relative_to(&r, p("/p"), p("/p/a/b.txt")).as_deref(), Some("a/b.txt"));
        assert_eq!(relative_to(&r, p("/p"), p("/p")).as_deref(), Some(""));
        assert_eq!(relative_to(&r, p("/p"), p("/q/a")), None);
        assert_eq!(join_relative(p("/p"), "a/b.txt"), PathBuf::from("/p/a/b.txt"));
    }

    #[test]
    fn copy_names_do_not_collide() {
        let taken = ["a.txt", "a copy.txt"];
        assert_eq!(copy_name("a.txt", |n| taken.contains(&n)), "a copy 2.txt");
        assert_eq!(copy_name("b.txt", |n| taken.contains(&n)), "b.txt");
        assert_eq!(copy_name(".env", |n| n == ".env"), ".env copy");
    }
}
