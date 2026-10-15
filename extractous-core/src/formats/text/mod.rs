//! A port of what Tika 4.1's `TextAndCSVParser` decides for plain text, CSV and TSV: the charset
//! it decodes with, whether the text is a table and with which delimiter, the metadata it sets,
//! and the content it writes, as a value ready to be rendered as XHTML.
//!
//! # The decisions
//!
//! Tika reads the input with an [`AutoDetectReader`], told the content type the parser was
//! given. A charset the content type
//! declares is one more report to the detection: the byte order mark's report and Mojibuster's
//! come first, and the JunkFilter arbitrates among them all (see the `declared/` inputs of the
//! reference). Through `AutoDetectParser`, as extractous parses, the content type is the
//! detected type and declares no charset.
//!
//! When the content type is `text/plain`, `text/csv` or `text/tsv`, ignoring parameters, the
//! [`sniffer`] decides whether the text is a table. Any other type, `text/tab-separated-values`
//! among them, is plain text without sniffing.
//!
//! # The metadata
//!
//! - `Content-Type`: for a sniffed type, `text/plain`, `text/csv` or, with a tab, `text/tsv`,
//!   with `charset` the decoding charset and, for a table, `delimiter` its name; for any other
//!   type, the type given, with its parameters and `charset` set;
//! - `Content-Encoding`: the decoding charset;
//! - `tk:detected-encoding`, `tk:encoding-detector`, `tk:encoding-detection-trace`: the
//!   detected charset, the detector that won, and the trace;
//! - `tk:decoded-charset`: the decoding charset, when it is a superset of the detected one;
//! - for a table, `csv:delimiter` (its name), `csv:num-columns` (the first record's values, set
//!   once that record is written; absent without records) and `csv:num-rows` (set at the end).
//!
//! Tika never sets these for extractous otherwise: it reads no user override of the content
//! type and no `TextAndCSVConfig` from the parse context, since extractous passes neither.
//!
//! # The events
//!
//! The parser writes through an `XHTMLContentHandler`, which writes the `html`, `head` (with a
//! `meta` for each metadata value set so far) and `body` elements when the first body element
//! starts, so the head carries every value above except `csv:num-columns` and `csv:num-rows`.
//! Between `startDocument` and `endDocument` it emits, for a [`Body::Paragraph`]:
//!
//! ```text
//! startElement("p")
//! characters(chunk)        for each chunk
//! endElement("p")
//! ```
//!
//! and for a [`Body::Table`]:
//!
//! ```text
//! startElement("table")
//!   startElement("tr")     for each record
//!     startElement("td")   for each value
//!     characters(value)    unless the value is empty
//!     endElement("td")
//!   endElement("tr")
//! endElement("table")
//! ```
//!
//! The `XHTMLContentHandler` replaces characters XML cannot hold with U+FFFD, character by
//! character within each `characters` call; that is the renderer's part, not this port's.
//! TextAndCSVParser's recovery from a malformed table (a `div` of the rest as text) never runs,
//! since Commons CSV reads every text with the format it uses (see [`csv`]).

mod csv;
#[cfg(test)]
mod reference;
mod sniffer;

use sniffer::Delimiter;

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};
use crate::mime::MediaType;
use crate::text::charset::Detection;

/// The UTF-16 units of each read of `handleText`'s buffer, each written as one `characters`
/// event.
const CHUNK: usize = 4096;

/// Tika's `TextAndCSVParser`, for the plain text, CSV and TSV types extractous routes to it.
pub(crate) struct TextAndCsvParser;

impl Parser for TextAndCsvParser {
    fn name(&self) -> &'static str {
        "extractous::TextAndCSVParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["text/plain", "text/csv", "text/tab-separated-values"]
    }

    fn defers(&self, input: &[u8]) -> Option<&'static str> {
        AutoDetectReader::defers(input)
    }

    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let content_type = xhtml
            .metadata_mut()
            .get("Content-Type")
            .ok_or_else(|| Failure::Malformed("the document has no Content-Type".into()))?
            .to_owned();
        read(input, &content_type)?.write(xhtml)
    }
}

/// What TextAndCSVParser makes of an input.
#[derive(Debug, Clone, PartialEq)]
struct Document {
    /// The final `Content-Type`.
    content_type: MediaType,
    /// The charset detection, which the `Content-Encoding` and `tk:` metadata report.
    detection: Detection,
    body: Body,
}

/// The content TextAndCSVParser writes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Body {
    /// One paragraph of the whole decoded text, as `handleText` writes it: one `characters` event
    /// for each read of 4096 UTF-16 units, the last read shorter. A read that ends between the
    /// two halves of a surrogate pair writes a half surrogate at the end of one event and the
    /// other half at the start of the next; each half, which no Rust string holds, is U+FFFD
    /// here, as the `XHTMLContentHandler` writes it.
    Paragraph(Vec<String>),
    /// A table of records of values, one `characters` event for each value.
    Table {
        delimiter: Delimiter,
        records: Vec<Vec<String>>,
    },
}

impl Document {
    /// Sets the metadata and writes the body as TextAndCSVParser does. Of the values Tika sets
    /// more than once, only the last counts: each name keeps the place it was first set at.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let metadata = xhtml.metadata_mut();
        auto_detect_reader::set_metadata(&self.detection, metadata);
        metadata.set("Content-Encoding", self.detection.decode_as.name());
        metadata.set("Content-Type", self.content_type.to_string());
        match &self.body {
            Body::Paragraph(chunks) => {
                xhtml.start_element("p")?;
                for chunk in chunks {
                    xhtml.characters(chunk)?;
                }
                xhtml.end_element("p")?;
            }
            Body::Table { delimiter, records } => {
                metadata.set("csv:delimiter", delimiter.name());
                xhtml.start_element("table")?;
                for (index, record) in records.iter().enumerate() {
                    xhtml.start_element("tr")?;
                    for value in record {
                        xhtml.start_element("td")?;
                        xhtml.characters(value)?;
                        xhtml.end_element("td")?;
                    }
                    xhtml.end_element("tr")?;
                    if index == 0 {
                        xhtml
                            .metadata_mut()
                            .set("csv:num-columns", record.len().to_string());
                    }
                }
                xhtml
                    .metadata_mut()
                    .set("csv:num-rows", records.len().to_string());
                xhtml.end_element("table")?;
            }
        }
        Ok(())
    }
}

/// A `text/` media type without parameters.
fn text_type(subtype: &str) -> MediaType {
    MediaType::named(&format!("text/{subtype}"))
}

/// What TextAndCSVParser makes of `input` when the metadata's `Content-Type` is `content_type`.
fn read(input: &[u8], content_type: &str) -> Result<Document, Failure> {
    let given = MediaType::parse(content_type).ok_or_else(|| {
        Failure::Malformed(format!(
            "Tika's MediaType does not parse the content type {content_type:?}"
        ))
    })?;
    let AutoDetectReader { detection, text } = AutoDetectReader::read(input, content_type)?;
    let text = text.as_str();
    let charset = detection.decode_as;
    let sniffed =
        [text_type("csv"), text_type("tsv"), text_type("plain")].contains(&given.base_type());
    let (content_type, body) = match sniffed.then(|| sniffer::sniff(text)) {
        None => (given, Body::Paragraph(chunks(text))),
        Some(None) => (text_type("plain"), Body::Paragraph(chunks(text))),
        Some(Some(delimiter)) => {
            let table = text_type(if delimiter == Delimiter::Tab {
                "tsv"
            } else {
                "csv"
            });
            let body = Body::Table {
                delimiter,
                records: csv::records(text, delimiter.char()),
            };
            (table.with_parameter("delimiter", delimiter.name()), body)
        }
    };
    Ok(Document {
        content_type: content_type.with_parameter("charset", charset.name()),
        detection,
        body,
    })
}

/// The text as reads of [`CHUNK`] UTF-16 units, a surrogate pair split between two reads written
/// as U+FFFD in each.
fn chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::with_capacity(text.len() / CHUNK + 1);
    let mut chunk = String::new();
    let mut units = 0;
    for c in text.chars() {
        let width = c.len_utf16();
        if units + width > CHUNK {
            chunk.push(char::REPLACEMENT_CHARACTER);
            chunks.push(std::mem::take(&mut chunk));
            chunk.push(char::REPLACEMENT_CHARACTER);
            units = 1;
            continue;
        }
        chunk.push(c);
        units += width;
        if units == CHUNK {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};

    const TIKA_NAME: &str = "org.apache.tika.parser.csv.TextAndCSVParser";

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let (parsed, deferred) = documents(
            &TextAndCsvParser,
            &[
                Path::new("../test_files/documents"),
                Path::new("../quality/corpus"),
                Path::new("../quality/truth"),
            ],
        );
        assert!(deferred.is_empty(), "deferred: {deferred:?}");
        assert!(parsed.len() >= 100, "{} documents", parsed.len());
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
        let (parsed, deferred) = documents(&TextAndCsvParser, &[&conformance_corpus()]);
        assert!(parsed.len() >= 15, "{} documents", parsed.len());
        assert!(deferred.is_empty(), "deferred: {deferred:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    fn defers_charsets_it_cannot_decode() {
        let reason = |input: &[u8]| TextAndCsvParser.defers(input);
        assert_eq!(reason(b"plain text, nothing declared"), None);
        assert_eq!(reason(b"<meta charset=\"utf-8\"> text"), None);
        assert!(reason(b"<meta charset=\"x-IBM930\"> text").is_some());
        assert!(reason(b"\x1b$)C\x0e\x21\x21\x0f korean").is_some());
        assert_eq!(
            reason(b"\x1b$B$3$s$K$A$O\x1b(B world, \x1b$B@$3&\x1b(B"),
            None
        );
    }

    #[test]
    fn splits_reads_as_handle_text_does() {
        assert_eq!(chunks(""), Vec::<String>::new());
        let full = "a".repeat(CHUNK);
        assert_eq!(chunks(&full), std::slice::from_ref(&full));
        let split = chunks(&format!("{}\u{1F600}b", "a".repeat(CHUNK - 1)));
        assert_eq!(
            split,
            [
                format!("{}\u{FFFD}", "a".repeat(CHUNK - 1)),
                "\u{FFFD}b".to_string()
            ]
        );
        let whole = chunks(&format!("{}\u{1F600}b", "a".repeat(CHUNK - 2)));
        assert_eq!(whole[0].chars().last(), Some('\u{1F600}'));
        assert_eq!(whole[1], "b");
    }
}
