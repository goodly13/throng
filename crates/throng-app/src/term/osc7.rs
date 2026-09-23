//! OSC 7 (`ESC ] 7 ; file://host/path BEL`): a shell reporting its working directory. Many shells
//! and prompts send it; throng takes it when the host is this machine, as a more exact answer than
//! the shell process's own directory (which a nested shell or `tmux` hides).

use std::path::PathBuf;

/// The longest report kept while waiting for its end.
const LIMIT: usize = 4096;

/// Finds reports in output that arrives in pieces.
#[derive(Default)]
pub struct Osc7 {
    /// A report whose start has been seen but not its end.
    partial: Option<Vec<u8>>,
    /// The last byte seen was ESC (a string terminator may follow in the next piece).
    escape: bool,
}

impl Osc7 {
    /// The last complete report's URI in `data`, if any.
    pub fn scan(&mut self, data: &[u8]) -> Option<String> {
        const START: &[u8] = b"\x1b]7;";
        let mut found = None;
        let mut i = 0;
        while i < data.len() {
            if let Some(partial) = &mut self.partial {
                let byte = data[i];
                i += 1;
                let end = byte == 0x07 || (self.escape && byte == b'\\');
                if end {
                    if self.escape {
                        partial.pop();
                    }
                    found = String::from_utf8(std::mem::take(partial)).ok();
                    self.partial = None;
                    self.escape = false;
                    continue;
                }
                self.escape = byte == 0x1b;
                partial.push(byte);
                if partial.len() > LIMIT {
                    self.partial = None;
                    self.escape = false;
                }
                continue;
            }
            // Look for the start, which may itself straddle two pieces only in the rare case a
            // piece ends mid-introducer: such a report is missed, and the next prompt sends another.
            match data[i..].windows(START.len()).position(|w| w == START) {
                Some(at) => {
                    i += at + START.len();
                    self.partial = Some(Vec::new());
                    self.escape = false;
                }
                None => break,
            }
        }
        found
    }
}

/// The directory a report names, when its host is this machine (empty, `localhost` or our name).
#[must_use]
pub fn local_directory(uri: &str, hostname: Option<&str>) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || hostname.is_some_and(|h| host.eq_ignore_ascii_case(h));
    if !local {
        return None;
    }
    match throng_core::links::classify(&format!("file://{path}")) {
        Some(throng_core::links::Target::File { path, .. }) => Some(PathBuf::from(path)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_found_across_pieces_and_either_terminator() {
        let mut scan = Osc7::default();
        assert_eq!(scan.scan(b"prompt \x1b]7;file://box/home/me/a%20b"), None);
        assert_eq!(scan.scan(b"\x07$ ").as_deref(), Some("file://box/home/me/a%20b"));
        assert_eq!(scan.scan(b"\x1b]7;file:///tmp\x1b").as_deref(), None);
        assert_eq!(scan.scan(b"\\").as_deref(), Some("file:///tmp"));
        assert_eq!(scan.scan(b"no report here"), None);
    }

    #[test]
    fn only_this_machine_counts() {
        assert_eq!(local_directory("file://box/home/me/a%20b", Some("box")), Some("/home/me/a b".into()));
        assert_eq!(local_directory("file:///tmp", None), Some("/tmp".into()));
        assert_eq!(local_directory("file://server/srv", Some("box")), None, "a remote shell over ssh");
        assert_eq!(local_directory("https://x", None), None);
    }
}
