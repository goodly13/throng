//! Find in a terminal's scrollback: read-only, over the text as rendered, with
//! the editor's matching rules so the two agree.

use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use ropey::Rope;
use throng_editor::find::{self, Query};

/// A match: first and last cell, as (grid line, column). Lines are negative in the scrollback.
pub type CellSpan = ((i32, usize), (i32, usize));

/// One terminal panel's find session.
#[derive(Clone, Debug, Default)]
pub struct TermFind {
    pub query: Query,
    pub matches: Vec<CellSpan>,
    pub current: Option<usize>,
    /// What the matches were computed for: output seen, grid size, query.
    computed: Option<(u64, (u16, u16), Query)>,
    pub edited_at: Option<Instant>,
    pub focus_input: bool,
    /// Scroll the current match into view on the next frame.
    pub reveal: bool,
}

impl TermFind {
    /// Search again if the output, the size or the query changed (and the debounce passed).
    pub fn refresh<T>(
        &mut self,
        term: &Term<T>,
        version: u64,
        size: (u16, u16),
        debounce: Duration,
    ) -> Option<Duration> {
        if let Some(at) = self.edited_at {
            let elapsed = at.elapsed();
            if elapsed < debounce {
                return Some(debounce - elapsed);
            }
            self.edited_at = None;
            self.current = None;
        }
        let key = (version, size, self.query.clone());
        if self.computed.as_ref() == Some(&key) {
            return None;
        }
        let previous = self.current.and_then(|i| self.matches.get(i).copied());
        self.matches = search(term, &self.query);
        // Keep following the match the user was on; a new search starts at the newest output
        // (the bottom), which is where a terminal user is looking.
        self.current = match previous {
            Some(m) => self.matches.iter().position(|x| x.0 >= m.0).or(self.matches.len().checked_sub(1)),
            None => self.matches.len().checked_sub(1),
        };
        if previous.is_none() && self.current.is_some() {
            self.reveal = true;
        }
        self.computed = Some(key);
        None
    }

    pub fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.current = Some(match self.current {
            None => 0,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        });
        self.reveal = true;
    }

    #[must_use]
    pub fn current_match(&self) -> Option<CellSpan> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// Whether a cell is in a match, and in the current one.
    #[must_use]
    pub fn cell(&self, line: i32, col: usize) -> Option<bool> {
        let point = (line, col);
        let i = self.matches.partition_point(|m| m.1 < point);
        let m = self.matches.get(i)?;
        (m.0 <= point).then(|| self.current == Some(i))
    }
}

/// Every match of `query` in the scrollback and screen, in order. Rows the terminal wrapped are
/// searched as one line, so a match can span the wrap.
pub fn search<T>(term: &Term<T>, query: &Query) -> Vec<CellSpan> {
    if query.term.is_empty() {
        return Vec::new();
    }
    let grid = term.grid();
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32;
    let mut text = String::new();
    // For each character of `text`, the cell it came from (newlines map to the row's end).
    let mut cells: Vec<(i32, usize)> = Vec::new();
    for line in top..bottom {
        let row = &grid[Line(line)];
        for col in 0..cols {
            let cell = &row[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
            cells.push((line, col));
        }
        let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        if !wrapped {
            // Trailing blanks are not text.
            while text.ends_with(' ') && cells.last().is_some_and(|c| c.0 == line) {
                text.pop();
                cells.pop();
            }
            text.push('\n');
            cells.push((line, cols.saturating_sub(1)));
        }
    }
    let rope = Rope::from_str(&text);
    find::find_all(&rope, query, 100_000)
        .into_iter()
        .filter_map(|(from, to)| Some((*cells.get(from)?, *cells.get(to.checked_sub(1)?)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;

    fn term(cols: usize, bytes: &[u8]) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TermSize::new(cols, 5), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    fn q(t: &str) -> Query {
        Query { term: t.into(), case_sensitive: false, whole_word: false }
    }

    #[test]
    fn matches_come_back_as_cells_including_the_scrollback() {
        let t = term(20, b"one error\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix Error\r\n");
        let found = search(&t, &q("error"));
        assert_eq!(found.len(), 2);
        assert!(found[0].0.0 < 0, "the first is in the scrollback: {found:?}");
        assert_eq!(found[1].0.1, 4);
    }

    #[test]
    fn a_match_spanning_a_wrapped_row_is_found() {
        // 10 columns: "0123456789abc" wraps after '9'.
        let t = term(10, b"0123456789abc\r\n");
        let found = search(&t, &q("89ab"));
        assert_eq!(found, vec![((0, 8), (1, 1))]);
    }

    #[test]
    fn stepping_wraps_and_a_new_search_starts_at_the_newest_match() {
        let t = term(20, b"x\r\nx\r\nx\r\n");
        let mut f = TermFind { query: q("x"), ..TermFind::default() };
        f.refresh(&t, 1, (20, 5), Duration::ZERO);
        assert_eq!(f.current, Some(2));
        f.step(true);
        assert_eq!(f.current, Some(0));
        assert_eq!(f.cell(0, 0), Some(true));
        assert_eq!(f.cell(1, 0), Some(false));
        assert_eq!(f.cell(1, 1), None);
    }
}
