//! Find and replace inside a document: literal text, optionally case-sensitive and whole-word
//! (regular expressions are not offered).

use ropey::Rope;

use crate::change::Transaction;
use crate::lines::{CharClass, class};

/// What to look for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub term: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Query {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.term.is_empty()
    }
}

/// Case folding that never changes a string's length in characters, so offsets found in the folded
/// text are offsets in the original (a lower-cased copy can change length).
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| class(c) == CharClass::Word)
}

/// Every match, as character ranges, in order. An empty term matches nothing. Terms never span a
/// line break (the find input is a single line).
#[must_use]
pub fn find_all(rope: &Rope, query: &Query, limit: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if query.term.is_empty() || query.term.contains('\n') {
        return out;
    }
    let needle: Vec<char> = if query.case_sensitive {
        query.term.chars().collect()
    } else {
        query.term.chars().map(fold).collect()
    };
    let n = needle.len();
    let mut hay: Vec<char> = Vec::new();
    for (index, line) in rope.lines().enumerate() {
        let start = rope.line_to_char(index);
        hay.clear();
        hay.extend(line.chars().filter(|c| *c != '\n'));
        if hay.len() < n {
            continue;
        }
        if !query.case_sensitive {
            for c in &mut hay {
                *c = fold(*c);
            }
        }
        let raw: Vec<char> = if query.whole_word { line.chars().collect() } else { Vec::new() };
        let mut i = 0;
        while i + n <= hay.len() {
            if hay[i] == needle[0] && hay[i..i + n] == needle[..] {
                let ok = !query.whole_word || {
                    let before = i.checked_sub(1).map(|j| raw[j]);
                    let first = Some(raw[i]);
                    let last = Some(raw[i + n - 1]);
                    let after = raw.get(i + n).copied().filter(|c| *c != '\n');
                    !(is_word(before) && is_word(first)) && !(is_word(last) && is_word(after))
                };
                if ok {
                    out.push((start + i, start + i + n));
                    if out.len() >= limit {
                        return out;
                    }
                    i += n;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

/// The match to show first: the first at or after `pos`, wrapping to the first.
#[must_use]
pub fn first_from(matches: &[(usize, usize)], pos: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(matches.iter().position(|m| m.0 >= pos).unwrap_or(0))
}

/// Replace every match: one transaction, so one undo step.
#[must_use]
pub fn replace_all(rope: &Rope, matches: &[(usize, usize)], replacement: &str) -> Transaction {
    Transaction::replace(rope, matches.iter().map(|(f, t)| (*f, *t, replacement.to_owned())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(term: &str, case_sensitive: bool, whole_word: bool) -> Query {
        Query { term: term.into(), case_sensitive, whole_word }
    }

    #[test]
    fn case_and_whole_word_modes() {
        let rope = Rope::from_str("Foo foo food\n_foo foo.");
        assert_eq!(find_all(&rope, &q("foo", false, false), 100).len(), 5);
        assert_eq!(find_all(&rope, &q("foo", true, false), 100).len(), 4);
        assert_eq!(find_all(&rope, &q("foo", false, true), 100), vec![(0, 3), (4, 7), (18, 21)]);
    }

    #[test]
    fn offsets_survive_characters_whose_lower_case_is_longer() {
        // 'İ' lower-cases to two characters; folding keeps it as one.
        let rope = Rope::from_str("İx ix");
        assert_eq!(find_all(&rope, &q("ix", false, false), 100), vec![(3, 5)]);
    }

    #[test]
    fn the_first_match_is_at_or_after_the_caret_and_wraps() {
        let m = [(0, 1), (5, 6), (9, 10)];
        assert_eq!(first_from(&m, 4), Some(1));
        assert_eq!(first_from(&m, 10), Some(0));
    }

    #[test]
    fn a_replacement_containing_the_term_is_not_matched_again() {
        let mut rope = Rope::from_str("a a a");
        let matches = find_all(&rope, &q("a", true, false), 100);
        replace_all(&rope, &matches, "aa").apply(&mut rope);
        assert_eq!(rope.to_string(), "aa aa aa");
    }
}
