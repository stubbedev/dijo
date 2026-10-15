//! Tika's `WordExtractor` over the document POI reads, as extractous runs it: the headers, the
//! body's paragraphs, tables, lists, fields, hyperlinks and pictures, the text boxes,
//! footnotes, comments and endnotes as POI's `WordExtractor` gives their paragraphs, the
//! footers, the pictures no run claims, the objects of the object pool, and the metadata of the
//! document's features. Text is cleaned as our `WordText` cleans it, and the paragraphs of the
//! other stories as our substitute for POI's `getParagraphText` gives them.

use super::document::Document;
use super::fib::Story;
use super::fields::Part;
use super::lists::ListManager;
use super::pictures::{has_picture, Pictures};
use super::range::{CharacterRun, Paragraph, Span};
use crate::config::OfficeParserConfig;
use crate::formats::embedded::{Wrapping, RESOURCE_NAME};
use crate::formats::handlers::is_java_whitespace;
use crate::formats::java::{parse_int, JavaHashMap};
use crate::formats::metadata::office::RESOURCE_NAME_EXTENSION_INFERRED;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{office_document, Embedded};
use crate::formats::poi::{Read, Thrown};
use crate::formats::Unported;
use crate::ole2::{EntryId, EntryKind, Ole2};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Out {
    Start(&'static str),
    StartWith(&'static str, Vec<(&'static str, String)>),
    End(&'static str),
    /// Characters, never empty.
    Text(Vec<u16>),
    Embedded(Embedded, Wrapping),
}

/// The output of a document and the metadata of its features.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Extracted {
    /// `extractSavedByMetadata`: the places it was saved to, set before its content.
    pub(super) save_locations: Vec<String>,
    pub(super) out: Vec<Out>,
    pub(super) has_comments: bool,
    /// The authors of its revisions, in the order of Java's `HashSet` of them.
    pub(super) revision_authors: Vec<String>,
    pub(super) has_hidden_text: bool,
    pub(super) has_track_changes: bool,
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
        let level = parse_int(&last).unwrap_or(1).min(6);
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
        .all(|&unit| char::from_u32(u32::from(unit)).is_some_and(is_java_whitespace))
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
                return Err(Thrown(format!(
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

struct Extractor<'d> {
    document: &'d Document,
    file: &'d Ole2<'d>,
    root: EntryId,
    config: &'d OfficeParserConfig,
    pictures: Pictures,
    formatting: Vec<Tag>,
    out: Vec<Out>,
}

/// `WordExtractor.parse(root, xhtml)`.
pub(super) fn extract(
    document: &Document,
    file: &Ole2<'_>,
    root: EntryId,
    config: &OfficeParserConfig,
) -> Result<Extracted, Unported> {
    let save_locations = match &document.save_locations {
        None => Vec::new(),
        Some(locations) => locations
            .iter()
            .map(|location| {
                location.clone().ok_or_else(|| {
                    Unported("a save location POI reads as null, which Tika adds".to_owned())
                })
            })
            .collect::<Result<_, _>>()?,
    };
    let pictures = Pictures::read(document).map_err(super::document::unported)?;
    let mut extractor = Extractor {
        document,
        file,
        root,
        config,
        pictures,
        formatting: Vec::new(),
        out: Vec::new(),
    };
    let has_comments = extractor.body().map_err(super::document::unported)?;
    extractor.object_pool()?;
    let (has_hidden_text, has_track_changes) =
        features(document).map_err(super::document::unported)?;
    Ok(Extracted {
        save_locations,
        out: extractor.out,
        has_comments,
        revision_authors: revision_authors(document)?,
        has_hidden_text,
        has_track_changes,
    })
}

/// `extractFeatures`' authors: the distinct authors of the revisions, in the order Java's
/// `HashSet` of them iterates.
fn revision_authors(document: &Document) -> Result<Vec<String>, Unported> {
    let Some(authors) = &document.revision_authors else {
        return Ok(Vec::new());
    };
    let mut set: JavaHashMap<()> = JavaHashMap::for_hash_set(authors.len());
    for author in authors {
        let author = author.as_deref().ok_or_else(|| {
            Unported("a revision author POI reads as null, which Tika adds".to_owned())
        })?;
        set.insert_if_absent(author, ());
    }
    Ok(set.iter().map(|(author, ())| author.to_owned()).collect())
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

impl Extractor<'_> {
    fn start(&mut self, tag: &'static str) {
        self.out.push(Out::Start(tag));
    }

    fn end(&mut self, tag: &'static str) {
        self.out.push(Out::End(tag));
    }

    fn text(&mut self, text: Vec<u16>) {
        if !text.is_empty() {
            self.out.push(Out::Text(text));
        }
    }

    /// `xhtml.element(name, value)`: nothing for an empty value.
    fn element(&mut self, tag: &'static str, text: Vec<u16>) {
        if text.is_empty() {
            return;
        }
        self.start(tag);
        self.text(text);
        self.end(tag);
    }

    /// The headers, the body, the other stories, the footers and the unclaimed pictures:
    /// whether a comment had text.
    fn body(&mut self) -> Read<bool> {
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
        let mut index = 0;
        while index < document.paragraph_count(main)? {
            let paragraph = document.paragraph(main, index)?;
            index += self.paragraph(&paragraph, 0, main, Part::Main, &mut lists)?;
            index += 1;
        }
        if self.config.includes_shape_based_content() {
            for text in self.story_paragraphs(Story::Textbox)? {
                if !is_blank(&text) {
                    self.element("p", text);
                }
            }
        }
        for text in self.story_paragraphs(Story::Footnote)? {
            if !is_blank(&text) {
                self.element("p", text);
            }
        }
        let mut has_comments = false;
        for text in self.story_paragraphs(Story::Annotation)? {
            if !is_blank(&text) {
                self.element("p", text);
                has_comments = true;
            }
        }
        for text in self.story_paragraphs(Story::Endnote)? {
            if !is_blank(&text) {
                self.element("p", text);
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
        let document = self.document;
        let (start, end) = document.story(story);
        let range = document.range(start, end)?;
        (0..document.paragraph_count(range)?)
            .map(|index| {
                let paragraph = document.paragraph(range, index)?;
                Ok(clean(&strip_fields(document.text_of(paragraph.span)?)?))
            })
            .collect()
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
                    Thrown(format!(
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
    fn header_footer(&mut self, ranges: &[Option<Span>], kind: &'static str) -> Read<()> {
        let document = self.document;
        let mut count = 0;
        for range in ranges.iter().flatten() {
            count += document.paragraph_count(*range)?;
        }
        if count == 0 {
            return Ok(());
        }
        self.out
            .push(Out::StartWith("div", vec![("class", kind.to_owned())]));
        let mut lists = ListManager::new();
        for &range in ranges.iter().flatten() {
            let mut index = 0;
            while index < document.paragraph_count(range)? {
                let paragraph = document.paragraph(range, index)?;
                index += self.paragraph(&paragraph, 0, range, Part::Header, &mut lists)?;
                index += 1;
            }
        }
        self.end("div");
        Ok(())
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
    ) -> Read<usize> {
        let document = self.document;
        if paragraph.is_in_table() && paragraph.table_level() > parent_level && parent_level == 0 {
            let table = document.table(range, paragraph)?;
            self.start("table");
            self.start("tbody");
            for row in &table.rows {
                self.start("tr");
                for &cell in row {
                    self.start("td");
                    for index in 0..document.paragraph_count(cell)? {
                        let inner = document.paragraph(cell, index)?;
                        self.paragraph(&inner, paragraph.table_level(), cell, part, lists)?;
                    }
                    self.end("td");
                }
                self.end("tr");
            }
            self.end("tbody");
            self.end("table");
            return Ok(document.paragraph_count(table.span)? - 1);
        }
        if is_blank_paragraph(document.text_of(paragraph.span)?) {
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
            Some(class) => self
                .out
                .push(Out::StartWith(tag, vec![("class", class.clone())])),
            None => self.start(tag),
        }
        if let Some(numbering) = numbering {
            self.text(numbering.encode_utf16().collect());
        }
        let heading = tas.is_heading();
        let mut index = 0;
        while index < document.run_count(paragraph.span)? {
            let run = document.paragraph_run(paragraph, index)?;
            let text = document.text_of(run.span)?;
            let first = *text
                .first()
                .ok_or_else(|| Thrown::new("Index 0 out of bounds for length 0"))?;
            if first == FIELD_BEGIN {
                if let Some(field) = document.fields.at(part, run.span.start) {
                    if EMBEDDING_FIELDS.contains(&field.kind) {
                        let id = match field.separator {
                            Some(separator) => {
                                let mark =
                                    document.subrange(separator, separator.wrapping_add(1))?;
                                document
                                    .range_run(mark, 0)?
                                    .map(|run| format!("_{}", run.chp.fc_pic))
                            }
                            None => None,
                        };
                        self.out.push(Out::StartWith(
                            "div",
                            vec![
                                ("class", "embedded".to_owned()),
                                ("id", id.unwrap_or_else(|| "_unknown_id".to_owned())),
                            ],
                        ));
                        self.end("div");
                    }
                }
            }
            if text == [FIELD_BEGIN] {
                index += self.special_runs(paragraph, index, heading)?;
            } else if first == PICTURE_MARK {
                for _ in 0..text.len() {
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
        self.close_styles(false);
        self.end(tag);
        Ok(0)
    }

    /// `isRendered`.
    fn is_rendered(&self, run: Option<&CharacterRun>) -> bool {
        run.is_none_or(|run| !run.chp.deleted || self.config.includes_deleted_content())
    }

    /// `handleCharacterRun`.
    fn character_run(&mut self, run: &CharacterRun, skip_styling: bool) -> Read<()> {
        let text = self.document.text_of(run.span)?;
        if !self.is_rendered(Some(run)) || text == [CARRIAGE_RETURN] {
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
            self.ensure_formatting(&desired);
        }
        let text = text.strip_suffix(&[CELL_MARK]).unwrap_or(text);
        let cleaned = clean(text);
        self.text(cleaned);
        Ok(())
    }

    /// `FormattingUtils.ensureFormattingState`: the open tags not wanted closed, from the most
    /// recent, until none is left; then the wanted ones not open opened, in their order.
    fn ensure_formatting(&mut self, desired: &[Tag]) {
        while self.formatting.iter().any(|tag| !desired.contains(tag)) {
            let tag = self.formatting.pop().expect("an open tag");
            self.end(tag.name());
        }
        for &tag in desired {
            if !self.formatting.contains(&tag) {
                self.formatting.push(tag);
                self.start(tag.name());
            }
        }
    }

    /// `closeStyleElements`.
    fn close_styles(&mut self, skip_styling: bool) {
        if !skip_styling {
            self.ensure_formatting(&[]);
        }
    }

    /// `handleSpecialCharacterRuns`: the runs of a field from the run after its begin mark,
    /// written as a link where its code is a hyperlink; how many runs it took.
    fn special_runs(
        &mut self,
        paragraph: &Paragraph,
        index: usize,
        skip_styling: bool,
    ) -> Read<usize> {
        let document = self.document;
        let mut controls: Vec<CharacterRun> = Vec::new();
        let mut texts: Vec<CharacterRun> = Vec::new();
        let mut has_separator = false;
        let mut i = index + 1;
        while i < document.run_count(paragraph.span)? {
            let run = document.paragraph_run(paragraph, i)?;
            let text = document.text_of(run.span)?;
            if text == [FIELD_BEGIN] {
                i += self.special_runs(paragraph, i + 1, skip_styling)?;
            } else if text == [FIELD_SEPARATOR] {
                has_separator = true;
            } else if text == [FIELD_END] {
                if !has_separator {
                    texts = std::mem::take(&mut controls);
                }
                break;
            } else if text == [FIELD_SEPARATOR, FIELD_END] {
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
            code.extend_from_slice(document.text_of(run.span)?);
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
            self.close_styles(skip_styling);
            self.out.push(Out::StartWith("a", vec![("href", url)]));
            for run in &texts {
                self.character_run(run, skip_styling)?;
            }
            self.close_styles(skip_styling);
            self.end("a");
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

    /// `handlePictureCharacterRun`: an image element, and the picture handed on the first time.
    fn picture(&mut self, run: Option<&CharacterRun>, picture: Option<usize>) -> Read<()> {
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
        self.out.push(Out::StartWith(
            "img",
            vec![
                ("src", format!("embedded:{filename}")),
                ("alt", filename.clone()),
            ],
        ));
        self.end("img");
        if self.pictures.first_output(picture) {
            let data = self.pictures.get(picture).content()?;
            let mut metadata = TikaMetadata::new();
            metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
            metadata.set(RESOURCE_NAME, filename);
            metadata.set("Content-Type", kind.media_type());
            self.out
                .push(Out::Embedded(Embedded { data, metadata }, Wrapping::Bare));
        }
        Ok(())
    }

    /// The object pool's storages of embedded objects, each handed on as
    /// `handleEmbeddedOfficeDoc` hands it on.
    fn object_pool(&mut self) -> Result<(), Unported> {
        let Some(pool) = self.file.child(self.root, OBJECT_POOL) else {
            return Ok(());
        };
        if self.file.kind(pool) == EntryKind::Document {
            return Err(Unported(
                "the object pool is a document, which Tika fails to cast".to_owned(),
            ));
        }
        for entry in self.file.children(pool) {
            if !self.file.name(entry).starts_with('_')
                || self.file.kind(entry) == EntryKind::Document
            {
                continue;
            }
            if let Some(embedded) = office_document(self.file, entry, None)? {
                self.out
                    .push(Out::Embedded(embedded, Wrapping::PackageEntry));
            }
        }
        Ok(())
    }
}

/// The element names a paragraph can be written as, so that its tag lives as long as the
/// output: `p`, `pre` and the headings `h0` to `h6`.
fn leak_tag(tag: &str) -> &'static str {
    const TAGS: [&str; 9] = ["p", "pre", "h0", "h1", "h2", "h3", "h4", "h5", "h6"];
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
        assert_eq!(named("Heading 0", false).tag, "h0");
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
