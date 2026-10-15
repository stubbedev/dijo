//! Extraction in Rust, format by format (#41). A [`Parser`] writes a document as XHTML through an
//! [`Xhtml`], the port of Tika's `XHTMLContentHandler`, into the same handler chains extractous
//! puts behind Tika's parsers, so a native parser's text, XHTML and metadata are what Tika's
//! path gives. [`parser_for`] routes a media type to its native parser, if it has one.

mod auto_detect_reader;
mod docx;
mod element_metadata;
pub(crate) mod embedded;
mod epub;
mod fiction_book;
pub(crate) mod handlers;
mod html;
pub(crate) mod java;
mod java_time;
mod mail;
pub(crate) mod metadata;
mod odf;
mod ooxml;
#[cfg(test)]
pub(crate) mod parity;
#[cfg(test)]
pub(crate) mod reference;
mod rtf;
mod subtitle;
mod text;
pub(crate) mod xhtml;
pub(crate) mod xml;
mod zip_file;

use std::borrow::Cow;
use std::fs;
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ParserConfigs;
use crate::errors::{Error, ExtractResult};
use crate::isolate::isolate;
use crate::mime::{self, Detection, Origin};
use crate::tika::{self, Source};
use crate::{CharSet, Metadata};
use handlers::{AnchoredBlocks, BlockBreaks, Body, ToText, ToXml, WellFormed, Write, WriteLimit};
use metadata::TikaMetadata;
use xhtml::{ContentHandler, Stop, Xhtml};

/// The bytes a streamed extraction hands its reader at a time.
const CHUNK_BYTES: usize = 8192;
/// The chunks a streamed extraction writes ahead of its reader.
const CHUNKS_AHEAD: usize = 16;

/// Why a native parser did not finish its document.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The output stopped taking content.
    Stopped(Stop),
    /// The document is not well-formed in its format, with what was wrong where.
    Malformed(String),
    /// An extraction the document depends on failed, such as that of a document embedded in it.
    Extraction(Error),
}

impl From<Stop> for Failure {
    fn from(stop: Stop) -> Self {
        Self::Stopped(stop)
    }
}

/// A format's native parser.
pub(crate) trait Parser: Sync {
    /// The name recorded in `tk:parsed-by` for the documents it parses.
    fn name(&self) -> &'static str;

    /// The media types it parses, without parameters, as Tika's detection names them.
    fn media_types(&self) -> &'static [&'static str];

    /// Why Tika's path should extract `input` rather than this parser, if it should: a feature
    /// of this document the parser does not handle yet, decided before any output is written,
    /// from the document, the `metadata` Tika's parser starts from and the parser settings
    /// `configs` the extraction was given.
    fn defers(
        &self,
        _input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        None
    }

    /// Writes the content of `input` as XHTML and adds the metadata it finds, as the Tika parser
    /// it replaces does, under the parser settings `configs` the extraction was given. The
    /// document is started before and ended after it.
    fn parse(
        &self,
        input: &[u8],
        configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure>;
}

static PARSERS: &[&dyn Parser] = &[
    &docx::DocxParser,
    &fiction_book::FictionBookParser,
    &html::JSoupParser,
    &mail::EmlxParser,
    &epub::EpubParser,
    &odf::OpenDocumentParser,
    &odf::FlatOpenDocumentParser,
    &mail::Rfc822Parser,
    &rtf::RtfParser,
    &subtitle::SubtitleParser,
    &text::TextAndCsvParser,
];

/// The native parser for `media_type` (parameters such as `charset` are ignored), if any.
fn parser_for(media_type: &str) -> Option<&'static dyn Parser> {
    let base = media_type.split(';').next().unwrap_or_default().trim();
    PARSERS
        .iter()
        .copied()
        .find(|parser| parser.media_types().contains(&base))
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
}

/// A document a native parser reads, and what its detection found.
pub(crate) struct Routed<'a> {
    pub(crate) parser: &'static dyn Parser,
    pub(crate) detected: Detected,
    pub(crate) input: Cow<'a, [u8]>,
}

/// Routes a document to its native parser, if its format has one and the parser does not defer
/// this document to Tika. The document is detected in Rust, without the JVM, so a document a
/// native parser reads never starts it. Documents read from a URL, those whose embedded
/// documents are spooled, and files too large to read whole for their detection stay with Tika.
///
/// A document Tika's detection throws on (a corrupt binary property list, say) is routed to
/// Tika's path, whose own detection then fails it exactly as Tika does.
pub(crate) fn route<'a>(
    source: Source<'a>,
    spooling: bool,
    configs: &ParserConfigs,
) -> ExtractResult<Option<Routed<'a>>> {
    if spooling || matches!(source, Source::Url(_)) || !any_parsers() {
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
    if parser
        .defers(&input, &detected.metadata_for(parser), configs)
        .is_some()
    {
        return Ok(None);
    }
    Ok(Some(Routed {
        parser,
        detected,
        input,
    }))
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
fn java_file_name(path: &str) -> &str {
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
        let origin = match source {
            Source::File(path) => {
                let name = java_file_name(path);
                metadata.set("tk:resource-name", name);
                metadata.set("Content-Length", input.len().to_string());
                Origin::File { name }
            }
            Source::Bytes(_) | Source::Url(_) => Origin::Bytes,
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

fn run(
    parser: &dyn Parser,
    input: &[u8],
    detected: &Detected,
    configs: &ParserConfigs,
    handler: &mut dyn ContentHandler,
) -> (TikaMetadata, Result<(), Failure>) {
    let (mut metadata, record, result) =
        Xhtml::write(handler, detected.metadata_for(parser), configs, |xhtml| {
            parser.parse(input, configs, xhtml)
        });
    record.finish(&mut metadata, parser.name());
    (metadata, result)
}

fn failure_error(parser: &dyn Parser, failure: Failure) -> Error {
    match failure {
        Failure::Malformed(reason) => Error::ParseError(format!(
            "{} could not parse the document: {reason}",
            parser.name()
        )),
        Failure::Stopped(stop) => Error::ParseError(format!(
            "{} stopped writing the document: {stop:?}",
            parser.name()
        )),
        Failure::Extraction(error) => error,
    }
}

/// What a parse writes: the text of the body, or the whole document as XHTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Output {
    Text,
    Xml,
}

/// The handlers extractous puts behind a parser, which differ between Tika's two entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chain {
    /// `ParsingInputStream`'s: anchored blocks held back, XML declaring its `encoding`.
    Stream {
        output: Output,
        encoding: &'static str,
    },
    /// `TikaNativeMain.parseToString`'s: at most `max_length` UTF-16 code units of text.
    String { output: Output, max_length: usize },
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
            } => write(&mut WriteLimit::new(
                WellFormed::new(ToXml::new(out, None)),
                max_length,
            )),
            Chain::String {
                output: Output::Text,
                max_length,
            } => write(&mut Body::new(BlockBreaks::new(WriteLimit::new(
                ToText::new(out),
                max_length,
            )))),
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
/// handlers `TikaNativeMain.parseToString` uses.
pub(crate) fn parse_to_string(
    parser: &dyn Parser,
    input: &[u8],
    detected: &Detected,
    configs: &ParserConfigs,
    max_length: usize,
    output: Output,
) -> ExtractResult<(String, Metadata)> {
    let mut out = StringOut::default();
    let (metadata, result) = Chain::String { output, max_length }.write_to(&mut out, |handler| {
        run(parser, input, detected, configs, handler)
    });
    match result {
        Ok(()) | Err(Failure::Stopped(Stop::LimitReached)) => Ok((out.0, metadata.to_map())),
        Err(failure) => Err(failure_error(parser, failure)),
    }
}

enum Message {
    /// A full chunk of output, written while the parse goes on.
    Chunk(Vec<u8>),
    /// The output after the last full chunk, and how the parse ended.
    Done {
        tail: Vec<u8>,
        outcome: ExtractResult<Metadata>,
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
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamOptions {
    pub(crate) char_set: CharSet,
    pub(crate) output: Output,
    pub(crate) timeout: Option<Duration>,
}

/// Parses `input` on a thread of its own into a stream encoded with the options' charset,
/// through the handlers `ParsingInputStream` uses. Like `ParsingInputStream`, it returns once
/// the parse has written its first chunk or ended, with the metadata known then: the final
/// metadata of a document whose output fits in one chunk, else that of its detection.
pub(crate) fn parse_to_stream(
    parser: &'static dyn Parser,
    input: Vec<u8>,
    detected: Detected,
    configs: ParserConfigs,
    options: StreamOptions,
) -> ExtractResult<(NativeStream, Metadata)> {
    let deadline = options.timeout.map(|timeout| Instant::now() + timeout);
    let detected_metadata = detected.metadata.to_map();
    let (sender, receiver) = mpsc::sync_channel(CHUNKS_AHEAD);
    thread::Builder::new()
        .name(format!("extractous {}", parser.name()))
        .spawn(move || {
            let mut out = ChannelOut {
                sender: sender.clone(),
                char_set: options.char_set,
                buffer: Vec::with_capacity(CHUNK_BYTES),
            };
            let chain = Chain::Stream {
                output: options.output,
                encoding: char_set_name(options.char_set),
            };
            let (metadata, result) = chain.write_to(&mut out, |handler| {
                run(parser, &input, &detected, &configs, handler)
            });
            let outcome = match result {
                Ok(()) => Ok(metadata.to_map()),
                Err(Failure::Stopped(Stop::ReaderGone)) => return,
                Err(failure) => Err(failure_error(parser, failure)),
            };
            let tail = std::mem::take(&mut out.buffer);
            let _ = sender.send(Message::Done { tail, outcome });
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
    };
    let first = stream.next_message()?;
    stream.accept(first);
    let metadata = match &stream.outcome {
        Some(Ok(metadata)) => metadata.clone(),
        _ => detected_metadata,
    };
    Ok((stream, metadata))
}

/// The text of a native parse, read as it is written.
pub(crate) struct NativeStream {
    receiver: Receiver<Message>,
    chunk: Vec<u8>,
    position: usize,
    deadline: Option<Instant>,
    outcome: Option<ExtractResult<Metadata>>,
}

impl NativeStream {
    /// The final metadata, once the stream has been read to its end.
    pub(crate) fn metadata(&self) -> ExtractResult<Metadata> {
        match &self.outcome {
            Some(Ok(metadata)) => Ok(metadata.clone()),
            Some(Err(error)) => Err(Error::ParseError(error.to_string())),
            None => Err(Error::StreamNotFinished),
        }
    }

    fn accept(&mut self, message: Message) {
        match message {
            Message::Chunk(chunk) => self.chunk = chunk,
            Message::Done { tail, outcome } => {
                self.chunk = tail;
                self.outcome = Some(outcome);
            }
        }
        self.position = 0;
    }

    fn next_message(&self) -> ExtractResult<Message> {
        let disconnected = || {
            Error::ParseError(
                "The native parser's thread ended without finishing its document".to_owned(),
            )
        };
        match self.deadline {
            None => self.receiver.recv().map_err(|_| disconnected()),
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                self.receiver.recv_timeout(wait).map_err(|e| match e {
                    RecvTimeoutError::Timeout => Error::Timeout,
                    RecvTimeoutError::Disconnected => disconnected(),
                })
            }
        }
    }
}

impl Read for NativeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.position == self.chunk.len() {
            if let Some(outcome) = &self.outcome {
                return match outcome {
                    Ok(_) => Ok(0),
                    Err(error) => Err(io::Error::other(Error::ParseError(error.to_string()))),
                };
            }
            let message = self.next_message().map_err(io::Error::other)?;
            self.accept(message);
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
