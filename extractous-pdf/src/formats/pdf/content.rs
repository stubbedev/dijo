//! Content streams tokenized as PDFBox's `PDFStreamParser` tokenizes them, leniency included:
//! operands and operators in order, a malformed dictionary or array ending the stream, numbers
//! read with PDFBox's repairs, and an inline image read with its parameters and its data up to
//! its `EI`.

use std::borrow::Cow;

use lopdf::{Dictionary, Object, StringFormat};

use super::Unported;
use crate::java;

/// `BaseParser.MAX_RECURSION_DEPTH`.
const MAX_RECURSION_DEPTH: usize = 500;
/// `BaseParser.MAX_LENGTH_LONG`, the digits of `Long.MAX_VALUE`.
const MAX_BIN_CHAR_TEST_LENGTH: usize = 10;

/// What a content stream holds: an operand, or the operator the operands before it belong to,
/// borrowed from the stream unless it is not ASCII, or the `BI` operator of an inline image.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Token<'a> {
    Operand(Object),
    Operator(Cow<'a, str>),
    InlineImage(Box<InlineImage>),
}

/// The `BI` operator as `PDFStreamParser` reads it: the parameters up to `ID`, and the data `ID`
/// read, none when the parameters did not end at an `ID`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct InlineImage {
    pub(super) parameters: Dictionary,
    pub(super) data: Option<Vec<u8>>,
}

/// The bytes of an operator as Java decodes them, ISO-8859-1.
fn operator(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(text) if bytes.is_ascii() => Cow::Borrowed(text),
        _ => Cow::Owned(java::latin1(bytes)),
    }
}

/// Why a token could not be read: the stream is malformed where PDFBox throws, which the port
/// does not follow through PDFBox's error handling.
fn malformed(what: &str, offset: usize) -> Unported {
    Unported(format!(
        "a content stream is malformed where PDFBox fails on it: {what} at offset {offset}"
    ))
}

/// A content stream being tokenized.
pub(super) struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
    closed: bool,
    depth: usize,
    inline_image_depth: usize,
    /// The data the last `ID` read.
    inline_data: Option<Vec<u8>>,
    /// The bytes of the number being read, kept for the next.
    number: Vec<u8>,
    /// Whether the bytes are a document's own, where `COSParser` reads indirect references,
    /// rather than a content stream, where `PDFStreamParser` stops at one.
    file: bool,
}

/// What `BaseParser.parseDirObject` reads inside a dictionary or an array: an object, the `R` of
/// a reference, or nothing, for what it skips.
enum Direct {
    Object(Object),
    Reference,
    Skipped,
}

/// An object that failed to parse inside a dictionary or an array: PDFBox stops reading the
/// stream there, or it fails.
enum Nested {
    Stop,
    Failed(Unported),
}

impl From<Unported> for Nested {
    fn from(error: Unported) -> Self {
        Self::Failed(error)
    }
}

pub(super) fn is_whitespace(c: Option<u8>) -> bool {
    matches!(c, Some(0 | 9 | 12 | 10 | 13 | 32))
}

pub(super) fn is_end_of_name(c: Option<u8>) -> bool {
    matches!(
        c,
        None | Some(
            b' ' | b'\r'
                | b'\n'
                | 9
                | b'>'
                | b'<'
                | b'['
                | b'/'
                | b']'
                | b')'
                | b'('
                | 0
                | 12
                | b'%'
        )
    )
}

pub(super) fn is_digit(c: Option<u8>) -> bool {
    matches!(c, Some(b'0'..=b'9'))
}

fn is_hex_digit(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

fn hex_value(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

impl<'a> Lexer<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            closed: false,
            depth: 0,
            inline_image_depth: 0,
            inline_data: None,
            number: Vec::new(),
            file: false,
        }
    }

    /// A lexer of a document's own bytes, as `COSParser` reads them from `pos` on: indirect
    /// references are objects there.
    pub(super) fn file(data: &'a [u8], pos: usize) -> Self {
        Self {
            pos: pos.min(data.len()),
            file: true,
            ..Self::new(data)
        }
    }

    pub(super) fn position(&self) -> usize {
        self.pos
    }

    /// `seek`, which stops at the end of the data.
    pub(super) fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }

    /// `COSParser.parseCOSDictionary`, or `None` where it throws.
    pub(super) fn file_dictionary(&mut self) -> Result<Option<Dictionary>, Unported> {
        match self.parse_dictionary() {
            Ok(dict) => Ok(Some(dict)),
            Err(Nested::Stop) => Ok(None),
            Err(Nested::Failed(unported)) => Err(unported),
        }
    }

    /// `BaseParser.parseDirObject` as `COSParser` reads an indirect object: the object, or
    /// `Some(None)` where it reads none, or `None` where it throws. A lone `R` reads as `null`,
    /// a reference to nothing.
    pub(super) fn file_object(&mut self) -> Result<Option<Option<Object>>, Unported> {
        match self.parse_direct_object() {
            Ok(Direct::Object(object)) => Ok(Some(Some(object))),
            Ok(Direct::Reference) => Ok(Some(Some(Object::Null))),
            Ok(Direct::Skipped) => Ok(Some(None)),
            Err(Nested::Stop) => Ok(None),
            Err(Nested::Failed(unported)) => Err(unported),
        }
    }

    pub(super) fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub(super) fn read(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// `rewind(n)`, after reads that may have run past the end.
    pub(super) fn rewind(&mut self, count: usize) {
        self.pos = self.pos.saturating_sub(count);
    }

    /// A read of one byte, undone only when it read one: PDFBox's `if (c != -1) rewind(1)`.
    fn unread(&mut self, c: Option<u8>) {
        if c.is_some() {
            self.rewind(1);
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// `BaseParser.skipSpaces`: whitespace and comments.
    pub(super) fn skip_spaces(&mut self) {
        let mut c = self.read();
        while is_whitespace(c) || c == Some(b'%') {
            if c == Some(b'%') {
                c = self.read();
                while !matches!(c, Some(b'\n' | b'\r') | None) {
                    c = self.read();
                }
            } else {
                c = self.read();
            }
        }
        self.unread(c);
    }

    /// `BaseParser.readString`: the characters up to the end of a name.
    pub(super) fn read_string(&mut self) -> Vec<u8> {
        self.read_word().to_vec()
    }

    /// [`Lexer::read_string`], as the bytes of the data it read.
    fn read_word(&mut self) -> &'a [u8] {
        self.skip_spaces();
        let start = self.pos;
        let mut c = self.read();
        while !is_end_of_name(c) {
            c = self.read();
        }
        self.unread(c);
        &self.data[start..self.pos]
    }

    /// `PDFStreamParser.parseNextToken`: the next token, or `None` at the end of the stream or
    /// where PDFBox stops reading it.
    pub(super) fn next_token(&mut self) -> Result<Option<Token<'a>>, Unported> {
        if self.closed {
            return Ok(None);
        }
        self.skip_spaces();
        let Some(c) = self.peek() else {
            self.closed = true;
            return Ok(None);
        };
        match c {
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    match self.parse_dictionary() {
                        Ok(dict) => Ok(Some(Token::Operand(Object::Dictionary(dict)))),
                        Err(Nested::Stop) => {
                            self.closed = true;
                            Ok(None)
                        }
                        Err(Nested::Failed(error)) => Err(error),
                    }
                } else {
                    self.parse_string().map(|s| Some(Token::Operand(s)))
                }
            }
            b'[' => match self.parse_array() {
                Ok(array) => Ok(Some(Token::Operand(Object::Array(array)))),
                Err(Nested::Stop) => {
                    self.closed = true;
                    Ok(None)
                }
                Err(Nested::Failed(error)) => Err(error),
            },
            b'(' => self.parse_string().map(|s| Some(Token::Operand(s))),
            b'/' => Ok(Some(Token::Operand(self.parse_name()))),
            b'n' => {
                let word = self.read_word();
                Ok(Some(if word == b"null" {
                    Token::Operand(Object::Null)
                } else {
                    Token::Operator(operator(word))
                }))
            }
            b't' | b'f' => {
                let word = self.read_word();
                let boolean = match word {
                    b"true" => Some(true),
                    b"false" => Some(false),
                    _ => None,
                };
                Ok(Some(match boolean {
                    Some(boolean) => Token::Operand(Object::Boolean(boolean)),
                    None => Token::Operator(operator(word)),
                }))
            }
            b'0'..=b'9' | b'-' | b'+' | b'.' => self.stream_number().map(Some),
            b'B' => {
                let word = self.read_word();
                if word == b"BI" {
                    return Ok(Some(Token::InlineImage(Box::new(self.inline_image()?))));
                }
                Ok(Some(Token::Operator(operator(word))))
            }
            b'I' => {
                let first = self.read();
                let second = self.read();
                if (first, second) != (Some(b'I'), Some(b'D')) {
                    self.closed = true;
                    return Err(malformed(
                        "an operator starting with I that is not ID",
                        self.pos,
                    ));
                }
                self.inline_data = Some(self.inline_image_data());
                Ok(Some(Token::Operator(Cow::Borrowed("ID"))))
            }
            b']' => {
                self.read();
                Ok(Some(Token::Operand(Object::Null)))
            }
            _ => {
                let control = |c: char| c <= ' ';
                let operator = match operator(self.read_operator()) {
                    Cow::Borrowed(text) => Cow::Borrowed(text.trim_matches(control)),
                    Cow::Owned(text) if text.trim_matches(control).len() == text.len() => {
                        Cow::Owned(text)
                    }
                    Cow::Owned(text) => Cow::Owned(text.trim_matches(control).to_owned()),
                };
                if operator.is_empty() {
                    return Ok(None);
                }
                Ok(Some(Token::Operator(operator)))
            }
        }
    }

    /// The number `PDFStreamParser` reads where a token starts with a digit, a sign or a dot.
    fn stream_number(&mut self) -> Result<Token<'a>, Unported> {
        let first = self.read().unwrap_or_default();
        let mut buffer = std::mem::take(&mut self.number);
        buffer.clear();
        buffer.push(first);
        if first == b'-' && self.peek() == Some(b'-') {
            self.read();
        }
        let mut dot_not_read = first != b'.';
        loop {
            let c = self.peek();
            let accept = is_digit(c) || (dot_not_read && c == Some(b'.')) || c == Some(b'-');
            if !accept {
                break;
            }
            let c = c.unwrap_or_default();
            if c != b'-' {
                buffer.push(c);
            }
            self.read();
            if dot_not_read && c == b'.' {
                dot_not_read = false;
            }
        }
        let token = if buffer == b"+" {
            Ok(Token::Operand(Object::Null))
        } else {
            number(ascii(&buffer))
                .map(Token::Operand)
                .map_err(|what| malformed(&what, self.pos))
        };
        self.number = buffer;
        token
    }

    /// `PDFStreamParser.readOperator`, as the bytes of the data it read.
    fn read_operator(&mut self) -> &'a [u8] {
        self.skip_spaces();
        let start = self.pos;
        let mut next = self.peek();
        while let Some(c) = next {
            if is_whitespace(next) || matches!(c, b'[' | b'<' | b'(' | b'/' | b'%' | b'0'..=b'9') {
                break;
            }
            self.read();
            next = self.peek();
            if c == b'd' && matches!(next, Some(b'0' | b'1')) {
                self.read();
                next = self.peek();
            }
        }
        &self.data[start..self.pos]
    }

    /// The parameters of an inline image after `BI`, and the data of the `ID` they end at.
    fn inline_image(&mut self) -> Result<InlineImage, Unported> {
        self.inline_image_depth += 1;
        if self.inline_image_depth > 1 {
            return Err(malformed("a nested BI operator", self.pos));
        }
        let mut parameters = Dictionary::new();
        let mut data = None;
        loop {
            match self.next_token()? {
                Some(Token::Operand(Object::Name(key))) => match self.next_token()? {
                    Some(Token::Operand(value)) => parameters.set(key, value),
                    _ => break,
                },
                Some(Token::Operator(name)) => {
                    if name == "ID" {
                        data = self.inline_data.take();
                    }
                    self.inline_image_depth -= 1;
                    break;
                }
                _ => break,
            }
        }
        Ok(InlineImage { parameters, data })
    }

    /// The data of an inline image after `ID`, up to the `EI` that ends it as PDFBox finds it.
    fn inline_image_data(&mut self) -> Vec<u8> {
        if !self.skip_linebreak() && is_whitespace(self.peek()) {
            self.read();
        }
        let mut data = Vec::new();
        let mut last = self.read();
        let mut current = self.read();
        while !(last == Some(b'E')
            && current == Some(b'I')
            && matches!(self.peek(), Some(10 | 13 | 32))
            && self.no_following_binary_data())
            && !self.is_eof()
        {
            data.extend(last);
            last = current;
            current = self.read();
        }
        data
    }

    /// `BaseParser.skipLinebreak`: a CR, an LF or a CR LF.
    fn skip_linebreak(&mut self) -> bool {
        match self.read() {
            Some(b'\r') => {
                let next = self.read();
                if next != Some(b'\n') {
                    self.unread(next);
                }
                true
            }
            Some(b'\n') => true,
            _ => {
                self.rewind(1);
                false
            }
        }
    }

    /// `PDFStreamParser.hasNoFollowingBinData`: whether what follows a candidate `EI` looks like
    /// content stream operators rather than more image data.
    fn no_following_binary_data(&mut self) -> bool {
        let start = self.pos.min(self.data.len());
        let end = (start + MAX_BIN_CHAR_TEST_LENGTH).min(self.data.len());
        let window = &self.data[start..end];
        if window.is_empty() {
            return true;
        }
        let blank = |b: u8| matches!(b, 0 | 9 | 0x20 | 0x0a | 0x0d);
        let mut no_binary = true;
        let mut start_op = None;
        let mut end_op = None;
        for (index, &b) in window.iter().enumerate() {
            if (b != 0 && b < 0x09) || (b > 0x0a && b < 0x20 && b != 0x0d) || b > 0x7f {
                no_binary = false;
                break;
            }
            if start_op.is_none() && !blank(b) {
                start_op = Some(index);
            } else if start_op.is_some() && end_op.is_none() && blank(b) {
                end_op = Some(index);
            }
        }
        let is_number = |s: &[u8]| {
            let mut dots = 0;
            s.iter().all(|&c| {
                if c == b'.' {
                    dots += 1;
                    dots == 1
                } else {
                    c.is_ascii_digit()
                }
            })
        };
        let mut word: &[u8] = &[];
        if let (true, Some(from), Some(to)) = (no_binary, start_op, end_op) {
            word = &window[from..to];
            if word != b"Q" && word != b"EMC" && word != b"S" && !is_number(word) {
                no_binary = false;
            }
        }
        if let (true, Some(from)) = (no_binary, start_op) {
            if window.len() == MAX_BIN_CHAR_TEST_LENGTH {
                let to = end_op.unwrap_or(MAX_BIN_CHAR_TEST_LENGTH);
                if end_op.is_none() {
                    word = &window[from..to];
                }
                if to - from > 3 && !is_number(word) {
                    no_binary = false;
                }
            }
        }
        no_binary
    }

    /// `BaseParser.parseCOSName`.
    fn parse_name(&mut self) -> Object {
        self.read();
        let mut buffer = Vec::new();
        let mut c = self.read();
        while !is_end_of_name(c) {
            let ch = c.unwrap_or_default();
            if ch == b'#' {
                let c1 = self.read();
                let c2 = self.read();
                match (c1, c2) {
                    (Some(h1), Some(h2)) if is_hex_digit(h1) && is_hex_digit(h2) => {
                        buffer.push(hex_value(h1) * 16 + hex_value(h2));
                        c = self.read();
                    }
                    (None, _) | (_, None) => {
                        c = None;
                        break;
                    }
                    _ => {
                        self.rewind(1);
                        c = c1;
                        buffer.push(ch);
                    }
                }
            } else {
                buffer.push(ch);
                c = self.read();
            }
        }
        self.unread(c);
        Object::Name(buffer)
    }

    /// `BaseParser.parseCOSString`: a literal or hexadecimal string.
    fn parse_string(&mut self) -> Result<Object, Unported> {
        match self.read() {
            Some(b'<') => return self.parse_hex_string(),
            Some(b'(') => {}
            _ => {
                return Err(malformed(
                    "a string that starts with neither ( nor <",
                    self.pos,
                ))
            }
        }
        let mut out = Vec::new();
        let mut braces = 1;
        let mut c = self.read();
        while braces > 0 {
            let Some(ch) = c else { break };
            let mut next_c: Option<Option<u8>> = None;
            match ch {
                b')' => {
                    braces -= 1;
                    braces = self.check_for_end_of_string(braces);
                    if braces != 0 {
                        out.push(ch);
                    }
                }
                b'(' => {
                    braces += 1;
                    out.push(ch);
                }
                b'\\' => {
                    let next = self.read();
                    match next {
                        Some(b'n') => out.push(b'\n'),
                        Some(b'r') => out.push(b'\r'),
                        Some(b't') => out.push(b'\t'),
                        Some(b'b') => out.push(8),
                        Some(b'f') => out.push(12),
                        Some(b')') => {
                            braces = self.check_for_end_of_string(braces);
                            out.push(if braces != 0 { b')' } else { b'\\' });
                        }
                        Some(b'(' | b'\\') => out.push(next.unwrap_or_default()),
                        Some(10 | 13) => {
                            let mut after = self.read();
                            while matches!(after, Some(10 | 13)) {
                                after = self.read();
                            }
                            next_c = Some(after);
                        }
                        Some(digit @ b'0'..=b'7') => {
                            let mut value = u32::from(digit - b'0');
                            let second = self.read();
                            match second {
                                Some(d @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(d - b'0');
                                    let third = self.read();
                                    match third {
                                        Some(d @ b'0'..=b'7') => {
                                            value = value * 8 + u32::from(d - b'0')
                                        }
                                        other => next_c = Some(other),
                                    }
                                }
                                other => next_c = Some(other),
                            }
                            out.push(value as u8);
                        }
                        Some(other) => out.push(other),
                        None => out.push(0xFF),
                    }
                }
                other => out.push(other),
            }
            c = match next_c {
                Some(next) => next,
                None => self.read(),
            };
        }
        self.unread(c);
        Ok(Object::String(out, StringFormat::Literal))
    }

    /// `BaseParser.checkForEndOfString`: a `)` that is followed by a line break and a `/` or
    /// `>` ends the string whatever the count of open parentheses.
    fn check_for_end_of_string(&self, braces: i32) -> i32 {
        if braces == 0 {
            return 0;
        }
        let next = &self.data[self.pos.min(self.data.len())..];
        if next.len() < 3 {
            return braces;
        }
        let ends = (matches!(next[0], 13 | 10) && matches!(next[1], b'/' | b'>'))
            || (next[0] == 13 && next[1] == 10 && matches!(next[2], b'/' | b'>'));
        if ends {
            0
        } else {
            braces
        }
    }

    /// `BaseParser.parseCOSHexString` and `COSString.parseHex`.
    fn parse_hex_string(&mut self) -> Result<Object, Unported> {
        let mut digits = Vec::new();
        loop {
            match self.read() {
                Some(c) if is_hex_digit(c) => digits.push(c),
                Some(b'>') => break,
                None => return Err(malformed("an unclosed hexadecimal string", self.pos)),
                Some(b' ' | b'\n' | b'\t' | b'\r' | 8 | 12) => {}
                Some(_) => {
                    if digits.len() % 2 != 0 {
                        digits.pop();
                    }
                    loop {
                        match self.read() {
                            Some(b'>') => break,
                            None => {
                                return Err(malformed("an unclosed hexadecimal string", self.pos))
                            }
                            Some(_) => {}
                        }
                    }
                    break;
                }
            }
        }
        let mut bytes: Vec<u8> = digits
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[high, low]| hex_value(*high) * 16 + hex_value(*low))
            .collect();
        if digits.len() % 2 != 0 {
            bytes.push(hex_value(digits[digits.len() - 1]) * 16);
        }
        Ok(Object::String(bytes, StringFormat::Hexadecimal))
    }

    fn enter(&mut self) -> Result<(), Nested> {
        self.depth += 1;
        if self.depth > MAX_RECURSION_DEPTH {
            self.depth -= 1;
            return Err(Nested::Stop);
        }
        Ok(())
    }

    /// `BaseParser.parseCOSDictionary(true)`, where an error stops the stream.
    fn parse_dictionary(&mut self) -> Result<Dictionary, Nested> {
        self.enter()?;
        let result = self.dictionary_body();
        self.depth -= 1;
        result
    }

    fn dictionary_body(&mut self) -> Result<Dictionary, Nested> {
        if self.read() != Some(b'<') || self.read() != Some(b'<') {
            return Err(Nested::Stop);
        }
        self.skip_spaces();
        let mut dict = Dictionary::new();
        loop {
            self.skip_spaces();
            match self.peek() {
                Some(b'>') => break,
                Some(b'/') => {
                    let Object::Name(key) = self.parse_name() else {
                        unreachable!("parse_name returns a name")
                    };
                    let value = self.dictionary_value()?;
                    self.skip_spaces();
                    match value {
                        None => return Ok(dict),
                        Some(Object::Integer(i64::MAX | i64::MIN)) => {}
                        Some(value) => dict.set(key, value),
                    }
                }
                _ => {
                    if self.read_until_end_of_dictionary() {
                        return Ok(dict);
                    }
                }
            }
        }
        if self.read() == Some(b'>') {
            self.read();
        }
        Ok(dict)
    }

    /// `BaseParser.parseCOSDictionaryValue`: a reference, which a content stream cannot hold,
    /// stops the stream.
    fn dictionary_value(&mut self) -> Result<Option<Object>, Nested> {
        let value = match self.parse_direct_object()? {
            Direct::Object(object) => object,
            Direct::Reference => Object::Null,
            Direct::Skipped => return Ok(None),
        };
        self.skip_spaces();
        let is_number = matches!(value, Object::Integer(_) | Object::Real(_));
        if !is_number || !is_digit(self.peek()) {
            return Ok(Some(value));
        }
        if !self.file {
            return Err(Nested::Stop);
        }
        let generation = match self.parse_direct_object()? {
            Direct::Object(object) => Some(object),
            Direct::Reference | Direct::Skipped => None,
        };
        self.skip_spaces();
        if self.read() != Some(b'R') {
            return Err(Nested::Stop);
        }
        let (Object::Integer(number), Some(Object::Integer(generation))) = (value, generation)
        else {
            return Ok(Some(Object::Null));
        };
        if number <= 0 || (generation as i32) < 0 {
            return Ok(Some(Object::Null));
        }
        reference(number, generation as i32).map(Some)
    }

    /// `BaseParser.readUntilEndOfCOSDictionary`: whether the stream ran out or the object ended.
    fn read_until_end_of_dictionary(&mut self) -> bool {
        let mut c = self.read();
        while !matches!(c, None | Some(b'/' | b'>')) {
            if c == Some(b'e') {
                c = self.read();
                if c == Some(b'n') {
                    c = self.read();
                    if c == Some(b'd') {
                        c = self.read();
                        let is_stream = c == Some(b's')
                            && self.read() == Some(b't')
                            && self.read() == Some(b'r')
                            && self.read() == Some(b'e')
                            && self.read() == Some(b'a')
                            && self.read() == Some(b'm');
                        let is_obj = !is_stream
                            && c == Some(b'o')
                            && self.read() == Some(b'b')
                            && self.read() == Some(b'j');
                        if is_stream || is_obj {
                            return true;
                        }
                    }
                }
            }
            c = self.read();
        }
        if c.is_none() {
            return true;
        }
        self.rewind(1);
        false
    }

    /// `BaseParser.parseCOSArray`, where a reference or an error stops the stream.
    fn parse_array(&mut self) -> Result<Vec<Object>, Nested> {
        self.enter()?;
        let result = self.array_body();
        self.depth -= 1;
        result
    }

    fn array_body(&mut self) -> Result<Vec<Object>, Nested> {
        self.read();
        let mut array = Vec::new();
        self.skip_spaces();
        while let Some(c) = self.peek() {
            if c == b']' || c == 0 {
                break;
            }
            let element = match self.parse_direct_object()? {
                Direct::Reference => {
                    if let (true, Some(&Object::Integer(generation))) =
                        (array.len() > 1, array.last())
                    {
                        array.pop();
                        if let Some(&Object::Integer(number)) = array.last() {
                            array.pop();
                            if number >= 0 && generation as i32 >= 0 {
                                if !self.file {
                                    return Err(Nested::Stop);
                                }
                                array.push(reference(number, generation as i32)?);
                                self.skip_spaces();
                                continue;
                            }
                        }
                    }
                    Direct::Skipped
                }
                other => other,
            };
            match element {
                Direct::Object(object) => array.push(object),
                Direct::Reference | Direct::Skipped => {
                    let word = self.read_string();
                    if word.is_empty() && self.peek() == Some(b'[') {
                        return Ok(array);
                    }
                    self.rewind(word.len());
                    if word == b"endobj" || word == b"endstream" {
                        return Ok(array);
                    }
                }
            }
            self.skip_spaces();
        }
        self.read();
        self.skip_spaces();
        Ok(array)
    }

    /// `BaseParser.parseDirObject` as `PDFStreamParser` runs it: `None` for what it skips, and
    /// a reference stops the stream.
    fn parse_direct_object(&mut self) -> Result<Direct, Nested> {
        self.enter()?;
        let result = self.direct_object();
        self.depth -= 1;
        result
    }

    fn direct_object(&mut self) -> Result<Direct, Nested> {
        self.skip_spaces();
        let Some(c) = self.peek() else {
            return Ok(Direct::Skipped);
        };
        let object = match c {
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    Object::Dictionary(self.parse_dictionary()?)
                } else {
                    self.parse_string()?
                }
            }
            b'[' => Object::Array(self.parse_array()?),
            b'(' => self.parse_string()?,
            b'/' => self.parse_name(),
            b'n' => self.expect_word(b"null").map(|()| Object::Null)?,
            b't' => self.expect_word(b"true").map(|()| Object::Boolean(true))?,
            b'f' => self
                .expect_word(b"false")
                .map(|()| Object::Boolean(false))?,
            b'R' => {
                self.read();
                return Ok(Direct::Reference);
            }
            b'0'..=b'9' | b'-' | b'+' | b'.' => self.direct_number()?,
            _ => {
                let word = self.read_string();
                if word.is_empty() {
                    return Err(Nested::Stop);
                }
                if word == b"endobj" || word == b"endstream" {
                    self.rewind(word.len());
                    return Ok(Direct::Skipped);
                }
                return Ok(match self.file {
                    true => Direct::Object(Object::Null),
                    false => Direct::Skipped,
                });
            }
        };
        Ok(Direct::Object(object))
    }

    /// `BaseParser.readExpectedString`, which throws where the word differs: the stream stops.
    fn expect_word(&mut self, word: &[u8]) -> Result<(), Nested> {
        self.skip_spaces();
        for &expected in word {
            if self.read() != Some(expected) {
                return Err(Nested::Stop);
            }
        }
        self.skip_spaces();
        Ok(())
    }

    /// `BaseParser.parseCOSNumber`, inside a dictionary or an array.
    fn direct_number(&mut self) -> Result<Object, Nested> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'E' | b'e'))
        {
            self.pos += 1;
        }
        if matches!(self.data[start..self.pos].last(), Some(b'e' | b'E')) {
            self.rewind(1);
        }
        number(ascii(&self.data[start..self.pos])).map_err(|_| Nested::Stop)
    }
}

/// `BaseParser.parseCOSString` of a literal string at the start of `bytes`: its bytes, or
/// `None` where PDFBox fails to read one.
pub(super) fn parse_literal(bytes: &[u8]) -> Option<Vec<u8>> {
    match Lexer::new(bytes).parse_string() {
        Ok(Object::String(string, _)) => Some(string),
        _ => None,
    }
}

/// The reference `COSParser` reads for the object `number` of `generation`, which `lopdf` holds
/// in fewer bits than PDFBox.
fn reference(number: i64, generation: i32) -> Result<Object, Nested> {
    match (u32::try_from(number), u16::try_from(generation)) {
        (Ok(number), Ok(generation)) => Ok(Object::Reference((number, generation))),
        _ => Err(Nested::Failed(Unported(format!(
            "a reference to object {number} {generation}, which lopdf cannot hold"
        )))),
    }
}

/// The text of the bytes of a number, which are ASCII.
fn ascii(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("a number is read from ASCII bytes")
}

/// `COSNumber.get`: an integer or a real, repaired as PDFBox repairs malformed numbers; an
/// integer out of the range of a Java `long` saturates, as PDFBox marks it.
pub(super) fn number(text: &str) -> Result<Object, String> {
    if text.len() == 1 {
        let c = text.as_bytes()[0];
        return match c {
            b'0'..=b'9' => Ok(Object::Integer(i64::from(c - b'0'))),
            b'-' | b'.' => Ok(Object::Integer(0)),
            _ => Err(format!("not a number: {text}")),
        };
    }
    if text.contains(['.', 'e']) {
        return real(text).map(Object::Real);
    }
    if let Some(value) = crate::java::parse_long(text) {
        if text
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b'-' || c == b'+')
        {
            return Ok(Object::Integer(value));
        }
    }
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(format!("not a number: {text}"));
    }
    Ok(Object::Integer(if text.starts_with('-') {
        i64::MIN
    } else {
        i64::MAX
    }))
}

/// `new COSFloat(String)`: `Float.parseFloat`, with PDFBox's repairs of doubled and misplaced
/// minus signs, and its coercion of infinities and values too small for a normal `float`.
fn real(text: &str) -> Result<f32, String> {
    if let Some(value) = parse_java_float(text) {
        return Ok(coerce(value));
    }
    let repaired = if let Some(rest) = text.strip_prefix("--") {
        format!("-{rest}")
    } else if is_zero_dot_zeros_minus_digits(text) {
        format!("-{}", text.replacen('-', "", 1))
    } else if is_minus_digits_dot_minus_digits(text) {
        format!("-{}", text.replace('-', ""))
    } else {
        return Err(format!("not a floating point number: {text}"));
    };
    parse_java_float(&repaired)
        .map(coerce)
        .ok_or_else(|| format!("not a floating point number: {text}"))
}

/// `^0\.0*-\d+`, found at the start.
fn is_zero_dot_zeros_minus_digits(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("0.") else {
        return false;
    };
    let rest = rest.trim_start_matches('0');
    rest.strip_prefix('-')
        .is_some_and(|digits| digits.starts_with(|c: char| c.is_ascii_digit()))
}

/// `^-\d+\.-\d+`, found at the start.
fn is_minus_digits_dot_minus_digits(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('-') else {
        return false;
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return false;
    }
    rest[digits..]
        .strip_prefix(".-")
        .is_some_and(|tail| tail.starts_with(|c: char| c.is_ascii_digit()))
}

/// `Float.parseFloat` for the characters PDFBox's number readers collect: an optional sign,
/// digits with at most one dot, and an optional exponent.
fn parse_java_float(text: &str) -> Option<f32> {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let mantissa = body.split(['e', 'E']).next().unwrap_or_default();
    if mantissa.is_empty() || mantissa == "." {
        return None;
    }
    text.parse::<f32>().ok()
}

/// `COSFloat.coerce`.
fn coerce(value: f32) -> f32 {
    if value == f32::INFINITY {
        f32::MAX
    } else if value == f32::NEG_INFINITY {
        -f32::MAX
    } else if value.abs() < f32::MIN_POSITIVE {
        0.0
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(data: &[u8]) -> Vec<Token<'_>> {
        let mut lexer = Lexer::new(data);
        let mut tokens = Vec::new();
        while let Some(token) = lexer.next_token().unwrap() {
            tokens.push(token);
        }
        tokens
    }

    fn op(name: &str) -> Token<'_> {
        Token::Operator(Cow::Borrowed(name))
    }

    #[test]
    fn reads_operands_and_operators() {
        assert_eq!(
            tokens(b"BT /F1 12 Tf 1 0 0 1 72.5 -.5 Tm (Hi\\051) Tj [(a) -120 <414>] TJ ET"),
            vec![
                op("BT"),
                Token::Operand(Object::Name(b"F1".to_vec())),
                Token::Operand(Object::Integer(12)),
                op("Tf"),
                Token::Operand(Object::Integer(1)),
                Token::Operand(Object::Integer(0)),
                Token::Operand(Object::Integer(0)),
                Token::Operand(Object::Integer(1)),
                Token::Operand(Object::Real(72.5)),
                Token::Operand(Object::Real(-0.5)),
                op("Tm"),
                Token::Operand(Object::String(b"Hi)".to_vec(), StringFormat::Literal)),
                op("Tj"),
                Token::Operand(Object::Array(vec![
                    Object::String(b"a".to_vec(), StringFormat::Literal),
                    Object::Integer(-120),
                    Object::String(b"A@".to_vec(), StringFormat::Hexadecimal),
                ])),
                op("TJ"),
                op("ET"),
            ]
        );
    }

    #[test]
    fn repairs_numbers_as_pdfbox_does() {
        assert_eq!(number("--16.33"), Ok(Object::Real(-16.33)));
        assert_eq!(number("0.00-33"), Ok(Object::Real(-0.0033)));
        assert_eq!(number("-12.-1"), Ok(Object::Real(-12.1)));
        assert_eq!(number("-"), Ok(Object::Integer(0)));
        assert_eq!(
            number("99999999999999999999"),
            Ok(Object::Integer(i64::MAX))
        );
        assert!(number("-.").is_err());
        assert_eq!(
            tokens(b"5-3 Td"),
            vec![Token::Operand(Object::Integer(53)), op("Td")]
        );
        assert_eq!(tokens(b"+ x"), vec![Token::Operand(Object::Null), op("x")]);
    }

    #[test]
    fn reads_marked_content_properties_and_inline_images() {
        assert_eq!(
            tokens(b"/P <</MCID 3>> BDC BI /W 2 /H 1 ID \x00\xff EI Q EMC"),
            vec![
                Token::Operand(Object::Name(b"P".to_vec())),
                Token::Operand(Object::Dictionary(Dictionary::from_iter(vec![(
                    "MCID",
                    Object::Integer(3)
                )]))),
                op("BDC"),
                Token::InlineImage(Box::new(InlineImage {
                    parameters: Dictionary::from_iter(vec![
                        ("W", Object::Integer(2)),
                        ("H", Object::Integer(1)),
                    ]),
                    data: Some(vec![0x00, 0xff, b' ']),
                })),
                op("Q"),
                op("EMC"),
            ]
        );
    }

    #[test]
    fn a_reference_in_an_array_ends_the_stream() {
        assert_eq!(tokens(b"q [1 0 R] TJ Q"), vec![op("q")]);
    }

    #[test]
    fn type3_operators_keep_their_digit() {
        assert_eq!(
            tokens(b"0 0 d0"),
            vec![
                Token::Operand(Object::Integer(0)),
                Token::Operand(Object::Integer(0)),
                op("d0")
            ]
        );
    }
}
