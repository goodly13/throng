//! Links on a terminal's screen: paths and addresses found in the text, one link across a
//! soft wrap, and the hyperlinks programs emit with OSC 8, judged on their target.

use std::collections::HashSet;

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use throng_core::links::{self, Target};

/// A link and the cells it covers: `(screen row, first column, end column)` per row it spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenLink {
    pub cells: Vec<(i32, usize, usize)>,
    pub target: Target,
}

impl ScreenLink {
    #[must_use]
    pub fn contains(&self, row: i32, col: usize) -> bool {
        self.cells.iter().any(|&(r, from, to)| r == row && from <= col && col < to)
    }

    fn add(&mut self, row: i32, col: usize, width: usize) {
        match self.cells.last_mut() {
            Some((r, _, to)) if *r == row && *to == col => *to = col + width,
            _ => self.cells.push((row, col, col + width)),
        }
    }
}

/// Every link on the visible screen.
pub fn scan<T: EventListener>(term: &Term<T>) -> Vec<ScreenLink> {
    let grid = term.grid();
    let offset = grid.display_offset() as i32;
    let rows = grid.screen_lines() as i32;
    let cols = grid.columns();
    let mut out: Vec<ScreenLink> = Vec::new();

    // Hyperlinks a program marked, grouped by their id while their cells run on.
    let mut marked: HashSet<(i32, usize)> = HashSet::new();
    let mut open: Option<(String, ScreenLink)> = None;
    for r in 0..rows {
        let row = &grid[Line(r - offset)];
        for c in 0..cols {
            let cell = &row[Column(c)];
            let Some(link) = cell.hyperlink() else {
                if let Some((_, done)) = open.take() {
                    out.push(done);
                }
                continue;
            };
            let Some(target) = links::classify(link.uri()) else { continue };
            marked.insert((r, c));
            match &mut open {
                Some((id, current)) if id == link.id() => current.add(r, c, 1),
                _ => {
                    if let Some((_, done)) = open.take() {
                        out.push(done);
                    }
                    let mut fresh = ScreenLink { cells: Vec::new(), target };
                    fresh.add(r, c, 1);
                    open = Some((link.id().to_owned(), fresh));
                }
            }
        }
    }
    if let Some((_, done)) = open {
        out.push(done);
    }

    // Links in the text, a soft-wrapped line read as one.
    let mut r = 0;
    while r < rows {
        let mut text = String::new();
        let mut map: Vec<(i32, usize, usize)> = Vec::new();
        loop {
            let row = &grid[Line(r - offset)];
            for c in 0..cols {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
                text.push(cell.c);
                map.push((r, c, width));
            }
            let wrapped = cols > 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            r += 1;
            if !wrapped || r >= rows {
                break;
            }
        }
        for found in links::find(&text) {
            let span = &map[found.start..found.end];
            if span.iter().any(|&(row, col, _)| marked.contains(&(row, col))) {
                continue;
            }
            let mut link = ScreenLink { cells: Vec::new(), target: found.target };
            for &(row, col, width) in span {
                link.add(row, col, width);
            }
            out.push(link);
        }
    }
    out
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

    #[test]
    fn a_path_that_wraps_is_one_link_on_both_rows() {
        let t = term(10, b"at src/long/name.rs:4 ok");
        let links = scan(&t);
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(
            links[0].target,
            Target::File { path: "src/long/name.rs".into(), line: Some(4), column: None }
        );
        assert_eq!(links[0].cells, vec![(0, 3, 10), (1, 0, 10), (2, 0, 1)]);
        assert!(links[0].contains(1, 5));
    }

    #[test]
    fn osc8_hyperlinks_are_judged_on_their_target_and_unsafe_ones_ignored() {
        let t = term(
            40,
            b"\x1b]8;;https://x.dev\x1b\\docs\x1b]8;;\x1b\\ \x1b]8;;javascript:x\x1b\\bad\x1b]8;;\x1b\\",
        );
        let links = scan(&t);
        assert_eq!(
            links,
            vec![ScreenLink { cells: vec![(0, 0, 4)], target: Target::Web("https://x.dev".into()) }]
        );
    }
}
