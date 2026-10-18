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
//!
//! The document is read twice: once as far as deciding whether it is extracted, every
//! paragraph read and its events let go, then as it is written, its events handed on as they
//! are made.

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

use std::convert::Infallible;

use document::Word;
use extractor::{extract, paragraphs, Out, Sink};

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::Summaries;
use crate::formats::poi::{planned, written, Halt};
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::ole2::Ole2;

const MEDIA_TYPE: &str = "application/msword";

pub struct DocParser;

impl Parser for DocParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        crate::formats::poi::OFFICE_PARSER
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the streams in place, keeping the piece table, the character and paragraph
    /// runs, the style sheet and the other tables POI reads as indexes, and reads the text of
    /// a paragraph or a run from the main stream where it is written; the pictures are handed
    /// on from the data stream and the objects from their storages, as they come. What grows is
    /// what POI's model holds that Tika's output needs: the runs, which a document has about as
    /// many of as paragraphs; the text of the footnotes, comments, endnotes and text boxes,
    /// which Tika's extractor gives a paragraph at a time; an encrypted document's streams,
    /// decrypted whole as POI decrypts them, and a stream whose blocks are scattered, copied
    /// together; and a document before Word 97, read whole as `HWPFOldDocument` reads it.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Fails encrypted documents the password does not open and documents POI or Tika throws
    /// on, and defers macros extracted when asked to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (file, summaries, difference) = plan(input, configs)?;
        unless_different(
            Box::new(move |xhtml| {
                summaries.write(xhtml.metadata_mut());
                xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
                written(read(&file, configs, &mut |out| write(out, xhtml))).map(|_| ())
            }),
            difference,
        )
    }
}

/// The compound document of a Word document and its summary information, the document read as
/// far as writing it, its events let go, and where Tika's extractor writes it otherwise than
/// the native reading, if it does.
fn plan<'a>(
    input: &'a [u8],
    configs: &ParserConfigs,
) -> Result<(Ole2<'a>, Summaries, Option<Difference>), Decline<'static>> {
    let file = office::open(input, configs, MEDIA_TYPE, "Word document")?;
    let difference = planned(read(&file, configs, &mut |_| Ok::<(), Infallible>(())))?;
    let summaries = Summaries::read(&file, file.root())?;
    Ok((file, summaries, difference))
}

/// What Tika's extractor makes of the Word document in a compound document, handed to `sink`
/// as it is made, and where it differs from the native reading, if it does.
fn read<E>(
    file: &Ole2<'_>,
    configs: &ParserConfigs,
    sink: &mut Sink<'_, E>,
) -> Result<Option<Difference>, Halt<E>> {
    let root = file.root();
    match Word::read(file, root, configs)? {
        Word::Modern(streams) => {
            let document = streams.document()?;
            extract(&document, file, root, configs, sink)
        }
        Word::Old(document) => paragraphs(document.paragraph_text(), sink)
            .map(|()| None)
            .map_err(Halt::Written),
    }
}

/// Writes an event of the document as Tika's extractor does while it reads it. Where the
/// output stops taking text, Tika's parse ends there, before the metadata of the document's
/// features.
fn write(out: Out<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
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
        Out::Add(name, value) => xhtml.metadata_mut().add(name, value),
        Out::Set(name, value) => xhtml.metadata_mut().set(name, value),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
