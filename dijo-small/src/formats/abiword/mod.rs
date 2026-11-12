//! AbiWord documents (`.abw`), read for their text: the AWML `<abiword>` document AbiWord saves,
//! in the AWML namespace or, as older versions wrote it, in none.
//!
//! Each `<section>` is read in order, a header or footer section in a `div` of class `header` or
//! `footer`. A `<p>` is a paragraph, a heading where its style is `Heading N` or `Numbered
//! Heading N` or is based on one, and an item of a list where it names one by `listid`: list
//! items are nested in `ol` or `ul` by their `level`, ordered or not as the list's definition in
//! `<lists>` (or else the paragraph's `list-style`) numbers it, without the label AbiWord draws
//! and the tab after it. Its text is written as it stands, `<br/>`, `<cbr/>` and `<pbr/>` as line
//! breaks and `<a xlink:href>` as a link. A footnote, endnote or annotation is written in a
//! `div` of class `footnote`, `endnote` or `annotation` after the paragraph it is anchored in, as
//! the output places a block inside a paragraph, and a text box frame in a `div` of class
//! `textbox`. A `<table>` is a table whose rows are its cells' `top-attach`.
//! Bookmarks, the placement of images and the values of fields, which AbiWord computes and does
//! not save, are not written; each item of `<data>`, an image or an object, is handed to the
//! embedded document extractor, named by its `name` and of its `mime-type`.
//!
//! The `<metadata>` properties are the document's Dublin Core, its keywords, generator and the
//! dates it was created and last changed. AbiWord writes those dates as C's `ctime` writes the
//! local time it saved in, without its time zone: they are given as ISO local date-times without
//! an offset, and a date in any other form as it is written.
//!
//! A gzip-compressed `.zabw` document is detected as gzip, whose parser hands the document it
//! holds to this one.

use std::collections::HashMap;

use crate::config::ParserConfigs;
use crate::formats::embedded::{ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xml::{self, attribute, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::java::simple_date_format::SHORT_MONTHS;
use crate::java::CommonsBase64;
use crate::mime::{self, MediaType};

const NAME: &str = "dijo::AbiWordParser";
const WHAT: &str = "The AbiWord document";
const AWML_NAMESPACE: &str = "http://www.abisource.com/awml.dtd";
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";

const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
/// The deepest a list is nested: AbiWord's list levels go no deeper.
const MAX_LIST_DEPTH: usize = 9;

/// The text of AbiWord documents.
pub struct AbiWordParser;

impl Parser for AbiWordParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-abiword"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| read_document(input, pages, xhtml)))
    }
}

/// Writes the document `input` as its XML is read, its `pages` released as they are.
fn read_document(input: &[u8], pages: Pages<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut reading = Reading {
        xhtml,
        open: Vec::new(),
        styles: Vec::new(),
        headings: HashMap::new(),
        lists: HashMap::new(),
        stories: vec![Story::default()],
        tables: Vec::new(),
        property: None,
        datum: None,
        after_label: false,
    };
    xml::parse_paged(input, pages, WHAT, &mut reading)
}

/// What a `<metadata>` property is in the document's metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Property {
    Text(&'static str),
    Date(&'static str),
}

/// The metadata keys of AbiWord's properties.
const PROPERTIES: [(&str, Property); 17] = [
    ("dc.title", Property::Text("dc:title")),
    ("dc.creator", Property::Text("dc:creator")),
    ("dc.subject", Property::Text("dc:subject")),
    ("dc.description", Property::Text("dc:description")),
    ("dc.publisher", Property::Text("dc:publisher")),
    ("dc.contributor", Property::Text("dc:contributor")),
    ("dc.type", Property::Text("dc:type")),
    ("dc.format", Property::Text("dc:format")),
    ("dc.source", Property::Text("dc:source")),
    ("dc.language", Property::Text("dc:language")),
    ("dc.relation", Property::Text("dc:relation")),
    ("dc.coverage", Property::Text("dc:coverage")),
    ("dc.rights", Property::Text("dc:rights")),
    ("abiword.keywords", Property::Text("meta:keyword")),
    ("abiword.generator", Property::Text("xmp:CreatorTool")),
    ("dc.date", Property::Date("dcterms:created")),
    (
        "abiword.date_last_changed",
        Property::Date("dcterms:modified"),
    ),
];

impl Property {
    fn of(key: &str) -> Option<Property> {
        PROPERTIES
            .iter()
            .find(|&&(name, _)| name == key)
            .map(|&(_, property)| property)
    }

    /// Sets this property to `value` in `metadata`, unless it is blank.
    fn set(self, value: &str, metadata: &mut TikaMetadata) {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        match self {
            Property::Text(key) => metadata.set(key, value),
            Property::Date(key) => match local_date_time(value) {
                Some(date) => metadata.set(key, date),
                None => metadata.set(key, value),
            },
        }
    }
}

/// The ISO 8601 local date-time of `text` as C's `ctime` writes it, `Thu Nov 12 21:04:05 2026`,
/// if it is one.
fn local_date_time(text: &str) -> Option<String> {
    let mut fields = text.split_ascii_whitespace();
    let (_weekday, month, day, time, year) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if fields.next().is_some() {
        return None;
    }
    let month = SHORT_MONTHS.iter().position(|&name| name == month)? + 1;
    let number = |text: &str, range: std::ops::RangeInclusive<u32>| {
        text.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| text.parse::<u32>().ok())
            .flatten()
            .filter(|value| range.contains(value))
    };
    let day = number(day, 1..=31)?;
    let year = number(year, 0..=9999)?;
    let mut clock = time.split(':');
    let hour = number(clock.next()?, 0..=23)?;
    let minute = number(clock.next()?, 0..=59)?;
    let second = number(clock.next()?, 0..=60)?;
    if clock.next().is_some() {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
    ))
}

/// The value of the property `key` of the AbiWord properties `props`, `key:value; key:value`.
fn prop<'p>(props: &'p str, key: &str) -> Option<&'p str> {
    props.split(';').find_map(|pair| {
        let (name, value) = pair.split_once(':')?;
        (name.trim() == key).then(|| value.trim())
    })
}

/// How a list numbers its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Ordered,
    Unordered,
    /// AbiWord's `NOT_A_LIST`: its paragraphs are not list items.
    None,
}

impl ListKind {
    /// The kind of AbiWord's `FL_ListType` numbered `number`, as `<l type>` writes it.
    fn of_type(number: &str) -> ListKind {
        match number.trim().parse::<u32>() {
            Ok(0..=4 | 0x80 | 0x81) => ListKind::Ordered,
            Ok(0xff) => ListKind::None,
            Ok(_) | Err(_) => ListKind::Unordered,
        }
    }

    /// The kind of the list style named `style`, as a paragraph's `list-style` names it.
    fn of_style(style: &str) -> ListKind {
        match style {
            "Numbered List" | "Lower Case List" | "Upper Case List" | "Lower Roman List"
            | "Upper Roman List" | "Arabic List" | "Hebrew List" => ListKind::Ordered,
            "None" => ListKind::None,
            _ => ListKind::Unordered,
        }
    }

    fn element(self) -> &'static str {
        match self {
            ListKind::Ordered => "ol",
            ListKind::Unordered | ListKind::None => "ul",
        }
    }
}

/// The heading level of AbiWord's built-in style named `style`, if it is a heading's.
fn built_in_heading(style: &str) -> Option<usize> {
    let level = style
        .strip_prefix("Heading ")
        .or_else(|| style.strip_prefix("Numbered Heading "))?;
    level
        .parse::<usize>()
        .ok()
        .filter(|level| (1..=HEADINGS.len()).contains(level))
}

/// The heading level of each style of `styles`, its name and the style it is based on, that is
/// a heading's or is based on one; a cycle of styles is none.
fn heading_levels(styles: &[(String, String)]) -> HashMap<String, usize> {
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
            if let Some(level) = built_in_heading(current) {
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

/// What a paragraph is written as.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Heading(usize),
    Item {
        list: String,
        level: usize,
        list_kind: ListKind,
    },
}

/// Where a story is in its paragraphs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    /// Between paragraphs.
    Between,
    /// In a paragraph none of whose content is written yet.
    Pending(Kind),
    /// In a paragraph being written.
    Open(Kind),
}

/// A list being written, its item open or not.
#[derive(Debug)]
struct OpenList {
    id: String,
    list_kind: ListKind,
    item_open: bool,
}

/// A run of paragraphs: a section, a cell, a note or a text box, each with the lists it has
/// open.
#[derive(Debug)]
struct Story {
    lists: Vec<OpenList>,
    block: Block,
}

impl Default for Story {
    fn default() -> Self {
        Self {
            lists: Vec::new(),
            block: Block::Between,
        }
    }
}

impl Story {
    /// Closes the innermost list, if any.
    fn close_list(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        let Some(list) = self.lists.pop() else {
            return Ok(());
        };
        if list.item_open {
            xhtml.end_element("li")?;
        }
        xhtml.end_element(list.list_kind.element())
    }

    fn close_lists(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        while !self.lists.is_empty() {
            self.close_list(xhtml)?;
        }
        Ok(())
    }

    /// Starts an item of the list `id` at `level`, nesting lists down to it and closing those
    /// below it or of another list at it.
    fn start_item(
        &mut self,
        xhtml: &mut Xhtml,
        id: &str,
        level: usize,
        list_kind: ListKind,
    ) -> Result<(), Stop> {
        while self.lists.len() > level
            || (self.lists.len() == level && self.lists.last().is_some_and(|top| top.id != id))
        {
            self.close_list(xhtml)?;
        }
        if self.lists.len() == level {
            let top = self.lists.last_mut().expect("a list is open at the level");
            if top.item_open {
                xhtml.end_element("li")?;
                top.item_open = false;
            }
        }
        while self.lists.len() < level {
            if let Some(parent) = self.lists.last_mut() {
                if !parent.item_open {
                    xhtml.start_element("li")?;
                    parent.item_open = true;
                }
            }
            xhtml.start_element(list_kind.element())?;
            self.lists.push(OpenList {
                id: id.to_owned(),
                list_kind,
                item_open: false,
            });
        }
        let top = self.lists.last_mut().expect("a list is open at the level");
        xhtml.start_element("li")?;
        top.item_open = true;
        Ok(())
    }

    /// Writes the start of the paragraph if none of it is written yet; whether the story is in
    /// a paragraph.
    fn open(&mut self, xhtml: &mut Xhtml) -> Result<bool, Stop> {
        let kind = match &self.block {
            Block::Between => return Ok(false),
            Block::Open(_) => return Ok(true),
            Block::Pending(kind) => kind.clone(),
        };
        match &kind {
            Kind::Paragraph => {
                self.close_lists(xhtml)?;
                xhtml.start_element("p")?;
            }
            Kind::Heading(level) => {
                self.close_lists(xhtml)?;
                xhtml.start_element(HEADINGS[level - 1])?;
            }
            Kind::Item {
                list,
                level,
                list_kind,
            } => self.start_item(xhtml, list, *level, *list_kind)?,
        }
        self.block = Block::Open(kind);
        Ok(true)
    }

    /// Ends the paragraph; an item stays open for the lists nested in it.
    fn end_paragraph(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        match std::mem::replace(&mut self.block, Block::Between) {
            Block::Open(Kind::Paragraph) => xhtml.end_element("p"),
            Block::Open(Kind::Heading(level)) => xhtml.end_element(HEADINGS[level - 1]),
            Block::Open(Kind::Item { .. }) | Block::Pending(_) | Block::Between => Ok(()),
        }
    }
}

/// A table being written, with the `top-attach` of its open row.
#[derive(Debug, Default)]
struct Table {
    row: Option<u64>,
}

/// How the bytes of a `<data>` item are written.
#[derive(Debug)]
enum Encoding {
    Base64(CommonsBase64),
    Raw,
}

/// A `<data>` item being read.
#[derive(Debug)]
struct Datum {
    name: String,
    media_type: Option<MediaType>,
    encoding: Encoding,
    bytes: Vec<u8>,
}

impl Datum {
    fn start(attributes: &[Attribute]) -> Datum {
        Datum {
            name: attribute(attributes, "", "name").unwrap_or("").to_owned(),
            media_type: attribute(attributes, "", "mime-type").and_then(MediaType::parse),
            encoding: match attribute(attributes, "", "base64") {
                Some("yes") => Encoding::Base64(CommonsBase64::default()),
                Some(_) | None => Encoding::Raw,
            },
            bytes: Vec::new(),
        }
    }

    fn text(&mut self, text: &str) {
        match &mut self.encoding {
            Encoding::Base64(decoder) => decoder.push(text, &mut self.bytes),
            Encoding::Raw => self.bytes.extend_from_slice(text.as_bytes()),
        }
    }

    /// Hands the item to the embedded document extractor.
    fn embed(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let mut bytes = self.bytes;
        if let Encoding::Base64(decoder) = self.encoding {
            decoder.finish(&mut bytes);
        }
        let mut metadata = TikaMetadata::new();
        let extension = self.media_type.as_ref().map_or("", mime::extension);
        let name = match self.name.ends_with(extension) {
            true => self.name,
            false => format!("{}{extension}", self.name),
        };
        if !name.is_empty() {
            metadata.set(RESOURCE_NAME, name);
        }
        if let Some(media_type) = &self.media_type {
            metadata.set("Content-Type", media_type.to_string());
        }
        ResourceType::Inline.set(&mut metadata);
        xhtml.parse_embedded(&bytes, &metadata, Wrapping::PackageEntry)
    }
}

/// What ending an open element does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Nothing,
    Styles,
    Property,
    Datum,
    /// Ends a section, in a `div` of the class, if any.
    Section(Option<&'static str>),
    Paragraph,
    Link,
    /// Ends a note or text box, in a `div` of the class.
    Story(&'static str),
    Table,
    Cell,
}

/// Which pages of the document a header or footer section is on.
fn section_class(kind: Option<&str>) -> Result<Option<&'static str>, Failure> {
    match kind {
        None | Some("") => Ok(None),
        Some("header" | "header-even" | "header-first" | "header-last") => Ok(Some("header")),
        Some("footer" | "footer-even" | "footer-first" | "footer-last") => Ok(Some("footer")),
        Some(other) => Err(Failure::Malformed(format!(
            "{WHAT} has a section of the unknown type {other:?}"
        ))),
    }
}

/// The document being read and the document it is written to, as the SAX handler of its XML.
struct Reading<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    open: Vec<Open>,
    /// The name and base of each style, until `<styles>` ends.
    styles: Vec<(String, String)>,
    headings: HashMap<String, usize>,
    lists: HashMap<String, ListKind>,
    /// The stories being written, the innermost last, the document's own first.
    stories: Vec<Story>,
    tables: Vec<Table>,
    property: Option<(Property, String)>,
    datum: Option<Datum>,
    /// Whether a list label was just read, the tab after which is not written.
    after_label: bool,
}

impl Reading<'_, '_> {
    /// Writes the start of the paragraph if none of it is written yet; whether the document is
    /// in a paragraph.
    fn open_paragraph(&mut self) -> Result<bool, Stop> {
        let story = self.stories.last_mut().expect("a story is open");
        story.open(self.xhtml)
    }

    fn start_story(&mut self, class: Option<&'static str>) -> Result<(), Stop> {
        if let Some(class) = class {
            self.xhtml.start_element_with("div", &[("class", class)])?;
        }
        self.stories.push(Story::default());
        Ok(())
    }

    fn end_story(&mut self, class: Option<&'static str>) -> Result<(), Stop> {
        if self.stories.len() > 1 {
            let mut story = self.stories.pop().expect("a story is open");
            story.end_paragraph(self.xhtml)?;
            story.close_lists(self.xhtml)?;
        }
        if class.is_some() {
            self.xhtml.end_element("div")?;
        }
        Ok(())
    }

    /// What the paragraph with `attributes` is written as.
    fn paragraph_kind(&self, attributes: &[Attribute]) -> Kind {
        let style = attribute(attributes, "", "style").unwrap_or("");
        if let Some(level) = self
            .headings
            .get(style)
            .copied()
            .or_else(|| built_in_heading(style))
        {
            return Kind::Heading(level);
        }
        let Some(list) = attribute(attributes, "", "listid").filter(|id| *id != "0") else {
            return Kind::Paragraph;
        };
        let props = attribute(attributes, "", "props").unwrap_or("");
        let list_kind = self.lists.get(list).copied().unwrap_or_else(|| {
            prop(props, "list-style").map_or(ListKind::Unordered, ListKind::of_style)
        });
        if list_kind == ListKind::None {
            return Kind::Paragraph;
        }
        let level = attribute(attributes, "", "level")
            .and_then(|level| level.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, MAX_LIST_DEPTH);
        Kind::Item {
            list: list.to_owned(),
            level,
            list_kind,
        }
    }

    fn start_cell(&mut self, attributes: &[Attribute]) -> Result<(), Failure> {
        let props = attribute(attributes, "", "props").unwrap_or("");
        let top = prop(props, "top-attach")
            .and_then(|top| top.parse::<u64>().ok())
            .ok_or_else(|| {
                Failure::Malformed(format!(
                    "{WHAT} has a table cell without a top-attach: {props:?}"
                ))
            })?;
        let Some(table) = self.tables.last_mut() else {
            return Err(Failure::Malformed(format!(
                "{WHAT} has a table cell outside a table"
            )));
        };
        if table.row.is_some_and(|row| row != top) {
            self.xhtml.end_element("tr")?;
        }
        if table.row != Some(top) {
            self.xhtml.start_element("tr")?;
            table.row = Some(top);
        }
        self.xhtml.start_element("td")?;
        self.stories.push(Story::default());
        Ok(())
    }

    fn line_break(&mut self) -> Result<(), Stop> {
        if self.open_paragraph()? {
            self.xhtml.start_element("br")?;
            self.xhtml.end_element("br")?;
        }
        Ok(())
    }

    fn start(&mut self, local_name: &str, attributes: &[Attribute]) -> Result<Open, Failure> {
        let props = || attribute(attributes, "", "props").unwrap_or("");
        Ok(match local_name {
            "m" => {
                self.property = attribute(attributes, "", "key")
                    .and_then(Property::of)
                    .map(|property| (property, String::new()));
                Open::Property
            }
            "styles" => Open::Styles,
            "s" => {
                if let Some(name) = attribute(attributes, "", "name") {
                    let base = attribute(attributes, "", "basedon").unwrap_or("");
                    self.styles.push((name.to_owned(), base.to_owned()));
                }
                Open::Nothing
            }
            "l" => {
                if let Some(id) = attribute(attributes, "", "id") {
                    let list_kind = attribute(attributes, "", "type")
                        .map_or(ListKind::Unordered, ListKind::of_type);
                    self.lists.insert(id.to_owned(), list_kind);
                }
                Open::Nothing
            }
            "d" => {
                self.datum = Some(Datum::start(attributes));
                Open::Datum
            }
            "section" => {
                let class = section_class(attribute(attributes, "", "type"))?;
                self.start_story(class)?;
                Open::Section(class)
            }
            "p" => {
                let kind = self.paragraph_kind(attributes);
                let story = self.stories.last_mut().expect("a story is open");
                story.end_paragraph(self.xhtml)?;
                story.block = Block::Pending(kind);
                self.after_label = false;
                Open::Paragraph
            }
            "a" => match attribute(attributes, XLINK_NAMESPACE, "href") {
                Some(href) if self.open_paragraph()? => {
                    self.xhtml.start_element_with("a", &[("href", href)])?;
                    Open::Link
                }
                Some(_) | None => Open::Nothing,
            },
            "br" | "cbr" | "pbr" => {
                self.line_break()?;
                Open::Nothing
            }
            "tab" => {
                if self.open_paragraph()? {
                    self.xhtml.characters("\t")?;
                }
                Open::Nothing
            }
            "field" => {
                self.after_label = attribute(attributes, "", "type") == Some("list_label");
                Open::Nothing
            }
            "foot" | "endnote" | "ann" => {
                let class = match local_name {
                    "foot" => "footnote",
                    "endnote" => "endnote",
                    _ => "annotation",
                };
                self.open_paragraph()?;
                self.start_story(Some(class))?;
                Open::Story(class)
            }
            "frame" if prop(props(), "frame-type").is_none_or(|kind| kind == "textbox") => {
                self.start_story(Some("textbox"))?;
                Open::Story("textbox")
            }
            "table" => {
                let story = self.stories.last_mut().expect("a story is open");
                story.end_paragraph(self.xhtml)?;
                story.close_lists(self.xhtml)?;
                self.xhtml.start_element("table")?;
                self.tables.push(Table::default());
                Open::Table
            }
            "cell" => {
                self.start_cell(attributes)?;
                Open::Cell
            }
            _ => Open::Nothing,
        })
    }

    fn end(&mut self, open: Open) -> Result<(), Failure> {
        match open {
            Open::Nothing => {}
            Open::Styles => {
                self.headings = heading_levels(&self.styles);
                self.styles = Vec::new();
            }
            Open::Property => {
                if let Some((property, value)) = self.property.take() {
                    property.set(&value, self.xhtml.metadata_mut());
                }
            }
            Open::Datum => {
                if let Some(datum) = self.datum.take() {
                    datum.embed(self.xhtml)?;
                }
            }
            Open::Section(class) => self.end_story(class)?,
            Open::Paragraph => {
                let story = self.stories.last_mut().expect("a story is open");
                story.end_paragraph(self.xhtml)?;
                self.after_label = false;
            }
            Open::Link => self.xhtml.end_element("a")?,
            Open::Story(class) => self.end_story(Some(class))?,
            Open::Table => {
                if let Some(table) = self.tables.pop() {
                    if table.row.is_some() {
                        self.xhtml.end_element("tr")?;
                    }
                }
                self.xhtml.end_element("table")?;
            }
            Open::Cell => {
                self.end_story(None)?;
                self.xhtml.end_element("td")?;
            }
        }
        Ok(())
    }
}

/// Whether an element in `namespace` is an AWML element.
fn is_awml(namespace: &str) -> bool {
    namespace == AWML_NAMESPACE || namespace.is_empty()
}

impl SaxHandler for Reading<'_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let open = match is_awml(namespace) {
            true => self.start(local_name, attributes)?,
            false => Open::Nothing,
        };
        self.open.push(open);
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        match self.open.pop() {
            Some(open) => self.end(open),
            None => Ok(()),
        }
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if let Some((_, value)) = &mut self.property {
            value.push_str(text);
            return Ok(());
        }
        if let Some(datum) = &mut self.datum {
            datum.text(text);
            return Ok(());
        }
        let text = match std::mem::take(&mut self.after_label) {
            true => text.strip_prefix('\t').unwrap_or(text),
            false => text,
        };
        if text.is_empty() {
            return Ok(());
        }
        if self
            .stories
            .last()
            .is_some_and(|story| matches!(story.block, Block::Pending(_)))
            && text.bytes().all(|byte| byte.is_ascii_whitespace())
        {
            return Ok(());
        }
        if self.open_paragraph()? {
            self.xhtml.characters(text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
