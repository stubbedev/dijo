//! The native parsers' common ground (#41). A [`Parser`] writes a document as XHTML through an
//! [`Xhtml`], the port of Tika's `XHTMLContentHandler`, into the same handler chains extractous
//! puts behind Tika's parsers, so a native parser's text, XHTML and metadata are what Tika's
//! path gives. The parsers themselves live in the crates of their formats; `extractous-core`
//! gathers them and routes a media type to its native parser, if it has one.

pub mod auto_detect_reader;
pub mod crypt;
pub mod data_format;
pub mod element_metadata;
pub mod embedded;
pub mod handlers;
pub mod jdk_zip;
pub mod list_numbering;
pub mod metadata;
pub mod ole10_native;
#[cfg(any(test, feature = "testing"))]
pub mod parity;
pub mod secure;
pub mod xhtml;
pub mod xml;
pub mod xmp;
pub mod zip_file;

use std::sync::OnceLock;

use crate::config::ParserConfigs;
#[cfg(feature = "tika")]
use crate::errors::ExtractResult;
use crate::errors::{Error, Unsupported};
use crate::input::Pages;
#[cfg(feature = "tika")]
use crate::tika::EmbeddedEvents;
use handlers::{AnchoredBlocks, BlockBreaks, Body, ToText, ToXml, WellFormed, Write, WriteLimit};
use metadata::TikaMetadata;
use xhtml::{ContentHandler, Stop, Xhtml};

/// Why a native parser leaves a document to Tika: what of it the port does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unported(pub String);

/// Why a native parser did not finish its document.
#[derive(Debug)]
pub enum Failure {
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
/// document the parser cannot read.
pub type Deferral = String;

/// A detail in which a native parser's reading of a document differs from Tika's parser's, which
/// the parser otherwise reads: why a build with Tika reads the document on Tika's path, where a
/// build without it keeps the native reading and records the difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference(pub String);

impl Difference {
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

/// Why a native parser does not take a document as it prepared it. A [`Decline<'static>`] holds no
/// reading borrowed from its input.
pub enum Decline<'a> {
    /// The parser cannot read it: Tika's path should extract it.
    Deferred(Deferral),
    /// The parser reads it, as `reading` writes it, but differently from Tika's parser in the
    /// `difference`: Tika's path should extract it in a build that has Tika, and a build
    /// without Tika takes the reading.
    Differs {
        reading: Prepared<'a>,
        difference: Difference,
    },
    /// Tika's parser throws this on it before it starts the document: the extraction fails at
    /// once, without output, as Tika's path does.
    Fails(Error),
    /// Tika's parser writes no document of it, not even its start and end, and leaves only
    /// this metadata: what it started from, with what it found.
    Unwritten(TikaMetadata),
}

impl std::fmt::Debug for Decline<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deferred(deferral) => f.debug_tuple("Deferred").field(deferral).finish(),
            Self::Differs { difference, .. } => f
                .debug_struct("Differs")
                .field("difference", difference)
                .finish_non_exhaustive(),
            Self::Fails(error) => f.debug_tuple("Fails").field(error).finish(),
            Self::Unwritten(metadata) => f.debug_tuple("Unwritten").field(metadata).finish(),
        }
    }
}

impl From<Deferral> for Decline<'_> {
    fn from(deferral: Deferral) -> Self {
        Self::Deferred(deferral)
    }
}

impl Unported {
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl From<Unported> for Decline<'_> {
    fn from(Unported(reason): Unported) -> Self {
        Self::Deferred(reason)
    }
}

impl std::fmt::Display for Decline<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deferred(deferral) => f.write_str(deferral),
            Self::Differs {
                difference: Difference(difference),
                ..
            } => f.write_str(difference),
            Self::Fails(error) => write!(f, "{error}"),
            Self::Unwritten(_) => f.write_str("Tika's parser writes no document of it"),
        }
    }
}

/// The reading a parser prepared of a document and where it reads the document differently from
/// Tika's parser, if anywhere, or else why it does not take it.
pub fn noted<'a>(
    prepared: Result<Prepared<'a>, Decline<'a>>,
) -> Result<(Prepared<'a>, Option<Difference>), Decline<'static>> {
    match prepared {
        Ok(reading) => Ok((reading, None)),
        Err(Decline::Differs {
            reading,
            difference,
        }) => Ok((reading, Some(difference))),
        Err(Decline::Deferred(deferral)) => Err(Decline::Deferred(deferral)),
        Err(Decline::Fails(error)) => Err(Decline::Fails(error)),
        Err(Decline::Unwritten(metadata)) => Err(Decline::Unwritten(metadata)),
    }
}

/// [`noted`] without the reading: what a preparation of a document decided.
pub fn decision(
    prepared: Result<Prepared<'_>, Decline<'_>>,
) -> Result<Option<Difference>, Decline<'static>> {
    noted(prepared).map(|(_, difference)| difference)
}

/// [`noted`] without the difference: the reading a parser prepared of a document, whether or not
/// it reads it differently from Tika's parser.
pub fn reading<'a>(
    prepared: Result<Prepared<'a>, Decline<'a>>,
) -> Result<Prepared<'a>, Decline<'static>> {
    noted(prepared).map(|(reading, _)| reading)
}

/// What a parser prepared of a document: the `reading`, which it takes unless it reads the
/// document differently from Tika's parser, in the `difference` given.
pub fn unless_different(
    reading: Prepared<'_>,
    difference: Option<Difference>,
) -> Result<Prepared<'_>, Decline<'_>> {
    match difference {
        None => Ok(reading),
        Some(difference) => Err(Decline::Differs {
            reading,
            difference,
        }),
    }
}

/// How the memory a native parser takes grows with its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Footprint {
    /// With the input: it reads all of it into structures of its own. A file larger than
    /// [`input::MAX_READ_WHOLE`] is left to Tika, which streams it.
    Proportional,
    /// Bounded whatever the input's size: it reads only bounded parts of its input, such as
    /// headers and indexes, in place, and skips the rest. It reads a file of any size, mapped.
    Bounded,
}

/// A document its parser has read as far as deciding it extracts it: what writes the document's
/// content as XHTML and adds the metadata it finds, holding what the parser read on the way, so
/// that nothing is read twice. The document is started before and ended after it.
pub type Prepared<'a> = Box<dyn FnOnce(&mut Xhtml<'_>) -> Result<(), Failure> + 'a>;

/// A format's native parser.
pub trait Parser: Sync {
    /// The name recorded in `tk:parsed-by` for the documents it parses.
    fn name(&self) -> &'static str;

    /// The class name of the parser Tika's path parses its documents with, which it replaces.
    fn tika_name(&self) -> &'static str;

    /// The parsers it records delegating to, each native name paired with the class name of
    /// Tika's.
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// The media types it parses, as Tika's detection names them.
    fn media_types(&self) -> &'static [&'static str];

    /// The types of its [`Parser::media_types`] whose subtypes it parses too, as the parser it
    /// replaces parses those Tika's composite parser has no parser of their own for.
    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &[]
    }

    /// How the memory it takes grows with its input, which decides whether it reads a file too
    /// large to read whole.
    fn footprint(&self) -> Footprint {
        Footprint::Proportional
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
    ) -> Result<Prepared<'a>, Decline<'a>>;

    /// [`Parser::prepare`], given the `pages` of `input`, which a parser of
    /// [`Footprint::Bounded`] that reads all of a mapped file releases as it reads on.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        _pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare(input, metadata, configs)
    }

    /// Why Tika's path should extract `input`, in a build that has Tika, if it should: what of it
    /// the parser cannot read, or where it reads it differently from Tika's parser.
    fn defers(
        &self,
        input: &[u8],
        metadata: &TikaMetadata,
        configs: &ParserConfigs,
    ) -> Option<Deferral> {
        match self.prepare(input, metadata, configs) {
            Err(Decline::Deferred(deferral)) => Some(deferral),
            Err(Decline::Differs { difference, .. }) => Some(difference.0),
            Ok(_) | Err(Decline::Fails(_) | Decline::Unwritten(_)) => None,
        }
    }
}

/// The file Tika's `TikaInputStream` holds a document in, which a parser that asks it for a path
/// (`getPath`) is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFile {
    /// The file the document was read from, at this path.
    Original(String),
    /// None yet: a document read from memory or fetched, which Tika spools to a temporary file of
    /// its own the first time a parser asks for a path.
    Spooled,
}

/// Why no native parser prepares a document embedded in one a native parser reads.
#[derive(Debug)]
pub enum Unrouted {
    /// It is without content, which Tika's zero-byte check fails.
    Empty,
    /// No native parser reads it, which Tika's path parses in a build with Tika.
    Unsupported(Unsupported),
}

/// How the `TikaInputStream` of an embedded document was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// With its metadata, as `TikaInputStream.get(stream, tmp, metadata)` opens it: reading it
    /// to its end records its length there.
    WithMetadata,
    /// Alone, as `TikaInputStream.get(stream)` opens it: its metadata never learns its length.
    Alone,
}

/// A document embedded in one a native parser reads, as its native parser prepared it.
pub struct EmbeddedRoute<'a> {
    pub parser: &'static dyn Parser,
    /// The metadata the parser starts from.
    pub start: TikaMetadata,
    pub prepared: Prepared<'a>,
    /// Where the parser reads it differently from Tika's parser, in a build without Tika, which
    /// takes the native reading where a build with Tika parses the document on Tika's path.
    #[cfg(not(feature = "tika"))]
    pub difference: Option<Difference>,
}

/// Why no native parser prepares a document embedded in one a native parser reads: Tika's path
/// parses it, unless it is empty or its parser writes no document of it, which the native path
/// records as Tika does. Tika's path records the failure of one Tika's parser fails before it
/// starts, which only that path can tell.
#[derive(Debug)]
pub enum Bridged {
    /// No native parser prepares it.
    Unrouted(Unrouted),
    /// Its native parser, of this name, cannot read it, for this reason.
    Deferred(&'static str, Deferral),
    /// Its native parser, of this name, reads it differently from Tika's parser in this way.
    #[cfg(feature = "tika")]
    Differs(&'static str, Difference),
    /// Tika's parser throws this on it before it starts, as its native parser, of this name,
    /// reproduces.
    Fails(&'static str, Error),
    /// Its native parser, of this name, writes no document of it, as Tika's parser does, and
    /// leaves this metadata.
    Unwritten(&'static str, TikaMetadata),
}

/// The native parser of a document embedded in one a native parser reads, with the metadata it
/// starts from, given its `data`, the `metadata` its embedding parser gives it and how its
/// stream was `opened`, as Tika's `AutoDetectParser` and `CompositeParser` route it: detected
/// with the resource name and `Content-Type` of its metadata as hints, then parsed by the parser
/// of the type the metadata overrides the parser with, else of the detected type. Otherwise why
/// Tika's path parses it: that parser is not native, the document is empty, which Tika's
/// zero-byte check fails, or Tika's detection throws on it.
pub fn embedded_parser(
    data: &[u8],
    metadata: &TikaMetadata,
    opened: Opened,
) -> Result<(&'static dyn Parser, TikaMetadata), Unrouted> {
    routes().embedded_parser(data, metadata, opened)
}

/// Parses with Tika's path a document embedded in one a native parser reads, which no native
/// parser prepares for the reason `bridged` gives: what Tika made of it, given its `data`, the
/// metadata its embedding parser gives it, as `pairs`, and the parser settings `configs`.
#[cfg(feature = "tika")]
pub fn parse_with_tika(
    bridged: Bridged,
    data: &[u8],
    pairs: &[(&str, &str)],
    configs: &ParserConfigs,
) -> ExtractResult<EmbeddedEvents> {
    routes().parse_with_tika(bridged, data, pairs, configs)
}

/// What the native parsers reach of the crate that gathers them all, `extractous-core`, which
/// installs it with [`install_routes`] before it routes a document: where a document embedded in
/// one a native parser reads goes.
pub trait Routes: Sync {
    /// [`embedded_parser`].
    fn embedded_parser(
        &self,
        data: &[u8],
        metadata: &TikaMetadata,
        opened: Opened,
    ) -> Result<(&'static dyn Parser, TikaMetadata), Unrouted>;

    /// [`parse_with_tika`].
    #[cfg(feature = "tika")]
    fn parse_with_tika(
        &self,
        bridged: Bridged,
        data: &[u8],
        pairs: &[(&str, &str)],
        configs: &ParserConfigs,
    ) -> ExtractResult<EmbeddedEvents>;
}

static ROUTES: OnceLock<&'static dyn Routes> = OnceLock::new();

/// Installs where documents embedded in those native parsers read go, once per process: the
/// routes installed first stay.
pub fn install_routes(routes: &'static dyn Routes) {
    ROUTES.get_or_init(|| routes);
}

/// The routes installed.
fn routes() -> &'static dyn Routes {
    *ROUTES
        .get()
        .expect("extractous-core installs the routes of embedded documents before it routes one")
}

/// Routes a document embedded in one a native parser reads to its [`embedded_parser`], and
/// returns what that parser prepared, if it takes the document, or why Tika's path parses it.
/// A build without Tika takes the reading of a parser that reads the document differently from
/// Tika's.
pub fn route_embedded<'a>(
    data: &'a [u8],
    metadata: &TikaMetadata,
    opened: Opened,
    configs: &'a ParserConfigs,
) -> Result<EmbeddedRoute<'a>, Bridged> {
    let (parser, start) = embedded_parser(data, metadata, opened).map_err(Bridged::Unrouted)?;
    let name = parser.name();
    let (prepared, difference) = match parser.prepare(data, &start, configs) {
        Ok(prepared) => (prepared, None),
        Err(Decline::Differs {
            reading,
            difference,
        }) => (reading, Some(difference)),
        Err(Decline::Deferred(deferral)) => return Err(Bridged::Deferred(name, deferral)),
        Err(Decline::Fails(error)) => return Err(Bridged::Fails(name, error)),
        Err(Decline::Unwritten(left)) => return Err(Bridged::Unwritten(name, left)),
    };
    #[cfg(feature = "tika")]
    if let Some(difference) = difference {
        return Err(Bridged::Differs(name, difference));
    }
    Ok(EmbeddedRoute {
        parser,
        start,
        prepared,
        #[cfg(not(feature = "tika"))]
        difference,
    })
}

/// `Path.getFileName().toString()` of a path as Java's `Paths.get` reads it on this platform:
/// the last name, trailing separators ignored.
pub fn java_file_name(path: &str) -> &str {
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let trimmed = path.trim_end_matches(separators);
    trimmed.rsplit(separators).next().unwrap_or(trimmed)
}

/// The metadata of a document the parser named `parser` writes nothing of, as Tika leaves it
/// once that parser has returned, from the `metadata` the parser left.
pub fn unwritten(
    parser: &str,
    mut metadata: TikaMetadata,
    configs: &ParserConfigs,
) -> TikaMetadata {
    embedded::EmbeddedRecord::default().finish(&mut metadata, parser);
    mark_password_used(&mut metadata, configs);
    metadata
}

/// Marks a document parsed under `configs` as parsed with a password, if one was given.
pub fn mark_password_used(metadata: &mut TikaMetadata, configs: &ParserConfigs) {
    if configs.password.is_some() {
        metadata.set(PASSWORD_USED, "true");
    }
}

/// What our Java `LegacyOfficePasswordParser` records once a document a password was given for
/// is parsed, and its `PasswordProvider` once a parser asks it for the password.
pub const PASSWORD_USED: &str = "extractous:password-used";

/// What a parse writes: the text of the body, or the whole document as XHTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Text,
    Xml,
}

/// The handlers extractous puts behind a parser. Both entry points hold back anchored blocks,
/// which only Tika's streamed extraction (`ParsingInputStream`) does: its string extraction
/// (`TikaNativeMain.parseToString`) leaves them where they cut the paragraph they are anchored
/// in (#63).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    /// A stream's: XML declaring its `encoding`.
    Stream {
        output: Output,
        encoding: &'static str,
    },
    /// A string's: at most `max_length` UTF-16 code units of text.
    String { output: Output, max_length: usize },
    /// `TikaNativeMain.parseToString`'s, without anchored blocks held back, which the parity
    /// tests compare a native parser's string extraction with Tika's under (#63).
    #[cfg(any(test, feature = "testing"))]
    TikaString { output: Output, max_length: usize },
}

impl Chain {
    /// Runs `write` with the handler at the head of this chain, writing to `out`.
    pub fn write_to<W: Write, R>(
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
            #[cfg(any(test, feature = "testing"))]
            Chain::TikaString {
                output: Output::Xml,
                max_length,
            } => write(&mut WriteLimit::new(
                WellFormed::new(ToXml::new(out, None)),
                max_length,
            )),
            #[cfg(any(test, feature = "testing"))]
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
    pub fn max_length(self) -> Option<usize> {
        match self {
            Chain::Stream { .. } => None,
            Chain::String { max_length, .. } => Some(max_length),
            #[cfg(any(test, feature = "testing"))]
            Chain::TikaString { max_length, .. } => Some(max_length),
        }
    }
}

/// Collects written text into a string.
#[derive(Debug, Default)]
pub struct StringOut(pub String);

impl Write for StringOut {
    fn write(&mut self, text: &str) -> Result<(), Stop> {
        self.0.push_str(text);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Stop> {
        Ok(())
    }
}
