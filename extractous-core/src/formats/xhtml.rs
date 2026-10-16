//! Tika's `XHTMLContentHandler`, with the `SafeContentHandler` it extends: what a parser calls to
//! write its document as XHTML. It opens `html`, writes the `head` from the metadata before the
//! first body content, indents table cells and list items with a tab, ends block elements with a
//! line break and replaces characters XML cannot hold with U+FFFD.

use crate::formats::embedded::{
    self, EmbeddedRecord, Embedding, Event, ParseContext, Wrapping, PACKAGE_ENTRY,
};
use crate::formats::handlers::Body;
use crate::formats::metadata::TikaMetadata;
use crate::formats::Failure;
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
    /// How many XHTML handlers the content passes through: more than one while a parser writes
    /// that another parser hands its handler on to.
    layers: usize,
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
            layers: 1,
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
            for _ in 0..self.layers {
                self.handler.ignorable_whitespace("\t")?;
            }
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
            for _ in 0..self.layers {
                self.newline()?;
            }
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
    fn characters_chunk(&mut self, text: &str) -> Result<(), Stop> {
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
    pub(crate) fn parse_embedded(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
    ) -> Result<(), Failure> {
        self.parse_embedded_spooling(data, data, metadata, wrapping)
    }

    /// Parses a storage of a compound document embedded in this one, written as a compound
    /// document of its own in `data`, as [`Xhtml::parse_embedded`] does but for what an
    /// extraction that spools its embedded documents copies: nothing, as Tika hands the storage
    /// on as an open container with a stream of no bytes (`TikaInputStream.getFromContainer`).
    pub(crate) fn parse_embedded_container(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
    ) -> Result<(), Failure> {
        self.parse_embedded_spooling(data, &[], metadata, wrapping)
    }

    fn parse_embedded_spooling(
        &mut self,
        data: &[u8],
        spooled: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
    ) -> Result<(), Failure> {
        within_budget()?;
        if !embedded::is_parsed(metadata) || self.embedding.spool(spooled, metadata)? {
            return Ok(());
        }
        if wrapping == Wrapping::PackageEntry {
            self.start_element_with("div", &[("class", PACKAGE_ENTRY)])?;
            if let Some(name) = embedded::heading(metadata) {
                self.start_element("h1")?;
                self.characters_chunk(name)?;
                self.end_element("h1")?;
            }
        }
        let parsed = isolate()
            .and_then(|isolate| {
                tika::parse_embedded(
                    isolate,
                    data,
                    &metadata.pairs(),
                    &self.embedding.context().configs().with_password(None),
                )
            })
            .map_err(Failure::Extraction)?;
        if let EmbeddedOutcome::Parsed { metadata, .. } = &parsed.outcome {
            self.embedding.absorb_parsers(metadata);
        }
        self.write_embedded(embedded::decode(&parsed.events)?, wrapping, parsed.outcome)
    }

    /// Has `write` write content as a parser that this document's parser calls itself and
    /// hands its handler on to through `EmbeddedContentHandler(BodyContentHandler(xhtml))`, the
    /// other parser writing through an XHTML handler of its own: its body goes into this one as
    /// it is written, its metadata is this document's, and the line breaks after block elements
    /// and the tabs before indented ones, which each of the two handlers writes, are written
    /// twice.
    pub(crate) fn write_nested<T>(&mut self, write: impl FnOnce(&mut Xhtml) -> T) -> T {
        self.layers += 1;
        let written = write(self);
        self.layers -= 1;
        written
    }

    /// Parses a storage of a compound document embedded in this one with a native parser, as
    /// [`Xhtml::parse_embedded_container`] parses it with Tika: `write`, which `parser` runs,
    /// writes the document from `metadata`, and its body goes into this one.
    pub(crate) fn parse_container_natively(
        &mut self,
        metadata: &TikaMetadata,
        wrapping: Wrapping,
        parser: &str,
        write: impl FnOnce(&mut Xhtml) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        within_budget()?;
        if !embedded::is_parsed(metadata) || self.embedding.spool(&[], metadata)? {
            return Ok(());
        }
        if wrapping == Wrapping::PackageEntry {
            self.start_element_with("div", &[("class", PACKAGE_ENTRY)])?;
            if let Some(name) = embedded::heading(metadata) {
                self.start_element("h1")?;
                self.characters_chunk(name)?;
                self.end_element("h1")?;
            }
        }
        let mut recorder = embedded::Recorder::default();
        let (mut parsed, record, result) = {
            let mut body = Body::new(&mut recorder);
            Xhtml::write(&mut body, metadata.clone(), self.context(), write)
        };
        record.finish(&mut parsed, parser);
        let parsed = parsed.to_map();
        self.embedding.absorb_parsers(&parsed);
        let outcome = match result {
            Ok(()) => EmbeddedOutcome::Parsed {
                metadata: parsed,
                recorded_exception: None,
            },
            Err(Failure::Extraction(error)) => EmbeddedOutcome::Failed(error),
            Err(failure) => return Err(failure),
        };
        self.write_embedded(recorder.into_events(), wrapping, outcome)
    }

    /// Writes the body of an embedded document, then takes in what its parse recorded and ends
    /// its `package-entry` div; a parse that failed, or recorded failing, has the elements it
    /// left open closed first.
    fn write_embedded(
        &mut self,
        events: Vec<Event>,
        wrapping: Wrapping,
        outcome: EmbeddedOutcome,
    ) -> Result<(), Failure> {
        let mut open = Vec::new();
        if let Err(stop) = self.replay(events, &mut open) {
            if wrapping == Wrapping::PackageEntry {
                self.end_open(&mut open)?;
                self.end_element("div")?;
            }
            return Err(stop.into());
        }
        let (clean, failure) = match outcome {
            EmbeddedOutcome::Parsed {
                metadata,
                recorded_exception,
            } => {
                let clean = recorded_exception.is_none();
                self.embedding
                    .absorb_failures(&metadata, recorded_exception);
                (clean, None)
            }
            EmbeddedOutcome::Failed(error) => (false, Some(error)),
        };
        if wrapping == Wrapping::PackageEntry {
            if !clean {
                self.end_open(&mut open)?;
            }
            self.end_element("div")?;
        }
        failure.map_or(Ok(()), |error| Err(Failure::Extraction(error)))
    }

    /// Writes the events of an embedded document, keeping the names of the elements left open
    /// in `open`, until the output stops taking them.
    fn replay(&mut self, events: Vec<Event>, open: &mut Vec<String>) -> Result<(), Stop> {
        for event in events {
            match event {
                Event::Start { name, attributes } => {
                    let borrowed: Vec<(&str, &str)> = attributes
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str()))
                        .collect();
                    self.start_element_with(&name, &borrowed)?;
                    open.push(name);
                }
                Event::End(name) => {
                    open.pop();
                    self.end_element(&name)?;
                }
                Event::Characters(text) => self.characters_chunk(&text)?,
                Event::IgnorableWhitespace(text) => self.ignorable_whitespace(&text)?,
            }
        }
        Ok(())
    }

    /// `XHTMLBalancingHandler.drainOpenElements`: ends the elements an aborted embedded parse
    /// left open, innermost first.
    fn end_open(&mut self, open: &mut Vec<String>) -> Result<(), Stop> {
        while let Some(name) = open.pop() {
            self.end_element(&name)?;
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
