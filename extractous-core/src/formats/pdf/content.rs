//! Content streams tokenized as PDFBox's `PDFStreamParser` tokenizes them, leniency included:
//! operands and operators in order, a malformed dictionary or array ending the stream, numbers
//! read with PDFBox's repairs, and inline image data skipped up to its `EI`.

use lopdf::{Dictionary, Object, StringFormat};

use super::Unported;

/// `BaseParser.MAX_RECURSION_DEPTH`.
const MAX_RECURSION_DEPTH: usize = 500;
/// `BaseParser.MAX_LENGTH_LONG`, the digits of `Long.MAX_VALUE`.
const MAX_BIN_CHAR_TEST_LENGTH: usize = 10;

/// What a content stream holds: an operand, or the operator the operands before it belong to.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Token {
    Operand(Object),
    Operator(String),
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

fn is_whitespace(c: Option<u8>) -> bool {
    matches!(c, Some(0 | 9 | 12 | 10 | 13 | 32))
}

fn is_end_of_name(c: Option<u8>) -> bool {
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

fn is_digit(c: Option<u8>) -> bool {
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
        }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn read(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// `rewind(n)`, after reads that may have run past the end.
    fn rewind(&mut self, count: usize) {
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
    fn skip_spaces(&mut self) {
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
    fn read_string(&mut self) -> Vec<u8> {
        self.skip_spaces();
        let mut buffer = Vec::new();
        let mut c = self.read();
        while !is_end_of_name(c) {
            buffer.push(c.unwrap_or_default());
            c = self.read();
        }
        self.unread(c);
        buffer
    }

    /// `PDFStreamParser.parseNextToken`: the next token, or `None` at the end of the stream or
    /// where PDFBox stops reading it.
    pub(super) fn next_token(&mut self) -> Result<Option<Token>, Unported> {
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
                let word = self.read_string();
                Ok(Some(if word == b"null" {
                    Token::Operand(Object::Null)
                } else {
                    Token::Operator(latin1(&word))
                }))
            }
            b't' | b'f' => {
                let word = self.read_string();
                Ok(Some(match word.as_slice() {
                    b"true" => Token::Operand(Object::Boolean(true)),
                    b"false" => Token::Operand(Object::Boolean(false)),
                    _ => Token::Operator(latin1(&word)),
                }))
            }
            b'0'..=b'9' | b'-' | b'+' | b'.' => self.stream_number().map(Some),
            b'B' => {
                let word = self.read_string();
                if word == b"BI" {
                    self.inline_image()?;
                }
                Ok(Some(Token::Operator(latin1(&word))))
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
                self.skip_inline_image_data();
                Ok(Some(Token::Operator("ID".to_owned())))
            }
            b']' => {
                self.read();
                Ok(Some(Token::Operand(Object::Null)))
            }
            _ => {
                let operator = self.read_operator();
                let trimmed = latin1(&operator)
                    .trim_matches(|c: char| c <= ' ')
                    .to_owned();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                Ok(Some(Token::Operator(trimmed)))
            }
        }
    }

    /// The number `PDFStreamParser` reads where a token starts with a digit, a sign or a dot.
    fn stream_number(&mut self) -> Result<Token, Unported> {
        let first = self.read().unwrap_or_default();
        let mut buffer = vec![first];
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
        if buffer == b"+" {
            return Ok(Token::Operand(Object::Null));
        }
        number(&latin1(&buffer))
            .map(Token::Operand)
            .map_err(|what| malformed(&what, self.pos))
    }

    /// `PDFStreamParser.readOperator`.
    fn read_operator(&mut self) -> Vec<u8> {
        self.skip_spaces();
        let mut buffer = Vec::new();
        let mut next = self.peek();
        while let Some(c) = next {
            if is_whitespace(next) || matches!(c, b'[' | b'<' | b'(' | b'/' | b'%' | b'0'..=b'9') {
                break;
            }
            self.read();
            next = self.peek();
            buffer.push(c);
            if c == b'd' && matches!(next, Some(b'0' | b'1')) {
                buffer.push(self.read().unwrap_or_default());
                next = self.peek();
            }
        }
        buffer
    }

    /// The parameters of an inline image after `BI`, up to its data, which `ID` skips.
    fn inline_image(&mut self) -> Result<(), Unported> {
        self.inline_image_depth += 1;
        if self.inline_image_depth > 1 {
            return Err(malformed("a nested BI operator", self.pos));
        }
        loop {
            match self.next_token()? {
                Some(Token::Operand(Object::Name(_))) => match self.next_token()? {
                    Some(Token::Operand(_)) => {}
                    _ => break,
                },
                Some(Token::Operator(_)) => {
                    self.inline_image_depth -= 1;
                    break;
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// The data of an inline image after `ID`, up to the `EI` that ends it as PDFBox finds it.
    fn skip_inline_image_data(&mut self) {
        if !self.skip_linebreak() && is_whitespace(self.peek()) {
            self.read();
        }
        let mut last = self.read();
        let mut current = self.read();
        while !(last == Some(b'E')
            && current == Some(b'I')
            && matches!(self.peek(), Some(10 | 13 | 32))
            && self.no_following_binary_data())
            && !self.is_eof()
        {
            last = current;
            current = self.read();
        }
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
        Err(Nested::Stop)
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
                                return Err(Nested::Stop);
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
                }
                return Ok(Direct::Skipped);
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
        let mut buffer = Vec::new();
        let mut c = self.read();
        while let Some(ch) = c {
            if !(ch.is_ascii_digit() || matches!(ch, b'-' | b'+' | b'.' | b'E' | b'e')) {
                break;
            }
            buffer.push(ch);
            c = self.read();
        }
        self.unread(c);
        if matches!(buffer.last(), Some(b'e' | b'E')) {
            buffer.pop();
            self.rewind(1);
        }
        number(&latin1(&buffer)).map_err(|_| Nested::Stop)
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

/// Bytes read as ISO-8859-1, as PDFBox's parsers read operator names.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
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

    fn tokens(data: &[u8]) -> Vec<Token> {
        let mut lexer = Lexer::new(data);
        let mut tokens = Vec::new();
        while let Some(token) = lexer.next_token().unwrap() {
            tokens.push(token);
        }
        tokens
    }

    fn op(name: &str) -> Token {
        Token::Operator(name.to_owned())
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
    fn reads_marked_content_properties_and_skips_inline_images() {
        assert_eq!(
            tokens(b"/P <</MCID 3>> BDC BI /W 2 /H 1 ID \x00\xff EI Q EMC"),
            vec![
                Token::Operand(Object::Name(b"P".to_vec())),
                Token::Operand(Object::Dictionary(Dictionary::from_iter(vec![(
                    "MCID",
                    Object::Integer(3)
                )]))),
                op("BDC"),
                op("BI"),
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
