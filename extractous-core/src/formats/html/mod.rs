//! HTML as Tika 4.1's `JSoupParser` reads it, the parser extractous maps `text/html` and the
//! XHTML types to:
//!
//! - its encoding detection, the chain [`charset::detect`] ports, sets the `tk:` metadata, a
//!   `Content-Type` of the type with the detected charset and a `Content-Encoding`;
//! - jsoup decodes the bytes with the detected charset's superset, unless they start with a byte
//!   order mark, which overrides it (`DataUtil.detectCharsetFromBom`, which reads four bytes,
//!   zeros past the end, so `FF FE` alone reads as UTF-32);
//! - a port of jsoup 1.23.2's tokeniser and tree builder, with Tika's tag set, builds the
//!   document ([`tree_builder`]);
//! - Tika's traversal and `HtmlHandler` write it ([`handler`]).
//!
//! A document defers to Tika when a detector may name a charset the native decoders do not
//! decode, or when Tika would parse embedded content or resolve links against a URL
//! ([`handler::deferral`]).

mod dom;
mod entities;
mod handler;
mod reader;
mod tags;
mod token;
mod tokeniser;
mod tree_builder;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod trees;

use dom::Dom;
use reader::Source;

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};
use crate::text::charset::{self, Charset, Detection};

/// Tika's `JSoupParser`.
pub(crate) struct JSoupParser;

/// The content types JSoupParser names in its `Content-Type`, by the prefix it recognises.
const TYPES: [&str; 4] = [
    "text/html",
    "application/xhtml+xml",
    "application/vnd.wap.xhtml+xml",
    "application/x-asp",
];

impl Parser for JSoupParser {
    fn name(&self) -> &'static str {
        "extractous::JSoupParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &TYPES
    }

    fn defers(&self, input: &[u8], metadata: &TikaMetadata) -> Option<&'static str> {
        if let Some(reason) = AutoDetectReader::defers(input) {
            return Some(reason);
        }
        match read(input, metadata.get("Content-Type")) {
            Ok(document) => handler::deferral(&document.dom, metadata.get("tk:resource-name")),
            Err(_) => Some("the document cannot be decoded natively"),
        }
    }

    /// Parses as `JSoupParser.parse`, which ends the document even when writing it fails.
    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let previous = xhtml.metadata_mut().get("Content-Type").map(str::to_owned);
        let document = read(input, previous.as_deref())?;
        let metadata = xhtml.metadata_mut();
        auto_detect_reader::set_metadata(&document.detection, metadata);
        let charset = document.detection.charset.name();
        let kind = match previous.as_deref() {
            None => Some(TYPES[0]),
            Some(previous) => TYPES
                .iter()
                .copied()
                .find(|kind| previous.starts_with(kind)),
        };
        if let Some(kind) = kind {
            metadata.set("Content-Type", format!("{kind}; charset={charset}"));
        }
        metadata.set("Content-Encoding", charset);
        let Err(stop) = handler::write(&document.dom, xhtml) else {
            return Ok(());
        };
        xhtml.end_document()?;
        Err(stop.into())
    }
}

struct Document {
    detection: Detection,
    dom: Dom,
}

/// Detects, decodes and builds the document as JSoupParser does.
fn read(input: &[u8], content_type: Option<&str>) -> Result<Document, Failure> {
    let detection = charset::detect(input, content_type);
    let text = decode(input, detection.decode_as)?;
    let units: Vec<u16> = text.encode_utf16().collect();
    Ok(Document {
        detection,
        dom: tree_builder::parse(&units, Source::Stream),
    })
}

/// The text jsoup reads: decoded with a byte order mark's charset when the input starts with
/// one, else with `decode_as`.
fn decode(input: &[u8], decode_as: Charset) -> Result<String, Failure> {
    let mut bom = [0u8; 4];
    for (slot, &byte) in bom.iter_mut().zip(input) {
        *slot = byte;
    }
    let (charset, bytes) = match bom {
        [0x00, 0x00, 0xFE, 0xFF] | [0xFF, 0xFE, 0x00, 0x00] => (Charset::named("UTF-32"), input),
        [0xFE, 0xFF, _, _] | [0xFF, 0xFE, _, _] => (Charset::named("UTF-16"), input),
        [0xEF, 0xBB, 0xBF, _] => (Charset::named("UTF-8"), &input[3..]),
        _ => (decode_as, input),
    };
    charset.decode(bytes).ok_or_else(|| {
        Failure::Malformed(format!(
            "jsoup decodes the document as {}, which the native decoders do not decode",
            charset.name()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};

    const TIKA_NAME: &str = "org.apache.tika.parser.html.JSoupParser";

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let (parsed, deferred) = documents(
            &JSoupParser,
            &[
                Path::new("../test_files/documents"),
                Path::new("../quality/corpus"),
                Path::new("../quality/truth"),
            ],
        );
        assert!(deferred.is_empty(), "deferred: {deferred:?}");
        assert!(parsed.len() >= 3, "{} documents", parsed.len());
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
        let (parsed, deferred) = documents(&JSoupParser, &[&conformance_corpus()]);
        let deferred: Vec<String> = deferred
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            deferred,
            [
                "big-preamble.html",
                "big-preamble.html",
                "testHTML_embedded_img.html",
                "testSrcDoc.html"
            ],
            "deferred"
        );
        assert!(parsed.len() >= 31, "{} documents", parsed.len());
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    fn decodes_with_a_byte_order_mark_over_the_detection() {
        let windows = Charset::named("windows-1252");
        assert_eq!(decode(b"\xEF\xBB\xBFcaf\xC3\xA9", windows).unwrap(), "café");
        assert_eq!(decode(b"\xFE\xFF\x00a", windows).unwrap(), "a");
        assert_eq!(decode(b"caf\xE9", windows).unwrap(), "café");
        assert_eq!(decode(b"\xFF\xFE", windows).unwrap(), "\u{FFFD}");
    }
}
