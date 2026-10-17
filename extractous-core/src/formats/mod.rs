//! Extraction in Rust, format by format (#41). A [`Parser`] writes a document as XHTML through an
//! [`Xhtml`], the port of Tika's `XHTMLContentHandler`, into the same handler chains extractous
//! puts behind Tika's parsers, so a native parser's text, XHTML and metadata are what Tika's
//! path gives. [`parser_for`] routes a media type to its native parser, if it has one.

mod access;
mod apple_single;
mod archive;
mod auto_detect_reader;
mod chm;
mod cms;
mod crypt;
pub(crate) mod data_format;
mod dbf;
mod dc_xml;
mod dif;
mod djvu;
mod doc;
mod docx;
mod element_metadata;
pub(crate) mod embedded;
mod empty;
mod epub;
mod escher;
mod executable;
mod feed;
mod fiction_book;
mod font;
mod geogebra;
pub(crate) mod handlers;
pub(crate) mod html;
mod hwpx;
mod idml;
mod image;
mod iwork;
mod jdk_zip;
mod list_numbering;
mod mail;
mod markdown;
pub(crate) mod media;
pub(crate) mod metadata;
mod mif;
mod mobi;
mod ms_owner;
mod msg;
mod notebook;
mod odf;
mod ole10_native;
mod ole2;
mod onenote;
mod ooxml;
mod open_pgp;
#[cfg(test)]
pub(crate) mod parity;
pub(crate) mod pdf;
pub(crate) mod plist;
mod poi;
mod ppt;
mod pptx;
mod protected;
mod prt;
mod publisher;
#[cfg(test)]
pub(crate) mod reference;
mod rtf;
mod source_code;
mod subtitle;
mod text;
mod tnef;
mod translation;
mod warc;
mod wordperfect;
pub(crate) mod xhtml;
mod xls;
mod xlsx;
pub(crate) mod xml;
mod xml_2003;
pub(crate) mod xmp;
mod zip_file;

use std::borrow::Cow;
use std::fs;
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ParserConfigs;
use crate::embedded::{EmbeddedSpool, SpooledEmbedded};
use crate::errors::{Error, ExtractResult};
use crate::isolate::{heap_cap, isolate};
use crate::memory;
use crate::mime::{self, Detection, Origin};
use crate::tika::{self, Source};
use crate::{CharSet, Metadata};
use embedded::ParseContext;
use handlers::{AnchoredBlocks, BlockBreaks, Body, ToText, ToXml, WellFormed, Write, WriteLimit};
use metadata::TikaMetadata;
use xhtml::{ContentHandler, Stop, Xhtml};

/// The bytes a streamed extraction hands its reader at a time.
const CHUNK_BYTES: usize = 8192;
/// The chunks a streamed extraction writes ahead of its reader.
const CHUNKS_AHEAD: usize = 16;

/// Why a native parser leaves a document to Tika: what of it the port does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unported(pub(crate) String);

/// Why a native parser did not finish its document.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The output stopped taking content.
    Stopped(Stop),
    /// The document is not well-formed in its format, with what was wrong where.
    Malformed(String),
    /// An extraction the document depends on failed.
    Extraction(Error),
    /// A document embedded in it failed in a way that fails every document it is embedded in,
    /// as an exception Tika's embedded document extractor does not catch does.
    Embedded(Error),
}

impl From<Stop> for Failure {
    fn from(stop: Stop) -> Self {
        Self::Stopped(stop)
    }
}

/// Why Tika's path should extract a document rather than its native parser: a feature of the
/// document the parser does not handle as Tika does yet.
pub(crate) type Deferral = String;

/// Why a native parser writes nothing of a document.
#[derive(Debug)]
pub(crate) enum Declined {
    /// Tika's path should extract it.
    Deferred(Deferral),
    /// Tika's parser throws this on it before it starts the document: the extraction fails at
    /// once, without output, as Tika's path does.
    Fails(Error),
}

impl From<Deferral> for Declined {
    fn from(deferral: Deferral) -> Self {
        Self::Deferred(deferral)
    }
}

impl Unported {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl From<Unported> for Declined {
    fn from(Unported(reason): Unported) -> Self {
        Self::Deferred(reason)
    }
}

/// A document its parser has read as far as deciding it extracts it: what writes the document's
/// content as XHTML and adds the metadata it finds, holding what the parser read on the way, so
/// that nothing is read twice. The document is started before and ended after it.
pub(crate) type Prepared<'a> = Box<dyn FnOnce(&mut Xhtml<'_>) -> Result<(), Failure> + 'a>;

/// A format's native parser.
pub(crate) trait Parser: Sync {
    /// The name recorded in `tk:parsed-by` for the documents it parses.
    fn name(&self) -> &'static str;

    /// The class name of the parser Tika's path parses its documents with, which it replaces.
    #[cfg(test)]
    fn tika_name(&self) -> &'static str;

    /// The parsers it records delegating to, each native name paired with the class name of
    /// Tika's.
    #[cfg(test)]
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// The media types it parses, as Tika's detection names them.
    fn media_types(&self) -> &'static [&'static str];

    /// Whether it parses a type whose parameters its media types do not name, such as a
    /// `charset`, by the type without them, as Tika's `CompositeParser` walks to the type
    /// without its parameters when no parser takes the type with them.
    fn takes_base_types(&self) -> bool {
        true
    }

    /// Reads `input`, from the `metadata` Tika's parser starts from and under the parser
    /// settings `configs` the extraction was given, as far as deciding whether it extracts the
    /// document as the Tika parser it replaces does: what then writes it, or why it writes
    /// nothing. Nothing is written before the decision.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined>;

    /// Why Tika's path should extract `input`, if it should.
    #[cfg(test)]
    fn defers(
        &self,
        input: &[u8],
        metadata: &TikaMetadata,
        configs: &ParserConfigs,
    ) -> Option<Deferral> {
        match self.prepare(input, metadata, configs) {
            Err(Declined::Deferred(deferral)) => Some(deferral),
            Ok(_) | Err(Declined::Fails(_)) => None,
        }
    }
}

static PARSERS: &[&dyn Parser] = &[
    &access::JackcessParser,
    &archive::CompressorParser,
    &archive::PackageParser,
    &archive::RarParser,
    &archive::SevenZParser,
    &archive::ZipParser,
    &chm::ChmParser,
    &cms::CmsParser,
    &dc_xml::DcXmlParser,
    &doc::DocParser,
    &docx::DocxParser,
    &feed::FeedParser,
    &font::AdobeFontMetricParser,
    &font::TrueTypeParser,
    &fiction_book::FictionBookParser,
    &geogebra::GeoGebraParser,
    &html::JSoupParser,
    &image::HeifParser,
    &image::ImageParser,
    &image::JpegParser,
    &image::RecognizedImageParser,
    &image::TiffParser,
    &hwpx::HwpxParser,
    &idml::IdmlParser,
    &iwork::IworkArchiveParser,
    &mail::EmlxParser,
    &mail::MboxParser,
    &markdown::MarkdownParser,
    &media::FlacParser,
    &media::OpusParser,
    &media::VorbisParser,
    &media::MidiParser,
    &media::AudioParser,
    &media::FlvParser,
    &media::Mp3Parser,
    &media::Mp4Parser,
    &executable::ExecutableParser,
    &executable::UniversalExecutableParser,
    &translation::TmxParser,
    &translation::Xliff12Parser,
    &translation::XlzParser,
    &dif::DifParser,
    &djvu::DjvuParser,
    &mif::MifParser,
    &ms_owner::MsOwnerFileParser,
    &apple_single::AppleSingleFileParser,
    &prt::PrtParser,
    &dbf::DbfParser,
    &mobi::MobiParser,
    &msg::MsgParser,
    &notebook::NotebookParser,
    &epub::EpubParser,
    &odf::OpenDocumentParser,
    &odf::FlatOpenDocumentParser,
    &onenote::OneNoteParser,
    &open_pgp::OpenPgpParser,
    &pdf::PdfParser,
    &plist::PListParser,
    &ppt::PptParser,
    &pptx::PptxParser,
    &protected::ProtectedParser,
    &publisher::PublisherParser,
    &mail::Rfc822Parser,
    &rtf::RtfParser,
    &source_code::SourceCodeParser,
    &subtitle::SubtitleParser,
    &text::TextAndCsvParser,
    &tnef::TnefParser,
    &xls::XlsParser,
    &warc::WarcParser,
    &warc::HttpParser,
    &warc::WaczParser,
    &wordperfect::QuattroProParser,
    &wordperfect::WordPerfectParser,
    &xls::OldExcelParser,
    &xlsx::XlsxParser,
    &xml_2003::SpreadsheetMlParser,
    &xml_2003::WordMlParser,
    &empty::EmptyParser,
];

/// The native parser for `media_type`, if any, as Tika's `CompositeParser` looks one up: for
/// the type with its parameters, such as a version, then for the type without them, such as
/// `charset`.
pub(crate) fn parser_for(media_type: &str) -> Option<&'static dyn Parser> {
    let full = media_type.trim();
    let base = full.split(';').next().unwrap_or_default().trim();
    let serving = |name: &str, base: bool| {
        PARSERS
            .iter()
            .copied()
            .filter(|parser| !base || parser.takes_base_types())
            .find(|parser| parser.media_types().contains(&name))
    };
    serving(full, false).or_else(|| serving(base, true))
}

/// Whether any format has a native parser yet, which decides whether routing needs a detection.
fn any_parsers() -> bool {
    !PARSERS.is_empty()
}

/// What a document is and what Tika knows about it before its parser runs: the metadata of the
/// input (its resource name and length) and of its detection.
#[derive(Debug, Clone)]
pub(crate) struct Detected {
    pub(crate) media_type: String,
    pub(crate) metadata: TikaMetadata,
    pub(crate) file: InputFile,
}

/// The file Tika's `TikaInputStream` holds a document in, which a parser that asks it for a path
/// (`getPath`) is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputFile {
    /// The file the document was read from, at this path.
    Original(String),
    /// None yet: a document read from memory, which Tika spools to a temporary file of its own
    /// the first time a parser asks for a path.
    Spooled,
}

/// A document a native parser reads, and what its detection found.
pub(crate) struct Routed<'a> {
    pub(crate) parser: &'static dyn Parser,
    pub(crate) detected: Detected,
    pub(crate) input: Cow<'a, [u8]>,
}

/// Routes a document to its native parser, if its format has one. The document is detected in
/// Rust, without the JVM, so a document a native parser reads never starts it. Documents read
/// from a URL, files too large to read whole for their detection, and, while the Java heap is
/// capped, every document in a program that does not count its allocations, where a native
/// extraction cannot be held to the cap, stay with Tika. The parser itself may still leave the document to
/// Tika, when [`parse_to_string`] or [`parse_to_stream`] has it prepare the document.
///
/// A document Tika's detection throws on (a corrupt binary property list, say) is routed to
/// Tika's path, whose own detection then fails it exactly as Tika does.
pub(crate) fn route(source: Source<'_>) -> ExtractResult<Option<Routed<'_>>> {
    let unbudgeted = heap_cap().is_some() && !memory::counting();
    if matches!(source, Source::Url(_)) || !any_parsers() || unbudgeted {
        return Ok(None);
    }
    let Some(input) = detection_input(source)? else {
        return Ok(None);
    };
    let Ok(detected) = Detected::detect(source, &input) else {
        return Ok(None);
    };
    let Some(parser) = parser_for(&detected.media_type) else {
        return Ok(None);
    };
    Ok(Some(Routed {
        parser,
        detected,
        input,
    }))
}

/// The metadata key naming the type whose parser parses a document, whatever it is detected as.
const PARSER_OVERRIDE: &str = "tk:content-type-parser-override";

/// How the `TikaInputStream` of an embedded document was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opened {
    /// With its metadata, as `TikaInputStream.get(stream, tmp, metadata)` opens it: reading it
    /// to its end records its length there.
    WithMetadata,
    /// Alone, as `TikaInputStream.get(stream)` opens it: its metadata never learns its length.
    Alone,
}

/// A document embedded in one a native parser reads, as its native parser prepared it.
pub(crate) struct EmbeddedRoute<'a> {
    pub(crate) parser: &'static dyn Parser,
    /// The metadata the parser starts from.
    pub(crate) start: TikaMetadata,
    pub(crate) prepared: Prepared<'a>,
}

/// The native parser of a document embedded in one a native parser reads, with the metadata it
/// starts from, given its `data`, the `metadata` its embedding parser gives it and how its
/// stream was `opened`, as Tika's `AutoDetectParser` and `CompositeParser` route it: detected
/// with the resource name and `Content-Type` of its metadata as hints, then parsed by the parser
/// of the type the metadata overrides the parser with, else of the detected type. `None` when
/// that parser is not native, the document is empty, which Tika's zero-byte check fails, or
/// Tika's detection throws on it.
pub(crate) fn embedded_parser(
    data: &[u8],
    metadata: &TikaMetadata,
    opened: Opened,
) -> Option<(&'static dyn Parser, TikaMetadata)> {
    if data.is_empty() {
        return None;
    }
    let detected = Detected::detect_embedded(data, metadata, opened).ok()?;
    let parser = parser_for(
        metadata
            .get(PARSER_OVERRIDE)
            .unwrap_or(&detected.media_type),
    )?;
    let start = detected.metadata_for(parser);
    Some((parser, start))
}

/// Routes a document embedded in one a native parser reads to its [`embedded_parser`], and
/// returns what that parser prepared, if it takes the document. Otherwise Tika's path parses
/// the document, as it does one the parser defers and one Tika's parser fails before it starts,
/// as only Tika's path records that failure.
pub(crate) fn route_embedded<'a>(
    data: &'a [u8],
    metadata: &TikaMetadata,
    opened: Opened,
    configs: &'a ParserConfigs,
) -> Option<EmbeddedRoute<'a>> {
    let (parser, start) = embedded_parser(data, metadata, opened)?;
    let prepared = parser.prepare(data, &start, configs).ok()?;
    Some(EmbeddedRoute {
        parser,
        start,
        prepared,
    })
}

/// The largest file read whole to be detected in Rust: a larger one is left to Tika, which reads
/// only the parts of it its detectors look at.
const MAX_DETECTED_FILE: u64 = 64 * 1024 * 1024;

/// The bytes of a document read from a file or memory, which detection reads whole, or `None`
/// for a file larger than [`MAX_DETECTED_FILE`] or a document read from a URL.
fn detection_input(source: Source<'_>) -> ExtractResult<Option<Cow<'_, [u8]>>> {
    let unreadable =
        |path: &str, e: io::Error| Error::IoError(format!("Could not read {path}: {e}"));
    match source {
        Source::File(path) => {
            let length = fs::metadata(path).map_err(|e| unreadable(path, e))?.len();
            if length > MAX_DETECTED_FILE {
                return Ok(None);
            }
            fs::read(path)
                .map(|bytes| Some(Cow::Owned(bytes)))
                .map_err(|e| unreadable(path, e))
        }
        Source::Bytes(bytes) => Ok(Some(Cow::Borrowed(bytes))),
        Source::Url(_) => Ok(None),
    }
}

/// Detects the media type of a document as Tika's `DefaultDetector` does: in Rust for a file or
/// bytes, with Tika for a URL, which it fetches, for a file too large to read whole, and for a
/// document Tika's detection throws on, so the failure is Tika's own.
pub(crate) fn detect(source: Source<'_>) -> ExtractResult<String> {
    if let Some(input) = detection_input(source)? {
        if let Ok(detected) = Detected::detect(source, &input) {
            return Ok(detected.media_type);
        }
    }
    tika::detect(isolate()?, source)
}

/// `Path.getFileName().toString()` of a path as Java's `Paths.get` reads it on this platform:
/// the last name, trailing separators ignored.
pub(crate) fn java_file_name(path: &str) -> &str {
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let trimmed = path.trim_end_matches(separators);
    trimmed.rsplit(separators).next().unwrap_or(trimmed)
}

impl Detected {
    /// Detects `input`, read from `source`, with the metadata `TikaNativeMain.detect` holds after
    /// Tika's detection: for a file, the resource name and length `TikaInputStream.get(Path,
    /// metadata)` writes, then what the detectors write, in the order they write it.
    pub(crate) fn detect(source: Source<'_>, input: &[u8]) -> Result<Self, mime::DetectionError> {
        let mut metadata = TikaMetadata::new();
        let (origin, file) = match source {
            Source::File(path) => {
                let name = java_file_name(path);
                metadata.set("tk:resource-name", name);
                metadata.set("Content-Length", input.len().to_string());
                (Origin::File { name }, InputFile::Original(path.to_owned()))
            }
            Source::Bytes(_) | Source::Url(_) => (Origin::Bytes, InputFile::Spooled),
        };
        let Detection {
            media_type,
            metadata: entries,
        } = mime::detect(input, origin)?;
        for entry in entries {
            metadata.set(entry.key.name(), entry.value);
        }
        Ok(Self {
            media_type: media_type.to_string(),
            metadata,
            file,
        })
    }

    /// Detects an embedded document `input`, its stream `opened` as given, with the `metadata`
    /// its embedding parser gives it, which hints at its type and which the detection's
    /// metadata is added to.
    pub(crate) fn detect_embedded(
        input: &[u8],
        metadata: &TikaMetadata,
        opened: Opened,
    ) -> Result<Self, mime::DetectionError> {
        let origin = Origin::Embedded {
            name: metadata.get(embedded::RESOURCE_NAME),
            content_type: metadata.get("Content-Type"),
            learns_length: opened == Opened::WithMetadata,
        };
        let Detection {
            media_type,
            metadata: entries,
        } = mime::detect(input, origin)?;
        let mut metadata = metadata.clone();
        for entry in entries {
            metadata.set(entry.key.name(), entry.value);
        }
        Ok(Self {
            media_type: media_type.to_string(),
            metadata,
            file: InputFile::Spooled,
        })
    }

    /// The metadata a parser starts from, as Tika's `AutoDetectParser` and `CompositeParser`
    /// leave it: the detected `Content-Type` and the parser in `tk:parsed-by`.
    pub(crate) fn metadata_for(&self, parser: &dyn Parser) -> TikaMetadata {
        let mut metadata = self.metadata.clone();
        metadata.set("Content-Type", self.media_type.clone());
        metadata.add("tk:parsed-by", parser.name());
        metadata
    }
}

fn run<'h>(
    parser: &dyn Parser,
    prepared: Prepared<'_>,
    metadata: TikaMetadata,
    context: &'h ParseContext<'h>,
    handler: &'h mut dyn ContentHandler,
) -> (TikaMetadata, Result<(), Failure>) {
    let (mut metadata, record, result) = Xhtml::write(handler, metadata, context, prepared);
    record.finish(&mut metadata, parser.name());
    let result = result.and_then(|()| xhtml::within_budget().map_err(Failure::from));
    if result.is_ok() && context.configs().password.is_some() {
        metadata.set(PASSWORD_USED, "true");
    }
    (metadata, result)
}

/// What our Java `LegacyOfficePasswordParser` records once a document a password was given for
/// is parsed, and its `PasswordProvider` once a parser asks it for the password.
pub(crate) const PASSWORD_USED: &str = "extractous:password-used";

/// Whether a prepared document still keeps to its memory budget, before anything of it is
/// written.
fn prepared_within_budget(parser: &dyn Parser) -> ExtractResult<()> {
    xhtml::within_budget().map_err(|stop| failure_error(parser, Failure::Stopped(stop)))
}

/// Runs a native extraction's `work`, its preparation and writing, held to the cap on the Java
/// heap, if one is set.
fn within_budget<R>(work: impl FnOnce() -> R) -> R {
    match heap_cap() {
        Some(limit) => memory::within(limit, work),
        None => work(),
    }
}

fn failure_error(parser: &dyn Parser, failure: Failure) -> Error {
    match failure {
        Failure::Malformed(reason) => Error::ParseError(format!(
            "{} could not parse the document: {reason}",
            parser.name()
        )),
        Failure::Stopped(Stop::MemoryLimit(limit)) => Error::MemoryLimit(limit),
        Failure::Stopped(stop) => Error::ParseError(format!(
            "{} stopped writing the document: {stop:?}",
            parser.name()
        )),
        Failure::Extraction(error) | Failure::Embedded(error) => error,
    }
}

/// What a parse writes: the text of the body, or the whole document as XHTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Output {
    Text,
    Xml,
}

/// The handlers extractous puts behind a parser. Both entry points hold back anchored blocks,
/// which only Tika's streamed extraction (`ParsingInputStream`) does: its string extraction
/// (`TikaNativeMain.parseToString`) leaves them where they cut the paragraph they are anchored
/// in (#63).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chain {
    /// A stream's: XML declaring its `encoding`.
    Stream {
        output: Output,
        encoding: &'static str,
    },
    /// A string's: at most `max_length` UTF-16 code units of text.
    String { output: Output, max_length: usize },
    /// `TikaNativeMain.parseToString`'s, without anchored blocks held back, which the parity
    /// tests compare a native parser's string extraction with Tika's under (#63).
    #[cfg(test)]
    TikaString { output: Output, max_length: usize },
}

impl Chain {
    /// Runs `write` with the handler at the head of this chain, writing to `out`.
    pub(crate) fn write_to<W: Write, R>(
        self,
        out: &mut W,
        write: impl FnOnce(&mut dyn ContentHandler) -> R,
    ) -> R {
        match self {
            Chain::Stream {
                output: Output::Xml,
                encoding,
            } => write(&mut AnchoredBlocks::new(WellFormed::new(ToXml::new(
                out,
                Some(encoding),
            )))),
            Chain::Stream {
                output: Output::Text,
                ..
            } => write(&mut Body::new(AnchoredBlocks::new(BlockBreaks::new(
                ToText::new(out),
            )))),
            Chain::String {
                output: Output::Xml,
                max_length,
            } => write(&mut AnchoredBlocks::new(WriteLimit::new(
                WellFormed::new(ToXml::new(out, None)),
                max_length,
            ))),
            Chain::String {
                output: Output::Text,
                max_length,
            } => write(&mut Body::new(AnchoredBlocks::new(BlockBreaks::new(
                WriteLimit::new(ToText::new(out), max_length),
            )))),
            #[cfg(test)]
            Chain::TikaString {
                output: Output::Xml,
                max_length,
            } => write(&mut WriteLimit::new(
                WellFormed::new(ToXml::new(out, None)),
                max_length,
            )),
            #[cfg(test)]
            Chain::TikaString {
                output: Output::Text,
                max_length,
            } => write(&mut Body::new(BlockBreaks::new(WriteLimit::new(
                ToText::new(out),
                max_length,
            )))),
        }
    }

    /// The limit of the text this chain writes, if it has one.
    fn max_length(self) -> Option<usize> {
        match self {
            Chain::Stream { .. } => None,
            Chain::String { max_length, .. } => Some(max_length),
            #[cfg(test)]
            Chain::TikaString { max_length, .. } => Some(max_length),
        }
    }
}

/// Collects written text into a string.
#[derive(Debug, Default)]
pub(crate) struct StringOut(pub(crate) String);

impl Write for StringOut {
    fn write(&mut self, text: &str) -> Result<(), Stop> {
        self.0.push_str(text);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Stop> {
        Ok(())
    }
}

/// Parses `input` into a string of at most `max_length` UTF-16 code units of text, through the
/// handlers of [`Chain::String`]; `None` when the parser leaves the document to Tika.
pub(crate) fn parse_to_string(
    parser: &dyn Parser,
    input: &[u8],
    detected: &Detected,
    configs: &ParserConfigs,
    max_length: usize,
    output: Output,
) -> ExtractResult<Option<(String, Metadata)>> {
    parse_to_string_through(
        parser,
        input,
        detected,
        configs,
        Chain::String { output, max_length },
    )
}

/// [`parse_to_string`] through the handlers of `chain`, a string extraction's.
pub(crate) fn parse_to_string_through(
    parser: &dyn Parser,
    input: &[u8],
    detected: &Detected,
    configs: &ParserConfigs,
    chain: Chain,
) -> ExtractResult<Option<(String, Metadata)>> {
    let max_length = chain
        .max_length()
        .expect("a string extraction's chain limits its text");
    within_budget(|| {
        let metadata = detected.metadata_for(parser);
        let prepared = match parser.prepare(input, &metadata, configs) {
            Ok(prepared) => prepared,
            Err(Declined::Deferred(_)) => return Ok(None),
            Err(Declined::Fails(error)) => return Err(error),
        };
        prepared_within_budget(parser)?;
        let mut out = StringOut::default();
        let context = ParseContext::new(configs)
            .reading(detected.file.clone())
            .with_write_limit(max_length);
        let (metadata, result) = chain.write_to(&mut out, |handler| {
            run(parser, prepared, metadata, &context, handler)
        });
        match result {
            Ok(()) | Err(Failure::Stopped(Stop::LimitReached)) => {
                Ok(Some((out.0, metadata.to_map())))
            }
            Err(failure) => Err(failure_error(parser, failure)),
        }
    })
}

enum Message {
    /// A full chunk of output, written while the parse goes on.
    Chunk(Vec<u8>),
    /// The output after the last full chunk, how the parse ended, and the embedded documents
    /// it spooled.
    Done {
        tail: Vec<u8>,
        outcome: ExtractResult<Metadata>,
        spooled: Vec<SpooledEmbedded>,
    },
}

/// Encodes written text with the stream's charset, as Java's `OutputStreamWriter` does, and
/// hands it to the reader a full chunk at a time; what is left at the end goes with the outcome.
struct ChannelOut {
    sender: SyncSender<Message>,
    char_set: CharSet,
    buffer: Vec<u8>,
}

impl ChannelOut {
    fn send(&mut self) -> Result<(), Stop> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK_BYTES));
        self.sender
            .send(Message::Chunk(chunk))
            .map_err(|_| Stop::ReaderGone)
    }
}

impl Write for ChannelOut {
    fn write(&mut self, text: &str) -> Result<(), Stop> {
        encode(text, self.char_set, &mut self.buffer);
        if self.buffer.len() >= CHUNK_BYTES {
            self.send()?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Stop> {
        Ok(())
    }
}

/// Appends `text` encoded in `char_set`; a character US-ASCII cannot hold becomes `?`, as Java's
/// encoders replace it.
pub(crate) fn encode(text: &str, char_set: CharSet, out: &mut Vec<u8>) {
    match char_set {
        CharSet::UTF_8 => out.extend_from_slice(text.as_bytes()),
        CharSet::US_ASCII => out.extend(text.chars().map(|character| {
            if character.is_ascii() {
                character as u8
            } else {
                b'?'
            }
        })),
        CharSet::UTF_16BE => out.extend(text.encode_utf16().flat_map(u16::to_be_bytes)),
    }
}

fn char_set_name(char_set: CharSet) -> &'static str {
    match char_set {
        CharSet::UTF_8 => "UTF-8",
        CharSet::US_ASCII => "US-ASCII",
        CharSet::UTF_16BE => "UTF-16BE",
    }
}

/// How a native parse is streamed.
#[derive(Debug, Clone)]
pub(crate) struct StreamOptions {
    pub(crate) char_set: CharSet,
    pub(crate) output: Output,
    pub(crate) timeout: Option<Duration>,
    /// Where embedded documents are copied instead of parsed, if anywhere.
    pub(crate) spool: Option<EmbeddedSpool>,
}

/// Parses `input` on a thread of its own into a stream encoded with the options' charset,
/// through the handlers `ParsingInputStream` uses; `None` when the parser leaves the document to
/// Tika. Like `ParsingInputStream`, it returns once the parse has written its first chunk or
/// ended, or with the failure of a parse that ended without writing anything. It returns the
/// metadata known before the parse, of the input and its detection, whatever the parse has
/// found by then: the final metadata comes with [`NativeStream::metadata`] once the stream is
/// read. Tika's path returns a copy of the metadata made as the first chunk is written, the
/// final metadata for a document whose output fits in one chunk (#67).
pub(crate) fn parse_to_stream(
    parser: &'static dyn Parser,
    input: Vec<u8>,
    detected: Detected,
    configs: ParserConfigs,
    options: StreamOptions,
) -> ExtractResult<Option<(NativeStream, Metadata)>> {
    let deadline = options.timeout.map(|timeout| Instant::now() + timeout);
    let start = detected.metadata_for(parser);
    let known = start.to_map();
    let (sender, receiver) = mpsc::sync_channel(CHUNKS_AHEAD);
    let (decided, decision) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!("extractous {}", parser.name()))
        .spawn(move || {
            within_budget(|| {
                let metadata = start;
                let prepared = parser.prepare(&input, &metadata, &configs);
                let _ = decided.send(!matches!(prepared, Err(Declined::Deferred(_))));
                let prepared = match prepared {
                    Ok(prepared) => prepared_within_budget(parser).map(|()| prepared),
                    Err(Declined::Deferred(_)) => return,
                    Err(Declined::Fails(error)) => Err(error),
                };
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let _ = sender.send(Message::Done {
                            tail: Vec::new(),
                            outcome: Err(error),
                            spooled: Vec::new(),
                        });
                        return;
                    }
                };
                let mut out = ChannelOut {
                    sender: sender.clone(),
                    char_set: options.char_set,
                    buffer: Vec::with_capacity(CHUNK_BYTES),
                };
                let chain = Chain::Stream {
                    output: options.output,
                    encoding: char_set_name(options.char_set),
                };
                let context = match &options.spool {
                    Some(spool) => ParseContext::spooling(&configs, spool),
                    None => ParseContext::new(&configs),
                }
                .reading(detected.file.clone());
                let (metadata, result) = chain.write_to(&mut out, |handler| {
                    run(parser, prepared, metadata, &context, handler)
                });
                let outcome = match result {
                    Ok(()) => Ok(metadata.to_map()),
                    Err(Failure::Stopped(Stop::ReaderGone)) => return,
                    Err(failure) => Err(failure_error(parser, failure)),
                };
                let tail = std::mem::take(&mut out.buffer);
                let _ = sender.send(Message::Done {
                    tail,
                    outcome,
                    spooled: context.into_spooled(),
                });
            })
        })
        .map_err(|e| {
            Error::IoError(format!(
                "Could not start the {} parser's thread: {e}",
                parser.name()
            ))
        })?;
    let mut stream = NativeStream {
        receiver,
        chunk: Vec::new(),
        position: 0,
        deadline,
        outcome: None,
        spooled: Vec::new(),
    };
    let prepared = receive(
        &decision,
        deadline,
        "The native parser's thread ended before it prepared its document",
    );
    match prepared {
        Ok(true) => {}
        Ok(false) => return Ok(None),
        Err(Error::Timeout) => {
            stream.outcome = Some(Err(Error::Timeout));
            return Ok(Some((stream, known)));
        }
        Err(error) => return Err(error),
    }
    match stream.next_message() {
        Ok(Message::Done {
            tail,
            outcome: Err(error),
            ..
        }) if tail.is_empty() => return Err(error),
        Ok(message) => stream.accept(message),
        Err(Error::Timeout) => stream.outcome = Some(Err(Error::Timeout)),
        Err(error) => return Err(error),
    }
    Ok(Some((stream, known)))
}

/// The text of a native parse, read as it is written.
pub(crate) struct NativeStream {
    receiver: Receiver<Message>,
    chunk: Vec<u8>,
    position: usize,
    deadline: Option<Instant>,
    outcome: Option<ExtractResult<Metadata>>,
    spooled: Vec<SpooledEmbedded>,
}

impl NativeStream {
    /// The final metadata, once the stream has been read to its end.
    pub(crate) fn metadata(&self) -> ExtractResult<Metadata> {
        match &self.outcome {
            Some(Ok(metadata)) => Ok(metadata.clone()),
            Some(Err(error)) => Err(copy(error)),
            None => Err(Error::StreamNotFinished),
        }
    }

    /// The embedded documents the parse copied to the spool, once the stream has been read to
    /// its end.
    pub(crate) fn embedded(&self) -> ExtractResult<Vec<SpooledEmbedded>> {
        self.metadata().map(|_| self.spooled.clone())
    }

    fn accept(&mut self, message: Message) {
        match message {
            Message::Chunk(chunk) => self.chunk = chunk,
            Message::Done {
                tail,
                outcome,
                spooled,
            } => {
                self.chunk = tail;
                self.outcome = Some(outcome);
                self.spooled = spooled;
            }
        }
        self.position = 0;
    }

    fn next_message(&self) -> ExtractResult<Message> {
        receive(
            &self.receiver,
            self.deadline,
            "The native parser's thread ended without finishing its document",
        )
    }
}

/// A copy of the error a native extraction ended with, which the stream reports each time it
/// is asked: the same kind with the same message.
fn copy(error: &Error) -> Error {
    match error {
        Error::IoError(message) => Error::IoError(message.clone()),
        Error::Encrypted(message) => Error::Encrypted(message.clone()),
        Error::LimitExceeded(message) => Error::LimitExceeded(message.clone()),
        Error::Timeout => Error::Timeout,
        Error::MemoryLimit(limit) => Error::MemoryLimit(*limit),
        other => Error::ParseError(other.to_string()),
    }
}

/// What the parser's thread sends next, waiting at most until `deadline`; `ended` says what a
/// thread that ended without sending it did not do.
fn receive<T>(receiver: &Receiver<T>, deadline: Option<Instant>, ended: &str) -> ExtractResult<T> {
    let disconnected = || Error::ParseError(ended.to_owned());
    match deadline {
        None => receiver.recv().map_err(|_| disconnected()),
        Some(deadline) => {
            let wait = deadline.saturating_duration_since(Instant::now());
            receiver.recv_timeout(wait).map_err(|e| match e {
                RecvTimeoutError::Timeout => Error::Timeout,
                RecvTimeoutError::Disconnected => disconnected(),
            })
        }
    }
}

impl Read for NativeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.position == self.chunk.len() {
            if let Some(outcome) = &self.outcome {
                return match outcome {
                    Ok(_) => Ok(0),
                    Err(error) => Err(io::Error::other(copy(error))),
                };
            }
            match self.next_message() {
                Ok(message) => self.accept(message),
                Err(Error::Timeout) => self.outcome = Some(Err(Error::Timeout)),
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        let count = buf.len().min(self.chunk.len() - self.position);
        buf[..count].copy_from_slice(&self.chunk[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::process::Command;

    use super::*;
    use crate::Extractor;

    const CHILD: &str = "EXTRACTOUS_NATIVE_ROUTING_CHILD";
    const TEST: &str = "formats::tests::native_documents_never_start_the_jvm";

    /// Extracts and detects documents native parsers read, in every way the extractor offers,
    /// in a process of its own: the test runs itself again, so no other test has created the
    /// isolate first, and checks that none of it did.
    #[test]
    fn native_documents_never_start_the_jvm() {
        if std::env::var_os(CHILD).is_none() {
            let exe = std::env::current_exe().expect("the test binary");
            let child = Command::new(exe)
                .args(["--exact", TEST, "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .expect("the test binary runs");
            let report = String::from_utf8_lossy(&child.stdout);
            assert!(
                child.status.success() && report.contains(&format!("test {TEST} ... ok")),
                "the routing child failed: {report}{}",
                String::from_utf8_lossy(&child.stderr)
            );
            return;
        }
        let documents = "../test_files/documents";
        let extractor = Extractor::new();
        let (text, metadata) = extractor
            .extract_file_to_string(&format!("{documents}/book.fb2"))
            .unwrap();
        assert!(!text.is_empty());
        assert_eq!(
            metadata.get("tk:parsed-by").map(Vec::as_slice),
            Some(["extractous::FictionBookParser".to_owned()].as_slice())
        );
        let odt = std::fs::read(format!("{documents}/simple.odt")).unwrap();
        let (text, _) = extractor.extract_bytes_to_string(&odt).unwrap();
        assert!(!text.is_empty());
        let (mut stream, _) = extractor
            .extract_file(&format!("{documents}/simple.odt"))
            .unwrap();
        let mut streamed = String::new();
        stream.read_to_string(&mut streamed).unwrap();
        assert!(!streamed.is_empty());
        assert_eq!(
            extractor
                .detect_file(&format!("{documents}/2022_Q3_AAPL.pdf"))
                .unwrap()
                .as_str(),
            "application/pdf"
        );
        assert_eq!(
            extractor.detect_bytes(&odt).unwrap().as_str(),
            "application/vnd.oasis.opendocument.text"
        );
        assert!(!crate::isolate::isolate_created());
    }

    /// Documents of native formats that embed documents spool them as Tika's path does, with a
    /// budget that holds them all and one that runs out partway.
    #[test]
    fn spools_embedded_documents_as_tikas_path_does() {
        // science-exploration-1p.pptx is not compared: its slides are read otherwise than
        // Tika's path reads them on purpose (#102).
        for name in ["nested.zip", "vodafone.xlsx"] {
            let path = std::path::PathBuf::from("../test_files/documents").join(name);
            for budget in [u64::MAX, 3000] {
                // The ZIP's integrity check now reaches its head (#99).
                let departure = (name == "nested.zip")
                    .then_some((crate::formats::archive::as_tika_checks_zips as _, None));
                let spooled =
                    crate::formats::parity::spools_as_tika_rewriting(&path, budget, departure, &[]);
                assert!(matches!(spooled, Ok(1..)), "{spooled:?}");
            }
        }
    }

    /// Every document of Tika's tests a native parser reads spools its embedded documents as
    /// Tika's path does.
    #[test]
    #[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
    fn spools_tikas_documents_as_tikas_path_does() {
        let corpus = crate::formats::parity::conformance_corpus();
        let exceptions = [
            (
                "testZSTD.zst",
                crate::formats::parity::ZSTD_WITHOUT_ZSTD_JNI,
            ),
            (
                "testPSD_xmp.psd",
                crate::formats::parity::PSD_PROFILE_AWT_REJECTS,
            ),
            (
                "testHTML_metadata_two_titles.html",
                "a meta title after the title element is a further dc:title (#86)",
            ),
            (
                "testZipEncrypted.zip",
                "Tika writes the name of an encrypted entry as text (#99)",
            ),
            (
                "testEPUB_multi-metadata-vals.epub",
                "Tika writes the pictures outside the spine in HashMap order (#88)",
            ),
            (
                "testControlCharacters.doc",
                "Word's tables, lists and fields are read otherwise than Tika's path reads them (#111)",
            ),
            (
                "testWORD_2006ml.doc",
                "Word's tables, lists and fields are read otherwise than Tika's path reads them (#111)",
            ),
            (
                "testWORD_features.doc",
                "Word's tables, lists and fields are read otherwise than Tika's path reads them (#111)",
            ),
            (
                "testJBIG2.jb2",
                "Tika records the JBIG2 dimension node twice (#120)",
            ),
            (
                "CVLKRA-KYC_Download_File_Structure_V3.1.xlsx",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "protectedSheets.xlsx",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_charts.xlsx",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_diagramData.xlsx",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_hyperlinks.xlsx",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL-charts.xls",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_extra_metadata.xls",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_headers_footers.xls",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_hyperlinks.xls",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_labels-govdocs-515858.xls",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "LibreOfficeImpress_odp_1.3.odp",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "test-columnar.ods",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testMasterFooter.odp",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testNPEOpenDocument.odt",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testODFwithOOo3.odt",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testODPMacro.fodp",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testODPMacro.odp",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testODP_NPE.odp",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testODT-TIKA-6000.odt",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testODTStyles3.odt",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testOpenOffice2.odf",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testStarOffice-6.0-writer-template.stw",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testStarOffice-6.0-writer.sxw",
                "OpenDocument headings, notes, styles and namespaces are read otherwise than Tika's path reads them (#69, #90, #91)",
            ),
            (
                "testPPT.potm",
                "slides' comments, placeholders and links are read otherwise than Tika's path reads them (#102)",
            ),
            (
                "testPPT_autodate.pptx",
                "slides' comments, placeholders and links are read otherwise than Tika's path reads them (#102)",
            ),
            (
                "testPPT_various.pptx",
                "slides' comments, placeholders and links are read otherwise than Tika's path reads them (#102)",
            ),
            (
                "testPPT_various2.pptx",
                "slides' comments, placeholders and links are read otherwise than Tika's path reads them (#102)",
            ),
            (
                "testWPSAttachment.pptx",
                "slides' comments, placeholders and links are read otherwise than Tika's path reads them (#102)",
            ),
            (
                "test_embedded_zip.pptx",
                "slides' comments, placeholders and links are read otherwise than Tika's path reads them (#102)",
            ),
            (
                "testPDF_multiFormatEmbFiles.pdf",
                "its embedded files are written in object order, once each (#109)",
            ),
            (
                "complex.mbox",
                "its parts keep their whole content type (#75), and its messages their own headers (#103)",
            ),
            (
                "testEmailWithPNGAtt.eml",
                "its parts keep their whole content type (#75), and its messages their own headers (#103)",
            ),
            (
                "testRFC822_oddfrom",
                "its quoted-printable parts keep their own line ends (#73)",
            ),
        ];
        let mut differences = Vec::new();
        let mut spooling = 0;
        // The head of CMS content shows its Content-Length, which Tika's path does not give it
        // (#113); CMS content is parsed in place, never spooled, and its comparison with Tika's
        // is cms.rs's.
        for parser in PARSERS
            .iter()
            .filter(|parser| parser.name() != cms::CmsParser.name())
        {
            let (parsed, _) = crate::formats::parity::documents(*parser, &[&corpus]);
            for path in parsed {
                let name = path.file_name().unwrap().to_string_lossy();
                if exceptions.iter().any(|(exception, _)| *exception == name) {
                    continue;
                }
                // The fixes of #89, #99, #105 and #118 depart from Tika's path systematically.
                let head: crate::formats::parity::Normalisation =
                    crate::formats::dc_xml::without_head;
                let departure: Option<(crate::formats::parity::NativeRewrite, _)> = match parser
                    .name()
                {
                    "extractous::ZipParser" | "extractous::CmsParser" => {
                        Some((crate::formats::archive::as_tika_checks_zips, None))
                    }
                    "extractous::DcXMLParser" | "extractous::MobiParser" => Some((
                        crate::formats::dc_xml::without_unread_dublin_core,
                        Some(head),
                    )),
                    "extractous::EpubParser" => {
                        Some((crate::formats::epub::as_tika_writes_it, None))
                    }
                    "extractous::MP4Parser" => {
                        Some((crate::formats::media::mp4_as_tika_reads_it, None))
                    }
                    "extractous::DIFParser" => {
                        Some((crate::formats::dif::as_tika_keeps_last_pieces, None))
                    }
                    "extractous::MIFParser" => Some((crate::formats::mif::as_tika_writes_it, None)),
                    _ => None,
                };
                // The fixes of #73, #77, #79 and #112 leave values of mail metadata uncompared.
                let mut departures: Vec<crate::formats::parity::Departure> = match parser.name() {
                    "extractous::RFC822Parser"
                    | "extractous::EmlxParser"
                    | "extractous::MboxParser" => crate::formats::mail::tests::DEPARTURES.to_vec(),
                    "extractous::OfficeParser" => {
                        crate::formats::msg::tests::RECIPIENT_NAMES.to_vec()
                    }
                    _ => Vec::new(),
                };
                if crate::formats::mail::tests::OTHER_SENDER_NAMES.contains(&name.as_ref()) {
                    departures.push(crate::formats::parity::Departure::Values(
                        "message:from-name",
                        "#77",
                    ));
                }
                for budget in [u64::MAX, 3000] {
                    match crate::formats::parity::spools_as_tika_rewriting(
                        &path,
                        budget,
                        departure,
                        &departures,
                    ) {
                        Ok(0) => {}
                        Ok(_) => spooling += 1,
                        Err(difference) => differences.push(difference),
                    }
                }
            }
        }
        assert!(differences.is_empty(), "{}", differences.join("\n"));
        assert!(spooling > 0);
    }

    /// A document parsed with a password is marked as our Java's `LegacyOfficePasswordParser`
    /// marks it, and one parsed without none is not.
    #[test]
    fn marks_what_is_parsed_with_a_password() {
        let path = "../test_files/documents/simple.odt";
        let routed = route(Source::File(path)).unwrap().unwrap();
        let password = crate::config::Password::new("secret");
        let used = |configs: &ParserConfigs| {
            let (_, metadata) = parse_to_string(
                routed.parser,
                &routed.input,
                &routed.detected,
                configs,
                100_000,
                Output::Text,
            )
            .unwrap()
            .unwrap();
            metadata.get(PASSWORD_USED).cloned()
        };
        assert_eq!(used(&ParserConfigs::default()), None);
        assert_eq!(
            used(&ParserConfigs::default().with_password(Some(&password))),
            Some(vec!["true".to_owned()])
        );
    }

    #[test]
    fn a_native_extraction_over_its_memory_budget_fails_and_the_next_runs() {
        let path = "../test_files/documents/simple.odt";
        let routed = route(Source::File(path)).unwrap().unwrap();
        let configs = ParserConfigs::default();
        let extract = || {
            parse_to_string(
                routed.parser,
                &routed.input,
                &routed.detected,
                &configs,
                100_000,
                Output::Text,
            )
        };
        let capped = memory::within(1024, extract);
        assert!(
            matches!(capped, Err(Error::MemoryLimit(1024))),
            "{capped:?}"
        );
        let (text, _) = extract().unwrap().unwrap();
        assert!(text.contains("These are a few of my favorite things"));
    }

    /// A stream comes with the metadata known before the parse, however much output the parse
    /// writes before the stream returns, and the parse's own once it has been read (#67).
    #[test]
    fn a_stream_comes_with_the_metadata_known_before_the_parse() {
        let path = "../test_files/documents/simple.odt";
        let routed = route(Source::File(path)).unwrap().unwrap();
        let known = routed.detected.metadata_for(routed.parser).to_map();
        let options = StreamOptions {
            char_set: CharSet::UTF_8,
            output: Output::Text,
            timeout: None,
            spool: None,
        };
        let (mut stream, metadata) = parse_to_stream(
            routed.parser,
            routed.input.into_owned(),
            routed.detected,
            ParserConfigs::default(),
            options,
        )
        .unwrap()
        .unwrap();
        assert_eq!(metadata, known);
        assert!(!metadata.contains_key("tk:parsed-by-full-set"));
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        assert!(text.len() < CHUNK_BYTES, "the output fits in one chunk");
        let parsed = stream.metadata().unwrap();
        assert!(parsed.contains_key("tk:parsed-by-full-set"), "{parsed:?}");
    }

    #[test]
    fn names_files_as_java_does() {
        assert_eq!(java_file_name("a/b/report.pdf"), "report.pdf");
        assert_eq!(java_file_name("report.pdf"), "report.pdf");
        assert_eq!(java_file_name("a/b/"), "b");
        assert_eq!(java_file_name("a/.."), "..");
    }

    #[test]
    fn detects_files_with_the_metadata_tika_opens_them_with() {
        let path = "../test_files/documents/simple.odt";
        let input = std::fs::read(path).unwrap();
        let detected = Detected::detect(Source::File(path), &input).unwrap();
        assert_eq!(
            detected.media_type,
            "application/vnd.oasis.opendocument.text"
        );
        let names: Vec<&str> = detected.metadata.names().collect();
        for name in [
            "tk:resource-name",
            "Content-Length",
            "tk:content-type-magic-detected",
            "zip:detector-zip-file-opened",
        ] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
        assert_eq!(
            detected.metadata.get("tk:resource-name"),
            Some("simple.odt")
        );
        let bytes = Detected::detect(Source::Bytes(&input), &input).unwrap();
        assert_eq!(bytes.metadata.get("tk:resource-name"), None);
        assert_eq!(
            bytes.metadata.get("Content-Length"),
            Some(input.len().to_string().as_str())
        );
    }
}
