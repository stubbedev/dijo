//! The `Content-Type` and `Content-Disposition` fields as mime4j's strict field parsers read
//! them for a body's descriptor: the JavaCC grammars `ContentTypeParser` and
//! `ContentDispositionParser`, which keep what they read before an error, and
//! `MimeParameterMapping`, which joins and decodes RFC 2231 parameters.
//!
//! Unlike mime4j, the lexers read every character past ASCII, where JavaCC's tables judge one
//! by its low byte, and a `Content-Disposition` reads `/` as part of a value (#75). Only the
//! parameters RFC 2231 marks as extended are percent-decoded, each in its own charset and with
//! `+` kept, where mime4j URL-decodes later plain values in an earlier one's charset (#76).

use super::codec::{decode_encoded_words, lookup, percent_decode, UrlDecodeError};
use super::field::unfold;
use super::Unported;
use crate::java::JavaHashMap;

/// A token of the grammars' lexers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Eof,
    Cr,
    Lf,
    Slash,
    Semicolon,
    Equals,
    Digits(String),
    Atom(String),
    Quoted(String),
}

/// A character no token or skipped text matches, which JavaCC reports as a `TokenMgrError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LexicalError;

/// A character of an `ATOKEN`: anything but white space and the specials.
fn is_atom(unit: u16) -> bool {
    match unit {
        0..=63 => 0x03FF_6CFA_FFFF_FDFFu64 & (1 << unit) != 0,
        64..=127 => 0xFFFF_FFFF_C7FF_FFFEu64 & (1 << (unit - 64)) != 0,
        _ => true,
    }
}

const SLASH: u16 = 0x2F;

fn is_digit(unit: u16) -> bool {
    (u16::from(b'0')..=u16::from(b'9')).contains(&unit)
}

/// The lexer the two grammars share; only the `Content-Type` one has a `/` token.
struct Lexer {
    units: Vec<u16>,
    pos: usize,
    slash: bool,
    peeked: Option<Result<Token, LexicalError>>,
}

impl Lexer {
    fn new(text: &str, slash: bool) -> Self {
        Lexer {
            units: text.encode_utf16().collect(),
            pos: 0,
            slash,
            peeked: None,
        }
    }

    fn peek(&mut self) -> Result<&Token, LexicalError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex());
        }
        match self.peeked.as_ref().expect("a token was just read") {
            Ok(token) => Ok(token),
            Err(error) => Err(*error),
        }
    }

    fn next(&mut self) -> Result<Token, LexicalError> {
        self.peek()?;
        self.peeked.take().expect("a token was just read")
    }

    fn at(&self, pos: usize) -> Option<u16> {
        self.units.get(pos).copied()
    }

    fn text(&self, from: usize, to: usize) -> String {
        String::from_utf16_lossy(&self.units[from..to])
    }

    fn lex(&mut self) -> Result<Token, LexicalError> {
        loop {
            let Some(unit) = self.at(self.pos) else {
                return Ok(Token::Eof);
            };
            match unit {
                0x20 | 0x09 => self.pos += 1,
                0x28 => {
                    self.pos += 1;
                    self.skip_comment()?;
                }
                0x22 => {
                    self.pos += 1;
                    return self.quoted();
                }
                0x3B => return Ok(self.single(Token::Semicolon)),
                0x3D => return Ok(self.single(Token::Equals)),
                0x2F if self.slash => return Ok(self.single(Token::Slash)),
                0x0D | 0x0A => {
                    let run = self.atom_run(self.pos + 1);
                    if run > self.pos + 1 {
                        return Ok(self.take_atom(run));
                    }
                    let token = if unit == 0x0D { Token::Cr } else { Token::Lf };
                    return Ok(self.single(token));
                }
                _ if self.is_atom(unit) => {
                    let atom = self.atom_run(self.pos);
                    let mut digits = self.pos;
                    while self.at(digits).is_some_and(is_digit) {
                        digits += 1;
                    }
                    if digits > self.pos && digits >= atom {
                        let token = Token::Digits(self.text(self.pos, digits));
                        self.pos = digits;
                        return Ok(token);
                    }
                    return Ok(self.take_atom(atom));
                }
                _ => return Err(LexicalError),
            }
        }
    }

    /// A character of an atom: in a `Content-Disposition`, which has no `/` token, `/` too.
    fn is_atom(&self, unit: u16) -> bool {
        is_atom(unit) || (!self.slash && unit == SLASH)
    }

    fn single(&mut self, token: Token) -> Token {
        self.pos += 1;
        token
    }

    fn atom_run(&self, from: usize) -> usize {
        let mut end = from;
        while self.at(end).is_some_and(|unit| self.is_atom(unit)) {
            end += 1;
        }
        end
    }

    fn take_atom(&mut self, end: usize) -> Token {
        let token = Token::Atom(self.text(self.pos, end));
        self.pos = end;
        token
    }

    /// A comment after its `(`, nested comments and quoted pairs included.
    fn skip_comment(&mut self) -> Result<(), LexicalError> {
        let mut depth = 0usize;
        loop {
            let unit = self.at(self.pos).ok_or(LexicalError)?;
            self.pos += 1;
            match unit {
                0x5C => {
                    if self.at(self.pos).is_some() {
                        self.pos += 1;
                    }
                }
                0x28 => depth += 1,
                0x29 if depth == 0 => return Ok(()),
                0x29 => depth -= 1,
                _ => {}
            }
        }
    }

    /// A quoted string after its opening quote, its quoted pairs unescaped.
    fn quoted(&mut self) -> Result<Token, LexicalError> {
        let mut image = Vec::new();
        loop {
            let unit = self.at(self.pos).ok_or(LexicalError)?;
            self.pos += 1;
            match unit {
                0x22 => return Ok(Token::Quoted(String::from_utf16_lossy(&image))),
                0x5C => {
                    let next = self.at(self.pos).ok_or(LexicalError)?;
                    image.push(next);
                    self.pos += 1;
                }
                _ => image.push(unit),
            }
        }
    }
}

/// Why a grammar stopped before the end of a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParseError;

impl From<LexicalError> for ParseError {
    fn from(_: LexicalError) -> Self {
        ParseError
    }
}

fn expect(lexer: &mut Lexer, wanted: &Token) -> Result<(), ParseError> {
    if lexer.peek()? == wanted {
        lexer.next()?;
        Ok(())
    } else {
        Err(ParseError)
    }
}

fn atom(lexer: &mut Lexer) -> Result<String, ParseError> {
    match lexer.peek()? {
        Token::Atom(_) => match lexer.next()? {
            Token::Atom(text) => Ok(text),
            _ => unreachable!("the token was just peeked"),
        },
        _ => Err(ParseError),
    }
}

/// A parameter's `attribute "=" value`, its value an atom, digits or a quoted string.
fn parameter(lexer: &mut Lexer) -> Result<(String, String), ParseError> {
    let name = atom(lexer)?;
    expect(lexer, &Token::Equals)?;
    let value = match lexer.peek()? {
        Token::Atom(_) | Token::Digits(_) | Token::Quoted(_) => match lexer.next()? {
            Token::Atom(value) | Token::Digits(value) | Token::Quoted(value) => value,
            _ => unreachable!("the token was just peeked"),
        },
        _ => return Err(ParseError),
    };
    Ok((name, value))
}

/// A `Content-Type` field as `ContentTypeFieldImpl` reads its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ContentType {
    /// The type as written, set with the subtype once both are read.
    pub(super) media_type: Option<String>,
    pub(super) subtype: Option<String>,
    /// `type/subtype` in lower case.
    pub(super) mime_type: Option<String>,
    /// The parameters by lower-case name, a later one replacing an earlier.
    parameters: JavaHashMap<String>,
}

impl ContentType {
    pub(super) fn parse(body: &str) -> ContentType {
        let mut lexer = Lexer::new(body, true);
        let mut names_and_values = Vec::new();
        let mut kinds = None;
        let _ = content_type(&mut lexer, &mut kinds, &mut names_and_values);
        let mut parsed = ContentType::default();
        if let Some((media_type, subtype)) = kinds {
            parsed.mime_type = Some(format!("{media_type}/{subtype}").to_lowercase());
            for (name, value) in names_and_values {
                parsed.parameters.insert(&name.to_lowercase(), value);
            }
            parsed.media_type = Some(media_type);
            parsed.subtype = Some(subtype);
        }
        parsed
    }

    pub(super) fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters.get(name).map(String::as_str)
    }

    #[cfg(test)]
    pub(super) fn parameters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.parameters
            .iter_inserted()
            .map(|(name, value)| (name, value.as_str()))
    }
}

fn content_type(
    lexer: &mut Lexer,
    kinds: &mut Option<(String, String)>,
    parameters: &mut Vec<(String, String)>,
) -> Result<(), ParseError> {
    let media_type = atom(lexer)?;
    expect(lexer, &Token::Slash)?;
    let subtype = atom(lexer)?;
    *kinds = Some((media_type, subtype));
    while *lexer.peek()? == Token::Semicolon {
        lexer.next()?;
        if matches!(lexer.peek()?, Token::Atom(_)) {
            parameters.push(parameter(lexer)?);
        }
    }
    expect(lexer, &Token::Eof)
}

/// A `Content-Disposition` field as `ContentDispositionFieldImpl` reads its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ContentDisposition {
    /// The disposition type in lower case, empty when none was read.
    pub(super) kind: String,
    /// The decoded parameters by lower-case name, in the order Java's map lists them.
    pub(super) parameters: JavaHashMap<String>,
}

impl ContentDisposition {
    pub(super) fn parse(body: &str) -> Result<ContentDisposition, Unported> {
        let mut lexer = Lexer::new(body, false);
        let mut kind = None;
        let mut mapping = ParameterMapping::default();
        let _ = content_disposition(&mut lexer, &mut kind, &mut mapping);
        let Some(kind) = kind else {
            return Ok(ContentDisposition::default());
        };
        Ok(ContentDisposition {
            kind: kind.to_lowercase(),
            parameters: mapping.parameters()?.copied(),
        })
    }

    pub(super) fn filename(&self) -> Option<&str> {
        self.parameters.get("filename").map(String::as_str)
    }
}

fn content_disposition(
    lexer: &mut Lexer,
    kind: &mut Option<String>,
    mapping: &mut ParameterMapping,
) -> Result<(), ParseError> {
    *kind = Some(atom(lexer)?);
    while *lexer.peek()? == Token::Semicolon {
        lexer.next()?;
        let (name, value) = parameter(lexer)?;
        mapping.add(&name, value);
    }
    expect(lexer, &Token::Eof)
}

/// How `MimeParameterMapping` files a parameter by the part of its name from the first `*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParameterKind {
    Standard,
    Extended,
    Continuation,
}

/// `MimeParameterMapping`: parameters filed by name, with Java's map and set orders.
#[derive(Debug, Default)]
struct ParameterMapping {
    names: JavaHashMap<()>,
    standard: JavaHashMap<String>,
    extended: JavaHashMap<String>,
    continuation: JavaHashMap<String>,
}

impl ParameterMapping {
    fn add(&mut self, name: &str, value: String) {
        let (name, kind) = match name.find('*') {
            None => (name.to_lowercase(), ParameterKind::Standard),
            Some(star) => {
                let after = &name[star..];
                let kind = if after == "*" {
                    ParameterKind::Extended
                } else if after
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'*' && pair[1].is_ascii_digit())
                {
                    ParameterKind::Continuation
                } else {
                    ParameterKind::Extended
                };
                (name[..star].to_lowercase(), kind)
            }
        };
        self.names.insert_if_absent(&name, ());
        match kind {
            ParameterKind::Extended => self.extended.insert_if_absent(&name, value),
            ParameterKind::Continuation => match self.continuation.get_mut(&name) {
                Some(joined) => joined.push_str(&value),
                None => self.continuation.insert(&name, value),
            },
            ParameterKind::Standard => self.standard.insert_if_absent(&name, value),
        }
    }

    /// `getParameters`: each name's value, in the order of the set of names.
    fn parameters(&mut self) -> Result<JavaHashMap<String>, Unported> {
        let names: Vec<String> = self.names.iter().map(|(name, _)| name.to_owned()).collect();
        let mut parameters = JavaHashMap::default();
        for name in names {
            let value = self.get(&name)?;
            parameters.insert(&name, value);
        }
        Ok(parameters)
    }

    /// `get`: the extended value, the joined continuations or the standard value, the first
    /// that decodes, else the first of them as written in the reverse order.
    fn get(&mut self, name: &str) -> Result<String, Unported> {
        let candidates = [
            (self.extended.get(name).cloned(), ParameterKind::Extended),
            (
                self.continuation.get(name).cloned(),
                ParameterKind::Continuation,
            ),
            (self.standard.get(name).cloned(), ParameterKind::Standard),
        ];
        for (value, kind) in &candidates {
            if let Some(value) = value {
                if let Ok(decoded) = decode(value, *kind)? {
                    return Ok(decoded);
                }
            }
        }
        let [(extended, _), (continuation, _), (standard, _)] = candidates;
        Ok(standard
            .or(continuation)
            .or(extended)
            .expect("every name has a value of one kind"))
    }
}

/// A parameter's value decoded: a standard value's encoded words, and an extended value's
/// `charset'language'text` percent-decoded in its charset, kept as written when the charset is
/// unknown or the value has no such prefix.
fn decode(value: &str, kind: ParameterKind) -> Result<Result<String, UrlDecodeError>, Unported> {
    if kind == ParameterKind::Standard {
        return Ok(Ok(decode_encoded_words(&unfold(value))?));
    }
    let charset_end = value.find('\'');
    let language_end =
        charset_end.and_then(|end| value[end + 1..].find('\'').map(|index| index + end + 1));
    let (Some(charset_end), Some(language_end)) = (charset_end, language_end) else {
        return Ok(Ok(value.to_owned()));
    };
    let charset = &value[..charset_end];
    let text = &value[language_end + 1..];
    if lookup(charset).is_none() {
        return Ok(Ok(text.to_owned()));
    }
    percent_decode(text, charset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_content_types() {
        let parsed = ContentType::parse("Text/Plain; Charset=\"utf-8\" (comment); name=a b");
        assert_eq!(parsed.mime_type.as_deref(), Some("text/plain"));
        assert_eq!(parsed.media_type.as_deref(), Some("Text"));
        assert_eq!(parsed.parameter("charset"), Some("utf-8"));
        assert_eq!(parsed.parameter("name"), Some("a"));
        let digits = ContentType::parse("text/plain; x=123; y=12a");
        assert_eq!(digits.parameter("x"), Some("123"));
        assert_eq!(digits.parameter("y"), Some("12a"));
        assert_eq!(ContentType::parse("text").mime_type, None);
        assert_eq!(ContentType::parse("123/x").mime_type, None);
        let unclosed = ContentType::parse("multipart/mixed; boundary=\"abc");
        assert_eq!(unclosed.mime_type.as_deref(), Some("multipart/mixed"));
        assert_eq!(unclosed.parameter("boundary"), None);
        let wide = ContentType::parse("text/plain; a=1; name=\"Āő\"; b=2");
        assert_eq!(wide.parameter("a"), Some("1"));
        assert_eq!(wide.parameter("name"), Some("Āő"));
        assert_eq!(wide.parameter("b"), Some("2"));
    }

    #[test]
    fn reads_content_dispositions() {
        let parsed = ContentDisposition::parse(
            "Attachment; filename*0*=utf-8''caf%C3%A9; filename*1=.txt; size=12",
        )
        .unwrap();
        assert_eq!(parsed.kind, "attachment");
        assert_eq!(parsed.filename(), Some("café.txt"));
        let slashed = ContentDisposition::parse("inline; filename=a/b.txt").unwrap();
        assert_eq!(slashed.filename(), Some("a/b.txt"));
        let mixed =
            ContentDisposition::parse("attachment; name*=iso-8859-1''%E9; filename=a+b%41.txt")
                .unwrap();
        assert_eq!(mixed.parameters.get("name").map(String::as_str), Some("é"));
        assert_eq!(mixed.filename(), Some("a+b%41.txt"));
        let plus = ContentDisposition::parse("attachment; filename*=utf-8''a+b%20c").unwrap();
        assert_eq!(plus.filename(), Some("a+b c"));
        let encoded =
            ContentDisposition::parse("attachment; filename=\"=?utf-8?q?r=C3=A9sum=C3=A9?=\"")
                .unwrap();
        assert_eq!(encoded.filename(), Some("résumé"));
        assert_eq!(ContentDisposition::parse(";").unwrap().kind, "");
    }
}
