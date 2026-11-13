//! Scribus documents (`.sla`), read for their text: the XML Scribus saves, its root
//! `SCRIBUSUTF8NEW` (Scribus 1.3 and later), `SCRIBUSUTF8` (1.2) or `SCRIBUS` (before).
//!
//! The story of each text frame and text on a path is written once, from the first frame of the
//! frames it is threaded through (`NEXTITEM`), and the stories of the frames after it in the
//! thread follow it, as Scribus appends them when it links them. The frames are written in the
//! order of their pages: those of the master pages first, then those of the document's pages,
//! then those on the pasteboard, each page's from top to bottom and left to right, and the
//! frames of a group in that order where the group is. A table is written as a `table` of its
//! rows and cells, each cell's story in it.
//!
//! A story's paragraphs end at its `para` elements, or, in older documents, at the line breaks
//! of its text; a paragraph is a heading where its style is `Heading N` or is based (`PARENT`) on
//! one. Tabs, hard spaces and hyphens are kept, line, column and frame breaks are line breaks,
//! and the hyphenation points Scribus marks with a soft hyphen are left out where the author did
//! not type one. Page numbers, which Scribus computes and does not save, and marks are not
//! written. A frame anchored in the text is written after the paragraph that anchors it; an
//! image Scribus keeps in the document (`isInlineImage`), compressed by Qt's `qCompress`, is
//! handed to the embedded document extractor, named by its frame. Note frames, whose text
//! Scribus saves apart from its frames, are not read.
//!
//! The document's information (the `DOCUMENT` element's `TITLE`, `AUTHOR`, `DOCDATE` and the
//! rest) is its Dublin Core, its `KEYWORDS` its keywords and its `ANZPAGES` its page count; its
//! language is `DOCLANGINFO`, or else the language its text is set in.
//!
//! Scribus before 1.3.4 writes a story's paragraph breaks and tabs as raw control characters,
//! which XML does not allow: such a document that the XML reader rejects is not read, rather
//! than read by a reader of its own that forgives what XML forbids, which would hold a second
//! copy of the document for the few files of those versions that hold more than one paragraph.
//! A gzip-compressed `.sla.gz` document is detected as gzip, whose parser hands the document it
//! holds to this one.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Range;
use std::str::FromStr;

use crate::compress::{zlib, Limited};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{record_stream_exception, ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::input::Input;
use crate::java::commons_base64_decode;

const NAME: &str = "dijo::ScribusParser";
const WHAT: &str = "The Scribus document";
const MEDIA_TYPE: &str = "application/vnd.scribus";

const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
/// The deepest frames are written in one another, through groups and frames anchored in text.
const MAX_NESTING: usize = 64;
/// The first version of Scribus that writes paragraph breaks and tabs as elements.
const ELEMENT_TEXT_VERSION: [u32; 3] = [1, 3, 4];

/// The metadata keys of the `DOCUMENT` element's attributes.
const PROPERTIES: [(&str, &str); 16] = [
    ("TITLE", "dc:title"),
    ("AUTHOR", "dc:creator"),
    ("SUBJECT", "dc:subject"),
    ("COMMENTS", "dc:description"),
    ("KEYWORDS", "meta:keyword"),
    ("PUBLISHER", "dc:publisher"),
    ("DOCCONTRIB", "dc:contributor"),
    ("DOCDATE", "dcterms:created"),
    ("DOCTYPE", "dc:type"),
    ("DOCFORMAT", "dc:format"),
    ("DOCIDENT", "dc:identifier"),
    ("DOCSOURCE", "dc:source"),
    ("DOCLANGINFO", "dc:language"),
    ("DOCRELATION", "dc:relation"),
    ("DOCCOVER", "dc:coverage"),
    ("DOCRIGHTS", "dc:rights"),
];
const LANGUAGE: &str = "dc:language";
const PAGE_COUNT: &str = "xmpTPg:NPages";
const CREATOR_TOOL: &str = "xmp:CreatorTool";

/// The text of Scribus documents.
pub struct ScribusParser;

impl Parser for ScribusParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails a document that is not well-formed XML, and leaves unread one of Scribus before
    /// 1.3.4 that is not.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let document = Document::read(input)?;
        Ok(Box::new(move |xhtml| document.write(xhtml)))
    }
}

/// A paragraph style, by the order its name was first read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StyleId(usize);

/// What of a story's text a paragraph holds, in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// The text of the story in this range.
    Text(Range<usize>),
    LineBreak,
    /// The frame anchored here, by its `InID`.
    Frame(i64),
}

/// A paragraph of a story: its style, and where its pieces end in the story's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Paragraph {
    style: Option<StyleId>,
    end: usize,
}

/// The text of a frame or table cell.
#[derive(Debug, Default)]
struct Story {
    /// The style of the paragraphs that name none.
    default_style: Option<StyleId>,
    text: String,
    pieces: Vec<Piece>,
    paragraphs: Vec<Paragraph>,
    /// The style an older document's `ITEXT` gives the paragraph being read (`PSTYLE`).
    legacy_style: Option<StyleId>,
    /// Whether the last character read is marked as a point to hyphenate at.
    hyphenation_point: bool,
}

impl Story {
    fn new(default_style: Option<StyleId>) -> Self {
        Self {
            default_style,
            ..Self::default()
        }
    }

    fn push_str(&mut self, text: &str) {
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        let paragraph_start = self.paragraph_start();
        match self.pieces[paragraph_start..].last_mut() {
            Some(Piece::Text(range)) => range.end = end,
            Some(_) | None => self.pieces.push(Piece::Text(start..end)),
        }
    }

    fn push_piece(&mut self, piece: Piece) {
        self.hyphenation_point = false;
        self.pieces.push(piece);
    }

    /// Reads `character` as Scribus reads a character of its text, a control character standing
    /// for what Scribus marks with it.
    fn push_char(&mut self, character: char) {
        match character {
            '\r' | '\n' | '\u{5}' => self.end_paragraph(None),
            '\t' | '\u{4}' => self.push_text_char('\t'),
            '\u{1a}' | '\u{1b}' | '\u{1c}' => self.push_piece(Piece::LineBreak),
            '\u{ad}' if self.hyphenation_point || self.is_empty() => self.push_text_char('\u{ad}'),
            '\u{ad}' => self.hyphenation_point = true,
            '\u{0}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => self.hyphenation_point = false,
            other => self.push_text_char(other),
        }
    }

    fn push_text_char(&mut self, character: char) {
        self.hyphenation_point = false;
        self.push_str(character.encode_utf8(&mut [0; 4]));
    }

    fn is_empty(&self) -> bool {
        self.pieces.is_empty() && self.paragraphs.is_empty()
    }

    /// Where the paragraph being read starts in the pieces.
    fn paragraph_start(&self) -> usize {
        self.paragraphs.last().map_or(0, |paragraph| paragraph.end)
    }

    /// Ends the paragraph being read, of `style` if it names one, else of the style the older
    /// document's text gave it.
    fn end_paragraph(&mut self, style: Option<StyleId>) {
        self.hyphenation_point = false;
        let legacy = self.legacy_style.take();
        self.paragraphs.push(Paragraph {
            style: style.or(legacy),
            end: self.pieces.len(),
        });
    }

    /// Ends the paragraph being read, if it holds anything.
    fn finish(&mut self) {
        if self.pieces.len() > self.paragraph_start() {
            self.end_paragraph(None);
        }
    }
}

/// A cell of a table.
#[derive(Debug)]
struct Cell {
    row: u32,
    column: u32,
    story: Story,
}

/// An image Scribus keeps in the document, compressed by `qCompress`.
#[derive(Debug)]
struct InlineImage {
    name: String,
    compressed: Vec<u8>,
}

/// What of a page item is written.
#[derive(Debug)]
enum Content {
    /// A text frame's or text on a path's story.
    Story(Story),
    /// A table's cells, by row and column once it is read.
    Table(Vec<Cell>),
    InlineImage(InlineImage),
    /// A group's items.
    Group(Vec<usize>),
    Nothing,
}

/// The lists of page items Scribus keeps, each item of which an element of its own names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum List {
    /// `MASTEROBJECT`: the items of the master pages.
    Master,
    /// `PAGEOBJECT`: the items of the pages and the pasteboard, and those of groups.
    Page,
    /// `FRAMEOBJECT`: the items anchored in text.
    Anchored,
}

impl List {
    fn of(element: &str) -> Option<List> {
        match element {
            "MASTEROBJECT" => Some(List::Master),
            "PAGEOBJECT" => Some(List::Page),
            "FRAMEOBJECT" => Some(List::Anchored),
            _ => None,
        }
    }

    fn element(self) -> &'static str {
        match self {
            List::Master => "MASTEROBJECT",
            List::Page => "PAGEOBJECT",
            List::Anchored => "FRAMEOBJECT",
        }
    }
}

/// How a page item names another to thread its text to: by its `ItemID` (Scribus 1.5 and
/// later), or by its place in its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Link {
    Id(i64),
    Ordinal(List, usize),
}

/// A page item: a frame, a shape, a group or a table.
#[derive(Debug)]
struct Item {
    list: List,
    /// The page it is on (`OwnPage`), negative on the pasteboard.
    page: i64,
    x: f64,
    y: f64,
    link: Link,
    next: Option<Link>,
    content: Content,
}

impl Item {
    /// Where the item is written among the items of the document or of its group: the master
    /// pages' first, then the pages', then the pasteboard's, by page and then from top to
    /// bottom and left to right.
    fn compare(&self, other: &Item) -> Ordering {
        self.section()
            .cmp(&other.section())
            .then(self.page.cmp(&other.page))
            .then(self.y.total_cmp(&other.y))
            .then(self.x.total_cmp(&other.x))
    }

    fn section(&self) -> u8 {
        match self.list {
            List::Master => 0,
            List::Page | List::Anchored if self.page >= 0 => 1,
            List::Page | List::Anchored => 2,
        }
    }
}

/// A Scribus document as read for its text.
#[derive(Debug, Default)]
struct Document {
    metadata: Vec<(&'static str, String)>,
    /// The name and parent of each paragraph style.
    styles: Vec<(String, String)>,
    style_ids: HashMap<String, StyleId>,
    items: Vec<Item>,
    /// The items of the master pages, the pages and the pasteboard, not those of groups.
    placed: Vec<usize>,
    /// The items anchored in text, by their `InID`.
    anchored: HashMap<i64, usize>,
}

impl Document {
    /// Reads `input`, failing it where it is not well-formed XML, unless it is of a version of
    /// Scribus that writes text XML does not allow, which is left unread.
    fn read<'a>(input: &[u8]) -> Result<Document, Decline<'a>> {
        let mut reading = Reading::default();
        match xml::parse_sax(input, WHAT, &mut reading) {
            Ok(()) => Ok(reading.document),
            Err(failure) => Err(declined(failure, reading.raw_text_version)),
        }
    }

    fn style_id(&mut self, name: &str) -> Option<StyleId> {
        if name.is_empty() {
            return None;
        }
        if let Some(&id) = self.style_ids.get(name) {
            return Some(id);
        }
        let id = StyleId(self.style_ids.len());
        self.style_ids.insert(name.to_owned(), id);
        Some(id)
    }

    /// The heading level of each style read, by its id.
    fn style_levels(&self) -> Vec<Option<usize>> {
        let headings = heading_levels(&self.styles, built_in_heading);
        let mut levels = vec![None; self.style_ids.len()];
        for (name, &StyleId(id)) in &self.style_ids {
            levels[id] = headings
                .get(name)
                .copied()
                .or_else(|| built_in_heading(name));
        }
        levels
    }

    fn write(mut self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        for (key, value) in std::mem::take(&mut self.metadata) {
            xhtml.metadata_mut().set(key, value);
        }
        let mut order = self.placed.clone();
        self.sort(&mut order);
        let mut writer = Writer::new(&self, xhtml);
        for &index in &order {
            writer.item(index, 0)?;
        }
        Ok(())
    }

    /// Sorts the items `indexes` into the order they are written in.
    fn sort(&self, indexes: &mut [usize]) {
        indexes.sort_by(|&a, &b| self.items[a].compare(&self.items[b]).then(a.cmp(&b)));
    }
}

/// Why `failure` of the reading of a document leaves it unread or fails it, the document of
/// a version of Scribus that writes raw control characters in its text if
/// `raw_text_version` names it.
fn declined<'a>(failure: Failure, raw_text_version: Option<String>) -> Decline<'a> {
    match (failure, raw_text_version) {
        (Failure::Malformed(reason), Some(version)) => Decline::Unread(format!(
            "a document of Scribus {version}, which writes paragraph breaks and tabs as raw \
             control characters XML does not allow, that is not well-formed: {reason}"
        )),
        (Failure::Malformed(reason), None) => Decline::Fails(Error::ParseError(reason)),
        (Failure::Extraction(error) | Failure::Embedded(error), _) => Decline::Fails(error),
        (Failure::Stopped(stop), _) => Decline::Fails(Error::Unknown(format!(
            "reading {WHAT} stopped before its text is written: {stop:?}"
        ))),
    }
}

/// Whether the Scribus `version` is before 1.3.4, which writes paragraph breaks and tabs as
/// raw control characters: each of its first three numbers, a missing one 0, compared.
fn writes_raw_text(version: &str) -> bool {
    let mut numbers = version.split('.').map(|part| {
        let digits = part.bytes().take_while(u8::is_ascii_digit).count();
        part[..digits].parse::<u32>().unwrap_or(0)
    });
    let release = [(); 3].map(|()| numbers.next().unwrap_or(0));
    release < ELEMENT_TEXT_VERSION
}

/// The heading level of a style named `Heading N`.
fn built_in_heading(style: &str) -> Option<usize> {
    style
        .strip_prefix("Heading ")?
        .parse::<usize>()
        .ok()
        .filter(|level| (1..=HEADINGS.len()).contains(level))
}

/// The heading level of each style of `styles`, its name and the style it is based on, that is
/// a heading's by `built_in` or is based on one; a cycle of styles is none.
fn heading_levels(
    styles: &[(String, String)],
    built_in: impl Fn(&str) -> Option<usize>,
) -> HashMap<String, usize> {
    let based_on: HashMap<&str, &str> = styles
        .iter()
        .map(|(name, base)| (name.as_str(), base.as_str()))
        .collect();
    let mut resolved: HashMap<&str, Option<usize>> = HashMap::with_capacity(styles.len());
    let mut chain = Vec::new();
    for (name, _) in styles {
        chain.clear();
        let mut current = name.as_str();
        let level = loop {
            if let Some(&level) = resolved.get(current) {
                break level;
            }
            if let Some(level) = built_in(current) {
                break Some(level);
            }
            if chain.contains(&current) {
                break None;
            }
            chain.push(current);
            match based_on.get(current) {
                Some(&base) if !base.is_empty() => current = base,
                Some(_) | None => break None,
            }
        };
        for &link in &chain {
            resolved.insert(link, level);
        }
    }
    resolved
        .into_iter()
        .filter_map(|(name, level)| Some((name.to_owned(), level?)))
        .collect()
}

/// The value of the attribute `name` of `element` as a number, if it has one.
fn number<T: FromStr>(
    attributes: &[Attribute],
    element: &str,
    name: &str,
) -> Result<Option<T>, Failure> {
    attribute(attributes, "", name)
        .map(|value| {
            value.trim().parse::<T>().map_err(|_| {
                Failure::Malformed(format!(
                    "{WHAT} has a {element} whose {name} is not a number: {value:?}"
                ))
            })
        })
        .transpose()
}

/// What ending an open element does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Nothing,
    /// An element none of whose content is read: a pattern's items.
    Skipped,
    Item,
    Cell,
    StoryText,
}

/// The document being read, as the SAX handler of its XML.
#[derive(Debug, Default)]
struct Reading {
    document: Document,
    open: Vec<Open>,
    /// The items being read, the innermost last.
    items: Vec<usize>,
    cell: Option<Cell>,
    /// How many items of each list are read.
    ordinals: HashMap<List, usize>,
    /// The version of Scribus that wrote the document, if it writes raw control characters in
    /// its text.
    raw_text_version: Option<String>,
}

impl Reading {
    /// The story being read, if any.
    fn story(&mut self) -> Option<&mut Story> {
        if let Some(cell) = &mut self.cell {
            return Some(&mut cell.story);
        }
        let &index = self.items.last()?;
        match &mut self.document.items[index].content {
            Content::Story(story) => Some(story),
            Content::Table(_) | Content::InlineImage(_) | Content::Group(_) | Content::Nothing => {
                None
            }
        }
    }

    fn root(&mut self, local_name: &str, attributes: &[Attribute]) -> Result<(), Failure> {
        let version = attribute(attributes, "", "Version");
        let raw_text = match local_name {
            "SCRIBUSUTF8NEW" => version.is_some_and(writes_raw_text),
            "SCRIBUSUTF8" | "SCRIBUS" => true,
            other => {
                return Err(Failure::Malformed(format!(
                    "{WHAT} has the root element {other:?}, not SCRIBUSUTF8NEW, SCRIBUSUTF8 or \
                     SCRIBUS"
                )))
            }
        };
        if raw_text {
            self.raw_text_version = Some(version.unwrap_or(local_name).to_owned());
        }
        if let Some(version) = version.filter(|version| !version.trim().is_empty()) {
            self.document
                .metadata
                .push((CREATOR_TOOL, format!("Scribus {}", version.trim())));
        }
        Ok(())
    }

    fn document_properties(&mut self, attributes: &[Attribute]) -> Result<(), Failure> {
        let metadata = &mut self.document.metadata;
        for (name, key) in PROPERTIES {
            if let Some(value) = attribute(attributes, "", name).map(str::trim) {
                if !value.is_empty() {
                    metadata.push((key, value.to_owned()));
                }
            }
        }
        let language = attribute(attributes, "", "LANGUAGE").map(str::trim);
        if let Some(language) = language.filter(|language| !language.is_empty()) {
            if !metadata.iter().any(|&(key, _)| key == LANGUAGE) {
                metadata.push((LANGUAGE, language.replace('_', "-")));
            }
        }
        if let Some(pages) = number::<u32>(attributes, "DOCUMENT", "ANZPAGES")? {
            metadata.push((PAGE_COUNT, pages.to_string()));
        }
        Ok(())
    }

    fn start_item(&mut self, list: List, attributes: &[Attribute]) -> Result<(), Failure> {
        let element = list.element();
        let ordinal = self.ordinals.entry(list).or_insert(0);
        let own_ordinal = *ordinal;
        *ordinal += 1;
        let kind = number::<i32>(attributes, element, "PTYPE")?
            .ok_or_else(|| Failure::Malformed(format!("{WHAT} has a {element} without a PTYPE")))?;
        let flag = |name: &str| attribute(attributes, "", name) == Some("1");
        let content = match kind {
            2 if flag("isInlineImage") => {
                let data = attribute(attributes, "", "ImageData").unwrap_or("");
                let name = attribute(attributes, "", "ANNAME").unwrap_or("");
                let extension = attribute(attributes, "", "inlineImageExt").unwrap_or("");
                Content::InlineImage(InlineImage {
                    name: match extension.is_empty() {
                        true => name.to_owned(),
                        false => format!("{name}.{extension}"),
                    },
                    compressed: commons_base64_decode(data),
                })
            }
            4 | 8 if !flag("isNoteFrame") => {
                let style = attribute(attributes, "", "PSTYLE").unwrap_or("");
                Content::Story(Story::new(self.document.style_id(style)))
            }
            12 => Content::Group(Vec::new()),
            16 => Content::Table(Vec::new()),
            _ => Content::Nothing,
        };
        let id = number::<i64>(attributes, element, "ItemID")?;
        let next = number::<i64>(attributes, element, "NEXTITEM")?
            .filter(|&next| next >= 0)
            .map(|next| match id {
                Some(_) => Ok(Link::Id(next)),
                None => usize::try_from(next)
                    .map(|next| Link::Ordinal(list, next))
                    .map_err(|_| {
                        Failure::Malformed(format!(
                            "{WHAT} has a {element} whose NEXTITEM is out of range: {next}"
                        ))
                    }),
            })
            .transpose()?;
        let index = self.document.items.len();
        self.document.items.push(Item {
            list,
            page: number(attributes, element, "OwnPage")?.unwrap_or(0),
            x: number(attributes, element, "XPOS")?.unwrap_or(0.0),
            y: number(attributes, element, "YPOS")?.unwrap_or(0.0),
            link: id.map_or(Link::Ordinal(list, own_ordinal), Link::Id),
            next,
            content,
        });
        match self.items.last() {
            Some(&parent) => match &mut self.document.items[parent].content {
                Content::Group(children) => children.push(index),
                _ => {
                    return Err(Failure::Malformed(format!(
                        "{WHAT} has a {element} inside a page item that is not a group"
                    )))
                }
            },
            None if list == List::Anchored => {
                if let Some(anchor) = number::<i64>(attributes, element, "InID")? {
                    self.document.anchored.entry(anchor).or_insert(index);
                }
            }
            None => self.document.placed.push(index),
        }
        self.items.push(index);
        Ok(())
    }

    fn end_item(&mut self) {
        let Some(index) = self.items.pop() else {
            return;
        };
        match &mut self.document.items[index].content {
            Content::Story(story) => story.finish(),
            Content::Table(cells) => cells.sort_by_key(|cell| (cell.row, cell.column)),
            Content::InlineImage(_) | Content::Group(_) | Content::Nothing => {}
        }
    }

    fn start_cell(&mut self, attributes: &[Attribute]) -> Result<(), Failure> {
        let in_table = self
            .items
            .last()
            .is_some_and(|&index| matches!(self.document.items[index].content, Content::Table(_)));
        if !in_table || self.cell.is_some() {
            return Err(Failure::Malformed(format!(
                "{WHAT} has a Cell outside a table's TableData"
            )));
        }
        let position = |name: &str| {
            number::<u32>(attributes, "Cell", name)?.ok_or_else(|| {
                Failure::Malformed(format!("{WHAT} has a table Cell without a {name}"))
            })
        };
        self.cell = Some(Cell {
            row: position("Row")?,
            column: position("Column")?,
            story: Story::default(),
        });
        Ok(())
    }

    fn end_cell(&mut self) {
        let Some(mut cell) = self.cell.take() else {
            return;
        };
        cell.story.finish();
        if let Some(&index) = self.items.last() {
            if let Content::Table(cells) = &mut self.document.items[index].content {
                cells.push(cell);
            }
        }
    }

    /// Reads the `ITEXT` element with `attributes` into the story being read.
    fn text(&mut self, attributes: &[Attribute]) -> Result<(), Failure> {
        let legacy_style =
            attribute(attributes, "", "PSTYLE").and_then(|style| self.document.style_id(style));
        let anchor = number::<i64>(attributes, "ITEXT", "COBJ")?;
        let unicode = number::<u32>(attributes, "ITEXT", "Unicode")?;
        let Some(story) = self.story() else {
            return Ok(());
        };
        if legacy_style.is_some() {
            story.legacy_style = legacy_style;
        }
        match (unicode, anchor) {
            (Some(_), Some(anchor)) => story.push_piece(Piece::Frame(anchor)),
            (Some(code), None) => {
                if let Some(character) = char::from_u32(code) {
                    story.push_char(character);
                }
            }
            (None, _) => {
                for character in attribute(attributes, "", "CH").unwrap_or("").chars() {
                    story.push_char(character);
                }
            }
        }
        Ok(())
    }

    fn paragraph_style(&mut self, attributes: &[Attribute]) -> Option<StyleId> {
        let parent = attribute(attributes, "", "PARENT").unwrap_or("");
        self.document.style_id(parent)
    }

    /// Reads the element `local_name` of a story into the story being read, if it is one.
    fn story_element(&mut self, local_name: &str, attributes: &[Attribute]) -> Result<(), Failure> {
        let character = match local_name {
            "ITEXT" => return self.text(attributes),
            "para" | "trail" | "DefaultStyle" => {
                let style = self.paragraph_style(attributes);
                if let Some(story) = self.story() {
                    match local_name {
                        "para" => story.end_paragraph(style),
                        "trail" => {
                            if story.pieces.len() > story.paragraph_start() {
                                story.end_paragraph(style);
                            }
                        }
                        _ => {
                            if style.is_some() {
                                story.default_style = style;
                            }
                        }
                    }
                }
                return Ok(());
            }
            "tab" => '\t',
            "breakline" | "breakcol" | "breakframe" => '\u{1c}',
            "nbhyphen" => '\u{2011}',
            "nbspace" => '\u{a0}',
            "zwnbspace" => '\u{2060}',
            "zwspace" => '\u{200b}',
            _ => return Ok(()),
        };
        if let Some(story) = self.story() {
            story.push_char(character);
        }
        Ok(())
    }

    fn start(&mut self, local_name: &str, attributes: &[Attribute]) -> Result<Open, Failure> {
        if self.open.is_empty() {
            self.root(local_name, attributes)?;
            return Ok(Open::Nothing);
        }
        if let Some(list) = List::of(local_name) {
            self.start_item(list, attributes)?;
            return Ok(Open::Item);
        }
        Ok(match local_name {
            "DOCUMENT" => {
                self.document_properties(attributes)?;
                Open::Nothing
            }
            "STYLE" => {
                if let Some(name) = attribute(attributes, "", "NAME") {
                    let parent = attribute(attributes, "", "PARENT").unwrap_or("");
                    self.document
                        .styles
                        .push((name.to_owned(), parent.to_owned()));
                }
                Open::Nothing
            }
            "Pattern" => Open::Skipped,
            "Cell" => {
                self.start_cell(attributes)?;
                Open::Cell
            }
            "StoryText" => Open::StoryText,
            _ => {
                self.story_element(local_name, attributes)?;
                Open::Nothing
            }
        })
    }

    fn end(&mut self, open: Open) {
        match open {
            Open::Nothing | Open::Skipped => {}
            Open::Item => self.end_item(),
            Open::Cell => self.end_cell(),
            Open::StoryText => {
                if let Some(story) = self.story() {
                    story.finish();
                }
            }
        }
    }
}

impl SaxHandler for Reading {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let open = match self.open.contains(&Open::Skipped) {
            true => Open::Nothing,
            false => self.start(local_name, attributes)?,
        };
        self.open.push(open);
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        if let Some(open) = self.open.pop() {
            self.end(open);
        }
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// The bytes `qCompress` compressed into `data`: their length, big-endian in four bytes, and
/// their zlib stream.
fn q_uncompress(data: &[u8]) -> Result<Input<'static>, String> {
    let stream = data
        .get(4..)
        .ok_or_else(|| format!("its {} bytes end inside their length", data.len()))?;
    zlib::decode(stream)
        .and_then(Limited::whole)
        .map_err(|undecoded| undecoded.to_string())
}

/// The writing of a document's items, each written once.
struct Writer<'d, 'x, 'h> {
    document: &'d Document,
    xhtml: &'x mut Xhtml<'h>,
    levels: Vec<Option<usize>>,
    written: Vec<bool>,
    /// Whether each item is written in the order of the document's pages, not where text
    /// anchors it.
    placed: Vec<bool>,
    /// The item each story is threaded on to, and from.
    next: Vec<Option<usize>>,
    previous: Vec<Option<usize>>,
}

impl<'d, 'x, 'h> Writer<'d, 'x, 'h> {
    fn new(document: &'d Document, xhtml: &'x mut Xhtml<'h>) -> Self {
        let count = document.items.len();
        let mut placed = vec![false; count];
        let mut pending = document.placed.clone();
        while let Some(index) = pending.pop() {
            placed[index] = true;
            if let Content::Group(children) = &document.items[index].content {
                pending.extend_from_slice(children);
            }
        }
        let is_story = |index: usize| matches!(document.items[index].content, Content::Story(_));
        let stories: HashMap<Link, usize> = (0..count)
            .filter(|&index| is_story(index))
            .map(|index| (document.items[index].link, index))
            .collect();
        let next: Vec<Option<usize>> = (0..count)
            .map(|index| {
                let target = stories.get(&document.items[index].next?).copied()?;
                (is_story(index) && target != index).then_some(target)
            })
            .collect();
        let mut previous = vec![None; count];
        for (index, target) in next.iter().enumerate() {
            if let Some(target) = *target {
                previous[target].get_or_insert(index);
            }
        }
        Self {
            document,
            xhtml,
            levels: document.style_levels(),
            written: vec![false; count],
            placed,
            next,
            previous,
        }
    }

    /// The first story of the thread the story of `index` is on, itself if the thread runs in
    /// a circle.
    fn thread_start(&self, index: usize) -> usize {
        let mut current = index;
        for _ in 0..self.previous.len() {
            match self.previous[current] {
                Some(previous) if previous == index => return index,
                Some(previous) => current = previous,
                None => return current,
            }
        }
        index
    }

    fn item(&mut self, index: usize, depth: usize) -> Result<(), Failure> {
        if self.written[index] {
            return Ok(());
        }
        if depth > MAX_NESTING {
            return Err(Failure::Malformed(format!(
                "{WHAT} nests frames in groups and text more than {MAX_NESTING} deep"
            )));
        }
        let document = self.document;
        match &document.items[index].content {
            Content::Story(_) => {
                let start = self.thread_start(index);
                if start != index && self.placed[start] && !self.written[start] {
                    return Ok(());
                }
                let mut current = Some(index);
                while let Some(story) = current.filter(|&story| !self.written[story]) {
                    self.written[story] = true;
                    if let Content::Story(text) = &document.items[story].content {
                        self.story(text, depth)?;
                    }
                    current = self.next[story];
                }
            }
            Content::Table(cells) => {
                self.written[index] = true;
                self.table(cells, depth)?;
            }
            Content::InlineImage(image) => {
                self.written[index] = true;
                self.image(image)?;
            }
            Content::Group(children) => {
                self.written[index] = true;
                let mut order = children.clone();
                document.sort(&mut order);
                for child in order {
                    self.item(child, depth + 1)?;
                }
            }
            Content::Nothing => self.written[index] = true,
        }
        Ok(())
    }

    fn story(&mut self, story: &Story, depth: usize) -> Result<(), Failure> {
        let mut start = 0;
        for paragraph in &story.paragraphs {
            let pieces = &story.pieces[start..paragraph.end];
            start = paragraph.end;
            let has_text = pieces.iter().any(|piece| !matches!(piece, Piece::Frame(_)));
            if has_text {
                let level = paragraph
                    .style
                    .or(story.default_style)
                    .and_then(|StyleId(id)| self.levels[id]);
                let element = level.map_or("p", |level| HEADINGS[level - 1]);
                self.xhtml.start_element(element)?;
                for piece in pieces {
                    match piece {
                        Piece::Text(range) => self.xhtml.characters(&story.text[range.clone()])?,
                        Piece::LineBreak => {
                            self.xhtml.start_element("br")?;
                            self.xhtml.end_element("br")?;
                        }
                        Piece::Frame(_) => {}
                    }
                }
                self.xhtml.end_element(element)?;
            }
            for piece in pieces {
                if let Piece::Frame(anchor) = piece {
                    if let Some(&frame) = self.document.anchored.get(anchor) {
                        self.item(frame, depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn table(&mut self, cells: &[Cell], depth: usize) -> Result<(), Failure> {
        if cells.is_empty() {
            return Ok(());
        }
        self.xhtml.start_element("table")?;
        let mut row = None;
        for cell in cells {
            if row != Some(cell.row) {
                if row.is_some() {
                    self.xhtml.end_element("tr")?;
                }
                self.xhtml.start_element("tr")?;
                row = Some(cell.row);
            }
            self.xhtml.start_element("td")?;
            self.story(&cell.story, depth)?;
            self.xhtml.end_element("td")?;
        }
        self.xhtml.end_element("tr")?;
        self.xhtml.end_element("table")?;
        Ok(())
    }

    /// Hands `image` to the embedded document extractor, or records why it does not
    /// decompress.
    fn image(&mut self, image: &InlineImage) -> Result<(), Failure> {
        let input = match q_uncompress(&image.compressed) {
            Ok(input) => input,
            Err(reason) => {
                record_stream_exception(
                    self.xhtml.metadata_mut(),
                    &format!(
                        "org.apache.tika.exception.TikaException: {WHAT} holds the image {:?}, \
                         which does not decompress: {reason}",
                        image.name
                    ),
                );
                return Ok(());
            }
        };
        let mut metadata = TikaMetadata::new();
        if !image.name.is_empty() {
            metadata.set(RESOURCE_NAME, image.name.as_str());
        }
        ResourceType::Inline.set(&mut metadata);
        self.xhtml
            .parse_embedded_input(&input, &metadata, Wrapping::PackageEntry)
    }
}

#[cfg(test)]
mod tests;
