//! Microsoft Works documents, which Tika reads as compound documents of their summary
//! information only: that summary information as metadata, then the text of a word processor
//! document, read as libwps documents the format. Works 3 and 4 for Windows ([`wps4`]) keep
//! their text in the `MN0` stream in a code page; Works 2000 to 9 ([`wps8`]) keep it in the
//! `CONTENTS` stream in UTF-16.
//!
//! The text is written story by story: the header, the body, the text boxes and table cells,
//! the footnotes, the endnotes and the footer, each but the body in a `div` of its class, a
//! paragraph each. Fields (page numbers, dates, the title), footnote marks and the places of
//! pictures and objects hold no text and are left out; empty paragraphs too.
//!
//! A Works database (an `MN0` stream that starts as a database does) and a Works document for
//! the Macintosh are read, as Tika reads them, for their summary information only: libwps reads
//! a database's cells with the spreadsheet formulas and number formats of Works 4, which this
//! parser does not port. Works spreadsheets of Works 5 and later are Excel workbooks
//! (`application/x-tika-msworks-spreadsheet`), which the Excel parser reads. Works files from
//! before Works 3 for Windows, and the spreadsheets of Works 3 and 4, are no compound documents:
//! POI cannot open them, and they fail as they fail Tika's parse.

mod wps4;
mod wps8;

use std::collections::BTreeMap;

use crate::config::ParserConfigs;
use crate::formats::compound::detected_otherwise;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::Summaries;
use crate::formats::poi::Unread;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::ole2::{Content, EntryId, EntryKind, Ole2};

const MEDIA_TYPE: &str = "application/vnd.ms-works";
/// The stream of a Works 3 or 4 document, and the one beside it whose first bytes tell a
/// Macintosh document.
const MN0: &str = "MN0";
const MM: &str = "MM";
/// The stream of a Works 2000 to 9 document.
const CONTENTS: &str = "CONTENTS";
/// What the `MM` stream of a Macintosh document starts with, and the `MN0` stream of a database.
const MACINTOSH: [u8; 2] = [0x44, 0x4E];
const DATABASE: [u8; 2] = [0xFF, 0x54];

pub struct WorksParser;

impl Parser for WorksParser {
    fn name(&self) -> &'static str {
        "dijo::WorksParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::WorksParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails a document POI does not open, as Tika's parse fails; a document of another kind to
    /// POI's container detection goes to that kind's parser, as `OfficeParser` reads it.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        if let Some(parser) = detected_otherwise(input, &[MEDIA_TYPE]) {
            return parser.prepare(input, metadata, configs);
        }
        let file = office::open_file(input).map_err(Unread::declined)?;
        let summaries = Summaries::read(&file, file.root())?;
        Ok(Box::new(move |xhtml| {
            summaries.write(xhtml.metadata_mut());
            xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
            let stories = read(&file).map_err(Failure::from)?;
            write(&stories, xhtml)
        }))
    }
}

/// What a Works document is, by the streams libwps tells it by.
enum Kind {
    /// A word processor document of this format, its text in this stream.
    WordProcessor(Format, EntryId),
    /// A database, a Macintosh document, or a compound document libwps names no Works document.
    SummaryOnly,
}

/// The formats of word processor documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Works 3 and 4, in the `MN0` stream.
    Works4,
    /// Works 2000 to 9, in the `CONTENTS` stream.
    Works8,
}

impl Format {
    /// The name of the stream the text is kept in.
    fn stream(self) -> &'static str {
        match self {
            Format::Works4 => MN0,
            Format::Works8 => CONTENTS,
        }
    }

    /// The stories of the stream the text is kept in.
    fn read(self, stream: &[u8]) -> Result<Stories, Malformed> {
        match self {
            Format::Works4 => wps4::read(stream),
            Format::Works8 => wps8::read(stream),
        }
    }
}

/// The kind of the compound document `file`, as libwps' `WPSHeader::constructHeader` tells it.
fn kind(file: &Ole2<'_>) -> Result<Kind, Malformed> {
    let root = file.root();
    let stream = |name: &str| {
        file.child(root, name)
            .filter(|&entry| file.kind(entry) == EntryKind::Document)
    };
    if let Some(mn0) = stream(MN0) {
        let macintosh = match stream(MM) {
            Some(mm) => starts_with(file, mm, MM, &MACINTOSH)?,
            None => false,
        };
        if macintosh || starts_with(file, mn0, MN0, &DATABASE)? {
            return Ok(Kind::SummaryOnly);
        }
        return Ok(Kind::WordProcessor(Format::Works4, mn0));
    }
    let Some(contents) = stream(CONTENTS) else {
        return Ok(Kind::SummaryOnly);
    };
    for magic in wps8::MAGICS {
        if starts_with(file, contents, CONTENTS, magic)? {
            return Ok(Kind::WordProcessor(Format::Works8, contents));
        }
    }
    Ok(Kind::SummaryOnly)
}

/// The content of the stream `entry`, of this `name`.
fn content<'a>(file: &Ole2<'a>, entry: EntryId, name: &str) -> Result<Content<'a>, Malformed> {
    file.content(entry)
        .map_err(|reason| Malformed(format!("cannot read the stream {name}: {}", reason.0)))
}

/// Whether the stream `entry`, of this `name`, starts with `prefix`.
fn starts_with(
    file: &Ole2<'_>,
    entry: EntryId,
    name: &str,
    prefix: &[u8],
) -> Result<bool, Malformed> {
    Ok(content(file, entry, name)?
        .range(0, prefix.len())
        .as_deref()
        == Some(prefix))
}

/// The stories of the document `file`, of its word processor text; none of a kind whose text
/// is not read.
fn read(file: &Ole2<'_>) -> Result<Stories, Malformed> {
    let Kind::WordProcessor(format, entry) = kind(file)? else {
        return Ok(Stories::default());
    };
    let name = format.stream();
    format
        .read(&content(file, entry, name)?.bytes())
        .map_err(|malformed| malformed.within(name))
}

/// Writes the stories, each but the body in a `div` of its class, a paragraph each.
fn write(stories: &Stories, xhtml: &mut Xhtml<'_>) -> Result<(), Failure> {
    for (story, paragraphs) in &stories.0 {
        if paragraphs.is_empty() {
            continue;
        }
        if let Some(class) = story.class() {
            xhtml.start_element_with("div", &[("class", class)])?;
        }
        for paragraph in paragraphs {
            xhtml.start_element("p")?;
            xhtml.characters(paragraph)?;
            xhtml.end_element("p")?;
        }
        if story.class().is_some() {
            xhtml.end_element("div")?;
        }
    }
    Ok(())
}

/// Why the text of a Works document cannot be read: what is wrong, where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Malformed(String);

impl Malformed {
    /// This, read in `context`.
    fn within(self, context: &str) -> Self {
        Self(format!("{context}: {}", self.0))
    }
}

impl From<Malformed> for Failure {
    fn from(Malformed(reason): Malformed) -> Self {
        Failure::Malformed(format!("Works document: {reason}"))
    }
}

/// The parts of a document its text is kept in, in the order they are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Story {
    Header,
    Body,
    Frame,
    Footnote,
    Endnote,
    Footer,
}

impl Story {
    /// The class of the `div` the story is written in; none for the body.
    fn class(self) -> Option<&'static str> {
        match self {
            Story::Header => Some("header"),
            Story::Body => None,
            Story::Frame => Some("textbox"),
            Story::Footnote => Some("footnote"),
            Story::Endnote => Some("endnote"),
            Story::Footer => Some("footer"),
        }
    }
}

/// The paragraphs of a document's stories, each story's in the order of the document.
#[derive(Debug, Default, PartialEq, Eq)]
struct Stories(BTreeMap<Story, Vec<String>>);

impl Stories {
    /// Adds the paragraphs `text` holds to the end of `story`.
    fn add(&mut self, story: Story, text: Paragraphs) {
        let paragraphs = text.finish();
        if !paragraphs.is_empty() {
            self.0.entry(story).or_default().extend(paragraphs);
        }
    }
}

/// Paragraphs as their characters are read: those that hold more than white space.
#[derive(Debug, Default)]
struct Paragraphs {
    done: Vec<String>,
    current: String,
}

impl Paragraphs {
    fn push(&mut self, character: char) {
        self.current.push(character);
    }

    fn push_str(&mut self, text: &str) {
        self.current.push_str(text);
    }

    /// Ends the paragraph being read.
    fn end(&mut self) {
        if self.current.trim().is_empty() {
            self.current.clear();
        } else {
            self.done.push(std::mem::take(&mut self.current));
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.end();
        self.done
    }
}

/// Little-endian reads of a stream's bytes, each checked to lie in the stream.
#[derive(Clone, Copy)]
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn len(self) -> usize {
        self.0.len()
    }

    /// The `length` bytes at `at`.
    fn slice(self, at: usize, length: usize) -> Result<&'a [u8], Malformed> {
        at.checked_add(length)
            .and_then(|end| self.0.get(at..end))
            .ok_or_else(|| {
                Malformed(format!(
                    "{length} bytes at {at} lie past the end of the stream, at {}",
                    self.0.len()
                ))
            })
    }

    fn array<const N: usize>(self, at: usize) -> Result<[u8; N], Malformed> {
        Ok(self
            .slice(at, N)?
            .try_into()
            .expect("a slice of the array's length"))
    }

    fn u8(self, at: usize) -> Result<u8, Malformed> {
        Ok(self.slice(at, 1)?[0])
    }

    fn u16(self, at: usize) -> Result<u16, Malformed> {
        self.array(at).map(u16::from_le_bytes)
    }

    fn i16(self, at: usize) -> Result<i16, Malformed> {
        self.array(at).map(i16::from_le_bytes)
    }

    fn u32(self, at: usize) -> Result<u32, Malformed> {
        self.array(at).map(u32::from_le_bytes)
    }

    fn i32(self, at: usize) -> Result<i32, Malformed> {
        self.array(at).map(i32::from_le_bytes)
    }
}

#[cfg(test)]
mod tests;
