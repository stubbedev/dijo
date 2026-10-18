//! Word 97–2003 documents (DOC and DOT) as Tika's `OfficeParser` reads them with its
//! `WordExtractor` on POI's HWPF, and as extractous customises both: the summary information as
//! metadata, then the headers, the body with its paragraphs written by style, its tables, list
//! numbers, embedded object markers, hyperlinks and pictures, the text boxes, footnotes,
//! comments and endnotes, the footers, the pictures no run claims and the objects of the object
//! pool, and the comment authors, hidden text and tracked changes as metadata. The text is
//! cleaned of control characters as our `WordText` cleans it, and the other stories' paragraphs
//! are given as our substitute for POI's `getParagraphText` gives them.
//!
//! A document before Word 97 is read as POI's `HWPFOldDocument` reads it for Tika's
//! `parseWord6`: a paragraph for each of its paragraphs. An encrypted document is decrypted with
//! the password, or failed as Tika's path fails it where the password does not open it. A
//! document POI reads differently from this port is left to Tika, as are macros extracted when
//! asked to: see [`DocParser::defers`].

mod bins;
mod bytes;
mod document;
mod extractor;
mod fib;
mod fields;
mod lists;
mod old;
mod pictures;
mod props;
mod range;
mod sprm;
mod styles;
mod text;

use document::{Document, Word};
use extractor::{extract, Extracted, Out};

use crate::config::ParserConfigs;
use crate::formats::metadata::office::{
    COMMENT_PERSONS, HAS_COMMENTS, HAS_HIDDEN_TEXT, HAS_TRACK_CHANGES, ORIGINAL_RESOURCE_NAME,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::Summaries;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};

const MEDIA_TYPE: &str = "application/msword";

pub struct DocParser;

impl Parser for DocParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails encrypted documents the password does not open and documents POI or Tika throws
    /// on, and defers macros extracted when asked to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (summaries, extracted) = read(input, configs)?;
        Ok(Box::new(move |xhtml| {
            summaries.write(xhtml.metadata_mut());
            write(extracted, xhtml)
        }))
    }
}

/// The summary information and what Tika's extractor makes of the Word document in a compound
/// document.
fn read(input: &[u8], configs: &ParserConfigs) -> Result<(Summaries, Extracted), Decline<'static>> {
    let file = office::open(input, configs, MEDIA_TYPE, "Word document")?;
    let root = file.root();
    let extracted = match Document::read(&file, root, configs)? {
        Word::Modern(document) => extract(&document, &file, root, configs)?,
        Word::Old(document) => Extracted::paragraphs(document.paragraph_text()),
    };
    Ok((Summaries::read(&file, root)?, extracted))
}

/// Writes the document as Tika's extractor does while it reads it. Where the output stops
/// taking text, Tika's parse ends there, before the metadata of the document's features.
fn write(extracted: Extracted, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let metadata = xhtml.metadata_mut();
    metadata.set("Content-Type", MEDIA_TYPE);
    for location in extracted.save_locations {
        metadata.add(ORIGINAL_RESOURCE_NAME, location);
    }
    for out in extracted.out {
        match out {
            Out::Start(tag) => xhtml.start_element(tag)?,
            Out::StartWith(tag, attributes) => {
                let attributes: Vec<(&str, &str)> = attributes
                    .iter()
                    .map(|(name, value)| (*name, value.as_str()))
                    .collect();
                xhtml.start_element_with(tag, &attributes)?;
            }
            Out::End(tag) => xhtml.end_element(tag)?,
            Out::Text(text) => xhtml.characters_utf16(&text)?,
            Out::Embedded(handed, wrapping) => handed.write(xhtml, wrapping)?,
        }
    }
    let metadata = xhtml.metadata_mut();
    if extracted.has_comments {
        metadata.set(HAS_COMMENTS, "true");
    }
    for author in extracted.revision_authors {
        metadata.add(COMMENT_PERSONS, author);
    }
    if extracted.has_hidden_text {
        metadata.set(HAS_HIDDEN_TEXT, "true");
    }
    if extracted.has_track_changes {
        metadata.set(HAS_TRACK_CHANGES, "true");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
