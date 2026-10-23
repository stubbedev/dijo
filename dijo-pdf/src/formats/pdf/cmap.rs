//! FontBox's `CMap` and `CMapParser`: the character maps of composite fonts and the
//! `ToUnicode` maps of all fonts, read as FontBox reads them, and the predefined Identity maps.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{LazyLock, OnceLock};

use super::cos::decode_java;
use super::Unported;
use crate::hash::FastMap;

/// The CMaps FontBox 3.0.8 bundles (`org/apache/fontbox/cmap` of its jar), by name, as a TAR
/// archive compressed with xz.
const BUNDLE: &[u8] = include_bytes!("../../../models/pdf/cmaps.tar.xz");

/// The bundled CMaps, by name.
static BUNDLED: LazyLock<BTreeMap<String, Vec<u8>>> = LazyLock::new(|| {
    let tar = lzma_rust2::XzReader::new(BUNDLE, false);
    let mut archive = tar::Archive::new(tar);
    let mut maps = BTreeMap::new();
    for entry in archive
        .entries()
        .expect("the bundled CMaps are a TAR archive")
    {
        let mut entry = entry.expect("a bundled CMap reads");
        let name = entry
            .path()
            .expect("a bundled CMap has a name")
            .to_string_lossy()
            .into_owned();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).expect("a bundled CMap reads");
        maps.insert(name, data);
    }
    maps
});

/// The bundled CMaps, parsed once each when first used, by name.
static PARSED: LazyLock<BTreeMap<&'static str, OnceLock<Result<CMap, CMapError>>>> =
    LazyLock::new(|| {
        BUNDLED
            .keys()
            .map(|name| (name.as_str(), OnceLock::new()))
            .collect()
    });

/// Why a CMap was not read: FontBox throws an `IOException` on it, which PDFBox catches for a
/// `ToUnicode` map, or the map needs something the port does not hold.
#[derive(Debug, Clone)]
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

    fn is_full_match<'a>(&self, code: impl ExactSizeIterator<Item = &'a u8>) -> bool {
        code.len() == self.len()
            && code
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
    one_byte: FastMap<u32, String>,
    two_bytes: FastMap<u32, String>,
    more_bytes: FastMap<u32, String>,
    /// `unicodeToByteCodes`: the code of each text, the last mapped to it.
    unicode_to_codes: FastMap<String, Vec<u8>>,
    code_to_cid: FastMap<usize, FastMap<u32, u32>>,
    cid_ranges: Vec<CidRange>,
    space_mapping: i64,
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
            one_byte: FastMap::default(),
            two_bytes: FastMap::default(),
            more_bytes: FastMap::default(),
            unicode_to_codes: FastMap::default(),
            code_to_cid: FastMap::default(),
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

    /// `CMap.getCodesFromUnicode`: the code `unicode` was last mapped from.
    pub(super) fn codes_from_unicode(&self, unicode: &str) -> Option<&[u8]> {
        self.unicode_to_codes.get(unicode).map(Vec::as_slice)
    }

    /// `CMap.readCode`: the code at the start of `bytes`, and how many bytes it takes, as
    /// FontBox reads it from a stream; `None` where FontBox fails because the map has no
    /// codespace ranges.
    pub(super) fn read_code(&self, bytes: &[u8]) -> Option<(u32, usize)> {
        if self.min_code_length == 0 || self.min_code_length > self.max_code_length {
            return None;
        }
        let code = |count: usize| {
            (0..count).map(move |index| match bytes.get(index) {
                Some(byte) => byte,
                None if index < self.min_code_length => &0,
                None => &0xFF,
            })
        };
        for count in self.min_code_length..=self.max_code_length {
            if self
                .codespace_ranges
                .iter()
                .any(|range| range.is_full_match(code(count)))
            {
                return Some((to_int(code(count)), count.min(bytes.len())));
            }
        }
        Some((
            to_int(code(self.min_code_length)),
            self.min_code_length.min(bytes.len()),
        ))
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
        let map = match codes.len() {
            1 => &mut self.one_byte,
            2 => &mut self.two_bytes,
            3 | 4 => &mut self.more_bytes,
            _ => return,
        };
        map.insert(to_int(codes), unicode.clone());
        self.unicode_to_codes.insert(unicode, codes.to_vec());
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

    /// `CMap.useCmap`: the other map's ranges and mappings added to this one's, each text
    /// keeping the code the other map last mapped to it. FontBox instead gives each text the
    /// last of its codes in the order of a Java hash map, and writes a one-byte code `k` as
    /// `k % 0xFF`, which turns 255 into 0; the port keeps the other map's own codes (#186).
    fn use_cmap(&mut self, other: &CMap) {
        for range in &other.codespace_ranges {
            self.add_codespace_range(range.clone());
        }
        let copy = |to: &mut FastMap<u32, String>, from: &FastMap<u32, String>| {
            to.extend(from.iter().map(|(&code, unicode)| (code, unicode.clone())));
        };
        copy(&mut self.one_byte, &other.one_byte);
        copy(&mut self.two_bytes, &other.two_bytes);
        copy(&mut self.more_bytes, &other.more_bytes);
        self.unicode_to_codes.extend(
            other
                .unicode_to_codes
                .iter()
                .map(|(unicode, codes)| (unicode.clone(), codes.clone())),
        );
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
fn to_int<'a>(bytes: impl IntoIterator<Item = &'a u8>) -> u32 {
    bytes
        .into_iter()
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

/// `CMapManager.getPredefinedCMap`: the bundled map of that name, parsed as `parsePredefined`
/// parses it, the Identity maps made as FontBox's files define them.
pub(super) fn predefined(name: &str) -> Result<&'static CMap, CMapError> {
    match name {
        "Identity-H" => return Ok(&IDENTITY_H),
        "Identity-V" => return Ok(&IDENTITY_V),
        _ => {}
    }
    let (Some(cell), Some(data)) = (PARSED.get(name), BUNDLED.get(name)) else {
        return Err(malformed(&format!(
            "Error: Could not find referenced cmap stream {name}"
        )));
    };
    cell.get_or_init(|| parse(data))
        .as_ref()
        .map_err(Clone::clone)
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
                        crate::java::parse_int(&text)
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
        let unicode = predefined("UniJIS-UCS2-H").unwrap();
        assert_eq!(unicode.registry.as_deref(), Some("Adobe"));
        assert_eq!(unicode.ordering.as_deref(), Some("Japan1"));
        assert_eq!(unicode.to_cid(0x3042), 843);
        let ucs2 = predefined("Adobe-Japan1-UCS2").unwrap();
        assert_eq!(ucs2.to_unicode(843), Some("\u{3042}"));
        for name in BUNDLED.keys() {
            assert!(predefined(name).is_ok(), "{name}");
        }
        assert!(matches!(
            predefined("NoSuchMap"),
            Err(CMapError::Malformed(_))
        ));
    }

    /// What FontBox's `getCodesFromUnicode` gives each text that a bundled map maps more than
    /// one code to, in a map using the bundled one, as `UsedCMapCodes.java` writes it: the last
    /// of the text's codes in the order of a Java hash map.
    const USED_CMAP_CODES: &str = include_str!("../../../tests/fixtures/pdf/used-cmap-codes.tsv");

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    /// A map using a bundled one gives each text the code the bundled map last mapped to it,
    /// which maps back to the text, not the code FontBox's hash order leaves (#186).
    #[test]
    fn gives_texts_the_codes_a_used_map_last_mapped_to_them() {
        let mut checked = 0;
        let mut unlike_java = 0;
        let mut using: Option<(&str, CMap)> = None;
        for line in USED_CMAP_CODES
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let [name, unicode, java] = line.split('\t').collect::<Vec<_>>()[..] else {
                panic!("{line}");
            };
            if using.as_ref().is_none_or(|(used, _)| *used != name) {
                let parsed = parse(format!("/{name} usecmap\n").as_bytes()).unwrap();
                using = Some((name, parsed));
            }
            let (_, using) = using.as_ref().unwrap();
            let unicode = decode_java("UTF-16BE", &hex(unicode));
            let found = using.codes_from_unicode(&unicode).unwrap();
            let used = predefined(name).unwrap();
            assert_eq!(Some(found), used.codes_from_unicode(&unicode), "{line}");
            assert_eq!(
                used.to_unicode_length(to_int(found), found.len()),
                Some(unicode.as_str()),
                "{line}"
            );
            unlike_java += usize::from(found != hex(java).as_slice());
            checked += 1;
        }
        assert_eq!(checked, 3104);
        assert!(unlike_java > 0);
    }

    /// A one-byte code 255 of a used map stays 255, where FontBox writes it as `255 % 0xFF`, 0.
    #[test]
    fn keeps_a_used_one_byte_code_255() {
        let mut used = CMap::new();
        used.add_char_mapping(&[0xFF], "y".to_owned());
        let mut using = CMap::new();
        using.use_cmap(&used);
        assert_eq!(using.codes_from_unicode("y"), Some(&[0xFF][..]));
        assert_eq!(using.to_unicode_length(0xFF, 1), Some("y"));
    }

    /// `CMap.readCode` as FontBox writes it, reading into a buffer of the longest code's length.
    fn read_code_into_buffer(cmap: &CMap, bytes: &[u8]) -> Option<(u32, usize)> {
        if cmap.min_code_length == 0 || cmap.min_code_length > cmap.max_code_length {
            return None;
        }
        let mut code = vec![0u8; cmap.max_code_length];
        let first = cmap.min_code_length.min(bytes.len());
        code[..first].copy_from_slice(&bytes[..first]);
        let mut consumed = first;
        for index in cmap.min_code_length - 1..cmap.max_code_length {
            let count = index + 1;
            if cmap
                .codespace_ranges
                .iter()
                .any(|range| range.is_full_match(code[..count].iter()))
            {
                return Some((to_int(&code[..count]), consumed));
            }
            if count < cmap.max_code_length {
                match bytes.get(consumed) {
                    Some(&byte) => {
                        code[count] = byte;
                        consumed += 1;
                    }
                    None => code[count] = 0xFF,
                }
            }
        }
        Some((to_int(&code[..cmap.min_code_length]), first))
    }

    #[test]
    fn reads_codes_as_a_buffer_of_the_longest_code_reads_them() {
        let maps: [&[u8]; 6] = [
            b"",
            b"1 begincodespacerange <00> <80> endcodespacerange",
            b"2 begincodespacerange <00> <80> <8140> <9FFC> endcodespacerange",
            b"2 begincodespacerange <0000> <FFFF> <00> <FF> endcodespacerange",
            b"2 begincodespacerange <A1> <FE> <8EA1A1A1> <8EFEFEFE> endcodespacerange",
            b"2 begincodespacerange <000000> <0000FF> <0000000000> <FFFFFFFFFF> \
               endcodespacerange",
        ];
        let inputs: [&[u8]; 12] = [
            &[],
            &[0x00],
            &[0x41, 0x42],
            &[0x81],
            &[0x81, 0x40],
            &[0x8E, 0xA1],
            &[0x8E, 0xA1, 0xA1, 0xA1, 0x20],
            &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            &[0x00, 0x00],
            &[0x00, 0x00, 0x12, 0x34, 0x56],
            &[0xA1, 0xFE, 0xFF],
            &[0x90, 0x00, 0x00],
        ];
        let mut cmaps: Vec<&CMap> = vec![predefined("Identity-H").unwrap()];
        let parsed: Vec<CMap> = maps.iter().map(|map| parse(map).unwrap()).collect();
        cmaps.extend(&parsed);
        cmaps.push(predefined("UniJIS-UCS2-H").unwrap());
        for cmap in cmaps {
            for bytes in inputs {
                assert_eq!(
                    cmap.read_code(bytes),
                    read_code_into_buffer(cmap, bytes),
                    "{bytes:?}"
                );
            }
        }
    }

    #[test]
    fn a_one_byte_string_is_latin1_and_a_lone_surrogate_is_replaced() {
        assert_eq!(string_from_bytes(&[0xE6]), "æ");
        assert_eq!(string_from_bytes(&[0xD8, 0x00]), "\u{FFFD}");
    }
}
