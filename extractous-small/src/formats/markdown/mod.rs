//! Markdown, as Tika's `MarkdownParser` reads it: the text read through an `AutoDetectReader`,
//! parsed as CommonMark with GitHub's tables and strikethrough, and written by its
//! `XhtmlMarkdownVisitor`. The `pulldown-cmark` crate parses the text into the events that
//! visitor's walk of commonmark-java's tree writes; adjacent text is written in one piece, as
//! commonmark-java merges adjacent text nodes.
//!
//! The data URI a link or image leads to, and each one found in HTML that a quote ends, is
//! parsed as an inline document embedded after it ([`data_uri`](crate::formats::data_uri)). The parser reads differently
//! from Tika a document with a data URI of text in a charset this port does not encode, which it
//! encodes in UTF-8, one whose blocks nest deeper than commonmark-java opens them, where it
//! flattens the rest into paragraph text, a table commonmark-java reads leniently and a link
//! whose title is ambiguous to the parser: a build with Tika leaves them to Tika's path.

use std::ops::Range;

use pulldown_cmark::{
    Alignment, CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Options, RefDefs, Tag, TagEnd,
};

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::data_uri;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Difference, Failure, Parser, Prepared};
use crate::java;
use crate::mime::MediaType;

const MEDIA_TYPE: &str = "text/markdown";
/// The block nesting from which commonmark-java's `maxOpenBlockParsers(64)` may flatten blocks,
/// with a margin for the parsers it counts that the events do not show.
const MAX_BLOCK_DEPTH: usize = 56;

/// Tika's `MarkdownParser`.
pub struct MarkdownParser;

impl Parser for MarkdownParser {
    fn name(&self) -> &'static str {
        "extractous::MarkdownParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.markdown.MarkdownParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let content_type = metadata.get("Content-Type");
        let reader = AutoDetectReader::read(input, content_type);
        let (events, markdown_difference) = parse(&reader.text);
        let difference = AutoDetectReader::difference(input, content_type).or(markdown_difference);
        unless_different(
            Box::new(move |xhtml| {
                let metadata = xhtml.metadata_mut();
                auto_detect_reader::set_metadata(&reader.detection, metadata);
                let charset = reader.detection.decode_as.name();
                metadata.set(
                    "Content-Type",
                    MediaType::named(MEDIA_TYPE)
                        .with_parameter("charset", charset)
                        .to_string(),
                );
                metadata.set("Content-Encoding", charset);
                Visitor::new(xhtml).write(events)
            }),
            difference,
        )
    }
}

/// What the visitor writes: an event, or a link or image with its title as commonmark-java holds
/// it, `None` where the source gives none.
enum Item {
    Event(Event<'static>),
    Link {
        href: String,
        title: Option<String>,
    },
    Image {
        source: String,
        title: Option<String>,
    },
}

/// The items of `text`, and the first place where they differ from what commonmark-java and
/// Tika's visitor make of it, if any.
fn parse(text: &str) -> (Vec<Item>, Option<Difference>) {
    let mut difference = lenient_table(text).map(|line| {
        Difference(format!(
            "the delimiter row on line {line} has more columns than its header row, which \
             commonmark-java takes as a table and GitHub's tables do not"
        ))
    });
    let mut iterator = pulldown_cmark::Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    )
    .into_offset_iter();
    let events: Vec<(Event, Range<usize>)> = iterator.by_ref().collect();
    let definitions = iterator.reference_definitions();
    let mut items = Vec::with_capacity(events.len());
    let mut depth = 0usize;
    let mut html_block: Option<String> = None;
    for (event, range) in events {
        let differing = match &event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                destination_uri(dest_url).and_then(|uri| uri.difference())
            }
            Event::InlineHtml(html) => difference_in(html),
            Event::Start(Tag::HtmlBlock) => {
                html_block = Some(String::new());
                None
            }
            Event::Text(text) | Event::Html(text) => {
                if let Some(literal) = &mut html_block {
                    literal.push_str(text);
                }
                None
            }
            Event::End(TagEnd::HtmlBlock) => html_block
                .take()
                .and_then(|literal| difference_in(block_literal(&literal))),
            _ => None,
        };
        if let Some(reason) = differing {
            difference.get_or_insert(Difference(reason));
        }
        match &event {
            Event::Start(tag) if is_block(tag) => {
                depth += 1;
                if depth >= MAX_BLOCK_DEPTH {
                    difference.get_or_insert_with(|| {
                        Difference::new("blocks nested as deep as commonmark-java flattens them")
                    });
                }
            }
            Event::End(end) if is_block_end(end) => depth -= 1,
            _ => {}
        }
        let mut title_of = |link_type, dest_url: &str, title, id: &str| {
            link_title(
                &text[range.clone()],
                link_type,
                dest_url,
                title,
                id,
                definitions,
            )
            .unwrap_or_else(|ambiguous| {
                difference.get_or_insert(ambiguous);
                None
            })
        };
        let item = match event {
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let title = title_of(link_type, &dest_url, title, &id);
                let href = match link_type {
                    LinkType::Email => format!("mailto:{dest_url}"),
                    _ => dest_url.to_string(),
                };
                Item::Link { href, title }
            }
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => Item::Image {
                title: title_of(link_type, &dest_url, title, &id),
                source: dest_url.to_string(),
            },
            event => Item::Event(event.into_static()),
        };
        items.push(item);
    }
    (items, difference)
}

/// The data URI a link or image leads to, as `maybeEmbedDataURI` reads it.
fn destination_uri(destination: &str) -> Option<data_uri::DataUri> {
    destination
        .starts_with("data:")
        .then(|| data_uri::parse(destination))
        .flatten()
}

/// How the native parser reads a data URI in the HTML `html` differently from Tika, if it does.
fn difference_in(html: &str) -> Option<String> {
    data_uri::extract(html)
        .iter()
        .find_map(data_uri::DataUri::difference)
}

/// An HTML block's literal as commonmark-java holds it, without the line break that ends it.
fn block_literal(literal: &str) -> &str {
    literal.strip_suffix('\n').unwrap_or(literal)
}

/// The title commonmark-java gives a link or image whose `source` the parser read as
/// `link_type`: none where the source gives none, which the parser does not tell from an empty
/// one, and the reference definition's for a reference. A destination that ends as an empty
/// title would leaves the parser no telling which commonmark-java reads.
fn link_title(
    source: &str,
    link_type: LinkType,
    destination: &str,
    title: CowStr,
    id: &str,
    definitions: &RefDefs,
) -> Result<Option<String>, Difference> {
    if !title.is_empty() {
        return Ok(Some(title.to_string()));
    }
    match link_type {
        LinkType::Inline => {
            let inside = source.strip_suffix(')').unwrap_or(source);
            let inside = inside.trim_end_matches([' ', '\t', '\n', '\r']);
            let empty = ["\"\"", "''", "()"]
                .into_iter()
                .find(|empty| inside.ends_with(empty));
            match empty {
                None => Ok(None),
                Some(empty) if destination.ends_with(empty) => Err(Difference::new(
                    "a link whose destination ends as an empty title would, which the parser \
                     does not tell apart",
                )),
                Some(_) => Ok(Some(String::new())),
            }
        }
        LinkType::Reference
        | LinkType::ReferenceUnknown
        | LinkType::Collapsed
        | LinkType::CollapsedUnknown
        | LinkType::Shortcut
        | LinkType::ShortcutUnknown => Ok(definitions
            .get(id)
            .and_then(|definition| definition.title.as_ref())
            .map(ToString::to_string)),
        LinkType::Autolink | LinkType::Email | LinkType::WikiLink { .. } => Ok(None),
    }
}

/// The first line, counted from 1, that commonmark-java's `TableBlockParser` takes as the
/// delimiter row of a table whose header row, the line before it, has fewer cells: a table
/// there, where GitHub's tables want as many columns as header cells.
fn lenient_table(text: &str) -> Option<usize> {
    let mut previous: Option<&str> = None;
    for (index, line) in text.lines().enumerate() {
        if let Some(header) = previous.filter(|header| header.contains('|')) {
            if let Some(columns) = separator_columns(line.trim_start_matches([' ', '\t'])) {
                if columns > row_cells(header) {
                    return Some(index + 1);
                }
            }
        }
        previous = Some(line);
    }
    None
}

/// The columns of a delimiter row, as `TableBlockParser.parseSeparator` counts them, if it is
/// one.
fn separator_columns(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let (mut columns, mut pipes, mut valid, mut at) = (0usize, 0, false, 0);
    while at < bytes.len() {
        match bytes[at] {
            b'|' => {
                at += 1;
                pipes += 1;
                if pipes > 1 {
                    return None;
                }
                valid = true;
            }
            b'-' | b':' => {
                if pipes == 0 && columns > 0 {
                    return None;
                }
                if bytes[at] == b':' {
                    at += 1;
                }
                let dashes = bytes[at..].iter().take_while(|&&byte| byte == b'-').count();
                if dashes == 0 {
                    return None;
                }
                at += dashes;
                if bytes.get(at) == Some(&b':') {
                    at += 1;
                }
                columns += 1;
                pipes = 0;
            }
            b' ' | b'\t' => at += 1,
            _ => return None,
        }
    }
    (valid && columns > 0).then_some(columns)
}

/// The cells `TableBlockParser.split` splits a row into: between its pipes, a leading and a
/// trailing one aside, an escaped pipe not splitting, and the last cell only when it is not
/// empty.
fn row_cells(row: &str) -> usize {
    let row = row.trim_start_matches([' ', '\t']);
    let row = match row.strip_prefix('|') {
        Some(rest) => rest.trim_end_matches([' ', '\t']),
        None => row,
    };
    let mut cells = 0;
    let mut current = 0;
    let mut characters = row.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters.peek() == Some(&'|') => {
                characters.next();
                current += 1;
            }
            '|' => {
                cells += 1;
                current = 0;
            }
            _ => current += 1,
        }
    }
    cells + usize::from(current > 0)
}

fn is_block(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::List(_)
            | Tag::Item
            | Tag::Table(_)
    )
}

fn is_block_end(end: &TagEnd) -> bool {
    matches!(
        end,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::Table
    )
}

/// `XhtmlMarkdownVisitor`, over the events of the tree it walks.
struct Visitor<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    /// Text not yet written, which the next text joins.
    text: String,
    /// What the open code or HTML block holds so far, and the code block's info string.
    block: Option<(String, Option<String>)>,
    /// The alt text of the open images, the outermost first, with its source and title.
    images: Vec<(String, String, Option<String>)>,
    /// The alignments of the open table's columns, and whether its body has started.
    table: Option<(Vec<Alignment>, bool)>,
    /// The destinations of the open links, the outermost first.
    links: Vec<String>,
    in_head: bool,
    column: usize,
}

impl<'x, 'h> Visitor<'x, 'h> {
    fn new(xhtml: &'x mut Xhtml<'h>) -> Self {
        Self {
            xhtml,
            text: String::new(),
            block: None,
            images: Vec::new(),
            table: None,
            links: Vec::new(),
            in_head: false,
            column: 0,
        }
    }

    fn write(mut self, items: Vec<Item>) -> Result<(), Failure> {
        for item in items {
            let event = match item {
                Item::Event(event) => event,
                Item::Link { href, title } => {
                    if self.images.is_empty() {
                        self.flush()?;
                        let mut attributes = vec![("href", href.as_str())];
                        if let Some(title) = &title {
                            attributes.push(("title", title.as_str()));
                        }
                        self.xhtml.start_element_with("a", &attributes)?;
                        self.links.push(href);
                    }
                    continue;
                }
                Item::Image { source, title } => {
                    self.flush()?;
                    self.images.push((String::new(), source, title));
                    continue;
                }
            };
            if let Some((alt, _, _)) = self.images.last_mut() {
                match &event {
                    Event::Text(text) | Event::Code(text) | Event::InlineHtml(text) => {
                        alt.push_str(text);
                    }
                    Event::SoftBreak | Event::HardBreak => alt.push(' '),
                    Event::End(TagEnd::Image) => self.end_image()?,
                    _ => {}
                }
                continue;
            }
            if let Some((literal, _)) = &mut self.block {
                if let Event::Text(text) | Event::Html(text) = &event {
                    literal.push_str(text);
                    continue;
                }
            }
            if let Event::Text(text) = &event {
                self.text.push_str(text);
                continue;
            }
            self.flush()?;
            self.event(event)?;
        }
        self.flush()
    }

    /// The end of an image: its text added to the image around it, or the image written.
    fn end_image(&mut self) -> Result<(), Failure> {
        let (alt, source, title) = self.images.pop().expect("an image is open");
        if let Some((outer, _, _)) = self.images.last_mut() {
            outer.push_str(&alt);
            return Ok(());
        }
        let mut attributes = vec![("src", source.as_str())];
        if !alt.is_empty() {
            attributes.push(("alt", alt.as_str()));
        }
        if let Some(title) = &title {
            attributes.push(("title", title.as_str()));
        }
        self.xhtml.start_element_with("img", &attributes)?;
        self.xhtml.end_element("img")?;
        self.embed_destination(&source)
    }

    /// `maybeEmbedDataURI`: the document of the data URI a link or image leads to.
    fn embed_destination(&mut self, destination: &str) -> Result<(), Failure> {
        match destination_uri(destination) {
            Some(uri) => self.embed(&uri),
            None => Ok(()),
        }
    }

    /// `extractDataURIs`: the documents of the data URIs in HTML.
    fn embed_all(&mut self, html: &str) -> Result<(), Failure> {
        for uri in data_uri::extract(html) {
            self.embed(&uri)?;
        }
        Ok(())
    }

    /// `embed`: the document a data URI holds, inline, of its media type, if it has one.
    fn embed(&mut self, uri: &data_uri::DataUri) -> Result<(), Failure> {
        uri.embed(self.xhtml, MarkdownParser.tika_name())
    }

    fn flush(&mut self) -> Result<(), Failure> {
        if !self.text.is_empty() {
            self.xhtml.characters(&self.text)?;
            self.text.clear();
        }
        Ok(())
    }

    fn event(&mut self, event: Event<'static>) -> Result<(), Failure> {
        match event {
            Event::Start(tag) => self.start(tag)?,
            Event::End(end) => self.end(end)?,
            Event::Code(code) => {
                self.xhtml.start_element("code")?;
                self.xhtml.characters(&code)?;
                self.xhtml.end_element("code")?;
            }
            Event::InlineHtml(html) => {
                self.xhtml.characters(&html)?;
                self.embed_all(&html)?;
            }
            Event::Html(html) => self.xhtml.characters(&html)?,
            Event::SoftBreak => self.xhtml.characters("\n")?,
            Event::HardBreak => self.empty("br")?,
            Event::Rule => self.empty("hr")?,
            Event::Text(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::FootnoteReference(_)
            | Event::TaskListMarker(_) => {}
        }
        Ok(())
    }

    fn empty(&mut self, name: &str) -> Result<(), Failure> {
        self.xhtml.start_element(name)?;
        self.xhtml.end_element(name)?;
        Ok(())
    }

    fn start(&mut self, tag: Tag<'static>) -> Result<(), Failure> {
        match tag {
            Tag::Paragraph => self.xhtml.start_element("p")?,
            Tag::Heading { level, .. } => self.xhtml.start_element(heading(level))?,
            Tag::BlockQuote(_) => self.xhtml.start_element("blockquote")?,
            Tag::CodeBlock(kind) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => Some(info.to_string()),
                    CodeBlockKind::Indented => None,
                };
                self.block = Some((String::new(), info));
            }
            Tag::HtmlBlock => self.block = Some((String::new(), None)),
            Tag::List(Some(start)) if start != 1 => {
                let start = start.to_string();
                self.xhtml.start_element_with("ol", &[("start", &start)])?;
            }
            Tag::List(Some(_)) => self.xhtml.start_element("ol")?,
            Tag::List(None) => self.xhtml.start_element("ul")?,
            Tag::Item => self.xhtml.start_element("li")?,
            Tag::Table(alignments) => {
                self.table = Some((alignments, false));
                self.xhtml.start_element("table")?;
            }
            Tag::TableHead => {
                self.in_head = true;
                self.column = 0;
                self.xhtml.start_element("thead")?;
                self.xhtml.start_element("tr")?;
            }
            Tag::TableRow => {
                if let Some((_, body @ false)) = &mut self.table {
                    *body = true;
                    self.xhtml.start_element("tbody")?;
                }
                self.column = 0;
                self.xhtml.start_element("tr")?;
            }
            Tag::TableCell => {
                let name = if self.in_head { "th" } else { "td" };
                let align = self
                    .table
                    .as_ref()
                    .and_then(|(alignments, _)| alignments.get(self.column))
                    .and_then(|alignment| match alignment {
                        Alignment::None => None,
                        Alignment::Left => Some("left"),
                        Alignment::Center => Some("center"),
                        Alignment::Right => Some("right"),
                    });
                match align {
                    Some(align) => self.xhtml.start_element_with(name, &[("align", align)])?,
                    None => self.xhtml.start_element(name)?,
                }
            }
            Tag::Emphasis => self.xhtml.start_element("em")?,
            Tag::Strong => self.xhtml.start_element("strong")?,
            Tag::Strikethrough => self.xhtml.start_element("del")?,
            Tag::Link { .. } | Tag::Image { .. } => {}
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
        Ok(())
    }

    fn end(&mut self, end: TagEnd) -> Result<(), Failure> {
        match end {
            TagEnd::Paragraph => self.xhtml.end_element("p")?,
            TagEnd::Heading(level) => self.xhtml.end_element(heading(level))?,
            TagEnd::BlockQuote(_) => self.xhtml.end_element("blockquote")?,
            TagEnd::CodeBlock => {
                let (literal, info) = self.block.take().expect("a code block is open");
                self.code_block(info.as_deref(), &literal)?;
            }
            TagEnd::HtmlBlock => {
                let (literal, _) = self.block.take().expect("an HTML block is open");
                let literal = block_literal(&literal);
                self.xhtml.start_element("p")?;
                self.xhtml.characters(literal)?;
                self.xhtml.end_element("p")?;
                self.embed_all(literal)?;
            }
            TagEnd::List(true) => self.xhtml.end_element("ol")?,
            TagEnd::List(false) => self.xhtml.end_element("ul")?,
            TagEnd::Item => self.xhtml.end_element("li")?,
            TagEnd::Table => {
                if let Some((_, true)) = self.table.take() {
                    self.xhtml.end_element("tbody")?;
                }
                self.xhtml.end_element("table")?;
            }
            TagEnd::TableHead => {
                self.in_head = false;
                self.xhtml.end_element("tr")?;
                self.xhtml.end_element("thead")?;
            }
            TagEnd::TableRow => self.xhtml.end_element("tr")?,
            TagEnd::TableCell => {
                self.xhtml
                    .end_element(if self.in_head { "th" } else { "td" })?;
                self.column += 1;
            }
            TagEnd::Emphasis => self.xhtml.end_element("em")?,
            TagEnd::Strong => self.xhtml.end_element("strong")?,
            TagEnd::Strikethrough => self.xhtml.end_element("del")?,
            TagEnd::Link => {
                self.xhtml.end_element("a")?;
                let destination = self.links.pop().expect("a link is open");
                self.embed_destination(&destination)?;
            }
            TagEnd::Image => {}
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
        Ok(())
    }

    /// `emitCodeBlock`: the code in a `pre`, its language and any further fence info as
    /// attributes.
    fn code_block(&mut self, info: Option<&str>, literal: &str) -> Result<(), Failure> {
        let trimmed = info.map(java::trim).filter(|info| !info.is_empty());
        let language = trimmed.map(|info| info.split_once(' ').map_or(info, |(first, _)| first));
        let class = language.map(|language| format!("language-{language}"));
        let mut attributes = Vec::new();
        if let Some(class) = &class {
            attributes.push(("class", class.as_str()));
        }
        if let Some(info) = trimmed.filter(|info| Some(*info) != language) {
            attributes.push(("data-info", info));
        }
        self.xhtml.start_element("pre")?;
        self.xhtml.start_element_with("code", &attributes)?;
        self.xhtml.characters(literal)?;
        self.xhtml.end_element("code")?;
        self.xhtml.end_element("pre")?;
        Ok(())
    }
}

fn heading(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    }
}

#[cfg(test)]
mod tests;
