//! HTML as Tika 4.1's `JSoupParser` reads it, the parser dijo maps `text/html` and the
//! XHTML types to:
//!
//! - its encoding detection, the chain [`charset::detect`] ports, sets the `tk:` metadata, a
//!   `Content-Type` of the type with the detected charset and a `Content-Encoding`;
//! - jsoup decodes the bytes with the detected charset's superset, unless they start with a byte
//!   order mark, which overrides it (`DataUtil.detectCharsetFromBom`, without its padding of
//!   a short input with zeros, which reads `FF FE` alone as UTF-32: #85);
//! - html5ever builds the document by the WHATWG parsing algorithm ([`tree_builder`]), where
//!   Tika's jsoup departs from it (#84): jsoup keeps a CR and a NUL, reads U+FFFF as the end of
//!   the input, closes `<div/>` and the other tags Tika lets close themselves, never fosters text
//!   out of a table, keeps white space before `head` and after `body`, caps its stack of open
//!   elements and its reconstruction of formatting elements, and reads `applet` scope and SVG
//!   `foreignObject` wrongly;
//! - Tika's traversal and `HtmlHandler` write it ([`handler`]).
//!
//! A document of at most [`WHOLE`] bytes is built whole before it is written. A larger one is
//! decoded and built [`PIECE`] bytes at a time, twice: once to survey where the tree builder
//! changes what it has built already, and once to write each part as soon as nothing changes it
//! any more and let go of it ([`tree_builder`]), so that the memory it takes is bounded by how
//! much of it the tree builder may still change rather than by its size.
//!
//! The parser reads a document differently from Tika's where a declaration names a charset the
//! native decoders do not decode ([`AutoDetectReader::difference`]), and where it embeds the text
//! of a data URI in a charset it does not encode ([`handler::difference`]). A parser that must
//! know that of each of many documents before it writes any [`inspect`]s each first, which
//! holds the charset detected rather than the document built, and builds it again from that as
//! it writes it.

mod dom;
mod handler;
mod tree_builder;
mod url;
mod walk;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod trees;

pub use dom::Attribute;
use dom::Dom;
use handler::{may_hold_data_uri, Base, HtmlHandler, Scan};
use walk::{walk_whole, Holds};
pub use walk::{ParsedNode, Visitor};

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging};
use crate::text::charset::{self, Charset, Detection};

/// The largest document built whole before it is written.
const WHOLE: usize = 4 << 20;

/// The bytes of a larger document decoded and built at a time.
const PIECE: usize = 1 << 16;

/// How the parser reads a document by its size: whole up to `whole` bytes, else `piece` bytes
/// at a time.
#[derive(Debug, Clone, Copy)]
struct Sizes {
    whole: usize,
    piece: usize,
}

const SIZES: Sizes = Sizes {
    whole: WHOLE,
    piece: PIECE,
};

/// Tika's `JSoupParser`.
pub struct JSoupParser;

/// The content types JSoupParser names in its `Content-Type`, by the prefix it recognises.
const TYPES: [&str; 4] = [
    "text/html",
    "application/xhtml+xml",
    "application/vnd.wap.xhtml+xml",
    "application/x-asp",
];

impl Parser for JSoupParser {
    fn name(&self) -> &'static str {
        "dijo::JSoupParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.html.JSoupParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &TYPES
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
        let previous = metadata.get("Content-Type").map(str::to_owned);
        let document = read(input, pages, previous.as_deref());
        prepared(document, previous, Base::of(metadata))
    }
}

/// What [`JSoupParser`] prepares of `input`, which holds nothing of it it does not copy: a
/// document it reads differently from Tika's parser is [`Decline::Differs`].
pub fn prepare(
    input: &[u8],
    metadata: &TikaMetadata,
) -> Result<Prepared<'static>, Decline<'static>> {
    let previous = metadata.get("Content-Type").map(str::to_owned);
    let document = read(input, Pages::IN_MEMORY, previous.as_deref()).into_owned();
    prepared(document, previous, Base::of(metadata))
}

/// What [`JSoupParser`] prepares of `input`, which it holds as it is, spooled or mapped, so
/// that a large page is built a piece at a time from it: a document it reads differently from
/// Tika's parser is [`Decline::Differs`].
pub fn prepare_input(
    input: Input<'static>,
    metadata: &TikaMetadata,
) -> Result<Prepared<'static>, Decline<'static>> {
    let previous = metadata.get("Content-Type").map(str::to_owned);
    let document = read_from(Bytes::Held(input, 0), previous.as_deref(), SIZES);
    prepared(document, previous, Base::of(metadata))
}

/// What [`inspect`] finds of a document that [`Inspection::prepare`] needs to prepare it as
/// [`prepare`] does: the charset detected, and what the metadata given says.
pub struct Inspection {
    detection: Detection,
    previous: Option<String>,
    base: Base,
}

/// Where [`JSoupParser`] reads `input` differently from Tika's parser, as [`prepare`] finds,
/// without holding the document it builds to find it: its [`Inspection`] prepares it later
/// without detecting its charset again.
pub fn inspect(input: &[u8], metadata: &TikaMetadata) -> (Inspection, Option<Difference>) {
    let previous = metadata.get("Content-Type").map(str::to_owned);
    let detection = charset::detect_decodable(input, previous.as_deref());
    let difference = AutoDetectReader::detected_difference(&detection)
        .or_else(|| read_otherwise(input, detection.decode_as, SIZES));
    let inspection = Inspection {
        detection,
        previous,
        base: Base::of(metadata),
    };
    (inspection, difference)
}

impl Inspection {
    /// What [`prepare`] prepares of `input`, the input inspected, which reads it differently
    /// from Tika's parser where [`inspect`] said so.
    pub fn prepare(self, input: &[u8]) -> Prepared<'static> {
        let source = Bytes::Borrowed(input, Pages::IN_MEMORY);
        let (reading, difference) = read_decoded(source, self.detection.decode_as, SIZES);
        let document = Document {
            detection: self.detection,
            reading,
            difference,
        }
        .into_owned();
        Box::new(move |xhtml| write(document, self.previous.as_deref(), self.base, xhtml))
    }
}

/// What the parser prepares of `document`, read when the metadata's `Content-Type` was
/// `previous`, with links resolved against `base`.
fn prepared(
    document: Document<'_>,
    previous: Option<String>,
    base: Base,
) -> Result<Prepared<'_>, Decline<'_>> {
    let difference = AutoDetectReader::detected_difference(&document.detection)
        .or_else(|| document.difference.clone());
    unless_different(
        Box::new(move |xhtml| write(document, previous.as_deref(), base, xhtml)),
        difference,
    )
}

/// What `JSoupParser.parseString` prepares of HTML already decoded, as Java's UTF-16 units: a
/// lone surrogate is U+FFFD, as the WHATWG decoder of UTF-16 makes it, and where the parser
/// reads the HTML differently from Tika's, if anywhere: the parser reads any HTML.
/// The document is built as it is written, unless finding that difference builds it already.
pub fn prepare_string(html: &[u16]) -> (Prepared<'static>, Option<Difference>) {
    let text = String::from_utf16_lossy(html);
    let (whole, difference) = match build_if_it_may_differ(&text) {
        Some((dom, difference)) => (Whole::Built(dom), difference),
        None => (Whole::Text(text), None),
    };
    (
        Box::new(move |xhtml| {
            let mut dom = match whole {
                Whole::Built(dom) => dom,
                Whole::Text(text) => tree_builder::build(&text).dom,
            };
            let mut handler = HtmlHandler::new(Base::Unknown, xhtml);
            let written = walk_whole(&mut dom, &mut handler);
            ended(written, xhtml)
        }),
        difference,
    )
}

/// A document read whole: built, or its text, to be built.
enum Whole {
    Built(Dom),
    Text(String),
}

/// Walks the document `html` holds, built whole, as `TikaNodeFilter` traverses it, for a parser
/// that writes its nodes itself.
pub fn walk_parsed<V: Visitor>(html: &str, visitor: &mut V) -> Result<(), V::Stop> {
    walk_whole(&mut tree_builder::build(html).dom, visitor)
}

/// Writes the document as `JSoupParser.parse` does, which ends the document even when writing
/// it fails; `previous` is the `Content-Type` the metadata held before.
fn write(
    document: Document<'_>,
    previous: Option<&str>,
    base: Base,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
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
    let mut handler = HtmlHandler::new(base, xhtml);
    let written = match document.reading {
        Reading::Whole(mut dom) => walk_whole(&mut dom, &mut handler),
        Reading::Surveyed { holds, text } => text.walk(&holds, &mut handler),
    };
    ended(written, xhtml)
}

/// What writing a document through Tika's handlers gave, which end the document even when
/// writing it fails.
fn ended(written: Result<(), Failure>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let Err(failure) = written else {
        return Ok(());
    };
    xhtml.end_document()?;
    Err(failure)
}

/// What the parser reads of a document before it writes it.
struct Document<'a> {
    detection: Detection,
    reading: Reading<'a>,
    /// Where it reads the document differently from Tika's parser, as [`Scan`] finds.
    difference: Option<Difference>,
}

/// A document built whole, or surveyed to be built again as it is written.
enum Reading<'a> {
    Whole(Dom),
    Surveyed { holds: Holds, text: Text<'a> },
}

impl Document<'_> {
    /// The same reading, holding a copy of what it reads of the input.
    fn into_owned(self) -> Document<'static> {
        Document {
            detection: self.detection,
            difference: self.difference,
            reading: match self.reading {
                Reading::Whole(dom) => Reading::Whole(dom),
                Reading::Surveyed { holds, text } => Reading::Surveyed {
                    holds,
                    text: Text {
                        bytes: Bytes::Held(text.bytes.bytes().to_vec().into(), 0),
                        charset: text.charset,
                        piece: text.piece,
                    },
                },
            },
        }
    }
}

/// The bytes of a document from where jsoup decodes them on.
enum Bytes<'a> {
    /// Bytes read in place, whose pages are given.
    Borrowed(&'a [u8], Pages<'a>),
    /// Bytes held, from the offset given on.
    Held(Input<'static>, usize),
}

impl Bytes<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes, _) => bytes,
            Self::Held(input, start) => &input[*start..],
        }
    }

    fn pages(&self) -> Pages<'_> {
        match self {
            Self::Borrowed(_, pages) => *pages,
            Self::Held(input, _) => input.pages(),
        }
    }

    /// The bytes from `skipped` on.
    fn skip(self, skipped: usize) -> Self {
        match self {
            Self::Borrowed(bytes, pages) => Self::Borrowed(&bytes[skipped..], pages),
            Self::Held(input, start) => Self::Held(input, start + skipped),
        }
    }
}

/// The bytes of a document jsoup decodes, the charset it decodes them with, and how many it
/// decodes at a time.
struct Text<'a> {
    bytes: Bytes<'a>,
    charset: Charset,
    piece: usize,
}

impl Text<'_> {
    /// Builds the document a piece at a time to survey it.
    fn survey(&self) -> tree_builder::Survey {
        let paging = Paging::new(self.bytes.pages());
        tree_builder::survey(self.pieces(&paging))
    }

    /// Builds the document a piece at a time again, walking it with `visitor` as it is built.
    fn walk<V: Visitor>(&self, holds: &Holds, visitor: &mut V) -> Result<(), V::Stop> {
        let paging = Paging::new(self.bytes.pages());
        tree_builder::stream(self.pieces(&paging), holds, visitor)
    }

    /// The text, decoded a piece at a time, the pages read released as it goes.
    fn pieces<'p>(&'p self, paging: &'p Paging<'_>) -> impl Iterator<Item = String> + 'p {
        self.charset
            .pieces(self.bytes.bytes(), self.piece)
            .expect("the port decodes every charset its detection settles on")
            .inspect(move |_| paging.read(self.piece))
    }
}

/// Detects, decodes and builds the document as JSoupParser does: whole, or surveyed a piece at a
/// time when it is larger than [`WHOLE`] bytes.
fn read<'a>(input: &'a [u8], pages: Pages<'a>, content_type: Option<&str>) -> Document<'a> {
    read_by(input, pages, content_type, SIZES)
}

/// [`read`], by the `sizes` given.
fn read_by<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    content_type: Option<&str>,
    sizes: Sizes,
) -> Document<'a> {
    read_from(Bytes::Borrowed(input, pages), content_type, sizes)
}

/// [`read_by`] of the bytes `source`.
fn read_from<'a>(source: Bytes<'a>, content_type: Option<&str>, sizes: Sizes) -> Document<'a> {
    let detection = charset::detect_decodable(source.bytes(), content_type);
    let (reading, difference) = read_decoded(source, detection.decode_as, sizes);
    Document {
        detection,
        reading,
        difference,
    }
}

/// Decodes and builds the bytes `source` as JSoupParser does when its detection decodes them
/// as `decode_as`, by the `sizes` given, and finds where it reads them differently from Tika's.
fn read_decoded(
    source: Bytes<'_>,
    decode_as: Charset,
    sizes: Sizes,
) -> (Reading<'_>, Option<Difference>) {
    let input = source.bytes();
    let (charset, bytes) = jsoup_charset(input, decode_as);
    if input.len() <= sizes.whole {
        let (dom, difference) = build(&decode(charset, bytes));
        return (Reading::Whole(dom), difference);
    }
    let skipped = input.len() - bytes.len();
    let text = Text {
        bytes: source.skip(skipped),
        charset,
        piece: sizes.piece,
    };
    let survey = text.survey();
    let difference = if survey.reads_otherwise {
        let mut scan = Scan::default();
        text.walk(&survey.holds, &mut scan)
            .unwrap_or_else(|stop| match stop {});
        scan.difference()
    } else {
        None
    };
    let reading = Reading::Surveyed {
        holds: survey.holds,
        text,
    };
    (reading, difference)
}

/// Where the parser reads `input` differently from Tika's when its detection decodes it as
/// `decode_as`, as [`read_decoded`] finds by the `sizes` given, without building a document read
/// whole that holds no data URI.
fn read_otherwise(input: &[u8], decode_as: Charset, sizes: Sizes) -> Option<Difference> {
    if input.len() > sizes.whole {
        return read_decoded(Bytes::Borrowed(input, Pages::IN_MEMORY), decode_as, sizes).1;
    }
    let (charset, bytes) = jsoup_charset(input, decode_as);
    build_if_it_may_differ(&decode(charset, bytes)).and_then(|(_, difference)| difference)
}

/// [`build`] of `html`, unless it holds no data URI, which the parser reads as Tika's does.
fn build_if_it_may_differ(html: &str) -> Option<(Dom, Option<Difference>)> {
    may_hold_data_uri(html).then(|| build(html))
}

/// The text of `bytes` in `charset`, which jsoup decodes them with.
fn decode(charset: Charset, bytes: &[u8]) -> String {
    charset
        .decode(bytes)
        .expect("the port decodes every charset its detection settles on")
}

/// The document `html` holds, built whole, and where the parser reads it differently from
/// Tika's.
fn build(html: &str) -> (Dom, Option<Difference>) {
    let mut built = tree_builder::build(html);
    let difference = if built.reads_otherwise {
        let mut scan = Scan::default();
        walk_whole(&mut built.dom, &mut scan).unwrap_or_else(|stop| match stop {});
        scan.difference()
    } else {
        None
    };
    (built.dom, difference)
}

/// Where [`Scan`] finds the parser reads the document `html` holds differently from Tika's.
#[cfg(test)]
fn scan(html: &str) -> Scan {
    let mut scan = Scan::default();
    walk_whole(&mut tree_builder::build(html).dom, &mut scan).unwrap_or_else(|stop| match stop {});
    scan
}

/// The charset jsoup decodes `input` with, and the bytes it decodes: a byte order mark's charset
/// when the input starts with one, else `decode_as`. A UTF-32 mark needs four bytes of input:
/// jsoup pads a shorter input with zeros, so `FF FE` alone reads as UTF-32 there (#85).
fn jsoup_charset(input: &[u8], decode_as: Charset) -> (Charset, &[u8]) {
    match input {
        [0x00, 0x00, 0xFE, 0xFF, ..] | [0xFF, 0xFE, 0x00, 0x00, ..] => {
            (Charset::named("UTF-32"), input)
        }
        [0xFE, 0xFF, ..] | [0xFF, 0xFE, ..] => (Charset::named("UTF-16"), input),
        [0xEF, 0xBB, 0xBF, ..] => (Charset::named("UTF-8"), &input[3..]),
        _ => (decode_as, input),
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::formats::embedded::ParseContext;
    use crate::formats::noted;
    use crate::formats::xhtml::{ContentHandler, Stop};

    /// Every event a document's writing gives, adjacent characters joined.
    #[derive(Default)]
    struct Events {
        written: String,
        text: String,
    }

    impl Events {
        fn flush(&mut self) {
            if !self.text.is_empty() {
                self.written.push_str(&format!("T {}\n", self.text));
                self.text.clear();
            }
        }
    }

    impl ContentHandler for Events {
        fn start_document(&mut self) -> Result<(), Stop> {
            Ok(())
        }

        fn start_prefix_mapping(&mut self, _prefix: &str, _uri: &str) -> Result<(), Stop> {
            Ok(())
        }

        fn start_element_in(
            &mut self,
            _namespace: &str,
            name: &str,
            attributes: &[(&str, &str)],
        ) -> Result<(), Stop> {
            self.flush();
            self.written.push_str(&format!("S {name} {attributes:?}\n"));
            Ok(())
        }

        fn end_element_in(&mut self, _namespace: &str, name: &str) -> Result<(), Stop> {
            self.flush();
            self.written.push_str(&format!("E {name}\n"));
            Ok(())
        }

        fn characters(&mut self, text: &str) -> Result<(), Stop> {
            self.text.push_str(text);
            Ok(())
        }

        fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
            self.flush();
            self.written.push_str(&format!("W {text:?}\n"));
            Ok(())
        }

        fn end_document(&mut self) -> Result<(), Stop> {
            self.flush();
            Ok(())
        }
    }

    /// What the parser writes of `input` of the content type `content_type`, read by `sizes`:
    /// its events, its metadata, and where it reads it differently from Tika's.
    fn written(input: &[u8], content_type: &str, sizes: Sizes) -> String {
        let mut given = TikaMetadata::new();
        given.set("Content-Type", content_type);
        let previous = Some(content_type.to_owned());
        let document = read_by(input, Pages::IN_MEMORY, previous.as_deref(), sizes);
        let reading = noted(prepared(document, previous, Base::of(&given))).unwrap();
        events_of(reading, given)
    }

    /// The events, metadata and outcome of writing `prepared` from the metadata `given`, and
    /// the difference noted of it.
    fn events_of(
        (prepared, difference): (Prepared<'_>, Option<Difference>),
        given: TikaMetadata,
    ) -> String {
        let configs = ParserConfigs::default();
        let context = ParseContext::new(&configs);
        let mut events = Events::default();
        let (metadata, _, result) = Xhtml::write(&mut events, given, &context, prepared);
        let metadata: Vec<String> = metadata
            .names()
            .map(|name| format!("{name}={:?}", metadata.values(name)))
            .collect();
        format!("{}{metadata:?}\n{result:?}\n{difference:?}", events.written)
    }

    /// Every document of [`every_input`], read a few bytes at a time and written as it is built,
    /// is written as it is once built whole.
    #[test]
    fn writes_a_document_as_it_is_built_as_once_it_is_built_whole() {
        dijo_core::testing::install_routes();
        let inputs = every_input();
        assert!(inputs.len() > 3000, "{} inputs", inputs.len());
        let whole = Sizes {
            whole: usize::MAX,
            piece: PIECE,
        };
        for (label, input, content_type) in &inputs {
            let expected = written(input, content_type, whole);
            for piece in [1, 61] {
                let streamed = Sizes { whole: 0, piece };
                assert_eq!(
                    written(input, content_type, streamed),
                    expected,
                    "{label} read {piece} bytes at a time"
                );
            }
        }
    }

    /// Documents that embed data URIs the parser reads differently from Tika's, spelled out and
    /// with character references, and one that declares a charset it does not decode.
    const DIFFERING: [&str; 4] = [
        "<p>a</p><img src=\"data:text/plain;charset=windows-1252,hej\">",
        "<img src=\"&#100;ata:text/plain;charset=windows-1252,hej\">",
        "<img src=\"data&colon;text/plain;charset=windows-1252,hej\">",
        "<meta charset=\"x-IBM930\"><p>plain text</p>",
    ];

    /// The documents of the references, and [`DIFFERING`], each with its label and content type.
    fn reference_inputs() -> Vec<(String, Vec<u8>, String)> {
        let mut inputs: Vec<(String, Vec<u8>, String)> =
            crate::formats::reference::cases(reference::REFERENCE)
                .into_iter()
                .map(|case| (case.label, case.input, case.content_type))
                .collect();
        inputs.extend(DIFFERING.map(|html| {
            (
                html.to_owned(),
                html.as_bytes().to_vec(),
                "text/html".to_owned(),
            )
        }));
        inputs
    }

    /// [`reference_inputs`] and html5lib's documents.
    fn every_input() -> Vec<(String, Vec<u8>, String)> {
        let mut inputs = reference_inputs();
        inputs.extend(
            trees::html5lib_inputs()
                .into_iter()
                .map(|(name, input)| (name, input.into_bytes(), "text/html".to_owned())),
        );
        inputs
    }

    /// Every document of [`every_input`], built or not, read whole or a piece at a time, is read
    /// differently from Tika's where it is once built whole.
    #[test]
    fn finds_where_it_reads_a_document_otherwise_without_building_it() {
        let inputs = every_input();
        let whole = Sizes {
            whole: usize::MAX,
            piece: PIECE,
        };
        let streamed = Sizes {
            whole: 0,
            piece: 61,
        };
        let mut differences = 0;
        for (label, input, content_type) in &inputs {
            let built = read_by(input, Pages::IN_MEMORY, Some(content_type), whole);
            let decode_as = built.detection.decode_as;
            differences += usize::from(built.difference.is_some());
            for sizes in [whole, streamed] {
                assert_eq!(
                    read_otherwise(input, decode_as, sizes),
                    built.difference,
                    "{label}"
                );
            }
        }
        assert!(differences >= 3, "{differences} differences");
    }

    /// Every document of [`every_input`] given decoded is written, and read differently from
    /// Tika's, as it is once built at once.
    #[test]
    fn prepares_a_decoded_document_as_once_it_is_built() {
        dijo_core::testing::install_routes();
        for (label, input, _) in every_input() {
            let text = String::from_utf8_lossy(&input).into_owned();
            let units: Vec<u16> = text.encode_utf16().collect();
            let (mut dom, difference) = build(&text);
            let built: Prepared<'static> = Box::new(move |xhtml| {
                let mut handler = HtmlHandler::new(Base::Unknown, xhtml);
                let written = walk_whole(&mut dom, &mut handler);
                ended(written, xhtml)
            });
            let expected = events_of((built, difference), TikaMetadata::new());
            let prepared = events_of(prepare_string(&units), TikaMetadata::new());
            assert_eq!(prepared, expected, "{label}");
        }
    }

    /// Every document of the references and of [`DIFFERING`], inspected and then prepared, is
    /// written as it is once prepared, with the same difference.
    #[test]
    fn prepares_an_inspected_document_as_it_prepares_it_at_once() {
        dijo_core::testing::install_routes();
        let inputs = reference_inputs();
        let mut differences = 0;
        for (label, input, content_type) in &inputs {
            let mut given = TikaMetadata::new();
            given.set("Content-Type", content_type);
            given.set("Content-Location", "http://example.com/dir/page.html");
            let expected = events_of(noted(prepare(input, &given)).unwrap(), given.clone());
            let (inspection, difference) = inspect(input, &given);
            differences += usize::from(difference.is_some());
            let inspected = events_of((inspection.prepare(input), difference), given);
            assert_eq!(inspected, expected, "{label}");
        }
        assert!(differences >= DIFFERING.len(), "{differences} differences");
    }

    /// A document larger than the memory budget allows to build whole is written as it is
    /// built, a part at a time, whatever its size.
    #[test]
    fn writes_a_document_larger_than_its_memory_budget() {
        use std::io::{BufRead, BufReader, BufWriter, Write};

        const BUDGET: u64 = 32 << 20;
        let dir =
            crate::process::temp_dir().join(format!("dijo-large-html-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("large.html");
        let mut file = BufWriter::new(std::fs::File::create(&path).unwrap());
        file.write_all(
            b"<!DOCTYPE html><html lang=\"da\"><head><title>Stor side</title></head><body>\n",
        )
        .unwrap();
        let mut paragraphs = 0u64;
        while file.get_ref().metadata().unwrap().len() < 3 * crate::input::MAX_READ_WHOLE / 2 {
            for _ in 0..10_000 {
                paragraphs += 1;
                writeln!(
                    file,
                    "<p>Afsnit {paragraphs}: <b>fed</b> og <a href=\"#a{paragraphs}\">link</a>.\
                     </p><table><tr><td>Celle {paragraphs}</td></tr></table>"
                )
                .unwrap();
            }
            file.flush().unwrap();
        }
        file.write_all(b"<p>Slut.</p></body></html>\n").unwrap();
        drop(file);
        crate::process::configure(crate::process::ProcessSettings {
            max_heap_bytes: std::num::NonZeroU64::new(BUDGET),
            ..crate::process::ProcessSettings::default()
        })
        .unwrap();
        let (stream, _) = crate::Extractor::new()
            .extract_file(path.to_str().unwrap())
            .unwrap();
        let mut reader = BufReader::new(stream);
        let (mut found, mut cells, mut last) = (0u64, 0u64, String::new());
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 0 {
            let read = line.trim();
            if read.starts_with("Afsnit ") {
                found += 1;
                assert_eq!(read, format!("Afsnit {found}: fed og link."));
            } else if read.starts_with("Celle ") {
                cells += 1;
            }
            if !read.is_empty() {
                last = read.to_owned();
            }
            line.clear();
        }
        let metadata = reader.into_inner().metadata().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((found, cells), (paragraphs, paragraphs));
        assert_eq!(last, "Slut.");
        assert_eq!(metadata["dc:title"], ["Stor side"]);
        assert_eq!(metadata["Content-Language"], ["da"]);
        assert_eq!(metadata["tk:parsed-by"].last().unwrap(), JSoupParser.name());
    }

    #[test]
    fn decodes_with_a_byte_order_mark_over_the_detection() {
        let decode = |input: &[u8], decode_as: Charset| {
            let (charset, bytes) = jsoup_charset(input, decode_as);
            charset.decode(bytes).unwrap()
        };
        let windows = Charset::named("windows-1252");
        assert_eq!(decode(b"\xEF\xBB\xBFcaf\xC3\xA9", windows), "café");
        assert_eq!(decode(b"\xFE\xFF\x00a", windows), "a");
        assert_eq!(decode(b"caf\xE9", windows), "café");
        assert_eq!(decode(b"\xFF\xFE", windows), "");
        assert_eq!(decode(b"\xFF\xFEa\x00", windows), "a");
        assert_eq!(decode(b"\xFF\xFE\x00\x00a\x00\x00\x00", windows), "a");
    }
}
