//! Tika's `XHTMLContentHandler`, with the `SafeContentHandler` it extends: what a parser calls to
//! write its document as XHTML. It opens `html`, writes the `head` from the metadata before the
//! first body content, indents table cells and list items with a tab, ends block elements with a
//! line break and replaces characters XML cannot hold with U+FFFD.

use crate::formats::embedded::{
    self, EmbeddedRecord, Embedding, ParseContext, Wrapping, PACKAGE_ENTRY,
};
use crate::formats::handlers::Body;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{self, Bridged, Declined, EmbeddedRoute, Failure, Opened, Unrouted};
use crate::memory;
#[cfg(feature = "tika")]
use crate::tika::{self, EmbeddedOutcome, Event};
use crate::Metadata;

/// What an embedded document no native parser reads is recorded as unread by, in a build
/// without Tika.
#[cfg(not(feature = "tika"))]
const UNSUPPORTED: &str = "extractous";

/// Whether the extraction holds no more memory than its budget, which it checks as it writes.
pub fn within_budget() -> Result<(), Stop> {
    memory::overrun().map_or(Ok(()), |limit| Err(Stop::MemoryLimit(limit)))
}

/// The XHTML namespace, which every element a native parser writes is in.
pub const XHTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";

pub const ENDLINE: [&str; 29] = [
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
pub const INDENT: [&str; 6] = ["li", "dd", "dt", "td", "th", "frame"];
const REPLACEMENT: char = '\u{fffd}';

/// Why the output stopped taking content before the parser finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The text reached the extraction's length limit, as Tika's `WriteLimitReachedException`.
    LimitReached,
    /// Whoever was reading the output went away.
    ReaderGone,
    /// The extraction holds more memory than its budget of this many bytes.
    MemoryLimit(u64),
}

/// Where the XHTML of a document goes: the SAX events Tika's handlers receive, all in the XHTML
/// namespace and named by their local name, and the namespace prefixes a parser maps.
pub trait ContentHandler {
    fn start_document(&mut self) -> Result<(), Stop>;
    /// SAX's `startPrefixMapping`: `prefix`, empty for the default namespace, stands for `uri`
    /// from the next element on.
    fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop>;
    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop>;
    fn end_element(&mut self, name: &str) -> Result<(), Stop>;
    fn characters(&mut self, text: &str) -> Result<(), Stop>;
    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop>;
    fn end_document(&mut self) -> Result<(), Stop>;

    /// Passes on what the handler has written so far, for a reader to have it before the parser
    /// waits; a handler that writes nowhere has nothing to pass on.
    fn flush(&mut self) -> Result<(), Stop> {
        Ok(())
    }

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

    fn flush(&mut self) -> Result<(), Stop> {
        (**self).flush()
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

    /// Ends `name`, unless the embedded document has no element of that name open: a parser
    /// stopped by the length limit inside its body ends its document, whose `body` end tag the
    /// `BodyContentHandler` in front of this passes on while the body's elements are still open.
    /// Tika's path writes that `</body>` into the embedding document (#113).
    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        if !self.open.iter().any(|open| open == name) {
            return Ok(());
        }
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
pub struct Nested<'a, 'h> {
    pub outer: &'a mut Xhtml<'h>,
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

/// What a document wrote while its handler was held back from: the events as the handler would
/// have received them, and the text among them, as Tika's `TextLayerCheckingPdfParser` records
/// a page.
#[derive(Debug, Default)]
pub struct Held {
    events: Vec<HeldEvent>,
    text: String,
}

#[derive(Debug)]
enum HeldEvent {
    StartDocument,
    PrefixMapping(String, String),
    Start(String, Vec<(String, String)>),
    End(String),
    Characters(String),
    Whitespace(String),
    EndDocument,
}

impl Held {
    /// The characters and ignorable whitespace written, in order.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl ContentHandler for Held {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.events.push(HeldEvent::StartDocument);
        Ok(())
    }

    fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        self.events
            .push(HeldEvent::PrefixMapping(prefix.to_owned(), uri.to_owned()));
        Ok(())
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        let attributes = attributes
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        self.events
            .push(HeldEvent::Start(name.to_owned(), attributes));
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.events.push(HeldEvent::End(name.to_owned()));
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        self.text.push_str(text);
        self.events.push(HeldEvent::Characters(text.to_owned()));
        Ok(())
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        self.text.push_str(text);
        self.events.push(HeldEvent::Whitespace(text.to_owned()));
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.events.push(HeldEvent::EndDocument);
        Ok(())
    }
}

/// A document being written as XHTML, holding its metadata until the head is written.
pub struct Xhtml<'h> {
    handler: &'h mut dyn ContentHandler,
    /// What is held back from the handler, the innermost last.
    held: Vec<Held>,
    metadata: TikaMetadata,
    head: Head,
    ended: bool,
    embedding: Embedding<'h>,
    /// The bytes of text written since the pages of the document's bytes were last released.
    written: usize,
}

/// How many bytes of text a document writes between releases of the pages of the document's
/// bytes: a parser reading a file mapped too large to read whole writes its text as it reads on,
/// so that what it has read is released as it goes, whatever the parser.
const RELEASE_EVERY_WRITTEN: usize = 8 << 20;

impl<'h> Xhtml<'h> {
    /// Writes a document to `handler`: starts it, has `body` write its content, and ends it when
    /// `body` succeeds. A failed document stays unfinished, as a Tika parser that throws leaves
    /// it. Documents embedded in it are parsed or spooled as the extraction's `context` says.
    /// Returns the metadata as `body` left it, and what its embedded documents leave for it,
    /// which [`EmbeddedRecord::finish`] adds as Tika's `CompositeParser` does once a parser has
    /// returned.
    pub fn write<E: From<Stop>>(
        handler: &'h mut dyn ContentHandler,
        metadata: TikaMetadata,
        context: &'h ParseContext<'h>,
        body: impl FnOnce(&mut Xhtml) -> Result<(), E>,
    ) -> (TikaMetadata, EmbeddedRecord, Result<(), E>) {
        let mut xhtml = Self {
            handler,
            held: Vec::new(),
            metadata,
            head: Head::NotStarted,
            ended: false,
            embedding: Embedding::new(context),
            written: 0,
        };
        let result = xhtml
            .start_document()
            .map_err(E::from)
            .and_then(|()| body(&mut xhtml))
            .and_then(|()| xhtml.end_document().map_err(E::from));
        (xhtml.metadata, xhtml.embedding.into_record(), result)
    }

    /// Where the events go: what holds them back, or the handler.
    fn out(&mut self) -> &mut dyn ContentHandler {
        match self.held.last_mut() {
            Some(held) => held,
            None => &mut *self.handler,
        }
    }

    /// Passes on what is written so far, unless it is held back, for whoever reads the output
    /// to have it before the parser waits.
    pub fn flush(&mut self) -> Result<(), Stop> {
        self.out().flush()
    }

    /// Runs `write`, its events, those of the documents it parses into this one included, held
    /// back from the handler: what it wrote, to be released or dropped. Everything else it does,
    /// to the metadata and the embedded documents' record, happens as it would.
    pub fn hold<E>(&mut self, write: impl FnOnce(&mut Self) -> Result<(), E>) -> Result<Held, E> {
        self.held.push(Held::default());
        let written = write(self);
        let held = self.held.pop().expect("what was held is still held");
        written.map(|()| held)
    }

    /// Passes on what was held back, as the handler would have received it.
    pub fn release(&mut self, held: Held) -> Result<(), Stop> {
        for event in held.events {
            within_budget()?;
            let out = self.out();
            match event {
                HeldEvent::StartDocument => out.start_document()?,
                HeldEvent::PrefixMapping(prefix, uri) => out.start_prefix_mapping(&prefix, &uri)?,
                HeldEvent::Start(name, attributes) => {
                    let attributes: Vec<(&str, &str)> = attributes
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str()))
                        .collect();
                    out.start_element(&name, &attributes)?;
                }
                HeldEvent::End(name) => out.end_element(&name)?,
                HeldEvent::Characters(text) => out.characters(&text)?,
                HeldEvent::Whitespace(text) => out.ignorable_whitespace(&text)?,
                HeldEvent::EndDocument => out.end_document()?,
            }
        }
        Ok(())
    }

    /// `XHTMLContentHandler.startDocument`: the document starts, in the XHTML namespace.
    fn start_document(&mut self) -> Result<(), Stop> {
        self.out().start_document()?;
        self.out().start_prefix_mapping("", XHTML_NAMESPACE)
    }

    /// Maps `prefix`, empty for the default namespace, to `uri`, as a parser does that hands
    /// Tika's handler the prefix mappings of the XML it reads: XML output declares the mapping on
    /// the next element it writes, unless the elements around that one declare it already.
    pub fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        self.out().start_prefix_mapping(prefix, uri)
    }

    /// The metadata, which the head shows as it stands when the first body content is written.
    pub fn metadata_mut(&mut self) -> &mut TikaMetadata {
        &mut self.metadata
    }

    /// Records `parser` among the parsers of this document, as Tika's `CompositeParser` records
    /// the one it dispatches a document parsed into this one's handler to.
    pub fn record_parser(&mut self, parser: &str) {
        self.embedding.record_parser(parser);
    }

    /// Has `body` write the document with `metadata` in place of the document's, as a parser
    /// does that hands Tika's handler to another parser with metadata of its own: the head shows
    /// `metadata` as `body` leaves it, and the document's metadata is back once `body` returns.
    pub fn with_metadata<T>(
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
    pub fn delegate_to(&mut self, parser: &str) {
        self.metadata.add("tk:parsed-by", parser);
        self.embedding.record_parser(parser);
    }

    pub fn start_element(&mut self, name: &str) -> Result<(), Stop> {
        self.start_element_with(name, &[])
    }

    pub fn start_element_with(
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
            self.out().ignorable_whitespace("\t")?;
        }
        self.safe_start_element(name, attributes)
    }

    pub fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.end_element_in(XHTML_NAMESPACE, name)
    }

    /// Ends `name` as a parser that names it in `namespace` does: only an element ended in the
    /// XHTML namespace is followed by a line break.
    pub fn end_element_in(&mut self, namespace: &str, name: &str) -> Result<(), Stop> {
        within_budget()?;
        if AUTO.contains(&name) {
            return Ok(());
        }
        self.out().end_element(name)?;
        if namespace == XHTML_NAMESPACE && ENDLINE.contains(&name) {
            self.newline()?;
        }
        Ok(())
    }

    /// Ends `name` as another `XHTMLContentHandler` ended it, which broke the line after it
    /// itself: a handler below this one receives its events as they are.
    pub fn end_element_passed_on(&mut self, name: &str) -> Result<(), Stop> {
        within_budget()?;
        self.out().end_element(name)
    }

    /// Writes `text`; empty text writes nothing, as Tika's `characters(String)`.
    pub fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if text.is_empty() {
            return Ok(());
        }
        self.characters_chunk(text)
    }

    /// Ends the head and starts the body, as the first content of the body does: the head shows
    /// the metadata as it stands now.
    pub fn start_body(&mut self) -> Result<(), Stop> {
        self.end_head(false)
    }

    /// Writes `text`, as Tika's `characters(char[], int, int)`: empty text still ends the head.
    pub fn characters_chunk(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        self.wrote(text.len());
        let frameset = self.frameset();
        self.end_head(frameset)?;
        self.safe_text(text, Text::Characters)
    }

    /// Writes Java characters, as Tika's `characters(char[], int, int)`: a surrogate without its
    /// pair is invalid like the characters XML cannot hold, and each becomes U+FFFD.
    pub fn characters_utf16(&mut self, units: &[u16]) -> Result<(), Stop> {
        within_budget()?;
        self.wrote(units.len() * 2);
        let frameset = self.frameset();
        self.end_head(frameset)?;
        let mut run = String::with_capacity(units.len());
        for decoded in char::decode_utf16(units.iter().copied()) {
            match decoded {
                Ok(character) if !is_invalid(character) => run.push(character),
                _ => {
                    if !run.is_empty() {
                        self.out().characters(&run)?;
                        run.clear();
                    }
                    self.out()
                        .characters(REPLACEMENT.encode_utf8(&mut [0; 4]))?;
                }
            }
        }
        self.out().characters(&run)
    }

    pub fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        self.safe_text(text, Text::IgnorableWhitespace)
    }

    /// Whether `name` is a block element a line break ends (`XHTMLContentHandler.ENDLINE`).
    pub fn is_endline(name: &str) -> bool {
        ENDLINE.contains(&name)
    }

    /// Whether `name` is an element a tab starts (`XHTMLContentHandler.INDENT`).
    pub fn is_indented(name: &str) -> bool {
        INDENT.contains(&name)
    }

    /// The character Tika's safe content handler writes for `character`: U+FFFD for one XML
    /// cannot hold.
    pub fn safe_char(character: char) -> char {
        if is_invalid(character) {
            REPLACEMENT
        } else {
            character
        }
    }

    /// Starts `name` in the handler under the XHTML, as a parser does that writes to the content
    /// handler it was given rather than to its `XHTMLContentHandler`: in no namespace, without a
    /// tab, and without ending the head.
    pub fn raw_start_element(&mut self, name: &str) -> Result<(), Stop> {
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
        self.out().start_element(name, attributes)
    }

    /// Ends `name` in the handler under the XHTML, without a line break.
    pub fn raw_end_element(&mut self, name: &str) -> Result<(), Stop> {
        within_budget()?;
        self.out().end_element(name)
    }

    /// Writes `text` to the handler under the XHTML, the characters XML cannot hold as they are.
    pub fn raw_characters(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        self.out().characters(text)
    }

    /// Writes ignorable whitespace to the handler under the XHTML.
    fn raw_ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        within_budget()?;
        self.out().ignorable_whitespace(text)
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

    pub fn newline(&mut self) -> Result<(), Stop> {
        self.out().ignorable_whitespace("\n")
    }

    /// Writes an element holding `value`, or nothing when `value` is empty.
    pub fn element(&mut self, name: &str, value: &str) -> Result<(), Stop> {
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
    pub fn end_document(&mut self) -> Result<(), Stop> {
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        let frameset = self.frameset();
        self.end_head(frameset)?;
        self.out()
            .end_element(if frameset { "frameset" } else { "body" })?;
        self.out().end_element("html")?;
        self.out().end_document()
    }

    /// Counts `bytes` of text written, releasing the pages of the document's bytes each time
    /// [`RELEASE_EVERY_WRITTEN`] have been.
    fn wrote(&mut self, bytes: usize) {
        self.written += bytes;
        if self.written >= RELEASE_EVERY_WRITTEN {
            self.context().pages().release();
            self.written = 0;
        }
    }

    /// What the parses of this extraction share, which a document parsed into this one shares.
    pub fn context(&self) -> &'h ParseContext<'h> {
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
    pub fn parse_embedded(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
    ) -> Result<(), Failure> {
        self.parse_embedded_opened(data, metadata, wrapping, Opened::WithMetadata)
    }

    /// Parses a storage of a compound document embedded in this one with a native parser, as
    /// [`Xhtml::parse_embedded`] parses a document: `write`, which `parser` runs, writes the
    /// document from `metadata`, and its body goes into this one; an extraction that spools its
    /// embedded documents copies `spooled`, the storage as a document of its own. Where `write`
    /// finds the document malformed, as Tika's parser would throw, the failure is recorded in
    /// this document's metadata, which goes on, as for a document [`Xhtml::parse_embedded`]
    /// parses.
    pub fn parse_container_natively(
        &mut self,
        spooled: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
        parser: &str,
        write: impl FnOnce(&mut Xhtml) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        if !self.enter_embedded(spooled, metadata, wrapping)? {
            return Ok(());
        }
        let mut open = Vec::new();
        let target = Target::of(wrapping);
        let (parsed, result) =
            self.write_natively(metadata.clone(), parser, write, &mut open, target);
        let ended = match result {
            Ok(()) => {
                self.embedding.absorb_failures(&parsed, None);
                Ended::Clean
            }
            Err(Failure::Stopped(stop)) => Ended::Stopped(stop),
            Err(Failure::Extraction(error)) => Ended::Failed(Failure::Embedded(error)),
            Err(Failure::Malformed(reason)) => {
                let failure = embedded::describe_failure(parser, metadata, &reason);
                self.embedding.absorb_failures(&parsed, Some(failure));
                Ended::Recorded
            }
            Err(failure) => Ended::Failed(failure),
        };
        self.leave_embedded(ended, open, wrapping)
    }

    /// [`Xhtml::parse_embedded`] of a document whose `TikaInputStream` the parser `opened` as
    /// given.
    pub fn parse_embedded_opened(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
        opened: Opened,
    ) -> Result<(), Failure> {
        if !self.enter_embedded(data, metadata, wrapping)? {
            return Ok(());
        }
        let mut open = Vec::new();
        let ended = self.parse_routed(data, metadata, opened, &mut open, Target::of(wrapping));
        self.leave_embedded(ended, open, wrapping)
    }

    /// Starts an embedded document that is parsed rather than spooled, `spooled` being what an
    /// extraction that spools copies of it: whether it is parsed.
    fn enter_embedded(
        &mut self,
        spooled: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
    ) -> Result<bool, Failure> {
        within_budget()?;
        if !embedded::is_parsed(metadata) || self.embedding.spool(spooled, metadata)? {
            return Ok(false);
        }
        if wrapping != Wrapping::Bare {
            let target = Target::of(wrapping);
            self.start_in(target, "div", &[("class", PACKAGE_ENTRY)])?;
            if let Some(name) = embedded::heading(metadata) {
                self.start_in(target, "h1", &[])?;
                self.characters_in(target, name)?;
                self.end_in(target, "h1")?;
            }
        }
        Ok(true)
    }

    /// Parses an embedded document with its native parser, if it has one, and with Tika's
    /// path otherwise. One of no bytes no parser starts: its failure is recorded as Tika's
    /// zero-byte check fails it.
    fn parse_routed(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        opened: Opened,
        open: &mut Vec<String>,
        target: Target,
    ) -> Ended {
        let configs = self.embedding.context().configs();
        match formats::route_embedded(data, metadata, opened, configs) {
            Ok(route) => self.parse_natively(route, metadata, open, target),
            Err(Bridged::Unrouted(Unrouted::Empty)) => {
                self.embedding.record_zero_bytes();
                Ended::Recorded
            }
            Err(Bridged::Declined(parser, Declined::Unwritten(start))) => {
                let parsed = formats::unwritten(parser, start, configs).to_map();
                self.embedding.absorb_parsers(&parsed);
                self.embedding.absorb_failures(&parsed, None);
                Ended::Clean
            }
            #[cfg(feature = "tika")]
            Err(bridged) => self.parse_with_tika(bridged, data, metadata, open, target),
            #[cfg(not(feature = "tika"))]
            Err(Bridged::Declined(parser, Declined::Fails(error))) => {
                self.record_unread(parser, metadata, &embedded::error_reason(&error))
            }
            #[cfg(not(feature = "tika"))]
            Err(Bridged::Declined(parser, Declined::Deferred(reason))) => {
                let unsupported = crate::Unsupported::Deferred { parser, reason };
                self.record_unread(UNSUPPORTED, metadata, &unsupported.to_string())
            }
            #[cfg(not(feature = "tika"))]
            Err(Bridged::Unrouted(Unrouted::Unsupported(unsupported))) => {
                self.record_unread(UNSUPPORTED, metadata, &unsupported.to_string())
            }
        }
    }

    /// Records that an embedded document stays unread, in a build without Tika, where its parse
    /// on Tika's path would be: what did not read it, which document and why. The document
    /// embedding it goes on, as it does past an embedded document whose parser fails.
    #[cfg(not(feature = "tika"))]
    fn record_unread(&mut self, reader: &str, metadata: &TikaMetadata, reason: &str) -> Ended {
        let failure = embedded::describe_failure(reader, metadata, reason);
        self.embedding
            .absorb_failures(&Metadata::new(), Some(failure));
        Ended::Recorded
    }

    /// Ends an embedded document as its parse `ended`: the elements it left open closed where
    /// it did not end cleanly, and its `package-entry` div.
    fn leave_embedded(
        &mut self,
        ended: Ended,
        mut open: Vec<String>,
        wrapping: Wrapping,
    ) -> Result<(), Failure> {
        let (clean, failure) = match ended {
            Ended::Clean => (true, None),
            Ended::Recorded => (false, None),
            Ended::Stopped(stop) => (false, Some(Failure::Stopped(stop))),
            Ended::Failed(failure) => (false, Some(failure)),
        };
        if wrapping != Wrapping::Bare {
            let target = Target::of(wrapping);
            if !clean {
                self.end_open(&mut open, target)?;
            }
            self.end_in(target, "div")?;
        }
        failure.map_or(Ok(()), Err)
    }

    /// Parses an embedded document, which no native parser prepares for the reason `bridged`
    /// gives, with Tika's path and replays its events.
    #[cfg(feature = "tika")]
    fn parse_with_tika(
        &mut self,
        bridged: Bridged,
        data: &[u8],
        metadata: &TikaMetadata,
        open: &mut Vec<String>,
        target: Target,
    ) -> Ended {
        let parsed = match self.parse_with_tika_quietly(bridged, data, metadata) {
            Ok(parsed) => parsed,
            Err(failure) => return Ended::Failed(failure),
        };
        if let EmbeddedOutcome::Parsed { metadata, .. } = &parsed.outcome {
            self.embedding.absorb_parsers(metadata);
        }
        let events = match tika::decode_events(&parsed.events) {
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

    /// Tika's path's parse of an embedded document `bridged` to it, its events and how it ended,
    /// which this document has not taken in yet. It is parsed with the extraction's password,
    /// as Tika's path parses every embedded document under the password its parse context
    /// provides.
    #[cfg(feature = "tika")]
    fn parse_with_tika_quietly(
        &self,
        bridged: Bridged,
        data: &[u8],
        metadata: &TikaMetadata,
    ) -> Result<tika::EmbeddedEvents, Failure> {
        formats::parse_with_tika(
            bridged,
            data,
            &metadata.pairs(),
            self.embedding.context().configs(),
        )
        .map_err(Failure::Embedded)
    }

    /// Parses an embedded document with the native `parser`, from the metadata `start` it
    /// starts from, its events passed on to this document as they come. Where the parser fails
    /// on the document, as Tika's would throw, the failure is recorded in this document's
    /// metadata, which goes on: what failed, on which document and why, where Tika records the
    /// stack trace of the exception its own parser throws (#68).
    fn parse_natively(
        &mut self,
        route: EmbeddedRoute<'_>,
        metadata: &TikaMetadata,
        open: &mut Vec<String>,
        target: Target,
    ) -> Ended {
        let EmbeddedRoute {
            parser,
            start,
            prepared,
        } = route;
        let (parsed, result) = self.write_natively(start, parser.name(), prepared, open, target);
        let reason = match result {
            Ok(()) => {
                self.embedding.absorb_failures(&parsed, None);
                return Ended::Clean;
            }
            Err(Failure::Stopped(stop)) => return Ended::Stopped(stop),
            Err(failure @ Failure::Embedded(_)) => return Ended::Failed(failure),
            Err(Failure::Malformed(reason)) => reason,
            Err(Failure::Extraction(error)) => embedded::error_reason(&error),
        };
        let failure = embedded::describe_failure(parser.name(), metadata, &reason);
        self.embedding.absorb_failures(&parsed, Some(failure));
        Ended::Recorded
    }

    /// Writes an embedded document with the native parser named `parser`, which `write` runs,
    /// from the metadata `start` it starts from, its events passed on to this document as they
    /// come: the metadata it ends with, which this document takes its parsers from, and how it
    /// ended.
    fn write_natively(
        &mut self,
        start: TikaMetadata,
        parser: &str,
        write: impl FnOnce(&mut Xhtml) -> Result<(), Failure>,
        open: &mut Vec<String>,
        target: Target,
    ) -> (Metadata, Result<(), Failure>) {
        let context = self.embedding.context();
        let (mut parsed, record, result) = {
            let mut forward = Embedded {
                outer: self,
                open,
                target,
            };
            let mut body = Body::new(&mut forward);
            Xhtml::write(&mut body, start, context, write)
        };
        record.finish(&mut parsed, parser);
        let parsed = parsed.to_map();
        self.embedding.absorb_parsers(&parsed);
        (parsed, result)
    }

    /// Writes the events of an embedded document, keeping the names of the elements left open
    /// in `open`, until the output stops taking them.
    #[cfg(feature = "tika")]
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
        match self.held.last_mut() {
            Some(held) => held.head_metadata(&self.metadata),
            None => self.handler.head_metadata(&self.metadata),
        }
        let language = self.metadata.get("Content-Language").map(str::to_owned);
        match language {
            Some(language) => self.safe_start_element("html", &[("lang", &language)])?,
            None => self.out().start_element("html", &[])?,
        }
        self.newline()?;
        self.out().start_element("head", &[])?;
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
        self.out().end_element("head")?;
        self.newline()?;
        self.out()
            .start_element(if frameset { "frameset" } else { "body" }, &[])
    }

    /// Writes the metadata as `meta` elements sorted by name, where Tika writes them in the
    /// order of its metadata's `HashMap` (#64), then the title.
    fn write_head(&mut self, metadata: &TikaMetadata) -> Result<(), Stop> {
        for name in metadata.names().filter(|name| *name != "title") {
            for value in metadata.values(name) {
                self.safe_start_element("meta", &[("name", name), ("content", value)])?;
                self.out().end_element("meta")?;
                self.newline()?;
            }
        }
        self.out().start_element("title", &[])?;
        if let Some(title) = metadata.get("dc:title") {
            self.safe_text(title, Text::Characters)?;
        } else {
            self.out().characters("")?;
        }
        self.out().end_element("title")?;
        self.newline()
    }

    fn safe_start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        if !attributes.iter().any(|(_, value)| has_invalid(value)) {
            return self.out().start_element(name, attributes);
        }
        let cleaned: Vec<(&str, String)> = attributes
            .iter()
            .map(|(attribute, value)| (*attribute, replace_invalid(value)))
            .collect();
        let borrowed: Vec<(&str, &str)> = cleaned
            .iter()
            .map(|(attribute, value)| (*attribute, value.as_str()))
            .collect();
        self.out().start_element(name, &borrowed)
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
            Text::Characters => self.out().characters(text),
            Text::IgnorableWhitespace => self.out().ignorable_whitespace(text),
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
