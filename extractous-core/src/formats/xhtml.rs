//! Tika's `XHTMLContentHandler`, with the `SafeContentHandler` it extends: what a parser calls to
//! write its document as XHTML. It opens `html`, writes the `head` from the metadata before the
//! first body content, indents table cells and list items with a tab, ends block elements with a
//! line break and replaces characters XML cannot hold with U+FFFD.

use crate::config::ParserConfigs;
use crate::formats::embedded::{self, EmbeddedRecord, Embedding, Event, Wrapping, PACKAGE_ENTRY};
use crate::formats::metadata::TikaMetadata;
use crate::formats::Failure;
use crate::isolate::isolate;
use crate::tika::{self, EmbeddedOutcome};

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
}

impl<'h> Xhtml<'h> {
    /// Writes a document to `handler`: starts it, has `body` write its content, and ends it when
    /// `body` succeeds. A failed document stays unfinished, as a Tika parser that throws leaves
    /// it. Documents embedded in it are parsed under `configs`. Returns the metadata as `body`
    /// left it, and what its embedded documents leave for it, which
    /// [`EmbeddedRecord::finish`] adds as Tika's `CompositeParser` does once a parser has returned.
    pub(crate) fn write<E: From<Stop>>(
        handler: &'h mut dyn ContentHandler,
        metadata: TikaMetadata,
        configs: &'h ParserConfigs,
        body: impl FnOnce(&mut Xhtml) -> Result<(), E>,
    ) -> (TikaMetadata, EmbeddedRecord, Result<(), E>) {
        let mut xhtml = Self {
            handler,
            metadata,
            head: Head::NotStarted,
            ended: false,
            embedding: Embedding::new(configs),
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
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        self.handler.start_prefix_mapping(prefix, uri)
    }

    /// The metadata, which the head shows as it stands when the first body content is written.
    pub(crate) fn metadata_mut(&mut self) -> &mut TikaMetadata {
        &mut self.metadata
    }

    pub(crate) fn start_element(&mut self, name: &str) -> Result<(), Stop> {
        self.start_element_with(name, &[])
    }

    pub(crate) fn start_element_with(
        &mut self,
        name: &str,
        attributes: &[(&str, &str)],
    ) -> Result<(), Stop> {
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
    fn characters_chunk(&mut self, text: &str) -> Result<(), Stop> {
        let frameset = self.frameset();
        self.end_head(frameset)?;
        self.safe_text(text, Text::Characters)
    }

    /// Writes Java characters, as Tika's `characters(char[], int, int)`: a surrogate without its
    /// pair is invalid like the characters XML cannot hold, and each becomes U+FFFD.
    pub(crate) fn characters_utf16(&mut self, units: &[u16]) -> Result<(), Stop> {
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
        self.safe_text(text, Text::IgnorableWhitespace)
    }

    /// Whether `name` is a block element a line break ends (`XHTMLContentHandler.ENDLINE`).
    pub(crate) fn is_endline(name: &str) -> bool {
        ENDLINE.contains(&name)
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

    /// Parses a document embedded in this one, given `data` and the `metadata` the parser gives
    /// it, as Tika's `ParsingEmbeddedDocumentExtractor` does when a parser hands it this
    /// `XHTMLContentHandler`: unless it is a thumbnail, its body is written here, in a
    /// `package-entry` div headed by its name when `wrapping` asks for one, with the elements a
    /// failed parse left open closed before the div ends. Tika records the failures it can in
    /// this document's metadata; any other fails this document, after the content written
    /// before it.
    pub(crate) fn parse_embedded(
        &mut self,
        data: &[u8],
        metadata: &TikaMetadata,
        wrapping: Wrapping,
    ) -> Result<(), Failure> {
        if !embedded::is_parsed(metadata) {
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
                    self.embedding.configs(),
                    None,
                )
            })
            .map_err(Failure::Extraction)?;
        if let EmbeddedOutcome::Parsed { metadata, .. } = &parsed.outcome {
            self.embedding.absorb_parsers(metadata);
        }
        let mut open = Vec::new();
        for event in embedded::decode(&parsed.events)? {
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
        let (clean, failure) = match parsed.outcome {
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
                while let Some(name) = open.pop() {
                    self.end_element(&name)?;
                }
            }
            self.end_element("div")?;
        }
        failure.map_or(Ok(()), |error| Err(Failure::Extraction(error)))
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
