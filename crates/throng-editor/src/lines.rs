//! Line and character helpers over a rope.

use ropey::Rope;
use unicode_width::UnicodeWidthChar;

/// The line holding character offset `pos`.
#[must_use]
pub fn line_of(rope: &Rope, pos: usize) -> usize {
    rope.char_to_line(pos.min(rope.len_chars()))
}

/// Offset of the first character of `line`.
#[must_use]
pub fn line_start(rope: &Rope, line: usize) -> usize {
    rope.line_to_char(line.min(rope.len_lines().saturating_sub(1)))
}

/// Offset just before the line's `\n` (or the end of the document).
#[must_use]
pub fn line_end(rope: &Rope, line: usize) -> usize {
    let line = line.min(rope.len_lines().saturating_sub(1));
    let slice = rope.line(line);
    let len = slice.len_chars();
    let trailing = usize::from(len > 0 && slice.char(len - 1) == '\n');
    rope.line_to_char(line) + len - trailing
}

/// The line's text without its `\n`.
#[must_use]
pub fn line_text(rope: &Rope, line: usize) -> String {
    let mut text = rope.line(line.min(rope.len_lines().saturating_sub(1))).to_string();
    if text.ends_with('\n') {
        text.pop();
    }
    text
}

/// Characters in the line, without its `\n`.
#[must_use]
pub fn line_len(rope: &Rope, line: usize) -> usize {
    line_end(rope, line) - line_start(rope, line)
}

/// How a character groups for word movement and double-click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharClass {
    Word,
    Space,
    Newline,
    Punctuation,
}

#[must_use]
pub fn class(c: char) -> CharClass {
    if c == '\n' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Columns a character occupies at display column `col` (tabs reach the next stop).
#[must_use]
pub fn char_width(c: char, col: usize, tab: usize) -> usize {
    if c == '\t' {
        let tab = tab.max(1);
        tab - col % tab
    } else {
        c.width().unwrap_or(0)
    }
}

/// Display column of character `index` within `line` text.
#[must_use]
pub fn display_col(line: &str, index: usize, tab: usize) -> usize {
    let mut col = 0;
    for c in line.chars().take(index) {
        col += char_width(c, col, tab);
    }
    col
}

/// The character index in `line` nearest display column `target` (never past the end).
#[must_use]
pub fn index_at_col(line: &str, target: usize, tab: usize) -> usize {
    let mut col = 0;
    for (index, c) in line.chars().enumerate() {
        let w = char_width(c, col, tab);
        if col + w > target {
            // Land on the nearer side of a wide character or tab.
            return if target - col > w / 2 && w > 1 { index + 1 } else { index };
        }
        col += w;
    }
    line.chars().count()
}

/// The leading whitespace of a line's text.
#[must_use]
pub fn leading_whitespace(line: &str) -> &str {
    let end = line.find(|c: char| c != ' ' && c != '\t').unwrap_or(line.len());
    &line[..end]
}

/// A column in UTF-16 code units, as compilers and linters count. 1-based.
#[must_use]
pub fn utf16_column(rope: &Rope, pos: usize) -> usize {
    let pos = pos.min(rope.len_chars());
    let start = rope.line_to_char(rope.char_to_line(pos));
    rope.slice(start..pos).chars().map(char::len_utf16).sum::<usize>() + 1
}

/// A document's size as the status bar reports it: characters in UTF-16 code units,
/// line breaks included, and words as runs of non-whitespace.
#[must_use]
pub fn counts(rope: &Rope) -> (usize, usize) {
    let mut words = 0;
    let mut in_word = false;
    for chunk in rope.chunks() {
        for c in chunk.chars() {
            let space = c.is_whitespace();
            if !space && !in_word {
                words += 1;
            }
            in_word = !space;
        }
    }
    (rope.len_utf16_cu(), words)
}

/// Characters (UTF-16 code units) between two character positions, as the selection readout
/// counts them.
#[must_use]
pub fn utf16_between(rope: &Rope, from: usize, to: usize) -> usize {
    let (a, b) = (from.min(to).min(rope.len_chars()), from.max(to).min(rope.len_chars()));
    rope.char_to_utf16_cu(b) - rope.char_to_utf16_cu(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_take_utf16_units_line_breaks_and_whitespace_runs() {
        let rope = Rope::from_str("one two\n  three 😀\n");
        // 18 characters, one of them two UTF-16 units.
        assert_eq!(counts(&rope), (19, 4));
        assert_eq!(counts(&Rope::new()), (0, 0));
        assert_eq!(utf16_between(&rope, 14, 17), 4, "the emoji counts twice");
    }

    #[test]
    fn line_bounds_exclude_the_newline() {
        let rope = Rope::from_str("ab\ncd\n");
        assert_eq!((line_start(&rope, 1), line_end(&rope, 1)), (3, 5));
        assert_eq!(line_text(&rope, 1), "cd");
        assert_eq!(rope.len_lines(), 3);
        assert_eq!((line_start(&rope, 2), line_end(&rope, 2)), (6, 6));
    }

    #[test]
    fn columns_expand_tabs_and_wide_characters() {
        assert_eq!(display_col("\tab", 1, 4), 4);
        assert_eq!(display_col("a\tb", 2, 4), 4);
        assert_eq!(display_col("日本x", 2, 4), 4);
        assert_eq!(index_at_col("日本x", 4, 4), 2);
        assert_eq!(index_at_col("ab", 9, 4), 2);
    }

    #[test]
    fn utf16_columns_count_surrogate_pairs() {
        let rope = Rope::from_str("a😀b");
        assert_eq!(utf16_column(&rope, 2), 4);
    }
}
