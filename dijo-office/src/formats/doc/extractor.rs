//! Tika's `WordExtractor` over the document POI reads, as dijo runs it: the headers, the
//! body's paragraphs, tables, lists, fields, hyperlinks and pictures, each text box anchored in
//! the body before the paragraph its anchor is in (where Tika writes them after the body, #111),
//! the other text boxes, footnotes, comments and endnotes as POI's `WordExtractor` gives their
//! paragraphs, the
//! footers, the pictures no run claims, the objects of the object pool, and the metadata of the
//! document's features. Text is cleaned as our `WordText` cleans it, and the paragraphs of the
//! other stories as our substitute for POI's `getParagraphText` gives them.
//!
//! The events are handed on as they are made, the text of a run read from the main stream a
//! piece at a time, a picture handed on from where it is stored.

use std::collections::VecDeque;

use super::document::Document;
use super::fib::Story;
use super::fields::Part;
use super::lists::ListManager;
use super::pictures::{has_picture, Pictures};
use super::range::{CharacterRun, Paragraph, Span};
use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::embedded::{Wrapping, RESOURCE_NAME};
use crate::formats::metadata::office::{
    COMMENT_PERSONS, HAS_COMMENTS, HAS_HIDDEN_TEXT, HAS_TRACK_CHANGES, ORIGINAL_RESOURCE_NAME,
    RESOURCE_NAME_EXTENSION_INFERRED,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{office_document, Embedded, Handed};
use crate::formats::poi::{Halt, Read, Thrown};
use crate::formats::Difference;
use crate::java::parse_int;
use crate::ole2::{EntryId, EntryKind, Node, Ole2};
use crate::unicode::is_whitespace;

const FIELD_BEGIN: u16 = 0x13;
const FIELD_SEPARATOR: u16 = 0x14;
const FIELD_END: u16 = 0x15;
const CELL_MARK: u16 = 0x07;
const PICTURE_MARK: u16 = 0x08;
const CARRIAGE_RETURN: u16 = 0x0D;
const OBJECT_POOL: &str = "ObjectPool";
/// The field types of embedded objects: `EMBED` and `IMPORT`.
const EMBEDDING_FIELDS: [i8; 2] = [58, 56];
/// The indexes of the header and footer stories in the `PlcfHdd`, first, even and odd.
const HEADERS: [usize; 3] = [10, 6, 7];
const FOOTERS: [usize; 3] = [11, 8, 9];

/// What the extractor writes.
#[derive(Debug)]
pub(super) enum Out<'d> {
    Start(&'static str),
    StartWith(&'static str, Vec<(&'static str, String)>),
    End(&'static str),
    /// Characters, never empty.
    Text(Vec<u16>),
    Embedded(Handed<'d>, Wrapping),
    /// A value added to the document's metadata.
    Add(&'static str, String),
    /// A value set in the document's metadata.
    Set(&'static str, String),
}

/// Where the extractor hands its events on, whatever they borrow.
pub(super) type Sink<'s, E> = dyn for<'x> FnMut(Out<'x>) -> Result<(), E> + 's;

/// What extracting the document does next, or why it stops.
type Step<T, E> = Result<T, Halt<E>>;

/// What `parseWord6` writes: an element for each paragraph that is not empty.
pub(super) fn paragraphs<E>(paragraphs: Vec<Vec<u16>>, sink: &mut Sink<'_, E>) -> Result<(), E> {
    for paragraph in paragraphs.into_iter().filter(|text| !text.is_empty()) {
        sink(Out::Start("p"))?;
        sink(Out::Text(paragraph))?;
        sink(Out::End("p"))?;
    }
    Ok(())
}

/// `FormattingUtils.Tag`, in the order of its declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    B,
    I,
    S,
    U,
}

impl Tag {
    const ALL: [Tag; 4] = [Tag::B, Tag::I, Tag::S, Tag::U];

    fn name(self) -> &'static str {
        match self {
            Tag::B => "b",
            Tag::I => "i",
            Tag::S => "s",
            Tag::U => "u",
        }
    }
}

/// `TagAndStyle`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TagAndStyle {
    tag: String,
    class: Option<String>,
}

impl TagAndStyle {
    fn paragraph() -> Self {
        TagAndStyle {
            tag: "p".to_owned(),
            class: None,
        }
    }

    /// `isHeading`.
    fn is_heading(&self) -> bool {
        self.tag.len() == 2 && self.tag.starts_with('h')
    }
}

/// `buildParagraphTagAndStyle`: the element and class a paragraph of a style is written as.
fn tag_and_style(name: &[u16], in_table: bool) -> TagAndStyle {
    if name.len() < 2 {
        return TagAndStyle::paragraph();
    }
    let text = String::from_utf16_lossy(name);
    let fixed = |tag: &str, class: Option<&str>| TagAndStyle {
        tag: tag.to_owned(),
        class: class.map(str::to_owned),
    };
    match text.as_str() {
        "Default" | "Normal" => return TagAndStyle::paragraph(),
        "heading" | "Heading" => return fixed("h1", None),
        "Title" => return fixed("h1", Some("title")),
        "Subtitle" => return fixed("h2", Some("subtitle")),
        "HTML Preformatted" => return fixed("pre", None),
        "Table Contents" if in_table => return TagAndStyle::paragraph(),
        _ => {}
    }
    if text.starts_with("heading") || text.starts_with("Heading") {
        let last = String::from_utf16_lossy(&name[name.len() - 1..]);
        // Tika writes `Heading 0` as `h0`, which XHTML does not have (#111).
        let level = parse_int(&last).unwrap_or(1).clamp(1, 6);
        return TagAndStyle {
            tag: format!("h{level}"),
            class: None,
        };
    }
    let replaced: Vec<u16> = name
        .iter()
        .map(|&unit| {
            if unit == u16::from(b' ') {
                u16::from(b'_')
            } else {
                unit
            }
        })
        .collect();
    let first = String::from_utf16(&replaced[..1])
        .map(|first| first.to_lowercase())
        .unwrap_or_else(|_| String::from_utf16_lossy(&replaced[..1]));
    TagAndStyle {
        tag: "p".to_owned(),
        class: Some(first + &String::from_utf16_lossy(&replaced[1..])),
    }
}

/// `isBlankParagraph`.
fn is_blank_paragraph(text: &[u16]) -> bool {
    text.iter()
        .all(|&unit| matches!(unit, 0x0D | 0x0A | 0x20 | 0x09 | 0x0B | 0x0C))
}

/// `StringUtils.isBlank` over UTF-16 units.
fn is_blank(text: &[u16]) -> bool {
    text.iter()
        .all(|&unit| char::from_u32(u32::from(unit)).is_some_and(is_whitespace))
}

/// Our `WordText.clean`: anchors of pictures, drawings, notes and comments dropped, tabs kept,
/// a non-breaking hyphen and an optional break as their characters, any other control
/// character a line break.
pub(super) fn clean(text: &[u16]) -> Vec<u16> {
    text.iter()
        .filter_map(|&unit| match unit {
            0x01..=0x05 | 0x08 => None,
            0x09 => Some(0x09),
            0x1E => Some(0x2011),
            0x1F => Some(0x200B),
            unit if unit < 0x20 => Some(u16::from(b'\n')),
            unit => Some(unit),
        })
        .collect()
}

fn index_of(text: &[u16], unit: u16, from: usize) -> Option<usize> {
    text.get(from..)?
        .iter()
        .position(|&found| found == unit)
        .map(|at| at + from)
}

fn last_index_of(text: &[u16], unit: u16) -> Option<usize> {
    text.iter().rposition(|&found| found == unit)
}

/// `Range.stripFields`: each field's code dropped and its result kept, failing where Java's
/// substring does.
pub(super) fn strip_fields(text: &[u16]) -> Read<Vec<u16>> {
    let mut text = text.to_vec();
    if index_of(&text, FIELD_BEGIN, 0).is_none() {
        return Ok(text);
    }
    while let (Some(first13), Some(_)) = (
        index_of(&text, FIELD_BEGIN, 0),
        index_of(&text, FIELD_END, 0),
    ) {
        let next13 = index_of(&text, FIELD_BEGIN, first13 + 1);
        let first14 = index_of(&text, FIELD_SEPARATOR, first13 + 1);
        let last15 = last_index_of(&text, FIELD_END).expect("an end mark");
        if last15 < first13 {
            break;
        }
        if next13.is_none() && first14.is_none() {
            text.drain(first13..=last15);
            break;
        }
        if let Some(first14) = first14.filter(|&first14| next13.is_none_or(|next| first14 < next)) {
            if first14 + 1 > last15 {
                return Err(Thrown::new(format!(
                    "begin {}, end {last15}, length {}",
                    first14 + 1,
                    text.len()
                )));
            }
            let mut stripped = text[..first13].to_vec();
            stripped.extend_from_slice(&text[first14 + 1..last15]);
            stripped.extend_from_slice(&text[last15 + 1..]);
            text = stripped;
            continue;
        }
        text.drain(first13..=last15);
    }
    Ok(text)
}

/// `findHyperlinkEnd`.
fn hyperlink_end(text: &[u16], start: usize) -> usize {
    for mark in [u16::from(b'"'), 0x201D, CARRIAGE_RETURN] {
        if let Some(end) = last_index_of(text, mark).filter(|&end| end > start) {
            return end;
        }
    }
    text.len()
}

/// The paragraphs of the text box story: those of each text box anchored in the main text,
/// with the position of its anchor, in the order of the anchors, and the others.
#[derive(Default)]
struct Textboxes {
    anchored: VecDeque<(i32, Vec<Vec<u16>>)>,
    unanchored: Vec<Vec<u16>>,
}
struct Extractor<'s, 'd, E> {
    document: &'s Document<'d>,
    file: &'s Ole2<'d>,
    root: EntryId,
    config: &'s OfficeParserConfig,
    configs: &'s ParserConfigs,
    pictures: Pictures<'d>,
    formatting: Vec<Tag>,
    sink: &'s mut Sink<'s, E>,
}

/// `WordExtractor.parse(root, xhtml)`: its events handed to `sink` as they are made, the
/// metadata of the document's features after them; and where they differ from Tika's, if
/// they do.
pub(super) fn extract<'s, 'd, E>(
    document: &'s Document<'d>,
    file: &'s Ole2<'d>,
    root: EntryId,
    configs: &'s ParserConfigs,
    sink: &'s mut Sink<'s, E>,
) -> Step<Option<Difference>, E> {
    let save_locations: Vec<String> = document
        .save_locations
        .iter()
        .flatten()
        .flatten()
        .cloned()
        .collect();
    let pictures = Pictures::read(document)?;
    let mut extractor = Extractor {
        document,
        file,
        root,
        config: &configs.office,
        configs,
        pictures,
        formatting: Vec::new(),
        sink,
    };
    for location in save_locations {
        extractor.emit(Out::Add(ORIGINAL_RESOURCE_NAME, location))?;
    }
    let has_comments = extractor.body()?;
    let difference = extractor.object_pool()?;
    let (has_hidden_text, has_track_changes) = features(document)?;
    let revision_authors = revision_authors(document);
    if has_comments {
        extractor.emit(Out::Set(HAS_COMMENTS, "true".to_owned()))?;
    }
    for author in revision_authors {
        extractor.emit(Out::Add(COMMENT_PERSONS, author))?;
    }
    if has_hidden_text {
        extractor.emit(Out::Set(HAS_HIDDEN_TEXT, "true".to_owned()))?;
    }
    if has_track_changes {
        extractor.emit(Out::Set(HAS_TRACK_CHANGES, "true".to_owned()))?;
    }
    Ok(difference)
}

/// `extractFeatures`' authors: the distinct authors of the revisions, in the order the
/// document lists them, where Tika gives them in the order of Java's `HashSet` of them (#111).
/// An author POI reads as null Tika adds as `Metadata.add` adds null: as nothing.
fn revision_authors(document: &Document) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::new();
    for author in document.revision_authors.iter().flatten().flatten() {
        if !distinct.iter().any(|known| known == author) {
            distinct.push(author.clone());
        }
    }
    distinct
}

/// `extractFeatures`' runs: whether any run of the main text is hidden, and whether any is a
/// tracked change.
fn features(document: &Document) -> Read<(bool, bool)> {
    let (start, end) = document.story(Story::Main);
    let range = document.range(start, end)?;
    let (mut hidden, mut tracked) = (false, false);
    for index in 0..document.run_count(range)? {
        let run = document
            .range_run(range, index)?
            .ok_or_else(|| Thrown::new("a run of the main text without a paragraph"))?;
        hidden |= run.chp.vanish || run.chp.field_vanish;
        tracked |= run.chp.deleted || run.chp.inserted;
    }
    Ok((hidden, tracked))
}

impl<'d, E> Extractor<'_, 'd, E> {
    /// Hands an event on.
    fn emit(&mut self, out: Out<'d>) -> Step<(), E> {
        (self.sink)(out).map_err(Halt::Written)
    }

    fn start(&mut self, tag: &'static str) -> Step<(), E> {
        self.emit(Out::Start(tag))
    }

    fn end(&mut self, tag: &'static str) -> Step<(), E> {
        self.emit(Out::End(tag))
    }

    fn text(&mut self, text: Vec<u16>) -> Step<(), E> {
        if text.is_empty() {
            return Ok(());
        }
        self.emit(Out::Text(text))
    }

    /// `xhtml.element(name, value)`: nothing for an empty value.
    fn element(&mut self, tag: &'static str, text: Vec<u16>) -> Step<(), E> {
        if text.is_empty() {
            return Ok(());
        }
        self.start(tag)?;
        self.text(text)?;
        self.end(tag)
    }

    /// The headers, the body, the other stories, the footers and the unclaimed pictures:
    /// whether a comment had text.
    fn body(&mut self) -> Step<bool, E> {
        let document = self.document;
        let headers = if self.config.includes_headers_and_footers() {
            Some(document.header_plex()?)
        } else {
            None
        };
        if let Some(plex) = &headers {
            let ranges = self.header_ranges(plex.as_ref(), &HEADERS)?;
            self.header_footer(&ranges, "header")?;
        }
        let (start, end) = document.story(Story::Main);
        let main = document.range(start, end)?;
        let mut lists = ListManager::new();
        let mut textboxes = if self.config.includes_shape_based_content() {
            self.textboxes()?
        } else {
            Textboxes::default()
        };
        let mut index = 0;
        while index < document.paragraph_count(main)? {
            let paragraph = document.paragraph(main, index)?;
            self.anchored_textboxes(&mut textboxes, paragraph.span.end)?;
            index += self.paragraph(&paragraph, 0, main, Part::Main, &mut lists)?;
            index += 1;
        }
        self.anchored_textboxes(&mut textboxes, i32::MAX)?;
        self.textbox_paragraphs(textboxes.unanchored)?;
        for text in self.story_paragraphs(Story::Footnote)? {
            if !is_blank(&text) {
                self.element("p", text)?;
            }
        }
        let mut has_comments = false;
        for text in self.story_paragraphs(Story::Annotation)? {
            if !is_blank(&text) {
                self.element("p", text)?;
                has_comments = true;
            }
        }
        for text in self.story_paragraphs(Story::Endnote)? {
            if !is_blank(&text) {
                self.element("p", text)?;
            }
        }
        if let Some(plex) = &headers {
            let ranges = self.header_ranges(plex.as_ref(), &FOOTERS)?;
            self.header_footer(&ranges, "footer")?;
        }
        while let Some(picture) = self.pictures.next_unclaimed() {
            self.picture(None, Some(picture))?;
        }
        Ok(has_comments)
    }

    /// Our `WordExtractor.getParagraphText(range)` of a story: each paragraph's text with its
    /// fields' codes stripped, cleaned.
    fn story_paragraphs(&self, story: Story) -> Read<Vec<Vec<u16>>> {
        Ok(self
            .story_paragraph_spans(story)?
            .into_iter()
            .map(|(_, text)| text)
            .collect())
    }

    /// [`Extractor::story_paragraphs`], each paragraph with its range of the text.
    fn story_paragraph_spans(&self, story: Story) -> Read<Vec<(Span, Vec<u16>)>> {
        let document = self.document;
        let (start, end) = document.story(story);
        let range = document.range(start, end)?;
        (0..document.paragraph_count(range)?)
            .map(|index| {
                let paragraph = document.paragraph(range, index)?;
                let text = clean(&strip_fields(&document.text_of(paragraph.span)?)?);
                Ok((paragraph.span, text))
            })
            .collect()
    }

    /// The paragraphs of the text box story: those of each text box anchored in the main text,
    /// by its anchor, and the others in order.
    fn textboxes(&self) -> Read<Textboxes> {
        let mut paragraphs: Vec<Option<(Span, Vec<u16>)>> = self
            .story_paragraph_spans(Story::Textbox)?
            .into_iter()
            .map(Some)
            .collect();
        let mut anchored = VecDeque::new();
        for (anchor, textbox) in self.document.anchored_textboxes()? {
            let inside: Vec<Vec<u16>> = paragraphs
                .iter_mut()
                .filter(|paragraph| {
                    paragraph.as_ref().is_some_and(|(span, _)| {
                        span.start >= textbox.start && span.start < textbox.end
                    })
                })
                .filter_map(|paragraph| paragraph.take().map(|(_, text)| text))
                .collect();
            anchored.push_back((anchor, inside));
        }
        Ok(Textboxes {
            anchored,
            unanchored: paragraphs
                .into_iter()
                .flatten()
                .map(|(_, text)| text)
                .collect(),
        })
    }

    /// The paragraphs of each text box anchored before `end`, before the paragraph that ends
    /// there, where Tika writes every text box after the main text (#111).
    fn anchored_textboxes(&mut self, textboxes: &mut Textboxes, end: i32) -> Step<(), E> {
        while let Some((anchor, _)) = textboxes.anchored.front() {
            if *anchor >= end {
                break;
            }
            if let Some((_, paragraphs)) = textboxes.anchored.pop_front() {
                self.textbox_paragraphs(paragraphs)?;
            }
        }
        Ok(())
    }

    /// Each paragraph of a text box that is not blank.
    fn textbox_paragraphs(&mut self, paragraphs: Vec<Vec<u16>>) -> Step<(), E> {
        for text in paragraphs {
            if !is_blank(&text) {
                self.element("p", text)?;
            }
        }
        Ok(())
    }

    /// `HeaderStories.getSubrangeAt` of each index: the stories with text, within the header
    /// story.
    fn header_ranges(
        &self,
        plex: Option<&super::text::Plex>,
        indexes: &[usize],
    ) -> Read<Vec<Option<Span>>> {
        let document = self.document;
        let Some(plex) = plex else {
            return Ok(vec![None; indexes.len()]);
        };
        let (story_start, story_end) = document.story(Story::Header);
        let stories = document.range(story_start, story_end)?;
        indexes
            .iter()
            .map(|&index| {
                let (start, end, _) = plex.entries.get(index).ok_or_else(|| {
                    Thrown::new(format!(
                        "Index {index} out of bounds for length {}",
                        plex.entries.len()
                    ))
                })?;
                if start >= end {
                    return Ok(None);
                }
                let length = stories.end - stories.start;
                let (start, end) = ((*start).min(length), (*end).min(length));
                document
                    .subrange(stories.start + start, stories.start + end)
                    .map(Some)
            })
            .collect()
    }

    /// `handleHeaderFooter`.
    fn header_footer(&mut self, ranges: &[Option<Span>], kind: &'static str) -> Step<(), E> {
        let document = self.document;
        let mut count = 0;
        for range in ranges.iter().flatten() {
            count += document.paragraph_count(*range)?;
        }
        if count == 0 {
            return Ok(());
        }
        self.emit(Out::StartWith("div", vec![("class", kind.to_owned())]))?;
        let mut lists = ListManager::new();
        for &range in ranges.iter().flatten() {
            let mut index = 0;
            while index < document.paragraph_count(range)? {
                let paragraph = document.paragraph(range, index)?;
                index += self.paragraph(&paragraph, 0, range, Part::Header, &mut lists)?;
                index += 1;
            }
        }
        self.end("div")
    }

    /// `isBlankParagraph` of a paragraph's text, read a piece at a time as far as the first
    /// character that is not blank.
    fn is_blank_paragraph(&self, span: Span) -> Read<bool> {
        let mut blank = true;
        self.document.chunks_of(span, |units| {
            blank = is_blank_paragraph(&units);
            Ok(blank)
        })?;
        Ok(blank)
    }

    /// `handleParagraph`: a table, a paragraph of runs, or nothing for a blank paragraph; the
    /// paragraphs past this one it took in.
    fn paragraph(
        &mut self,
        paragraph: &Paragraph,
        parent_level: i32,
        range: Span,
        part: Part,
        lists: &mut ListManager,
    ) -> Step<usize, E> {
        let document = self.document;
        if paragraph.is_in_table() && paragraph.table_level() > parent_level && parent_level == 0 {
            let table = document.table(range, paragraph)?;
            self.start("table")?;
            self.start("tbody")?;
            for row in &table.rows {
                self.start("tr")?;
                for &cell in row {
                    self.start("td")?;
                    for index in 0..document.paragraph_count(cell)? {
                        let inner = document.paragraph(cell, index)?;
                        self.paragraph(&inner, paragraph.table_level(), cell, part, lists)?;
                    }
                    self.end("td")?;
                }
                self.end("tr")?;
            }
            self.end("tbody")?;
            self.end("table")?;
            return Ok(document.paragraph_count(table.span)? - 1);
        }
        if self.is_blank_paragraph(paragraph.span)? {
            return Ok(0);
        }
        let mut numbering = None;
        let tas = if (document.styles.len() as i64) > i64::from(paragraph.istd) {
            match document.styles.style(i32::from(paragraph.istd))? {
                Some(style) if !style.name.is_empty() => {
                    if paragraph.is_in_list() {
                        numbering = Some(lists.formatted_number(
                            document.lists.as_ref(),
                            paragraph.ilfo(),
                            paragraph.ilvl(),
                        )?);
                    }
                    tag_and_style(&style.name, parent_level > 0)
                }
                _ => TagAndStyle::paragraph(),
            }
        } else {
            TagAndStyle::paragraph()
        };
        let tag = leak_tag(&tas.tag);
        match &tas.class {
            Some(class) => self.emit(Out::StartWith(tag, vec![("class", class.clone())]))?,
            None => self.start(tag)?,
        }
        if let Some(numbering) = numbering {
            self.text(numbering.encode_utf16().collect())?;
        }
        let heading = tas.is_heading();
        let mut index = 0;
        while index < document.run_count(paragraph.span)? {
            let run = document.paragraph_run(paragraph, index)?;
            document.check_span(run.span)?;
            let length = (run.span.end - run.span.start) as usize;
            if length == 0 {
                return Err(Thrown::new("Index 0 out of bounds for length 0").into());
            }
            let first = document.text_of(Span {
                start: run.span.start,
                end: run.span.start + 1,
            })?[0];
            if first == FIELD_BEGIN {
                // The fields of the headers count from the start of their story; Tika looks
                // them up by the position in the whole text (#111).
                let base = match part {
                    Part::Header => document.story(Story::Header).0,
                    _ => 0,
                };
                if let Some(field) = document.fields.at(part, run.span.start - base) {
                    if EMBEDDING_FIELDS.contains(&field.kind) {
                        let id = match field.separator {
                            Some(separator) => {
                                let separator = separator.wrapping_add(base);
                                let mark =
                                    document.subrange(separator, separator.wrapping_add(1))?;
                                document
                                    .range_run(mark, 0)?
                                    .map(|run| format!("_{}", run.chp.fc_pic))
                            }
                            None => None,
                        };
                        self.emit(Out::StartWith(
                            "div",
                            vec![
                                ("class", "embedded".to_owned()),
                                ("id", id.unwrap_or_else(|| "_unknown_id".to_owned())),
                            ],
                        ))?;
                        self.end("div")?;
                    }
                }
            }
            if first == FIELD_BEGIN && length == 1 {
                index += self.special_runs(paragraph, index, heading)?;
            } else if first == PICTURE_MARK {
                for _ in 0..length {
                    let picture = self.pictures.next_unclaimed();
                    self.picture(Some(&run), picture)?;
                }
            } else if has_picture(document, &run)? {
                let picture = self.pictures.picture_of(&run);
                self.picture(Some(&run), picture)?;
            } else {
                self.character_run(&run, heading)?;
            }
            index += 1;
        }
        self.close_styles(false)?;
        self.end(tag)?;
        Ok(0)
    }

    /// `isRendered`.
    fn is_rendered(&self, run: Option<&CharacterRun>) -> bool {
        run.is_none_or(|run| !run.chp.deleted || self.config.includes_deleted_content())
    }

    /// `handleCharacterRun`: its text, but a closing cell mark, cleaned and written a piece at
    /// a time as it is read.
    fn character_run(&mut self, run: &CharacterRun, skip_styling: bool) -> Step<(), E> {
        let document = self.document;
        if document.text_is(run.span, &[CARRIAGE_RETURN])? || !self.is_rendered(Some(run)) {
            return Ok(());
        }
        if !skip_styling {
            let chp = &run.chp;
            let desired: Vec<Tag> = Tag::ALL
                .into_iter()
                .filter(|tag| match tag {
                    Tag::B => chp.bold,
                    Tag::I => chp.italic,
                    Tag::S => chp.strike,
                    Tag::U => chp.kul != 0,
                })
                .collect();
            self.ensure_formatting(&desired)?;
        }
        let mut span = run.span;
        if span.end > span.start
            && document.text_of(Span {
                start: span.end - 1,
                end: span.end,
            })? == [CELL_MARK]
        {
            span.end -= 1;
        }
        let mut written = Ok(());
        document.chunks_of(span, |units| {
            written = self.text(clean(&units));
            Ok(written.is_ok())
        })?;
        written
    }

    /// `FormattingUtils.ensureFormattingState`: the open tags not wanted closed, from the most
    /// recent, until none is left; then the wanted ones not open opened, in their order.
    fn ensure_formatting(&mut self, desired: &[Tag]) -> Step<(), E> {
        while self.formatting.iter().any(|tag| !desired.contains(tag)) {
            let tag = self.formatting.pop().expect("an open tag");
            self.end(tag.name())?;
        }
        for &tag in desired {
            if !self.formatting.contains(&tag) {
                self.formatting.push(tag);
                self.start(tag.name())?;
            }
        }
        Ok(())
    }

    /// `closeStyleElements`.
    fn close_styles(&mut self, skip_styling: bool) -> Step<(), E> {
        if !skip_styling {
            self.ensure_formatting(&[])?;
        }
        Ok(())
    }

    /// `handleSpecialCharacterRuns`: the runs of a field from the run after its begin mark,
    /// written as a link where its code is a hyperlink; how many runs it took.
    fn special_runs(
        &mut self,
        paragraph: &Paragraph,
        index: usize,
        skip_styling: bool,
    ) -> Step<usize, E> {
        let document = self.document;
        let mut controls: Vec<CharacterRun> = Vec::new();
        let mut texts: Vec<CharacterRun> = Vec::new();
        let mut has_separator = false;
        let mut i = index + 1;
        while i < document.run_count(paragraph.span)? {
            let run = document.paragraph_run(paragraph, i)?;
            if document.text_is(run.span, &[FIELD_BEGIN])? {
                // Tika hands on the run after the nested field's begin mark as if it were the
                // mark, skipping a run, and ends this field at the nested one's end (#111).
                i += self.special_runs(paragraph, i, skip_styling)?;
            } else if document.text_is(run.span, &[FIELD_SEPARATOR])? {
                has_separator = true;
            } else if document.text_is(run.span, &[FIELD_END])? {
                if !has_separator {
                    texts = std::mem::take(&mut controls);
                }
                break;
            } else if document.text_is(run.span, &[FIELD_SEPARATOR, FIELD_END])? {
                has_separator = true;
            } else if has_separator {
                texts.push(run);
            } else {
                controls.push(run);
            }
            i += 1;
        }
        if controls.is_empty() {
            for run in &texts {
                self.character_run(run, skip_styling)?;
            }
            return Ok(i - index);
        }
        let mut code: Vec<u16> = Vec::new();
        for run in &controls {
            code.extend(document.text_of(run.span)?);
        }
        let quote = u16::from(b'"');
        let hyperlink: Vec<u16> = "HYPERLINK".encode_utf16().collect();
        let spaced: Vec<u16> = " HYPERLINK".encode_utf16().collect();
        let is_link = (code.starts_with(&hyperlink) || code.starts_with(&spaced))
            && index_of(&code, quote, 0).is_some();
        if is_link {
            let start = index_of(&code, quote, 0).expect("a quote") + 1;
            let end = hyperlink_end(&code, start);
            let url = if start < end && end <= code.len() {
                String::from_utf16_lossy(&code[start..end])
            } else {
                String::new()
            };
            self.close_styles(skip_styling)?;
            self.emit(Out::StartWith("a", vec![("href", url)]))?;
            for run in &texts {
                self.character_run(run, skip_styling)?;
            }
            self.close_styles(skip_styling)?;
            self.end("a")?;
        } else {
            for run in &texts {
                if has_picture(document, run)? {
                    let picture = self.pictures.picture_of(run);
                    self.picture(Some(run), picture)?;
                } else {
                    self.character_run(run, skip_styling)?;
                }
            }
        }
        Ok(i - index)
    }

    /// `handlePictureCharacterRun`: an image element, and the picture handed on the first time,
    /// from where it is stored.
    fn picture(&mut self, run: Option<&CharacterRun>, picture: Option<usize>) -> Step<(), E> {
        let Some(picture) = picture else {
            return Ok(());
        };
        if !self.is_rendered(run) {
            return Ok(());
        }
        let kind = self.pictures.get(picture).kind();
        let extension = kind.extension();
        let mut filename = format!("image-{}", self.pictures.number(picture));
        if !extension.is_empty() {
            filename.push('.');
            filename.push_str(extension);
        }
        self.emit(Out::StartWith(
            "img",
            vec![
                ("src", format!("embedded:{filename}")),
                ("alt", filename.clone()),
            ],
        ))?;
        self.end("img")?;
        if self.pictures.first_output(picture) {
            let data = self.pictures.get(picture).content()?;
            let mut metadata = TikaMetadata::new();
            metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
            metadata.set(RESOURCE_NAME, filename);
            metadata.set("Content-Type", kind.media_type());
            self.emit(Out::Embedded(
                Handed::Document(Embedded {
                    data: Node::from(data),
                    metadata,
                }),
                Wrapping::Bare,
            ))?;
        }
        Ok(())
    }

    /// The object pool's storages of embedded objects, each handed on as
    /// `handleEmbeddedOfficeDoc` hands it on, its streams in place; and where they differ from
    /// Tika's, if they do.
    fn object_pool(&mut self) -> Step<Option<Difference>, E> {
        let file = self.file;
        let Some(pool) = file.child(self.root, OBJECT_POOL) else {
            return Ok(None);
        };
        let mut difference = None;
        if file.kind(pool) == EntryKind::Document {
            return Err(Thrown::new(
                "java.lang.ClassCastException: the object pool is a document, which Tika casts to \
                 a directory",
            )
            .into());
        }
        for entry in file.children(pool) {
            if !file.name(entry).starts_with('_') || file.kind(entry) == EntryKind::Document {
                continue;
            }
            if let Some(embedded) =
                office_document(file, entry, TikaMetadata::new(), None, self.configs)?
            {
                difference = difference.or_else(|| embedded.difference());
                self.emit(Out::Embedded(embedded, Wrapping::PackageEntry))?;
            }
        }
        Ok(difference)
    }
}

/// The element names a paragraph can be written as, so that its tag lives as long as the
/// output: `p`, `pre` and the headings `h1` to `h6`.
fn leak_tag(tag: &str) -> &'static str {
    const TAGS: [&str; 8] = ["p", "pre", "h1", "h2", "h3", "h4", "h5", "h6"];
    TAGS.into_iter()
        .find(|&known| known == tag)
        .expect("a paragraph tag")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn cleans_text_as_word_text_does() {
        assert_eq!(
            clean(&units("a\u{1}b\tc\u{1e}d\u{1f}e\u{b}f\u{7}")),
            units("ab\tc\u{2011}d\u{200b}e\nf\n")
        );
    }

    #[test]
    fn strips_field_codes_as_poi_does() {
        let field = units("x\u{13} HYPERLINK \"u\" \u{14}link\u{15}y");
        assert_eq!(strip_fields(&field).unwrap(), units("xlinky"));
        assert_eq!(
            strip_fields(&units("a\u{13}code\u{15}b")).unwrap(),
            units("ab")
        );
        assert!(strip_fields(&units("\u{13}a\u{15}b\u{14}")).is_err());
    }

    #[test]
    fn names_paragraph_styles_as_tika_does() {
        let named = |name: &str, in_table: bool| tag_and_style(&units(name), in_table);
        assert_eq!(named("heading 3", false).tag, "h3");
        assert_eq!(named("Heading 9", false).tag, "h6");
        assert_eq!(named("Heading 0", false).tag, "h1");
        assert_eq!(named("Heading X", false).tag, "h1");
        assert_eq!(
            named("List Bullet", false).class.as_deref(),
            Some("list_Bullet")
        );
        assert_eq!(named("Table Contents", true), TagAndStyle::paragraph());
        assert_eq!(
            named("Table Contents", false).class.as_deref(),
            Some("table_Contents")
        );
        assert_eq!(named("X", false), TagAndStyle::paragraph());
    }

    #[test]
    fn finds_the_end_of_a_hyperlink_as_tika_does() {
        assert_eq!(hyperlink_end(&units("HYPERLINK \"a\" "), 11), 12);
        assert_eq!(hyperlink_end(&units("HYPERLINK \"a\u{201d}"), 11), 12);
        assert_eq!(hyperlink_end(&units("HYPERLINK \"ab"), 11), 13);
    }
}
