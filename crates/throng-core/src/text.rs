//! Byte-exact text round trips.
//!
//! A document is held LF-normalised for editing, and saved back with the encoding, BOM and line
//! endings it was read with. A file with mixed endings keeps each unchanged line's own ending; only
//! lines the user actually edited take the dominant one. Bytes that are not valid text are refused
//! rather than decoded lossily: decoding a Windows-1252 `é` to U+FFFD would write that back on
//! the next save.

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffOp, capture_diff_slices};
use thiserror::Error;

/// How the bytes were encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    Utf8 { bom: bool },
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 { bom: false } => "UTF-8",
            Self::Utf8 { bom: true } => "UTF-8 with BOM",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
        }
    }
}

/// A line terminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
            Self::Cr => "CR",
        }
    }
}

/// Why bytes could not be opened as text.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum DecodeError {
    #[error("This looks like a binary file.")]
    Binary,
    #[error(
        "This file is not valid UTF-8 (first bad byte at offset {offset}). throng will not open it, so it cannot be re-encoded by accident."
    )]
    InvalidUtf8 { offset: usize },
    #[error("This file is not valid UTF-16.")]
    InvalidUtf16,
}

/// Everything needed to write a document back the way it was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFormat {
    pub encoding: Encoding,
    /// The ending used for new and edited lines.
    pub eol: LineEnding,
    /// Present only for files whose lines did not all end the same way: the original lines (without
    /// terminators) and each one's terminator, so unchanged lines keep theirs on save.
    pub mixed: Option<Vec<(String, Option<LineEnding>)>>,
}

impl TextFormat {
    /// The format for a brand-new document.
    #[must_use]
    pub fn new_document(eol: LineEnding) -> Self {
        Self { encoding: Encoding::Utf8 { bom: false }, eol, mixed: None }
    }

    /// A human summary for the status bar, e.g. "UTF-8 · CRLF" or "UTF-8 · Mixed (LF)".
    #[must_use]
    pub fn describe(&self) -> String {
        let eol = if self.mixed.is_some() {
            format!("Mixed ({})", self.eol.label())
        } else {
            self.eol.label().to_owned()
        };
        format!("{} · {}", self.encoding.label(), eol)
    }
}

/// A decoded document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// LF-normalised text.
    pub text: String,
    pub format: TextFormat,
}

const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Decode file bytes. `default_eol` applies to files with no line breaks at all.
pub fn decode(bytes: &[u8], default_eol: LineEnding) -> Result<Decoded, DecodeError> {
    let (encoding, raw) = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (Encoding::Utf8 { bom: true }, utf8(rest, 3)?)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        (Encoding::Utf16Le, utf16(rest, encoding_rs::UTF_16LE)?)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        (Encoding::Utf16Be, utf16(rest, encoding_rs::UTF_16BE)?)
    } else {
        if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
            return Err(DecodeError::Binary);
        }
        (Encoding::Utf8 { bom: false }, utf8(bytes, 0)?)
    };
    let lines = split_lines(&raw);
    let (eol, mixed) = classify(&lines, default_eol);
    let text = lines
        .iter()
        .map(|(content, ending)| if ending.is_some() { format!("{content}\n") } else { content.clone() })
        .collect();
    Ok(Decoded { text, format: TextFormat { encoding, eol, mixed: if mixed { Some(lines) } else { None } } })
}

fn utf8(bytes: &[u8], offset_base: usize) -> Result<String, DecodeError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|e| DecodeError::InvalidUtf8 { offset: offset_base + e.valid_up_to() })
}

fn utf16(bytes: &[u8], encoding: &'static encoding_rs::Encoding) -> Result<String, DecodeError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(DecodeError::InvalidUtf16);
    }
    encoding
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(std::borrow::Cow::into_owned)
        .ok_or(DecodeError::InvalidUtf16)
}

/// Split raw text into lines and their terminators. The final line has `None` unless the text ends
/// with a terminator, in which case no empty trailing line is produced.
fn split_lines(raw: &str) -> Vec<(String, Option<LineEnding>)> {
    let mut lines = Vec::new();
    let bytes = raw.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push((raw[start..i].to_owned(), Some(LineEnding::Lf)));
                i += 1;
                start = i;
            }
            b'\r' => {
                let crlf = bytes.get(i + 1) == Some(&b'\n');
                lines.push((
                    raw[start..i].to_owned(),
                    Some(if crlf { LineEnding::CrLf } else { LineEnding::Cr }),
                ));
                i += if crlf { 2 } else { 1 };
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        lines.push((raw[start..].to_owned(), None));
    }
    lines
}

/// The dominant ending (ties prefer LF, then CRLF) and whether more than one kind occurs.
fn classify(lines: &[(String, Option<LineEnding>)], default_eol: LineEnding) -> (LineEnding, bool) {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    for (_, ending) in lines {
        match ending {
            Some(LineEnding::Lf) => lf += 1,
            Some(LineEnding::CrLf) => crlf += 1,
            Some(LineEnding::Cr) => cr += 1,
            None => {}
        }
    }
    let kinds = [lf, crlf, cr].iter().filter(|n| **n > 0).count();
    let dominant = if kinds == 0 {
        default_eol
    } else if lf >= crlf && lf >= cr {
        LineEnding::Lf
    } else if crlf >= cr {
        LineEnding::CrLf
    } else {
        LineEnding::Cr
    };
    (dominant, kinds > 1)
}

/// Encode LF-normalised `text` in `format`.
#[must_use]
pub fn encode(text: &str, format: &TextFormat) -> Vec<u8> {
    let new_lines: Vec<&str> = {
        let mut parts: Vec<&str> = text.split('\n').collect();
        // "a\n" splits to ["a", ""]: the trailing empty piece is not a line.
        if text.ends_with('\n') {
            parts.pop();
        }
        if text.is_empty() {
            parts.clear();
        }
        parts
    };
    let ends_with_newline = text.ends_with('\n');
    let mut endings: Vec<LineEnding> = vec![format.eol; new_lines.len()];
    if let Some(original) = &format.mixed {
        let old_contents: Vec<&str> = original.iter().map(|(c, _)| c.as_str()).collect();
        for op in capture_diff_slices(Algorithm::Myers, &old_contents, &new_lines) {
            if let DiffOp::Equal { old_index, new_index, len } = op {
                for k in 0..len {
                    if let Some(ending) = original[old_index + k].1 {
                        endings[new_index + k] = ending;
                    }
                }
            }
        }
    }
    let mut joined = String::with_capacity(text.len() + new_lines.len());
    for (i, line) in new_lines.iter().enumerate() {
        joined.push_str(line);
        let last = i + 1 == new_lines.len();
        if !last || ends_with_newline {
            joined.push_str(endings[i].as_str());
        }
    }
    match format.encoding {
        Encoding::Utf8 { bom } => {
            let mut out = Vec::with_capacity(joined.len() + 3);
            if bom {
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            out.extend_from_slice(joined.as_bytes());
            out
        }
        Encoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE];
            for unit in joined.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out
        }
        Encoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF];
            for unit in joined.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
            out
        }
    }
}

/// Indentation inferred from a document's own lines (Principle XI: document state, not view state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    Tabs,
    Spaces(usize),
}

impl Indent {
    #[must_use]
    pub fn unit(self) -> String {
        match self {
            Self::Tabs => "\t".to_owned(),
            Self::Spaces(n) => " ".repeat(n),
        }
    }
}

/// Infer a document's indentation. The first `min(ceil(10% of lines), 100)` lines
/// are sampled (at least one), and only their first 20 characters, so the cost does not grow with
/// the file. Of the sampled lines that begin with whitespace: any whose whitespace starts with a
/// tab means tabs; otherwise the most frequent leading-space count wins, ties to the smaller.
/// Whitespace running past the 20 inspected characters has no known width and is not counted.
/// `None` when no style emerges, and the configured profile applies.
#[must_use]
pub fn infer_indent(text: &str) -> Option<Indent> {
    const INSPECTED: usize = 20;
    let total = text.lines().count();
    let sample = total.div_ceil(10).clamp(1, 100);
    let mut counts: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for line in text.lines().take(sample) {
        let prefix: String = line.chars().take(INSPECTED).collect();
        match prefix.chars().next() {
            Some('\t') => return Some(Indent::Tabs),
            Some(c) if c.is_whitespace() => {}
            _ => continue,
        }
        let spaces = prefix.chars().take_while(|c| *c == ' ').count();
        let indeterminate = spaces == prefix.chars().count() && line.chars().count() > INSPECTED;
        if spaces > 0 && !indeterminate {
            *counts.entry(spaces).or_default() += 1;
        }
    }
    // BTreeMap iterates widths in ascending order, so the first maximum is the smaller width.
    let best = counts.iter().fold(None, |best: Option<(usize, usize)>, (&w, &n)| match best {
        Some((_, bn)) if bn >= n => best,
        _ => Some((w, n)),
    });
    best.map(|(width, _)| Indent::Spaces(width))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(bytes: &[u8]) {
        let decoded = decode(bytes, LineEnding::Lf).expect("decodes");
        assert_eq!(encode(&decoded.text, &decoded.format), bytes, "round trip of {bytes:?}");
    }

    #[test]
    fn unchanged_documents_round_trip_byte_for_byte() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"a\n");
        round_trip(b"a\r\nb\r\n");
        round_trip(b"a\r\nb");
        round_trip(b"a\rb\r");
        round_trip(b"a\nb\r\nc\rd");
        round_trip(b"\n\n\r\n");
        round_trip("\u{feff}caf\u{e9}\r\n".as_bytes());
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "hi\r\nthere".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        round_trip(&utf16);
        let mut utf16be = vec![0xFE, 0xFF];
        for u in "x\ny".encode_utf16() {
            utf16be.extend_from_slice(&u.to_be_bytes());
        }
        round_trip(&utf16be);
    }

    #[test]
    fn text_is_lf_normalised_and_format_recorded() {
        let d = decode(b"a\r\nb\r\n", LineEnding::Lf).unwrap();
        assert_eq!(d.text, "a\nb\n");
        assert_eq!(d.format.eol, LineEnding::CrLf);
        assert!(d.format.mixed.is_none());
        assert_eq!(d.format.describe(), "UTF-8 · CRLF");
        let bom = decode(b"\xEF\xBB\xBFx", LineEnding::Lf).unwrap();
        assert_eq!(bom.format.encoding, Encoding::Utf8 { bom: true });
        assert_eq!(bom.text, "x");
    }

    #[test]
    fn edits_keep_the_files_line_ending() {
        let d = decode(b"a\r\nb\r\n", LineEnding::Lf).unwrap();
        assert_eq!(encode("a\nnew\nb\n", &d.format), b"a\r\nnew\r\nb\r\n");
    }

    #[test]
    fn mixed_files_only_change_edited_lines() {
        let d = decode(b"one\ntwo\r\nthree\rfour\n", LineEnding::Lf).unwrap();
        assert_eq!(d.format.describe(), "UTF-8 · Mixed (LF)");
        // Edit "three", insert a line after "one".
        let edited = "one\ninserted\ntwo\nTHREE\nfour\n";
        assert_eq!(encode(edited, &d.format), b"one\ninserted\ntwo\r\nTHREE\nfour\n");
    }

    #[test]
    fn invalid_utf8_and_binary_are_refused() {
        // Windows-1252 "café": the é is 0xE9, not valid UTF-8.
        assert_eq!(decode(b"caf\xE9", LineEnding::Lf), Err(DecodeError::InvalidUtf8 { offset: 3 }));
        assert_eq!(
            decode(b"\xEF\xBB\xBFok\xFF", LineEnding::Lf),
            Err(DecodeError::InvalidUtf8 { offset: 5 })
        );
        assert_eq!(decode(b"PNG\0\0data", LineEnding::Lf), Err(DecodeError::Binary));
        assert_eq!(decode(&[0xFF, 0xFE, 0x41], LineEnding::Lf), Err(DecodeError::InvalidUtf16));
    }

    #[test]
    fn files_without_breaks_take_the_default() {
        let d = decode(b"single", LineEnding::CrLf).unwrap();
        assert_eq!(d.format.eol, LineEnding::CrLf);
        assert_eq!(encode("single\nmore", &d.format), b"single\r\nmore");
    }

    #[test]
    fn indentation_is_inferred() {
        // 20 lines sample 2; the second begins with a tab.
        let tabs = format!("fn a() {{\n\tx();\n{}", "}\n".repeat(18));
        assert_eq!(infer_indent(&tabs), Some(Indent::Tabs));
        // 40 lines sample 4: widths 2, 4, 2 → 2.
        let yaml = format!("a:\n  b:\n    c: 1\n  d: 2\n{}", "e: 3\n".repeat(36));
        assert_eq!(infer_indent(&yaml), Some(Indent::Spaces(2)));
        // A tie goes to the smaller width.
        let tie = format!("x\n    y\n  z\n{}", "w\n".repeat(27));
        assert_eq!(infer_indent(&tie), Some(Indent::Spaces(2)));
        assert_eq!(infer_indent("flat\ntext\n"), None);
        // A one-line file still samples its one line.
        assert_eq!(infer_indent("    return 1"), Some(Indent::Spaces(4)));
        // Whitespace past the inspected 20 characters has no known width.
        let deep = format!("{}x\n{}", " ".repeat(24), "y\n".repeat(9));
        assert_eq!(infer_indent(&deep), None);
        // Only the sample counts: indentation after it is not seen.
        let late = format!("{}    z\n", "a\n".repeat(30));
        assert_eq!(infer_indent(&late), None);
    }
}
