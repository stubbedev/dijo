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
//! Between `startDocument` and `endDocument` it emits, for plain text:
//!
//! ```text
//! startElement("p")
//! characters(chunk)        for each chunk
//! endElement("p")
//! ```
//!
//! and for a table:
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

use csv::Records;
use sniffer::Delimiter;

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Deferral, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::mime::MediaType;

/// The UTF-16 units of each read of `handleText`'s buffer, each written as one `characters`
/// event.
const CHUNK: usize = 4096;

/// Tika's `TextAndCSVParser`, for the plain text, CSV and TSV types extractous routes to it.
pub struct TextAndCsvParser;

impl Parser for TextAndCsvParser {
    fn name(&self) -> &'static str {
        "extractous::TextAndCSVParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.csv.TextAndCSVParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["text/plain", "text/csv", "text/tab-separated-values"]
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &["text/plain"]
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
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        prepare(input, pages, metadata).map_err(Decline::Deferred)
    }
}

/// What [`TextAndCsvParser`] prepares of `input`, whose `pages` are released as the text is
/// read: the text, decoded, sniffed and written a piece at a time.
pub(crate) fn prepare<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    metadata: &TikaMetadata,
) -> Result<Prepared<'a>, Deferral> {
    if let Some(reason) = AutoDetectReader::defers(input) {
        return Err(reason.to_owned());
    }
    let content_type = metadata.get("Content-Type").map(str::to_owned);
    Ok(Box::new(move |xhtml| {
        write(input, pages, content_type.as_deref(), Sniff::Table, xhtml)
    }))
}

/// What [`prepare`] prepares of a `text/plain` part of an email: plain text, never a table.
/// Tika sniffs such a part for a table, and makes it `text/csv` when it looks like one (#82).
pub fn prepare_plain(input: Vec<u8>) -> Result<Prepared<'static>, Deferral> {
    if let Some(reason) = AutoDetectReader::defers(&input) {
        return Err(reason.to_owned());
    }
    Ok(Box::new(move |xhtml| {
        write(&input, Pages::IN_MEMORY, None, Sniff::Never, xhtml)
    }))
}

/// Whether text without a content type, or of a type a table may be, is sniffed for a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sniff {
    Table,
    Never,
}

/// A `text/` media type without parameters.
fn text_type(subtype: &str) -> MediaType {
    MediaType::named(&format!("text/{subtype}"))
}

/// Writes what TextAndCSVParser makes of `input` when the metadata's `Content-Type` is
/// `content_type`: without one, or with a type a table may be, it sniffs the text for a table,
/// unless `sniff` says never. Of the values Tika sets more than once, only the last counts:
/// each name keeps the place it was first set at.
fn write(
    input: &[u8],
    pages: Pages<'_>,
    content_type: Option<&str>,
    sniff: Sniff,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let given = content_type
        .map(|content_type| {
            MediaType::parse(content_type).ok_or_else(|| {
                Failure::Malformed(format!(
                    "Tika's MediaType does not parse the content type {content_type:?}"
                ))
            })
        })
        .transpose()?;
    let (detection, mut text) = AutoDetectReader::stream(input, content_type, pages)?;
    let sniffable = |given: &MediaType| {
        [text_type("csv"), text_type("tsv"), text_type("plain")].contains(&given.base_type())
    };
    let mut head = Vec::new();
    let (content_type, delimiter) = match given {
        Some(given) if !sniffable(&given) => (given, None),
        _ if sniff == Sniff::Never => (text_type("plain"), None),
        _ => {
            let mut units = 0;
            while units < sniffer::MARK_LIMIT {
                let Some(piece) = text.next() else {
                    break;
                };
                units += piece.encode_utf16().count();
                head.push(piece);
            }
            match sniffer::sniff(&head.concat()) {
                None => (text_type("plain"), None),
                Some(delimiter) => {
                    let table = text_type(if delimiter == Delimiter::Tab {
                        "tsv"
                    } else {
                        "csv"
                    });
                    (
                        table.with_parameter("delimiter", delimiter.name()),
                        Some(delimiter),
                    )
                }
            }
        }
    };
    let metadata = xhtml.metadata_mut();
    auto_detect_reader::set_metadata(&detection, metadata);
    metadata.set("Content-Encoding", detection.decode_as.name());
    metadata.set(
        "Content-Type",
        content_type
            .with_parameter("charset", detection.decode_as.name())
            .to_string(),
    );
    let chars = head
        .into_iter()
        .chain(text)
        .flat_map(|piece| piece.chars().collect::<Vec<char>>());
    match delimiter {
        None => {
            xhtml.start_element("p")?;
            for chunk in chunks(chars) {
                xhtml.characters(&chunk)?;
            }
            xhtml.end_element("p")?;
        }
        Some(delimiter) => {
            xhtml.metadata_mut().set("csv:delimiter", delimiter.name());
            xhtml.start_element("table")?;
            let mut rows = 0usize;
            for record in Records::new(chars, delimiter.char()) {
                xhtml.start_element("tr")?;
                for value in &record {
                    xhtml.start_element("td")?;
                    xhtml.characters(value)?;
                    xhtml.end_element("td")?;
                }
                xhtml.end_element("tr")?;
                if rows == 0 {
                    xhtml
                        .metadata_mut()
                        .set("csv:num-columns", record.len().to_string());
                }
                rows += 1;
            }
            xhtml.metadata_mut().set("csv:num-rows", rows.to_string());
            xhtml.end_element("table")?;
        }
    }
    Ok(())
}

/// The text as reads of [`CHUNK`] UTF-16 units, a read that would end between the two halves of
/// a surrogate pair ending before it. Tika's `handleText` splits the pair, and each half becomes
/// U+FFFD (#66).
fn chunks(mut text: impl Iterator<Item = char>) -> impl Iterator<Item = String> {
    let mut carried: Option<char> = None;
    std::iter::from_fn(move || {
        let mut chunk = String::new();
        let mut units = 0;
        while let Some(c) = carried.take().or_else(|| text.next()) {
            let width = c.len_utf16();
            if units + width > CHUNK {
                carried = Some(c);
                return Some(chunk);
            }
            chunk.push(c);
            units += width;
            if units == CHUNK {
                return Some(chunk);
            }
        }
        (!chunk.is_empty()).then_some(chunk)
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.csv.TextAndCSVParser";

    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
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
        let reason = |input: &[u8]| {
            TextAndCsvParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
        };
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
    fn splits_reads_between_characters() {
        let chunks = |text: &str| chunks(text.chars()).collect::<Vec<String>>();
        assert_eq!(chunks(""), Vec::<String>::new());
        let full = "a".repeat(CHUNK);
        assert_eq!(chunks(&full), std::slice::from_ref(&full));
        let split = chunks(&format!("{}\u{1F600}b", "a".repeat(CHUNK - 1)));
        assert_eq!(split, ["a".repeat(CHUNK - 1), "\u{1F600}b".to_string()]);
        let whole = chunks(&format!("{}\u{1F600}b", "a".repeat(CHUNK - 2)));
        assert_eq!(whole[0].chars().last(), Some('\u{1F600}'));
        assert_eq!(whole[1], "b");
    }
}
