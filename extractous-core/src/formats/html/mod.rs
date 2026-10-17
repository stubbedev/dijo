//! HTML as Tika 4.1's `JSoupParser` reads it, the parser extractous maps `text/html` and the
//! XHTML types to:
//!
//! - its encoding detection, the chain [`charset::detect`] ports, sets the `tk:` metadata, a
//!   `Content-Type` of the type with the detected charset and a `Content-Encoding`;
//! - jsoup decodes the bytes with the detected charset's superset, unless they start with a byte
//!   order mark, which overrides it (`DataUtil.detectCharsetFromBom`, without its padding of
//!   a short input with zeros, which reads `FF FE` alone as UTF-32: #85);
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
use crate::formats::{Declined, Deferral, Failure, Parser, Prepared};
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
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.html.JSoupParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &TYPES
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(input, metadata).map_err(Declined::Deferred)
    }
}

/// What [`JSoupParser`] prepares of `input`, which holds nothing of it.
pub(crate) fn prepare(
    input: &[u8],
    metadata: &TikaMetadata,
) -> Result<Prepared<'static>, Deferral> {
    if let Some(reason) = AutoDetectReader::defers(input) {
        return Err(reason.to_owned());
    }
    let previous = metadata.get("Content-Type").map(str::to_owned);
    let document = read(input, previous.as_deref())
        .map_err(|_| "the document cannot be decoded natively".to_owned())?;
    if let Some(reason) = handler::deferral(&document.dom, metadata.get("tk:resource-name")) {
        return Err(reason.to_owned());
    }
    Ok(Box::new(move |xhtml| {
        write(&document, previous.as_deref(), xhtml)
    }))
}

/// What `JSoupParser.parseString` prepares of HTML already decoded, as Java's UTF-16 units.
pub(crate) fn prepare_string(html: &[u16]) -> Result<Prepared<'static>, Deferral> {
    let dom = tree_builder::parse(html, Source::String);
    if let Some(reason) = handler::deferral(&dom, None) {
        return Err(reason.to_owned());
    }
    Ok(Box::new(move |xhtml| {
        let Err(stop) = handler::write(&dom, xhtml) else {
            return Ok(());
        };
        xhtml.end_document()?;
        Err(stop.into())
    }))
}

/// Writes the document as `JSoupParser.parse` does, which ends the document even when writing
/// it fails; `previous` is the `Content-Type` the metadata held before.
fn write(document: &Document, previous: Option<&str>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    {
        let metadata = xhtml.metadata_mut();
        auto_detect_reader::set_metadata(&document.detection, metadata);
        let charset = document.detection.charset.name();
        let kind = match previous {
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
    }
    let Err(stop) = handler::write(&document.dom, xhtml) else {
        return Ok(());
    };
    xhtml.end_document()?;
    Err(stop.into())
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

/// A document `Jsoup.parse` builds from a string, for a parser that walks its nodes itself.
pub(crate) struct ParsedDocument(Dom);

/// A node of a [`ParsedDocument`], as a `NodeFilter` sees it.
pub(crate) enum ParsedNode<'d> {
    Document,
    /// An element, by its name (`nodeName`), with its attributes in their order.
    Element {
        name: &'d str,
        attributes: Vec<(&'d str, &'d str)>,
    },
    /// A text node (`TextNode`, which a CDATA section is too): its whole text.
    Text(&'d str),
    /// The data of a `script` or `style` element (`DataNode`).
    Data(&'d str),
    /// Any other node, by its name: `#comment` or `#doctype`.
    Other(&'static str),
}

impl ParsedDocument {
    pub(crate) fn parse(html: &str) -> Self {
        let units: Vec<u16> = html.encode_utf16().collect();
        Self(tree_builder::parse(&units, Source::String))
    }

    /// The document node, the root of the tree.
    pub(crate) fn root(&self) -> usize {
        dom::DOCUMENT
    }

    pub(crate) fn children(&self, node: usize) -> &[usize] {
        &self.0.node(node).children
    }

    pub(crate) fn node(&self, node: usize) -> ParsedNode<'_> {
        match &self.0.node(node).data {
            dom::NodeData::Document { .. } => ParsedNode::Document,
            dom::NodeData::Element { .. } => ParsedNode::Element {
                name: &self.0.tag(node).name,
                attributes: self
                    .0
                    .attributes(node)
                    .iter()
                    .map(|attribute| {
                        (
                            attribute.key.as_str(),
                            attribute.value.as_deref().unwrap_or(""),
                        )
                    })
                    .collect(),
            },
            dom::NodeData::Text(text) | dom::NodeData::CData(text) => ParsedNode::Text(text),
            dom::NodeData::Data(data) => ParsedNode::Data(data),
            dom::NodeData::Comment(_) => ParsedNode::Other("#comment"),
            dom::NodeData::Doctype { .. } => ParsedNode::Other("#doctype"),
        }
    }
}

/// The text jsoup reads: decoded with a byte order mark's charset when the input starts with
/// one, else with `decode_as`. A UTF-32 mark needs four bytes of input: jsoup pads a shorter
/// input with zeros, so `FF FE` alone reads as UTF-32 there (#85).
fn decode(input: &[u8], decode_as: Charset) -> Result<String, Failure> {
    let (charset, bytes) = match input {
        [0x00, 0x00, 0xFE, 0xFF, ..] | [0xFF, 0xFE, 0x00, 0x00, ..] => {
            (Charset::named("UTF-32"), input)
        }
        [0xFE, 0xFF, ..] | [0xFF, 0xFE, ..] => (Charset::named("UTF-16"), input),
        [0xEF, 0xBB, 0xBF, ..] => (Charset::named("UTF-8"), &input[3..]),
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
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.html.JSoupParser";

    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
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
        let (departing, paths): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| path.ends_with(TWO_TITLES));
        assert_eq!(departing.len(), 1);
        assert_same_as_tika(&paths, TIKA_NAME);
        let (_, metadata) = crate::Extractor::new()
            .extract_file_to_string(departing[0].to_str().unwrap())
            .unwrap();
        assert_eq!(
            metadata.get("dc:title"),
            Some(&vec!["ActualTitle".to_owned(), "OldMetaTitle".to_owned()])
        );
    }

    /// The document of Tika's tests with a `meta` title after its `title` element, which Tika
    /// records as `html:dc:title` and the port as a further `dc:title` (#86).
    #[cfg(feature = "tika")]
    const TWO_TITLES: &str = "testHTML_metadata_two_titles.html";

    #[test]
    fn decodes_with_a_byte_order_mark_over_the_detection() {
        let windows = Charset::named("windows-1252");
        assert_eq!(decode(b"\xEF\xBB\xBFcaf\xC3\xA9", windows).unwrap(), "café");
        assert_eq!(decode(b"\xFE\xFF\x00a", windows).unwrap(), "a");
        assert_eq!(decode(b"caf\xE9", windows).unwrap(), "café");
        assert_eq!(decode(b"\xFF\xFE", windows).unwrap(), "");
        assert_eq!(decode(b"\xFF\xFEa\x00", windows).unwrap(), "a");
        assert_eq!(
            decode(b"\xFF\xFE\x00\x00a\x00\x00\x00", windows).unwrap(),
            "a"
        );
    }
}
