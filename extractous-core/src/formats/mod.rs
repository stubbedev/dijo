//! Extraction in Rust, format by format (#41): the native parsers gathered, on the common ground
//! of `extractous_base::formats`. [`parser_for`] routes a media type to its native parser, if it
//! has one.

#[cfg(all(test, feature = "tika"))]
pub(crate) mod audit;
mod composite;
#[cfg(any(test, feature = "parity"))]
pub mod large;
#[cfg(any(test, feature = "parity"))]
pub mod parity;
#[cfg(all(test, feature = "tika"))]
mod url_tests;

use std::borrow::Cow;
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::Instant;

pub(crate) use crate::mime::{Detected, OpenedFrom};
pub(crate) use extractous_base::formats::*;
pub(crate) use extractous_html::formats::html;
pub(crate) use extractous_html::formats::text;
pub(crate) use extractous_image::formats::djvu;
pub(crate) use extractous_image::formats::image;
pub(crate) use extractous_image::formats::metafile;
pub(crate) use extractous_mail::formats::archive;
pub(crate) use extractous_mail::formats::cms;
pub(crate) use extractous_mail::formats::mail;
#[cfg(not(feature = "static-image"))]
pub(crate) use extractous_mail::formats::sqlite;
pub(crate) use extractous_mail::formats::warc;
pub(crate) use extractous_media::formats::media;
pub(crate) use extractous_odf::formats::epub;
pub(crate) use extractous_odf::formats::odf;
pub(crate) use extractous_office::formats::access;
pub(crate) use extractous_office::formats::compound;
pub(crate) use extractous_office::formats::dgn;
pub(crate) use extractous_office::formats::doc;
pub(crate) use extractous_office::formats::msg;
pub(crate) use extractous_office::formats::ppt;
pub(crate) use extractous_office::formats::pst;
pub(crate) use extractous_office::formats::publisher;
pub(crate) use extractous_office::formats::rtf;
pub(crate) use extractous_office::formats::tnef;
pub(crate) use extractous_office::formats::visio;
pub(crate) use extractous_office::formats::xls;
pub(crate) use extractous_ooxml::formats::docx;
pub(crate) use extractous_ooxml::formats::pptx;
pub(crate) use extractous_ooxml::formats::protected;
pub(crate) use extractous_ooxml::formats::vsdx;
pub(crate) use extractous_ooxml::formats::xlsx;
pub(crate) use extractous_ooxml::formats::xps;
pub(crate) use extractous_pdf::formats::font;
pub(crate) use extractous_pdf::formats::pdf;
pub(crate) use extractous_small::formats::apple_single;
pub(crate) use extractous_small::formats::chm;
pub(crate) use extractous_small::formats::class_file;
pub(crate) use extractous_small::formats::dbf;
pub(crate) use extractous_small::formats::dc_xml;
pub(crate) use extractous_small::formats::dif;
pub(crate) use extractous_small::formats::dwg;
pub(crate) use extractous_small::formats::empty;
pub(crate) use extractous_small::formats::executable;
pub(crate) use extractous_small::formats::feed;
pub(crate) use extractous_small::formats::fiction_book;
pub(crate) use extractous_small::formats::geogebra;
pub(crate) use extractous_small::formats::hwpx;
pub(crate) use extractous_small::formats::idml;
pub(crate) use extractous_small::formats::iwork;
pub(crate) use extractous_small::formats::iwork_package;
pub(crate) use extractous_small::formats::markdown;
pub(crate) use extractous_small::formats::mat;
pub(crate) use extractous_small::formats::mif;
pub(crate) use extractous_small::formats::mobi;
pub(crate) use extractous_small::formats::ms_owner;
pub(crate) use extractous_small::formats::notebook;
pub(crate) use extractous_small::formats::onenote;
pub(crate) use extractous_small::formats::open_pgp;
pub(crate) use extractous_small::formats::plist;
pub(crate) use extractous_small::formats::prt;
pub(crate) use extractous_small::formats::sas;
pub(crate) use extractous_small::formats::source_code;
pub(crate) use extractous_small::formats::subtitle;
pub(crate) use extractous_small::formats::translation;
pub(crate) use extractous_small::formats::tsd;
pub(crate) use extractous_small::formats::wordperfect;
pub(crate) use extractous_small::formats::xml_2003;

use crate::config::ParserConfigs;
use crate::embedded::{EmbeddedSpool, SpooledEmbedded};
use crate::errors::{Error, ExtractResult, Unsupported};
use crate::fetch::{self, Located};
use crate::input::{self, Input, Source};
use crate::isolate::heap_cap;
#[cfg(feature = "tika")]
use crate::isolate::isolate;
use crate::memory;
use crate::mime::{self, Intent, MediaType};
#[cfg(feature = "tika")]
use crate::tika;
use crate::{CharSet, Metadata};
use embedded::ParseContext;
use handlers::Write;
use metadata::TikaMetadata;
use secure::Secure;
use xhtml::{ContentHandler, Stop, Xhtml};

/// The bytes a streamed extraction hands its reader at a time.
const CHUNK_BYTES: usize = 8192;
/// The chunks a streamed extraction writes ahead of its reader.
const CHUNKS_AHEAD: usize = 16;

static PARSERS: &[&dyn Parser] = &[
    &access::JackcessParser,
    &archive::CompressorParser,
    &archive::PackageParser,
    &archive::RarParser,
    &archive::SevenZParser,
    &archive::ZipParser,
    &chm::ChmParser,
    &class_file::ClassParser,
    &cms::CmsParser,
    &compound::CompoundDocumentParser,
    &dc_xml::DcXmlParser,
    &dgn::Dgn8Parser,
    &doc::DocParser,
    &docx::DocxParser,
    &docx::flat::Word2006MlParser,
    &dwg::DwgParser,
    &feed::FeedParser,
    &font::AdobeFontMetricParser,
    &font::TrueTypeParser,
    &fiction_book::FictionBookParser,
    &geogebra::GeoGebraParser,
    &html::JSoupParser,
    &image::BpgParser,
    &image::HeifParser,
    &image::IcnsParser,
    &image::ImageParser,
    &image::JpegParser,
    &image::JxlParser,
    &image::RawTiffParser,
    &image::RecognizedImageParser,
    &image::TiffParser,
    &hwpx::HwpxParser,
    &idml::IdmlParser,
    &iwork::IworkArchiveParser,
    &iwork_package::IworkPackageParser,
    &mail::EmlxParser,
    &metafile::EmfParser,
    &metafile::WmfParser,
    &mail::MboxParser,
    &markdown::MarkdownParser,
    &mat::MatParser,
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
    &pst::OutlookPstParser,
    &publisher::PublisherParser,
    &mail::Rfc822Parser,
    &rtf::RtfParser,
    &sas::Sas7bdatParser,
    &source_code::SourceCodeParser,
    #[cfg(not(feature = "static-image"))]
    &sqlite::Sqlite3Parser,
    &subtitle::SubtitleParser,
    &text::TextAndCsvParser,
    &tnef::TnefParser,
    &tsd::TsdParser,
    &visio::VisioParser,
    &xls::XlsParser,
    &warc::WarcParser,
    &warc::HttpParser,
    &warc::WaczParser,
    &wordperfect::OldQuattroProParser,
    &wordperfect::QuattroProParser,
    &wordperfect::WordPerfectParser,
    &xls::OldExcelParser,
    &xlsx::XlsxParser,
    &xml_2003::SpreadsheetMlParser,
    &xml_2003::WordMlParser,
    &xlsx::XlsbParser,
    &vsdx::VsdxParser,
    &xps::XpsParser,
    &empty::EmptyParser,
];

/// The native parser for a document of `media_type`, picked as `CompositeParser.getParser` picks
/// Tika's: the parser of the first type of its [`mime::lineage`] a parser reads, else Tika's
/// fallback, the [`empty::EmptyParser`]. A supertype's parser takes the document only where it
/// reads that type's subtypes. Where Tika's parser for a type is not native, and at a type the
/// default types do not declare, whose parser in Tika's composite parser only a parse could
/// tell, Tika's path reads the document; but for the undeclared types Tika's detection names
/// that it is known to read with no parser.
fn parser_for(media_type: &str) -> Result<&'static dyn Parser, Unsupported> {
    install_routes(&Registry);
    let Some(parsed) = MediaType::parse(media_type) else {
        return Ok(&empty::EmptyParser);
    };
    let mut lineage = mime::lineage(&parsed).peekable();
    let own = lineage
        .peek()
        .map(MediaType::base_type)
        .expect("a lineage starts with the type itself");
    for kind in lineage {
        let reads = |parser: &&dyn Parser| {
            let types = if kind.base_type() == own {
                parser.media_types()
            } else {
                parser.media_types_with_subtypes()
            };
            types.contains(&kind.as_str())
        };
        if let Some(parser) = PARSERS.iter().copied().find(reads) {
            return Ok(parser);
        }
        if composite::is_unparsed_undeclared(&kind) {
            return Ok(&empty::EmptyParser);
        }
        if !mime::declares(&kind) || composite::tika_reads(&kind) {
            return Err(Unsupported::Format(media_type.to_owned()));
        }
    }
    Ok(&empty::EmptyParser)
}

/// The native parsers gathered, which route the documents embedded in those they read.
pub(crate) struct Registry;

impl Routes for Registry {
    fn embedded_parser(
        &self,
        data: &[u8],
        metadata: &TikaMetadata,
        opened: Opened,
    ) -> Result<(&'static dyn Parser, TikaMetadata), Unrouted> {
        if data.is_empty() {
            return Err(Unrouted::Empty);
        }
        let detected = Detected::detect_embedded(data, metadata, opened)
            .map_err(|error| Unrouted::Unsupported(Unsupported::Undetected(error.to_string())))?;
        let parsed_as = metadata
            .get(embedded::PARSER_OVERRIDE)
            .unwrap_or(&detected.media_type);
        let parser = parser_for(parsed_as).map_err(Unrouted::Unsupported)?;
        let start = detected.metadata_for(parser);
        Ok((parser, start))
    }

    #[cfg(feature = "tika")]
    fn parse_with_tika(
        &self,
        #[cfg_attr(
            not(test),
            expect(unused_variables, reason = "only the tests record why")
        )]
        bridged: Bridged,
        data: &[u8],
        pairs: &[(&str, &str)],
        configs: &ParserConfigs,
    ) -> ExtractResult<tika::EmbeddedEvents> {
        #[cfg(test)]
        audit::bridged(bridged);
        isolate().and_then(|isolate| tika::parse_embedded(isolate, data, pairs, configs))
    }
}

/// A document opened from its source, for its detection and native parse.
#[derive(Debug)]
pub(crate) struct Document<'a> {
    pub(crate) from: OpenedFrom<'a>,
    pub(crate) input: Input<'a>,
}

/// Opens the document `source` names: reads the file, takes the bytes, or fetches the document
/// at the URL within `deadline`, the extraction's time limit, if it has one.
pub(crate) fn open(source: Source<'_>, deadline: Option<Instant>) -> ExtractResult<Document<'_>> {
    match source {
        Source::File(path) => open_file(Cow::Borrowed(path)),
        Source::Bytes(bytes) => Ok(Document {
            from: OpenedFrom::Bytes,
            input: Input::Borrowed(bytes),
        }),
        Source::Url(url) => match fetch::locate(url, deadline)? {
            Located::File(path) => open_file(Cow::Owned(path)),
            Located::Fetched(fetched) => Ok(Document {
                from: OpenedFrom::Url(fetched.headers),
                input: fetched.body,
            }),
        },
    }
}

fn open_file(path: Cow<'_, str>) -> ExtractResult<Document<'_>> {
    let input = input::read_file(&path)?;
    Ok(Document {
        from: OpenedFrom::File(path),
        input,
    })
}

/// Opens the document `source` names for a native extraction, as [`open`] does, unless the
/// extraction cannot be native: while the memory is capped, in a program that does not count
/// its allocations, a native extraction cannot be held to the cap, and Tika's path, whose heap
/// the cap bounds, extracts every document from its source.
pub(crate) fn open_to_extract(
    source: Source<'_>,
    deadline: Option<Instant>,
) -> ExtractResult<Result<Document<'_>, Unsupported>> {
    if heap_cap().is_some() && !memory::counting() {
        return Ok(Err(Unsupported::UncountedMemoryCap));
    }
    open(source, deadline).map(Ok)
}

/// A document a native parser reads, and what its detection found.
pub struct Routed<'a> {
    pub(crate) parser: &'static dyn Parser,
    pub detected: Detected,
    pub input: Input<'a>,
}

/// Routes a document to its native parser, if its format has one, or says why it stays with
/// Tika. The document is detected in Rust, without the JVM, so a document a native parser reads
/// never starts it. In a build with Tika, a file too large to read whole goes to its native
/// parser only if that parser's memory stays bounded whatever its input's size, and otherwise to
/// Tika, which streams it; a build without Tika has every parser read it mapped, under the
/// extraction's memory budget. The parser itself may still leave the document to Tika, when [`parse_to_string`] or
/// [`parse_to_stream`] has it prepare the document.
///
/// A document Tika's detection throws on (a corrupt binary property list, say) is routed to
/// Tika's path, whose own detection then fails it exactly as Tika does.
pub(crate) fn route<'a>(document: &Document<'a>) -> Result<Routed<'a>, Unsupported> {
    let detected = Detected::detect(&document.from, &document.input, Intent::Parse)
        .map_err(|error| Unsupported::Undetected(error.to_string()))?;
    let parser = parser_for(&detected.media_type)?;
    #[cfg(feature = "tika")]
    if document.input.is_mapped() && parser.footprint() == Footprint::Proportional {
        return Err(Unsupported::TooLarge {
            parser: parser.name(),
            limit: input::MAX_READ_WHOLE,
        });
    }
    Ok(Routed {
        parser,
        detected,
        input: document.input.clone(),
    })
}

/// Routes the file at `path` as an extraction of it does.
#[cfg(any(test, feature = "parity"))]
pub fn route_file(path: &str) -> Result<Routed<'_>, Unsupported> {
    route(&open(Source::File(path), None).expect("the file opens"))
}

/// Detects the media type of a document as Tika's `DefaultDetector` does: in Rust, and with Tika
/// for a document Tika's detection throws on, so the failure is Tika's own. A build without Tika
/// fails with why Tika's detection throws.
pub(crate) fn detect(source: Source<'_>) -> ExtractResult<String> {
    let document = open(source, None)?;
    match Detected::detect(&document.from, &document.input, Intent::Detect) {
        Ok(detected) => Ok(detected.media_type),
        #[cfg(feature = "tika")]
        Err(_) => tika::detect(isolate()?, source),
        #[cfg(not(feature = "tika"))]
        Err(error) => Err(Unsupported::Undetected(error.to_string()).into()),
    }
}

/// What a native parser prepared of a document that the extraction takes.
struct Taken<'a> {
    prepared: Prepared<'a>,
    /// Where the parser reads the document differently from Tika's parser, in a build without
    /// Tika, which takes the native reading and records the difference.
    #[cfg(not(feature = "tika"))]
    difference: Option<Difference>,
}

/// Why the extraction writes nothing of what a native parser prepared of a document, which the
/// parser reads.
enum Refused {
    /// The extraction fails at once, without output, as Tika's path does.
    Fails(Error),
    /// The parser writes no document of it and leaves this metadata.
    Unwritten(TikaMetadata),
}

/// What the extraction takes of what `parser` `prepared` of a document, or why Tika's path
/// should extract it, which a build without Tika fails with. A reading that differs from Tika's
/// parser's is left to Tika's path in a build with Tika, and taken in a build without it.
fn take<'a>(
    parser: &dyn Parser,
    prepared: Result<Prepared<'a>, Decline<'a>>,
) -> Result<Result<Taken<'a>, Refused>, Unsupported> {
    let leave_to_tika = |reason| Unsupported::Deferred {
        parser: parser.name(),
        reason,
    };
    match prepared {
        Ok(prepared) => Ok(Ok(Taken {
            prepared,
            #[cfg(not(feature = "tika"))]
            difference: None,
        })),
        #[cfg(feature = "tika")]
        Err(Decline::Differs { difference, .. }) => Err(leave_to_tika(difference.0)),
        #[cfg(not(feature = "tika"))]
        Err(Decline::Differs {
            reading,
            difference,
        }) => Ok(Ok(Taken {
            prepared: reading,
            difference: Some(difference),
        })),
        Err(Decline::Deferred(reason)) => Err(leave_to_tika(reason)),
        Err(Decline::Fails(error)) => Ok(Err(Refused::Fails(error))),
        Err(Decline::Unwritten(metadata)) => Ok(Err(Refused::Unwritten(metadata))),
    }
}

/// Writes the document of `bytes` bytes that `parser` prepared, as the extraction `taken` it, to
/// `handler`, through the `SecureContentHandler` Tika's `AutoDetectParser` puts in front of it.
/// A reading that differs from Tika's parser's records the difference as a warning.
fn run<'h>(
    parser: &dyn Parser,
    taken: Taken<'_>,
    metadata: TikaMetadata,
    bytes: usize,
    context: &'h ParseContext<'h>,
    handler: &'h mut dyn ContentHandler,
) -> (TikaMetadata, Result<(), Failure>) {
    let mut secure = Secure::new(handler, bytes);
    let (mut metadata, record, result) =
        Xhtml::write(&mut secure, metadata, context, taken.prepared);
    record.finish(&mut metadata, parser.name());
    #[cfg(not(feature = "tika"))]
    if let Some(difference) = taken.difference {
        embedded::record_exception(&mut metadata, &difference.warning(parser.name()));
    }
    let result = result.and_then(|()| xhtml::within_budget().map_err(Failure::from));
    if result.is_ok() {
        mark_password_used(&mut metadata, context.configs());
    }
    (metadata, result)
}

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
        Failure::Stopped(Stop::ZipBomb(bomb)) => Error::LimitExceeded(bomb.to_string()),
        Failure::Stopped(stop) => Error::ParseError(format!(
            "{} stopped writing the document: {stop:?}",
            parser.name()
        )),
        Failure::Extraction(error) | Failure::Embedded(error) => error,
    }
}

/// Parses `input` into a string of at most `max_length` UTF-16 code units of text, through the
/// handlers of [`Chain::String`]; why not when the parser leaves the document to Tika.
pub fn parse_to_string(
    parser: &dyn Parser,
    input: &Input<'_>,
    detected: &Detected,
    configs: &ParserConfigs,
    max_length: usize,
    output: Output,
) -> ExtractResult<Result<(String, Metadata), Unsupported>> {
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
    input: &Input<'_>,
    detected: &Detected,
    configs: &ParserConfigs,
    chain: Chain,
) -> ExtractResult<Result<(String, Metadata), Unsupported>> {
    let mut out = StringOut::default();
    let parsed = parse_through(parser, input, detected, configs, chain, &mut out)?;
    Ok(parsed.map(|metadata| (out.0, metadata)))
}

/// Parses `input` on this thread through the handlers of `chain`, a string extraction's, into
/// `out`: the final metadata, or why not when the parser leaves the document to Tika.
pub(crate) fn parse_through<W: Write>(
    parser: &dyn Parser,
    input: &Input<'_>,
    detected: &Detected,
    configs: &ParserConfigs,
    chain: Chain,
    out: &mut W,
) -> ExtractResult<Result<Metadata, Unsupported>> {
    let max_length = chain
        .max_length()
        .expect("a string extraction's chain limits its text");
    within_budget(|| {
        let metadata = detected.metadata_for(parser);
        let prepared = parser.prepare_paged(input, input.pages(), &metadata, configs);
        let taken = match take(parser, prepared) {
            Ok(Ok(taken)) => taken,
            Err(unsupported) => return Ok(Err(unsupported)),
            Ok(Err(Refused::Fails(error))) => return Err(error),
            Ok(Err(Refused::Unwritten(metadata))) => {
                let metadata = unwritten(parser.name(), metadata, configs);
                return Ok(Ok(metadata.to_map()));
            }
        };
        prepared_within_budget(parser)?;
        let context = ParseContext::new(configs)
            .reading(detected.file.clone())
            .paging(input.pages())
            .with_write_limit(max_length);
        let (metadata, result) = chain.write_to(out, |handler| {
            run(parser, taken, metadata, input.len(), &context, handler)
        });
        match result {
            Ok(()) | Err(Failure::Stopped(Stop::LimitReached)) => Ok(Ok(metadata.to_map())),
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

    fn flush(&mut self) -> Result<(), Stop> {
        self.send()
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
    /// When reading the stream fails with [`Error::Timeout`], if it is limited.
    pub(crate) deadline: Option<Instant>,
    /// Where embedded documents are copied instead of parsed, if anywhere.
    pub(crate) spool: Option<EmbeddedSpool>,
}

/// Parses `input` on a thread of its own into a stream encoded with the options' charset,
/// through the handlers `ParsingInputStream` uses; why not when the parser leaves the document
/// to Tika. Like `ParsingInputStream`, it returns once the parse has written its first chunk or
/// ended, or with the failure of a parse that ended without writing anything. It returns the
/// metadata known before the parse, of the input and its detection, whatever the parse has
/// found by then: the final metadata comes with [`NativeStream::metadata`] once the stream is
/// read. Tika's path returns a copy of the metadata made as the first chunk is written, the
/// final metadata for a document whose output fits in one chunk (#67).
pub(crate) fn parse_to_stream(
    parser: &'static dyn Parser,
    input: Input<'static>,
    detected: Detected,
    configs: ParserConfigs,
    options: StreamOptions,
) -> ExtractResult<Result<(NativeStream, Metadata), Unsupported>> {
    let deadline = options.deadline;
    let start = detected.metadata_for(parser);
    let known = start.to_map();
    let (sender, receiver) = mpsc::sync_channel(CHUNKS_AHEAD);
    let (decided, decision) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!("extractous {}", parser.name()))
        .spawn(move || {
            within_budget(|| {
                let metadata = start;
                let prepared = parser.prepare_paged(&input, input.pages(), &metadata, &configs);
                let taken = match take(parser, prepared) {
                    Ok(taken) => {
                        let _ = decided.send(Ok(()));
                        taken
                    }
                    Err(unsupported) => {
                        let _ = decided.send(Err(unsupported));
                        return;
                    }
                };
                let taken = match taken {
                    Ok(taken) => prepared_within_budget(parser).map(|()| taken),
                    Err(Refused::Fails(error)) => Err(error),
                    Err(Refused::Unwritten(metadata)) => {
                        let metadata = unwritten(parser.name(), metadata, &configs);
                        let _ = sender.send(Message::Done {
                            tail: Vec::new(),
                            outcome: Ok(metadata.to_map()),
                            spooled: Vec::new(),
                        });
                        return;
                    }
                };
                let taken = match taken {
                    Ok(taken) => taken,
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
                .reading(detected.file.clone())
                .paging(input.pages());
                let (metadata, result) = chain.write_to(&mut out, |handler| {
                    run(parser, taken, metadata, input.len(), &context, handler)
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
        Ok(Ok(())) => {}
        Ok(Err(unsupported)) => return Ok(Err(unsupported)),
        Err(Error::Timeout) => {
            stream.outcome = Some(Err(Error::Timeout));
            return Ok(Ok((stream, known)));
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
    Ok(Ok((stream, known)))
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
    #[cfg(feature = "tika")]
    use std::process::Command;

    use super::*;
    use crate::formats::embedded::Wrapping;
    #[cfg(feature = "tika")]
    use crate::Extractor;

    #[test]
    fn closes_an_embedded_document_the_output_stops_in() {
        install_routes(&Registry);
        let configs = ParserConfigs::default();
        let mut entry = TikaMetadata::new();
        entry.set("tk:resource-name", "lines.txt");
        let text = "A line of text the write limit cuts short.\n".repeat(4);
        let mut out = StringOut::default();
        let chain = Chain::String {
            output: Output::Xml,
            max_length: 100,
        };
        let result = chain.write_to(&mut out, |handler| {
            Xhtml::write(
                handler,
                TikaMetadata::new(),
                &ParseContext::new(&configs),
                |xhtml| xhtml.parse_embedded(text.as_bytes(), &entry, Wrapping::PackageEntry),
            )
            .2
        });
        assert!(matches!(result, Err(Failure::Stopped(_))));
        assert!(out.0.ends_with("</p></div>"), "{}", out.0);
    }

    /// An embedded HTML document the length limit stops in its body ends its own document,
    /// whose `</body>` stays out of the embedding document, which Tika's path writes (#113).
    #[test]
    fn keeps_the_body_end_of_an_embedded_document_cut_short_out() {
        install_routes(&Registry);
        let configs = ParserConfigs::default();
        let mut entry = TikaMetadata::new();
        entry.set("tk:resource-name", "page.html");
        let html = format!(
            "<html><head><title>t</title></head><body><p>{}</p></body></html>",
            "Words the write limit cuts short. ".repeat(8)
        );
        let mut out = StringOut::default();
        let chain = Chain::String {
            output: Output::Xml,
            max_length: 100,
        };
        let result = chain.write_to(&mut out, |handler| {
            Xhtml::write(
                handler,
                TikaMetadata::new(),
                &ParseContext::new(&configs),
                |xhtml| xhtml.parse_embedded(html.as_bytes(), &entry, Wrapping::PackageEntry),
            )
            .2
        });
        assert!(matches!(result, Err(Failure::Stopped(_))));
        assert!(!out.0.contains("</body>"), "{}", out.0);
        assert!(out.0.ends_with("</p></div>"), "{}", out.0);
    }

    /// A native parser's failure on an embedded document is recorded in the embedding
    /// document's metadata on one line, naming the parser, the document and why, and the
    /// embedding document goes on (#68).
    #[test]
    fn records_a_native_parsers_failure_on_an_embedded_document_concisely() {
        install_routes(&Registry);
        let configs = ParserConfigs::default();
        let mut entry = TikaMetadata::new();
        entry.set("tk:resource-name", "broken.xml");
        // XML whose elements do not nest, which the native parser fails on.
        let xml = br#"<?xml version="1.0"?><a><b>text</a>"#;
        let mut out = StringOut::default();
        let chain = Chain::Stream {
            output: Output::Text,
            encoding: "UTF-8",
        };
        let (mut metadata, record, result) = chain.write_to(&mut out, |handler| {
            Xhtml::write(
                handler,
                TikaMetadata::new(),
                &ParseContext::new(&configs),
                |xhtml| {
                    xhtml.parse_embedded(xml, &entry, Wrapping::PackageEntry)?;
                    xhtml.element("p", "After.").map_err(Failure::from)
                },
            )
        });
        result.unwrap();
        record.finish(&mut metadata, "outer");
        let [failure] = metadata.values("tk:exception:embedded-exception") else {
            panic!("{metadata:?}");
        };
        assert!(
            failure.starts_with("extractous::")
                && failure.contains(" could not parse the embedded document \"broken.xml\": ")
                && !failure.contains('\n'),
            "{failure}"
        );
        assert!(out.0.contains("After."), "{}", out.0);
    }

    #[cfg(feature = "tika")]
    const CHILD: &str = "EXTRACTOUS_NATIVE_ROUTING_CHILD";
    #[cfg(feature = "tika")]
    const TEST: &str = "formats::tests::native_documents_never_start_the_jvm";

    /// Extracts and detects documents native parsers read, in every way the extractor offers,
    /// in a process of its own: the test runs itself again, so no other test has created the
    /// isolate first, and checks that none of it did.
    #[cfg(feature = "tika")]
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
    #[cfg(feature = "tika")]
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
    #[cfg(feature = "tika")]
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
                "testZipEncrypted.zip",
                "Tika writes the name of an encrypted entry as text (#99)",
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
                "testWORD_numbered_list.doc",
                "a text box is written where it is anchored, not after the main text (#111)",
            ),
            (
                "testWORD_various.doc",
                "a text box is written where it is anchored, not after the main text (#111)",
            ),
            (
                "testJBIG2.jb2",
                "Tika records the JBIG2 dimension node twice (#120)",
            ),
            (
                "testQuattro.wq1",
                "Tika's path fails on Quattro Pro 1 to 6, which the native parser reads (#158)",
            ),
            (
                "testQuattro.wq2",
                "Tika's path fails on Quattro Pro 1 to 6, which the native parser reads (#158)",
            ),
            (
                "testQuattro.wb1",
                "Tika's path fails on Quattro Pro 1 to 6, which the native parser reads (#158)",
            ),
            (
                "testQuattro.wb2",
                "Tika's path fails on Quattro Pro 1 to 6, which the native parser reads (#158)",
            ),
            (
                "testFODS_embeddedChart.fods",
                "a flat document's master pages and version are read, which Tika's path leaves out (#69, #91)",
            ),
            (
                "testODSMacro.fods",
                "a flat document's master pages and version are read, which Tika's path leaves out (#69, #91)",
            ),
            (
                "testODTMacro.fodt",
                "a flat document's master pages and version are read, which Tika's path leaves out (#69, #91)",
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
                "testEXCEL_charts.xlsb",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_diagramData.xlsb",
                "the sheets' links, charts, protection and formats are read otherwise than Tika's path reads them (#92, #93, #95, #107)",
            ),
            (
                "testEXCEL_various.xlsb",
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
            (
                "testMHTMLFirefox.mhtml",
                "its quoted-printable parts keep their own line ends (#73), which moves where a \
                 budget cuts the spool; mail::tests compares its spool unbudgeted",
            ),
            archive::TIKAS_SPLIT_SEGMENT,
        ];
        let compared_otherwise = [
            crate::formats::html::TWO_TITLES,
            crate::formats::image::TIKAS_TEST_BMP,
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
                if exceptions.iter().any(|(exception, _)| *exception == name)
                    || compared_otherwise.contains(&name.as_ref())
                {
                    continue;
                }
                let walked = (parser.name() == onenote::OneNoteParser.name())
                    .then(|| onenote::walked(&name))
                    .flatten();
                // The fixes of #89, #99, #105, #118, #129, #132 and #156 depart from Tika's path
                // systematically.
                let head: crate::formats::parity::Normalisation =
                    crate::formats::dc_xml::without_head;
                let departure: Option<(crate::formats::parity::NativeRewrite, _)> = match parser
                    .name()
                {
                    _ if walked.is_some() => None,
                    "extractous::ZipParser" | "extractous::CmsParser" => {
                        Some((crate::formats::archive::as_tika_checks_zips, None))
                    }
                    "extractous::DcXMLParser" | "extractous::MobiParser" => Some((
                        crate::formats::dc_xml::without_unread_dublin_core,
                        Some(head),
                    )),
                    "extractous::JackcessParser" => {
                        let (rewrite, normalise) = access::as_tika_writes(&path);
                        Some((rewrite, Some(normalise)))
                    }
                    "extractous::EpubParser" => {
                        Some((crate::formats::epub::as_tika_writes_it, None))
                    }
                    "extractous::MP4Parser" => {
                        Some((crate::formats::media::mp4_as_tika_reads_it, None))
                    }
                    "extractous::DIFParser" => {
                        Some((crate::formats::dif::as_tika_keeps_last_pieces, None))
                    }
                    "extractous::MatParser" => Some((crate::formats::mat::as_jmatio_names, None)),
                    "extractous::MIFParser" => Some((crate::formats::mif::as_tika_writes_it, None)),
                    "extractous::OneNoteParser" => Some((
                        crate::formats::onenote::as_tika_names_packaged_objects,
                        None,
                    )),
                    "extractous::ClassParser" => Some((
                        crate::formats::class_file::as_written,
                        Some(crate::formats::class_file::without_misread_access),
                    )),
                    "extractous::BPGParser" => {
                        Some((crate::formats::image::as_tika_misreads_the_header, None))
                    }
                    "extractous::JXLParser" => Some((
                        crate::formats::image::as_tika_writes_no_jpeg_xl_document,
                        None,
                    )),
                    _ => None,
                };
                // The fixes of #73, #77, #79 and #112 leave values of mail metadata uncompared, that of
                // #118 the source language XLIFF stores as `original`, and that of #124 the page
                // counts Tika's IDMLParser adds up across the documents it parses, so that the
                // second parse of a document, under a byte budget, counts twice.
                let mut departures: Vec<crate::formats::parity::Departure> = match parser.name() {
                    "extractous::RFC822Parser"
                    | "extractous::EmlxParser"
                    | "extractous::MboxParser" => crate::formats::mail::departures_of(&path),
                    "extractous::OfficeParser" => crate::formats::msg::RECIPIENT_NAMES.to_vec(),
                    "extractous::XLIFF12Parser" | "extractous::XLZParser" => {
                        crate::formats::translation::SOURCE_LANGUAGE_AS_ORIGINAL.to_vec()
                    }
                    "extractous::IDMLParser" => {
                        crate::formats::idml::ACCUMULATED_PAGE_COUNTS.to_vec()
                    }
                    "extractous::ChmParser" => crate::formats::chm::DEPARTURES.to_vec(),
                    "extractous::TSDParser" => crate::formats::tsd::DEPARTURES.to_vec(),
                    _ => walked.unwrap_or_default().to_vec(),
                };
                if parser.name() == pdf::PdfParser.name() && name == pdf::MALFORMED_XFA {
                    departures.extend_from_slice(pdf::XFA_FAILURE);
                }
                if parser.media_types() == ppt::PptParser.media_types() {
                    departures.extend_from_slice(ppt::DEPARTURES);
                }
                if parser.media_types() == protected::ProtectedParser.media_types() {
                    departures.extend_from_slice(protected::DEPARTURES);
                }
                if epub::IN_HASH_ORDER.contains(&name.as_ref()) {
                    departures.extend_from_slice(epub::MANIFEST_ORDER);
                }
                if crate::formats::parity::ANCHORED_BLOCK_DOCUMENTS.contains(&name.as_ref()) {
                    departures.extend_from_slice(crate::formats::parity::ANCHORED_BLOCKS);
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
        let routed = route_file(path).unwrap();
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
        let routed = route_file(path).unwrap();
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

    /// A parser that reads every document as the paragraph "native", differently from Tika's
    /// parser in "a detail".
    struct DifferingParser;

    impl Parser for DifferingParser {
        fn name(&self) -> &'static str {
            "extractous::DifferingParser"
        }
        fn tika_name(&self) -> &'static str {
            "org.apache.tika.parser.EmptyParser"
        }
        fn media_types(&self) -> &'static [&'static str] {
            &[]
        }
        fn prepare<'a>(
            &self,
            _input: &'a [u8],
            _metadata: &TikaMetadata,
            _configs: &'a ParserConfigs,
        ) -> Result<Prepared<'a>, Decline<'a>> {
            unless_different(
                Box::new(|xhtml| Ok(xhtml.element("p", "native")?)),
                Some(Difference::new("a detail")),
            )
        }
    }

    /// A reading that differs from Tika's parser's is left to Tika's path in a build with Tika,
    /// and taken in a build without it, which records the difference as a warning, in a string
    /// extraction and a stream alike (#144).
    #[test]
    fn takes_a_reading_that_differs_from_tikas_only_without_tika() {
        let routed = route_file("../test_files/documents/simple.odt").unwrap();
        let parsed = parse_to_string(
            &DifferingParser,
            &routed.input,
            &routed.detected,
            &ParserConfigs::default(),
            100_000,
            Output::Text,
        )
        .unwrap();
        let options = StreamOptions {
            char_set: CharSet::UTF_8,
            output: Output::Text,
            deadline: None,
            spool: None,
        };
        let streamed = parse_to_stream(
            &DifferingParser,
            routed.input.to_static(),
            routed.detected,
            ParserConfigs::default(),
            options,
        )
        .unwrap();
        #[cfg(feature = "tika")]
        {
            let left = Unsupported::Deferred {
                parser: "extractous::DifferingParser",
                reason: "a detail".to_owned(),
            };
            assert_eq!(parsed.unwrap_err(), left);
            assert_eq!(streamed.err(), Some(left));
        }
        #[cfg(not(feature = "tika"))]
        {
            let warning = vec![
                "extractous::DifferingParser read the document differently from Tika: a detail\n"
                    .to_owned(),
            ];
            let (text, metadata) = parsed.unwrap();
            assert_eq!(text.trim(), "native");
            assert_eq!(metadata.get(embedded::EXCEPTION_WARNING), Some(&warning));
            let (mut stream, _) = streamed.unwrap();
            let mut text = String::new();
            stream.read_to_string(&mut text).unwrap();
            assert_eq!(text.trim(), "native");
            let metadata = stream.metadata().unwrap();
            assert_eq!(metadata.get(embedded::EXCEPTION_WARNING), Some(&warning));
        }
    }

    /// A stream comes with the metadata known before the parse, however much output the parse
    /// writes before the stream returns, and the parse's own once it has been read (#67).
    #[test]
    fn a_stream_comes_with_the_metadata_known_before_the_parse() {
        let path = "../test_files/documents/simple.odt";
        let routed = route_file(path).unwrap();
        let known = routed.detected.metadata_for(routed.parser).to_map();
        let options = StreamOptions {
            char_set: CharSet::UTF_8,
            output: Output::Text,
            deadline: None,
            spool: None,
        };
        let (mut stream, metadata) = parse_to_stream(
            routed.parser,
            routed.input.to_static(),
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

    /// A file at `path` of `length` bytes that starts with `head`, the rest a hole the file
    /// system keeps no data for, which reads as zeros.
    fn sparse(path: &std::path::Path, head: &[u8], length: u64) {
        std::fs::write(path, head).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(length)
            .unwrap();
    }

    /// The header of a 16-bit stereo PCM WAVE file of `data` bytes of samples.
    fn wave_header(data: u32) -> Vec<u8> {
        let mut wave = b"RIFF".to_vec();
        wave.extend((36 + data).to_le_bytes());
        wave.extend(b"WAVEfmt ");
        wave.extend(16u32.to_le_bytes());
        wave.extend(1u16.to_le_bytes());
        wave.extend(2u16.to_le_bytes());
        wave.extend(44_100u32.to_le_bytes());
        wave.extend((44_100u32 * 4).to_le_bytes());
        wave.extend(4u16.to_le_bytes());
        wave.extend(16u16.to_le_bytes());
        wave.extend(b"data");
        wave.extend(data.to_le_bytes());
        wave
    }

    /// The boxes of an MP4 file before its media data: its type, a movie of `seconds` at a
    /// time scale of 1000, and the header of a media data box of `data` bytes.
    fn mp4_header(seconds: u32, data: u64) -> Vec<u8> {
        let mut mp4 = 20u32.to_be_bytes().to_vec();
        mp4.extend(b"ftypisom");
        mp4.extend(0x200u32.to_be_bytes());
        mp4.extend(b"isom");
        mp4.extend(116u32.to_be_bytes());
        mp4.extend(b"moov");
        mp4.extend(108u32.to_be_bytes());
        mp4.extend(b"mvhd");
        mp4.extend([0; 12]);
        mp4.extend(1000u32.to_be_bytes());
        mp4.extend((seconds * 1000).to_be_bytes());
        mp4.extend(0x0001_0000u32.to_be_bytes());
        mp4.extend(0x0100u16.to_be_bytes());
        mp4.extend([0; 10]);
        for entry in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            mp4.extend(entry.to_be_bytes());
        }
        mp4.extend([0; 24]);
        mp4.extend(2u32.to_be_bytes());
        mp4.extend(1u32.to_be_bytes());
        mp4.extend(b"mdat");
        mp4.extend((16 + data).to_be_bytes());
        mp4
    }

    /// Files too large to read whole, in a directory of their own removed when they are dropped:
    /// a WAVE file, an MP4 file, a PDF and an RTF document of 2 GiB each.
    struct LargeFiles {
        dir: std::path::PathBuf,
        wave: String,
        mp4: String,
        pdf: String,
        rtf: String,
    }

    impl LargeFiles {
        fn new(test: &str) -> Self {
            let dir = crate::isolate::temp_dir()
                .join(format!("extractous-large-{test}-{}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            let large = 2u64 << 30;
            let path = |name: &str| dir.join(name).to_str().unwrap().to_owned();
            let (wave, mp4, pdf, rtf) = (
                path("long.wav"),
                path("long.mp4"),
                path("long.pdf"),
                path("long.rtf"),
            );
            let samples = u32::try_from(large).unwrap() - 44;
            sparse(wave.as_ref(), &wave_header(samples), large);
            let header = mp4_header(7200, 0);
            sparse(
                mp4.as_ref(),
                &mp4_header(7200, large - header.len() as u64),
                large,
            );
            sparse(pdf.as_ref(), b"%PDF-1.4\n", large);
            sparse(rtf.as_ref(), b"{\\rtf1 ", large);
            Self {
                dir,
                wave,
                mp4,
                pdf,
                rtf,
            }
        }
    }

    impl Drop for LargeFiles {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).unwrap();
        }
    }

    /// A file too large to read whole is mapped, and goes to its native parser where that
    /// parser reads only bounded parts of its input; a parser whose memory grows with its input
    /// leaves it to Tika, which streams it, or in a build without Tika reads it mapped too.
    #[test]
    fn files_too_large_to_read_whole_go_to_parsers_of_bounded_memory() {
        let files = LargeFiles::new("routing");
        for (path, parser) in [
            (&files.wave, "extractous::AudioParser"),
            (&files.mp4, "extractous::MP4Parser"),
            (&files.pdf, pdf::PdfParser.name()),
        ] {
            let routed = route_file(path).unwrap();
            assert_eq!(routed.parser.name(), parser);
            assert!(routed.input.is_mapped());
        }
        #[cfg(feature = "tika")]
        assert!(matches!(
            route_file(&files.rtf),
            Err(Unsupported::TooLarge {
                parser: "extractous::RTFParser",
                limit: input::MAX_READ_WHOLE,
            })
        ));
        #[cfg(not(feature = "tika"))]
        {
            let routed = route_file(&files.rtf).unwrap();
            assert_eq!(routed.parser.name(), "extractous::RTFParser");
            assert!(routed.input.is_mapped());
        }
    }

    /// The parsers of bounded memory extract a file too large to read whole as Tika's path does.
    #[cfg(feature = "tika")]
    #[test]
    fn extracts_files_too_large_to_read_whole_as_tikas_path_does() {
        let files = LargeFiles::new("parity");
        for path in [&files.wave, &files.mp4] {
            let routed = route_file(path).unwrap();
            crate::formats::parity::assert_same_as_tika(
                &[std::path::Path::new(path)],
                routed.parser.tika_name(),
            );
        }
    }

    /// A build without Tika fails a document no native parser reads with why, and records one
    /// embedded in a document a native parser reads as unread, the embedding document going on.
    #[cfg(not(feature = "tika"))]
    #[test]
    fn fails_documents_no_native_parser_reads_with_why() {
        let hwp = crate::ole2::tests::compound(&["\u{5}HwpSummaryInformation"], b"");
        let no_parser = Unsupported::Format("application/x-hwp-v5".to_owned());
        let extractor = crate::Extractor::new();
        let failed = extractor.extract_bytes_to_string(&hwp).unwrap_err();
        assert!(
            matches!(&failed, Error::Unsupported(why) if *why == no_parser),
            "{failed:?}"
        );
        let streamed = extractor.extract_bytes(&hwp).map(|_| ()).unwrap_err();
        assert!(
            matches!(&streamed, Error::Unsupported(why) if *why == no_parser),
            "{streamed:?}"
        );
        let archive = crate::compress::zip::file::tests::zip(&[
            ("notes.txt", b"Plain text."),
            ("data.hwp", &hwp),
        ]);
        let (text, metadata) = extractor.extract_bytes_to_string(&archive).unwrap();
        assert!(text.contains("Plain text."), "{text}");
        assert_eq!(
            metadata["tk:exception:embedded-exception"],
            [format!(
                "extractous could not parse the embedded document \"data.hwp\": {no_parser}"
            )]
        );
    }

    /// A build without Tika reads natively an embedded document its parser reads differently
    /// from Tika's parser, and records the difference in the embedding document (#144).
    #[cfg(not(feature = "tika"))]
    #[test]
    fn reads_an_embedded_document_its_parser_reads_differently_from_tika() {
        let page =
            b"<html><body><p>Inside.</p><img src=\"data:text/plain;charset=windows-1252,hej\"></body></html>";
        let archive = crate::compress::zip::file::tests::zip(&[("page.html", page)]);
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&archive)
            .unwrap();
        assert!(text.contains("Inside."), "{text}");
        assert!(text.contains("hej"), "{text}");
        assert_eq!(
            metadata["tk:exception:embedded-exception"],
            [
                "extractous::JSoupParser read the embedded document \"page.html\" differently \
                 from Tika: a data URI of text in windows-1252, which the native parser encodes in \
                 UTF-8"
            ]
        );
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
        let detected =
            Detected::detect(&OpenedFrom::File(path.into()), &input, Intent::Parse).unwrap();
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
        let bytes = Detected::detect(&OpenedFrom::Bytes, &input, Intent::Parse).unwrap();
        assert_eq!(bytes.metadata.get("tk:resource-name"), None);
        assert_eq!(
            bytes.metadata.get("Content-Length"),
            Some(input.len().to_string().as_str())
        );
    }
}
