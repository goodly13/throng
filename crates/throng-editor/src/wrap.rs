//! Visual rows: how a document's lines break across the view at a given width.
//!
//! The editor draws in a monospace font, so wrapping is arithmetic on display columns (tabs, wide
//! characters) and never needs the font. Only a row count per line is kept for the whole
//! document; the break positions of a line are recomputed when it is drawn.

use ropey::Rope;

use crate::doc::Applied;
use crate::lines::{self, char_width};

/// Where a line's visual rows start (character indices within the line; the first is 0).
#[must_use]
pub fn row_starts(line: &str, width: Option<usize>, tab: usize) -> Vec<usize> {
    let Some(width) = width.filter(|w| *w > 0) else { return vec![0] };
    let chars: Vec<char> = line.chars().collect();
    let mut starts = vec![0];
    let mut row_start = 0;
    let mut col = 0;
    // The index after the most recent space in this row: a preferred break.
    let mut soft_break: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let w = char_width(c, col, tab);
        if col + w > width && i > row_start {
            let at = match soft_break {
                Some(b) if b > row_start && b <= i => b,
                _ => i,
            };
            starts.push(at);
            row_start = at;
            soft_break = None;
            col = 0;
            i = at;
            continue;
        }
        col += w;
        if c == ' ' || c == '\t' {
            soft_break = Some(i + 1);
        }
        i += 1;
    }
    starts
}

/// Row counts for every line of a document at one width.
#[derive(Clone, Debug)]
pub struct Wrap {
    width: Option<usize>,
    tab: usize,
    counts: Vec<u32>,
    /// `prefix[i]` = rows before line `i`; rebuilt on demand.
    prefix: Vec<usize>,
    prefix_valid: bool,
}

impl Wrap {
    /// Wrap `rope` at `width` columns (`None`: one row per line).
    #[must_use]
    pub fn new(rope: &Rope, width: Option<usize>, tab: usize) -> Self {
        let mut wrap = Self { width, tab, counts: Vec::new(), prefix: Vec::new(), prefix_valid: false };
        wrap.rebuild(rope);
        wrap
    }

    #[must_use]
    pub fn width(&self) -> Option<usize> {
        self.width
    }

    #[must_use]
    pub fn tab(&self) -> usize {
        self.tab
    }

    /// Recount every line (after a width or tab change).
    pub fn rebuild(&mut self, rope: &Rope) {
        self.counts = (0..rope.len_lines()).map(|l| self.count(rope, l)).collect();
        self.prefix_valid = false;
    }

    pub fn set_width(&mut self, rope: &Rope, width: Option<usize>, tab: usize) {
        if self.width != width || self.tab != tab {
            self.width = width;
            self.tab = tab;
            self.rebuild(rope);
        }
    }

    fn count(&self, rope: &Rope, line: usize) -> u32 {
        if self.width.is_none() {
            return 1;
        }
        row_starts(&lines::line_text(rope, line), self.width, self.tab).len() as u32
    }

    /// Carry the counts across an edit, recounting only the lines it touched.
    pub fn apply(&mut self, rope: &Rope, applied: &Applied) {
        let total = rope.len_lines();
        let old_last = (applied.last_line as isize - applied.line_delta).max(applied.first_line as isize - 1);
        let old_range = applied.first_line.min(self.counts.len())
            ..((old_last + 1).max(0) as usize).clamp(applied.first_line, self.counts.len());
        let new_lines = applied.first_line..(applied.last_line + 1).min(total);
        let fresh: Vec<u32> = new_lines.map(|l| self.count(rope, l)).collect();
        self.counts.splice(old_range, fresh);
        if self.counts.len() != total {
            // Should not happen; stay correct if it does.
            self.rebuild(rope);
        }
        self.prefix_valid = false;
    }

    fn ensure_prefix(&mut self) {
        if self.prefix_valid {
            return;
        }
        self.prefix.clear();
        self.prefix.reserve(self.counts.len() + 1);
        let mut sum = 0usize;
        for &c in &self.counts {
            self.prefix.push(sum);
            sum += c as usize;
        }
        self.prefix.push(sum);
        self.prefix_valid = true;
    }

    /// All visual rows in the document.
    pub fn total_rows(&mut self) -> usize {
        self.ensure_prefix();
        self.prefix.last().copied().unwrap_or(1).max(1)
    }

    /// The first visual row of `line`.
    pub fn first_row_of(&mut self, line: usize) -> usize {
        self.ensure_prefix();
        self.prefix[line.min(self.counts.len().saturating_sub(1))]
    }

    /// The line holding visual row `row`, and which of its rows it is.
    pub fn line_at_row(&mut self, row: usize) -> (usize, usize) {
        self.ensure_prefix();
        let lines = self.counts.len();
        if lines == 0 {
            return (0, 0);
        }
        // Last line whose first row is <= row.
        let line = match self.prefix[..lines].binary_search(&row) {
            Ok(mut l) => {
                // Lines with zero rows cannot exist, but equal prefixes would; take the last.
                while l + 1 < lines && self.prefix[l + 1] == row {
                    l += 1;
                }
                l
            }
            Err(l) => l.saturating_sub(1),
        };
        let line = line.min(lines - 1);
        (line, row - self.prefix[line])
    }

    /// Visual row and display column of a character position.
    pub fn locate(&mut self, rope: &Rope, pos: usize) -> (usize, usize) {
        let line = lines::line_of(rope, pos);
        let text = lines::line_text(rope, line);
        let index = pos - lines::line_start(rope, line);
        let starts = row_starts(&text, self.width, self.tab);
        let sub = starts.iter().rposition(|s| *s <= index).unwrap_or(0);
        let row_text: String = text.chars().skip(starts[sub]).collect();
        let col = lines::display_col(&row_text, index - starts[sub], self.tab);
        (self.first_row_of(line) + sub, col)
    }

    /// The character position in visual row `row` nearest display column `col`.
    pub fn position(&mut self, rope: &Rope, row: usize, col: usize) -> usize {
        let last = self.total_rows() - 1;
        let (line, sub) = self.line_at_row(row.min(last));
        let text = lines::line_text(rope, line);
        let starts = row_starts(&text, self.width, self.tab);
        let sub = sub.min(starts.len() - 1);
        let start = starts[sub];
        let end = starts.get(sub + 1).copied().unwrap_or_else(|| text.chars().count());
        let row_text: String = text.chars().skip(start).take(end - start).collect();
        let mut index = lines::index_at_col(&row_text, col, self.tab);
        // A wrapped row's last position belongs to the next row; stay on this one.
        if sub + 1 < starts.len() && index >= end - start && end > start {
            index = end - start - 1;
        }
        lines::line_start(rope, line) + start + index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::Transaction;
    use crate::doc::TextDoc;
    use crate::history::EditKind;
    use crate::selection::Selection;

    #[test]
    fn long_lines_break_after_spaces_when_they_can() {
        assert_eq!(row_starts("hello world again", Some(12), 4), vec![0, 12]);
        assert_eq!(row_starts("abcdefghij", Some(4), 4), vec![0, 4, 8]);
        assert_eq!(row_starts("short", Some(80), 4), vec![0]);
        assert_eq!(row_starts("anything", None, 4), vec![0]);
    }

    #[test]
    fn rows_map_to_lines_and_back() {
        let rope = Rope::from_str("abcdefghij\nxy\n");
        let mut wrap = Wrap::new(&rope, Some(4), 4);
        assert_eq!(wrap.total_rows(), 5);
        assert_eq!(wrap.line_at_row(2), (0, 2));
        assert_eq!(wrap.line_at_row(3), (1, 0));
        assert_eq!(wrap.locate(&rope, 9), (2, 1));
        assert_eq!(wrap.position(&rope, 1, 2), 6);
        // Past the end of a wrapped row stays on that row.
        assert_eq!(wrap.position(&rope, 0, 9), 3);
    }

    #[test]
    fn edits_recount_only_what_changed() {
        let mut doc = TextDoc::new("aaaa aaaa\nb\ncccc cccc\n");
        let mut wrap = Wrap::new(doc.rope(), Some(5), 4);
        assert_eq!(wrap.total_rows(), 6);
        let tx = Transaction::replace(doc.rope(), vec![(10, 11, "bbbb bbbb\nbb".into())]);
        let applied = doc.edit(tx, Selection::point(0), Selection::point(0), EditKind::Other, 0).unwrap();
        wrap.apply(doc.rope(), &applied);
        let fresh = Wrap::new(doc.rope(), Some(5), 4);
        assert_eq!(wrap.counts, fresh.counts);
    }
}
