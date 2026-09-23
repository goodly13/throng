//! Links in text: web addresses, a few safe schemes, and references to files, as
//! editors and terminals find them — one grammar for both.
//!
//! Detection is syntactic: a well-formed path is a link whether or not it exists, and
//! what following a missing one does is the caller's business. Nothing here touches the disk or asks
//! which operating system it is on.

/// What a link points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An `http` or `https` address.
    Web(String),
    /// An address for the system's handler, on the allowlist (`mailto:`, `tel:`).
    Scheme(String),
    /// A file or folder, as written (a `file:` URI already decoded), with a position in it.
    File { path: String, line: Option<u32>, column: Option<u32> },
}

/// A link found in a line of text. `start..end` are character offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: usize,
    pub end: usize,
    pub target: Target,
}

/// Schemes followed through the system's handler. Everything else that is not `http`,
/// `https` or `file` — `javascript:`, `data:` and the rest — is never a link.
const ALLOWED_SCHEMES: &[&str] = &["mailto:", "tel:"];

/// Characters that end an unquoted token.
fn is_break(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | '\u{0}'..='\u{1f}')
}

/// Every link in `text` (one line), left to right, never overlapping.
#[must_use]
pub fn find(text: &str) -> Vec<Link> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Link> = Vec::new();
    quoted(&chars, &mut out);
    let mut i = 0;
    while i < chars.len() {
        if is_break(chars[i]) || out.iter().any(|l| l.start <= i && i < l.end) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && !is_break(chars[j]) {
            j += 1;
        }
        if let Some(link) = token(&chars, i, j) {
            out.push(link);
        }
        i = j;
    }
    out.sort_by_key(|l| l.start);
    out
}

/// What an explicit hyperlink's target is (a terminal's OSC 8, a Markdown link): a web address, an
/// allowed scheme, a `file:` URI — or `None` for anything unopenable.
#[must_use]
pub fn classify(uri: &str) -> Option<Target> {
    let uri = uri.trim();
    if uri.chars().any(char::is_control) {
        return None;
    }
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return (uri.len() > lower.find("://")? + 3).then(|| Target::Web(uri.to_owned()));
    }
    if lower.starts_with("file:") {
        let (uri, line, column) = split_position(uri)?;
        return Some(Target::File { path: file_uri_path(&uri)?, line, column });
    }
    if ALLOWED_SCHEMES.iter().any(|s| lower.starts_with(s)) {
        return Some(Target::Scheme(uri.to_owned()));
    }
    None
}

/// Paths in matching quotes or backticks, which may hold spaces.
fn quoted(chars: &[char], out: &mut Vec<Link>) {
    let mut i = 0;
    while i < chars.len() {
        let q = chars[i];
        if !matches!(q, '"' | '\'' | '`') {
            i += 1;
            continue;
        }
        let Some(close) = chars[i + 1..].iter().position(|c| *c == q).map(|p| p + i + 1) else {
            i += 1;
            continue;
        };
        let inner: String = chars[i + 1..close].iter().collect();
        if inner.contains(' ')
            && !inner.contains("://")
            && let Some((path, line, column)) = split_position(&inner)
            && looks_like_path(&path, line.is_some())
        {
            out.push(Link { start: i + 1, end: close, target: Target::File { path, line, column } });
        }
        i = close + 1;
    }
}

/// One unquoted token, `chars[start..end]`, trimmed to the link inside it, if any.
fn token(chars: &[char], mut start: usize, mut end: usize) -> Option<Link> {
    // Leading brackets and punctuation are not part of a link.
    while start < end && matches!(chars[start], '(' | '[' | '{' | ',' | ';') {
        start += 1;
    }
    let lower: String = chars[start..end].iter().collect::<String>().to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        end = trim_end(chars, start, end, true);
        let url: String = chars[start..end].iter().collect();
        return (url.len() > lower.find("://")? + 3).then_some(Link { start, end, target: Target::Web(url) });
    }
    if lower.starts_with("file:") {
        end = trim_end(chars, start, end, true);
        let raw: String = chars[start..end].iter().collect();
        let (uri, line, column) = split_position(&raw)?;
        let path = file_uri_path(&uri)?;
        return Some(Link { start, end, target: Target::File { path, line, column } });
    }
    if let Some(scheme) = ALLOWED_SCHEMES.iter().find(|s| lower.starts_with(**s)) {
        end = trim_end(chars, start, end, false);
        return (end - start > scheme.len()).then(|| Link {
            start,
            end,
            target: Target::Scheme(chars[start..end].iter().collect()),
        });
    }
    if let Some(colon) = lower.find(':')
        && lower[..colon].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && colon > 1
        && lower[colon + 1..].starts_with("//")
    {
        // Some other scheme://: never a link.
        return None;
    }
    if let Some(email) = email(chars, start, end) {
        return Some(email);
    }
    end = trim_end(chars, start, end, false);
    let raw: String = chars[start..end].iter().collect();
    let (path, line, column) = split_position(&raw)?;
    looks_like_path(&path, line.is_some()).then_some(Link {
        start,
        end,
        target: Target::File { path, line, column },
    })
}

/// Drop trailing sentence punctuation and closing brackets that have no opening partner in the
/// token.
fn trim_end(chars: &[char], start: usize, mut end: usize, url: bool) -> usize {
    loop {
        if end <= start {
            return end;
        }
        let last = chars[end - 1];
        let unbalanced = |open: char, close: char| {
            let token = &chars[start..end];
            token.iter().filter(|c| **c == close).count() > token.iter().filter(|c| **c == open).count()
        };
        let drop = match last {
            '.' | ',' | ';' | '!' | '?' => true,
            ':' => true,
            ')' => unbalanced('(', ')'),
            ']' => unbalanced('[', ']'),
            '}' => unbalanced('{', '}'),
            '*' | '_' if !url => true,
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= 1;
    }
}

/// Split a `:line`, `:line:col`, `(line)` or `(line,col)` suffix off a path.
fn split_position(raw: &str) -> Option<(String, Option<u32>, Option<u32>)> {
    let number = |s: &str| {
        (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse::<u32>().ok()).flatten()
    };
    if let Some(open) = raw.rfind('(')
        && raw.ends_with(')')
        && open > 0
    {
        let inner = &raw[open + 1..raw.len() - 1];
        let mut parts = inner.splitn(2, ',');
        if let Some(line) = parts.next().and_then(|s| number(s.trim())) {
            let column = parts.next().and_then(|s| number(s.trim()));
            return Some((raw[..open].to_owned(), Some(line), column));
        }
    }
    let mut path = raw;
    let mut numbers = Vec::new();
    while numbers.len() < 2
        && let Some(colon) = path.rfind(':')
        && let Some(n) = number(&path[colon + 1..])
    {
        numbers.push(n);
        path = &path[..colon];
    }
    // `C:` alone is a drive, not a path with a position.
    if path.is_empty() || (path.len() == 1 && numbers.len() == 1) {
        return Some((raw.to_owned(), None, None));
    }
    numbers.reverse();
    Some((path.to_owned(), numbers.first().copied(), numbers.get(1).copied()))
}

/// Whether a candidate reads as a file reference rather than prose: an explicit form (absolute,
/// home, `./`, `../`, a drive, UNC), or a relative path whose last part has an extension, or any
/// name followed by a position (`main.rs:12`).
fn looks_like_path(path: &str, has_position: bool) -> bool {
    if path.is_empty() || path.chars().any(char::is_control) {
        return false;
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let explicit = drive
        || path.starts_with("~/")
        || path.starts_with("./")
        || path.starts_with("../")
        || path.starts_with(".\\")
        || path.starts_with("..\\")
        || path.starts_with("\\\\")
        || (path.starts_with('/') && path.len() > 1 && !path.starts_with("//"));
    let has_separator = path.contains('/') || path.contains('\\');
    let last = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let has_extension = last.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=10).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    let wordy = path.chars().any(char::is_alphabetic);
    if !wordy {
        return false;
    }
    if explicit {
        return path.len() > 2;
    }
    if has_position {
        return has_extension || has_separator;
    }
    has_separator && has_extension
}

/// A bare email address becomes a `mailto:` link.
fn email(chars: &[char], start: usize, end: usize) -> Option<Link> {
    let end = trim_end(chars, start, end, false);
    let text: String = chars[start..end].iter().collect();
    let (user, domain) = text.split_once('@')?;
    let user_ok = !user.is_empty() && user.chars().all(|c| c.is_alphanumeric() || "._%+-".contains(c));
    let domain_ok = domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-');
    (user_ok && domain_ok).then(|| Link { start, end, target: Target::Scheme(format!("mailto:{text}")) })
}

/// The path of a `file:` URI, percent-decoded. A host other than `localhost` is kept as
/// a UNC-style `//host/...` path.
fn file_uri_path(uri: &str) -> Option<String> {
    let rest = uri.get(5..)?;
    let decoded = percent_decode(rest)?;
    let path = match decoded.strip_prefix("//") {
        Some(after) => {
            let (host, path) = after.split_at(after.find('/').unwrap_or(after.len()));
            if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
                path.to_owned()
            } else {
                format!("//{host}{path}")
            }
        }
        None => decoded,
    };
    // `file:///C:/x` names a drive path.
    let path = match path.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => path[1..].to_owned(),
        _ => path,
    };
    (!path.is_empty() && !path.chars().any(char::is_control)).then_some(path)
}

/// A Git Bash, MSYS, Cygwin or WSL spelling of a Windows drive path, as Windows spells it:
/// `/c/Users/x`, `/cygdrive/c/Users/x` and `/mnt/c/Users/x` all name `C:\Users\x`. `None` for
/// anything else, including a longer first segment such as `/cc/x` or `/mnt/data/x`.
#[must_use]
pub fn windows_drive_form(written: &str) -> Option<String> {
    let rest = written
        .strip_prefix("/mnt/")
        .or_else(|| written.strip_prefix("/cygdrive/"))
        .or_else(|| written.strip_prefix('/'))?;
    let mut chars = rest.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    let tail = chars.as_str();
    if !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    let tail = tail.trim_start_matches('/').replace('/', "\\");
    Some(format!("{}:\\{tail}", drive.to_ascii_uppercase()))
}

/// Percent-decoding that refuses what would decode to a control character (a link never
/// smuggles one) or to invalid UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            let b = u8::from_str_radix(hex, 16).ok()?;
            if b < 0x20 || b == 0x7f {
                return None;
            }
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_bash_msys_cygwin_and_wsl_drive_paths_have_a_windows_spelling() {
        assert_eq!(windows_drive_form("/c/Users/me/a.rs").as_deref(), Some(r"C:\Users\me\a.rs"));
        assert_eq!(windows_drive_form("/mnt/d/src/x.rs").as_deref(), Some(r"D:\src\x.rs"));
        assert_eq!(windows_drive_form("/cygdrive/e/notes.md").as_deref(), Some(r"E:\notes.md"));
        assert_eq!(windows_drive_form("/c").as_deref(), Some(r"C:\"));
        assert_eq!(windows_drive_form("/mnt/c/").as_deref(), Some(r"C:\"));
        for not_a_drive in ["/cc/x", "/mnt/data/x", "/usr/lib", "c/x", "/1/x", "", "/"] {
            assert_eq!(windows_drive_form(not_a_drive), None, "{not_a_drive}");
        }
    }

    fn file(path: &str, line: Option<u32>, column: Option<u32>) -> Target {
        Target::File { path: path.into(), line, column }
    }

    fn targets(text: &str) -> Vec<Target> {
        find(text).into_iter().map(|l| l.target).collect()
    }

    fn spans(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find(text).into_iter().map(|l| chars[l.start..l.end].iter().collect()).collect()
    }

    #[test]
    fn paths_of_every_form_with_their_positions() {
        assert_eq!(targets("see src/main.rs:12:5 now"), vec![file("src/main.rs", Some(12), Some(5))]);
        assert_eq!(targets("error at ./a.ts:3"), vec![file("./a.ts", Some(3), None)]);
        assert_eq!(targets("../docs/x.md"), vec![file("../docs/x.md", None, None)]);
        assert_eq!(targets("/usr/lib/os-release"), vec![file("/usr/lib/os-release", None, None)]);
        assert_eq!(targets("~/notes.txt"), vec![file("~/notes.txt", None, None)]);
        assert_eq!(targets(r"D:\git\x.ts(4,2)"), vec![file(r"D:\git\x.ts", Some(4), Some(2))]);
        assert_eq!(targets("main.rs:7"), vec![file("main.rs", Some(7), None)], "a bare name with a position");
    }

    #[test]
    fn trailing_punctuation_and_unbalanced_brackets_are_not_part_of_it() {
        assert_eq!(spans("(see src/a.rs)."), vec!["src/a.rs"]);
        assert_eq!(spans("[src/a.rs:3]"), vec!["src/a.rs:3"]);
        assert_eq!(spans("at src/a.rs:3:"), vec!["src/a.rs:3"]);
        assert_eq!(spans("https://example.com/a_(b)."), vec!["https://example.com/a_(b)"]);
    }

    #[test]
    fn prose_numbers_and_foreign_schemes_are_not_links() {
        for text in [
            "and/or",
            "1/2 of it",
            "e.g. this",
            "km/h",
            "javascript:alert(1)",
            "data:text/html,x",
            "ftp://x/y.txt",
            "a.b",
        ] {
            assert!(find(text).is_empty(), "{text:?} -> {:?}", find(text));
        }
    }

    #[test]
    fn web_addresses_schemes_and_emails() {
        assert_eq!(
            targets("go to https://x.dev/a?b=1, then"),
            vec![Target::Web("https://x.dev/a?b=1".into())]
        );
        assert_eq!(targets("mailto:a@b.co"), vec![Target::Scheme("mailto:a@b.co".into())]);
        assert_eq!(targets("write to me@example.org."), vec![Target::Scheme("mailto:me@example.org".into())]);
        assert_eq!(targets("https://"), vec![]);
    }

    #[test]
    fn quoted_paths_may_hold_spaces_and_file_uris_are_decoded() {
        assert_eq!(targets(r#"open "My Docs/plan.md" now"#), vec![file("My Docs/plan.md", None, None)]);
        assert_eq!(targets("file:///home/me/a%20b.txt"), vec![file("/home/me/a b.txt", None, None)]);
        assert_eq!(targets("file://localhost/etc/hosts:3"), vec![file("/etc/hosts", Some(3), None)]);
        assert_eq!(targets("file://server/share/x.txt"), vec![file("//server/share/x.txt", None, None)]);
        assert_eq!(targets("file:///C:/x/y.txt"), vec![file("C:/x/y.txt", None, None)]);
        assert!(find("file:///a%0Ab").is_empty(), "a control character is refused");
    }

    #[test]
    fn explicit_hyperlinks_are_judged_on_their_target() {
        assert_eq!(classify("https://x.dev"), Some(Target::Web("https://x.dev".into())));
        assert_eq!(classify("file:///tmp/a.txt"), Some(file("/tmp/a.txt", None, None)));
        assert_eq!(classify("mailto:a@b.co"), Some(Target::Scheme("mailto:a@b.co".into())));
        for refused in ["javascript:alert(1)", "data:text/html,x", "vbscript:x", "ssh://host", "https://"] {
            assert_eq!(classify(refused), None, "{refused}");
        }
    }

    #[test]
    fn offsets_are_characters_not_bytes() {
        let links = find("é src/a.rs");
        assert_eq!((links[0].start, links[0].end), (2, 10));
    }
}
