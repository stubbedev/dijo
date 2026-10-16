//! Tika's `XHTMLContentHandler`, with the `SafeContentHandler` it extends: what a parser calls to
//! write its document as XHTML. It opens `html`, writes the `head` from the metadata before the
//! first body content, indents table cells and list items with a tab, ends block elements with a
//! line break and replaces characters XML cannot hold with U+FFFD.

use crate::errors::Error;
use crate::formats::embedded::{
    self, EmbeddedRecord, Embedding, Event, ParseContext, Wrapping, PACKAGE_ENTRY,
};
use crate::formats::handlers::Body;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{self, EmbeddedRoute, Failure, Opened};
use crate::isolate::isolate;
use crate::memory;
use crate::tika::{self, EmbeddedOutcome};

/// Whether the extraction holds no more memory than its budget, which it checks as it writes.
pub(crate) fn within_budget() -> Result<(), Stop> {
    memory::overrun().map_or(Ok(()), |limit| Err(Stop::MemoryLimit(limit)))
}

/// The XHTML namespace, which every element a native parser writes is in.
pub(crate) const XHTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";

const ENDLINE: [&str; 29] = [
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "div",
    "ul",
    "ol",
    "dl",
    "pre",
    "hr",
    "blockquote",
    "address",
    "fieldset",
    "table",
    "form",
    "noscript",
    "li",
    "dt",
    "dd",
    "noframes",
    "br",
    "tr",
    "select",
    "option",
    "link",
    "script",
];
const HEAD: [&str; 5] = ["title", "link", "base", "meta", "script"];
const AUTO: [&str; 2] = ["head", "frameset"];
const INDENT: [&str; 6] = ["li", "dd", "dt", "td", "th", "frame"];
const REPLACEMENT: char = '\u{fffd}';

/// Why the output stopped taking content before the parser finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The text reached the extraction's length limit, as Tika's `WriteLimitReachedException`.
    LimitReached,
    /// Whoever was reading the output went away.
    ReaderGone,
    /// The extraction holds more memory than its budget of this many bytes.
    MemoryLimit(u64),
}

/// Where the XHTML of a document goes: the SAX events Tika's handlers receive, all in the XHTML
/// namespace and named by their local name, and the namespace prefixes a parser maps.
pub(crate) trait ContentHandler {
    fn start_document(&mut self) -> Result<(), Stop>;
    /// SAX's `startPrefixMapping`: `prefix`, empty for the default namespace, stands for `uri`
    /// from the next element on.
    fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop>;
    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop>;
    fn end_element(&mut self, name: &str) -> Result<(), Stop>;
    fn characters(&mut self, text: &str) -> Result<(), Stop>;
    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop>;
    fn end_document(&mut self) -> Result<(), Stop>;

    /// The metadata of the document an `XHTMLContentHandler` is about to start the head of,
    /// for a handler that shares it with the document it writes into.
    fn head_metadata(&mut self, _metadata: &TikaMetadata) {}
}

impl<H: ContentHandler + ?Sized> ContentHandler for &mut H {
    fn start_document(&mut self) -> Result<(), Stop> {
        (**self).start_document()
    }

    fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        (**self).start_prefix_mapping(prefix, uri)
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        (**self).start_element(name, attributes)
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        (**self).end_element(name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        (**self).characters(text)
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        (**self).ignorable_whitespace(text)
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        (**self).end_document()
    }

    fn head_metadata(&mut self, metadata: &TikaMetadata) {
        (**self).head_metadata(metadata);
    }
}

/// How the parse of an embedded document ended, for the `package-entry` div around it and the
/// embedding document.
enum Ended {
    Clean,
    /// Tika recorded a failure in the embedding document's metadata, which goes on.
    Recorded,
    Stopped(Stop),
    /// A failure that fails the embedding document too.
    Failed(Failure),
}

/// Where the events of a document embedded in another go: through the embedding document's
/// `XHTMLContentHandler`, or to the content handler under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Xhtml,
    Handler,
}

impl Target {
    fn of(wrapping: Wrapping) -> Self {
        match wrapping {
            Wrapping::PackageEntry | Wrapping::Bare => Target::Xhtml,
            Wrapping::UnderlyingPackageEntry => Target::Handler,
        }
    }
}

/// Tika's `EmbeddedContentHandler`, which drops the start and end of an embedded document,
/// passing the rest on to the `target` of the embedding document, and the
/// `XHTMLBalancingHandler` around it, which keeps the names of the elements left open in `open`.
pub(crate) struct Embedded<'a, 'h> {
    pub(crate) outer: &'a mut Xhtml<'h>,
    pub(crate) open: &'a mut Vec<String>,
    pub(crate) target: Target,
}

impl ContentHandler for Embedded<'_, '_> {
    fn start_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        self.outer.start_prefix_mapping(prefix, uri)
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        self.outer.start_in(self.target, name, attributes)?;
        self.open.push(name.to_owned());
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.open.pop();
        self.outer.end_in(self.target, name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        self.outer.characters_in(self.target, text)
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        self.outer.whitespace_in(self.target, text)
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }
}

/// The handler of a document a parser writes into the `XHTMLContentHandler` of the document
/// it parses, as a parser does that hands that handler, shielded from `endDocument`, to another
/// parser with the same metadata: every event of the nested document, its `html`, `body` and
/// head elements included, is written into `outer` as an element of the XHTML, and the metadata
/// the nested document's head shows is the outer document's too, as the two share it.
pub(crate) struct Nested<'a, 'h> {
    pub(crate) outer: &'a mut Xhtml<'h>,
}

impl ContentHandler for Nested<'_, '_> {
    fn start_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        self.outer.start_prefix_mapping(prefix, uri)
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        self.outer.start_element_with(name, attributes)
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.outer.end_element(name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        self.outer.characters_chunk(text)
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        self.outer.ignorable_whitespace(text)
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn head_metadata(&mut self, metadata: &TikaMetadata) {
        *self.outer.metadata_mut() = metadata.clone();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    NotStarted,
    Started,
    Ended { frameset: bool },
}

/// A document being written as XHTML, holding its metadata until the head is written.
pub(crate) struct Xhtml<'h> {
    handler: &'h mut dyn ContentHandler,
    metadata: TikaMetadata,
    head: Head,
    ended: bool,
    embedding: Embedding<'h>,
    /// What learns the metadata the head shows, once it is written.
    on_head: Option<&'h mut dyn FnMut(&TikaMetadata)>,
}

impl<'h> Xhtml<'h> {
    /// Writes a document to `handler`: starts it, has `body` write its content, and ends it when
    /// `body` succeeds. A failed document stays unfinished, as a Tika parser that throws leaves
    /// it. Documents embedded in it are parsed or spooled as the extraction's `context` says.
    /// Returns the metadata as `body` left it, and what its embedded documents leave for it,
    /// which [`EmbeddedRecord::finish`] adds as Tika's `CompositeParser` does once a parser has
    /// returned.
    pub(crate) fn write<E: From<Stop>>(
        handler: &'h mut dyn ContentHandler,
        metadata: TikaMetadata,
        context: &'h ParseContext<'h>,
        body: impl FnOnce(&mut Xhtml) -> Result<(), E>,
    ) -> (TikaMetadata, EmbeddedRecord, Result<(), E>) {
        Self::write_reporting_head(handler, metadata, context, None, body)
    }

    /// Writes a document as [`Xhtml::write`] does, handing `on_head` the metadata the head shows
    /// once it is written: what a reader of the output can know of the document before its
    /// content.
    pub(crate) fn write_reporting_head<E: From<Stop>>(
        handler: &'h mut dyn ContentHandler,
        metadata: TikaMetadata,
        context: &'h ParseContext<'h>,
        on_head: Option<&'h mut dyn FnMut(&TikaMetadata)>,
        body: impl FnOnce(&mut Xhtml) -> Result<(), E>,
    ) -> (TikaMetadata, EmbeddedRecord, Result<(), E>) {
        let mut xhtml = Self {
            handler,
            metadata,
            head: Head::NotStarted,
            ended: false,
            embedding: Embedding::new(context),
            on_head,
        };
        let result = xhtml
            .start_document()
            .map_err(E::from)
            .and_then(|()| body(&mut xhtml))
            .and_then(|()| xhtml.end_document().map_err(E::from));
        (xhtml.metadata, xhtml.embedding.into_record(), result)
    }

    /// `XHTMLContentHandler.startDocument`: the document starts, in the XHTML namespace.
    fn start_document(&mut self) -> Result<(), Stop> {
        self.handler.start_document()?;
        self.handler.start_prefix_mapping("", XHTML_NAMESPACE)
    }

    /// Maps `prefix`, empty for the default namespace, to `uri`, as a parser does that hands
    /// Tika's handler the prefix mappings of the XML it reads: XML output declares the mapping on
    /// the next element it writes, unless the elements around that one declare it already.
    pub(crate) fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        self.handler.start_prefix_mapping(prefix, uri)
    }

    /// The metadata, which the head shows as it stands when the first body content is written.
    pub(crate) fn metadata_mut(&mut self) -> &mut TikaMetadata {
        &mut self.metadata
    }

    /// Records `parser` among the parsers of this document, as Tika's `CompositeParser` records
    /// the one it dispatches a document parsed into this one's handler to.
    pub(crate) fn record_parser(&mut self, parser: &str) {
        self.embedding.record_parser(parser);
    }

    /// Has `body` write the document with `metadata` in place of the document's, as a parser
    /// does that hands Tika's handler to another parser with metadata of its own: the head shows
    /// `metadata` as `body` leaves it, and the document's metadata is back once `body` returns.
    pub(crate) fn with_metadata<T>(
        &mut self,
        metadata: TikaMetadata,
        body: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let document = std::mem::replace(&mut self.metadata, metadata);
        let result = body(self);
        self.metadata = document;
        result
    }

    /// Records that the document's parser hands it to `parser`, as Tika's `CompositeParser`
    /// records the parser it delegates to: in `tk:parsed-by`, and among the parsers the
    /// extraction used.
    pub(crate) fn delegate_to(&mut self, parser: &str) {
        self.metadata.add("tk:parsed-by", parser);
        self.embedding.record_parser(parser);
    }

    pub(crate) fn start_element(&mut self, name: &str) -> Result<(), Stop> {
        self.start_element_with(name, &[])
    }

    pub(crate) fn start_element_with(
        &mut self,
        name: &str,
        attributes: &[(&str, &str)],
    ) -> Result<(), Stop> {
        within_budget()?;
        if name == "frameset" {
            return self.end_head(true);
        }
        if AUTO.contains(&name) {
            return Ok(());
        }
        if HEAD.contains(&name) {
            self.start_head()?;
        } else {
            self.end_head(false)?;
        }
        if INDENT.contains(&name) {
            self.handler.ignorable_whitespace("\t")?;
        }
        self.safe_start_element(name, attributes)
    }

    pub(crate) fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.end_element_in(XHTML_NAMESPACE, name)
    }

    /// Ends `name` as a parser that names it in `namespace` does: only an element ended in the
    /// XHTML namespace is followed by a line break.
    pub(crate) fn end_element_in(&mut self, namespace: &str, name: &str) -> Result<(), Stop> {
        within_budget()?;
        if AUTO.contains(&name) {
            return Ok(());
        }
        self.handler.end_element(name)?;
        if namespace == XHTML_NAMESPACE && ENDLINE.contains(&name) {
            self.newline()?;
        }
        Ok(())
    }

    /// Writes `text`; empty text writes nothing, as Tika's `characters(String)`.
    pub(crate) fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if text.is_empty() {
            return Ok(());
        }
        self.characters_chunk(text)
    }

    /// Writes `text`, as Tika's `characters(char[], int, int)`: empty text still ends the head.
    pub(crate) fn characters_chunk(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        let frameset = self.frameset();
        self.end_head(frameset)?;
        self.safe_text(text, Text::Characters)
    }

    /// Writes Java characters, as Tika's `characters(char[], int, int)`: a surrogate without its
    /// pair is invalid like the characters XML cannot hold, and each becomes U+FFFD.
    pub(crate) fn characters_utf16(&mut self, units: &[u16]) -> Result<(), Stop> {
        within_budget()?;
        let frameset = self.frameset();
        self.end_head(frameset)?;
        let mut run = String::with_capacity(units.len());
        for decoded in char::decode_utf16(units.iter().copied()) {
            match decoded {
                Ok(character) if !is_invalid(character) => run.push(character),
                _ => {
                    if !run.is_empty() {
                        self.handler.characters(&run)?;
                        run.clear();
                    }
                    self.handler
                        .characters(REPLACEMENT.encode_utf8(&mut [0; 4]))?;
                }
            }
        }
        self.handler.characters(&run)
    }

    pub(crate) fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        self.safe_text(text, Text::IgnorableWhitespace)
    }

    /// Whether `name` is a block element a line break ends (`XHTMLContentHandler.ENDLINE`).
    pub(crate) fn is_endline(name: &str) -> bool {
        ENDLINE.contains(&name)
    }

    /// Whether `name` is an element a tab starts (`XHTMLContentHandler.INDENT`).
    pub(crate) fn is_indented(name: &str) -> bool {
        INDENT.contains(&name)
    }

    /// The character Tika's safe content handler writes for `character`: U+FFFD for one XML
    /// cannot hold.
    pub(crate) fn safe_char(character: char) -> char {
        if is_invalid(character) {
            REPLACEMENT
        } else {
            character
        }
    }

    /// Starts `name` in the handler under the XHTML, as a parser does that writes to the content
    /// handler it was given rather than to its `XHTMLContentHandler`: in no namespace, without a
    /// tab, and without ending the head.
    pub(crate) fn raw_start_element(&mut self, name: &str) -> Result<(), Stop> {
        self.raw_start_element_with(name, &[])
    }

    /// Starts `name` with `attributes` in the handler under the XHTML, as
    /// [`Xhtml::raw_start_element`] does, the characters XML cannot hold as they are.
    pub(crate) fn raw_start_element_with(
        &mut self,
        name: &str,
        attributes: &[(&str, &str)],
    ) -> Result<(), Stop> {
        within_budget()?;
        self.handler.start_element(name, attributes)
    }

    /// Ends `name` in the handler under the XHTML, without a line break.
    pub(crate) fn raw_end_element(&mut self, name: &str) -> Result<(), Stop> {
        within_budget()?;
        self.handler.end_element(name)
    }

    /// Writes `text` to the handler under the XHTML, the characters XML cannot hold as they are.
    pub(crate) fn raw_characters(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        self.handler.characters(text)
    }

    /// Writes ignorable whitespace to the handler under the XHTML.
    fn raw_ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        self.handler.ignorable_whitespace(text)
    }

    fn start_in(
        &mut self,
        target: Target,
        name: &str,
        attributes: &[(&str, &str)],
    ) -> Result<(), Stop> {
        match target {
            Target::Xhtml => self.start_element_with(name, attributes),
            Target::Handler => self.raw_start_element_with(name, attributes),
        }
    }

    fn end_in(&mut self, target: Target, name: &str) -> Result<(), Stop> {
        match target {
            Target::Xhtml => self.end_element(name),
            Target::Handler => self.raw_end_element(name),
        }
    }

    fn characters_in(&mut self, target: Target, text: &str) -> Result<(), Stop> {
        match target {
            Target::Xhtml => self.characters_chunk(text),
            Target::Handler => self.raw_characters(text),
        }
    }

    fn whitespace_in(&mut self, target: Target, text: &str) -> Result<(), Stop> {
        match target {
            Target::Xhtml => self.ignorable_whitespace(text),
            Target::Handler => self.raw_ignorable_whitespace(text),
        }
    }

    pub(crate) fn newline(&mut self) -> Result<(), Stop> {
        self.handler.ignorable_whitespace("\n")
    }

    /// Writes an element holding `value`, or nothing when `value` is empty.
    pub(crate) fn element(&mut self, name: &str, value: &str) -> Result<(), Stop> {
        if value.is_empty() {
            return Ok(());
        }
        self.start_element(name)?;
        self.characters(value)?;
        self.end_element(name)
    }

    /// Ends the document, once: the body and `html`. [`Xhtml::write`] ends a document whose
    /// parser succeeds; a parser that ends its document even when it fails, as a Tika parser does
    /// in a `finally` block, calls this before it returns its failure.
    pub(crate) fn end_document(&mut self) -> Result<(), Stop> {
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        let frameset = self.frameset();
        self.end_head(frameset)?;
        self.handler
            .end_element(if frameset { "frameset" } else { "body" })?;
        self.handler.end_element("html")?;
        self.handler.end_document()
    }

    /// What the parses of this extraction share, which a document parsed into this one shares.
    pub(crate) fn context(&self) -> &'h ParseContext<'h> {
        self.embedding.context()
    }

    /// Parses a document embedded in this one, given `data` and the `metadata` the parser gives
    /// it, as Tika's `ParsingEmbeddedDocumentExtractor` does when a parser hands it this
    /// `XHTMLContentHandler`: unless it is a thumbnail, its body is written here, in a
    /// `package-entry` div headed by its name when `wrapping` asks for one, with the elements a
    /// failed parse left open closed before the div ends. Tika records the failures it can in
    /// this document's metadata; any other fails this document, after the content written
    /// before it. An extraction that spools its embedded documents copies it to the spool and
    /// writes nothing of it.
    ///
    /// A document a native parser takes ([`formats::route_embedded`]) is parsed here, its
    /// events passed on as they come, through an `EmbeddedContentHandler` over a
    /// `BodyContentHandler`, so the output stops it where it stops Tika's. Any other is parsed
    /// by Tika's path and its events replayed.
    pub(crate) fn parse_embedded(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
    ) -> Result<(), Failure> {
        self.parse_embedded_opened(data, metadata, wrapping, Opened::WithMetadata)
    }

    /// [`Xhtml::parse_embedded`] of a document whose `TikaInputStream` the parser `opened` as
    /// given.
    pub(crate) fn parse_embedded_opened(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
        opened: Opened,
    ) -> Result<(), Failure> {
        within_budget()?;
        if !embedded::is_parsed(metadata) || self.embedding.spool(data, metadata)? {
            return Ok(());
        }
        let target = Target::of(wrapping);
        let wrapped = wrapping != Wrapping::Bare;
        if wrapped {
            self.start_in(target, "div", &[("class", PACKAGE_ENTRY)])?;
            if let Some(name) = embedded::heading(metadata) {
                self.start_in(target, "h1", &[])?;
                self.characters_in(target, name)?;
                self.end_in(target, "h1")?;
            }
        }
        let mut open = Vec::new();
        let configs = self.embedding.context().configs();
        let ended = match formats::route_embedded(data, metadata, opened, configs) {
            Some(route) => self.parse_natively(route, data, metadata, &mut open, target),
            None => self.parse_with_tika(data, metadata, &mut open, target),
        };
        let (clean, failure) = match ended {
            Ended::Clean => (true, None),
            Ended::Recorded => (false, None),
            Ended::Stopped(stop) => (false, Some(Failure::Stopped(stop))),
            Ended::Failed(failure) => (false, Some(failure)),
        };
        if wrapped {
            if !clean {
                self.end_open(&mut open, target)?;
            }
            self.end_in(target, "div")?;
        }
        failure.map_or(Ok(()), Err)
    }

    /// Parses an embedded document with Tika's path and replays its events.
    fn parse_with_tika(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        open: &mut Vec<String>,
        target: Target,
    ) -> Ended {
        let parsed = match self.parse_with_tika_quietly(data, metadata) {
            Ok(parsed) => parsed,
            Err(failure) => return Ended::Failed(failure),
        };
        if let EmbeddedOutcome::Parsed { metadata, .. } = &parsed.outcome {
            self.embedding.absorb_parsers(metadata);
        }
        let events = match embedded::decode(&parsed.events) {
            Ok(events) => events,
            Err(failure) => return Ended::Failed(failure),
        };
        if let Err(stop) = self.replay(events, open, target) {
            return Ended::Stopped(stop);
        }
        match parsed.outcome {
            EmbeddedOutcome::Parsed {
                metadata,
                recorded_exception,
            } => {
                let clean = recorded_exception.is_none();
                self.embedding
                    .absorb_failures(&metadata, recorded_exception);
                if clean {
                    Ended::Clean
                } else {
                    Ended::Recorded
                }
            }
            EmbeddedOutcome::Failed(error) => Ended::Failed(Failure::Embedded(error)),
        }
    }

    /// Tika's path's parse of an embedded document, its events and how it ended, which this
    /// document has not taken in yet.
    fn parse_with_tika_quietly(
        &self,
        data: &[u8],
        metadata: &TikaMetadata,
    ) -> Result<tika::EmbeddedEvents, Failure> {
        isolate()
            .and_then(|isolate| {
                tika::parse_embedded(
                    isolate,
                    data,
                    &metadata.pairs(),
                    self.embedding.context().configs(),
                    None,
                )
            })
            .map_err(Failure::Embedded)
    }

    /// Parses an embedded document with the native `parser`, from the metadata `start` it
    /// starts from, its events passed on to this document as they come. Where the parser fails
    /// as Tika's would throw, Tika records the exception its own parser throws, which only its
    /// path can tell: that path parses the document again, for the exception alone.
    fn parse_natively(
        &mut self,
        route: EmbeddedRoute<'_>,
        data: &[u8],
        metadata: &TikaMetadata,
        open: &mut Vec<String>,
        target: Target,
    ) -> Ended {
        let EmbeddedRoute {
            parser,
            start,
            prepared,
        } = route;
        let context = self.embedding.context();
        let (mut parsed, record, result) = {
            let mut forward = Embedded {
                outer: self,
                open,
                target,
            };
            let mut body = Body::new(&mut forward);
            Xhtml::write(&mut body, start, context, prepared)
        };
        record.finish(&mut parsed, parser.name());
        let parsed = parsed.to_map();
        self.embedding.absorb_parsers(&parsed);
        let reason = match result {
            Ok(()) => {
                self.embedding.absorb_failures(&parsed, None);
                return Ended::Clean;
            }
            Err(Failure::Stopped(stop)) => return Ended::Stopped(stop),
            Err(failure @ Failure::Embedded(_)) => return Ended::Failed(failure),
            Err(Failure::Malformed(reason)) => reason,
            Err(Failure::Extraction(error)) => error.to_string(),
        };
        let tika = match self.parse_with_tika_quietly(data, metadata) {
            Ok(tika) => tika,
            Err(failure) => return Ended::Failed(failure),
        };
        match tika.outcome {
            EmbeddedOutcome::Parsed {
                recorded_exception: Some(exception),
                ..
            } => {
                self.embedding.absorb_failures(&parsed, Some(exception));
                Ended::Recorded
            }
            EmbeddedOutcome::Parsed {
                recorded_exception: None,
                ..
            } => Ended::Failed(Failure::Extraction(Error::ParseError(format!(
                "{} failed on an embedded document Tika's path parses: {reason}",
                parser.name()
            )))),
            EmbeddedOutcome::Failed(error) => Ended::Failed(Failure::Embedded(error)),
        }
    }

    /// Writes the events of an embedded document, keeping the names of the elements left open
    /// in `open`, until the output stops taking them.
    fn replay(
        &mut self,
        events: Vec<Event>,
        open: &mut Vec<String>,
        target: Target,
    ) -> Result<(), Stop> {
        for event in events {
            match event {
                Event::Start { name, attributes } => {
                    let borrowed: Vec<(&str, &str)> = attributes
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str()))
                        .collect();
                    self.start_in(target, &name, &borrowed)?;
                    open.push(name);
                }
                Event::End(name) => {
                    open.pop();
                    self.end_in(target, &name)?;
                }
                Event::Characters(text) => self.characters_in(target, &text)?,
                Event::IgnorableWhitespace(text) => self.whitespace_in(target, &text)?,
            }
        }
        Ok(())
    }

    /// `XHTMLBalancingHandler.drainOpenElements`: ends the elements an aborted embedded parse
    /// left open, innermost first.
    fn end_open(&mut self, open: &mut Vec<String>, target: Target) -> Result<(), Stop> {
        while let Some(name) = open.pop() {
            self.end_in(target, &name)?;
        }
        Ok(())
    }

    fn frameset(&self) -> bool {
        matches!(self.head, Head::Ended { frameset: true })
    }

    fn start_head(&mut self) -> Result<(), Stop> {
        if self.head != Head::NotStarted {
            return Ok(());
        }
        self.head = Head::Started;
        self.handler.head_metadata(&self.metadata);
        let language = self.metadata.get("Content-Language").map(str::to_owned);
        match language {
            Some(language) => self.safe_start_element("html", &[("lang", &language)])?,
            None => self.handler.start_element("html", &[])?,
        }
        self.newline()?;
        self.handler.start_element("head", &[])?;
        self.newline()
    }

    fn end_head(&mut self, frameset: bool) -> Result<(), Stop> {
        self.start_head()?;
        if matches!(self.head, Head::Ended { .. }) {
            return Ok(());
        }
        self.head = Head::Ended { frameset };
        let metadata = std::mem::take(&mut self.metadata);
        let written = self.write_head(&metadata);
        self.metadata = metadata;
        written?;
        if let Some(on_head) = &mut self.on_head {
            on_head(&self.metadata);
        }
        self.handler.end_element("head")?;
        self.newline()?;
        self.handler
            .start_element(if frameset { "frameset" } else { "body" }, &[])
    }

    fn write_head(&mut self, metadata: &TikaMetadata) -> Result<(), Stop> {
        for name in metadata.names().filter(|name| *name != "title") {
            for value in metadata.values(name) {
                self.safe_start_element("meta", &[("name", name), ("content", value)])?;
                self.handler.end_element("meta")?;
                self.newline()?;
            }
        }
        self.handler.start_element("title", &[])?;
        if let Some(title) = metadata.get("dc:title") {
            self.safe_text(title, Text::Characters)?;
        } else {
            self.handler.characters("")?;
        }
        self.handler.end_element("title")?;
        self.newline()
    }

    fn safe_start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        if !attributes.iter().any(|(_, value)| has_invalid(value)) {
            return self.handler.start_element(name, attributes);
        }
        let cleaned: Vec<(&str, String)> = attributes
            .iter()
            .map(|(attribute, value)| (*attribute, replace_invalid(value)))
            .collect();
        let borrowed: Vec<(&str, &str)> = cleaned
            .iter()
            .map(|(attribute, value)| (*attribute, value.as_str()))
            .collect();
        self.handler.start_element(name, &borrowed)
    }

    fn safe_text(&mut self, text: &str, kind: Text) -> Result<(), Stop> {
        let mut start = 0;
        for (index, character) in text.char_indices() {
            if is_invalid(character) {
                if index > start {
                    self.write_text(&text[start..index], kind)?;
                }
                self.write_text(REPLACEMENT.encode_utf8(&mut [0; 4]), kind)?;
                start = index + character.len_utf8();
            }
        }
        self.write_text(&text[start..], kind)
    }

    fn write_text(&mut self, text: &str, kind: Text) -> Result<(), Stop> {
        match kind {
            Text::Characters => self.handler.characters(text),
            Text::IgnorableWhitespace => self.handler.ignorable_whitespace(text),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Text {
    Characters,
    IgnorableWhitespace,
}

/// Whether XHTML cannot hold `character`: C0 controls other than tab, line feed and carriage
/// return, C1 controls and DEL, and the non-characters U+FFFE and U+FFFF.
fn is_invalid(character: char) -> bool {
    matches!(character,
        '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{9f}'
        | '\u{fffe}' | '\u{ffff}')
}

fn has_invalid(text: &str) -> bool {
    text.chars().any(is_invalid)
}

fn replace_invalid(text: &str) -> String {
    text.chars()
        .map(|character| {
            if is_invalid(character) {
                REPLACEMENT
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ParserConfigs;
    use crate::formats::{Chain, Output, StringOut};

    #[test]
    fn closes_an_embedded_document_the_output_stops_in() {
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
}
