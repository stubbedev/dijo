//! Lotus Word Pro documents (`.lwp`), which Tika does not read, as LibreOffice's `lwpfilter`
//! reads those of Word Pro 97 and later: the object store ([`objects`]) of the `WordProData`
//! stream, which a small file keeps compressed with the PKWARE library's implode
//! ([`explode`]), and the text of the documents and divisions it holds ([`story`]).
//!
//! The text is written in the order of the divisions: each division's headers, its body, then
//! its footers and the frames placed on its pages, each but the body in a `div` of its class.
//! A table, frame or note is written where its paragraph anchors it, a table as a `table` of
//! its rows and cells, a frame or note in a `div`; a footnote or endnote follows the paragraph
//! that marks it. Tabs, line breaks and hard spaces are kept; fields, page numbers, paragraph
//! numbers, bookmarks and the places of pictures and OLE objects hold no text of their own and
//! are left out, as are blank paragraphs and divisions LibreOffice skips. The text of tracked
//! edits is read as it stands, inserted and deleted runs alike, without their revision marks;
//! the earlier versions a document keeps are not read.
//!
//! The document's description, keywords, author and the times it was made and last revised
//! are read from its document data (`LwpDocData`) as metadata.
//!
//! A document of Word Pro 96 or before (file revision below 0x0B) is not read, as LibreOffice
//! does not read it.

mod explode;
mod objects;
mod story;

use std::borrow::Cow;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::java::time::format_instant;

use objects::{ObjectId, Objects, Unread};
use story::{Block, Text};

const MEDIA_TYPE: &str = "application/vnd.lotus-wordpro";
/// Where the `WordProData` stream starts in the file, after the file's own header, and what an
/// uncompressed stream starts with.
const STREAM_START: usize = 0x10;
const UNCOMPRESSED: &[u8; 4] = b"LWP7";

/// Reads Lotus Word Pro documents.
pub struct WordProParser;

impl Parser for WordProParser {
    fn name(&self) -> &'static str {
        "dijo::WordProParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::WordProParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails a document whose stream does not decompress, and leaves unread one of a file
    /// revision before Word Pro 97's.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let data = stream(input)
            .map_err(|malformed| Decline::Fails(Error::ParseError(malformed.message())))?;
        match objects::readable(&data) {
            Ok(()) => {}
            Err(Unread::Revision(revision)) => {
                return Err(Decline::Unread(format!(
                    "a Word Pro document of file revision {revision:#x}, before Word Pro 97's"
                )))
            }
            Err(Unread::Malformed(malformed)) => {
                return Err(Decline::Fails(Error::ParseError(malformed.message())))
            }
        }
        Ok(Box::new(move |xhtml| {
            xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
            let objects = Objects::read(&data).map_err(Failure::from)?;
            let text = story::read(&objects).map_err(Failure::from)?;
            properties(&objects, &text, xhtml.metadata_mut()).map_err(Failure::from)?;
            write(&text.blocks, xhtml)
        }))
    }
}

/// The `WordProData` stream of the file `input`, with the file's first 16 bytes, at whose end
/// the offsets of its objects start: in place, or, of a small file, decompressed and followed
/// by the rest of the file (`lwpfilter`'s `Decompress`).
fn stream(input: &[u8]) -> Result<Cow<'_, [u8]>, Malformed> {
    let header = input
        .get(..STREAM_START + UNCOMPRESSED.len())
        .ok_or_else(|| {
            Malformed(format!(
                "the file ends at {} bytes, inside its header",
                input.len()
            ))
        })?;
    if &header[STREAM_START..] == UNCOMPRESSED {
        return Ok(Cow::Borrowed(input));
    }
    let compressed = &input[STREAM_START..];
    let limit = usize::try_from(crate::compress::decode_limit(compressed.len()))
        .unwrap_or(usize::MAX)
        .min(crate::compress::MAX_DECODED);
    let (decompressed, used) = explode::explode(compressed, limit)
        .map_err(|malformed| malformed.within("the compressed WordProData stream"))?;
    let rest = &compressed[used..];
    let mut data = Vec::with_capacity(STREAM_START + decompressed.len() + rest.len());
    data.extend_from_slice(&input[..STREAM_START]);
    data.extend_from_slice(&decompressed);
    data.extend_from_slice(rest);
    Ok(Cow::Owned(data))
}

/// Adds the properties of the document data of `text` to `metadata` (`LwpDocData::Read`): of
/// its first object by id, the root document's, which is made before any a division of an
/// older version kept.
fn properties(
    objects: &Objects<'_>,
    text: &Text,
    metadata: &mut TikaMetadata,
) -> Result<(), Malformed> {
    let Some(&id) = text.doc_data.first() else {
        return Ok(());
    };
    let within = |malformed: Malformed| malformed.within(&format!("document data {id}"));
    let properties = doc_data(objects, id).map_err(within)?;
    for (name, value) in [
        ("dc:description", properties.description),
        ("meta:keyword", properties.keywords),
        ("dc:creator", properties.author),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            metadata.set(name, value);
        }
    }
    for (name, seconds) in [
        ("dcterms:created", properties.created),
        ("dcterms:modified", properties.revised),
    ] {
        if seconds > 0 {
            metadata.set(name, format_instant(i64::from(seconds) * 1000));
        }
    }
    Ok(())
}

/// What a document's data says of it.
struct Properties {
    description: Option<String>,
    keywords: Option<String>,
    author: Option<String>,
    /// Seconds since 1970, UTC.
    created: i32,
    revised: i32,
}

/// The properties the document data object `id` holds.
fn doc_data(objects: &Objects<'_>, id: ObjectId) -> Result<Properties, Malformed> {
    let object = objects.indexed(id)?;
    let mut fields = objects.fields(&object.body);
    fields.u16()?;
    for _ in 0..4 {
        fields.atom()?;
    }
    fields.skip(3 * 2)?;
    fields.skip_extra()?;
    Ok(Properties {
        description: fields.atom()?,
        keywords: fields.atom()?,
        author: fields.atom()?,
        created: fields.i32()?,
        revised: fields.i32()?,
    })
}

/// Writes `blocks`: a paragraph each, a table of rows and cells, a division in a `div` of its
/// class.
fn write(blocks: &[Block], xhtml: &mut Xhtml<'_>) -> Result<(), Failure> {
    for block in blocks {
        match block {
            Block::Paragraph(text) => {
                xhtml.start_element("p")?;
                xhtml.characters(text)?;
                xhtml.end_element("p")?;
            }
            Block::Table(rows) => {
                xhtml.start_element("table")?;
                for row in rows {
                    xhtml.start_element("tr")?;
                    for cell in row {
                        xhtml.start_element("td")?;
                        write(cell, xhtml)?;
                        xhtml.end_element("td")?;
                    }
                    xhtml.end_element("tr")?;
                }
                xhtml.end_element("table")?;
            }
            Block::Division(class, blocks) => {
                xhtml.start_element_with("div", &[("class", class.name())])?;
                write(blocks, xhtml)?;
                xhtml.end_element("div")?;
            }
        }
    }
    Ok(())
}

/// Why a Word Pro document cannot be read: what is wrong, where.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Malformed(String);

impl Malformed {
    /// This, read in `context`.
    fn within(self, context: &str) -> Self {
        Self(format!("{context}: {}", self.0))
    }

    fn message(self) -> String {
        format!("Word Pro document: {}", self.0)
    }
}

impl From<Malformed> for Failure {
    fn from(malformed: Malformed) -> Self {
        Failure::Malformed(malformed.message())
    }
}

#[cfg(test)]
mod tests;
