//! Editing and movement: what keys and menu items do to a document and a view's selection.
//!
//! Every edit is one transaction, so one command is one undo step however many carets it touched.

use ropey::Rope;

use crate::change::{Assoc, Transaction};
use crate::doc::{Applied, TextDoc};
use crate::history::EditKind;
use crate::lines::{self, CharClass, class};
use crate::selection::{Range, Selection};
use crate::wrap::Wrap;

/// How the document indents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndentStyle {
    pub tabs: bool,
    /// Spaces per level (and the tab stop width when `tabs`).
    pub width: usize,
}

impl IndentStyle {
    #[must_use]
    pub fn unit(&self) -> String {
        if self.tabs { "\t".into() } else { " ".repeat(self.width.max(1)) }
    }
}

/// What an editing command did.
#[derive(Debug)]
pub struct Edited {
    pub applied: Option<Applied>,
    /// The editing view's selection afterwards.
    pub selection: Selection,
}

/// How far a horizontal move goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Char,
    Word,
}

// ------------------------------------------------------------------------------------------------
// Movement

/// Left or right by a character or a word. Without `extend`, a selection collapses to its edge
/// first.
#[must_use]
pub fn move_horizontal(rope: &Rope, sel: &Selection, forward: bool, unit: Unit, extend: bool) -> Selection {
    sel.transform(|r| {
        if !extend && !r.is_empty() && unit == Unit::Char {
            return Range::point(if forward { r.to() } else { r.from() });
        }
        let head = match unit {
            Unit::Char => {
                if forward {
                    (r.head + 1).min(rope.len_chars())
                } else {
                    r.head.saturating_sub(1)
                }
            }
            Unit::Word => step_word(rope, r.head, forward),
        };
        r.put_head(head, extend)
    })
}

/// The next word boundary: whitespace is skipped, then a run of one class (word characters or
/// punctuation). A line end is a stop of its own.
#[must_use]
pub fn step_word(rope: &Rope, pos: usize, forward: bool) -> usize {
    let len = rope.len_chars();
    let mut p = pos.min(len);
    if forward {
        if p >= len {
            return len;
        }
        if class(rope.char(p)) == CharClass::Newline {
            return p + 1;
        }
        while p < len && class(rope.char(p)) == CharClass::Space {
            p += 1;
        }
        if p >= len || class(rope.char(p)) == CharClass::Newline {
            return p;
        }
        let kind = class(rope.char(p));
        while p < len && class(rope.char(p)) == kind {
            p += 1;
        }
        p
    } else {
        if p == 0 {
            return 0;
        }
        if class(rope.char(p - 1)) == CharClass::Newline {
            return p - 1;
        }
        while p > 0 && class(rope.char(p - 1)) == CharClass::Space {
            p -= 1;
        }
        if p == 0 || class(rope.char(p - 1)) == CharClass::Newline {
            return p;
        }
        let kind = class(rope.char(p - 1));
        while p > 0 && class(rope.char(p - 1)) == kind {
            p -= 1;
        }
        p
    }
}

/// Home: to the first non-blank character, or to column 0 if already there.
#[must_use]
pub fn move_line_start(rope: &Rope, sel: &Selection, extend: bool) -> Selection {
    sel.transform(|r| {
        let line = lines::line_of(rope, r.head);
        let start = lines::line_start(rope, line);
        let text = lines::line_text(rope, line);
        let indent = lines::leading_whitespace(&text).chars().count();
        let first = start + indent;
        let head = if r.head == first || indent == text.chars().count() { start } else { first };
        r.put_head(head, extend)
    })
}

/// End: to the end of the line.
#[must_use]
pub fn move_line_end(rope: &Rope, sel: &Selection, extend: bool) -> Selection {
    sel.transform(|r| r.put_head(lines::line_end(rope, lines::line_of(rope, r.head)), extend))
}

#[must_use]
pub fn move_doc_edge(rope: &Rope, sel: &Selection, end: bool, extend: bool) -> Selection {
    let target = if end { rope.len_chars() } else { 0 };
    sel.transform(|r| r.put_head(target, extend))
}

/// Up or down by visual rows, keeping the column the run of moves started at. Past the first or
/// last row the caret goes to the start or end of the document.
pub fn move_vertical(rope: &Rope, wrap: &mut Wrap, sel: &Selection, rows: isize, extend: bool) -> Selection {
    let total = wrap.total_rows();
    sel.transform(|r| {
        let (row, col) = wrap.locate(rope, r.head);
        let goal = r.goal.unwrap_or(col);
        let target = row as isize + rows;
        let head = if target < 0 {
            0
        } else if target as usize >= total {
            rope.len_chars()
        } else {
            wrap.position(rope, target as usize, goal)
        };
        let mut moved = r.put_head(head, extend);
        moved.goal = Some(goal);
        moved
    })
}

#[must_use]
pub fn select_all(rope: &Rope) -> Selection {
    Selection::single(0, rope.len_chars())
}

/// The word (or run of punctuation or space) around `pos`, for double-click.
#[must_use]
pub fn word_at(rope: &Rope, pos: usize) -> Range {
    let len = rope.len_chars();
    if len == 0 {
        return Range::point(0);
    }
    let probe = if pos < len && class(rope.char(pos)) != CharClass::Newline {
        pos
    } else if pos > 0 {
        pos - 1
    } else {
        return Range::point(pos);
    };
    let kind = class(rope.char(probe));
    if kind == CharClass::Newline {
        return Range::point(pos);
    }
    let mut from = probe;
    while from > 0 && class(rope.char(from - 1)) == kind {
        from -= 1;
    }
    let mut to = probe + 1;
    while to < len && class(rope.char(to)) == kind {
        to += 1;
    }
    Range::new(from, to)
}

/// The whole line around `pos`, including its line break, for triple-click.
#[must_use]
pub fn line_range(rope: &Rope, pos: usize) -> Range {
    let line = lines::line_of(rope, pos);
    let start = lines::line_start(rope, line);
    let end = if line + 1 < rope.len_lines() { rope.line_to_char(line + 1) } else { rope.len_chars() };
    Range::new(start, end)
}

/// A rectangle of ranges between two (line, display column) corners: column selection.
/// Short lines get a caret at their end.
#[must_use]
pub fn column_rect(rope: &Rope, tab: usize, anchor: (usize, usize), head: (usize, usize)) -> Selection {
    let last = rope.len_lines().saturating_sub(1);
    let (a_line, h_line) = (anchor.0.min(last), head.0.min(last));
    let (lo, hi) = (a_line.min(h_line), a_line.max(h_line));
    let mut ranges = Vec::with_capacity(hi - lo + 1);
    for line in lo..=hi {
        let text = lines::line_text(rope, line);
        let start = lines::line_start(rope, line);
        let at = |col| start + lines::index_at_col(&text, col, tab);
        ranges.push(Range::new(at(anchor.1), at(head.1)));
    }
    let primary = h_line - lo;
    Selection::new(ranges, primary)
}

// ------------------------------------------------------------------------------------------------
// Editing

/// Replace every range with the text `f` gives it, putting each caret `offset` characters into
/// its replacement.
fn replace_each(
    doc: &mut TextDoc,
    sel: &Selection,
    kind: EditKind,
    now_ms: u64,
    mut f: impl FnMut(&Rope, Range) -> (String, usize),
) -> Edited {
    let rope = doc.rope();
    let mut edits = Vec::with_capacity(sel.len());
    let mut carets = Vec::with_capacity(sel.len());
    for r in sel.ranges() {
        let (text, offset) = f(rope, *r);
        carets.push((r.from(), offset));
        edits.push((r.from(), r.to(), text));
    }
    let tx = Transaction::replace(rope, edits);
    let after = Selection::new(
        carets.into_iter().map(|(from, offset)| Range::point(tx.map(from, Assoc::Before) + offset)).collect(),
        sel.primary_index(),
    );
    let applied = doc.edit(tx, sel.clone(), after.clone(), kind, now_ms);
    Edited { applied, selection: after }
}

/// Type text at every caret, replacing any selection.
pub fn insert_text(doc: &mut TextDoc, sel: &Selection, text: &str, now_ms: u64) -> Edited {
    let kind =
        if text.chars().count() == 1 && !text.contains('\n') { EditKind::Insert } else { EditKind::Other };
    let len = text.chars().count();
    replace_each(doc, sel, kind, now_ms, |_, _| (text.to_owned(), len))
}

/// Enter: a new line indented like the one it leaves; between a bracket pair, the
/// pair opens onto its own lines.
pub fn insert_newline(doc: &mut TextDoc, sel: &Selection, style: IndentStyle, now_ms: u64) -> Edited {
    replace_each(doc, sel, EditKind::Other, now_ms, |rope, r| {
        let line = lines::line_of(rope, r.from());
        let start = lines::line_start(rope, line);
        let text = lines::line_text(rope, line);
        let before_caret: String = text.chars().take(r.from() - start).collect();
        let indent = lines::leading_whitespace(&before_caret).to_owned();
        let prev = (r.from() > start).then(|| rope.char(r.from() - 1));
        let next = (r.to() < rope.len_chars()).then(|| rope.char(r.to()));
        let pair =
            matches!((prev, next), (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')')));
        if pair {
            let inner = format!("\n{indent}{}", style.unit());
            let offset = inner.chars().count();
            (format!("{inner}\n{indent}"), offset)
        } else {
            let text = format!("\n{indent}");
            let offset = text.chars().count();
            (text, offset)
        }
    })
}

/// Tab: indent the touched lines when anything is selected, otherwise insert one
/// indent (to the next tab stop when indenting with spaces).
pub fn insert_tab(doc: &mut TextDoc, sel: &Selection, style: IndentStyle, now_ms: u64) -> Edited {
    if !sel.all_empty() {
        return indent_lines(doc, sel, style, now_ms);
    }
    replace_each(doc, sel, EditKind::Insert, now_ms, |rope, r| {
        if style.tabs {
            return ("\t".into(), 1);
        }
        let line = lines::line_of(rope, r.head);
        let text = lines::line_text(rope, line);
        let col = lines::display_col(&text, r.head - lines::line_start(rope, line), style.width);
        let width = style.width.max(1);
        let n = width - col % width;
        (" ".repeat(n), n)
    })
}

/// Merge overlapping `(from, to)` deletions.
fn merged(mut spans: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    spans.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (from, to) in spans {
        match out.last_mut() {
            Some(last) if from < last.1 => last.1 = last.1.max(to),
            _ => out.push((from, to)),
        }
    }
    out
}

fn delete_spans(
    doc: &mut TextDoc,
    sel: &Selection,
    spans: Vec<(usize, usize)>,
    kind: EditKind,
    now_ms: u64,
) -> Edited {
    let spans: Vec<(usize, usize)> = merged(spans).into_iter().filter(|(f, t)| f < t).collect();
    if spans.is_empty() {
        return Edited { applied: None, selection: sel.clone() };
    }
    let tx =
        Transaction::replace(doc.rope(), spans.into_iter().map(|(f, t)| (f, t, String::new())).collect());
    let after = sel.transform(|r| Range::point(tx.map(r.from(), Assoc::Before)));
    let applied = doc.edit(tx, sel.clone(), after.clone(), kind, now_ms);
    Edited { applied, selection: after }
}

/// Backspace. In leading spaces it removes back to the previous indent stop.
pub fn delete_backward(
    doc: &mut TextDoc,
    sel: &Selection,
    unit: Unit,
    style: IndentStyle,
    now_ms: u64,
) -> Edited {
    let rope = doc.rope();
    let spans = sel
        .ranges()
        .iter()
        .map(|r| {
            if !r.is_empty() {
                return (r.from(), r.to());
            }
            let head = r.head;
            let start = match unit {
                Unit::Word => step_word(rope, head, false),
                Unit::Char => {
                    let line = lines::line_of(rope, head);
                    let line_start = lines::line_start(rope, line);
                    let before: String = rope.slice(line_start..head).to_string();
                    if !style.tabs && !before.is_empty() && before.chars().all(|c| c == ' ') {
                        let col = before.chars().count();
                        let width = style.width.max(1);
                        let target = (col - 1) / width * width;
                        head - (col - target)
                    } else {
                        head.saturating_sub(1)
                    }
                }
            };
            (start, head)
        })
        .collect();
    delete_spans(doc, sel, spans, EditKind::Delete, now_ms)
}

/// Delete.
pub fn delete_forward(doc: &mut TextDoc, sel: &Selection, unit: Unit, now_ms: u64) -> Edited {
    let rope = doc.rope();
    let len = rope.len_chars();
    let spans = sel
        .ranges()
        .iter()
        .map(|r| {
            if !r.is_empty() {
                return (r.from(), r.to());
            }
            let end = match unit {
                Unit::Char => (r.head + 1).min(len),
                Unit::Word => step_word(rope, r.head, true),
            };
            (r.head, end)
        })
        .collect();
    delete_spans(doc, sel, spans, EditKind::Delete, now_ms)
}

/// Every line a selection touches, once each. A selection ending at the start of a line does not
/// touch that line.
#[must_use]
pub fn touched_lines(rope: &Rope, sel: &Selection) -> Vec<usize> {
    let mut out = Vec::new();
    for r in sel.ranges() {
        let first = lines::line_of(rope, r.from());
        let mut last = lines::line_of(rope, r.to());
        if last > first && r.to() == lines::line_start(rope, last) {
            last -= 1;
        }
        out.extend(first..=last);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Indent every touched line by one level. Selections that started at a line start keep
/// covering the new indentation.
pub fn indent_lines(doc: &mut TextDoc, sel: &Selection, style: IndentStyle, now_ms: u64) -> Edited {
    let rope = doc.rope();
    let lines_touched = touched_lines(rope, sel);
    let many = lines_touched.len() > 1;
    let unit = style.unit();
    let edits: Vec<(usize, usize, String)> = lines_touched
        .into_iter()
        .filter(|l| !(many && lines::line_len(rope, *l) == 0))
        .map(|l| {
            let at = lines::line_start(rope, l);
            (at, at, unit.clone())
        })
        .collect();
    let line_starts: Vec<bool> = sel
        .ranges()
        .iter()
        .map(|r| r.from() == lines::line_start(rope, lines::line_of(rope, r.from())))
        .collect();
    let tx = Transaction::replace(rope, edits);
    let mut i = 0;
    let after = sel.transform(|r| {
        let keep_start = line_starts.get(i).copied().unwrap_or(false) && !r.is_empty();
        i += 1;
        let from = tx.map(r.from(), if keep_start { Assoc::Before } else { Assoc::After });
        let to = tx.map(r.to(), Assoc::After).max(from);
        if r.anchor <= r.head { Range::new(from, to) } else { Range::new(to, from) }
    });
    let applied = doc.edit(tx, sel.clone(), after.clone(), EditKind::Other, now_ms);
    Edited { applied, selection: after }
}

/// Remove up to one level of indentation from every touched line.
pub fn outdent_lines(doc: &mut TextDoc, sel: &Selection, style: IndentStyle, now_ms: u64) -> Edited {
    let rope = doc.rope();
    let edits: Vec<(usize, usize, String)> = touched_lines(rope, sel)
        .into_iter()
        .filter_map(|l| {
            let at = lines::line_start(rope, l);
            let text = lines::line_text(rope, l);
            let n = if text.starts_with('\t') {
                1
            } else {
                text.chars().take(style.width.max(1)).take_while(|c| *c == ' ').count()
            };
            (n > 0).then(|| (at, at + n, String::new()))
        })
        .collect();
    let tx = Transaction::replace(rope, edits);
    let after = sel.map(&tx);
    let applied = doc.edit(tx, sel.clone(), after.clone(), EditKind::Other, now_ms);
    Edited { applied, selection: after }
}

// ------------------------------------------------------------------------------------------------
// Clipboard

/// How copied text was taken, which decides how it pastes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipMode {
    Plain,
    /// Whole lines (copied or cut with nothing selected): pasting puts them above the caret's line.
    FullLine,
    /// A column block: pasting puts one row per line.
    Column,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    pub text: String,
    pub mode: ClipMode,
}

fn whole_line_span(rope: &Rope, line: usize) -> (usize, usize) {
    let start = lines::line_start(rope, line);
    if line + 1 < rope.len_lines() {
        (start, rope.line_to_char(line + 1))
    } else if line > 0 {
        // The last line has no break of its own: take the one before it.
        (start - 1, rope.len_chars())
    } else {
        (start, rope.len_chars())
    }
}

/// What Copy puts on the clipboard. `column` says the selection came from column selection.
#[must_use]
pub fn copy(rope: &Rope, sel: &Selection, column: bool) -> Clip {
    if sel.all_empty() {
        let mut lines_seen = Vec::new();
        let mut text = String::new();
        for r in sel.ranges() {
            let line = lines::line_of(rope, r.head);
            if lines_seen.contains(&line) {
                continue;
            }
            lines_seen.push(line);
            text.push_str(&lines::line_text(rope, line));
            text.push('\n');
        }
        return Clip { text, mode: ClipMode::FullLine };
    }
    let parts: Vec<String> = sel
        .ranges()
        .iter()
        .map(|r| {
            if r.is_empty() {
                lines::line_text(rope, lines::line_of(rope, r.head))
            } else {
                rope.slice(r.from()..r.to()).to_string()
            }
        })
        .collect();
    let mode = if column && sel.len() > 1 { ClipMode::Column } else { ClipMode::Plain };
    Clip { text: parts.join("\n"), mode }
}

/// Cut: copy, then delete the selections, and the whole line of each bare caret.
pub fn cut(doc: &mut TextDoc, sel: &Selection, column: bool, now_ms: u64) -> (Clip, Edited) {
    let rope = doc.rope();
    let clip = copy(rope, sel, column);
    let spans = sel
        .ranges()
        .iter()
        .map(|r| {
            if r.is_empty() {
                whole_line_span(rope, lines::line_of(rope, r.head))
            } else {
                (r.from(), r.to())
            }
        })
        .collect();
    let edited = delete_spans(doc, sel, spans, EditKind::Other, now_ms);
    (clip, edited)
}

/// Paste `clip`. `pad` is the character short lines are padded with for a column paste.
pub fn paste(doc: &mut TextDoc, sel: &Selection, clip: &Clip, pad: char, now_ms: u64) -> Edited {
    let rope = doc.rope();
    match clip.mode {
        ClipMode::FullLine if sel.all_empty() => {
            let mut text = clip.text.clone();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            let mut lines_seen = Vec::new();
            let edits: Vec<(usize, usize, String)> = sel
                .ranges()
                .iter()
                .filter_map(|r| {
                    let line = lines::line_of(rope, r.head);
                    if lines_seen.contains(&line) {
                        return None;
                    }
                    lines_seen.push(line);
                    let at = lines::line_start(rope, line);
                    Some((at, at, text.clone()))
                })
                .collect();
            let tx = Transaction::replace(rope, edits);
            let after = sel.transform(|r| Range::point(tx.map(r.head, Assoc::After)));
            let applied = doc.edit(tx, sel.clone(), after.clone(), EditKind::Other, now_ms);
            Edited { applied, selection: after }
        }
        ClipMode::Column if sel.len() == 1 && sel.all_empty() => paste_column(doc, sel, clip, pad, now_ms),
        _ => {
            let rows: Vec<&str> = clip.text.split('\n').collect();
            if sel.len() > 1 && rows.len() == sel.len() {
                let mut i = 0;
                replace_each(doc, sel, EditKind::Other, now_ms, |_, _| {
                    let row = rows[i].to_owned();
                    i += 1;
                    let len = row.chars().count();
                    (row, len)
                })
            } else {
                let len = clip.text.chars().count();
                replace_each(doc, sel, EditKind::Other, now_ms, |_, _| (clip.text.clone(), len))
            }
        }
    }
}

/// A column block pasted at one caret: row `i` goes onto the caret's line `+ i` at the caret's
/// display column, padding short lines and adding lines past the end.
fn paste_column(doc: &mut TextDoc, sel: &Selection, clip: &Clip, pad: char, now_ms: u64) -> Edited {
    let rope = doc.rope();
    let tab = 4;
    let caret = sel.primary().head;
    let first_line = lines::line_of(rope, caret);
    let caret_text = lines::line_text(rope, first_line);
    let col = lines::display_col(&caret_text, caret - lines::line_start(rope, first_line), tab);
    let rows: Vec<&str> = clip.text.split('\n').collect();
    let mut edits = Vec::new();
    let mut carets = Vec::new();
    let mut appended = String::new();
    let last = rope.len_lines();
    for (i, row) in rows.iter().enumerate() {
        let line = first_line + i;
        if line < last {
            let text = lines::line_text(rope, line);
            let width = lines::display_col(&text, text.chars().count(), tab);
            let start = lines::line_start(rope, line);
            if width < col {
                let padding: String = std::iter::repeat_n(pad, col - width).collect();
                let at = lines::line_end(rope, line);
                edits.push((at, at, format!("{padding}{row}")));
                carets.push((at, padding.chars().count() + row.chars().count()));
            } else {
                let at = start + lines::index_at_col(&text, col, tab);
                edits.push((at, at, (*row).to_owned()));
                carets.push((at, row.chars().count()));
            }
        } else {
            let padding: String = std::iter::repeat_n(pad, col).collect();
            appended.push('\n');
            appended.push_str(&padding);
            appended.push_str(row);
        }
    }
    let end = rope.len_chars();
    if !appended.is_empty() {
        edits.push((end, end, appended));
    }
    let tx = Transaction::replace(rope, edits);
    let mut ranges: Vec<Range> =
        carets.into_iter().map(|(at, offset)| Range::point(tx.map(at, Assoc::Before) + offset)).collect();
    if ranges.is_empty() {
        ranges.push(Range::point(tx.map(end, Assoc::After)));
    }
    let after = Selection::new(ranges, 0);
    let applied = doc.edit(tx, sel.clone(), after.clone(), EditKind::Other, now_ms);
    Edited { applied, selection: after }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACES2: IndentStyle = IndentStyle { tabs: false, width: 2 };

    fn doc(text: &str) -> TextDoc {
        TextDoc::new(text)
    }

    #[test]
    fn typing_at_several_carets_is_one_undo_step() {
        let mut d = doc("ab\ncd\n");
        let sel = Selection::new(vec![Range::point(1), Range::point(4)], 0);
        let out = insert_text(&mut d, &sel, "X", 0);
        assert_eq!(d.text(), "aXb\ncXd\n");
        assert_eq!(out.selection.ranges(), &[Range::point(2), Range::point(6)]);
        d.undo();
        assert_eq!(d.text(), "ab\ncd\n");
    }

    #[test]
    fn enter_keeps_indentation_and_opens_bracket_pairs() {
        let mut d = doc("  if x {}");
        let out = insert_newline(&mut d, &Selection::point(8), SPACES2, 0);
        assert_eq!(d.text(), "  if x {\n    \n  }");
        assert_eq!(out.selection, Selection::point(13));
    }

    #[test]
    fn backspace_in_leading_spaces_goes_back_one_indent_stop() {
        let mut d = doc("     x");
        let out = delete_backward(&mut d, &Selection::point(5), Unit::Char, SPACES2, 0);
        assert_eq!(d.text(), "    x");
        assert_eq!(out.selection, Selection::point(4));
    }

    #[test]
    fn tab_and_shift_tab_act_on_each_touched_line_once() {
        let mut d = doc("a\nb\nc\n");
        let sel = Selection::new(vec![Range::new(0, 3), Range::point(3)], 0);
        let out = insert_tab(&mut d, &sel, SPACES2, 0);
        assert_eq!(d.text(), "  a\n  b\nc\n");
        assert_eq!(out.selection.ranges()[0].from(), 0, "a selection from column 0 keeps the indent");
        let out = outdent_lines(&mut d, &out.selection, SPACES2, 0);
        assert_eq!(d.text(), "a\nb\nc\n");
        let _ = out;
    }

    #[test]
    fn tab_with_a_caret_reaches_the_next_stop() {
        let mut d = doc("abc");
        insert_tab(&mut d, &Selection::point(3), IndentStyle { tabs: false, width: 4 }, 0);
        assert_eq!(d.text(), "abc ");
    }

    #[test]
    fn cut_with_no_selection_takes_the_line_and_paste_puts_it_above() {
        let mut d = doc("one\ntwo\nthree");
        let (clip, out) = cut(&mut d, &Selection::point(5), false, 0);
        assert_eq!(clip, Clip { text: "two\n".into(), mode: ClipMode::FullLine });
        assert_eq!(d.text(), "one\nthree");
        let caret = out.selection;
        let out = paste(&mut d, &caret, &clip, ' ', 0);
        assert_eq!(d.text(), "one\ntwo\nthree");
        assert_eq!(lines::line_of(d.rope(), out.selection.primary().head), 2, "the caret stays on its line");
    }

    #[test]
    fn cutting_the_last_line_takes_the_break_before_it() {
        let mut d = doc("one\ntwo");
        cut(&mut d, &Selection::point(6), false, 0);
        assert_eq!(d.text(), "one");
    }

    #[test]
    fn column_selection_copies_and_pastes_as_a_block() {
        let mut d = doc("abcd\nef\nghij\n");
        let sel = column_rect(d.rope(), 4, (0, 1), (2, 3));
        let clip = copy(d.rope(), &sel, true);
        assert_eq!(clip, Clip { text: "bc\nf\nhi".into(), mode: ClipMode::Column });
        // Paste the block at the end of line 0 (column 4): the short line is padded.
        let out = paste(&mut d, &Selection::point(4), &clip, ' ', 0);
        assert_eq!(d.text(), "abcdbc\nef  f\nghijhi\n");
        assert_eq!(out.selection.len(), 3);
    }

    #[test]
    fn a_plain_paste_with_matching_carets_distributes_lines() {
        let mut d = doc("a\nb\n");
        let sel = Selection::new(vec![Range::point(1), Range::point(3)], 0);
        paste(&mut d, &sel, &Clip { text: "1\n2".into(), mode: ClipMode::Plain }, ' ', 0);
        assert_eq!(d.text(), "a1\nb2\n");
    }

    #[test]
    fn word_moves_skip_space_then_one_class() {
        let rope = Rope::from_str("foo.bar  baz\nqux");
        assert_eq!(step_word(&rope, 0, true), 3);
        assert_eq!(step_word(&rope, 3, true), 4);
        assert_eq!(step_word(&rope, 7, true), 12);
        assert_eq!(step_word(&rope, 12, true), 13);
        assert_eq!(step_word(&rope, 13, false), 12);
        assert_eq!(step_word(&rope, 12, false), 9);
        assert_eq!(word_at(&rope, 5), Range::new(4, 7));
    }

    #[test]
    fn home_toggles_between_indent_and_column_zero() {
        let rope = Rope::from_str("   x");
        let sel = move_line_start(&rope, &Selection::point(4), false);
        assert_eq!(sel, Selection::point(3));
        assert_eq!(move_line_start(&rope, &sel, false), Selection::point(0));
    }

    #[test]
    fn vertical_moves_keep_their_goal_column() {
        let rope = Rope::from_str("abcdef\nab\nabcdef");
        let mut wrap = Wrap::new(&rope, None, 4);
        let down = move_vertical(&rope, &mut wrap, &Selection::point(5), 1, false);
        assert_eq!(down.primary().head, 9, "clamped to the short line");
        let down = move_vertical(&rope, &mut wrap, &down, 1, false);
        assert_eq!(down.primary().head, 15, "back at column 5");
    }
}
