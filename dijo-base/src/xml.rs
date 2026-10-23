//! A namespace-aware, non-validating XML parser that fails where the JDK's built-in Xerces, as
//! Tika configures it (no external DTD, no external entities), raises a fatal error, and reads
//! the bytes as Xerces reads them: the byte order mark and first bytes pick the encoding, the
//! declaration switches it for the bytes after it, and a malformed UTF-8, UTF-16 or UCS-4
//! sequence fails the parse when the scanner reaches it, as Xerces' own readers do. It reports
//! start and end tags and the text inside elements; `formats::xml` gives SAX's view of them to
//! the native parsers.
//!
//! What it does not reproduce: the IANA-to-Java encoding table of Xerces (a declared encoding is
//! looked up as Java's `Charset.forName` looks it up), the replacement text of parameter
//! entities (a reference to one is noted, not expanded), and general entities whose replacement
//! text holds markup.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::input::{Pages, Paging};
use crate::text::charset::{Charset, Pieces};

const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The entities XML predefines, by name, and the characters they stand for.
const PREDEFINED: [(&str, char); 5] = [
    ("lt", '<'),
    ("gt", '>'),
    ("amp", '&'),
    ("apos", '\''),
    ("quot", '"'),
];

/// Why a parse stopped short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlError {
    /// A byte sequence the encoding's reader rejects: Java's `CharConversionException`.
    CharConversion,
    /// A fatal well-formedness error, such as a `SAXParseException`.
    Malformed(String),
    /// More entity expansions than [`ENTITY_EXPANSION_LIMIT`]: the JDK's `JAXP00010001`.
    EntityExpansionLimit,
}

/// The entity expansions Tika's `XMLReaderUtils` lets Xerces make in one document, which stops
/// a billion-laughs bomb.
pub(crate) const ENTITY_EXPANSION_LIMIT: usize = 20;

type Result<T> = std::result::Result<T, XmlError>;

fn malformed<T>(what: impl Into<String>) -> Result<T> {
    Err(XmlError::Malformed(what.into()))
}

/// The namespace of `xmlns` attributes, which SAX leaves out of an element's attributes.
pub const XMLNS: &str = XMLNS_NAMESPACE;

/// An attribute of a start tag, its namespace resolved: the empty string for none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attribute {
    pub qualified_name: String,
    pub namespace: String,
    pub local_name: String,
    pub value: String,
}

/// An element's name, its namespace resolved: the empty string for none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    qualified: QName,
    namespace: Rc<str>,
}

impl Name {
    pub fn qualified_name(&self) -> &str {
        &self.qualified.name
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn local_name(&self) -> &str {
        self.qualified.local()
    }
}

/// A qualified name: a local name after an optional prefix and colon.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QName {
    name: String,
    /// Where the local name starts: after the colon, or at the start.
    local_start: usize,
}

impl QName {
    fn prefix(&self) -> Option<&str> {
        (self.local_start > 0).then(|| &self.name[..self.local_start - 1])
    }

    fn local(&self) -> &str {
        &self.name[self.local_start..]
    }
}

/// Where text in an element comes from, which decides how Xerces hands it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// A run of character data between markup and references, its line ends normalized.
    Data,
    /// The content of a CDATA section.
    CData,
    /// What a character or entity reference stands for.
    Reference,
}

/// What the parser reports.
pub enum Event<'a> {
    /// A document type declaration, once the parser has read it.
    DocumentType,
    Start(&'a Name, &'a [Attribute]),
    End(&'a Name),
    /// Text inside an element, in the pieces of markup and references it comes in. A CDATA
    /// section is reported even when it is empty, as SAX's `startCDATA` and `endCDATA` report
    /// it; other text only when it is not. A long run of character data in a document that is
    /// decoded may come in several parts, each ending where a piece of it as SAX reports it
    /// ends.
    Text(&'a str, TextKind),
    /// A comment outside the document type declaration, its text between `<!--` and `-->`.
    Comment(&'a str),
    /// A processing instruction outside the document type declaration other than the XML
    /// declaration: its target, and its data from the first character after the spaces that
    /// follow the target.
    ProcessingInstruction(&'a str, &'a str),
}

/// Whether the handler wants the parse to go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// How a parse reads a document type declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentTypes {
    /// As Xerces' SAX parser reads one: the entities and attribute defaults of its internal
    /// subset apply, and an external subset or a parameter entity reference may declare an
    /// entity the document uses.
    Processed,
    /// As the JDK's StAX reader reads one without DTD support (`SUPPORT_DTD` off): its internal
    /// subset is skipped up to the first `]`, and nothing of it applies.
    Skipped,
}

/// The order in which a parse reports the attributes of a start tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttributeOrder {
    /// As they are written, defaults after them.
    Written,
    /// The `xmlns` attributes first and then the others, each in the written order, as SAX
    /// reports them apart.
    DeclarationsFirst,
}

/// Parses `bytes` as one XML document, handing each start and end tag to `handler` until it
/// asks to stop. An error is what Xerces would throw before the handler stopped it.
pub fn parse(bytes: &[u8], handler: &mut dyn FnMut(Event<'_>) -> Flow) -> Result<()> {
    parse_pages(bytes, Pages::IN_MEMORY, handler)
}

/// [`parse`] of `bytes` whose `pages` are released as the parse reads on: a mapped file that a
/// parser of bounded memory reads.
pub fn parse_pages(
    bytes: &[u8],
    pages: Pages<'_>,
    handler: &mut dyn FnMut(Event<'_>) -> Flow,
) -> Result<()> {
    parse_paged(
        bytes,
        &Paging::new(pages),
        DocumentTypes::Processed,
        AttributeOrder::Written,
        handler,
    )
}

/// Whether `bytes` are read as UTF-8 throughout, so that the ASCII characters of the document
/// stand in them as their ASCII bytes: their first bytes imply UTF-8, and the XML declaration
/// they start with, if any, names no other encoding.
pub fn is_read_as_utf8(bytes: &[u8]) -> bool {
    let Some((detected, _, start)) = detect_encoding(bytes) else {
        return false;
    };
    if detected != "UTF-8" {
        return false;
    }
    let Some(declaration) = bytes[start..].strip_prefix(b"<?xml") else {
        return true;
    };
    let Some(end) = memchr::memmem::find(declaration, b"?>") else {
        return false;
    };
    let declaration = &declaration[..end];
    let Some(at) = memchr::memmem::find(declaration, b"encoding") else {
        return true;
    };
    let Some(value) = declaration[at + b"encoding".len()..]
        .trim_ascii_start()
        .strip_prefix(b"=")
        .map(<[u8]>::trim_ascii_start)
    else {
        return false;
    };
    match value.split_first() {
        Some((&quote, value)) if quote == b'"' || quote == b'\'' => value
            .split(|&byte| byte == quote)
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(b"UTF-8")),
        _ => false,
    }
}

/// [`parse`] of `bytes` whose pages `paging` releases as their characters are first checked,
/// reporting the attributes of each start tag in `attribute_order`.
pub(crate) fn parse_paged(
    bytes: &[u8],
    paging: &Paging,
    document_types: DocumentTypes,
    attribute_order: AttributeOrder,
    handler: &mut dyn FnMut(Event<'_>) -> Flow,
) -> Result<()> {
    let Some((detected, reader, start)) = detect_encoding(bytes) else {
        return malformed("UCS-4 in an unusual byte order is unsupported");
    };
    let mut parser = Parser {
        text: Text::new(bytes, detected, reader, start, paging),
        position: 0,
        mark: 0,
        document_types,
        attribute_order,
        entities: HashMap::new(),
        expansions: Cell::new(0),
        defaults: HashMap::new(),
        external_subset: false,
        parameter_references: false,
        standalone: false,
        attributes: Vec::new(),
        spare_names: Vec::new(),
    };
    match parser.document(handler) {
        Ok(()) | Err(Stop::Handler) => Ok(()),
        Err(Stop::Error(error)) => Err(error),
    }
}

/// How a parse ends early: an error, or the handler asking.
enum Stop {
    Error(XmlError),
    Handler,
}

fn fatal<T>(what: impl Into<String>) -> std::result::Result<T, Stop> {
    Err(Stop::Error(XmlError::Malformed(what.into())))
}

impl From<XmlError> for Stop {
    fn from(error: XmlError) -> Self {
        Stop::Error(error)
    }
}

/// The encodings Xerces reads with its own readers, and the Java decoders it reads the rest
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reader {
    Utf8,
    Utf16 { big_endian: bool },
    Ucs4 { big_endian: bool },
    Java(Charset),
}

/// How many bytes of the document a reader decodes into the window at a time.
const CHUNK: usize = 64 << 10;

/// The document's characters, decoded as Xerces decodes them, line ends normalized, up to the
/// first character its reader cannot deliver, read a chunk at a time into a window that drops
/// the characters the parser no longer needs as it moves on. The window of a UTF-8 document lies
/// in its bytes for as long as no carriage return needs normalizing; any other holds the
/// characters decoded, as UTF-8. The byte offsets the switch to a declared encoding needs are
/// decoded again for the few characters of the declaration.
struct Text<'a> {
    /// The characters from `base` on that are read and not yet dropped: borrowed from the
    /// bytes while they are read where they lie.
    window: Cow<'a, str>,
    /// Where the window starts among all the characters, in bytes of their UTF-8.
    base: usize,
    /// What decodes the characters past the window, until it has decoded all it can.
    source: Option<Source<'a>>,
    /// Whether the characters stop at a malformed sequence rather than at the end, once the
    /// source has decoded all it can.
    malformed: bool,
    paging: &'a Paging<'a>,
    bytes: &'a [u8],
    start: usize,
    /// The reader the characters are decoded with.
    reader: Reader,
    /// The encoding name Xerces records for the document before its declaration.
    detected: &'static str,
}

/// A reader decoding the characters past the window, and the state of their line ends.
struct Source<'a> {
    decoder: Decoder<'a>,
    line_ends: LineEnds,
}

impl<'a> Source<'a> {
    fn new(reader: Reader, bytes: &'a [u8], start: usize) -> Self {
        Self {
            decoder: Decoder::new(reader, bytes, start),
            line_ends: LineEnds::default(),
        }
    }

    /// Decodes onto `window` until it grows or the reader has delivered all it can: whether it
    /// has.
    fn decode_onto(&mut self, window: &mut String) -> bool {
        let length = window.len();
        loop {
            if !self.decoder.decode_into(&mut self.line_ends, window) {
                return true;
            }
            if window.len() > length {
                return false;
            }
        }
    }
}

impl<'a> Text<'a> {
    fn new(
        bytes: &'a [u8],
        detected: &'static str,
        reader: Reader,
        start: usize,
        paging: &'a Paging<'a>,
    ) -> Text<'a> {
        let window = match reader {
            Reader::Utf8 => Cow::Borrowed(""),
            _ => Cow::Owned(String::new()),
        };
        Text {
            window,
            base: 0,
            source: Some(Source::new(reader, bytes, start)),
            malformed: false,
            paging,
            bytes,
            start,
            reader,
            detected,
        }
    }

    /// Where the decoded characters end.
    fn end(&self) -> usize {
        self.base + self.window.len()
    }

    /// Whether all the characters the reader delivers are decoded.
    fn complete(&self) -> bool {
        self.source.is_none()
    }

    /// The characters from `at` to the window's end.
    fn from(&self, at: usize) -> &str {
        &self.window[at - self.base..]
    }

    /// The characters of `range`, which the window holds.
    fn slice(&self, range: std::ops::Range<usize>) -> &str {
        &self.window[range.start - self.base..range.end - self.base]
    }

    /// Drops the characters before `keep` once they are the larger part of the window.
    fn drop_before(&mut self, keep: usize) {
        let dropped = keep - self.base;
        if dropped <= self.window.len() / 2 {
            return;
        }
        match &mut self.window {
            Cow::Borrowed(window) => *window = &window[dropped..],
            Cow::Owned(window) => {
                window.drain(..dropped);
            }
        }
        self.base = keep;
    }

    /// Reads more characters into the window, first dropping those before `keep` once they are
    /// the larger part of it: whether it read any, or found that the reader delivers no more,
    /// which a complete text never does again. A window that lies in the bytes grows over the
    /// next chunk of them, until a carriage return in it makes the window a decoded one.
    fn fill(&mut self, keep: usize) -> bool {
        if self.complete() {
            return false;
        }
        self.drop_before(keep);
        let at = self.start.min(self.bytes.len()) + self.base;
        let Some(source) = &mut self.source else {
            unreachable!("an incomplete text has a source")
        };
        let read = source.decoder.read();
        let finished = match &mut self.window {
            Cow::Borrowed(window) => match source.decoder.utf8_run() {
                None => true,
                Some(run) if memchr::memchr(b'\r', run.as_bytes()).is_none() => {
                    let end = at + window.len() + run.len();
                    *window = unsafe { std::str::from_utf8_unchecked(&self.bytes[at..end]) };
                    source.decoder.utf8_advance(run.len());
                    false
                }
                Some(_) => {
                    let mut decoded = window.to_owned();
                    let finished = source.decode_onto(&mut decoded);
                    self.window = Cow::Owned(decoded);
                    finished
                }
            },
            Cow::Owned(window) => source.decode_onto(window),
        };
        self.paging.read(source.decoder.read() - read);
        if finished {
            self.malformed = source.decoder.malformed;
            self.source = None;
        }
        true
    }

    /// Switches to the declared encoding at the end of the declaration: the characters of the
    /// declaration stay, and a new reader decodes the bytes after them. `declaration_end`
    /// counts the characters up to the end of the declaration, which the window still holds
    /// from the start. Xerces switches where its first reader stopped reading bytes, so that
    /// what it read in chunks past a short declaration stays decoded in the encoding it
    /// detected, and what a malformed sequence left unread is lost.
    fn switch(&mut self, declared: &str, declaration_end: usize) -> Result<()> {
        if declared == self.detected {
            return Ok(());
        }
        let upper = declared.to_ascii_uppercase();
        if self.detected.starts_with("UTF-16") && upper == "UTF-16" {
            return Ok(());
        }
        let reader = match upper.as_str() {
            "UTF-8" => Reader::Utf8,
            "UTF-16BE" => Reader::Utf16 { big_endian: true },
            "UTF-16LE" => Reader::Utf16 { big_endian: false },
            "ISO-10646-UCS-4" | "ISO-10646-UCS-2" => {
                return malformed(format!("the byte order of {declared} is unknown"))
            }
            _ => match Charset::for_name(declared) {
                Some(charset) if charset.decode(b"").is_some() => Reader::Java(charset),
                _ => return malformed(format!("the encoding {declared} is unsupported")),
            },
        };
        let mut resume = self.start;
        let mut decoded = 0;
        decode_normalized(self.reader, self.bytes, self.start, |_, end| {
            resume = end;
            decoded += 1;
            decoded < declaration_end
        });
        let kept_bytes = self
            .window
            .char_indices()
            .nth(declaration_end)
            .map_or(self.window.len(), |(index, _)| index);
        self.window = Cow::Owned(self.window[..kept_bytes].to_owned());
        self.reader = reader;
        self.source = Some(Source::new(reader, self.bytes, resume));
        self.malformed = false;
        Ok(())
    }
}

/// Makes each `\r\n` and lone `\r` a `\n` as decoded text comes in, a character or a run at a
/// time.
#[derive(Default)]
struct LineEnds {
    /// Whether the last character that came in was a carriage return.
    after_return: bool,
}

impl LineEnds {
    fn push(&mut self, text: &mut String, c: char) {
        match c {
            '\n' if self.after_return => {}
            '\r' => text.push('\n'),
            c => text.push(c),
        }
        self.after_return = c == '\r';
    }

    fn push_str(&mut self, text: &mut String, run: &str) {
        let mut rest = run;
        if self.after_return && !rest.is_empty() {
            self.after_return = false;
            rest = rest.strip_prefix('\n').unwrap_or(rest);
        }
        while let Some(at) = memchr::memchr(b'\r', rest.as_bytes()) {
            text.push_str(&rest[..at]);
            text.push('\n');
            rest = &rest[at + 1..];
            if rest.is_empty() {
                self.after_return = true;
                return;
            }
            rest = rest.strip_prefix('\n').unwrap_or(rest);
        }
        text.push_str(rest);
    }
}

/// `XMLEntityManager.getEncodingInfo`: the encoding the first four bytes imply, its reader, and
/// how many bytes of byte order mark to skip.
fn detect_encoding(bytes: &[u8]) -> Option<(&'static str, Reader, usize)> {
    Some(match bytes {
        [0xFE, 0xFF, ..] => ("UTF-16BE", Reader::Utf16 { big_endian: true }, 2),
        [0xFF, 0xFE, ..] => ("UTF-16LE", Reader::Utf16 { big_endian: false }, 2),
        [0xEF, 0xBB, 0xBF, ..] => ("UTF-8", Reader::Utf8, 3),
        [0x00, 0x00, 0x00, 0x3C, ..] => ("UCS-4", Reader::Ucs4 { big_endian: true }, 0),
        [0x3C, 0x00, 0x00, 0x00, ..] => ("UCS-4", Reader::Ucs4 { big_endian: false }, 0),
        [0x00, 0x00, 0x3C, 0x00, ..] | [0x00, 0x3C, 0x00, 0x00, ..] => return None,
        [0x00, 0x3C, 0x00, 0x3F, ..] => ("UTF-16BE", Reader::Utf16 { big_endian: true }, 0),
        [0x3C, 0x00, 0x3F, 0x00, ..] => ("UTF-16LE", Reader::Utf16 { big_endian: false }, 0),
        [0x4C, 0x6F, 0xA7, 0x94, ..] => ("CP037", Reader::Java(Charset::named("IBM037")), 0),
        _ => ("UTF-8", Reader::Utf8, 0),
    })
}

/// Decodes from `start` as [`decode`] does, normalizing line ends: `\r\n` and a lone `\r` both
/// become `\n`, which ends where the last of them ends. Returns whether a malformed sequence
/// stopped the reader.
fn decode_normalized(
    reader: Reader,
    bytes: &[u8],
    start: usize,
    mut sink: impl FnMut(char, usize) -> bool,
) -> bool {
    let mut carriage_return: Option<usize> = None;
    let mut going = true;
    let malformed = decode(reader, bytes, start, |c, end| {
        if let Some(return_end) = carriage_return.take() {
            if c == '\n' {
                going = sink('\n', end);
                return going;
            }
            going = sink('\n', return_end);
            if !going {
                return false;
            }
        }
        if c == '\r' {
            carriage_return = Some(end);
            return true;
        }
        going = sink(c, end);
        going
    });
    if let (true, Some(return_end)) = (going, carriage_return) {
        sink('\n', return_end);
    }
    malformed
}

/// Decodes from `start` until the reader fails, the bytes end or `sink` asks to stop, handing
/// `sink` each character with the byte offset after it. Returns whether a malformed sequence
/// stopped the reader.
fn decode(
    reader: Reader,
    bytes: &[u8],
    start: usize,
    mut sink: impl FnMut(char, usize) -> bool,
) -> bool {
    let mut decoder = Decoder::new(reader, bytes, start);
    while let Some((c, end)) = decoder.next_char() {
        if !sink(c, end) {
            return false;
        }
    }
    decoder.malformed
}

/// A reader decoding the bytes from where it starts, a character or a chunk at a time. UTF-8,
/// UTF-16 and UCS-4 deliver every character before the first malformed sequence and stop there:
/// an unpaired surrogate, an odd final byte of UTF-16, a code point outside Unicode or a partial
/// final unit of UCS-4. Xerces instead makes unpaired surrogates and code points outside Unicode
/// U+FFFE, drops a partial final unit of UCS-4, and fails the whole read an odd final byte of
/// UTF-16 falls in, dropping the characters of that read with it (#186). Any other charset
/// decodes with [`Charset::decode`], each malformed sequence one U+FFFD, and gives each
/// character the end of the bytes as its end.
struct Decoder<'a> {
    bytes: &'a [u8],
    reading: Reading<'a>,
    /// Whether the reader has delivered all it can.
    finished: bool,
    /// Whether a malformed sequence stopped the reader, once it has finished.
    malformed: bool,
}

/// Where a reader stands in the bytes.
enum Reading<'a> {
    /// At `at`, the bytes up to `checked` valid, and `malformed_after` them a malformed sequence.
    Utf8 {
        at: usize,
        checked: usize,
        malformed_after: bool,
    },
    /// At the `index`th of the `units` whole units from `start`.
    Utf16 {
        big_endian: bool,
        start: usize,
        index: usize,
        units: usize,
        odd: bool,
    },
    /// At the `index`th of the `units` whole units from `start`, a partial unit after them if
    /// `partial`.
    Ucs4 {
        big_endian: bool,
        start: usize,
        index: usize,
        units: usize,
        partial: bool,
    },
    /// At `at` in the `piece` of text the decoder of `pieces` gave last.
    Java {
        pieces: Pieces<'a>,
        piece: String,
        at: usize,
    },
}

impl<'a> Decoder<'a> {
    fn new(reader: Reader, bytes: &'a [u8], start: usize) -> Self {
        let start = start.min(bytes.len());
        let length = bytes.len() - start;
        let reading = match reader {
            Reader::Utf8 => Reading::Utf8 {
                at: start,
                checked: start,
                malformed_after: false,
            },
            Reader::Utf16 { big_endian } => Reading::Utf16 {
                big_endian,
                start,
                index: 0,
                units: length / 2,
                odd: length % 2 == 1,
            },
            Reader::Ucs4 { big_endian } => Reading::Ucs4 {
                big_endian,
                start,
                index: 0,
                units: length / 4,
                partial: !length.is_multiple_of(4),
            },
            Reader::Java(charset) => Reading::Java {
                pieces: charset
                    .pieces(&bytes[start..], CHUNK)
                    .expect("the reader is only chosen for a charset that decodes"),
                piece: String::new(),
                at: 0,
            },
        };
        Self {
            bytes,
            reading,
            finished: false,
            malformed: false,
        }
    }

    fn finish(&mut self, malformed: bool) {
        self.finished = true;
        self.malformed = malformed;
    }

    /// How far into the bytes the reader has read.
    fn read(&self) -> usize {
        match &self.reading {
            Reading::Utf8 { checked, .. } => *checked,
            Reading::Utf16 { start, index, .. } => start + 2 * index,
            Reading::Ucs4 { start, index, .. } => start + 4 * index,
            Reading::Java { pieces, .. } => self.bytes.len() - pieces.unread(),
        }
    }

    /// The valid UTF-8 from where a UTF-8 reader stands, a chunk more checked once it has
    /// delivered all it checked: `None` once it has finished.
    fn utf8_run(&mut self) -> Option<&'a str> {
        let bytes = self.bytes;
        let Reading::Utf8 {
            at,
            checked,
            malformed_after,
        } = &mut self.reading
        else {
            unreachable!("a UTF-8 run is read by a UTF-8 reader")
        };
        if *at == *checked && !*malformed_after && *at < bytes.len() {
            let end = bytes.len().min(*at + CHUNK);
            (*checked, *malformed_after) = match std::str::from_utf8(&bytes[*at..end]) {
                Ok(_) => (end, false),
                Err(error) if error.error_len().is_none() && end < bytes.len() => {
                    (*at + error.valid_up_to(), false)
                }
                Err(error) => (*at + error.valid_up_to(), true),
            };
        }
        if *at == *checked {
            let malformed = *malformed_after;
            self.finish(malformed);
            return None;
        }
        Some(unsafe { std::str::from_utf8_unchecked(&bytes[*at..*checked]) })
    }

    /// Marks `length` bytes of a UTF-8 run delivered.
    fn utf8_advance(&mut self, length: usize) {
        if let Reading::Utf8 { at, .. } = &mut self.reading {
            *at += length;
        }
    }

    /// The next character and the byte offset after it, or `None` once the reader has finished.
    fn next_char(&mut self) -> Option<(char, usize)> {
        if self.finished {
            return None;
        }
        let bytes = self.bytes;
        match &mut self.reading {
            Reading::Utf8 { .. } => {
                let c = self.utf8_run()?.chars().next()?;
                self.utf8_advance(c.len_utf8());
                Some((c, self.utf8_at()))
            }
            Reading::Utf16 {
                big_endian,
                start,
                index,
                units,
                odd,
            } => {
                let (odd, units) = (*odd, *units);
                if *index >= units {
                    self.finish(odd);
                    return None;
                }
                let unit = |i: usize| {
                    let pair = [bytes[*start + 2 * i], bytes[*start + 2 * i + 1]];
                    if *big_endian {
                        u16::from_be_bytes(pair)
                    } else {
                        u16::from_le_bytes(pair)
                    }
                };
                let first = unit(*index);
                let decoded = match first {
                    0xD800..=0xDBFF if *index + 1 < units => {
                        let second = unit(*index + 1);
                        (0xDC00..=0xDFFF).contains(&second).then(|| {
                            let code = 0x10000
                                + ((u32::from(first) - 0xD800) << 10)
                                + (u32::from(second) - 0xDC00);
                            (
                                char::from_u32(code).expect("a surrogate pair is a character"),
                                2,
                            )
                        })
                    }
                    _ => char::from_u32(u32::from(first)).map(|c| (c, 1)),
                };
                let Some((c, used)) = decoded else {
                    self.finish(true);
                    return None;
                };
                *index += used;
                Some((c, *start + 2 * *index))
            }
            Reading::Ucs4 {
                big_endian,
                start,
                index,
                units,
                partial,
            } => {
                if *index >= *units {
                    let partial = *partial;
                    self.finish(partial);
                    return None;
                }
                let at = *start + 4 * *index;
                let quad = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
                let code = if *big_endian {
                    u32::from_be_bytes(quad)
                } else {
                    u32::from_le_bytes(quad)
                };
                let Some(c) = char::from_u32(code) else {
                    self.finish(true);
                    return None;
                };
                *index += 1;
                Some((c, at + 4))
            }
            Reading::Java { pieces, piece, at } => loop {
                if let Some(c) = piece[*at..].chars().next() {
                    *at += c.len_utf8();
                    return Some((c, bytes.len()));
                }
                match pieces.next() {
                    Some(next) => {
                        *piece = next;
                        *at = 0;
                    }
                    None => {
                        self.finish(false);
                        return None;
                    }
                }
            },
        }
    }

    /// Where a UTF-8 reader stands.
    fn utf8_at(&self) -> usize {
        match self.reading {
            Reading::Utf8 { at, .. } => at,
            _ => unreachable!("only a UTF-8 reader stands at a UTF-8 offset"),
        }
    }

    /// Decodes a chunk more onto `text`, its line ends normalized by `line_ends`, the chunk
    /// perhaps empty: whether the reader may deliver more.
    fn decode_into(&mut self, line_ends: &mut LineEnds, text: &mut String) -> bool {
        if self.finished {
            return false;
        }
        match &mut self.reading {
            Reading::Utf8 { .. } => {
                if let Some(run) = self.utf8_run() {
                    line_ends.push_str(text, run);
                    self.utf8_advance(run.len());
                }
            }
            Reading::Java { pieces, piece, at } => {
                if *at < piece.len() {
                    line_ends.push_str(text, &piece[*at..]);
                    *at = piece.len();
                } else {
                    match pieces.next() {
                        Some(next) => line_ends.push_str(text, &next),
                        None => self.finish(false),
                    }
                }
            }
            Reading::Utf16 { .. } | Reading::Ucs4 { .. } => {
                let target = text.len() + CHUNK;
                while text.len() < target {
                    match self.next_char() {
                        Some((c, _)) => line_ends.push(text, c),
                        None => break,
                    }
                }
            }
        }
        !self.finished
    }
}

/// Whether `c` is an XML 1.0 `Char`.
fn is_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn is_name_start(c: char) -> bool {
    c == ':' || c == '_' || c.is_ascii_alphabetic() || (!c.is_ascii() && is_unicode_name(c))
}

/// [`is_name_char`] of an ASCII character, told by its byte.
fn is_ascii_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-' | b'.')
}

fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || c == '-'
        || c == '.'
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || (!c.is_ascii() && (c.is_alphanumeric() || is_unicode_name(c)))
}

fn is_unicode_name(c: char) -> bool {
    matches!(c,
        '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}' | '\u{370}'..='\u{37D}'
        | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}'
        | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}'
        | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// A general entity of the internal subset.
enum Entity {
    Internal(String),
    External,
}

/// A default value an `ATTLIST` declaration gives an attribute.
struct Default {
    name: String,
    value: String,
}

struct Parser<'a> {
    text: Text<'a>,
    /// Where the parser stands among the characters, in bytes of their UTF-8.
    position: usize,
    /// Where the characters start that the parser still refers to behind its position, which
    /// the window keeps.
    mark: usize,
    document_types: DocumentTypes,
    attribute_order: AttributeOrder,
    entities: HashMap<String, Entity>,
    /// The entities expanded so far, as Xerces counts them against its limit.
    expansions: Cell<usize>,
    defaults: HashMap<String, Vec<Default>>,
    external_subset: bool,
    parameter_references: bool,
    standalone: bool,
    /// The attributes of the start tag being read, their strings kept for the next one.
    attributes: Vec<Attribute>,
    /// The strings of names whose elements ended, for the next ones.
    spare_names: Vec<String>,
}

/// The namespace scopes of the open elements.
struct Scopes {
    bindings: Vec<(String, Rc<str>)>,
    marks: Vec<usize>,
    /// The namespace of a name without one.
    none: Rc<str>,
}

impl Scopes {
    fn new() -> Scopes {
        Scopes {
            bindings: vec![("xml".to_owned(), Rc::from(XML_NAMESPACE))],
            marks: Vec::new(),
            none: Rc::from(""),
        }
    }

    fn resolve(&self, prefix: &str) -> Option<&Rc<str>> {
        self.bindings
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri)
    }
}

impl Parser<'_> {
    /// The characters from the position on that the window holds.
    fn rest(&self) -> &str {
        self.text.from(self.position)
    }

    /// Decodes more characters into the window, keeping those from the mark or the position on.
    fn fill(&mut self) -> bool {
        self.text.fill(self.mark.min(self.position))
    }

    /// Fills the window until it holds `length` bytes from the position on, or all there are.
    fn buffer(&mut self, length: usize) {
        while self.text.end() - self.position < length && self.fill() {}
    }

    /// The end of the characters: where the document ends, or the error of the sequence the
    /// reader could not deliver.
    fn text_end(&self) -> Result<Option<char>> {
        if self.text.malformed {
            return Err(XmlError::CharConversion);
        }
        Ok(None)
    }

    fn peek_at(&mut self, offset: usize) -> Result<Option<char>> {
        self.buffer(4 * (offset + 1));
        match self.rest().chars().nth(offset) {
            Some(c) => Ok(Some(c)),
            None => self.text_end(),
        }
    }

    fn peek(&mut self) -> Result<Option<char>> {
        self.peek_at(0)
    }

    fn next(&mut self) -> Result<Option<char>> {
        let c = self.peek()?;
        if let Some(c) = c {
            self.position += c.len_utf8();
        }
        Ok(c)
    }

    fn expect_char(&mut self, what: &str) -> Result<char> {
        match self.next()? {
            Some(c) if is_char(c) => Ok(c),
            Some(c) => malformed(format!("invalid character U+{:04X} in {what}", c as u32)),
            None => malformed(format!("the document ends in {what}")),
        }
    }

    /// Whether the text goes on with the ASCII `literal`, read up to the first character that
    /// differs.
    fn starts_with(&mut self, literal: &str) -> Result<bool> {
        debug_assert!(literal.is_ascii());
        self.buffer(literal.len());
        let rest = self.rest().as_bytes();
        let compared = rest.len().min(literal.len());
        if !rest.iter().zip(literal.as_bytes()).all(|(a, b)| a == b) {
            return Ok(false);
        }
        if compared < literal.len() {
            self.text_end()?;
            return Ok(false);
        }
        Ok(true)
    }

    fn skip(&mut self, literal: &str) -> Result<bool> {
        if self.starts_with(literal)? {
            self.position += literal.len();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn require(&mut self, literal: &str, what: &str) -> Result<()> {
        if self.skip(literal)? {
            Ok(())
        } else {
            malformed(format!("{what} expected"))
        }
    }

    fn skip_spaces(&mut self) -> Result<bool> {
        let start = self.position;
        loop {
            let skipped = self
                .rest()
                .bytes()
                .take_while(|&b| is_space(char::from(b)))
                .count();
            self.position += skipped;
            if self.position < self.text.end() || !self.fill() {
                break;
            }
        }
        if self.position == self.text.end() {
            self.text_end()?;
        }
        Ok(self.position > start)
    }

    fn require_spaces(&mut self, what: &str) -> Result<()> {
        if self.skip_spaces()? {
            Ok(())
        } else {
            malformed(format!("white space required {what}"))
        }
    }

    fn name(&mut self, what: &str) -> Result<String> {
        let mut name = String::new();
        self.name_into(what, &mut name)?;
        Ok(name)
    }

    /// Reads a name into `name`, which it replaces.
    fn name_into(&mut self, what: &str, name: &mut String) -> Result<()> {
        match self.peek()? {
            Some(c) if is_name_start(c) => {}
            _ => return malformed(format!("a name expected for {what}")),
        }
        let length = self.name_length();
        name.clear();
        name.push_str(&self.rest()[..length]);
        self.position += length;
        if self.position == self.text.end() {
            self.text_end()?;
        }
        Ok(())
    }

    /// The length in bytes of the run of name characters at the position, which the window
    /// then holds.
    fn name_length(&mut self) -> usize {
        let mut length = 0;
        loop {
            let (run, stopped) = name_run(self.text.from(self.position + length));
            length += run;
            if stopped || !self.fill() {
                return length;
            }
        }
    }

    /// Moves the position to where the run of characters Xerces accepts from it ends in the
    /// window: at the window's end, at a character it rejects, or at an ASCII byte `stops`
    /// picks.
    fn scan_buffered(&mut self, stops: impl Fn(u8) -> bool) {
        self.position += char_run(self.rest(), |byte| {
            !stops(byte) && (byte >= 0x20 || is_space(char::from(byte)))
        });
    }

    /// Moves the position to where the run of characters Xerces accepts from it ends: at the
    /// end, at a character it rejects, or at an ASCII byte `stops` picks.
    fn scan_plain(&mut self, stops: impl Fn(u8) -> bool) {
        loop {
            self.scan_buffered(&stops);
            if self.position < self.text.end() || !self.fill() {
                return;
            }
        }
    }

    /// Reads up to and past the ASCII `terminator`, each character on the way one Xerces
    /// accepts in `what`, as a loop of [`Parser::skip`] and [`Parser::expect_char`] would; the
    /// characters before the terminator, which the window holds until the mark moves on.
    fn until(&mut self, terminator: &str, what: &str) -> Result<std::ops::Range<usize>> {
        let first = terminator.as_bytes()[0];
        let start = self.position;
        self.mark = start;
        loop {
            self.scan_plain(|byte| byte == first);
            let end = self.position;
            if self.skip(terminator)? {
                return Ok(start..end);
            }
            self.expect_char(what)?;
        }
    }

    /// Whether an XML declaration starts the document. Xerces' `skipString` reads all five
    /// characters of `<?xml` before it compares them, so a malformed sequence among them fails
    /// the parse even when the document starts otherwise.
    fn declaration_follows(&mut self) -> Result<bool> {
        self.peek_at(4)?;
        Ok(self.starts_with("<?xml")? && self.peek_at(5)?.is_some_and(is_space))
    }

    fn document(
        &mut self,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<(), Stop> {
        if self.declaration_follows()? {
            self.position += "<?xml".len();
            self.declaration()?;
        }
        let mut seen_doctype = false;
        loop {
            self.mark = self.position;
            self.skip_spaces()?;
            match self.peek()? {
                None => return fatal("the document has no root element"),
                Some('<') => {}
                Some(_) => return fatal("content is not allowed in prolog"),
            }
            if self.skip("<!--")? {
                self.report_comment(handler)?;
            } else if self.skip("<?")? {
                self.report_processing_instruction(handler)?;
            } else if self.skip("<!DOCTYPE")? {
                if seen_doctype {
                    return fatal("a second DOCTYPE");
                }
                seen_doctype = true;
                self.doctype()?;
                if handler(Event::DocumentType) == Flow::Stop {
                    return Err(Stop::Handler);
                }
            } else if self.peek_at(1)?.is_some_and(is_name_start) {
                break;
            } else {
                return fatal("markup not recognized in prolog");
            }
        }
        let mut scopes = Scopes::new();
        self.element(&mut scopes, handler)?;
        loop {
            self.mark = self.position;
            self.skip_spaces()?;
            match self.peek()? {
                None => return Ok(()),
                Some('<') => {}
                Some(_) => return fatal("content after the root element"),
            }
            if self.skip("<!--")? {
                self.report_comment(handler)?;
            } else if self.skip("<?")? {
                self.report_processing_instruction(handler)?;
            } else {
                return fatal("markup after the root element");
            }
        }
    }

    fn declaration(&mut self) -> Result<()> {
        let mut saw_space = self.skip_spaces()?;
        let mut state = 0;
        let mut encoding = None;
        let mut found = false;
        while self.peek()? != Some('?') {
            found = true;
            let name = if self.skip("version")? {
                "version"
            } else if self.skip("encoding")? {
                "encoding"
            } else if self.skip("standalone")? {
                "standalone"
            } else {
                return malformed("a pseudo attribute expected in the XML declaration");
            };
            self.skip_spaces()?;
            self.require("=", "= in the XML declaration")?;
            self.skip_spaces()?;
            let quote = match self.next()? {
                Some(q @ ('"' | '\'')) => q,
                _ => return malformed("a quote expected in the XML declaration"),
            };
            let mut value = String::new();
            loop {
                match self.next()? {
                    Some(c) if c == quote => break,
                    Some(c) if is_char(c) => value.push(c),
                    Some(_) => return malformed("invalid character in the XML declaration"),
                    None => return malformed("the XML declaration is unterminated"),
                }
            }
            if !saw_space {
                return malformed("white space required in the XML declaration");
            }
            match (state, name) {
                (0, "version") => {
                    if value != "1.0" && value != "1.1" {
                        return malformed(format!("XML version {value} unsupported"));
                    }
                    state = 1;
                }
                (1, "encoding") => {
                    encoding = Some(value);
                    state = 2;
                }
                (1 | 2, "standalone") => {
                    if value != "yes" && value != "no" {
                        return malformed("standalone must be yes or no");
                    }
                    self.standalone = value == "yes";
                    state = 3;
                }
                _ => return malformed("the XML declaration's pseudo attributes are out of order"),
            }
            saw_space = self.skip_spaces()?;
        }
        if !found || state == 0 {
            return malformed("the XML declaration has no version");
        }
        self.require("?>", "the end of the XML declaration")?;
        if let Some(encoding) = encoding {
            if !is_encoding_name(&encoding) {
                return malformed(format!("invalid encoding name {encoding}"));
            }
            let declaration_end = self.text.slice(0..self.position).chars().count();
            self.text.switch(&encoding, declaration_end)?;
        }
        Ok(())
    }

    /// Reads a comment after its `<!--`: where its text lies.
    fn comment(&mut self) -> Result<std::ops::Range<usize>> {
        let text = self.until("--", "a comment")?;
        if self.skip(">")? {
            return Ok(text);
        }
        malformed("-- inside a comment")
    }

    /// Reads a processing instruction after its `<?`: its target, and where its data lies.
    fn processing_instruction(&mut self) -> Result<(String, std::ops::Range<usize>)> {
        let target = self.name("a processing instruction")?;
        if target.eq_ignore_ascii_case("xml") {
            return malformed("the processing instruction target xml is reserved");
        }
        if target.contains(':') {
            return malformed("a colon in a processing instruction target");
        }
        if self.skip("?>")? {
            return Ok((target, self.position..self.position));
        }
        self.require_spaces("after a processing instruction target")?;
        let data = self.until("?>", "a processing instruction")?;
        Ok((target, data))
    }

    /// Reads a comment or processing instruction outside the document type declaration, its
    /// start already read, and hands it to `handler`.
    fn report_comment(
        &mut self,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<(), Stop> {
        let text = self.comment()?;
        match handler(Event::Comment(self.text.slice(text))) {
            Flow::Continue => Ok(()),
            Flow::Stop => Err(Stop::Handler),
        }
    }

    /// Reads a processing instruction outside the document type declaration, its `<?` already
    /// read, and hands it to `handler`.
    fn report_processing_instruction(
        &mut self,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<(), Stop> {
        let (target, data) = self.processing_instruction()?;
        match handler(Event::ProcessingInstruction(&target, self.text.slice(data))) {
            Flow::Continue => Ok(()),
            Flow::Stop => Err(Stop::Handler),
        }
    }

    fn quoted(&mut self, what: &str) -> Result<String> {
        let quote = match self.next()? {
            Some(q @ ('"' | '\'')) => q,
            _ => return malformed(format!("a quoted {what} expected")),
        };
        let mut value = String::new();
        loop {
            let c = self.expect_char(what)?;
            if c == quote {
                return Ok(value);
            }
            value.push(c);
        }
    }

    fn external_id(&mut self, system_required: bool) -> Result<bool> {
        if self.skip("SYSTEM")? {
            self.require_spaces("after SYSTEM")?;
            self.quoted("system literal")?;
            return Ok(true);
        }
        if self.skip("PUBLIC")? {
            self.require_spaces("after PUBLIC")?;
            let public = self.quoted("public identifier")?;
            if !public
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " \r\n-'()+,./:=?;!*#@$_%".contains(c))
            {
                return malformed("invalid character in a public identifier");
            }
            let spaced = self.skip_spaces()?;
            if matches!(self.peek()?, Some('"' | '\'')) {
                if !spaced {
                    return malformed("white space required before a system literal");
                }
                self.quoted("system literal")?;
            } else if system_required {
                return malformed("a system literal expected");
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn doctype(&mut self) -> Result<()> {
        self.require_spaces("after DOCTYPE")?;
        self.name("the document type")?;
        let spaced = self.skip_spaces()?;
        if matches!(self.peek()?, Some('S' | 'P')) {
            if !spaced {
                return malformed("white space required before an external identifier");
            }
            let external_subset = self.external_id(true)?;
            self.external_subset =
                external_subset && self.document_types == DocumentTypes::Processed;
            self.skip_spaces()?;
        }
        if self.skip("[")? {
            match self.document_types {
                DocumentTypes::Processed => self.internal_subset()?,
                DocumentTypes::Skipped => self.skip_internal_subset()?,
            }
            self.skip_spaces()?;
        }
        self.require(">", "the end of the DOCTYPE")
    }

    /// Skips an internal subset up to the `]` that ends it, as a reader without DTD support
    /// skips it, reading none of it.
    fn skip_internal_subset(&mut self) -> Result<()> {
        loop {
            self.mark = self.position;
            match self.peek()? {
                None => return malformed("the internal subset does not end"),
                Some(c) => {
                    self.position += c.len_utf8();
                    if c == ']' {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn internal_subset(&mut self) -> Result<()> {
        loop {
            self.mark = self.position;
            self.skip_spaces()?;
            if self.skip("]")? {
                return Ok(());
            }
            if self.skip("%")? {
                self.name("a parameter entity reference")?;
                self.require(";", "; after a parameter entity reference")?;
                self.parameter_references = true;
            } else if self.skip("<!--")? {
                self.comment()?;
            } else if self.skip("<?")? {
                self.processing_instruction()?;
            } else if self.skip("<!ENTITY")? {
                self.entity_declaration()?;
            } else if self.skip("<!ATTLIST")? {
                self.attlist_declaration()?;
            } else if self.skip("<!ELEMENT")? || self.skip("<!NOTATION")? {
                self.require_spaces("in a declaration")?;
                self.name("a declaration")?;
                loop {
                    match self.expect_char("a declaration")? {
                        '>' => break,
                        '"' | '\'' => {
                            self.position -= 1;
                            self.quoted("literal")?;
                        }
                        _ => {}
                    }
                }
            } else {
                return malformed("markup not recognized in the internal subset");
            }
        }
    }

    fn entity_declaration(&mut self) -> Result<()> {
        self.require_spaces("after ENTITY")?;
        let parameter = self.skip("%")?;
        if parameter {
            self.require_spaces("after %")?;
        }
        let name = self.name("an entity")?;
        self.require_spaces("after an entity name")?;
        let entity = if matches!(self.peek()?, Some('"' | '\'')) {
            let value = self.quoted("entity value")?;
            if value.contains('%') {
                self.parameter_references = true;
            }
            Entity::Internal(value)
        } else {
            if !self.external_id(true)? {
                return malformed("an entity value or external identifier expected");
            }
            let spaced = self.skip_spaces()?;
            if self.starts_with("NDATA")? {
                if !spaced || parameter {
                    return malformed("NDATA misplaced");
                }
                self.position += "NDATA".len();
                self.require_spaces("after NDATA")?;
                self.name("a notation")?;
            }
            Entity::External
        };
        self.skip_spaces()?;
        self.require(">", "the end of an entity declaration")?;
        if !parameter {
            self.entities.entry(name).or_insert(entity);
        }
        Ok(())
    }

    fn attlist_declaration(&mut self) -> Result<()> {
        self.require_spaces("after ATTLIST")?;
        let element = self.name("an attribute list")?;
        loop {
            let spaced = self.skip_spaces()?;
            if self.skip(">")? {
                return Ok(());
            }
            if !spaced {
                return malformed("white space required in an attribute list");
            }
            let name = self.name("an attribute definition")?;
            self.require_spaces("after an attribute name")?;
            if self.skip("(")? {
                self.enumeration()?;
            } else {
                let kind = self.name("an attribute type")?;
                match kind.as_str() {
                    "CDATA" | "ID" | "IDREF" | "IDREFS" | "ENTITY" | "ENTITIES" | "NMTOKEN"
                    | "NMTOKENS" => {}
                    "NOTATION" => {
                        self.require_spaces("after NOTATION")?;
                        self.require("(", "a notation list")?;
                        self.enumeration()?;
                    }
                    _ => return malformed(format!("invalid attribute type {kind}")),
                }
            }
            self.require_spaces("before an attribute default")?;
            let value = if self.skip("#REQUIRED")? || self.skip("#IMPLIED")? {
                None
            } else {
                if self.skip("#FIXED")? {
                    self.require_spaces("after #FIXED")?;
                }
                Some(self.attribute_value()?)
            };
            if let Some(value) = value {
                let defaults = self.defaults.entry(element.clone()).or_default();
                if !defaults.iter().any(|d| d.name == name) {
                    defaults.push(Default { name, value });
                }
            }
        }
    }

    fn enumeration(&mut self) -> Result<()> {
        loop {
            self.skip_spaces()?;
            while let Some(c) = self.peek()?.filter(|&c| is_name_char(c)) {
                self.position += c.len_utf8();
            }
            self.skip_spaces()?;
            match self.next()? {
                Some('|') => {}
                Some(')') => return Ok(()),
                _ => return malformed("an enumeration is unterminated"),
            }
        }
    }

    /// An attribute value, its references replaced and its white space normalized.
    fn attribute_value(&mut self) -> Result<String> {
        let mut value = String::new();
        self.attribute_value_into(&mut value)?;
        Ok(value)
    }

    /// Reads an attribute value into `value`, which it replaces.
    fn attribute_value_into(&mut self, value: &mut String) -> Result<()> {
        let quote = match self.next()? {
            Some(q @ ('"' | '\'')) => q,
            _ => return malformed("a quoted attribute value expected"),
        };
        value.clear();
        loop {
            let plain = char_run(self.rest(), |byte| {
                byte >= 0x20 && byte != quote as u8 && byte != b'<' && byte != b'&'
            });
            value.push_str(&self.rest()[..plain]);
            self.position += plain;
            let c = self.expect_char("an attribute value")?;
            match c {
                c if c == quote => return Ok(()),
                '<' => return malformed("< in an attribute value"),
                '&' => {
                    let reference = self.reference()?;
                    self.expand_in_attribute(reference, value, 0)?;
                }
                c if is_space(c) => value.push(' '),
                c => value.push(c),
            }
        }
    }

    fn reference(&mut self) -> Result<Reference> {
        if self.skip("#")? {
            let hex = self.skip("x")?;
            let mut digits = String::new();
            while let Some(c) = self.peek()? {
                if c == ';' {
                    break;
                }
                let valid = if hex {
                    c.is_ascii_hexdigit()
                } else {
                    c.is_ascii_digit()
                };
                if !valid {
                    return malformed("an invalid character reference");
                }
                digits.push(c);
                self.position += 1;
            }
            self.require(";", "; after a character reference")?;
            let code = u32::from_str_radix(&digits, if hex { 16 } else { 10 })
                .ok()
                .and_then(char::from_u32)
                .filter(|&c| is_char(c));
            return match code {
                Some(c) if !digits.is_empty() => Ok(Reference::Char(c)),
                _ => malformed("a character reference to an invalid character"),
            };
        }
        for &(name, c) in &PREDEFINED {
            self.buffer(name.len() + 1);
            let rest = self.rest().as_bytes();
            if rest.starts_with(name.as_bytes()) && rest.get(name.len()) == Some(&b';') {
                self.position += name.len() + 1;
                return Ok(Reference::Char(c));
            }
        }
        let name = self.name("an entity reference")?;
        self.require(";", "; after an entity reference")?;
        Ok(Reference::Entity(name))
    }

    fn predefined(name: &str) -> Option<char> {
        PREDEFINED
            .iter()
            .find(|&&(predefined, _)| predefined == name)
            .map(|&(_, c)| c)
    }

    /// Whether an undeclared entity is a fatal error: only when every declaration it could
    /// have was read.
    fn undeclared(&self, name: &str) -> Result<()> {
        if (self.external_subset || self.parameter_references) && !self.standalone {
            Ok(())
        } else {
            malformed(format!("the entity {name} is not declared"))
        }
    }

    fn expand_in_attribute(
        &self,
        reference: Reference,
        value: &mut String,
        depth: usize,
    ) -> Result<()> {
        let name = match reference {
            Reference::Char(c) => {
                value.push(c);
                return Ok(());
            }
            Reference::Entity(name) => name,
        };
        if let Some(c) = Self::predefined(&name) {
            value.push(c);
            return Ok(());
        }
        match self.entities.get(&name) {
            None => self.undeclared(&name),
            Some(Entity::External) => malformed(format!(
                "a reference to the external entity {name} in an attribute"
            )),
            Some(Entity::Internal(text)) => {
                self.count_expansion()?;
                if depth > 64 {
                    return malformed(format!("the entity {name} refers to itself"));
                }
                let mut chars = text.chars().peekable();
                while let Some(c) = chars.next() {
                    match c {
                        '<' => return malformed("< in an attribute value"),
                        '&' => {
                            let mut inner = String::new();
                            for c in chars.by_ref() {
                                if c == ';' {
                                    break;
                                }
                                inner.push(c);
                            }
                            let reference = match inner.strip_prefix('#') {
                                Some(code) => {
                                    let parsed = match code.strip_prefix('x') {
                                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                                        None => code.parse().ok(),
                                    };
                                    match parsed.and_then(char::from_u32).filter(|&c| is_char(c)) {
                                        Some(c) => Reference::Char(c),
                                        None => return malformed("an invalid character reference"),
                                    }
                                }
                                None => Reference::Entity(inner),
                            };
                            self.expand_in_attribute(reference, value, depth + 1)?;
                        }
                        c if is_space(c) => value.push(' '),
                        c => value.push(c),
                    }
                }
                Ok(())
            }
        }
    }

    /// Reads a qualified name into `name`, which it replaces; where its local name starts.
    fn qname_into(&mut self, what: &str, name: &mut String) -> Result<usize> {
        self.name_into(what, name)?;
        let bytes = name.as_bytes();
        let Some(colon) = bytes.iter().position(|&byte| byte == b':') else {
            return Ok(0);
        };
        let local = &bytes[colon + 1..];
        if colon > 0 && !local.is_empty() && !local.contains(&b':') {
            Ok(colon + 1)
        } else {
            malformed(format!("{name} is not a qualified name"))
        }
    }

    fn element(
        &mut self,
        scopes: &mut Scopes,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<(), Stop> {
        let mut open: Vec<Name> = Vec::new();
        loop {
            self.require("<", "a start tag")?;
            let (name, empty) = self.start_tag(scopes, handler)?;
            if !empty {
                open.push(name);
            } else {
                Self::end(&name, scopes, handler)?;
                self.spare_names.push(name.qualified.name);
            }
            loop {
                if open.is_empty() {
                    return Ok(());
                }
                self.content(handler)?;
                if self.skip("</")? {
                    let mut qname = self.spare_names.pop().unwrap_or_default();
                    self.qname_into("an end tag", &mut qname)?;
                    self.skip_spaces()?;
                    self.require(">", "the end of an end tag")?;
                    let name = open.pop().expect("an element is open");
                    if name.qualified_name() != qname {
                        return fatal(format!(
                            "the element {} must end with </{}>",
                            name.qualified_name(),
                            name.qualified_name()
                        ));
                    }
                    self.spare_names.push(qname);
                    Self::end(&name, scopes, handler)?;
                    self.spare_names.push(name.qualified.name);
                } else if self.peek()?.is_none() {
                    return fatal("the document ends inside an element");
                } else {
                    break;
                }
            }
        }
    }

    fn end(
        name: &Name,
        scopes: &mut Scopes,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<(), Stop> {
        let flow = handler(Event::End(name));
        let mark = scopes.marks.pop().expect("a scope is open");
        scopes.bindings.truncate(mark);
        match flow {
            Flow::Continue => Ok(()),
            Flow::Stop => Err(Stop::Handler),
        }
    }

    /// Character data, references, comments, processing instructions and CDATA sections, up
    /// to the next start or end tag, their text handed over piece by piece.
    fn content(
        &mut self,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<(), Stop> {
        loop {
            let data = self.character_data(handler)?;
            match self.peek()? {
                None => return emit_text(handler, self.text.slice(data), TextKind::Data),
                Some('<') => {
                    emit_text(handler, self.text.slice(data), TextKind::Data)?;
                    self.buffer(2);
                    if self.rest().as_bytes().get(1).is_some_and(|&byte| {
                        byte == b'/' || (byte.is_ascii() && is_name_start(char::from(byte)))
                    }) {
                        return Ok(());
                    }
                    if self.skip("<!--")? {
                        self.report_comment(handler)?;
                    } else if self.skip("<![CDATA[")? {
                        let section = self.until("]]>", "a CDATA section")?;
                        emit_text(handler, self.text.slice(section), TextKind::CData)?;
                    } else if self.skip("<?")? {
                        self.report_processing_instruction(handler)?;
                    } else if self.peek_at(1)? == Some('/')
                        || self.peek_at(1)?.is_some_and(is_name_start)
                    {
                        return Ok(());
                    } else {
                        return fatal("markup not recognized in content");
                    }
                }
                Some('&') => {
                    emit_text(handler, self.text.slice(data), TextKind::Data)?;
                    self.position += 1;
                    match self.reference()? {
                        Reference::Char(c) => {
                            emit_text(handler, c.encode_utf8(&mut [0; 4]), TextKind::Reference)?;
                        }
                        reference => {
                            let mut expansion = String::new();
                            let expanded = self.expand_in_content(reference, &mut expansion, 0);
                            emit_text(handler, &expansion, TextKind::Reference)?;
                            expanded?;
                        }
                    }
                }
                Some(_) => unreachable!("character data runs up to markup or a reference"),
            }
        }
    }

    /// Reads a run of character data up to markup, a reference or the end, failing on `]]>`
    /// and on a character Xerces rejects in content: where the part of it not yet handed to
    /// `handler` lies. A run that outgrows a decoded window is handed over in parts as the
    /// window moves on, each ending where a SAX piece of the run ends, so that the run's pieces
    /// stay those of the whole run.
    fn character_data(
        &mut self,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<std::ops::Range<usize>, Stop> {
        let mut start = self.position;
        self.mark = start;
        let mut split_from = CHUNK;
        loop {
            self.scan_buffered(|byte| matches!(byte, b'<' | b'&' | b']'));
            if self.position == self.text.end() && !self.text.complete() {
                let length = self.position - start;
                if length >= split_from {
                    let settled = settled_sax_pieces(self.text.slice(start..self.position));
                    if settled == 0 {
                        split_from = 2 * length;
                    } else {
                        let part = start..start + settled;
                        emit_text(handler, self.text.slice(part), TextKind::Data)?;
                        start += settled;
                        self.mark = start;
                        split_from = CHUNK;
                    }
                }
                self.fill();
                continue;
            }
            match self.peek()? {
                None | Some('<' | '&') => return Ok(start..self.position),
                Some(_) => {
                    if self.starts_with("]]>")? {
                        return fatal("]]> in content");
                    }
                    self.expect_char("content")?;
                }
            }
        }
    }

    /// Counts the start of an entity's expansion, failing past the limit as Xerces does.
    fn count_expansion(&self) -> Result<()> {
        let expansions = self.expansions.get() + 1;
        self.expansions.set(expansions);
        if expansions > ENTITY_EXPANSION_LIMIT {
            return Err(XmlError::EntityExpansionLimit);
        }
        Ok(())
    }

    fn expand_in_content(
        &self,
        reference: Reference,
        text: &mut String,
        depth: usize,
    ) -> Result<()> {
        let name = match reference {
            Reference::Char(c) => {
                text.push(c);
                return Ok(());
            }
            Reference::Entity(name) => name,
        };
        if let Some(c) = Self::predefined(&name) {
            text.push(c);
            return Ok(());
        }
        match self.entities.get(&name) {
            None => self.undeclared(&name),
            Some(Entity::External) => self.count_expansion(),
            Some(Entity::Internal(replacement)) => {
                self.count_expansion()?;
                if replacement.contains('<') {
                    return malformed(format!(
                        "the entity {name} holds markup, which this parser does not expand"
                    ));
                }
                if depth > 64 {
                    return malformed(format!("the entity {name} refers to itself"));
                }
                let mut rest = replacement.as_str();
                while let Some(start) = rest.find('&') {
                    text.push_str(&rest[..start]);
                    let end = rest[start..]
                        .find(';')
                        .map(|e| start + e)
                        .ok_or_else(|| XmlError::Malformed("an unterminated reference".into()))?;
                    let inner = &rest[start + 1..end];
                    let reference = match inner.strip_prefix('#') {
                        Some(code) => {
                            let parsed = match code.strip_prefix('x') {
                                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                                None => code.parse().ok(),
                            };
                            match parsed.and_then(char::from_u32).filter(|&c| is_char(c)) {
                                Some(c) => Reference::Char(c),
                                None => return malformed("an invalid character reference"),
                            }
                        }
                        None => Reference::Entity(inner.to_owned()),
                    };
                    self.expand_in_content(reference, text, depth + 1)?;
                    rest = &rest[end + 1..];
                }
                text.push_str(rest);
                Ok(())
            }
        }
    }

    /// A start tag after its `<`: the element's name and whether it is empty.
    fn start_tag(
        &mut self,
        scopes: &mut Scopes,
        handler: &mut dyn FnMut(Event<'_>) -> Flow,
    ) -> std::result::Result<(Name, bool), Stop> {
        let mut element = self.spare_names.pop().unwrap_or_default();
        let local_start = self.qname_into("an element", &mut element)?;
        let qualified = QName {
            name: element,
            local_start,
        };
        let mut slots = std::mem::take(&mut self.attributes);
        let read = self.attributes_into(&qualified, &mut slots, scopes);
        let attribute_order = self.attribute_order;
        let result = read.and_then(|(count, empty, namespace)| {
            if attribute_order == AttributeOrder::DeclarationsFirst {
                declarations_first(&mut slots[..count]);
            }
            let name = Name {
                qualified,
                namespace,
            };
            match handler(Event::Start(&name, &slots[..count])) {
                Flow::Continue => Ok((name, empty)),
                Flow::Stop => Err(Stop::Handler),
            }
        });
        self.attributes = slots;
        result
    }

    /// The namespace of an element, once its start tag's declarations are in scope.
    fn element_namespace(qualified: &QName, scopes: &Scopes) -> std::result::Result<Rc<str>, Stop> {
        match qualified.prefix() {
            None => Ok(scopes.resolve("").unwrap_or(&scopes.none).clone()),
            Some(prefix) => match scopes.resolve(prefix) {
                Some(uri) if !uri.is_empty() => Ok(uri.clone()),
                _ => fatal(format!(
                    "the prefix {prefix} of {} is not bound",
                    qualified.name
                )),
            },
        }
    }

    /// Reads the attributes of the start tag of `element` into `slots`, defaults added and
    /// namespaces resolved, and opens the element's namespace scope: how many there are,
    /// whether the element is empty, and its namespace.
    fn attributes_into(
        &mut self,
        element: &QName,
        slots: &mut Vec<Attribute>,
        scopes: &mut Scopes,
    ) -> std::result::Result<(usize, bool, Rc<str>), Stop> {
        let mut count = 0;
        let empty = loop {
            let spaced = self.skip_spaces()?;
            if self.skip("/>")? {
                break true;
            }
            if self.skip(">")? {
                break false;
            }
            if !spaced {
                return fatal(format!("the element {} is unterminated", element.name));
            }
            if count == slots.len() {
                slots.push(Attribute::default());
            }
            let local_start = self.qname_into("an attribute", &mut slots[count].qualified_name)?;
            self.skip_spaces()?;
            self.require("=", "= after an attribute name")?;
            self.skip_spaces()?;
            self.attribute_value_into(&mut slots[count].value)?;
            let (read, slot) = slots.split_at_mut(count);
            let slot = &mut slot[0];
            if read.iter().any(|a| a.qualified_name == slot.qualified_name) {
                return fatal(format!(
                    "the attribute {} appears twice",
                    slot.qualified_name
                ));
            }
            slot.local_name.clear();
            slot.local_name
                .push_str(&slot.qualified_name[local_start..]);
            count += 1;
        };
        if let Some(defaults) = self.defaults.get(&element.name) {
            for default in defaults {
                if slots[..count]
                    .iter()
                    .any(|a| a.qualified_name == default.name)
                {
                    continue;
                }
                if count == slots.len() {
                    slots.push(Attribute::default());
                }
                let slot = &mut slots[count];
                let local_start = default.name.find(':').map_or(0, |colon| colon + 1);
                slot.qualified_name.clear();
                slot.qualified_name.push_str(&default.name);
                slot.local_name.clear();
                slot.local_name.push_str(&default.name[local_start..]);
                slot.value.clear();
                slot.value.push_str(&default.value);
                count += 1;
            }
        }
        scopes.marks.push(scopes.bindings.len());
        for attribute in &slots[..count] {
            let declared = match (
                attribute_prefix(attribute),
                attribute.qualified_name.as_str(),
            ) {
                (None, "xmlns") => Some(""),
                (Some("xmlns"), _) => Some(attribute.local_name.as_str()),
                _ => None,
            };
            let Some(declared) = declared else {
                continue;
            };
            let value = attribute.value.as_str();
            if declared == "xmlns" {
                return fatal("the prefix xmlns cannot be declared");
            }
            if (declared == "xml") != (value == XML_NAMESPACE) {
                return fatal("the xml prefix is bound to its namespace only");
            }
            if value == XMLNS_NAMESPACE {
                return fatal("the xmlns namespace cannot be bound");
            }
            if !declared.is_empty() && value.is_empty() {
                return fatal(format!("the prefix {declared} cannot be unbound"));
            }
            scopes.bindings.push((declared.to_owned(), Rc::from(value)));
        }
        let namespace = Self::element_namespace(element, scopes)?;
        for index in 0..count {
            let (resolved, rest) = slots.split_at_mut(index);
            let attribute = &mut rest[0];
            let uri: &str = match attribute_prefix(attribute) {
                None if attribute.qualified_name == "xmlns" => XMLNS_NAMESPACE,
                None => "",
                Some("xmlns") => XMLNS_NAMESPACE,
                Some(p) => match scopes.resolve(p) {
                    Some(uri) if !uri.is_empty() => uri,
                    _ => {
                        return fatal(format!(
                            "the prefix {p} of {} is not bound",
                            attribute.qualified_name
                        ))
                    }
                },
            };
            if !uri.is_empty()
                && resolved
                    .iter()
                    .any(|a| a.namespace == uri && a.local_name == attribute.local_name)
            {
                return fatal(format!(
                    "the attribute {} appears twice",
                    attribute.qualified_name
                ));
            }
            attribute.namespace.clear();
            attribute.namespace.push_str(uri);
        }
        Ok((count, empty, namespace))
    }
}

/// Moves the `xmlns` attributes among `attributes` before the others, each kept in order.
fn declarations_first(attributes: &mut [Attribute]) {
    let mut declarations = 0;
    for index in 0..attributes.len() {
        if attributes[index].namespace == XMLNS_NAMESPACE {
            attributes[declarations..=index].rotate_right(1);
            declarations += 1;
        }
    }
}

/// Hands `handler` the text of an element, unless it is empty text other than a CDATA section.
fn emit_text(
    handler: &mut dyn FnMut(Event<'_>) -> Flow,
    text: &str,
    kind: TextKind,
) -> std::result::Result<(), Stop> {
    if (text.is_empty() && kind != TextKind::CData)
        || handler(Event::Text(text, kind)) == Flow::Continue
    {
        Ok(())
    } else {
        Err(Stop::Handler)
    }
}

/// The length in bytes of the run `text` starts with of characters [`is_char`] accepts that are
/// not ASCII or are ASCII bytes `plain` picks. Of the characters not ASCII it rejects, a `str`
/// holds only U+FFFE and U+FFFF, whose UTF-8 is EF BF BE and EF BF BF, so the run steps over the
/// bytes of the others one at a time.
fn char_run(text: &str, plain: impl Fn(u8) -> bool) -> usize {
    let bytes = text.as_bytes();
    let mut length = 0;
    while length < bytes.len() {
        let byte = bytes[length];
        let accepted = if byte.is_ascii() {
            plain(byte)
        } else {
            byte != 0xEF || bytes[length + 1] != 0xBF || !matches!(bytes[length + 2], 0xBE | 0xBF)
        };
        if !accepted {
            break;
        }
        length += 1;
    }
    length
}

/// The length in bytes of the run of name characters `text` starts with, and whether a
/// character other than a name character ends it, rather than the end of `text`.
fn name_run(text: &str) -> (usize, bool) {
    let bytes = text.as_bytes();
    let mut length = 0;
    while length < bytes.len() {
        let byte = bytes[length];
        if byte.is_ascii() {
            if !is_ascii_name_char(byte) {
                return (length, true);
            }
            length += 1;
        } else {
            let c = text[length..]
                .chars()
                .next()
                .expect("a character starts here");
            if !is_name_char(c) {
                return (length, true);
            }
            length += c.len_utf8();
        }
    }
    (length, false)
}

/// Whether the JDK's scanner reads on over the character `byte` is part of in a run of character
/// data: anything but a line break, `]` and a character outside the Basic Multilingual Plane, the
/// characters whose UTF-8 starts with a byte of 0xF0 or more.
fn is_content_byte(byte: u8) -> bool {
    !matches!(byte, b'\n' | b']') && byte < 0xF0
}

/// The end of the run the JDK's scanner reads from `start`: line breaks, then content.
fn scanned_run(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    let content = rest.trim_start_matches('\n');
    let end = content
        .bytes()
        .position(|byte| !is_content_byte(byte))
        .unwrap_or(content.len());
    start + (rest.len() - content.len()) + end
}

fn after_brackets(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    start + rest.len() - rest.trim_start_matches(']').len()
}

/// Where the piece that starts at `start` of the character data `text`, whose line ends are
/// normalized, ends, as the JDK's SAX parser reports such data in pieces: a run of line breaks
/// and content, any `]`s, a second such run and its `]`s; or a run up to and with a character
/// outside the Basic Multilingual Plane. A piece that ends before the end of `text` ends there
/// whatever follows `text`.
pub(crate) fn sax_piece_end(text: &str, start: usize) -> usize {
    let mut position = scanned_run(text, start);
    if let Some(character) = text[position..].chars().next() {
        if character >= '\u{10000}' {
            position += character.len_utf8();
        } else {
            position = after_brackets(text, position);
            position = after_brackets(text, scanned_run(text, position));
        }
    }
    position
}

/// Where the last of the SAX pieces of the start of a run of character data ends that ends
/// before the end of `text`, the start of the run: zero if none does.
fn settled_sax_pieces(text: &str) -> usize {
    let mut settled = 0;
    loop {
        let end = sax_piece_end(text, settled);
        if end >= text.len() {
            return settled;
        }
        settled = end;
    }
}

/// The prefix of an attribute's qualified name, if it has one.
fn attribute_prefix(attribute: &Attribute) -> Option<&str> {
    let prefix_length = attribute.qualified_name.len() - attribute.local_name.len();
    (prefix_length > 0).then(|| &attribute.qualified_name[..prefix_length - 1])
}

enum Reference {
    Char(char),
    Entity(String),
}

/// Whether `name` is a valid encoding name: `[A-Za-z] ([A-Za-z0-9._] | '-')*`.
fn is_encoding_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(bytes: &[u8]) -> std::result::Result<Option<(String, String)>, XmlError> {
        let mut found = None;
        parse(bytes, &mut |event| {
            if let Event::Start(name, _) = event {
                found = Some((name.namespace().to_owned(), name.local_name().to_owned()));
                return Flow::Stop;
            }
            Flow::Continue
        })?;
        Ok(found)
    }

    fn malformed_root(bytes: &[u8]) -> bool {
        matches!(root(bytes), Err(XmlError::Malformed(_)))
    }

    #[test]
    fn finds_the_root_and_its_namespace() {
        assert_eq!(
            root(b"<?xml version=\"1.0\"?>\n<!-- c --><a:svg xmlns:a='urn:x' b='1'>"),
            Ok(Some(("urn:x".into(), "svg".into())))
        );
        assert_eq!(
            root(b"\xEF\xBB\xBF<html xmlns=\"http://www.w3.org/1999/xhtml\">"),
            Ok(Some(("http://www.w3.org/1999/xhtml".into(), "html".into())))
        );
    }

    #[test]
    fn rejects_what_xerces_rejects() {
        assert!(malformed_root(b"text<a/>"));
        assert!(malformed_root(b"<a:b/>"));
        assert!(malformed_root(b"<a x='1' x='2'/>"));
        assert!(malformed_root(b"<a x=1/>"));
        assert!(malformed_root(b"<!-- a -- b --><a/>"));
        assert!(malformed_root(b" <?xml version='1.0'?><a/>"));
        assert!(malformed_root(b"<?xml encoding='UTF-8'?><a/>"));
        assert!(malformed_root(b"<a x='&nbsp;'/>"));
        assert!(malformed_root(b""));
    }

    fn text(bytes: &[u8]) -> (String, Result<()>) {
        let mut text = String::new();
        let result = parse(bytes, &mut |event| {
            if let Event::Text(piece, _) = event {
                text.push_str(piece);
            }
            Flow::Continue
        });
        (text, result)
    }

    #[test]
    fn stops_expanding_entities_past_xerces_limit_with_the_text_read_before() {
        let mut bomb = String::from("<!DOCTYPE a [<!ENTITY e0 'lol'>");
        for level in 1..10 {
            bomb.push_str(&format!(
                "<!ENTITY e{level} '{}'>",
                format!("&e{};", level - 1).repeat(10)
            ));
        }
        bomb.push_str("]><a>&e9;</a>");
        assert_eq!(
            text(bomb.as_bytes()),
            ("lol".repeat(10), Err(XmlError::EntityExpansionLimit))
        );

        let twenty = "&e;".repeat(ENTITY_EXPANSION_LIMIT);
        let within = format!("<!DOCTYPE a [<!ENTITY e 'x'>]><a b='&e;&amp;'>{twenty}&#65;&lt;</a>");
        assert!(text(within.as_bytes()).1.is_err());
        let within = format!("<!DOCTYPE a [<!ENTITY e 'x'>]><a>{twenty}&#65;&lt;</a>");
        assert_eq!(
            text(within.as_bytes()),
            (format!("{}A<", "x".repeat(ENTITY_EXPANSION_LIMIT)), Ok(()))
        );
    }

    #[test]
    fn tolerates_entities_an_external_subset_may_declare() {
        assert_eq!(
            root(
                b"<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0//EN\" \"x.dtd\"><html t='&nbsp;'>"
            ),
            Ok(Some((String::new(), "html".into())))
        );
        assert_eq!(
            root(
                b"<!DOCTYPE a [<!ENTITY e 'v'><!ATTLIST a xmlns CDATA #FIXED 'urn:d'>]><a t='&e;'>"
            ),
            Ok(Some(("urn:d".into(), "a".into())))
        );
    }

    #[test]
    fn skips_a_document_type_without_dtd_support() {
        let skipped = |bytes: &[u8]| {
            let mut attributes = Vec::new();
            let mut texts = Vec::new();
            let paging = Paging::new(Pages::IN_MEMORY);
            let result = parse_paged(
                bytes,
                &paging,
                DocumentTypes::Skipped,
                AttributeOrder::Written,
                &mut |event| {
                    match event {
                        Event::Start(_, found) => {
                            attributes.extend(found.iter().map(|a| a.local_name.clone()))
                        }
                        Event::Text(text, _) => texts.push(text.to_owned()),
                        _ => {}
                    }
                    Flow::Continue
                },
            );
            (result.is_ok(), attributes, texts.concat())
        };
        assert_eq!(
            skipped(b"<!DOCTYPE a [ <!ENTITY e > %p; ]><a>x&amp;&#65;</a>"),
            (true, vec![], "x&A".to_owned())
        );
        assert_eq!(
            skipped(b"<!DOCTYPE a [<!ATTLIST a b CDATA 'd'>]><a/>"),
            (true, vec![], String::new())
        );
        for undeclared in [
            &b"<!DOCTYPE a [<!ENTITY e 'v'>]><a>&e;</a>"[..],
            b"<!DOCTYPE a SYSTEM 'x.dtd'><a>&e;</a>",
            b"<!DOCTYPE a [<!ENTITY % p 'x'> %p;]><a t='&e;'/>",
        ] {
            assert!(
                !skipped(undeclared).0,
                "{}",
                String::from_utf8_lossy(undeclared)
            );
        }
        assert_eq!(
            events(b"<!DOCTYPE a [<!ENTITY e 'v'>]><a>&e;</a>").0,
            ["<!DOCTYPE>", "<a>", "v", "</a>"]
        );
    }

    #[test]
    fn orders_declarations_first_as_a_stable_partition_does() {
        let tags = |bytes: &[u8], order: AttributeOrder| {
            let mut tags = Vec::new();
            let paging = Paging::new(Pages::IN_MEMORY);
            let result = parse_paged(
                bytes,
                &paging,
                DocumentTypes::Processed,
                order,
                &mut |event| {
                    if let Event::Start(_, found) = event {
                        tags.push(found.to_vec());
                    }
                    Flow::Continue
                },
            );
            assert_eq!(result, Ok(()));
            tags
        };
        for document in [
            &b"<a b='1' xmlns='urn:d' c:x='2' xmlns:c='urn:c' xmlns:e='urn:e'>\
                <e:f xmlns:g='urn:g' g:h='3' i='4'/><j k='5'/><l xmlns='urn:l'/><m/></a>"[..],
            b"<!DOCTYPE a [<!ATTLIST i xmlns:z CDATA #FIXED 'urn:z' d CDATA 'v'>]>\
                <a xmlns:y='urn:y' y:p='1'><i j='4'/><i xmlns:z='urn:z' z:q='2'/></a>",
            b"<a><b c='1'/></a>",
        ] {
            let written = tags(document, AttributeOrder::Written);
            let expected: Vec<Vec<Attribute>> = written
                .into_iter()
                .map(|attributes| {
                    let (mut declarations, others): (Vec<_>, Vec<_>) = attributes
                        .into_iter()
                        .partition(|a| a.namespace == XMLNS_NAMESPACE);
                    declarations.extend(others);
                    declarations
                })
                .collect();
            assert_eq!(tags(document, AttributeOrder::DeclarationsFirst), expected);
        }
    }

    /// The events of a parse and how it ended.
    fn events(bytes: &[u8]) -> (Vec<String>, Result<()>) {
        let mut events = Vec::new();
        let result = parse(bytes, &mut |event| {
            events.push(match event {
                Event::Start(name, _) => format!("<{}>", name.local_name()),
                Event::End(name) => format!("</{}>", name.local_name()),
                Event::Text(text, _) => text.to_owned(),
                Event::DocumentType => "<!DOCTYPE>".to_owned(),
                Event::Comment(text) => format!("<!--{text}-->"),
                Event::ProcessingInstruction(target, data) => format!("<?{target}|{data}?>"),
            });
            Flow::Continue
        });
        (events, result)
    }

    fn char_by_char_run(text: &str, plain: impl Fn(u8) -> bool) -> usize {
        text.char_indices()
            .find(|&(_, c)| {
                if c.is_ascii() {
                    !plain(c as u8)
                } else {
                    !is_char(c)
                }
            })
            .map_or(text.len(), |(at, _)| at)
    }

    #[test]
    fn runs_over_characters_as_a_check_of_each_does() {
        let content = |byte: u8| byte != b'<' && (byte >= 0x20 || is_space(char::from(byte)));
        let attribute = |byte: u8| byte >= 0x20 && byte != b'\'' && byte != b'&';
        for text in [
            "",
            "abc",
            "é",
            "a\u{FFFE}b",
            "\u{FFFF}",
            "é\u{FFFF}",
            "\u{FFFD}\u{FFFD}<",
            "\u{FEFF}\u{EFFF}\u{F7BF}\u{FFBE}x",
            "日本語<x",
            "a\u{10000}b\u{FFFE}",
            "\u{10FFFF}'",
            "\u{1}",
            "x\ty\n\r&",
            "\u{7F}\u{80}\u{7FF}\u{800}\u{FFFC}",
        ] {
            assert_eq!(
                char_run(text, content),
                char_by_char_run(text, content),
                "{text:?}"
            );
            assert_eq!(
                char_run(text, attribute),
                char_by_char_run(text, attribute),
                "{text:?}"
            );
        }
    }

    #[test]
    fn reads_references_and_characters_beyond_ascii() {
        let mut values = Vec::new();
        let mut texts = Vec::new();
        let result = parse(
            "<a b='&lt;&gt;&amp;&apos;&quot; é&#233;\u{FFFD}\u{FEFF}日\t&lt;z'>&lt;x&gt;&amp;&apos;&quot;é\u{FFFD}</a>"
                .as_bytes(),
            &mut |event| {
                match event {
                    Event::Start(_, found) => values.extend(found.iter().map(|a| a.value.clone())),
                    Event::Text(text, _) => texts.push(text.to_owned()),
                    _ => {}
                }
                Flow::Continue
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(values, ["<>&'\" éé\u{FFFD}\u{FEFF}日 <z"]);
        assert_eq!(texts, ["<", "x", ">", "&", "'", "\"", "é\u{FFFD}"]);
        for malformed in [
            "<a>\u{FFFE}</a>",
            "<a>é\u{FFFF}</a>",
            "<a b='\u{FFFE}'/>",
            "<a b='é\u{FFFF}'/>",
            "<a>&ltx;</a>",
            "<a>&lt</a>",
            "<a b='&lt'/>",
            "<a b='&quotx;'/>",
        ] {
            assert!(
                matches!(text(malformed.as_bytes()).1, Err(XmlError::Malformed(_))),
                "{malformed}"
            );
        }
    }

    #[test]
    fn reports_comments_instructions_and_empty_cdata_sections() {
        let (events, result) = events(
            b"<?xml version='1.0'?><!--a--><?pi  x ?><r>t<!-- b --><?q?><![CDATA[]]>&amp;</r><!---->",
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            events,
            [
                "<!--a-->",
                "<?pi|x ?>",
                "<r>",
                "t",
                "<!-- b -->",
                "<?q|?>",
                "",
                "&",
                "</r>",
                "<!---->"
            ]
        );
    }

    #[test]
    fn normalizes_line_ends_as_xerces_does() {
        assert_eq!(
            events(b"<a>1\r2\r\n3\n\r4\r</a>\r\n"),
            (
                vec![
                    "<a>".to_owned(),
                    "1\n2\n3\n\n4\n".to_owned(),
                    "</a>".to_owned()
                ],
                Ok(())
            )
        );
        assert_eq!(
            events(b"<a b='\r\n\rc'>\xE2\x82\xAC</a>").0,
            ["<a>", "\u{20AC}", "</a>"]
        );
    }

    /// What the JDK's SAX parser reports and throws for Latin-1 bytes read as UTF-8.
    #[test]
    fn fails_on_malformed_utf8_where_the_jdk_does() {
        let conversion = Err(XmlError::CharConversion);
        let cases: [(&[u8], &[&str]); 6] = [
            (b"<a b='c'>\xE9</a>", &["<a>"]),
            (b"<a b='c'/>\xE9", &["<a>", "</a>"]),
            (b"<?xml version=\"1.0\"?><a>x\xE9</a>", &["<a>"]),
            (
                b"<?xml version=\"1.0\"?><a><b/>\xE9</a>",
                &["<a>", "<b>", "</b>"],
            ),
            (b"<!-- \xE9 --><a/>", &[]),
            (b"<a>\xE9</a>", &[]),
        ];
        for (document, reported) in cases {
            assert_eq!(
                events(document),
                (
                    reported.iter().map(|e| e.to_string()).collect(),
                    conversion.clone()
                )
            );
        }
    }

    #[test]
    fn decodes_what_follows_the_declaration_in_its_encoding() {
        let short = b"<?xml version=\"1.0\" encoding='windows-1251'?><a>\xcc\xe0\xff\xea</a>";
        assert_eq!(
            events(short),
            (
                vec![
                    "<a>".to_owned(),
                    "\u{41c}\u{430}\u{44f}\u{43a}".to_owned(),
                    "</a>".to_owned()
                ],
                Ok(())
            )
        );
    }

    #[test]
    fn reads_utf16() {
        let doc: Vec<u8> = "\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-16\"?><r/>"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(root(&doc), Ok(Some((String::new(), "r".into()))));
    }

    /// UTF-16 delivers every character before an odd final byte and then fails, where Xerces
    /// drops the whole read of 8192 bytes the byte falls in (#186).
    #[test]
    fn reads_utf16_up_to_an_odd_final_byte() {
        let text = "y".repeat(10_000);
        let mut bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        bytes.push(b'<');
        let mut read = String::new();
        let malformed = decode(Reader::Utf16 { big_endian: true }, &bytes, 0, |c, _| {
            read.push(c);
            true
        });
        assert!(malformed);
        assert_eq!(read, text);
        let mut doc = vec![0xFE, 0xFF];
        doc.extend("<a>x</a>".encode_utf16().flat_map(u16::to_be_bytes));
        doc.push(b' ');
        assert_eq!(events(&doc).1, Err(XmlError::CharConversion));
    }

    /// An unpaired surrogate of UTF-16 is a malformed sequence, where Xerces makes it U+FFFE.
    #[test]
    fn fails_on_an_unpaired_utf16_surrogate() {
        let mut doc = vec![0xFE, 0xFF];
        doc.extend("<a>x".encode_utf16().flat_map(u16::to_be_bytes));
        doc.extend([0xD8, 0x00]);
        doc.extend("</a>".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(events(&doc).1, Err(XmlError::CharConversion));
    }

    #[test]
    fn checks_the_whole_document() {
        let mut ends = 0;
        let result = parse(b"<a><b>x</b><c/></a><!-- tail -->", &mut |event| {
            if let Event::End(_) = event {
                ends += 1;
            }
            Flow::Continue
        });
        assert_eq!(result, Ok(()));
        assert_eq!(ends, 3);
        assert!(parse(b"<a><b></a>", &mut |_| Flow::Continue).is_err());
        assert!(parse(b"<a/><b/>", &mut |_| Flow::Continue).is_err());
        assert!(parse(b"<a>]]></a>", &mut |_| Flow::Continue).is_err());
    }

    /// The character data of the one element of `document`, checked to come in data events that
    /// each end where a SAX piece of the whole ends.
    fn run_text(document: &[u8]) -> String {
        let mut parts = Vec::new();
        let result = parse(document, &mut |event| {
            if let Event::Text(text, kind) = event {
                assert_eq!(kind, TextKind::Data);
                parts.push(text.to_owned());
            }
            Flow::Continue
        });
        assert_eq!(result, Ok(()));
        let whole = parts.concat();
        let mut piece_ends = Vec::new();
        let mut at = 0;
        while at < whole.len() {
            at = sax_piece_end(&whole, at);
            piece_ends.push(at);
        }
        let mut end = 0;
        for part in &parts {
            end += part.len();
            assert!(
                piece_ends.binary_search(&end).is_ok(),
                "a part ends at {end}"
            );
        }
        whole
    }

    /// `head`, then each of `parts` from where it says among the bytes after `head`, the gaps
    /// filled with `x`, then `</a>`.
    fn cut_document(head: &[u8], parts: &[(usize, &[u8])]) -> Vec<u8> {
        let mut document = head.to_vec();
        for &(at, part) in parts {
            document.resize(head.len() + at, b'x');
            document.extend_from_slice(part);
        }
        document.extend_from_slice(b"</a>");
        document
    }

    /// What `document` holds between `head` and `</a>`, decoded by `charset`, its line ends
    /// normalized.
    fn expected_text(document: &[u8], head: &[u8], charset: &str) -> String {
        Charset::named(charset)
            .decode(&document[head.len()..document.len() - 4])
            .unwrap()
            .replace("\r\n", "\n")
            .replace('\r', "\n")
    }

    #[test]
    fn reads_line_ends_and_characters_cut_by_chunk_ends_as_whole() {
        let head = b"<a>";
        let parts: [(usize, &[u8]); 5] = [
            (CHUNK - 4, b"\r\n"),
            (2 * CHUNK - 4, "\u{e6}".as_bytes()),
            (3 * CHUNK - 5, b"\ry\n\n"),
            (4 * CHUNK - 5, "\u{20ac}".as_bytes()),
            (5 * CHUNK - 6, "\u{1F600}]\r".as_bytes()),
        ];
        let in_place = cut_document(
            head,
            &[
                (CHUNK - 4, "\u{e6}".as_bytes()),
                (2 * CHUNK - 5, "\u{1F600}\n".as_bytes()),
                (3 * CHUNK - 6, "\u{20ac}]\n]".as_bytes()),
                (5 * CHUNK, b"\r\nz\r"),
            ],
        );
        assert_eq!(run_text(&in_place), expected_text(&in_place, head, "UTF-8"));

        let utf8 = cut_document(head, &parts);
        let expected = expected_text(&utf8, head, "UTF-8");
        assert_eq!(run_text(&utf8), expected);

        let utf16: Vec<u8> = std::str::from_utf8(&utf8)
            .unwrap()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(run_text(&[&[0xFF, 0xFE], &utf16[..]].concat()), expected);

        let cyrillic = b"<?xml version=\"1.0\" encoding=\"windows-1251\"?><a>";
        let document = cut_document(
            cyrillic,
            &[
                (CHUNK - 4, b"\r\n"),
                (2 * CHUNK - 4, b"\r\xcc"),
                (3 * CHUNK - 5, b"\xe0\r"),
            ],
        );
        assert_eq!(
            run_text(&document),
            expected_text(&document, cyrillic, "windows-1251")
        );

        let japanese = b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?><a>";
        let document = cut_document(
            japanese,
            &[(CHUNK - 4, b"\x82\xa0"), (2 * CHUNK - 3, b"\r\n\x82\xa2")],
        );
        assert_eq!(
            run_text(&document),
            expected_text(&document, japanese, "Shift_JIS")
        );
    }

    #[test]
    fn hands_over_a_long_run_of_decoded_text_in_parts() {
        let line = "Linje med \u{e6}, \u{f8} og \u{e5}\r\n";
        let run = line.repeat(16 * CHUNK / line.len());
        let document = format!("<a>{run}</a>");
        let mut parts = 0;
        parse(document.as_bytes(), &mut |event| {
            if let Event::Text(..) = event {
                parts += 1;
            }
            Flow::Continue
        })
        .unwrap();
        assert!(parts > 1, "the run comes in {parts} parts");
        assert_eq!(run_text(document.as_bytes()), run.replace("\r\n", "\n"));
    }
}
