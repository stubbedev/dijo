//! A namespace-aware, non-validating XML parser that fails where the JDK's built-in Xerces, as
//! Tika configures it (no external DTD, no external entities), raises a fatal error, and reads
//! the bytes as Xerces reads them: the byte order mark and first bytes pick the encoding, the
//! bytes it reads before the declaration ends are decoded before the declaration can switch it,
//! and a malformed UTF-8 or UTF-16 sequence fails the parse when the scanner reaches it, as
//! Xerces' own readers do. It
//! reports start and end tags and the text inside elements; `formats::xml` gives SAX's view of
//! them to the native parsers.
//!
//! What it does not reproduce: the IANA-to-Java encoding table of Xerces (a declared encoding is
//! looked up as Java's `Charset.forName` looks it up), the replacement text of parameter
//! entities (a reference to one is noted, not expanded), and general entities whose replacement
//! text holds markup.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::input::{Pages, Paging, WINDOW};
use crate::text::charset::Charset;

const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

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
    /// it; other text only when it is not.
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

/// Parses `bytes` as one XML document, handing each start and end tag to `handler` until it
/// asks to stop. An error is what Xerces would throw before the handler stopped it.
pub fn parse(bytes: &[u8], handler: &mut dyn FnMut(Event<'_>) -> Flow) -> Result<()> {
    parse_paged(bytes, &Paging::new(Pages::IN_MEMORY), handler)
}

/// [`parse`] of `bytes` whose pages `paging` releases as their characters are first checked.
pub(crate) fn parse_paged(
    bytes: &[u8],
    paging: &Paging,
    handler: &mut dyn FnMut(Event<'_>) -> Flow,
) -> Result<()> {
    let Some((detected, reader, start)) = detect_encoding(bytes) else {
        return malformed("UCS-4 in an unusual byte order is unsupported");
    };
    let mut parser = Parser {
        text: Text::new(bytes, detected, reader, start, paging),
        position: 0,
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

/// Where Xerces' reads in chunks end before the declaration is scanned: the four bytes it sniffed
/// the encoding from, then a chunk of 28; it reads on byte by byte until the declaration ends.
const FIRST_READS: usize = 4 + 28;

/// The document's characters, decoded as Xerces decodes them, line ends normalized, up to the
/// first character its reader cannot deliver. A UTF-8 document without carriage returns is read
/// where it lies; any other is held decoded, as UTF-8. The byte offsets the switch to a declared
/// encoding needs are decoded again for the few characters of the declaration.
struct Text<'a> {
    text: Cow<'a, str>,
    /// Whether the characters stop at a malformed sequence rather than at the end.
    malformed: bool,
    bytes: &'a [u8],
    start: usize,
    /// The reader the characters were decoded with.
    reader: Reader,
    /// Where the reads in chunks made before the declaration is scanned end.
    first_read_end: usize,
    /// The encoding name Xerces records for the document before its declaration.
    detected: &'static str,
}

impl<'a> Text<'a> {
    fn new(
        bytes: &'a [u8],
        detected: &'static str,
        reader: Reader,
        start: usize,
        paging: &Paging,
    ) -> Text<'a> {
        let (text, malformed) = match reader {
            Reader::Utf8 => {
                let body = &bytes[start.min(bytes.len())..];
                let checked = Utf8Check::of(body, paging);
                let valid = checked.valid(body);
                let malformed = checked.malformed;
                if checked.carriage_return {
                    (Cow::Owned(normalize_line_ends(valid)), malformed)
                } else {
                    (Cow::Borrowed(valid), malformed)
                }
            }
            _ => {
                let mut text = String::with_capacity(bytes.len().saturating_sub(start));
                let malformed = decode_normalized(reader, bytes, start, |c, _| {
                    text.push(c);
                    true
                });
                (Cow::Owned(text), malformed)
            }
        };
        Text {
            text,
            malformed,
            bytes,
            start,
            reader,
            first_read_end: FIRST_READS.min(bytes.len()),
            detected,
        }
    }

    /// Switches to the declared encoding, as `XMLEntityScanner.setEncoding` does: the
    /// characters the first reader decoded stay, and a new reader takes over where the first
    /// stopped reading bytes, which loses what a malformed sequence left unread in the first.
    /// `declaration_end` counts the characters up to the end of the declaration.
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
        let mut ends = Vec::new();
        decode_normalized(self.reader, self.bytes, self.start, |_, end| {
            let previous_end = ends.last().copied().unwrap_or(self.start);
            ends.push(end);
            ends.len() < declaration_end || previous_end < self.first_read_end
        });
        let mut first_read = 0;
        let mut previous_end = self.start;
        for &end in &ends {
            if previous_end >= self.first_read_end {
                break;
            }
            first_read += 1;
            previous_end = end;
        }
        let holds_first_read_only = self.text.chars().take(first_read + 1).count() == first_read;
        let (kept, resume) = if declaration_end > first_read {
            (declaration_end, ends[declaration_end - 1])
        } else if holds_first_read_only && self.malformed {
            (first_read, self.first_read_end)
        } else {
            (first_read, previous_end.max(self.first_read_end))
        };
        let kept_bytes = self
            .text
            .char_indices()
            .nth(kept)
            .map_or(self.text.len(), |(index, _)| index);
        let text = self.text.to_mut();
        text.truncate(kept_bytes);
        self.reader = reader;
        self.malformed = decode_normalized(reader, self.bytes, resume, |c, _| {
            text.push(c);
            true
        });
        Ok(())
    }
}

/// `text` with each `\r\n` and lone `\r` made a `\n`.
/// What a check of UTF-8 bytes found, a window at a time.
struct Utf8Check {
    /// How many bytes from the start are valid UTF-8.
    valid_up_to: usize,
    /// Whether a malformed sequence ends them, rather than the end.
    malformed: bool,
    /// Whether a carriage return stands among them.
    carriage_return: bool,
}

impl Utf8Check {
    /// Checks `bytes` a window at a time, each window counted read by `paging`, a character
    /// cut by a window's end checked with the next.
    fn of(bytes: &[u8], paging: &Paging) -> Self {
        let mut at = 0;
        let mut carriage_return = false;
        while at < bytes.len() {
            let end = bytes.len().min(at + WINDOW);
            let window = &bytes[at..end];
            let (good, malformed) = match std::str::from_utf8(window) {
                Ok(_) => (window.len(), false),
                Err(error) if error.error_len().is_none() && end < bytes.len() => {
                    (error.valid_up_to(), false)
                }
                Err(error) => (error.valid_up_to(), true),
            };
            carriage_return |= memchr::memchr(b'\r', &window[..good]).is_some();
            paging.read(good);
            at += good;
            if malformed {
                return Self {
                    valid_up_to: at,
                    malformed,
                    carriage_return,
                };
            }
        }
        Self {
            valid_up_to: at,
            malformed: false,
            carriage_return,
        }
    }

    /// The valid characters of `bytes`, which this check checked: valid UTF-8, window by
    /// window, each window ending at a character's end. They are not checked again, which would
    /// read all their pages at once a second time.
    fn valid<'b>(&self, bytes: &'b [u8]) -> &'b str {
        let valid = &bytes[..self.valid_up_to];
        unsafe { std::str::from_utf8_unchecked(valid) }
    }
}

fn normalize_line_ends(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\r') {
        normalized.push_str(&rest[..at]);
        normalized.push('\n');
        rest = &rest[at + 1..];
        if let Some(after) = rest.strip_prefix('\n') {
            rest = after;
        }
    }
    normalized.push_str(rest);
    normalized
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
    let body = &bytes[start.min(bytes.len())..];
    match reader {
        Reader::Utf8 => {
            let (valid, malformed) = match std::str::from_utf8(body) {
                Ok(text) => (text, false),
                Err(error) => (
                    std::str::from_utf8(&body[..error.valid_up_to()])
                        .expect("the prefix up to the error is valid UTF-8"),
                    true,
                ),
            };
            for (offset, c) in valid.char_indices() {
                if !sink(c, start + offset + c.len_utf8()) {
                    return false;
                }
            }
            malformed
        }
        Reader::Utf16 { big_endian } => {
            let units = body.len() / 2;
            let odd = body.len() % 2 == 1;
            let last_chunk = utf16_last_chunk_start(body.len());
            let mut index = 0;
            while index < units {
                let unit = |i: usize| {
                    let pair = [body[2 * i], body[2 * i + 1]];
                    if big_endian {
                        u16::from_be_bytes(pair)
                    } else {
                        u16::from_le_bytes(pair)
                    }
                };
                let first = unit(index);
                let (c, used) = match first {
                    0xD800..=0xDBFF if index + 1 < units => {
                        let second = unit(index + 1);
                        if (0xDC00..=0xDFFF).contains(&second) {
                            let code = 0x10000
                                + ((u32::from(first) - 0xD800) << 10)
                                + (u32::from(second) - 0xDC00);
                            (char::from_u32(code).unwrap_or('\u{FFFE}'), 2)
                        } else {
                            ('\u{FFFE}', 1)
                        }
                    }
                    0xD800..=0xDFFF => ('\u{FFFE}', 1),
                    _ => (char::from_u32(u32::from(first)).unwrap_or('\u{FFFE}'), 1),
                };
                index += used;
                if odd && 2 * index > last_chunk {
                    break;
                }
                if !sink(c, start + 2 * index) {
                    return false;
                }
            }
            odd
        }
        Reader::Ucs4 { big_endian } => {
            for (index, quad) in body.as_chunks::<4>().0.iter().enumerate() {
                let quad = *quad;
                let code = if big_endian {
                    u32::from_be_bytes(quad)
                } else {
                    u32::from_le_bytes(quad)
                };
                if !sink(
                    char::from_u32(code).unwrap_or('\u{FFFE}'),
                    start + 4 * (index + 1),
                ) {
                    return false;
                }
            }
            false
        }
        Reader::Java(charset) => {
            let text = charset
                .decode(body)
                .expect("the reader is only chosen for a charset that decodes");
            let end = bytes.len();
            for c in text.chars() {
                if !sink(c, end) {
                    return false;
                }
            }
            false
        }
    }
}

/// Where the read that meets an odd final byte of UTF-16 starts: Xerces reads 128 bytes before
/// the declaration is scanned and 8192 at a time after.
fn utf16_last_chunk_start(length: usize) -> usize {
    if length <= 128 {
        return 0;
    }
    128 + (length - 1 - 128) / 8192 * 8192
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
    position: usize,
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
    /// The characters from the position on.
    fn rest(&self) -> &str {
        &self.text.text[self.position..]
    }

    /// The end of the characters: where the document ends, or the error of the sequence the
    /// reader could not deliver.
    fn text_end(&self) -> Result<Option<char>> {
        if self.text.malformed {
            return Err(XmlError::CharConversion);
        }
        Ok(None)
    }

    fn peek_at(&self, offset: usize) -> Result<Option<char>> {
        match self.rest().chars().nth(offset) {
            Some(c) => Ok(Some(c)),
            None => self.text_end(),
        }
    }

    fn peek(&self) -> Result<Option<char>> {
        match self.rest().chars().next() {
            Some(c) => Ok(Some(c)),
            None => self.text_end(),
        }
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
    fn starts_with(&self, literal: &str) -> Result<bool> {
        debug_assert!(literal.is_ascii());
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
        let skipped = self
            .rest()
            .bytes()
            .take_while(|&b| is_space(char::from(b)))
            .count();
        self.position += skipped;
        if self.position == self.text.text.len() {
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
        if self.position == self.text.text.len() {
            self.text_end()?;
        }
        Ok(())
    }

    /// The length in bytes of the run of name characters at the position.
    fn name_length(&self) -> usize {
        let rest = self.rest();
        let bytes = rest.as_bytes();
        let mut length = 0;
        while length < bytes.len() {
            let byte = bytes[length];
            if byte.is_ascii() {
                if !is_ascii_name_char(byte) {
                    break;
                }
                length += 1;
            } else {
                let c = rest[length..]
                    .chars()
                    .next()
                    .expect("a character starts here");
                if !is_name_char(c) {
                    break;
                }
                length += c.len_utf8();
            }
        }
        length
    }

    /// Where the run of characters Xerces accepts from the position on ends: at the end, at a
    /// character it rejects, or at an ASCII byte `stops` picks.
    fn scan_plain(&self, stops: impl Fn(u8) -> bool) -> usize {
        let text = self.text.text.as_ref();
        let bytes = text.as_bytes();
        let mut at = self.position;
        while at < bytes.len() {
            let byte = bytes[at];
            if byte.is_ascii() {
                if stops(byte) || (byte < 0x20 && !is_space(char::from(byte))) {
                    break;
                }
                at += 1;
            } else {
                let c = text[at..].chars().next().expect("a character starts here");
                if !is_char(c) {
                    break;
                }
                at += c.len_utf8();
            }
        }
        at
    }

    /// Reads up to and past the ASCII `terminator`, each character on the way one Xerces
    /// accepts in `what`, as a loop of [`Parser::skip`] and [`Parser::expect_char`] would; the
    /// characters before the terminator.
    fn until(&mut self, terminator: &str, what: &str) -> Result<std::ops::Range<usize>> {
        let first = terminator.as_bytes()[0];
        let start = self.position;
        loop {
            self.position = self.scan_plain(|byte| byte == first);
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
    fn declaration_follows(&self) -> Result<bool> {
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
            let declaration_end = self.text.text[..self.position].chars().count();
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
        match handler(Event::Comment(&self.text.text[text])) {
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
        match handler(Event::ProcessingInstruction(&target, &self.text.text[data])) {
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
            self.external_subset = self.external_id(true)?;
            self.skip_spaces()?;
        }
        if self.skip("[")? {
            self.internal_subset()?;
            self.skip_spaces()?;
        }
        self.require(">", "the end of the DOCTYPE")
    }

    fn internal_subset(&mut self) -> Result<()> {
        loop {
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
            let plain = self
                .rest()
                .bytes()
                .take_while(|&b| {
                    b.is_ascii() && b >= 0x20 && b != quote as u8 && b != b'<' && b != b'&'
                })
                .count();
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
        let name = self.name("an entity reference")?;
        self.require(";", "; after an entity reference")?;
        Ok(Reference::Entity(name))
    }

    fn predefined(name: &str) -> Option<char> {
        match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "apos" => Some('\''),
            "quot" => Some('"'),
            _ => None,
        }
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
        let emit = |handler: &mut dyn FnMut(Event<'_>) -> Flow,
                    text: &str,
                    kind: TextKind|
         -> std::result::Result<(), Stop> {
            if (text.is_empty() && kind != TextKind::CData)
                || handler(Event::Text(text, kind)) == Flow::Continue
            {
                Ok(())
            } else {
                Err(Stop::Handler)
            }
        };
        loop {
            let data_start = self.position;
            self.character_data()?;
            let data = &self.text.text[data_start..self.position];
            match self.peek()? {
                None => return emit(handler, data, TextKind::Data),
                Some('<') => {
                    emit(handler, data, TextKind::Data)?;
                    if self.rest().as_bytes().get(1).is_some_and(|&byte| {
                        byte == b'/' || (byte.is_ascii() && is_name_start(char::from(byte)))
                    }) {
                        return Ok(());
                    }
                    if self.skip("<!--")? {
                        self.report_comment(handler)?;
                    } else if self.skip("<![CDATA[")? {
                        let section = self.until("]]>", "a CDATA section")?;
                        emit(handler, &self.text.text[section], TextKind::CData)?;
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
                    emit(handler, data, TextKind::Data)?;
                    self.position += 1;
                    let reference = self.reference()?;
                    let mut expansion = String::new();
                    let expanded = self.expand_in_content(reference, &mut expansion, 0);
                    emit(handler, &expansion, TextKind::Reference)?;
                    expanded?;
                }
                Some(_) => unreachable!("character data runs up to markup or a reference"),
            }
        }
    }

    /// Reads a run of character data up to markup, a reference or the end, failing on `]]>`
    /// and on a character Xerces rejects in content.
    fn character_data(&mut self) -> Result<()> {
        loop {
            self.position = self.scan_plain(|byte| matches!(byte, b'<' | b'&' | b']'));
            match self.peek()? {
                None | Some('<' | '&') => return Ok(()),
                Some(_) => {
                    if self.starts_with("]]>")? {
                        return malformed("]]> in content");
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
        let result = read.and_then(|(count, empty, namespace)| {
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

    #[test]
    fn checks_utf8_across_windows_as_whole() {
        let paging = Paging::new(Pages::IN_MEMORY);
        let mut bytes = vec![b'a'; WINDOW + 16];
        bytes[WINDOW - 1..WINDOW + 1].copy_from_slice("\u{e6}".as_bytes());
        let check = Utf8Check::of(&bytes, &paging);
        assert_eq!(
            (check.valid_up_to, check.malformed, check.carriage_return),
            (bytes.len(), false, false)
        );
        bytes[WINDOW + 4] = b'\r';
        bytes[WINDOW + 8] = 0xFF;
        let check = Utf8Check::of(&bytes, &paging);
        assert_eq!(
            (check.valid_up_to, check.malformed, check.carriage_return),
            (WINDOW + 8, true, true)
        );
        assert_eq!(
            check.valid(&bytes),
            std::str::from_utf8(&bytes[..WINDOW + 8]).unwrap()
        );
        let cut = &"a\u{e6}".as_bytes()[..2];
        let check = Utf8Check::of(cut, &paging);
        assert_eq!((check.valid_up_to, check.malformed), (1, true));
    }

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
}
