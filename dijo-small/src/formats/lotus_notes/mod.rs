//! Lotus Notes and Domino databases (`.nsf`), which Tika does not read, read for their documents:
//! the data notes, in the order of their note identifiers, each a `<div class="note">` of a
//! definition list of its items, an item's name a `<dt>` and each of its values a `<dd>`, and of
//! its file attachments, handed to the embedded document extractor under their file names.
//!
//! The on-disk structure (ODS) is proprietary. What is read of it ([`ods`]) is what Joachim
//! Metz's documentation of the format and his libnsfdb read, checked against a database Notes 5
//! wrote: the database header, of the title and ODS version given as metadata; the newest
//! superblock and bucket descriptor block of their copies, CX-compressed ([`cx`]); the record
//! relocation vector (RRV) buckets of the data notes, and the summary and non-summary buckets
//! that hold the notes and the items they keep apart. The design notes, forms, views, the access
//! control list and the like, are not written.
//!
//! An item is written by its type ([`note`]): text, which keeps its line breaks, a text list, a
//! number, a time in UTC as ISO 8601, a list or range of numbers or times, and rich text
//! ([`composite`]): its paragraphs, line breaks and tables. Items of the names Notes reserves for
//! itself, those starting with `$`, items of other types (MIME parts, RFC 822 headers, document
//! links, signatures...) and empty items are not written. A deleted note's stub is skipped.
//!
//! What the documentation leaves unknown is declined rather than guessed at: a database of ODS
//! 21 or older (Notes 4 and before), whose RRV buckets are laid out otherwise; a superblock or a
//! bucket descriptor block compressed in any other way than CX; more than one page of summary or
//! non-summary bucket descriptors; a document stored outside the summary buckets; and an
//! attachment compressed (with Huffman coding or LZ1) or kept in its object in parts. A database
//! holding encrypted documents fails as encrypted.

mod composite;
mod cx;
mod note;
mod ods;

use crate::config::ParserConfigs;
use crate::errors::{Error, Unsupported};
use crate::formats::crypt::encrypted_document;
use crate::formats::embedded::{ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use note::{FileObject, Note};
use ods::Database;

const NAME: &str = "dijo::LotusNotesParser";
const MEDIA_TYPE: &str = "application/vnd.lotus-notes";
/// The metadata key of the database's on-disk structure version.
const ODS_VERSION: &str = "nsf:ods-version";

/// The documents of Lotus Notes databases.
pub struct LotusNotesParser;

impl Parser for LotusNotesParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the database's structures where they lie, and holds no more than its descriptor
    /// blocks and one note at a time.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Reads the database's structures and every data note as far as deciding that it reads
    /// them all, before anything is written.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let database = Database::open(input)?;
        database.check()?;
        Ok(Box::new(move |xhtml| write(&database, xhtml)))
    }
}

/// Why a database, or a part of it, is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Problem {
    /// It is not well-formed, as said.
    Malformed(String),
    /// It holds what the port does not read, as said.
    Unread(String),
    /// It holds documents encrypted for their readers, as said.
    Encrypted(String),
}

impl From<Problem> for Decline<'_> {
    fn from(problem: Problem) -> Self {
        match problem {
            Problem::Malformed(reason) => {
                Decline::Fails(Error::ParseError(format!("Lotus Notes database: {reason}")))
            }
            Problem::Unread(reason) => Decline::Unread(reason),
            Problem::Encrypted(what) => Decline::Fails(encrypted_document(&what)),
        }
    }
}

impl From<Problem> for Failure {
    fn from(problem: Problem) -> Self {
        match problem {
            Problem::Malformed(reason) => Failure::Malformed(reason),
            Problem::Unread(reason) => {
                Failure::Extraction(Error::Unsupported(Unsupported::Unread {
                    parser: NAME,
                    reason,
                }))
            }
            Problem::Encrypted(what) => Failure::Extraction(encrypted_document(&what)),
        }
    }
}

/// Writes the metadata of `database` and its data notes in the order of their identifiers.
fn write(database: &Database, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let metadata = xhtml.metadata_mut();
    metadata.set(ODS_VERSION, database.version.to_string());
    if !database.title.is_empty() {
        metadata.set("dc:title", database.title.as_str());
    }
    for note in database.notes() {
        if let Some(note) = note? {
            write_note(database, &note, xhtml)?;
        }
    }
    Ok(())
}

/// Writes `note`, unless it shows nothing: its items that show text in a definition list, the
/// name of a run of items of the same name given once, then its attachments.
fn write_note(database: &Database, note: &Note, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut started = false;
    let mut listed = false;
    let mut last_name: Option<&str> = None;
    for item in note.items(database.names()) {
        let item = item?;
        if !item.is_written() {
            continue;
        }
        start_note(&mut started, xhtml)?;
        if !std::mem::replace(&mut listed, true) {
            xhtml.start_element("dl")?;
        }
        if last_name != Some(item.name.text.as_str()) {
            xhtml.element("dt", &item.name.text)?;
            last_name = Some(item.name.text.as_str());
        }
        item.write(xhtml)?;
    }
    if listed {
        xhtml.end_element("dl")?;
    }
    for item in note.items(database.names()) {
        if let Some(attachment) = item?.attachment()? {
            start_note(&mut started, xhtml)?;
            embed(database, &attachment, xhtml)?;
        }
    }
    if started {
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// Starts the `div` of a note unless it is `started`.
fn start_note(started: &mut bool, xhtml: &mut Xhtml) -> Result<(), Failure> {
    if !std::mem::replace(started, true) {
        xhtml.start_element_with("div", &[("class", "note")])?;
    }
    Ok(())
}

/// Hands the file `attachment` to the embedded document extractor, under its file name.
fn embed(database: &Database, attachment: &FileObject, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let data = database.attachment(attachment)?;
    let mut metadata = TikaMetadata::new();
    if !attachment.name.is_empty() {
        metadata.set(RESOURCE_NAME, attachment.name.as_str());
    }
    ResourceType::Attachment.set(&mut metadata);
    xhtml.parse_embedded(data, &metadata, Wrapping::PackageEntry)
}

/// The byte offset of the file position `position`, kept in units of 256 bytes.
fn offset(position: u32) -> Result<usize, Problem> {
    usize::try_from(u64::from(position) << 8)
        .map_err(|_| Problem::Malformed(format!("the file position {position} lies beyond memory")))
}

/// The `N` bytes at `at` of `bytes`, a field of `what`.
fn field<const N: usize>(bytes: &[u8], at: usize, what: &str) -> Result<[u8; N], Problem> {
    at.checked_add(N)
        .and_then(|end| bytes.get(at..end))
        .and_then(|field| field.try_into().ok())
        .ok_or_else(|| Problem::Malformed(format!("{what} ends before its field at byte {at}")))
}

fn u16_at(bytes: &[u8], at: usize, what: &str) -> Result<u16, Problem> {
    field(bytes, at, what).map(u16::from_le_bytes)
}

fn u32_at(bytes: &[u8], at: usize, what: &str) -> Result<u32, Problem> {
    field(bytes, at, what).map(u32::from_le_bytes)
}

/// The `length` bytes from `at` of `bytes`, which hold `what` there.
fn span<'b>(bytes: &'b [u8], at: usize, length: usize, what: &str) -> Result<&'b [u8], Problem> {
    at.checked_add(length)
        .and_then(|end| bytes.get(at..end))
        .ok_or_else(|| {
            Problem::Malformed(format!(
                "{what} of {length} bytes at {at} runs past the {} bytes it lies in",
                bytes.len()
            ))
        })
}

#[cfg(test)]
mod tests;
