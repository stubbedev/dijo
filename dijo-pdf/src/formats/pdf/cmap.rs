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
    one_byte: IntegerHashMap<String>,
    two_bytes: IntegerHashMap<String>,
    more_bytes: IntegerHashMap<String>,
    /// `unicodeToByteCodes`: the code of each text, the last mapped to it.
    unicode_to_codes: FastMap<String, Vec<u8>>,
    /// Whether a map this one uses gave texts codes in an order Java's hash maps hold but the
    /// port does not know: that of a bucket turned into a tree.
    codes_unordered: bool,
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
            one_byte: IntegerHashMap::default(),
            two_bytes: IntegerHashMap::default(),
            more_bytes: IntegerHashMap::default(),
            unicode_to_codes: FastMap::default(),
            codes_unordered: false,
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
            1 => self.one_byte.get(code),
            2 => self.two_bytes.get(code),
            _ => self.more_bytes.get(code),
        }
        .map(String::as_str)
    }

    /// `CMap.getCodesFromUnicode`: the code `unicode` was last mapped from, or why the port
    /// does not know it: a map that uses another whose codes FontBox adds in the order of a
    /// hash map's bucket turned into a tree.
    pub(super) fn codes_from_unicode(&self, unicode: &str) -> Result<Option<&[u8]>, Unported> {
        if self.codes_unordered {
            return Err(Unported(
                "a CMap using another whose Unicode mappings a Java hash map holds in a bucket \
                 turned into a tree, whose order the port does not follow"
                    .to_owned(),
            ));
        }
        Ok(self.unicode_to_codes.get(unicode).map(Vec::as_slice))
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
        let map = match codes.len() {
            1 => &mut self.one_byte,
            2 => &mut self.two_bytes,
            3 | 4 => &mut self.more_bytes,
            _ => return,
        };
        map.put(to_int(codes), unicode.clone());
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

    /// `CMap.useCmap`: the other map's ranges and mappings added to this one's, its Unicode
    /// mappings each giving its text its code in the order Java's hash maps hold them, so that
    /// the last of the codes of a text in that order is the one kept. FontBox writes a one-byte
    /// code `k` as `k % 0xFF`, which takes 255 for 0 (#125).
    fn use_cmap(&mut self, other: &CMap) {
        for range in &other.codespace_ranges {
            self.add_codespace_range(range.clone());
        }
        self.one_byte.put_all(&other.one_byte);
        self.two_bytes.put_all(&other.two_bytes);
        self.more_bytes.put_all(&other.more_bytes);
        let maps = [&other.one_byte, &other.two_bytes, &other.more_bytes];
        self.codes_unordered |= maps.iter().any(|map| !map.ordered());
        for (code, unicode) in other.one_byte.in_java_order() {
            self.unicode_to_codes
                .insert(unicode.clone(), vec![(code % 0xFF) as u8]);
        }
        for (code, unicode) in other.two_bytes.in_java_order() {
            self.unicode_to_codes
                .insert(unicode.clone(), (code as u16).to_be_bytes().to_vec());
        }
        for (code, unicode) in other.more_bytes.in_java_order() {
            let bytes = code.to_be_bytes();
            let codes = match code <= 0xFF_FFFF {
                true => bytes[1..].to_vec(),
                false => bytes.to_vec(),
            };
            self.unicode_to_codes.insert(unicode.clone(), codes);
        }
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

/// `HashMap.DEFAULT_INITIAL_CAPACITY`.
const DEFAULT_CAPACITY: usize = 16;
/// `HashMap.TREEIFY_THRESHOLD`: the entries a bucket holds before one more turns it into a tree.
const TREEIFY_THRESHOLD: u32 = 8;
/// `HashMap.MIN_TREEIFY_CAPACITY`: the table a bucket turns into a tree in, at least; a smaller
/// one doubles instead.
const MIN_TREEIFY_CAPACITY: usize = 64;

/// A `java.util.HashMap<Integer, V>` as it fills, iterated in Java's order: by bucket of its
/// table, then in the order the entries came within a bucket. A bucket of more entries than
/// [`TREEIFY_THRESHOLD`] turns into a tree, whose order is not followed: the map is then no
/// longer [`IntegerHashMap::ordered`].
#[derive(Debug, Clone)]
struct IntegerHashMap<V> {
    /// The entries in the order they came.
    entries: Vec<(u32, V)>,
    /// Where each key's entry stands.
    index: FastMap<u32, usize>,
    /// The table's length, 0 before the first entry allocates it.
    capacity: usize,
    /// `threshold`: the size past which the table doubles, or, before the table is allocated,
    /// the length it is allocated at, if a `putAll` set it.
    threshold: usize,
    /// How many entries each bucket of the table holds.
    buckets: Vec<u32>,
    /// Whether no bucket turned into a tree.
    ordered: bool,
}

impl<V> Default for IntegerHashMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: FastMap::default(),
            capacity: 0,
            threshold: 0,
            buckets: Vec::new(),
            ordered: true,
        }
    }
}

impl<V: Clone> IntegerHashMap<V> {
    fn get(&self, key: u32) -> Option<&V> {
        self.index.get(&key).map(|&at| &self.entries[at].1)
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the map iterates in [`IntegerHashMap::in_java_order`].
    fn ordered(&self) -> bool {
        self.ordered
    }

    /// `Integer.hashCode` spread as `HashMap.hash` spreads it.
    fn hash(key: u32) -> usize {
        (key ^ (key >> 16)) as usize
    }

    fn bucket(&self, key: u32) -> usize {
        Self::hash(key) & (self.capacity - 1)
    }

    /// `put`: a key already present keeps its place with the new value.
    fn put(&mut self, key: u32, value: V) {
        if let Some(&at) = self.index.get(&key) {
            self.entries[at].1 = value;
            return;
        }
        if self.capacity == 0 {
            self.resize();
        }
        let bucket = self.bucket(key);
        self.buckets[bucket] += 1;
        self.index.insert(key, self.entries.len());
        self.entries.push((key, value));
        if self.buckets[bucket] > TREEIFY_THRESHOLD {
            match self.capacity < MIN_TREEIFY_CAPACITY {
                true => self.resize(),
                false => self.ordered = false,
            }
        }
        if self.entries.len() > self.threshold {
            self.resize();
        }
    }

    /// `putAll`, as `putMapEntries` sizes the table first.
    fn put_all(&mut self, other: &Self) {
        let size = other.entries.len();
        if size == 0 {
            return;
        }
        if self.capacity == 0 {
            let wanted = (size * 4).div_ceil(3);
            if wanted > self.threshold {
                self.threshold = wanted.next_power_of_two();
            }
        } else {
            while size > self.threshold {
                self.resize();
            }
        }
        self.ordered &= other.ordered;
        for (key, value) in other.in_java_order() {
            self.put(key, value.clone());
        }
    }

    /// `resize`: the table allocated, at the length a `putAll` set or the default one, or
    /// doubled.
    fn resize(&mut self) {
        self.capacity = match (self.capacity, self.threshold) {
            (0, 0) => DEFAULT_CAPACITY,
            (0, presized) => presized,
            (capacity, _) => capacity * 2,
        };
        self.threshold = self.capacity * 3 / 4;
        self.buckets = vec![0; self.capacity];
        for at in 0..self.entries.len() {
            let bucket = self.bucket(self.entries[at].0);
            self.buckets[bucket] += 1;
        }
    }

    /// The entries in the order Java iterates them, if the map is
    /// [`IntegerHashMap::ordered`]; otherwise in an order Java's may differ from.
    fn in_java_order(&self) -> impl Iterator<Item = (u32, &V)> {
        let mut order: Vec<(usize, usize)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(at, (key, _))| (self.bucket(*key), at))
            .collect();
        order.sort_unstable();
        order.into_iter().map(|(_, at)| {
            let (key, value) = &self.entries[at];
            (*key, value)
        })
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
    /// one code to, in a map using the bundled one, as `UsedCMapCodes.java` writes it.
    const USED_CMAP_CODES: &str = include_str!("../../../tests/fixtures/pdf/used-cmap-codes.tsv");

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn gives_texts_the_codes_of_a_used_map_in_javas_hash_order() {
        let mut checked = 0;
        let mut using: Option<(&str, CMap)> = None;
        for line in USED_CMAP_CODES
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let [name, unicode, codes] = line.split('\t').collect::<Vec<_>>()[..] else {
                panic!("{line}");
            };
            if using.as_ref().is_none_or(|(used, _)| *used != name) {
                let parsed = parse(format!("/{name} usecmap\n").as_bytes()).unwrap();
                using = Some((name, parsed));
            }
            let (_, using) = using.as_ref().unwrap();
            let unicode = decode_java("UTF-16BE", &hex(unicode));
            let found = using.codes_from_unicode(&unicode).unwrap();
            assert_eq!(found, Some(hex(codes).as_slice()), "{line}");
            checked += 1;
        }
        assert_eq!(checked, 3104);
        for name in BUNDLED.keys() {
            let cmap = predefined(name).unwrap();
            assert!(cmap.more_bytes.is_empty(), "{name}");
            let maps = [&cmap.one_byte, &cmap.two_bytes];
            assert!(maps.iter().all(|map| map.ordered()), "{name}");
        }
    }

    #[test]
    fn iterates_integer_keys_by_bucket_then_as_they_came() {
        let mut map = IntegerHashMap::default();
        for key in [33, 1, 17, 2] {
            map.put(key, ());
        }
        let keys = |map: &IntegerHashMap<()>| map.in_java_order().map(|(key, _)| key).collect();
        let order: Vec<u32> = keys(&map);
        assert_eq!(order, [33, 1, 17, 2]);
        for key in 100..109 {
            map.put(key, ());
        }
        assert_eq!(map.capacity, 32);
        assert_eq!(&keys(&map)[..4], [33, 1, 2, 100]);
        let mut presized = IntegerHashMap::default();
        presized.put_all(&map);
        assert_eq!(presized.capacity, 32);
        let mut crowded = IntegerHashMap::default();
        for key in 0..10 {
            crowded.put(key * 64, ());
        }
        assert_eq!(crowded.capacity, 64);
        assert!(crowded.ordered());
        crowded.put(640, ());
        assert!(!crowded.ordered());
    }

    #[test]
    fn a_one_byte_string_is_latin1_and_a_lone_surrogate_is_replaced() {
        assert_eq!(string_from_bytes(&[0xE6]), "æ");
        assert_eq!(string_from_bytes(&[0xD8, 0x00]), "\u{FFFD}");
    }
}
