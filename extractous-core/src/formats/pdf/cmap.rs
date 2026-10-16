//! FontBox's `CMap` and `CMapParser`: the character maps of composite fonts and the
//! `ToUnicode` maps of all fonts, read as FontBox reads them, and the predefined Identity maps.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::LazyLock;

use super::cos::decode_java;
use super::Unported;

/// The predefined CMaps FontBox bundles, by name.
const PREDEFINED: [&str; 92] = [
    "83pv-RKSJ-H",
    "90msp-RKSJ-H",
    "90msp-RKSJ-V",
    "90ms-RKSJ-H",
    "90ms-RKSJ-V",
    "90pv-RKSJ-H",
    "90pv-RKSJ-V",
    "Add-RKSJ-H",
    "Add-RKSJ-V",
    "Adobe-CNS1-0",
    "Adobe-CNS1-1",
    "Adobe-CNS1-2",
    "Adobe-CNS1-3",
    "Adobe-CNS1-4",
    "Adobe-CNS1-5",
    "Adobe-CNS1-6",
    "Adobe-CNS1-UCS2",
    "Adobe-GB1-0",
    "Adobe-GB1-1",
    "Adobe-GB1-2",
    "Adobe-GB1-3",
    "Adobe-GB1-4",
    "Adobe-GB1-5",
    "Adobe-GB1-UCS2",
    "Adobe-Japan1-0",
    "Adobe-Japan1-1",
    "Adobe-Japan1-2",
    "Adobe-Japan1-3",
    "Adobe-Japan1-4",
    "Adobe-Japan1-5",
    "Adobe-Japan1-6",
    "Adobe-Japan1-7",
    "Adobe-Japan1-UCS2",
    "Adobe-Japan2-0",
    "Adobe-Korea1-0",
    "Adobe-Korea1-1",
    "Adobe-Korea1-2",
    "Adobe-Korea1-UCS2",
    "B5pc-H",
    "B5pc-V",
    "CNS-EUC-H",
    "CNS-EUC-V",
    "ETen-B5-H",
    "ETen-B5-V",
    "ETenms-B5-H",
    "ETenms-B5-V",
    "EUC-H",
    "EUC-V",
    "Ext-RKSJ-H",
    "Ext-RKSJ-V",
    "GB-EUC-H",
    "GB-EUC-V",
    "GBK2K-H",
    "GBK2K-V",
    "GBK-EUC-H",
    "GBK-EUC-V",
    "GBKp-EUC-H",
    "GBKp-EUC-V",
    "GBpc-EUC-H",
    "GBpc-EUC-V",
    "H",
    "HKscs-B5-H",
    "HKscs-B5-V",
    "Identity-H",
    "Identity-V",
    "KSC-EUC-H",
    "KSC-EUC-V",
    "KSCms-UHC-H",
    "KSCms-UHC-HW-H",
    "KSCms-UHC-HW-V",
    "KSCms-UHC-V",
    "KSCpc-EUC-H",
    "KSCpc-EUC-V",
    "UniCNS-UCS2-H",
    "UniCNS-UCS2-V",
    "UniCNS-UTF16-H",
    "UniCNS-UTF16-V",
    "UniGB-UCS2-H",
    "UniGB-UCS2-V",
    "UniGB-UTF16-H",
    "UniGB-UTF16-V",
    "UniJIS-UCS2-H",
    "UniJIS-UCS2-HW-H",
    "UniJIS-UCS2-HW-V",
    "UniJIS-UCS2-V",
    "UniJIS-UTF16-H",
    "UniJIS-UTF16-V",
    "UniKS-UCS2-H",
    "UniKS-UCS2-V",
    "UniKS-UTF16-H",
    "UniKS-UTF16-V",
    "V",
];

/// Why a CMap was not read: FontBox throws an `IOException` on it, which PDFBox catches for a
/// `ToUnicode` map, or the map needs something the port does not hold.
#[derive(Debug)]
pub(super) enum CMapError {
    Malformed(String),
    Unported(Unported),
}

fn malformed(what: &str) -> CMapError {
    CMapError::Malformed(what.to_owned())
}

/// `CodespaceRange`.
#[derive(Debug, Clone)]
struct CodespaceRange {
    start: Vec<u8>,
    end: Vec<u8>,
}

impl CodespaceRange {
    fn new(start: &[u8], end: &[u8]) -> Result<Self, CMapError> {
        let start = if start.len() != end.len() && start == [0] {
            vec![0; end.len()]
        } else if start.len() != end.len() {
            return Err(malformed(
                "The start and the end values must not have different lengths.",
            ));
        } else {
            start.to_vec()
        };
        Ok(Self {
            start,
            end: end.to_vec(),
        })
    }

    fn len(&self) -> usize {
        self.end.len()
    }

    fn is_full_match(&self, code: &[u8]) -> bool {
        code.len() == self.len()
            && code
                .iter()
                .zip(self.start.iter().zip(&self.end))
                .all(|(&byte, (&start, &end))| byte >= start && byte <= end)
    }
}

/// `CIDRange`.
#[derive(Debug, Clone)]
struct CidRange {
    from: u32,
    to: u32,
    cid: u32,
    length: usize,
}

impl CidRange {
    fn map(&self, code: u32, length: usize) -> Option<u32> {
        (length == self.length && self.from <= code && code <= self.to)
            .then(|| self.cid.wrapping_add(code - self.from))
    }

    fn extend(&mut self, from: u32, to: u32, cid: u32, length: usize) -> bool {
        if self.length == length
            && from == self.to.wrapping_add(1)
            && cid == self.cid.wrapping_add(self.to - self.from).wrapping_add(1)
        {
            self.to = to;
            return true;
        }
        false
    }
}

/// FontBox's `CMap`.
#[derive(Debug, Clone)]
pub(super) struct CMap {
    pub(super) wmode: i32,
    pub(super) name: Option<String>,
    pub(super) registry: Option<String>,
    pub(super) ordering: Option<String>,
    pub(super) supplement: i32,
    min_code_length: usize,
    max_code_length: usize,
    min_cid_length: usize,
    max_cid_length: usize,
    codespace_ranges: Vec<CodespaceRange>,
    one_byte: CodeMap<u32, String>,
    two_bytes: CodeMap<u32, String>,
    more_bytes: CodeMap<u32, String>,
    code_to_cid: HashMap<usize, CodeMap<u32, u32>>,
    cid_ranges: Vec<CidRange>,
    space_mapping: i64,
}

/// A map keyed by character codes or CIDs, which are looked up glyph by glyph.
pub(super) type CodeMap<K, V> = HashMap<K, V, BuildHasherDefault<CodeHasher>>;

/// Fibonacci hashing of a character code: one multiplication, where SipHash's resistance to
/// chosen keys buys nothing for codes a font defines.
#[derive(Debug, Default)]
pub(super) struct CodeHasher(u64);

/// 2^64 divided by the golden ratio.
const FIBONACCI: u64 = 0x9E37_79B9_7F4A_7C15;

impl Hasher for CodeHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 << 8 | u64::from(byte)).wrapping_mul(FIBONACCI);
        }
    }

    fn write_u32(&mut self, code: u32) {
        self.0 = u64::from(code).wrapping_mul(FIBONACCI);
    }

    fn write_i32(&mut self, code: i32) {
        self.write_u32(code as u32);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl CMap {
    fn new() -> Self {
        Self {
            wmode: 0,
            name: None,
            registry: None,
            ordering: None,
            supplement: 0,
            min_code_length: 4,
            max_code_length: 0,
            min_cid_length: 4,
            max_cid_length: 0,
            codespace_ranges: Vec::new(),
            one_byte: CodeMap::default(),
            two_bytes: CodeMap::default(),
            more_bytes: CodeMap::default(),
            code_to_cid: HashMap::new(),
            cid_ranges: Vec::new(),
            space_mapping: -1,
        }
    }

    pub(super) fn has_cid_mappings(&self) -> bool {
        !self.code_to_cid.is_empty() || !self.cid_ranges.is_empty()
    }

    pub(super) fn has_unicode_mappings(&self) -> bool {
        !self.one_byte.is_empty() || !self.two_bytes.is_empty() || !self.more_bytes.is_empty()
    }

    /// `CMap.getSpaceMapping`: the code a space maps to, or -1.
    pub(super) fn space_mapping(&self) -> i64 {
        self.space_mapping
    }

    /// `CMap.toUnicode(int)`.
    pub(super) fn to_unicode(&self, code: u32) -> Option<&str> {
        let one = (code < 256)
            .then(|| self.to_unicode_length(code, 1))
            .flatten();
        if one.is_some() {
            return one;
        }
        if code <= 0xFFFF {
            return self.to_unicode_length(code, 2);
        }
        if code <= 0xFF_FFFF {
            return self.to_unicode_length(code, 3);
        }
        self.to_unicode_length(code, 4)
    }

    /// `CMap.toUnicode(int, int)`.
    pub(super) fn to_unicode_length(&self, code: u32, length: usize) -> Option<&str> {
        match length {
            1 => self.one_byte.get(&code),
            2 => self.two_bytes.get(&code),
            _ => self.more_bytes.get(&code),
        }
        .map(String::as_str)
    }

    /// `CMap.readCode`: the code at the start of `bytes`, and how many bytes it takes, as
    /// FontBox reads it from a stream; `None` where FontBox fails because the map has no
    /// codespace ranges.
    pub(super) fn read_code(&self, bytes: &[u8]) -> Option<(u32, usize)> {
        if self.min_code_length == 0 || self.min_code_length > self.max_code_length {
            return None;
        }
        let mut code = vec![0u8; self.max_code_length];
        let first = self.min_code_length.min(bytes.len());
        code[..first].copy_from_slice(&bytes[..first]);
        let mut consumed = first;
        for index in self.min_code_length - 1..self.max_code_length {
            let count = index + 1;
            if self
                .codespace_ranges
                .iter()
                .any(|range| range.is_full_match(&code[..count]))
            {
                return Some((to_int(&code[..count]), consumed));
            }
            if count < self.max_code_length {
                match bytes.get(consumed) {
                    Some(&byte) => {
                        code[count] = byte;
                        consumed += 1;
                    }
                    None => code[count] = 0xFF,
                }
            }
        }
        Some((to_int(&code[..self.min_code_length]), first))
    }

    /// `CMap.toCID(int)`.
    pub(super) fn to_cid(&self, code: u32) -> u32 {
        if !self.has_cid_mappings() {
            return 0;
        }
        let mut cid = 0;
        let mut length = self.min_cid_length;
        while cid == 0 && length <= self.max_cid_length {
            cid = self.to_cid_length(code, length);
            length += 1;
        }
        cid
    }

    /// `CMap.toCID(int, int)`.
    fn to_cid_length(&self, code: u32, length: usize) -> u32 {
        if !self.has_cid_mappings() || length < self.min_cid_length || length > self.max_cid_length
        {
            return 0;
        }
        if let Some(cid) = self.code_to_cid.get(&length).and_then(|map| map.get(&code)) {
            return *cid;
        }
        self.cid_ranges
            .iter()
            .find_map(|range| range.map(code, length))
            .unwrap_or(0)
    }

    fn add_char_mapping(&mut self, codes: &[u8], unicode: String) {
        if unicode == " " {
            self.space_mapping = i64::from(to_int_java(codes));
        }
        match codes.len() {
            1 => {
                self.one_byte.insert(to_int(codes), unicode);
            }
            2 => {
                self.two_bytes.insert(to_int(codes), unicode);
            }
            3 | 4 => {
                self.more_bytes.insert(to_int(codes), unicode);
            }
            _ => {}
        }
    }

    fn add_cid_mapping(&mut self, code: &[u8], cid: u32) {
        if !self.code_to_cid.contains_key(&code.len()) {
            self.min_cid_length = self.min_cid_length.min(code.len());
            self.max_cid_length = self.max_cid_length.max(code.len());
        }
        self.code_to_cid
            .entry(code.len())
            .or_default()
            .insert(to_int(code), cid);
    }

    fn add_cid_range(&mut self, from: &[u8], to: &[u8], cid: u32) {
        let (start, end, length) = (to_int(from), to_int(to), from.len());
        let extended = self
            .cid_ranges
            .last_mut()
            .is_some_and(|last| last.extend(start, end, cid, length));
        if !extended {
            self.cid_ranges.push(CidRange {
                from: start,
                to: end,
                cid,
                length,
            });
            self.min_cid_length = self.min_cid_length.min(length);
            self.max_cid_length = self.max_cid_length.max(length);
        }
    }

    fn add_codespace_range(&mut self, range: CodespaceRange) {
        self.max_code_length = self.max_code_length.max(range.len());
        self.min_code_length = self.min_code_length.min(range.len());
        self.codespace_ranges.push(range);
    }

    /// `CMap.useCmap`.
    fn use_cmap(&mut self, other: &CMap) {
        for range in &other.codespace_ranges {
            self.add_codespace_range(range.clone());
        }
        self.one_byte.extend(other.one_byte.clone());
        self.two_bytes.extend(other.two_bytes.clone());
        self.more_bytes.extend(other.more_bytes.clone());
        for (&length, map) in &other.code_to_cid {
            self.code_to_cid
                .entry(length)
                .or_default()
                .extend(map.iter().map(|(&k, &v)| (k, v)));
        }
        self.cid_ranges.extend(other.cid_ranges.iter().cloned());
        self.max_code_length = self.max_code_length.max(other.max_code_length);
        self.min_code_length = self.min_code_length.min(other.min_code_length);
        self.max_cid_length = self.max_cid_length.max(other.max_cid_length);
        self.min_cid_length = self.min_cid_length.min(other.min_cid_length);
    }
}

/// `CMap.toInt`, unsigned.
fn to_int(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |code, &byte| (code << 8) | u32::from(byte))
}

/// `CMap.toInt` as Java's `int` holds it.
fn to_int_java(bytes: &[u8]) -> i32 {
    to_int(bytes) as i32
}

static IDENTITY_H: LazyLock<CMap> = LazyLock::new(|| identity("Identity-H", 0));
static IDENTITY_V: LazyLock<CMap> = LazyLock::new(|| identity("Identity-V", 1));

/// The predefined `Identity-H` and `Identity-V` maps: two-byte codes, each its own CID.
fn identity(name: &str, wmode: i32) -> CMap {
    let mut cmap = CMap::new();
    cmap.name = Some(name.to_owned());
    cmap.registry = Some("Adobe".to_owned());
    cmap.ordering = Some("Identity".to_owned());
    cmap.wmode = wmode;
    cmap.add_codespace_range(CodespaceRange {
        start: vec![0, 0],
        end: vec![0xFF, 0xFF],
    });
    for high in 0..=0xFFu32 {
        cmap.add_cid_range(&[high as u8, 0], &[high as u8, 0xFF], high * 256);
    }
    cmap
}

/// Whether FontBox bundles a predefined CMap of that name.
pub(super) fn is_predefined(name: &str) -> bool {
    PREDEFINED.contains(&name)
}

/// `CMapManager.getPredefinedCMap`: the predefined map of that name.
pub(super) fn predefined(name: &str) -> Result<&'static CMap, CMapError> {
    match name {
        "Identity-H" => Ok(&IDENTITY_H),
        "Identity-V" => Ok(&IDENTITY_V),
        _ if PREDEFINED.contains(&name) => Err(CMapError::Unported(Unported(format!(
            "the predefined CMap {name}, which the port does not hold"
        )))),
        _ => Err(malformed(&format!(
            "Error: Could not find referenced cmap stream {name}"
        ))),
    }
}

/// A token of FontBox's CMap parser.
#[derive(Debug, Clone)]
enum Token {
    Comment,
    Text(String),
    EndDictionary,
    EndArray,
    Array(Vec<Token>),
    Dictionary,
    Name(String),
    Integer(i32),
    Real(f64),
    Bytes(Vec<u8>),
    Operator(String),
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

fn is_whitespace_or_eof(byte: Option<u8>) -> bool {
    matches!(byte, None | Some(0x20 | 0x0D | 0x0A))
}

fn is_delimiter(byte: Option<u8>) -> bool {
    matches!(
        byte,
        Some(b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
    )
}

impl Parser<'_> {
    fn read(&mut self) -> Option<u8> {
        let byte = self.data.get(self.pos).copied();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    fn rewind(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    fn next_token(&mut self) -> Result<Option<Token>, CMapError> {
        let mut byte = self.read();
        while matches!(byte, Some(0x09 | 0x20 | 0x0D | 0x0A)) {
            byte = self.read();
        }
        let Some(first) = byte else {
            return Ok(None);
        };
        Ok(Some(match first {
            b'%' => {
                while !matches!(self.read(), None | Some(0x0D | 0x0A)) {}
                Token::Comment
            }
            b'(' => {
                let mut text = String::new();
                loop {
                    match self.read() {
                        None | Some(b')') => break,
                        Some(byte) => text.push(char::from(byte)),
                    }
                }
                Token::Text(text)
            }
            b'>' => {
                if self.read() == Some(b'>') {
                    Token::EndDictionary
                } else {
                    return Err(malformed("Error: expected the end of a dictionary."));
                }
            }
            b']' => Token::EndArray,
            b'[' => {
                let mut items = Vec::new();
                loop {
                    match self.next_token()? {
                        None | Some(Token::EndArray) => break,
                        Some(token) => items.push(token),
                    }
                }
                Token::Array(items)
            }
            b'<' => self.read_dictionary_or_bytes()?,
            b'/' => {
                let mut name = String::new();
                let mut byte = self.read();
                while !is_whitespace_or_eof(byte) && !is_delimiter(byte) {
                    name.push(char::from(byte.unwrap_or_default()));
                    byte = self.read();
                }
                if is_delimiter(byte) {
                    self.rewind();
                }
                Token::Name(name)
            }
            b'0'..=b'9' => {
                let mut text = String::from(char::from(first));
                let mut byte = self.read();
                while !is_whitespace_or_eof(byte)
                    && (byte.is_some_and(|b| b.is_ascii_digit()) || byte == Some(b'.'))
                {
                    text.push(char::from(byte.unwrap_or_default()));
                    byte = self.read();
                }
                if byte.is_some() {
                    self.rewind();
                }
                if text.contains('.') {
                    Token::Real(
                        text.parse::<f64>()
                            .map_err(|_| malformed(&format!("Invalid number '{text}'")))?,
                    )
                } else {
                    Token::Integer(
                        crate::formats::java::parse_int(&text)
                            .ok_or_else(|| malformed(&format!("Invalid number '{text}'")))?,
                    )
                }
            }
            _ => {
                let mut text = String::from(char::from(first));
                let mut byte = self.read();
                while !is_whitespace_or_eof(byte)
                    && !is_delimiter(byte)
                    && !byte.is_some_and(|b| b.is_ascii_digit())
                {
                    text.push(char::from(byte.unwrap_or_default()));
                    byte = self.read();
                }
                if is_delimiter(byte) || byte.is_some_and(|b| b.is_ascii_digit()) {
                    self.rewind();
                }
                Token::Operator(text)
            }
        }))
    }

    fn read_dictionary_or_bytes(&mut self) -> Result<Token, CMapError> {
        let mut byte = self.read();
        if byte == Some(b'<') {
            let mut key = self.next_token()?;
            while let Some(Token::Name(name)) = &key {
                if name == ">>" {
                    break;
                }
                self.next_token()?;
                key = self.next_token()?;
            }
            return Ok(Token::Dictionary);
        }
        let mut bytes: Vec<u8> = Vec::new();
        let mut high = true;
        while let Some(current) = byte {
            if current == b'>' {
                break;
            }
            if is_whitespace_or_eof(Some(current)) {
                byte = self.read();
                continue;
            }
            let value = match current {
                b'0'..=b'9' => current - b'0',
                b'A'..=b'F' => current - b'A' + 10,
                b'a'..=b'f' => current - b'a' + 10,
                _ => {
                    return Err(malformed(&format!(
                        "Error: expected hex character and not {}",
                        char::from(current)
                    )))
                }
            };
            if high {
                if bytes.len() >= 512 {
                    return Err(malformed("cmap token ist larger than buffer size 512"));
                }
                bytes.push(value * 16);
            } else if let Some(last) = bytes.last_mut() {
                *last = last.wrapping_add(value);
            }
            high = !high;
            byte = self.read();
        }
        Ok(Token::Bytes(bytes))
    }

    fn integer(&mut self) -> Result<i32, CMapError> {
        match self.next_token()? {
            None => Err(malformed("expected integer value is missing")),
            Some(Token::Integer(value)) => Ok(value),
            Some(_) => Err(malformed("invalid type for next token")),
        }
    }

    fn bytes(&mut self) -> Result<Vec<u8>, CMapError> {
        match self.next_token()? {
            None => Err(malformed("expected byte[] value is missing")),
            Some(Token::Bytes(bytes)) => Ok(bytes),
            Some(_) => Err(malformed("invalid type for next token")),
        }
    }
}

/// `CMapParser.createStringFromBytes`.
fn string_from_bytes(bytes: &[u8]) -> String {
    match bytes.len() {
        0 => "\u{0}".to_owned(),
        1 => char::from(bytes[0]).to_string(),
        _ => decode_java("UTF-16BE", bytes),
    }
}

/// `CMapParser.increment`.
fn increment(data: &mut [u8], position: usize, strict: bool) -> bool {
    if position > 0 && data[position] == 255 {
        if strict {
            return false;
        }
        data[position] = 0;
        increment(data, position - 1, strict);
    } else {
        data[position] = data[position].wrapping_add(1);
    }
    true
}

/// `CMapParser.parse`, not strict, of a map read from a document.
pub(super) fn parse(data: &[u8]) -> Result<CMap, CMapError> {
    let mut parser = Parser { data, pos: 0 };
    let mut cmap = CMap::new();
    let mut previous: Option<Token> = None;
    while let Some(token) = parser.next_token()? {
        let count = match &previous {
            Some(Token::Integer(count)) => Some(*count),
            Some(Token::Real(count)) => Some(*count as i32),
            _ => None,
        };
        match &token {
            Token::Operator(op) => {
                if op == "endcmap" {
                    break;
                }
                match (op.as_str(), &previous, count) {
                    ("usecmap", Some(Token::Name(name)), _) => {
                        let used = predefined(name)?;
                        cmap.use_cmap(used);
                    }
                    ("begincodespacerange", _, Some(count)) => {
                        codespace_ranges(&mut parser, count, &mut cmap)?;
                    }
                    ("beginbfchar", _, Some(count)) => {
                        bf_chars(&mut parser, count, &mut cmap)?;
                    }
                    ("beginbfrange", _, Some(count)) => {
                        bf_ranges(&mut parser, count, &mut cmap)?;
                    }
                    ("begincidchar", _, Some(count)) => {
                        for _ in 0..count {
                            let code = match parser.next_token()? {
                                Some(Token::Operator(op)) => {
                                    expect(&op, "endcidchar", "cidchar")?;
                                    break;
                                }
                                Some(Token::Bytes(code)) => code,
                                _ => return Err(malformed("input code missing")),
                            };
                            let cid = parser.integer()?;
                            cmap.add_cid_mapping(&code, cid as u32);
                        }
                    }
                    ("begincidrange", Some(Token::Integer(count)), _) => {
                        for _ in 0..*count {
                            let start = match parser.next_token()? {
                                Some(Token::Operator(op)) => {
                                    expect(&op, "endcidrange", "cidrange")?;
                                    break;
                                }
                                Some(Token::Bytes(code)) => code,
                                _ => return Err(malformed("start code missing")),
                            };
                            let end = parser.bytes()?;
                            let cid = parser.integer()?;
                            if start.len() != end.len() {
                                return Err(malformed(
                                    "Error : ~cidrange values must not have different byte lengths",
                                ));
                            }
                            if start == end {
                                cmap.add_cid_mapping(&start, cid as u32);
                            } else {
                                cmap.add_cid_range(&start, &end, cid as u32);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Token::Name(name) => literal_name(&mut parser, name, &mut cmap)?,
            _ => {}
        }
        previous = Some(token);
    }
    Ok(cmap)
}

fn expect(operator: &str, expected: &str, range: &str) -> Result<(), CMapError> {
    if operator == expected {
        Ok(())
    } else {
        Err(malformed(&format!(
            "Error : ~{range} contains an unexpected operator : {operator}"
        )))
    }
}

fn literal_name(parser: &mut Parser, name: &str, cmap: &mut CMap) -> Result<(), CMapError> {
    match name {
        "WMode" => {
            if let Some(Token::Integer(value)) = parser.next_token()? {
                cmap.wmode = value;
            }
        }
        "CMapName" => {
            if let Some(Token::Name(value)) = parser.next_token()? {
                cmap.name = Some(value);
            }
        }
        "CMapVersion" | "CMapType" => {
            parser.next_token()?;
        }
        "Registry" => {
            if let Some(Token::Text(value)) = parser.next_token()? {
                cmap.registry = Some(value);
            }
        }
        "Ordering" => {
            if let Some(Token::Text(value)) = parser.next_token()? {
                cmap.ordering = Some(value);
            }
        }
        "Supplement" => {
            if let Some(Token::Integer(value)) = parser.next_token()? {
                cmap.supplement = value;
            }
        }
        _ => {}
    }
    Ok(())
}

fn codespace_ranges(parser: &mut Parser, count: i32, cmap: &mut CMap) -> Result<(), CMapError> {
    for _ in 0..count {
        let start = match parser.next_token()? {
            Some(Token::Operator(op)) => {
                expect(&op, "endcodespacerange", "codespacerange")?;
                break;
            }
            Some(Token::Bytes(start)) => start,
            _ => return Err(malformed("start range missing")),
        };
        let end = parser.bytes()?;
        cmap.add_codespace_range(CodespaceRange::new(&start, &end)?);
    }
    Ok(())
}

fn bf_chars(parser: &mut Parser, count: i32, cmap: &mut CMap) -> Result<(), CMapError> {
    for _ in 0..count {
        let code = match parser.next_token()? {
            Some(Token::Operator(op)) => {
                expect(&op, "endbfchar", "bfchar")?;
                break;
            }
            Some(Token::Bytes(code)) => code,
            _ => return Err(malformed("input code missing")),
        };
        match parser.next_token()? {
            Some(Token::Bytes(bytes)) => cmap.add_char_mapping(&code, string_from_bytes(&bytes)),
            Some(Token::Name(name)) => cmap.add_char_mapping(&code, name),
            _ => {
                return Err(malformed(
                    "Error parsing CMap beginbfchar, expected{COSString or COSName}",
                ))
            }
        }
    }
    Ok(())
}

fn bf_ranges(parser: &mut Parser, count: i32, cmap: &mut CMap) -> Result<(), CMapError> {
    for _ in 0..count {
        let mut start = match parser.next_token()? {
            Some(Token::Operator(op)) => {
                expect(&op, "endbfrange", "bfrange")?;
                break;
            }
            Some(Token::Bytes(code)) => code,
            _ => return Err(malformed("start code missing")),
        };
        let end = match parser.next_token()? {
            Some(Token::Operator(op)) => {
                expect(&op, "endbfrange", "bfrange")?;
                break;
            }
            Some(Token::Bytes(code)) => code,
            _ => return Err(malformed("end code missing")),
        };
        let (first, last) = (to_int_java(&start), to_int_java(&end));
        if last < first {
            break;
        }
        match parser.next_token()? {
            Some(Token::Array(items)) => {
                let span = i64::from(last) - i64::from(first);
                if !items.is_empty() && items.len() as i64 >= span {
                    for item in items {
                        let Token::Bytes(bytes) = item else {
                            return Err(CMapError::Unported(Unported(
                                "a bfrange array holding something other than strings, which \
                                 FontBox fails on"
                                    .to_owned(),
                            )));
                        };
                        cmap.add_char_mapping(&start, string_from_bytes(&bytes));
                        if !start.is_empty() {
                            let position = start.len() - 1;
                            increment(&mut start, position, false);
                        }
                    }
                }
            }
            Some(Token::Bytes(mut bytes)) if !bytes.is_empty() => {
                if bytes.len() == 2 && first == 0 && last == 0xFFFF && bytes == [0, 0] {
                    for high in 0..=255u8 {
                        start[0] = high;
                        start[1] = 0;
                        bytes[0] = high;
                        bytes[1] = 0;
                        range_mappings(cmap, &mut start, 256, &mut bytes);
                    }
                } else {
                    let values = i64::from(last) - i64::from(first) + 1;
                    range_mappings(cmap, &mut start, values, &mut bytes);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// `CMapParser.addMappingFrombfrange` with a start value: consecutive codes to consecutive
/// strings, stopping where the string's last byte would overflow in strict mode.
fn range_mappings(cmap: &mut CMap, start: &mut [u8], values: i64, bytes: &mut [u8]) {
    for _ in 0..values {
        cmap.add_char_mapping(start, string_from_bytes(bytes));
        if bytes.is_empty() || !increment(bytes, bytes.len() - 1, false) {
            break;
        }
        if !start.is_empty() {
            let position = start.len() - 1;
            increment(start, position, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TO_UNICODE: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap
/CMapName /Adobe-Identity-UCS def /CMapType 2 def
1 begincodespacerange <00> <FF> endcodespacerange
2 beginbfchar <01> <0048> <02> <0020> endbfchar
1 beginbfrange <03> <05> <0061> endbfrange
1 beginbfrange <06> <07> [<00660069> <D83DDE00>] endbfrange
endcmap CMapName currentdict /CMap defineresource pop end end";

    #[test]
    fn reads_to_unicode_maps() {
        let cmap = parse(TO_UNICODE).unwrap();
        assert_eq!(cmap.name.as_deref(), Some("Adobe-Identity-UCS"));
        assert_eq!(cmap.to_unicode(1), Some("H"));
        assert_eq!(cmap.to_unicode(4), Some("b"));
        assert_eq!(cmap.to_unicode(6), Some("fi"));
        assert_eq!(cmap.to_unicode(7), Some("\u{1F600}"));
        assert_eq!(cmap.to_unicode(9), None);
        assert_eq!(cmap.space_mapping(), 2);
        assert!(cmap.has_unicode_mappings());
        assert!(!cmap.has_cid_mappings());
        assert_eq!(cmap.read_code(&[0x41, 0x42]), Some((0x41, 1)));
    }

    #[test]
    fn identity_maps_two_byte_codes_to_themselves() {
        let identity = predefined("Identity-H").unwrap();
        assert_eq!(identity.read_code(&[0x12, 0x34, 0x56]), Some((0x1234, 2)));
        assert_eq!(identity.to_cid(0x1234), 0x1234);
        assert_eq!(identity.wmode, 0);
        assert_eq!(predefined("Identity-V").unwrap().wmode, 1);
        assert!(matches!(
            predefined("UniJIS-UCS2-H"),
            Err(CMapError::Unported(_))
        ));
        assert!(matches!(
            predefined("NoSuchMap"),
            Err(CMapError::Malformed(_))
        ));
    }

    #[test]
    fn a_one_byte_string_is_latin1_and_a_lone_surrogate_is_replaced() {
        assert_eq!(string_from_bytes(&[0xE6]), "æ");
        assert_eq!(string_from_bytes(&[0xD8, 0x00]), "\u{FFFD}");
    }
}
