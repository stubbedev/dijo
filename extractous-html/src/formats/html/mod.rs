//! HTML as Tika 4.1's `JSoupParser` reads it, the parser extractous maps `text/html` and the
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
//! of a data URI in a charset it does not encode ([`handler::difference`]).

mod dom;
mod handler;
mod tree_builder;
mod url;
mod walk;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod trees;

use std::borrow::Cow;

pub use dom::Attribute;
use dom::Dom;
use handler::{Base, HtmlHandler, Scan};
use walk::{walk_whole, Holds};
pub use walk::{ParsedNode, Visitor};

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
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

/// Where the text the parser writes departs from Tika's, for the parity tests of every parser
/// that hands it HTML: the WHATWG algorithm makes each line break an LF, drops the white space
/// before `head`, and puts the white space after `</body>` in the body (#84), and the WHATWG
/// URL standard resolves its links (#162).
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub const DEPARTURES: extractous_base::formats::parity::Departures = &[
    extractous_base::formats::parity::Departure::BlankLines("#84"),
    extractous_base::formats::parity::Departure::Links("#162"),
];

/// [`DEPARTURES`], for the parity tests of parsers that hand it pages whose tree differs from
/// jsoup's, by the WHATWG algorithm: their XHTML compared on its text, its elements aside (#84).
/// The HTML reference test compares such pages on their metadata alone. Its links are resolved
/// by the WHATWG URL standard (#162).
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub const TREE_DEPARTURES: extractous_base::formats::parity::Departures = &[
    extractous_base::formats::parity::Departure::BlankLines("#84"),
    extractous_base::formats::parity::Departure::Elements("#84"),
    extractous_base::formats::parity::Departure::Links("#162"),
];

/// Whether an attribute of an XHTML element by this name holds a link the parser resolves, as
/// the WHATWG URL standard resolves it, where Tika's path resolves it as `java.net.URL` does
/// (#162).
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub fn is_link_attribute(name: &str) -> bool {
    handler::URI_ATTRIBUTES.contains(&name)
        || name == handler::CODEBASE
        || handler::OBJECT_LINKS.contains(&name)
}

/// Whether `native`, a link the parser writes against the URL `base`, is `tika`, the one Tika's
/// path writes for it, as the WHATWG URL standard resolves it (#162).
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub fn resolves_alike(base: Option<&str>, tika: &str, native: &str) -> bool {
    url::resolves_alike(base, tika, native)
}

/// How many documents the HTML parser has built by the WHATWG algorithm in this process, every
/// parser that hands it HTML included: a native extraction that adds to it departs from Tika's
/// path by [`DEPARTURES`]. Counted across threads, as a stream extraction parses in its own, it
/// is exact where one test runs per process, as nextest runs them.
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub fn documents_built() -> usize {
    tree_builder::BUILT.load(std::sync::atomic::Ordering::Relaxed)
}

/// The document of Tika's tests with a `meta` title after its `title` element, which Tika
/// records as `html:dc:title` and the parser as a further `dc:title` (#86): the parity tests
/// check what the parser gives instead of comparing it with Tika's.
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub const TWO_TITLES: &str = "testHTML_metadata_two_titles.html";

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
        "extractous::JSoupParser"
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
        prepared(document, input, previous, Base::of(metadata))
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
    prepared(document, input, previous, Base::of(metadata))
}

/// What the parser prepares of `document`, read from `input` when the metadata's
/// `Content-Type` was `previous`, with links resolved against `base`.
fn prepared<'a>(
    document: Document<'a>,
    input: &[u8],
    previous: Option<String>,
    base: Base,
) -> Result<Prepared<'a>, Decline<'a>> {
    let difference = AutoDetectReader::difference(input, previous.as_deref())
        .or_else(|| document.difference.clone());
    unless_different(
        Box::new(move |xhtml| write(document, previous.as_deref(), base, xhtml)),
        difference,
    )
}

/// What `JSoupParser.parseString` prepares of HTML already decoded, as Java's UTF-16 units: a
/// lone surrogate is U+FFFD, as the WHATWG decoder of UTF-16 makes it, and where the parser
/// reads the HTML differently from Tika's, if anywhere: the parser reads any HTML.
pub fn prepare_string(html: &[u16]) -> (Prepared<'static>, Option<Difference>) {
    let (mut dom, difference) = build(&String::from_utf16_lossy(html));
    (
        Box::new(move |xhtml| {
            let mut handler = HtmlHandler::new(Base::Unknown, xhtml);
            let written = walk_whole(&mut dom, &mut handler);
            ended(written, xhtml)
        }),
        difference,
    )
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
                        bytes: Cow::Owned(text.bytes.into_owned()),
                        charset: text.charset,
                        pages: Pages::IN_MEMORY,
                        piece: text.piece,
                    },
                },
            },
        }
    }
}

/// The bytes of a document jsoup decodes, the charset it decodes them with, and how many it
/// decodes at a time.
struct Text<'a> {
    bytes: Cow<'a, [u8]>,
    charset: Charset,
    pages: Pages<'a>,
    piece: usize,
}

impl Text<'_> {
    /// Builds the document a piece at a time to survey it.
    fn survey(&self) -> tree_builder::Survey {
        let paging = Paging::new(self.pages);
        tree_builder::survey(self.pieces(&paging))
    }

    /// Builds the document a piece at a time again, walking it with `visitor` as it is built.
    fn walk<V: Visitor>(&self, holds: &Holds, visitor: &mut V) -> Result<(), V::Stop> {
        let paging = Paging::new(self.pages);
        tree_builder::stream(self.pieces(&paging), holds, visitor)
    }

    /// The text, decoded a piece at a time, the pages read released as it goes.
    fn pieces<'p>(&'p self, paging: &'p Paging<'_>) -> impl Iterator<Item = String> + 'p {
        self.charset
            .pieces(&self.bytes, self.piece)
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
    let detection = charset::detect_decodable(input, content_type);
    let (charset, bytes) = jsoup_charset(input, detection.decode_as);
    if input.len() <= sizes.whole {
        let text = charset
            .decode(bytes)
            .expect("the port decodes every charset its detection settles on");
        let (dom, difference) = build(&text);
        return Document {
            detection,
            reading: Reading::Whole(dom),
            difference,
        };
    }
    let text = Text {
        bytes: Cow::Borrowed(bytes),
        charset,
        pages,
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
    Document {
        detection,
        reading: Reading::Surveyed {
            holds: survey.holds,
            text,
        },
        difference,
    }
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
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::embedded::ParseContext;
    use crate::formats::noted;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika_departing, conformance_corpus, documents};
    use crate::formats::xhtml::{ContentHandler, Stop};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.html.JSoupParser";

    #[cfg(feature = "tika")]
    fn assert_same_as_whatwg_tika(documents: &[&Path]) {
        assert_same_as_tika_departing(documents, TIKA_NAME, DEPARTURES);
    }

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
        assert_same_as_whatwg_tika(&paths);
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
        let (parsed, deferred) = documents(&JSoupParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "deferred: {deferred:?}");
        assert!(parsed.len() >= 35, "{} documents", parsed.len());
        let (departing, paths): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| path.ends_with(TWO_TITLES));
        assert_eq!(departing.len(), 1);
        assert_same_as_whatwg_tika(&paths);
        let (_, metadata) = crate::Extractor::new()
            .extract_file_to_string(departing[0].to_str().unwrap())
            .unwrap();
        assert_eq!(
            metadata.get("dc:title"),
            Some(&vec!["ActualTitle".to_owned(), "OldMetaTitle".to_owned()])
        );
    }

    /// Links the WHATWG URL standard resolves otherwise than Tika's `java.net.URL` (#162): every
    /// other part of the page is still Tika's, and where Tika fails on a query against a `jar`
    /// base, the parser reads the page.
    #[cfg(feature = "tika")]
    #[test]
    fn departs_from_tika_by_the_links_the_url_standard_resolves() {
        use crate::formats::parity::{assert_read_where_tika_fails, Scratch};
        let scratch = Scratch::new("html-links");
        let page = |name: &str, base: &str, links: &[&str]| {
            let anchors: String = links
                .iter()
                .map(|link| format!("<p><a href=\"{link}\">{link}</a></p>"))
                .collect();
            scratch.write(
                name,
                format!("<html><head><base href=\"{base}\"></head><body>{anchors}</body></html>")
                    .as_bytes(),
            )
        };
        let departing = [
            page("user.html", "http://u:p@Host:80/a/b", &["?q", "c", "/d e"]),
            page("root.html", "http://h/a/", &["../../z", "y"]),
            page("pathless.html", "http://h", &["a/../../b", "#f"]),
            page("ftp.html", "ftp://h/d/", &["x", "mailto:me@x.org"]),
        ];
        let paths: Vec<&Path> = departing.iter().map(PathBuf::as_path).collect();
        assert_same_as_whatwg_tika(&paths);
        assert_read_where_tika_fails(&[&page("jar.html", "url:jar:#x", &["?q"])], "#162");
    }

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
        let (prepared, difference) =
            noted(prepared(document, input, previous, Base::of(&given))).unwrap();
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

    /// Every document of the references, read a few bytes at a time and written as it is built,
    /// is written as it is once built whole.
    #[test]
    fn writes_a_document_as_it_is_built_as_once_it_is_built_whole() {
        extractous_core::parity::install_routes();
        let mut inputs: Vec<(String, Vec<u8>, String)> =
            crate::formats::reference::cases(reference::REFERENCE)
                .into_iter()
                .map(|case| (case.label, case.input, case.content_type))
                .collect();
        inputs.extend(
            trees::html5lib_inputs()
                .into_iter()
                .map(|(name, input)| (name, input.into_bytes(), "text/html".to_owned())),
        );
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

    /// A document larger than the memory budget allows to build whole is written as it is
    /// built, a part at a time, whatever its size.
    #[test]
    fn writes_a_document_larger_than_its_memory_budget() {
        use std::io::{BufRead, BufReader, BufWriter, Write};

        const BUDGET: u64 = 32 << 20;
        let dir = crate::isolate::temp_dir()
            .join(format!("extractous-large-html-{}", std::process::id()));
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
        crate::isolate::configure(crate::isolate::IsolateSettings {
            max_heap_bytes: std::num::NonZeroU64::new(BUDGET),
            ..crate::isolate::IsolateSettings::default()
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
