//! Markdown for previews: CommonMark with the GitHub extensions, parsed into a
//! small document model that the preview draws. HTML is sanitised on the way in: a few
//! inline tags render, everything else is dropped, and a script's or style's content never reaches
//! the model at all.

use pulldown_cmark::{CodeBlockKind, Event, MetadataBlockKind, Options, Parser, Tag, TagEnd};

/// How a run of text is set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    pub kbd: bool,
    pub sup: bool,
    pub sub: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text {
        text: String,
        style: Style,
        link: Option<String>,
    },
    /// Drawn when it can be (see the preview); otherwise its alternative text stands in.
    Image {
        alt: String,
        src: String,
        /// The document's own title for it, if any.
        title: String,
        link: Option<String>,
    },
    Break,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// A task list item's checkbox, read-only.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        anchor: String,
    },
    Paragraph(Vec<Inline>),
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    Code {
        language: String,
        text: String,
    },
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    /// YAML front matter as key/value pairs.
    FrontMatter(Vec<(String, String)>),
}

/// A parsed Markdown document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// The source line (0-based) each top-level block starts on, for scroll sync.
    pub lines: Vec<usize>,
}

impl Document {
    /// Its text without markup, for Copy.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            block_text(block, &mut out);
        }
        out.trim_end().to_owned()
    }
}

fn inline_text(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => out.push_str(text),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::Break => out.push('\n'),
        }
    }
}

fn block_text(block: &Block, out: &mut String) {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
            inline_text(inlines, out);
            out.push_str("\n\n");
        }
        Block::List { items, .. } => {
            for item in items {
                for b in &item.blocks {
                    block_text(b, out);
                }
            }
        }
        Block::Quote(blocks) => blocks.iter().for_each(|b| block_text(b, out)),
        Block::Code { text, .. } => {
            out.push_str(text);
            out.push('\n');
        }
        Block::Table { head, rows } => {
            for row in std::iter::once(head).chain(rows) {
                let cells: Vec<String> = row
                    .iter()
                    .map(|c| {
                        let mut s = String::new();
                        inline_text(c, &mut s);
                        s
                    })
                    .collect();
                out.push_str(&cells.join("\t"));
                out.push('\n');
            }
            out.push('\n');
        }
        Block::Rule => out.push('\n'),
        Block::FrontMatter(pairs) => {
            for (k, v) in pairs {
                out.push_str(&format!("{k}: {v}\n"));
            }
            out.push('\n');
        }
    }
}

/// A heading's anchor, as GitHub makes them: lower case, spaces to hyphens, punctuation dropped.
#[must_use]
pub fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

enum Frame {
    Blocks(Vec<Block>),
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<Item> },
    Item { task: Option<bool>, blocks: Vec<Block> },
    Table { head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>>, row: Vec<Vec<Inline>> },
}

enum Gather {
    Paragraph,
    Heading(u8),
    Cell,
}

struct Builder {
    frames: Vec<Frame>,
    inline: Option<(Gather, Vec<Inline>)>,
    style: Vec<Style>,
    links: Vec<String>,
    /// An image being read: its source, title, and the alternative text so far.
    image: Option<(String, String, String)>,
    code: Option<(String, String)>,
    metadata: Option<String>,
    /// Inside `<script>` or `<style>`: everything until this closing tag is dropped.
    skip_until: Option<&'static str>,
    anchors: std::collections::HashMap<String, usize>,
    /// Where the current top-level construct, and the current paragraph or heading, start (byte
    /// offsets), and where each top-level block pushed so far started.
    top_start: usize,
    inline_start: usize,
    starts: Vec<usize>,
}

impl Builder {
    /// Add a block where blocks go now; a top-level one records where it started.
    fn push_block(&mut self, block: Block, start: usize) {
        let top = self.frames.len() == 1 && matches!(self.frames[0], Frame::Blocks(_));
        self.blocks().push(block);
        if top {
            self.starts.push(start);
        }
    }

    fn style(&self) -> Style {
        self.style.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, change: impl FnOnce(&mut Style)) {
        let mut s = self.style();
        change(&mut s);
        self.style.push(s);
    }

    fn blocks(&mut self) -> &mut Vec<Block> {
        if !matches!(self.frames.last(), Some(Frame::Blocks(_) | Frame::Quote(_) | Frame::Item { .. })) {
            // Only malformed input puts a block straight inside a list or table: it gets a frame of
            // its own, folded back in at the end.
            self.frames.push(Frame::Blocks(Vec::new()));
        }
        match self.frames.last_mut() {
            Some(Frame::Blocks(b) | Frame::Quote(b) | Frame::Item { blocks: b, .. }) => b,
            _ => unreachable!("a block frame was just ensured"),
        }
    }

    fn text(&mut self, text: &str) {
        if self.skip_until.is_some() {
            return;
        }
        if let Some((_, _, alt)) = &mut self.image {
            alt.push_str(text);
            return;
        }
        let style = self.style();
        let link = self.links.last().cloned();
        let (_, inlines) = self.inline.get_or_insert_with(|| (Gather::Paragraph, Vec::new()));
        if let Some(Inline::Text { text: last, style: s, link: l }) = inlines.last_mut()
            && *s == style
            && *l == link
        {
            last.push_str(text);
            return;
        }
        inlines.push(Inline::Text { text: text.to_owned(), style, link });
    }

    fn push_inline(&mut self, inline: Inline) {
        let (_, inlines) = self.inline.get_or_insert_with(|| (Gather::Paragraph, Vec::new()));
        inlines.push(inline);
    }

    /// Close an open paragraph (a tight list item's text has no paragraph of its own).
    fn flush(&mut self) {
        let Some((gather, inlines)) = self.inline.take() else { return };
        match gather {
            Gather::Paragraph => {
                if inlines.iter().any(|i| !matches!(i, Inline::Text { text, .. } if text.trim().is_empty())) {
                    self.push_block(Block::Paragraph(inlines), self.inline_start);
                }
            }
            Gather::Heading(level) => {
                let mut plain = String::new();
                inline_text(&inlines, &mut plain);
                let base = slug(&plain);
                let n = self.anchors.entry(base.clone()).or_insert(0);
                let anchor = if *n == 0 { base.clone() } else { format!("{base}-{n}") };
                *n += 1;
                self.push_block(Block::Heading { level, inlines, anchor }, self.inline_start);
            }
            Gather::Cell => {
                if let Some(Frame::Table { row, .. }) = self.frames.last_mut() {
                    row.push(inlines);
                }
            }
        }
    }

    /// Sanitise raw HTML: keep `kbd`, `sub`, `sup`, `br`, `summary` and `img`; drop every
    /// other tag, and the whole content of `script` and `style`.
    fn html(&mut self, html: &str) {
        let mut rest = html;
        while !rest.is_empty() {
            if let Some(end_tag) = self.skip_until {
                match rest.to_ascii_lowercase().find(end_tag) {
                    Some(at) => {
                        rest = &rest[at + end_tag.len()..];
                        self.skip_until = None;
                    }
                    None => return,
                }
                continue;
            }
            let Some(open) = rest.find('<') else {
                self.text(&decode_entities(rest));
                return;
            };
            if open > 0 {
                self.text(&decode_entities(&rest[..open]));
            }
            let Some(close) = rest[open..].find('>').map(|c| c + open) else {
                // A lone `<` is text.
                self.text(&decode_entities(&rest[open..]));
                return;
            };
            let tag = &rest[open + 1..close];
            rest = &rest[close + 1..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            match (name.as_str(), closing) {
                ("script", false) => self.skip_until = Some("</script"),
                ("style", false) => self.skip_until = Some("</style"),
                ("br", _) => self.push_inline(Inline::Break),
                ("kbd", false) => self.push_style(|s| s.kbd = true),
                ("sub", false) => self.push_style(|s| s.sub = true),
                ("sup", false) => self.push_style(|s| s.sup = true),
                ("summary", false) => self.push_style(|s| s.strong = true),
                ("kbd" | "sub" | "sup", true) => {
                    self.style.pop();
                }
                ("summary", true) => {
                    self.style.pop();
                    self.push_inline(Inline::Break);
                }
                ("img", false) => {
                    let alt = attribute(tag, "alt").unwrap_or_default();
                    let src = attribute(tag, "src").unwrap_or_default();
                    let title = attribute(tag, "title").unwrap_or_default();
                    let link = self.links.last().cloned();
                    self.push_inline(Inline::Image { alt, src, title, link });
                }
                _ => {}
            }
        }
    }

    fn metadata(&mut self, source: String) {
        match front_matter(&source) {
            Some(pairs) => self.push_block(Block::FrontMatter(pairs), self.top_start),
            // Not simple YAML: its source as a code block, never a notice.
            None => self.push_block(Block::Code { language: "yaml".into(), text: source }, self.top_start),
        }
    }
}

/// A quoted or bare attribute value in a tag's source.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name).map(|a| a + from) {
        let before_ok = at > 0 && lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = lower[at + name.len()..].trim_start();
        if before_ok && after.starts_with('=') {
            let value_start = tag.len() - after.len() + 1;
            let value = tag[value_start..].trim_start();
            let text = match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
                _ => value.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or(""),
            };
            return Some(decode_entities(text));
        }
        from = at + name.len();
    }
    None
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';').filter(|s| *s <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded.filter(|c| !c.is_control()) {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Simple YAML front matter as key/value pairs; nested values keep their YAML source. `None` when
/// it is not of that shape.
fn front_matter(source: &str) -> Option<Vec<(String, String)>> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in source.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t', '-']) {
            let (_, value) = pairs.last_mut()?;
            if !value.is_empty() {
                value.push('\n');
            }
            value.push_str(line);
            continue;
        }
        let (key, value) = line.split_once(':')?;
        if key.trim().is_empty() {
            return None;
        }
        pairs.push((key.trim().to_owned(), value.trim().trim_matches(['"', '\'']).to_owned()));
    }
    Some(pairs)
}

/// Parse Markdown into the preview's model.
#[must_use]
pub fn parse(text: &str) -> Document {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_GFM;
    let mut b = Builder {
        frames: vec![Frame::Blocks(Vec::new())],
        inline: None,
        style: Vec::new(),
        links: Vec::new(),
        image: None,
        code: None,
        metadata: None,
        skip_until: None,
        anchors: std::collections::HashMap::new(),
        top_start: 0,
        inline_start: 0,
        starts: Vec::new(),
    };
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        if depth == 0 && matches!(event, Event::Start(_) | Event::Rule | Event::Html(_)) {
            b.top_start = range.start;
        }
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    b.flush();
                    b.inline_start = range.start;
                    b.inline = Some((Gather::Paragraph, Vec::new()));
                }
                Tag::Heading { level, .. } => {
                    b.flush();
                    b.inline_start = range.start;
                    b.inline = Some((Gather::Heading(level as u8), Vec::new()));
                }
                Tag::BlockQuote(_) => {
                    b.flush();
                    b.frames.push(Frame::Quote(Vec::new()));
                }
                Tag::CodeBlock(kind) => {
                    b.flush();
                    let language = match kind {
                        CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").to_owned()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    b.code = Some((language, String::new()));
                }
                Tag::List(start) => {
                    b.flush();
                    b.frames.push(Frame::List { start, items: Vec::new() });
                }
                Tag::Item => {
                    b.flush();
                    b.frames.push(Frame::Item { task: None, blocks: Vec::new() });
                }
                Tag::Table(_) => {
                    b.flush();
                    b.frames.push(Frame::Table { head: Vec::new(), rows: Vec::new(), row: Vec::new() });
                }
                Tag::TableCell => b.inline = Some((Gather::Cell, Vec::new())),
                Tag::Emphasis => b.push_style(|s| s.emphasis = true),
                Tag::Strong => b.push_style(|s| s.strong = true),
                Tag::Strikethrough => b.push_style(|s| s.strike = true),
                Tag::Superscript => b.push_style(|s| s.sup = true),
                Tag::Subscript => b.push_style(|s| s.sub = true),
                Tag::Link { dest_url, .. } => b.links.push(dest_url.into_string()),
                Tag::Image { dest_url, title, .. } => {
                    b.image = Some((dest_url.into_string(), title.into_string(), String::new()));
                }
                Tag::MetadataBlock(MetadataBlockKind::YamlStyle) => b.metadata = Some(String::new()),
                Tag::HtmlBlock => b.flush(),
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock => b.flush(),
                TagEnd::BlockQuote(_) => {
                    b.flush();
                    if let Some(Frame::Quote(blocks)) = b.frames.pop() {
                        b.push_block(Block::Quote(blocks), b.top_start);
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some((language, mut text)) = b.code.take() {
                        if text.ends_with('\n') {
                            text.pop();
                        }
                        b.push_block(Block::Code { language, text }, b.top_start);
                    }
                }
                TagEnd::List(_) => {
                    b.flush();
                    if let Some(Frame::List { start, items }) = b.frames.pop() {
                        b.push_block(Block::List { start, items }, b.top_start);
                    }
                }
                TagEnd::Item => {
                    b.flush();
                    if let Some(Frame::Item { task, blocks }) = b.frames.pop()
                        && let Some(Frame::List { items, .. }) = b.frames.last_mut()
                    {
                        items.push(Item { task, blocks });
                    }
                }
                TagEnd::TableCell => b.flush(),
                TagEnd::TableHead => {
                    if let Some(Frame::Table { head, row, .. }) = b.frames.last_mut() {
                        *head = std::mem::take(row);
                    }
                }
                TagEnd::TableRow => {
                    if let Some(Frame::Table { rows, row, .. }) = b.frames.last_mut() {
                        rows.push(std::mem::take(row));
                    }
                }
                TagEnd::Table => {
                    if let Some(Frame::Table { head, rows, .. }) = b.frames.pop() {
                        b.push_block(Block::Table { head, rows }, b.top_start);
                    }
                }
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript => {
                    b.style.pop();
                }
                TagEnd::Link => {
                    b.links.pop();
                }
                TagEnd::Image => {
                    if let Some((src, title, alt)) = b.image.take() {
                        let link = b.links.last().cloned();
                        b.push_inline(Inline::Image { alt, src, title, link });
                    }
                }
                TagEnd::MetadataBlock(_) => {
                    if let Some(source) = b.metadata.take() {
                        b.metadata(source);
                    }
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some((_, code)) = &mut b.code {
                    code.push_str(&text);
                } else if let Some(meta) = &mut b.metadata {
                    meta.push_str(&text);
                } else {
                    b.text(&text);
                }
            }
            Event::Code(text) => {
                b.push_style(|s| s.code = true);
                b.text(&text);
                b.style.pop();
            }
            Event::InlineMath(text) => b.text(&format!("${text}$")),
            Event::DisplayMath(text) => b.text(&format!("$${text}$$")),
            Event::Html(html) | Event::InlineHtml(html) => b.html(&html),
            Event::SoftBreak => b.text(" "),
            Event::HardBreak => b.push_inline(Inline::Break),
            Event::Rule => {
                b.flush();
                b.push_block(Block::Rule, range.start);
            }
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item { task, .. }) = b.frames.last_mut() {
                    *task = Some(done);
                }
            }
            Event::FootnoteReference(name) => b.text(&format!("[{name}]")),
        }
    }
    b.flush();
    // Collapse whatever frames a malformed document left open.
    while b.frames.len() > 1 {
        let frame = b.frames.pop().expect("more than one frame");
        let blocks = match frame {
            Frame::Blocks(bl) | Frame::Quote(bl) | Frame::Item { blocks: bl, .. } => bl,
            Frame::List { start, items } => vec![Block::List { start, items }],
            Frame::Table { head, rows, .. } => vec![Block::Table { head, rows }],
        };
        b.blocks().extend(blocks);
    }
    let starts = std::mem::take(&mut b.starts);
    match b.frames.pop() {
        Some(Frame::Blocks(mut blocks)) => {
            autolink_blocks(&mut blocks);
            // Blocks folded in from a malformed document's open frames start where the last did.
            let last = starts.last().copied().unwrap_or(0);
            let line_starts: Vec<usize> =
                std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
            let lines = (0..blocks.len())
                .map(|i| {
                    let at = starts.get(i).copied().unwrap_or(last);
                    line_starts.partition_point(|&s| s <= at).saturating_sub(1)
                })
                .collect();
            Document { blocks, lines }
        }
        _ => Document::default(),
    }
}

/// GitHub's bare-address autolinks: a web or mail address in plain text becomes a
/// link, found with the same grammar editors and terminals use.
fn autolink_blocks(blocks: &mut [Block]) {
    for block in blocks {
        match block {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => autolink(inlines),
            Block::List { items, .. } => items.iter_mut().for_each(|i| autolink_blocks(&mut i.blocks)),
            Block::Quote(blocks) => autolink_blocks(blocks),
            Block::Table { head, rows } => {
                head.iter_mut().chain(rows.iter_mut().flatten()).for_each(autolink);
            }
            Block::Code { .. } | Block::Rule | Block::FrontMatter(_) => {}
        }
    }
}

fn autolink(inlines: &mut Vec<Inline>) {
    let mut out = Vec::with_capacity(inlines.len());
    for inline in inlines.drain(..) {
        let Inline::Text { text, style, link: None } = &inline else {
            out.push(inline);
            continue;
        };
        if style.code {
            out.push(inline);
            continue;
        }
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0;
        for found in throng_core::links::find(text) {
            let address = match found.target {
                throng_core::links::Target::Web(url) | throng_core::links::Target::Scheme(url) => url,
                throng_core::links::Target::File { .. } => continue,
            };
            if found.start > at {
                out.push(Inline::Text {
                    text: chars[at..found.start].iter().collect(),
                    style: *style,
                    link: None,
                });
            }
            let shown: String = chars[found.start..found.end].iter().collect();
            out.push(Inline::Text { text: shown, style: *style, link: Some(address) });
            at = found.end;
        }
        if at < chars.len() {
            out.push(Inline::Text { text: chars[at..].iter().collect(), style: *style, link: None });
        }
    }
    *inlines = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> Inline {
        Inline::Text { text: t.into(), style: Style::default(), link: None }
    }

    #[test]
    fn headings_paragraphs_and_inline_styles() {
        let doc = parse("# Title\n\nSome **bold** and `code`.\n\n## Title\n");
        assert_eq!(
            doc.blocks[0],
            Block::Heading { level: 1, inlines: vec![text("Title")], anchor: "title".into() }
        );
        let Block::Paragraph(p) = &doc.blocks[1] else { panic!("{:?}", doc.blocks[1]) };
        assert_eq!(
            p[1],
            Inline::Text {
                text: "bold".into(),
                style: Style { strong: true, ..Style::default() },
                link: None
            }
        );
        assert_eq!(
            p[3],
            Inline::Text { text: "code".into(), style: Style { code: true, ..Style::default() }, link: None }
        );
        assert!(
            matches!(&doc.blocks[2], Block::Heading { anchor, .. } if anchor == "title-1"),
            "anchors are unique"
        );
    }

    #[test]
    fn task_lists_tables_code_and_links() {
        let doc = parse(
            "- [x] done\n- [ ] todo\n\n| a | b |\n|---|---|\n| 1 | [x](src/x.md) |\n\n```rust\nfn main() {}\n```\n",
        );
        let Block::List { items, .. } = &doc.blocks[0] else { panic!() };
        assert_eq!((items[0].task, items[1].task), (Some(true), Some(false)));
        assert_eq!(items[0].blocks, vec![Block::Paragraph(vec![text("done")])]);
        let Block::Table { head, rows } = &doc.blocks[1] else { panic!("{:?}", doc.blocks[1]) };
        assert_eq!(head.len(), 2);
        assert_eq!(
            rows[0][1],
            vec![Inline::Text { text: "x".into(), style: Style::default(), link: Some("src/x.md".into()) }]
        );
        assert_eq!(doc.blocks[2], Block::Code { language: "rust".into(), text: "fn main() {}".into() });
    }

    #[test]
    fn the_sanitiser_is_in_the_path() {
        let doc = parse(
            "Hi <script>alert(1)</script><kbd>Ctrl</kbd><b onclick=\"x()\">b</b><br>\n\n<div><style>p{}</style>ok <img src=\"a.png\" alt=\"pic\" onerror=\"x()\"></div>\n",
        );
        let all = format!("{doc:?}");
        for banned in ["alert", "onclick", "onerror", "p{}", "script", "<"] {
            assert!(!all.contains(banned), "{banned} leaked: {all}");
        }
        let Block::Paragraph(p) = &doc.blocks[0] else { panic!() };
        assert!(p.contains(&Inline::Text {
            text: "Ctrl".into(),
            style: Style { kbd: true, ..Style::default() },
            link: None
        }));
        assert!(p.contains(&Inline::Break));
        assert!(all.contains("Image { alt: \"pic\", src: \"a.png\""));
    }

    #[test]
    fn front_matter_is_a_table_and_bad_front_matter_is_code() {
        let doc = parse("---\ntitle: Plan\ntags:\n  - a\n---\n# Body\n");
        assert_eq!(
            doc.blocks[0],
            Block::FrontMatter(vec![("title".into(), "Plan".into()), ("tags".into(), "  - a".into())])
        );
        let doc = parse("---\njust words\n---\n");
        assert!(matches!(&doc.blocks[0], Block::Code { language, .. } if language == "yaml"));
    }

    #[test]
    fn bare_web_addresses_become_links_but_not_in_code() {
        let doc = parse("Go to https://x.dev/a, or `https://not.linked`.\n");
        let Block::Paragraph(p) = &doc.blocks[0] else { panic!() };
        assert_eq!(
            p[1],
            Inline::Text {
                text: "https://x.dev/a".into(),
                style: Style::default(),
                link: Some("https://x.dev/a".into())
            }
        );
        assert!(
            p.iter().all(
                |i| !matches!(i, Inline::Text { text, link: Some(_), .. } if text.contains("not.linked"))
            )
        );
    }

    #[test]
    fn each_top_level_block_knows_the_line_it_starts_on() {
        let text = "---\ntitle: x\n---\n# Title\n\nA paragraph\nover two lines.\n\n- one\n- two\n\n```\ncode\n```\n\n---\n\n> quote\n";
        let doc = parse(text);
        assert_eq!(doc.blocks.len(), doc.lines.len());
        let kinds: Vec<&str> = doc
            .blocks
            .iter()
            .map(|b| match b {
                Block::FrontMatter(_) => "front",
                Block::Heading { .. } => "heading",
                Block::Paragraph(_) => "para",
                Block::List { .. } => "list",
                Block::Code { .. } => "code",
                Block::Rule => "rule",
                Block::Quote(_) => "quote",
                Block::Table { .. } => "table",
            })
            .collect();
        assert_eq!(kinds, ["front", "heading", "para", "list", "code", "rule", "quote"]);
        assert_eq!(doc.lines, [0, 3, 5, 8, 11, 15, 17]);
    }

    #[test]
    fn slugs_follow_github() {
        assert_eq!(slug("Hello, World! 2"), "hello-world-2");
        assert_eq!(slug("  Über_cool  "), "über_cool");
    }
}
