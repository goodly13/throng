//! Syntax highlighting whose cost follows what is on screen, not the file's size.
//!
//! syntect's parser is a state machine run line by line. The state at the start of every 32nd
//! line is kept, so drawing a line needs at most 31 lines of re-parsing. Checkpoints the view has
//! not reached yet are filled in under a time budget per frame; a line past the budget draws plain
//! and fills in on a later frame. An edit drops only the checkpoints after the line it touched.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use ropey::Rope;
use syntect::highlighting::{
    Color, FontStyle, HighlightState, Highlighter as SyntectHighlighter, RangedHighlightIterator,
    ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Lines longer than this draw plain but stay editable.
pub const LONG_LINE: usize = 10_000;
const STRIDE: usize = 32;
/// Cached lines kept before the cache is cleared.
const CACHE_LINES: usize = 20_000;

/// bat's syntax set: TOML, TypeScript, Dockerfile and many more beyond syntect's own.
#[must_use]
pub fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// An RGBA colour.
pub type Rgba = [u8; 4];

/// How a run of text is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenStyle {
    pub fg: Rgba,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// A styled run of a line: byte offsets within the line's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: TokenStyle,
}

/// The syntax colours a theme gives (the Editor · Syntax tokens).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxColours {
    pub foreground: Rgba,
    pub keyword: Rgba,
    pub string: Rgba,
    pub comment: Rgba,
    pub number: Rgba,
    pub type_: Rgba,
    pub function: Rgba,
    pub variable: Rgba,
    pub operator: Rgba,
    pub punctuation: Rgba,
    pub invalid: Rgba,
}

/// A syntect theme mapping TextMate scopes onto the token colours.
#[must_use]
pub fn build_theme(c: &SyntaxColours) -> Theme {
    let colour = |rgba: Rgba| Color { r: rgba[0], g: rgba[1], b: rgba[2], a: rgba[3] };
    let item = |scopes: &str, rgba: Rgba, font: Option<FontStyle>| ThemeItem {
        scope: ScopeSelectors::from_str(scopes).unwrap_or_default(),
        style: StyleModifier { foreground: Some(colour(rgba)), background: None, font_style: font },
    };
    Theme {
        name: Some("throng".into()),
        author: None,
        settings: ThemeSettings { foreground: Some(colour(c.foreground)), ..ThemeSettings::default() },
        scopes: vec![
            item("comment, punctuation.definition.comment", c.comment, Some(FontStyle::ITALIC)),
            item("string, punctuation.definition.string, constant.character.escape", c.string, None),
            item("constant.numeric, constant.language, constant.character", c.number, None),
            item("keyword, storage.modifier, storage.type.function, keyword.control", c.keyword, None),
            item("keyword.operator, punctuation.separator.key-value", c.operator, None),
            item(
                "storage.type, entity.name.type, support.type, entity.name.class, entity.other.inherited-class",
                c.type_,
                None,
            ),
            item(
                "entity.name.function, support.function, meta.function-call variable.function",
                c.function,
                None,
            ),
            item("variable, variable.parameter, entity.name.tag", c.variable, None),
            item("punctuation", c.punctuation, None),
            item("markup.heading", c.keyword, Some(FontStyle::BOLD)),
            item("markup.bold", c.foreground, Some(FontStyle::BOLD)),
            item("markup.italic", c.foreground, Some(FontStyle::ITALIC)),
            item("markup.underline.link, markup.underline", c.function, Some(FontStyle::UNDERLINE)),
            item("markup.raw, markup.inline.raw", c.string, None),
            item("invalid", c.invalid, None),
        ],
    }
}

type State = (ParseState, HighlightState);

/// Highlighting for one document in one language and theme.
pub struct Highlighter {
    syntax: &'static SyntaxReference,
    theme: Arc<Theme>,
    /// `checkpoints[k]` is the state at the start of line `k * STRIDE`.
    checkpoints: Vec<State>,
    cache: HashMap<usize, Arc<[Span]>>,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter").field("syntax", &self.syntax.name).finish_non_exhaustive()
    }
}

impl Highlighter {
    #[must_use]
    pub fn new(syntax: &'static SyntaxReference, theme: Arc<Theme>) -> Self {
        let mut highlighter = Self { syntax, theme, checkpoints: Vec::new(), cache: HashMap::new() };
        highlighter.reset();
        highlighter
    }

    #[must_use]
    pub fn syntax(&self) -> &'static SyntaxReference {
        self.syntax
    }

    /// Forget everything (a new language or theme, or a whole-text replacement).
    pub fn reset(&mut self) {
        let highlighter = SyntectHighlighter::new(&self.theme);
        let initial = (ParseState::new(self.syntax), HighlightState::new(&highlighter, ScopeStack::new()));
        self.checkpoints = vec![initial];
        self.cache.clear();
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
        self.reset();
    }

    pub fn set_syntax(&mut self, syntax: &'static SyntaxReference) {
        if !std::ptr::eq(self.syntax, syntax) {
            self.syntax = syntax;
            self.reset();
        }
    }

    /// Text changed from `line` on: states after it are no longer known.
    pub fn invalidate_from(&mut self, line: usize) {
        self.checkpoints.truncate(line / STRIDE + 1);
        self.cache.retain(|l, _| *l < line);
    }

    /// Spans for each of `lines`, or `None` for a line not reached before `deadline` (draw it
    /// plain; ask again next frame).
    pub fn spans(
        &mut self,
        rope: &Rope,
        lines: std::ops::Range<usize>,
        deadline: Instant,
    ) -> Vec<Option<Arc<[Span]>>> {
        let total = rope.len_lines();
        let (start, end) = (lines.start.min(total), lines.end.min(total));
        if (start..end).all(|l| self.cache.contains_key(&l)) {
            return (start..end).map(|l| self.cache.get(&l).cloned()).collect();
        }
        if self.cache.len() > CACHE_LINES {
            self.cache.clear();
        }
        let set = syntax_set();
        let theme = Arc::clone(&self.theme);
        let highlighter = SyntectHighlighter::new(&theme);
        let mut buf = String::new();

        // Reach the checkpoint at or before `start`.
        let wanted = start / STRIDE;
        while self.checkpoints.len() <= wanted {
            let k = self.checkpoints.len() - 1;
            let mut state = self.checkpoints[k].clone();
            for line in k * STRIDE..(k + 1) * STRIDE {
                if Instant::now() > deadline {
                    return (start..end).map(|l| self.cache.get(&l).cloned()).collect();
                }
                parse(rope, line, &mut state, set, &highlighter, &mut buf);
            }
            self.checkpoints.push(state);
        }

        let base = wanted * STRIDE;
        let mut state = self.checkpoints[wanted].clone();
        let mut out = Vec::with_capacity(end - start);
        for line in base..end {
            if line % STRIDE == 0 && line / STRIDE == self.checkpoints.len() {
                self.checkpoints.push(state.clone());
            }
            if line >= start {
                if let Some(cached) = self.cache.get(&line) {
                    // The state still has to advance past this line.
                    let cached = cached.clone();
                    parse(rope, line, &mut state, set, &highlighter, &mut buf);
                    out.push(Some(cached));
                    continue;
                }
                if Instant::now() > deadline {
                    out.push(None);
                    continue;
                }
            }
            let spans = parse(rope, line, &mut state, set, &highlighter, &mut buf);
            if line >= start {
                let spans: Arc<[Span]> = spans.into();
                self.cache.insert(line, spans.clone());
                out.push(Some(spans));
            }
        }
        out
    }
}

/// Parse one line, advancing `state`, and return its spans.
fn parse(
    rope: &Rope,
    line: usize,
    state: &mut State,
    set: &SyntaxSet,
    highlighter: &SyntectHighlighter<'_>,
    buf: &mut String,
) -> Vec<Span> {
    if line >= rope.len_lines() {
        return Vec::new();
    }
    let slice = rope.line(line);
    if slice.len_chars() > LONG_LINE {
        return Vec::new();
    }
    buf.clear();
    for chunk in slice.chunks() {
        buf.push_str(chunk);
    }
    if !buf.ends_with('\n') {
        buf.push('\n');
    }
    let Ok(ops) = state.0.parse_line(buf, set) else {
        return Vec::new();
    };
    let text_len = buf.len() - 1;
    RangedHighlightIterator::new(&mut state.1, &ops, buf, highlighter)
        .filter_map(|(style, _, range)| {
            let end = range.end.min(text_len);
            (range.start < end).then(|| Span {
                start: range.start,
                end,
                style: TokenStyle {
                    fg: [style.foreground.r, style.foreground.g, style.foreground.b, style.foreground.a],
                    bold: style.font_style.contains(FontStyle::BOLD),
                    italic: style.font_style.contains(FontStyle::ITALIC),
                    underline: style.font_style.contains(FontStyle::UNDERLINE),
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::lang;

    fn colours() -> SyntaxColours {
        let c = |r| [r, 0, 0, 255];
        SyntaxColours {
            foreground: c(1),
            keyword: c(2),
            string: c(3),
            comment: c(4),
            number: c(5),
            type_: c(6),
            function: c(7),
            variable: c(8),
            operator: c(9),
            punctuation: c(10),
            invalid: c(11),
        }
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn keywords_strings_and_comments_take_their_colours() {
        let rope = Rope::from_str("fn main() { let s = \"x\"; } // done\n");
        let mut h = Highlighter::new(lang::syntax_for_name("main.rs"), Arc::new(build_theme(&colours())));
        let spans = h.spans(&rope, 0..1, later())[0].clone().unwrap();
        let colour_of = |needle: &str| {
            let at = rope.line(0).to_string().find(needle).unwrap();
            spans.iter().find(|s| s.start <= at && at < s.end).unwrap().style.fg[0]
        };
        assert_eq!(colour_of("fn"), 2);
        assert_eq!(colour_of("\"x\""), 3);
        assert_eq!(colour_of("// done"), 4);
    }

    #[test]
    fn a_line_deep_in_a_big_file_needs_no_more_than_the_budget() {
        let text = "let x = 1;\n".repeat(200_000);
        let rope = Rope::from_str(&text);
        let mut h = Highlighter::new(lang::syntax_for_name("a.rs"), Arc::new(build_theme(&colours())));
        // A deadline already past: nothing is parsed, and nothing hangs.
        let out = h.spans(&rope, 150_000..150_010, Instant::now());
        assert!(out.iter().all(Option::is_none));
    }

    #[test]
    fn an_edit_invalidates_from_its_line_and_multiline_state_carries() {
        let mut rope = Rope::from_str("a\n/* open\nstill comment\n");
        let mut h = Highlighter::new(lang::syntax_for_name("a.c"), Arc::new(build_theme(&colours())));
        let first = h.spans(&rope, 2..3, later())[0].clone().unwrap();
        assert_eq!(first[0].style.fg[0], 4, "inside a block comment");
        rope.remove(2..4);
        h.invalidate_from(1);
        let after = h.spans(&rope, 2..3, later())[0].clone().unwrap();
        assert_ne!(after[0].style.fg[0], 4, "the comment opener is gone");
    }
}
