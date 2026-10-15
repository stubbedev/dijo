//! Extraction in Rust, format by format (#41). A [`Parser`] writes a document as XHTML through an
//! [`Xhtml`], the port of Tika's `XHTMLContentHandler`, into the same handler chains extractous
//! puts behind Tika's parsers, so a native parser's text, XHTML and metadata are what Tika's
//! path gives. [`parser_for`] routes a media type to its native parser, if it has one.

mod auto_detect_reader;
mod fiction_book;
pub(crate) mod handlers;
mod java_time;
mod media_type;
pub(crate) mod metadata;
mod odf;
#[cfg(test)]
pub(crate) mod parity;
mod rtf;
mod subtitle;
mod text;
pub(crate) mod xhtml;
pub(crate) mod xml;

use std::borrow::Cow;
use std::fs;
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ParserConfigs;
use crate::errors::{Error, ExtractResult};
use crate::isolate::isolate;
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
    /// of this document the parser does not handle yet, such as embedded documents, which Tika
    /// parses recursively. Decided before any output is written.
    fn defers(&self, _input: &[u8]) -> Option<&'static str> {
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
    &fiction_book::FictionBookParser,
    &odf::OpenDocumentParser,
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
/// this document to Tika, detecting it with Tika's detector until the detection is ported
/// (#41). Documents read from a URL, and those whose embedded documents are spooled, stay with
/// Tika.
pub(crate) fn route(source: Source<'_>, spooling: bool) -> ExtractResult<Option<Routed<'_>>> {
    if spooling || matches!(source, Source::Url(_)) || !any_parsers() {
        return Ok(None);
    }
    let (media_type, metadata) = tika::detect_with_metadata(isolate()?, source)?;
    let Some(parser) = parser_for(&media_type) else {
        return Ok(None);
    };
    let input = input(source)?;
    if parser.defers(&input).is_some() {
        return Ok(None);
    }
    Ok(Some(Routed {
        parser,
        detected: Detected::from_tika(media_type, metadata),
        input,
    }))
}

/// The bytes of a document read from a file or memory.
fn input(source: Source<'_>) -> ExtractResult<Cow<'_, [u8]>> {
    match source {
        Source::File(path) => fs::read(path)
            .map(Cow::Owned)
            .map_err(|e| Error::IoError(format!("Could not read {path}: {e}"))),
        Source::Bytes(bytes) => Ok(Cow::Borrowed(bytes)),
        Source::Url(url) => Err(Error::IoError(format!(
            "Documents read from a URL have no native parser: {url}"
        ))),
    }
}

impl Detected {
    /// What Tika's detection returned. Its metadata comes as a map without the order it was set
    /// in, so the names are set in sorted order, which decides the head's order only between
    /// names in the same bucket of Java's `HashMap`.
    fn from_tika(media_type: String, metadata: Metadata) -> Self {
        let mut names: Vec<(String, Vec<String>)> = metadata.into_iter().collect();
        names.sort();
        let mut ordered = TikaMetadata::new();
        for (name, values) in names {
            ordered.set_values(&name, values);
        }
        Self {
            media_type,
            metadata: ordered,
        }
    }

    /// The metadata a parser starts from, as Tika's `AutoDetectParser` and `CompositeParser`
    /// leave it: the detected `Content-Type` and the parser in `tk:parsed-by`.
    fn metadata_for(&self, parser: &dyn Parser) -> TikaMetadata {
        let mut metadata = self.metadata.clone();
        metadata.set("Content-Type", self.media_type.clone());
        metadata.add("tk:parsed-by", parser.name());
        metadata
    }
}

/// The metadata Tika's `CompositeParser` sets once its parser has returned.
fn finish_metadata(metadata: &mut TikaMetadata) {
    let parsers = metadata.values("tk:parsed-by").to_vec();
    metadata.set_values("tk:parsed-by-full-set", parsers);
}

fn run(
    parser: &dyn Parser,
    input: &[u8],
    detected: &Detected,
    configs: &ParserConfigs,
    handler: &mut dyn ContentHandler,
) -> (TikaMetadata, Result<(), Failure>) {
    let (mut metadata, result) = Xhtml::write(handler, detected.metadata_for(parser), |xhtml| {
        parser.parse(input, configs, xhtml)
    });
    finish_metadata(&mut metadata);
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
