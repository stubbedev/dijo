//! FontBox's `Type1Parser`, `Type1Lexer` and `PfbParser`, with the parts of `PDType1Font` that
//! cut a `FontFile` stream into its two segments: a Type 1 font program as PDFBox reads it from
//! a document, failing where FontBox fails. Text extraction reads its built-in encoding, font
//! matrix, bounding box, glyph names, and the widths its charstrings set.

use super::Unported;
use crate::hash::FastMap;

const EEXEC_KEY: u32 = 55665;
const CHARSTRING_KEY: u32 = 4330;
const PFB_START_MARKER: u8 = 0x80;
/// The most entries a count in a font program may announce before the port stops following it
/// where Java would allocate them all.
const MAX_ANNOUNCED: i32 = 1 << 20;

/// What reading a `FontFile` gives PDFBox.
#[derive(Debug)]
pub(super) enum Read {
    Font(Box<Type1>),
    /// No program: a segment is empty, which PDFBox takes for a font that is not embedded.
    Absent,
    /// The program fails to parse, which makes PDFBox call the font damaged.
    Damaged,
    Unported(String),
}

/// A font program's built-in encoding.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum BuiltIn {
    Standard,
    /// The codes and names of an `Encoding` array, in the order they are put.
    Custom(Vec<(i32, String)>),
}

/// A parsed Type 1 font program.
#[derive(Debug)]
pub(super) struct Type1 {
    pub(super) encoding: Option<BuiltIn>,
    pub(super) font_matrix: Vec<f32>,
    pub(super) font_bbox: Vec<f32>,
    charstrings: FastMap<String, Vec<u8>>,
}

/// Why parsing stopped: an `IOException`, which FontBox reports as a damaged font, a
/// `NumberFormatException`, which it reports so only while reading the clear text, or an
/// exception PDFBox does not catch.
enum Failure {
    Damaged,
    Number,
    Uncaught(&'static str),
}

type Parsed<T> = Result<T, Failure>;

/// `(char) byte`: Java widens a signed byte, so a byte from 0x80 becomes U+FF80 and up.
fn java_char(byte: u8) -> char {
    if byte < 0x80 {
        char::from(byte)
    } else {
        char::from_u32(0xFF00 | u32::from(byte)).unwrap_or_else(|| unreachable!())
    }
}

/// `Character.isWhitespace` of a widened byte.
fn is_whitespace(c: char) -> bool {
    crate::unicode::is_whitespace(c)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    String,
    Name,
    Literal,
    Real,
    Integer,
    StartArray,
    EndArray,
    StartProc,
    EndProc,
    StartDict,
    EndDict,
    CharString,
}

#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    text: Option<String>,
    data: Vec<u8>,
}

impl Token {
    fn text(kind: Kind, text: String) -> Self {
        Self {
            kind,
            text: Some(text),
            data: Vec::new(),
        }
    }

    fn is(&self, kind: Kind, text: &str) -> bool {
        self.kind == kind && self.text.as_deref() == Some(text)
    }

    /// `Float.parseFloat` of the token's text.
    fn float_value(&self) -> Result<f32, Failure> {
        let text = self.text.as_deref().ok_or(Failure::Uncaught(
            "a font program whose charstring is read as a number, which FontBox fails on",
        ))?;
        java_parse_float(text).ok_or(Failure::Number)
    }

    /// `(int) Float.parseFloat` of the token's text.
    fn int_value(&self) -> Result<i32, Failure> {
        self.float_value().map(|value| value as i32)
    }
}

/// `Float.parseFloat` of a token: an optionally signed decimal, with an exponent.
fn java_parse_float(text: &str) -> Option<f32> {
    let digits = text.trim_start_matches(['+', '-']);
    let valid = !digits.is_empty()
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
        && digits.chars().any(|c| c.is_ascii_digit());
    if !valid {
        return None;
    }
    text.parse::<f32>().ok()
}

/// `Type1Lexer`.
struct Lexer<'a> {
    bytes: &'a [u8],
    position: usize,
    ahead: Option<Token>,
    open_parens: i32,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8]) -> Parsed<Self> {
        let mut lexer = Self {
            bytes,
            position: 0,
            ahead: None,
            open_parens: 0,
        };
        lexer.ahead = lexer.read_token(None)?;
        Ok(lexer)
    }

    fn next_token(&mut self) -> Parsed<Option<Token>> {
        let current = self.ahead.take();
        self.ahead = self.read_token(current.as_ref())?;
        Ok(current)
    }

    fn peek(&self) -> Option<&Token> {
        self.ahead.as_ref()
    }

    fn peek_kind(&self, kind: Kind) -> bool {
        self.ahead.as_ref().is_some_and(|token| token.kind == kind)
    }

    fn has_remaining(&self) -> bool {
        self.position < self.bytes.len()
    }

    fn get_char(&mut self) -> Parsed<char> {
        let byte = *self.bytes.get(self.position).ok_or(Failure::Damaged)?;
        self.position += 1;
        Ok(java_char(byte))
    }

    fn read_token(&mut self, previous: Option<&Token>) -> Parsed<Option<Token>> {
        while self.has_remaining() {
            let c = self.get_char()?;
            match c {
                '%' => self.read_comment(),
                '(' => return self.read_string(),
                ')' => return Err(Failure::Damaged),
                '[' => return Ok(Some(Token::text(Kind::StartArray, "[".to_owned()))),
                '{' => return Ok(Some(Token::text(Kind::StartProc, "{".to_owned()))),
                ']' => return Ok(Some(Token::text(Kind::EndArray, "]".to_owned()))),
                '}' => return Ok(Some(Token::text(Kind::EndProc, "}".to_owned()))),
                '/' => {
                    let regular = self.read_regular().ok_or(Failure::Damaged)?;
                    return Ok(Some(Token::text(Kind::Literal, regular)));
                }
                '<' | '>' => {
                    let second = self.get_char()?;
                    if second == c {
                        let (kind, text) = if c == '<' {
                            (Kind::StartDict, "<<")
                        } else {
                            (Kind::EndDict, ">>")
                        };
                        return Ok(Some(Token::text(kind, text.to_owned())));
                    }
                    self.position -= 1;
                    return Ok(Some(Token::text(Kind::Name, c.to_string())));
                }
                c if is_whitespace(c) || c == '\0' => {}
                _ => {
                    self.position -= 1;
                    if let Some(number) = self.try_read_number()? {
                        return Ok(Some(number));
                    }
                    let name = self.read_regular().ok_or(Failure::Damaged)?;
                    if name == "RD" || name == "-|" {
                        return match previous {
                            Some(previous) if previous.kind == Kind::Integer => {
                                self.read_charstring(previous.int_value()?).map(Some)
                            }
                            _ => Err(Failure::Damaged),
                        };
                    }
                    return Ok(Some(Token::text(Kind::Name, name)));
                }
            }
        }
        Ok(None)
    }

    /// `tryReadNumber`: a number, or `None` with the position restored.
    fn try_read_number(&mut self) -> Parsed<Option<Token>> {
        let mark = self.position;
        let mut text = String::new();
        let mut radix: Option<String> = None;
        let mut c = self.get_char()?;
        let mut has_digit = false;
        if c == '+' || c == '-' {
            text.push(c);
            c = self.get_char()?;
        }
        while c.is_ascii_digit() {
            text.push(c);
            c = self.get_char()?;
            has_digit = true;
        }
        if c == '.' {
            text.push(c);
            c = self.get_char()?;
        } else if c == '#' {
            radix = Some(std::mem::take(&mut text));
            c = self.get_char()?;
        } else if text.is_empty() || !has_digit {
            self.position = mark;
            return Ok(None);
        } else if c != 'e' && c != 'E' {
            self.position -= 1;
            return Ok(Some(Token::text(Kind::Integer, text)));
        }
        if c.is_ascii_digit() {
            text.push(c);
            c = self.get_char()?;
        } else if c != 'e' && c != 'E' {
            self.position = mark;
            return Ok(None);
        }
        while c.is_ascii_digit() {
            text.push(c);
            c = self.get_char()?;
        }
        if c == 'E' || c == 'e' {
            text.push(c);
            c = self.get_char()?;
            if c == '-' {
                text.push(c);
                c = self.get_char()?;
            }
            if c.is_ascii_digit() {
                text.push(c);
                c = self.get_char()?;
            } else {
                self.position = mark;
                return Ok(None);
            }
            while c.is_ascii_digit() {
                text.push(c);
                c = self.get_char()?;
            }
        }
        self.position -= 1;
        if let Some(radix) = radix {
            let base = crate::java::parse_int(&radix).ok_or(Failure::Number)?;
            if !(2..=36).contains(&base) {
                return Err(Failure::Damaged);
            }
            let value = i32::from_str_radix(&text, base as u32).map_err(|_| Failure::Damaged)?;
            return Ok(Some(Token::text(Kind::Integer, value.to_string())));
        }
        Ok(Some(Token::text(Kind::Real, text)))
    }

    /// `readRegular`: a run of characters that are neither delimiters nor whitespace.
    fn read_regular(&mut self) -> Option<String> {
        let mut text = String::new();
        while let Some(&byte) = self.bytes.get(self.position) {
            let c = java_char(byte);
            if is_whitespace(c) || "()<>[]{}/%".contains(c) {
                break;
            }
            text.push(c);
            self.position += 1;
        }
        (!text.is_empty()).then_some(text)
    }

    fn read_comment(&mut self) {
        while let Some(&byte) = self.bytes.get(self.position) {
            self.position += 1;
            if byte == b'\r' || byte == b'\n' {
                break;
            }
        }
    }

    /// `readString`: a string up to its closing parenthesis, `None` where the bytes end first.
    fn read_string(&mut self) -> Parsed<Option<Token>> {
        let mut text = String::new();
        while self.has_remaining() {
            let c = self.get_char()?;
            match c {
                '(' => {
                    self.open_parens += 1;
                    text.push('(');
                }
                ')' => {
                    if self.open_parens == 0 {
                        return Ok(Some(Token::text(Kind::String, text)));
                    }
                    text.push(')');
                    self.open_parens -= 1;
                }
                '\\' => {
                    let escaped = self.get_char()?;
                    match escaped {
                        'n' | 'r' => text.push('\n'),
                        't' => text.push('\t'),
                        'b' => text.push('\u{8}'),
                        'f' => text.push('\u{c}'),
                        '\\' => text.push('\\'),
                        '(' => text.push('('),
                        ')' => text.push(')'),
                        _ => {}
                    }
                    if escaped.is_ascii_digit() {
                        let digits = [escaped, self.get_char()?, self.get_char()?];
                        let digits: String = digits.iter().collect();
                        let code = i32::from_str_radix(&digits, 8).map_err(|_| Failure::Damaged)?;
                        text.push(char::from_u32(code as u32).ok_or(Failure::Damaged)?);
                    }
                }
                '\r' | '\n' => text.push('\n'),
                other => text.push(other),
            }
        }
        Ok(None)
    }

    fn read_charstring(&mut self, length: i32) -> Parsed<Token> {
        if length > self.bytes.len() as i32 {
            return Err(Failure::Damaged);
        }
        if length < 0 {
            return Err(Failure::Uncaught(
                "a charstring of negative length, which FontBox throws on",
            ));
        }
        self.get_char()?;
        let end = self.position + length as usize;
        let data = self.bytes.get(self.position..end).ok_or(Failure::Damaged)?;
        self.position = end;
        Ok(Token {
            kind: Kind::CharString,
            text: None,
            data: data.to_vec(),
        })
    }
}

/// `Type1Parser`.
struct Parser<'a> {
    lexer: Lexer<'a>,
}

impl Parser<'_> {
    fn read(&mut self, kind: Kind) -> Parsed<Token> {
        match self.lexer.next_token()? {
            Some(token) if token.kind == kind => Ok(token),
            _ => Err(Failure::Damaged),
        }
    }

    fn read_named(&mut self, kind: Kind, name: &str) -> Parsed<()> {
        let token = self.read(kind)?;
        if token.text.as_deref() == Some(name) {
            Ok(())
        } else {
            Err(Failure::Damaged)
        }
    }

    fn read_maybe(&mut self, kind: Kind, name: &str) -> Parsed<Option<Token>> {
        if self.lexer.peek().is_some_and(|token| token.is(kind, name)) {
            return self.lexer.next_token();
        }
        Ok(None)
    }

    fn peek_name(&self, name: &str) -> bool {
        self.lexer
            .peek()
            .is_some_and(|token| token.is(Kind::Name, name))
    }

    /// `readDef`.
    fn read_def(&mut self) -> Parsed<()> {
        self.read_maybe(Kind::Name, "readonly")?;
        self.read_maybe(Kind::Name, "noaccess")?;
        let mut token = self.read(Kind::Name)?;
        match token.text.as_deref() {
            Some("ND" | "|-") => return Ok(()),
            Some("noaccess") => token = self.read(Kind::Name)?,
            _ => {}
        }
        if token.text.as_deref() == Some("def") {
            Ok(())
        } else {
            Err(Failure::Damaged)
        }
    }

    /// `readPut`.
    fn read_put(&mut self) -> Parsed<()> {
        self.read_maybe(Kind::Name, "readonly")?;
        let mut token = self.read(Kind::Name)?;
        match token.text.as_deref() {
            Some("NP" | "|") => return Ok(()),
            Some("noaccess") => token = self.read(Kind::Name)?,
            _ => {}
        }
        if token.text.as_deref() == Some("put") {
            Ok(())
        } else {
            Err(Failure::Damaged)
        }
    }

    /// `readProc`: the tokens of a procedure after its opening brace.
    fn read_proc(&mut self) -> Parsed<Vec<Token>> {
        let mut value = Vec::new();
        let mut open = 1;
        loop {
            if self.lexer.peek().is_none() {
                return Err(Failure::Damaged);
            }
            if self.lexer.peek_kind(Kind::StartProc) {
                open += 1;
            }
            let token = self.lexer.next_token()?.ok_or(Failure::Damaged)?;
            let end = token.kind == Kind::EndProc;
            value.push(token);
            if end {
                open -= 1;
                if open == 0 {
                    break;
                }
            }
        }
        if let Some(executeonly) = self.read_maybe(Kind::Name, "executeonly")? {
            value.push(executeonly);
        }
        Ok(value)
    }

    fn read_proc_void(&mut self) -> Parsed<()> {
        self.read_proc().map(|_| ())
    }

    /// `readValue`.
    fn read_value(&mut self) -> Parsed<Vec<Token>> {
        let mut value = Vec::new();
        let token = self.lexer.next_token()?;
        if self.lexer.peek().is_none() {
            return Ok(value);
        }
        let token = token.ok_or(Failure::Uncaught(
            "a font program value missing before more tokens, which FontBox fails on",
        ))?;
        let kind = token.kind;
        value.push(token);
        match kind {
            Kind::StartArray => {
                let mut open = 1;
                loop {
                    if self.lexer.peek().is_none() {
                        return Ok(value);
                    }
                    if self.lexer.peek_kind(Kind::StartArray) {
                        open += 1;
                    }
                    let token = self.lexer.next_token()?.ok_or(Failure::Damaged)?;
                    let end = token.kind == Kind::EndArray;
                    value.push(token);
                    if end {
                        open -= 1;
                        if open == 0 {
                            break;
                        }
                    }
                }
            }
            Kind::StartProc => value.extend(self.read_proc()?),
            Kind::StartDict => {
                self.read(Kind::EndDict)?;
                return Ok(value);
            }
            _ => {}
        }
        self.read_postscript_wrapper(&mut value)?;
        Ok(value)
    }

    fn read_postscript_wrapper(&mut self, value: &mut Vec<Token>) -> Parsed<()> {
        let Some(next) = self.lexer.peek() else {
            return Err(Failure::Damaged);
        };
        if next.text.as_deref() == Some("systemdict") {
            self.read_named(Kind::Name, "systemdict")?;
            self.read_named(Kind::Literal, "internaldict")?;
            self.read_named(Kind::Name, "known")?;
            self.read(Kind::StartProc)?;
            self.read_proc_void()?;
            self.read(Kind::StartProc)?;
            self.read_proc_void()?;
            self.read_named(Kind::Name, "ifelse")?;
            self.read(Kind::StartProc)?;
            self.read_named(Kind::Name, "pop")?;
            *value = self.read_value()?;
            self.read(Kind::EndProc)?;
            self.read_named(Kind::Name, "if")?;
        }
        Ok(())
    }

    fn read_dict_value(&mut self) -> Parsed<Vec<Token>> {
        let value = self.read_value()?;
        self.read_def()?;
        Ok(value)
    }

    /// `readSimpleDict`, keeping the keys a font info dictionary is read for.
    fn read_simple_dict(&mut self) -> Parsed<Vec<(String, Vec<Token>)>> {
        let mut dict = Vec::new();
        let length = self.read(Kind::Integer)?.int_value()?;
        self.read_named(Kind::Name, "dict")?;
        self.read_maybe(Kind::Name, "dup")?;
        if self.read_maybe(Kind::Name, "def")?.is_some() {
            return Ok(dict);
        }
        self.read_named(Kind::Name, "begin")?;
        for _ in 0..length {
            if self.lexer.peek().is_none() {
                break;
            }
            if self.lexer.peek_kind(Kind::Name) && !self.peek_name("end") {
                self.read(Kind::Name)?;
            }
            if self.lexer.peek().is_none() || self.peek_name("end") {
                break;
            }
            let key = self.read(Kind::Literal)?.text.unwrap_or_default();
            let value = self.read_dict_value()?;
            dict.push((key, value));
        }
        self.read_named(Kind::Name, "end")?;
        self.read_maybe(Kind::Name, "readonly")?;
        self.read_named(Kind::Name, "def")?;
        Ok(dict)
    }
}

/// `arrayToNumbers`: the numbers between an array's brackets.
fn array_to_numbers(value: &[Token]) -> Parsed<Vec<f32>> {
    let inner = value.get(1..value.len().saturating_sub(1)).unwrap_or(&[]);
    inner
        .iter()
        .map(|token| match token.kind {
            Kind::Real => token.float_value(),
            Kind::Integer => token.int_value().map(|value| value as f32),
            _ => Err(Failure::Damaged),
        })
        .collect()
}

fn first(value: &[Token]) -> Parsed<&Token> {
    value.first().ok_or(Failure::Uncaught(
        "a font program key without a value, which FontBox throws on",
    ))
}

/// `Type1Parser.decrypt`.
fn decrypt(cipher: &[u8], key: u32, skip: i32) -> Vec<u8> {
    if skip == -1 {
        return cipher.to_vec();
    }
    if cipher.is_empty() || skip > cipher.len() as i32 {
        return Vec::new();
    }
    let mut plain = vec![0u8; (cipher.len() as i64 - i64::from(skip)) as usize];
    let mut r = key;
    for (index, &byte) in cipher.iter().enumerate() {
        let cipher_byte = u32::from(byte);
        let target = index as i64 - i64::from(skip);
        if target >= 0 {
            plain[target as usize] = (cipher_byte ^ (r >> 8)) as u8;
        }
        r = ((cipher_byte + r).wrapping_mul(52845).wrapping_add(22719)) & 0xFFFF;
    }
    plain
}

fn is_hex_digit(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

/// `isBinary`: whether the encrypted portion is binary rather than hexadecimal.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.len() < 4
        || bytes[..4]
            .iter()
            .any(|&byte| !matches!(byte, b'\n' | b'\r' | b' ' | b'\t') && !is_hex_digit(byte))
}

fn hex_to_binary(bytes: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = bytes
        .iter()
        .filter_map(|&byte| char::from(byte).to_digit(16).map(|digit| digit as u8))
        .collect();
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[high, low]| high * 16 + low)
        .collect()
}

/// `Type1Parser.parse`.
fn parse(segment1: &[u8], segment2: &[u8]) -> Parsed<Type1> {
    let mut font = Type1 {
        encoding: None,
        font_matrix: Vec::new(),
        font_bbox: Vec::new(),
        charstrings: FastMap::default(),
    };
    parse_ascii(segment1, &mut font).map_err(|failure| match failure {
        Failure::Number => Failure::Damaged,
        other => other,
    })?;
    if !segment2.is_empty() {
        parse_binary(segment2, &mut font)?;
    }
    Ok(font)
}

fn parse_ascii(bytes: &[u8], font: &mut Type1) -> Parsed<()> {
    if bytes.is_empty() || bytes.len() < 2 || (bytes[0] != b'%' && bytes[1] != b'!') {
        return Err(Failure::Damaged);
    }
    let mut parser = Parser {
        lexer: Lexer::new(bytes)?,
    };
    let first_text = parser
        .lexer
        .peek()
        .ok_or(Failure::Uncaught(
            "a font program without tokens, which FontBox throws on",
        ))?
        .text
        .clone();
    if first_text.as_deref() == Some("FontDirectory") {
        parser.read_named(Kind::Name, "FontDirectory")?;
        parser.read(Kind::Literal)?;
        parser.read_named(Kind::Name, "known")?;
        parser.read(Kind::StartProc)?;
        parser.read_proc_void()?;
        parser.read(Kind::StartProc)?;
        parser.read_proc_void()?;
        parser.read_named(Kind::Name, "ifelse")?;
    }
    let length = parser.read(Kind::Integer)?.int_value()?;
    parser.read_named(Kind::Name, "dict")?;
    parser.read_maybe(Kind::Name, "dup")?;
    parser.read_named(Kind::Name, "begin")?;
    for _ in 0..length {
        let Some(token) = parser.lexer.peek() else {
            break;
        };
        if token.is(Kind::Name, "currentdict") || token.is(Kind::Name, "end") {
            break;
        }
        let key = parser.read(Kind::Literal)?.text.unwrap_or_default();
        match key.as_str() {
            "FontInfo" | "Fontinfo" => read_font_info(&parser.read_simple_dict()?)?,
            "Metrics" => {
                parser.read_simple_dict()?;
            }
            "Encoding" => font.encoding = Some(read_encoding(&mut parser)?),
            _ => read_simple_value(&mut parser, &key, font)?,
        }
    }
    parser.read_maybe(Kind::Name, "currentdict")?;
    parser.read_named(Kind::Name, "end")?;
    parser.read_named(Kind::Name, "currentfile")?;
    parser.read_named(Kind::Name, "eexec")?;
    Ok(())
}

fn read_simple_value(parser: &mut Parser, key: &str, font: &mut Type1) -> Parsed<()> {
    let value = parser.read_dict_value()?;
    match key {
        "FontName" | "FID" => {
            first(&value)?;
        }
        "PaintType" | "FontType" | "UniqueID" => {
            first(&value)?.int_value()?;
        }
        "StrokeWidth" => {
            first(&value)?.float_value()?;
        }
        "FontMatrix" => font.font_matrix = array_to_numbers(&value)?,
        "FontBBox" => font.font_bbox = array_to_numbers(&value)?,
        _ => {}
    }
    Ok(())
}

/// `readFontInfo`: reads the values PDFBox keeps, which fail where they are not numbers.
fn read_font_info(info: &[(String, Vec<Token>)]) -> Parsed<()> {
    let known = |key: &str| {
        matches!(
            key,
            "version"
                | "Notice"
                | "FullName"
                | "FamilyName"
                | "Weight"
                | "ItalicAngle"
                | "isFixedPitch"
                | "UnderlinePosition"
                | "UnderlineThickness"
        )
    };
    if info
        .iter()
        .any(|(key, value)| known(key) && value.is_empty())
    {
        return Err(Failure::Uncaught(
            "a font info key without a value, which FontBox throws on",
        ));
    }
    for (key, value) in info {
        if matches!(
            key.as_str(),
            "ItalicAngle" | "UnderlinePosition" | "UnderlineThickness"
        ) {
            first(value)?.float_value()?;
        }
    }
    Ok(())
}

fn read_encoding(parser: &mut Parser) -> Parsed<BuiltIn> {
    if parser.lexer.peek_kind(Kind::Name) {
        let name = parser
            .lexer
            .next_token()?
            .and_then(|token| token.text)
            .unwrap_or_default();
        if name != "StandardEncoding" {
            return Err(Failure::Damaged);
        }
        parser.read_maybe(Kind::Name, "readonly")?;
        parser.read_named(Kind::Name, "def")?;
        return Ok(BuiltIn::Standard);
    }
    parser.read(Kind::Integer)?.int_value()?;
    parser.read_maybe(Kind::Name, "array")?;
    while !(parser.peek_name("dup") || parser.peek_name("readonly") || parser.peek_name("def")) {
        if parser.lexer.next_token()?.is_none() {
            return Err(Failure::Damaged);
        }
    }
    let mut entries = Vec::new();
    while parser.peek_name("dup") {
        parser.read_named(Kind::Name, "dup")?;
        let code = parser.read(Kind::Integer)?.int_value()?;
        let name = parser.read(Kind::Literal)?.text.unwrap_or_default();
        parser.read_named(Kind::Name, "put")?;
        entries.push((code, name));
    }
    parser.read_maybe(Kind::Name, "readonly")?;
    parser.read_named(Kind::Name, "def")?;
    Ok(BuiltIn::Custom(entries))
}

fn parse_binary(bytes: &[u8], font: &mut Type1) -> Parsed<()> {
    let decrypted = if is_binary(bytes) {
        decrypt(bytes, EEXEC_KEY, 4)
    } else {
        decrypt(&hex_to_binary(bytes), EEXEC_KEY, 4)
    };
    let mut parser = Parser {
        lexer: Lexer::new(&decrypted)?,
    };
    loop {
        match parser.lexer.peek() {
            None => return Err(Failure::Damaged),
            Some(token) if token.text.as_deref() == Some("Private") => break,
            Some(_) => {
                parser.lexer.next_token()?;
            }
        }
    }
    parser.read_named(Kind::Literal, "Private")?;
    let length = parser.read(Kind::Integer)?.int_value()?;
    parser.read_named(Kind::Name, "dict")?;
    parser.read_maybe(Kind::Name, "dup")?;
    parser.read_named(Kind::Name, "begin")?;
    let mut len_iv = 4;
    for _ in 0..length {
        if !parser.lexer.peek_kind(Kind::Literal) {
            break;
        }
        let key = parser.read(Kind::Literal)?.text.unwrap_or_default();
        match key.as_str() {
            "Subrs" => read_subrs(&mut parser, len_iv)?,
            "OtherSubrs" => read_other_subrs(&mut parser)?,
            "lenIV" => len_iv = first(&parser.read_dict_value()?)?.int_value()?,
            "ND" => {
                parser.read(Kind::StartProc)?;
                parser.read_maybe(Kind::Name, "noaccess")?;
                parser.read_named(Kind::Name, "def")?;
                parser.read(Kind::EndProc)?;
                parser.read_maybe(Kind::Name, "executeonly")?;
                parser.read_maybe(Kind::Name, "readonly")?;
                parser.read_named(Kind::Name, "def")?;
            }
            "NP" => {
                parser.read(Kind::StartProc)?;
                parser.read_maybe(Kind::Name, "noaccess")?;
                parser.read(Kind::Name)?;
                parser.read(Kind::EndProc)?;
                parser.read_maybe(Kind::Name, "executeonly")?;
                parser.read_maybe(Kind::Name, "readonly")?;
                parser.read_named(Kind::Name, "def")?;
            }
            "RD" => {
                parser.read(Kind::StartProc)?;
                parser.read_proc_void()?;
                parser.read_maybe(Kind::Name, "bind")?;
                parser.read_maybe(Kind::Name, "executeonly")?;
                parser.read_maybe(Kind::Name, "readonly")?;
                parser.read_named(Kind::Name, "def")?;
            }
            _ => read_private(&key, &parser.read_dict_value()?)?,
        }
    }
    while !parser
        .lexer
        .peek()
        .is_some_and(|token| token.is(Kind::Literal, "CharStrings"))
    {
        if parser.lexer.next_token()?.is_none() {
            return Err(Failure::Damaged);
        }
    }
    parser.read_named(Kind::Literal, "CharStrings")?;
    read_charstrings(&mut parser, len_iv, font)
}

/// `readPrivate`: the values PDFBox keeps, which fail where they are not numbers.
fn read_private(key: &str, value: &[Token]) -> Parsed<()> {
    match key {
        "BlueValues" | "OtherBlues" | "FamilyBlues" | "FamilyOtherBlues" | "StdHW" | "StdVW"
        | "StemSnapH" | "StemSnapV" => {
            array_to_numbers(value)?;
        }
        "BlueScale" => {
            first(value)?.float_value()?;
        }
        "BlueShift" | "BlueFuzz" | "LanguageGroup" => {
            first(value)?.int_value()?;
        }
        "ForceBold" => {
            first(value)?;
        }
        _ => {}
    }
    Ok(())
}

fn announced(length: i32) -> Parsed<i32> {
    if length > MAX_ANNOUNCED {
        return Err(Failure::Uncaught(
            "a font program announcing more entries than the port allocates",
        ));
    }
    Ok(length)
}

fn read_subrs(parser: &mut Parser, len_iv: i32) -> Parsed<()> {
    let length = announced(parser.read(Kind::Integer)?.int_value()?)?;
    parser.read_named(Kind::Name, "array")?;
    for _ in 0..length {
        if parser.lexer.peek().is_none() || !parser.peek_name("dup") {
            break;
        }
        parser.read_named(Kind::Name, "dup")?;
        let index = parser.read(Kind::Integer)?;
        parser.read(Kind::Integer)?;
        let charstring = parser.read(Kind::CharString)?;
        if index.int_value()? < 0 {
            return Err(Failure::Uncaught(
                "a subroutine of negative index, which FontBox throws on",
            ));
        }
        decrypt(&charstring.data, CHARSTRING_KEY, len_iv);
        parser.read_put()?;
    }
    parser.read_def()
}

fn read_other_subrs(parser: &mut Parser) -> Parsed<()> {
    if parser.lexer.peek().is_none() {
        return Err(Failure::Damaged);
    }
    if parser.lexer.peek_kind(Kind::StartArray) {
        parser.read_value()?;
        return parser.read_def();
    }
    let length = announced(parser.read(Kind::Integer)?.int_value()?)?;
    parser.read_named(Kind::Name, "array")?;
    for _ in 0..length {
        parser.read_named(Kind::Name, "dup")?;
        parser.read(Kind::Integer)?;
        parser.read_value()?;
        parser.read_put()?;
    }
    parser.read_def()
}

fn read_charstrings(parser: &mut Parser, len_iv: i32, font: &mut Type1) -> Parsed<()> {
    let length = parser.read(Kind::Integer)?.int_value()?;
    parser.read_named(Kind::Name, "dict")?;
    parser.read_named(Kind::Name, "dup")?;
    parser.read_named(Kind::Name, "begin")?;
    for _ in 0..length {
        if parser.lexer.peek().is_none() || parser.peek_name("end") {
            break;
        }
        let name = parser.read(Kind::Literal)?.text.unwrap_or_default();
        parser.read(Kind::Integer)?;
        let charstring = parser.read(Kind::CharString)?;
        font.charstrings
            .insert(name, decrypt(&charstring.data, CHARSTRING_KEY, len_iv));
        parser.read_def()?;
    }
    parser.read_named(Kind::Name, "end")
}

/// The segments of a PFB file (`PfbParser`): the ASCII ones, then the binary ones.
fn pfb_segments(pfb: &[u8]) -> Parsed<(Vec<u8>, Vec<u8>)> {
    if pfb.len() < 18 {
        return Err(Failure::Damaged);
    }
    let mut position = 0usize;
    let mut records: Vec<(i32, Vec<u8>)> = Vec::new();
    let mut total: i64 = 0;
    loop {
        let mut read = || -> i32 {
            let byte = pfb.get(position).map_or(-1, |&byte| i32::from(byte));
            position = (position + 1).min(pfb.len());
            byte
        };
        let marker = read();
        if marker == -1 && total > 0 {
            break;
        }
        if marker != i32::from(PFB_START_MARKER) {
            return Err(Failure::Damaged);
        }
        let record_type = read();
        if record_type == 3 {
            break;
        }
        if record_type != 1 && record_type != 2 {
            return Err(Failure::Damaged);
        }
        let mut size = read();
        size = size.wrapping_add(read() << 8);
        size = size.wrapping_add(read() << 16);
        size = size.wrapping_add(read().wrapping_shl(24));
        if size < 0 || size as usize > pfb.len() {
            return Err(Failure::Damaged);
        }
        let end = position + size as usize;
        if end > pfb.len() || position == pfb.len() {
            return Err(Failure::Damaged);
        }
        records.push((record_type, pfb[position..end].to_vec()));
        total += i64::from(size);
        position = end;
    }
    let count = records.len();
    let mut ascii = Vec::new();
    for (index, (record_type, data)) in records.iter().enumerate() {
        if *record_type != 1 {
            continue;
        }
        let cleartomark = index == count - 1
            && data.len() < 600
            && data.windows(11).any(|window| window == b"cleartomark");
        if !cleartomark {
            ascii.extend_from_slice(data);
        }
    }
    let binary: Vec<u8> = records
        .iter()
        .filter(|(record_type, _)| *record_type == 2)
        .flat_map(|(_, data)| data.iter().copied())
        .collect();
    Ok((ascii, binary))
}

/// `findBinaryOffsetAfterExec`.
fn binary_offset_after_exec(bytes: &[u8], start: i64) -> i64 {
    let mut offset = start;
    while offset > 0 {
        let at = offset as usize;
        if bytes.get(at..at + 4) == Some(b"exec") {
            offset += 4;
            while (offset as usize) < bytes.len()
                && matches!(bytes[offset as usize], b'\r' | b'\n' | b' ' | b'\t')
            {
                offset += 1;
            }
            break;
        }
        offset -= 1;
    }
    offset
}

/// `PDType1Font.repairLength1`.
fn repair_length1(bytes: &[u8], length1: i64) -> i64 {
    let len = bytes.len() as i64;
    let mut offset = (length1 - 4).max(0);
    if offset <= 0 || offset > len - 4 {
        offset = len - 4;
    }
    offset = binary_offset_after_exec(bytes, offset);
    if offset == 0 && length1 > 0 {
        offset = binary_offset_after_exec(bytes, len - 4);
    }
    if length1 - offset != 0 && offset > 0 {
        return offset;
    }
    length1
}

/// `PDType1Font`'s reading of a `FontFile` stream: its decoded bytes, cut at its `Length1`
/// and `Length2` as PDFBox repairs them, or read as a whole PFB file.
pub(super) fn read(bytes: &[u8], length1: i32, length2: i32) -> Read {
    if bytes.is_empty() {
        return Read::Damaged;
    }
    let len = bytes.len() as i64;
    let length1 = repair_length1(bytes, i64::from(length1));
    let mut length2 = i64::from(length2);
    if length2 < 0 || length2 > len - length1 {
        length2 = len - length1;
    }
    let parsed = if bytes[0] == PFB_START_MARKER {
        pfb_segments(bytes).and_then(|(segment1, segment2)| parse(&segment1, &segment2).map(Some))
    } else {
        if length1 < 0 || length2 < 0 {
            return Read::Damaged;
        }
        if length1 == 0 || length2 == 0 {
            return Read::Absent;
        }
        let segment1 = &bytes[..length1 as usize];
        let segment2 = &bytes[length1 as usize..(length1 + length2) as usize];
        parse(segment1, segment2).map(Some)
    };
    match parsed {
        Ok(Some(font)) => Read::Font(Box::new(font)),
        Ok(None) => Read::Absent,
        Err(Failure::Damaged) => Read::Damaged,
        Err(Failure::Number) => Read::Unported(
            "a Type 1 font program with a malformed number past its clear text, which FontBox \
             throws on"
                .to_owned(),
        ),
        Err(Failure::Uncaught(reason)) => Read::Unported(reason.to_owned()),
    }
}

impl Type1 {
    /// `hasGlyph`.
    pub(super) fn has_glyph(&self, name: &str) -> bool {
        self.charstrings.contains_key(name)
    }

    /// `getWidth(name)`: the width the glyph's charstring sets with `hsbw` or `sbw`, the
    /// `.notdef` glyph's for a name the font lacks.
    pub(super) fn width(&self, name: &str) -> Result<f32, Unported> {
        let charstring = self
            .charstrings
            .get(name)
            .or_else(|| self.charstrings.get(".notdef"))
            .ok_or_else(|| {
                Unported(
                    "a Type 1 font without a .notdef glyph measured, which FontBox throws on"
                        .to_owned(),
                )
            })?;
        charstring_width(charstring)
    }
}

/// The width a Type 1 charstring sets: the second operand of `hsbw`, the third of `sbw`.
fn charstring_width(charstring: &[u8]) -> Result<f32, Unported> {
    let mut operands: Vec<f32> = Vec::new();
    let mut index = 0;
    while index < charstring.len() {
        let byte = charstring[index];
        index += 1;
        match byte {
            13 => return Ok(operands.get(1).copied().unwrap_or(0.0)),
            12 if charstring.get(index) == Some(&7) => {
                return Ok(operands.get(2).copied().unwrap_or(0.0))
            }
            0..=31 => break,
            32..=246 => operands.push(f32::from(byte) - 139.0),
            247..=250 => {
                let next = f32::from(*charstring.get(index).unwrap_or(&0));
                index += 1;
                operands.push((f32::from(byte) - 247.0) * 256.0 + next + 108.0);
            }
            251..=254 => {
                let next = f32::from(*charstring.get(index).unwrap_or(&0));
                index += 1;
                operands.push(-(f32::from(byte) - 251.0) * 256.0 - next - 108.0);
            }
            255 => {
                let bytes: [u8; 4] = charstring
                    .get(index..index + 4)
                    .and_then(|bytes| bytes.try_into().ok())
                    .unwrap_or([0; 4]);
                index += 4;
                operands.push(i32::from_be_bytes(bytes) as f32);
            }
        }
    }
    Err(Unported(
        "a Type 1 glyph whose width is not set by its first command, which the port does not \
         run"
        .to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypt(plain: &[u8], key: u32, skip: usize) -> Vec<u8> {
        let mut r = key;
        let mut out = Vec::new();
        for &byte in std::iter::repeat_n(&0u8, skip).chain(plain) {
            let cipher = (u32::from(byte) ^ (r >> 8)) as u8;
            out.push(cipher);
            r = ((u32::from(cipher) + r)
                .wrapping_mul(52845)
                .wrapping_add(22719))
                & 0xFFFF;
        }
        out
    }

    fn program(encoding: &str, glyph: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let ascii = format!(
            "%!PS-AdobeFont-1.0: Test\n10 dict begin\n/FontName /Test def\n/FontType 1 def\n\
             /FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/FontBBox {{-10 -20 500 700}} \
             readonly def\n/Encoding {encoding} def\ncurrentdict end\ncurrentfile eexec\n"
        );
        let charstring = encrypt(glyph, CHARSTRING_KEY, 4);
        let mut private = format!(
            "dup /Private 8 dict dup begin\n/RD{{string currentfile exch readstring pop}} \
             executeonly def\n/ND{{noaccess def}} executeonly def\n/NP{{noaccess put}} \
             executeonly def\n/BlueValues [-20 0] def\n/lenIV 4 def\n2 index /CharStrings 2 \
             dict dup begin\n/A {} RD ",
            charstring.len()
        )
        .into_bytes();
        private.extend_from_slice(&charstring);
        private.extend_from_slice(b" ND\nend\nend\nreadonly put\nnoaccess put\n");
        (ascii.into_bytes(), encrypt(&private, EEXEC_KEY, 4))
    }

    #[test]
    fn reads_a_type1_program_as_fontbox_does() {
        let hsbw = [139 + 10, 247, 2, 13];
        let (ascii, binary) = program(
            "256 array 0 1 255 {1 index exch /.notdef put} for dup 65 /A put readonly",
            &hsbw,
        );
        let mut bytes = ascii.clone();
        bytes.extend_from_slice(&binary);
        let Read::Font(font) = read(&bytes, ascii.len() as i32, binary.len() as i32) else {
            panic!("the program does not read");
        };
        assert_eq!(
            font.encoding,
            Some(BuiltIn::Custom(vec![(65, "A".to_owned())]))
        );
        assert_eq!(font.font_matrix, [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
        assert_eq!(font.font_bbox, [-10.0, -20.0, 500.0, 700.0]);
        assert!(font.has_glyph("A") && !font.has_glyph("B"));
        assert_eq!(font.width("A").unwrap(), 110.0);
    }

    #[test]
    fn repairs_lengths_and_reports_damage_as_pdfbox_does() {
        let (ascii, binary) = program("StandardEncoding", &[139, 139, 13]);
        let mut bytes = ascii.clone();
        bytes.extend_from_slice(&binary);
        let Read::Font(font) = read(&bytes, 7, -1) else {
            panic!("the repaired lengths do not read");
        };
        assert_eq!(font.encoding, Some(BuiltIn::Standard));
        assert!(matches!(read(&bytes[..ascii.len()], -1, -1), Read::Absent));
        let mut broken = bytes.clone();
        broken[2] = b'X';
        broken[1] = b'X';
        broken[0] = b'X';
        assert!(matches!(
            read(&broken, ascii.len() as i32, binary.len() as i32),
            Read::Damaged
        ));
    }
}
