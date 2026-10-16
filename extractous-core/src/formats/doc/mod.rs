//! Word 97–2003 documents (DOC and DOT) as Tika's `OfficeParser` reads them with its
//! `WordExtractor` on POI's HWPF, and as extractous customises both: the summary information as
//! metadata, then the headers, the body with its paragraphs written by style, its tables, list
//! numbers, embedded object markers, hyperlinks and pictures, the text boxes, footnotes,
//! comments and endnotes, the footers, the pictures no run claims and the objects of the object
//! pool, and the comment authors, hidden text and tracked changes as metadata. The text is
//! cleaned of control characters as our `WordText` cleans it, and the other stories' paragraphs
//! are given as our substitute for POI's `getParagraphText` gives them.
//!
//! A document POI reads differently from this port is left to Tika, as is one before Word 97,
//! which POI reads as `HWPFOldDocument`, an encrypted one, and macros extracted when asked to:
//! see [`DocParser::defers`].

mod bins;
mod bytes;
mod document;
mod extractor;
mod fib;
mod fields;
mod lists;
mod pictures;
mod props;
mod range;
mod sprm;
mod styles;
mod text;

use document::Document;
use extractor::{extract, Extracted, Out};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::metadata::office::{
    COMMENT_PERSONS, HAS_COMMENTS, HAS_HIDDEN_TEXT, HAS_TRACK_CHANGES, ORIGINAL_RESOURCE_NAME,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::Summaries;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared, Unported};
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{Ole2, Unreadable};

const MEDIA_TYPE: &str = "application/msword";

pub(crate) struct DocParser;

impl Parser for DocParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Defers documents POI or Tika throws on, those before Word 97, encrypted ones, and
    /// macros extracted when asked to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let (summaries, extracted) =
            read(input, &configs.office).map_err(|Unported(reason)| reason)?;
        Ok(Box::new(move |xhtml| {
            summaries.write(xhtml.metadata_mut());
            write(extracted, xhtml)
        }))
    }
}

/// The summary information and what Tika's extractor makes of the Word document in a compound
/// document.
fn read(input: &[u8], config: &OfficeParserConfig) -> Result<(Summaries, Extracted), Unported> {
    if config.extracts_macros() {
        return Err(Unported(
            "macros are extracted, which this port does not do".to_owned(),
        ));
    }
    let file = Ole2::open(input).map_err(|Unreadable(reason)| {
        Unported(format!("POI cannot open the compound document: {reason}"))
    })?;
    if let Err(Unreadable(reason)) = Ole2::open_stream(input) {
        return Err(Unported(format!(
            "POI reads the compound document from a stream differently: {reason}"
        )));
    }
    let root = file.root();
    if ole_directory_type(&file, root) != MediaType::named(MEDIA_TYPE) {
        return Err(Unported(
            "the compound document is no Word document to POI's container detection".to_owned(),
        ));
    }
    let document = Document::read(&file, root)?;
    let extracted = extract(&document, &file, root, config)?;
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
            Out::Embedded(embedded, wrapping) => {
                xhtml.parse_embedded(&embedded.data, &embedded.metadata, wrapping)?
            }
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
