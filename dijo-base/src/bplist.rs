//! Binary property lists as dd-plist 1.30's `BinaryPropertyListParser` reads them, for Tika's
//! `BPListDetector` and `PListParser` alike, read in place: [`Reader::validate`] fails where
//! dd-plist's parse of the whole tree fails, with offsets and lengths checked against the offset
//! table in Java's exact arithmetic, nesting up to 512 objects deep, cycles refused, each object
//! checked once however often it is referenced, and the `int` casts and array accesses Java
//! makes failing as a `RuntimeException` rather than a `PropertyListFormatException`. Each
//! object is then read on demand ([`Reader::node`]), from the top object down, so that only the
//! trailer and one bit per object are held, whatever the list's size.
//!
//! A port rather than a crate, as dd-plist's object model is what Tika reads and writes: a
//! dictionary is keyed by its keys' `toString`, whatever their objects, which the detector
//! follows and the parser refuses, and strings decode as the JDK decodes them. Unlike dd-plist,
//! a UID is named by its value, not by the number of its object; a UTF-8 string's characters
//! are measured by their real length, where dd-plist counts two bytes for each multibyte one;
//! and a dictionary keeps its null values, which dd-plist drops, though the detector still
//! looks only at keys with values (#104).

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use crate::input::{Pages, Paging};
use crate::java::dtoa::java_double_to_string;
use crate::text::charset::{Charset, Pieces};

const MAGIC: &[u8] = b"bplist";
const HEADER_AND_TRAILER: usize = 40;
const TRAILER: usize = 32;
/// `ParsedObjectStack.MAX_NESTING_DEPTH`.
const MAX_NESTING_DEPTH: usize = 512;
const SIMPLE_TYPE: u8 = 0x0;
const INT_TYPE: u8 = 0x1;
const REAL_TYPE: u8 = 0x2;
const DATE_TYPE: u8 = 0x3;
const DATA_TYPE: u8 = 0x4;
const ASCII_STRING_TYPE: u8 = 0x5;
const UTF16_STRING_TYPE: u8 = 0x6;
const UTF8_STRING_TYPE: u8 = 0x7;
const UID_TYPE: u8 = 0x8;
const ARRAY_TYPE: u8 = 0xA;
const ORDERED_SET_TYPE: u8 = 0xB;
const SET_TYPE: u8 = 0xC;
const DICTIONARY_TYPE: u8 = 0xD;
/// The keys `BPListDetector.detectOnKeys` tells lists apart by, in the order [`KindKeys`]
/// numbers them.
const KIND_KEYS: [&str; 7] = [
    "nodes",
    "edges",
    "graphEncodingVersion",
    "WebMainResource",
    "Playlists",
    "Tracks",
    "Music Folder",
];

/// What dd-plist throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thrown {
    /// A `PropertyListFormatException`.
    Format(String),
    /// A `RuntimeException`: an array index out of bounds or an illegal argument.
    Runtime(String),
}

fn format_error(reason: impl Into<String>) -> Thrown {
    Thrown::Format(reason.into())
}

/// The number of an object of a list, as references name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectId(i32);

/// An `NSNumber`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    Integer(i64),
    Real(f64),
    Boolean(bool),
}

impl Number {
    /// `NSNumber.toString`.
    pub fn text(self) -> String {
        match self {
            Self::Integer(value) => value.to_string(),
            Self::Boolean(value) => value.to_string(),
            Self::Real(value) if value.is_nan() => "nan".to_owned(),
            Self::Real(value) if value == f64::INFINITY => "+infinity".to_owned(),
            Self::Real(value) if value == f64::NEG_INFINITY => "-infinity".to_owned(),
            Self::Real(value) => java_double_to_string(value),
        }
    }
}

/// An object of a list, read in place: what it holds is read on demand.
#[derive(Debug, Clone, Copy)]
pub enum Node<'a> {
    /// What dd-plist holds as a null.
    Null,
    Number(Number),
    /// Seconds since 2001-01-01.
    Date(f64),
    Data(&'a [u8]),
    String(Text<'a>),
    Uid(u128),
    Array(Members),
    /// An ordered or unordered set, of whose members dd-plist keeps each reference once.
    Set(Members),
    Dict(Pairs),
}

/// The bytes of a string and the charset that decodes them.
#[derive(Debug, Clone, Copy)]
pub struct Text<'a> {
    bytes: &'a [u8],
    charset: &'static str,
}

impl<'a> Text<'a> {
    /// The string, as `NSString` decodes it.
    pub fn decode(self) -> String {
        Charset::named(self.charset)
            .decode(self.bytes)
            .expect("the JDK's Unicode and ASCII charsets decode natively")
    }

    /// The string decoded in pieces of at least `size` bytes each, as [`Text::decode`] decodes
    /// it whole.
    pub fn pieces(self, size: usize) -> Pieces<'a> {
        Charset::named(self.charset)
            .pieces(self.bytes, size)
            .expect("the JDK's Unicode and ASCII charsets decode natively")
    }
}

/// The references of an array or set, checked to lie within the offset table's bounds.
#[derive(Debug, Clone, Copy)]
pub struct Members {
    base: i32,
    count: i32,
}

/// The key and value references of a dictionary, checked to lie within the offset table's
/// bounds.
#[derive(Debug, Clone, Copy)]
pub struct Pairs {
    keys: i32,
    values: i32,
    count: i32,
}

/// What a property list holds, as `BPListDetector.detectOnKeys` tells it by the keys of its root
/// dictionary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Memgraph,
    WebArchive,
    Itunes,
    Plain,
}

impl ListKind {
    /// The media type of a binary property list of this kind.
    pub fn binary_type(self) -> &'static str {
        match self {
            Self::Memgraph => "application/x-bplist-memgraph",
            Self::WebArchive => "application/x-bplist-webarchive",
            Self::Itunes => "application/x-bplist-itunes",
            Self::Plain => "application/x-bplist",
        }
    }

    /// The media type of an XML property list of this kind, `detectXMLOnKeys`.
    pub fn xml_type(self) -> &'static str {
        match self {
            Self::Memgraph => "application/x-plist-memgraph",
            Self::WebArchive => "application/x-plist-webarchive",
            Self::Itunes => "application/x-plist-itunes",
            Self::Plain => "application/x-plist",
        }
    }
}

/// Which of the keys `BPListDetector.detectOnKeys` looks for a dictionary put with a value: the
/// keys of dd-plist's dictionary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindKeys(u8);

impl KindKeys {
    /// Notes that the key named `name` was put with a value.
    pub fn put(&mut self, name: &str) {
        if let Some(index) = KIND_KEYS.iter().position(|key| *key == name) {
            self.0 |= 1 << index;
        }
    }

    fn has(self, name: &str) -> bool {
        KIND_KEYS
            .iter()
            .position(|key| *key == name)
            .is_some_and(|index| self.0 & 1 << index != 0)
    }

    /// `BPListDetector.detectOnKeys` of the dictionary's keys.
    pub fn kind(self) -> ListKind {
        let has = |key: &str| self.has(key);
        if has("nodes") && has("edges") && has("graphEncodingVersion") {
            ListKind::Memgraph
        } else if has("WebMainResource") {
            ListKind::WebArchive
        } else if has("Playlists") && has("Tracks") && has("Music Folder") {
            ListKind::Itunes
        } else {
            ListKind::Plain
        }
    }
}

/// `parseUnsignedInt`: the bytes `start..end`, Java `int` indexes, as a big-endian number of
/// which the low 32 bits are kept; an index outside the bytes fails as Java's array access does.
fn unsigned_int(bytes: &[u8], start: i32, end: i32) -> Result<u32, Thrown> {
    let mut value: u64 = 0;
    let mut index = start;
    while index < end {
        let byte = usize::try_from(index)
            .ok()
            .and_then(|at| bytes.get(at))
            .ok_or_else(|| Thrown::Runtime(format!("Index {index} out of bounds")))?;
        value = value << 8 | u64::from(*byte);
        index += 1;
    }
    Ok(value as u32)
}

/// `parseLong`: the bytes as a big-endian number, its low 64 bits kept.
fn long(bytes: &[u8]) -> i64 {
    bytes
        .iter()
        .fold(0i64, |value, &byte| value << 8 | i64::from(byte))
}

/// A binary property list, its trailer read, its objects read where they are as they are asked
/// for, the pages of a mapped list released as they are read.
pub struct Reader<'a> {
    bytes: &'a [u8],
    pages: Pages<'a>,
    paging: Paging<'a>,
    object_ref_size: i32,
    offset_size: i32,
    num_objects: i32,
    offset_table: i32,
    top: ObjectId,
}

impl<'a> Reader<'a> {
    /// `BinaryPropertyListParser.parse` up to the objects: the header and trailer of `bytes`,
    /// whose pages are `pages`.
    pub fn open(bytes: &'a [u8], pages: Pages<'a>) -> Result<Self, Thrown> {
        if bytes.len() < 8 {
            return Err(format_error(
                "The available binary property list data is too short.",
            ));
        }
        if !bytes.starts_with(MAGIC) || !bytes[6].is_ascii_digit() || !bytes[7].is_ascii_digit() {
            return Err(format_error(
                "The binary property list has an invalid file header",
            ));
        }
        if bytes[6] != b'0' {
            return Err(format_error(
                "Unsupported binary property list format: Version 1.0 and later are not yet supported.",
            ));
        }
        if bytes.len() < HEADER_AND_TRAILER {
            return Err(format_error(
                "The binary property list does not contain a complete object offset table.",
            ));
        }
        let length = i64::try_from(bytes.len()).expect("a slice's length fits an i64");
        let trailer = &bytes[bytes.len() - TRAILER..];
        let field = |range: std::ops::Range<usize>| i64::from(long(&trailer[range]) as u32);
        let offset_size = i64::from(trailer[6]);
        let object_ref_size = i64::from(trailer[7]);
        let num_objects = field(8..16);
        let top_object = field(16..24);
        let offset_table = field(24..32);
        let corrupted =
            || format_error("The binary property list contains a corrupted object offset table.");
        if offset_size < 1
            || object_ref_size < 1
            || num_objects > i64::from(i32::MAX)
            || top_object >= length - TRAILER as i64
            || offset_table > length
            || offset_table + (num_objects + 1) * offset_size > length
        {
            return Err(corrupted());
        }
        let int = |value: i64| i32::try_from(value).map_err(|_| corrupted());
        Ok(Self {
            bytes,
            pages,
            paging: Paging::new(pages),
            object_ref_size: int(object_ref_size)?,
            offset_size: int(offset_size)?,
            num_objects: int(num_objects)?,
            offset_table: int(offset_table)?,
            top: ObjectId(int(top_object)?),
        })
    }

    /// The list's top object.
    pub fn top(&self) -> ObjectId {
        self.top
    }

    /// How many objects the offset table numbers.
    pub fn objects(&self) -> u64 {
        u64::try_from(self.num_objects).expect("the trailer's object count is not negative")
    }

    /// The pages of the list's bytes.
    pub fn pages(&self) -> Pages<'a> {
        self.pages
    }

    /// Fails where dd-plist's parse of the whole tree from the top object fails, each object
    /// checked once, as dd-plist parses each once.
    pub fn validate(&self) -> Result<(), Thrown> {
        let words = usize::try_from(self.num_objects)
            .expect("the trailer's object count is not negative")
            .div_ceil(64);
        self.parse(&mut Vec::new(), &mut vec![0u64; words], self.top)
    }

    /// `parseObject`: checks the object `object` and what it holds, unless it was checked
    /// already, with `stack` the objects that hold it and `parsed` a bit for each object
    /// checked.
    fn parse(
        &self,
        stack: &mut Vec<ObjectId>,
        parsed: &mut [u64],
        object: ObjectId,
    ) -> Result<(), Thrown> {
        if stack.len() >= MAX_NESTING_DEPTH {
            return Err(format_error(
                "The nesting depth of the property list exceeds the maximum supported depth of 512.",
            ));
        }
        if stack.contains(&object) {
            return Err(format_error(
                "The given binary property list contains a cyclic reference.",
            ));
        }
        let bit = usize::try_from(object.0)
            .ok()
            .filter(|&number| number < parsed.len() * 64);
        if bit.is_some_and(|number| parsed[number / 64] & 1 << (number % 64) != 0) {
            return Ok(());
        }
        stack.push(object);
        let checked = self.parse_held(stack, parsed, object);
        stack.pop();
        checked?;
        let number = bit.expect("an object read is numbered within the offset table");
        parsed[number / 64] |= 1 << (number % 64);
        Ok(())
    }

    /// Checks `object` and parses what it holds.
    fn parse_held(
        &self,
        stack: &mut Vec<ObjectId>,
        parsed: &mut [u64],
        object: ObjectId,
    ) -> Result<(), Thrown> {
        match self.node(object)? {
            Node::Array(members) | Node::Set(members) => {
                for member in self.members(members) {
                    self.parse(stack, parsed, member)?;
                }
            }
            Node::Dict(pairs) => {
                for (index, (key, value)) in self.pairs(pairs).enumerate() {
                    self.parse(stack, parsed, key)?;
                    if matches!(self.node(key)?, Node::Null) {
                        return Err(format_error(format!(
                            "The key #{} of the NSDictionary at offset {} is NULL.",
                            index + 1,
                            self.object_offset(object.0)?
                        )));
                    }
                    self.parse(stack, parsed, value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The members of an array or set, in the order of their references.
    pub fn members(&self, members: Members) -> impl Iterator<Item = ObjectId> + '_ {
        (0..members.count).map(move |index| self.reference(members.base, index))
    }

    /// The keys and values of a dictionary, each pair as it is stored.
    pub fn pairs(&self, pairs: Pairs) -> impl Iterator<Item = (ObjectId, ObjectId)> + '_ {
        (0..pairs.count).map(move |index| {
            (
                self.reference(pairs.keys, index),
                self.reference(pairs.values, index),
            )
        })
    }

    /// The entries of a dictionary as dd-plist's `NSDictionary` holds them, a `LinkedHashMap`
    /// keyed by its keys' `toString`: each key in the place it was first put, with the value it
    /// was last put with, and each key without a `toString` the port has in a place of its own.
    pub fn entries(&self, pairs: Pairs) -> Result<Vec<(ObjectId, ObjectId)>, Thrown> {
        let mut entries: Vec<(ObjectId, ObjectId)> = Vec::new();
        let mut positions: HashMap<String, usize> = HashMap::new();
        for (key, value) in self.pairs(pairs) {
            let Some(name) = self.name(key)? else {
                entries.push((key, value));
                continue;
            };
            match positions.entry(name) {
                Entry::Occupied(position) => entries[*position.get()].1 = value,
                Entry::Vacant(position) => {
                    position.insert(entries.len());
                    entries.push((key, value));
                }
            }
        }
        Ok(entries)
    }

    /// `BPListDetector.detectOnKeys` of the keys of a dictionary that are put with a value.
    pub fn kind(&self, pairs: Pairs) -> Result<ListKind, Thrown> {
        let mut keys = KindKeys::default();
        for (key, value) in self.pairs(pairs) {
            if matches!(self.node(value)?, Node::Null) {
                continue;
            }
            if let Some(name) = self.name(key)? {
                keys.put(&name);
            }
        }
        Ok(keys.kind())
    }

    /// `toString` of an object whose text the port writes: a string, a number, a UID.
    pub fn name(&self, object: ObjectId) -> Result<Option<String>, Thrown> {
        Ok(match self.node(object)? {
            Node::String(text) => Some(text.decode()),
            Node::Number(number) => Some(number.text()),
            Node::Uid(value) => Some(format!("{value:x} ({value})")),
            Node::Null
            | Node::Date(_)
            | Node::Data(_)
            | Node::Array(_)
            | Node::Set(_)
            | Node::Dict(_) => None,
        })
    }

    /// `safeAddOffset`: `offset + length`, failing past the offset table.
    fn safe_add(&self, offset: i32, length: i32, what: &str) -> Result<i32, Thrown> {
        offset
            .checked_add(length)
            .filter(|&end| end <= self.offset_table)
            .ok_or_else(|| {
                format_error(format!(
                    "The length of the {what} at offset {offset} is larger than the amount of available data."
                ))
            })
    }

    /// The bytes `start..end`, both within the offset table's bounds by now.
    fn slice(&self, start: i32, end: i32) -> &'a [u8] {
        &self.bytes[start as usize..end as usize]
    }

    /// The object numbered `object`, as `parseObject` reads it before what it holds.
    pub fn node(&self, object: ObjectId) -> Result<Node<'a>, Thrown> {
        let offset = self.object_offset(object.0)?;
        let marker = *usize::try_from(offset)
            .ok()
            .and_then(|at| self.bytes.get(at))
            .ok_or_else(|| Thrown::Runtime(format!("Index {offset} out of bounds")))?;
        let (kind, info) = (marker >> 4, marker & 0x0F);
        let unsupported = || {
            format_error(format!(
                "The NSObject at offset {offset} has an unknown or unsupported type (0x{marker:02x})"
            ))
        };
        let (node, end) = match kind {
            SIMPLE_TYPE => match info {
                0x0 => (Node::Null, offset + 1),
                0x8 => (Node::Number(Number::Boolean(false)), offset + 1),
                0x9 => (Node::Number(Number::Boolean(true)), offset + 1),
                0xC | 0xD => {
                    return Err(format_error(format!(
                        "The NSObject at offset {offset} is a URL, which is not supported."
                    )))
                }
                0xE => self.uid(offset, 16)?,
                _ => return Err(unsupported()),
            },
            INT_TYPE | REAL_TYPE => {
                let length = 1i32 << info;
                let end = self.safe_add(offset, length + 1, "NSNumber")?;
                let bytes = self.slice(offset + 1, end);
                let number = if kind == INT_TYPE {
                    Number::Integer(long(bytes))
                } else {
                    Number::Real(match length {
                        8 => f64::from_bits(long(bytes) as u64),
                        4 => f64::from(f32::from_bits(long(bytes) as u32)),
                        _ => {
                            return Err(Thrown::Runtime(format!(
                                "endIndex - startIndex ({length}) != 4 or 8"
                            )))
                        }
                    })
                };
                (Node::Number(number), end)
            }
            DATE_TYPE => {
                if info != 0x3 {
                    return Err(unsupported());
                }
                let end = self.safe_add(offset, 9, "NSDate")?;
                let seconds = f64::from_bits(long(self.slice(offset + 1, end)) as u64);
                (Node::Date(seconds), end)
            }
            DATA_TYPE => {
                let (length, header) = self.length_and_offset(info, offset)?;
                let start = offset + header;
                let end = self.safe_add(start, length, "NSData")?;
                (Node::Data(self.slice(start, end)), end)
            }
            ASCII_STRING_TYPE | UTF16_STRING_TYPE | UTF8_STRING_TYPE => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let start = offset + header;
                let content = || {
                    format_error(format!(
                        "The length of the NSString content at offset {offset} is larger than the amount of available data."
                    ))
                };
                let (length, charset) = match kind {
                    ASCII_STRING_TYPE => (count, "US-ASCII"),
                    UTF16_STRING_TYPE => (count.checked_mul(2).ok_or_else(content)?, "UTF-16BE"),
                    _ => (self.utf8_length(start, count).ok_or_else(content)?, "UTF-8"),
                };
                let end = self.safe_add(start, length, "NSString content")?;
                let text = Text {
                    bytes: self.slice(start, end),
                    charset,
                };
                (Node::String(text), end)
            }
            UID_TYPE => self.uid(offset, i32::from(info) + 1)?,
            ARRAY_TYPE | ORDERED_SET_TYPE | SET_TYPE => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let base = offset + header;
                let end = self.validate_references(base, count)?;
                let members = Members { base, count };
                if kind == ARRAY_TYPE {
                    (Node::Array(members), end)
                } else {
                    (Node::Set(members), end)
                }
            }
            DICTIONARY_TYPE => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let keys = offset + header;
                let values = self.validate_references(keys, count)?;
                let end = self.validate_references(values, count)?;
                let pairs = Pairs {
                    keys,
                    values,
                    count,
                };
                (Node::Dict(pairs), end)
            }
            _ => return Err(unsupported()),
        };
        self.paging.read((end - offset) as usize);
        Ok(node)
    }

    /// `getObjectOffset`.
    fn object_offset(&self, object: i32) -> Result<i32, Thrown> {
        if object >= self.num_objects {
            return Err(format_error(format!(
                "The given binary property list contains an invalid object identifier ({object})."
            )));
        }
        let start = self
            .offset_table
            .wrapping_add(object.wrapping_mul(self.offset_size));
        let offset = unsigned_int(self.bytes, start, start.wrapping_add(self.offset_size))? as i32;
        if offset > self.offset_table {
            return Err(format_error(format!(
                "The given binary property list contains an invalid object offset ({offset}) for object {object}."
            )));
        }
        Ok(offset)
    }

    /// `parseObjectReferenceFromList`, within a list `validate_references` checked.
    fn reference(&self, base: i32, index: i32) -> ObjectId {
        let at = (base + index * self.object_ref_size) as usize;
        let end = at + self.object_ref_size as usize;
        ObjectId(long(&self.bytes[at..end]) as u32 as i32)
    }

    /// `validateObjectReferenceListLength`: where the list of `count` references at `base`
    /// ends, failing past the offset table.
    fn validate_references(&self, base: i32, count: i32) -> Result<i32, Thrown> {
        count
            .checked_mul(self.object_ref_size)
            .and_then(|size| base.checked_add(size))
            .filter(|&end| end <= self.offset_table)
            .ok_or_else(|| {
                format_error(format!(
                    "The length of the object reference list at offset {base} is larger than the amount of available data."
                ))
            })
    }

    /// `readLengthAndOffset`: an object's count, and how many bytes its marker and count take.
    fn length_and_offset(&self, info: u8, offset: i32) -> Result<(i32, i32), Thrown> {
        if info != 0xF {
            return Ok((i32::from(info), 1));
        }
        let invalid = || {
            format_error(format!(
                "The length/offset integer at offset {offset} is invalid."
            ))
        };
        let at = offset.checked_add(1).ok_or_else(invalid)?;
        let marker = *usize::try_from(at)
            .ok()
            .and_then(|at| self.bytes.get(at))
            .ok_or_else(invalid)?;
        let size = 1i32 << (marker & 0x0F);
        let length_length = 2 + size;
        let start = offset.checked_add(2).ok_or_else(invalid)?;
        let end = self
            .safe_add(offset, length_length, "length integer")
            .map_err(|_| invalid())?;
        let digits = self.slice(start, end);
        let value = if size < 3 {
            long(digits) as u32 as i32
        } else {
            long(&digits[digits.len() - 4..]) as i32
        };
        if value < 0 {
            return Err(format_error(format!(
                "The length integer at offset {offset} is negative or exceeds the maximum supported value."
            )));
        }
        Ok((value, length_length))
    }

    /// `calculateUtf8StringLength`: the bytes of `characters` characters, and the count itself
    /// where the bytes run out or turn out malformed; `None` where Java's exact sum overflows.
    fn utf8_length(&self, start: i32, characters: i32) -> Option<i32> {
        let mut length: i32 = 0;
        for _ in 0..characters {
            let at = start.checked_add(length)?;
            if at >= self.offset_table {
                return Some(characters);
            }
            let byte = self.bytes[at as usize];
            if byte & 0x80 == 0 {
                length += 1;
                continue;
            }
            let following = match byte {
                _ if byte & 0xC0 == 0x80 => return Some(characters),
                _ if byte & 0xE0 == 0xC0 => 1,
                _ if byte & 0xF0 == 0xE0 => 2,
                _ if byte & 0xF8 == 0xF0 => 3,
                _ => 0,
            };
            let complete = (1..=following).all(|index| {
                at + index < self.offset_table && self.bytes[(at + index) as usize] & 0xC0 == 0x80
            });
            if !complete {
                return Some(characters);
            }
            length = length.checked_add(following + 1)?;
        }
        Some(length)
    }

    /// `parseUid`: the UID of `length` bytes after the marker, and where it ends.
    fn uid(&self, offset: i32, length: i32) -> Result<(Node<'a>, i32), Thrown> {
        let end = self.safe_add(offset, length + 1, "UID")?;
        let value = self
            .slice(offset + 1, end)
            .iter()
            .fold(0u128, |value, &byte| value << 8 | u128::from(byte));
        Ok((Node::Uid(value), end))
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    #[cfg(test)]
    use super::*;

    /// An object of a binary property list to write, its references by object number.
    pub enum Item {
        Null,
        Bool(bool),
        Int(i64),
        Real(f64),
        Float(f32),
        Date(f64),
        Data(&'static [u8]),
        Ascii(&'static str),
        Utf16(&'static str),
        Utf8(&'static str),
        Uid(u8),
        Array(Vec<u8>),
        Set(Vec<u8>),
        Dict(Vec<(u8, u8)>),
    }

    fn marker(kind: u8, length: usize) -> Vec<u8> {
        if length < 15 {
            vec![kind << 4 | length as u8]
        } else {
            vec![kind << 4 | 0xF, 0x10, length as u8]
        }
    }

    /// A binary property list of `items`, with one-byte references and two-byte offsets.
    pub fn bplist(items: &[Item], top: u8) -> Vec<u8> {
        let mut bytes = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        for item in items {
            offsets.push(bytes.len() as u16);
            let encoded = match item {
                Item::Null => vec![0x00],
                Item::Bool(value) => vec![if *value { 0x09 } else { 0x08 }],
                Item::Int(value) => [vec![0x13], value.to_be_bytes().to_vec()].concat(),
                Item::Real(value) => [vec![0x23], value.to_be_bytes().to_vec()].concat(),
                Item::Float(value) => [vec![0x22], value.to_be_bytes().to_vec()].concat(),
                Item::Date(value) => [vec![0x33], value.to_be_bytes().to_vec()].concat(),
                Item::Data(data) => [marker(0x4, data.len()), data.to_vec()].concat(),
                Item::Ascii(text) => [marker(0x5, text.len()), text.as_bytes().to_vec()].concat(),
                Item::Utf16(text) => {
                    let units: Vec<u16> = text.encode_utf16().collect();
                    let mut encoded = marker(0x6, units.len());
                    encoded.extend(units.iter().flat_map(|unit| unit.to_be_bytes()));
                    encoded
                }
                Item::Utf8(text) => {
                    [marker(0x7, text.chars().count()), text.as_bytes().to_vec()].concat()
                }
                Item::Uid(value) => vec![0x80, *value],
                Item::Array(refs) => [marker(0xA, refs.len()), refs.clone()].concat(),
                Item::Set(refs) => [marker(0xC, refs.len()), refs.clone()].concat(),
                Item::Dict(pairs) => {
                    let mut encoded = marker(0xD, pairs.len());
                    encoded.extend(pairs.iter().map(|(key, _)| *key));
                    encoded.extend(pairs.iter().map(|(_, value)| *value));
                    encoded
                }
            };
            bytes.extend(encoded);
        }
        let table = bytes.len() as u64;
        for offset in offsets {
            bytes.extend(offset.to_be_bytes());
        }
        bytes.extend([0; 6]);
        bytes.extend([2, 1]);
        bytes.extend((items.len() as u64).to_be_bytes());
        bytes.extend(u64::from(top).to_be_bytes());
        bytes.extend(table.to_be_bytes());
        bytes
    }

    #[cfg(test)]
    fn validated(bytes: &[u8]) -> Result<Reader<'_>, Thrown> {
        let reader = Reader::open(bytes, Pages::IN_MEMORY)?;
        reader.validate()?;
        Ok(reader)
    }

    #[cfg(test)]
    fn thrown(bytes: &[u8]) -> Thrown {
        validated(bytes).err().expect("the list fails")
    }

    /// The pairs of the top dictionary of `reader`, as dd-plist holds them.
    #[cfg(test)]
    fn top_dict(reader: &Reader) -> Pairs {
        let top = reader.node(reader.top()).unwrap();
        let Node::Dict(pairs) = top else {
            panic!("{top:?}");
        };
        pairs
    }

    #[test]
    fn reads_objects() {
        use Item::*;
        let list = bplist(
            &[
                Dict(vec![(1, 2), (3, 4), (1, 5), (6, 7), (8, 9)]),
                Ascii("key"),
                Ascii("first"),
                Ascii("empty"),
                Null,
                Ascii("second"),
                Ascii("named"),
                Utf8("\u{20AC}uro"),
                Utf8("na\u{ef}ve"),
                Ascii("caf\u{e9}"),
            ],
            0,
        );
        let reader = validated(&list).unwrap();
        let entries: Vec<(String, Option<String>)> = reader
            .entries(top_dict(&reader))
            .unwrap()
            .into_iter()
            .map(|(key, value)| {
                let value = match reader.node(value).unwrap() {
                    Node::Null => None,
                    Node::String(text) => Some(text.decode()),
                    other => panic!("{other:?}"),
                };
                (reader.name(key).unwrap().unwrap(), value)
            })
            .collect();
        assert_eq!(
            entries,
            [
                ("key".to_owned(), Some("second".to_owned())),
                ("empty".to_owned(), None),
                ("named".to_owned(), Some("\u{20AC}uro".to_owned())),
                (
                    "na\u{ef}ve".to_owned(),
                    Some("caf\u{FFFD}\u{FFFD}".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn names_lists_by_the_keys_put_with_a_value() {
        use Item::*;
        let kind = |items: &[Item]| {
            let list = bplist(items, 0);
            let reader = validated(&list).unwrap();
            reader.kind(top_dict(&reader)).unwrap()
        };
        let archive = [Dict(vec![(1, 2)]), Ascii("WebMainResource"), Int(1)];
        assert_eq!(kind(&archive), ListKind::WebArchive);
        let null = [Dict(vec![(1, 2)]), Ascii("WebMainResource"), Null];
        assert_eq!(kind(&null), ListKind::Plain);
        let put_again = [
            Dict(vec![(1, 2), (1, 3)]),
            Ascii("WebMainResource"),
            Int(1),
            Null,
        ];
        assert_eq!(kind(&put_again), ListKind::WebArchive);
    }

    #[test]
    fn checks_each_shared_object_once() {
        let mut items: Vec<Item> = (1..=60u8)
            .map(|next| Item::Array(vec![next, next]))
            .collect();
        items.push(Item::Ascii("leaf"));
        let list = bplist(&items, 0);
        assert!(validated(&list).is_ok());
    }

    #[test]
    fn fails_as_dd_plist_does() {
        use Item::*;
        let cycle = bplist(&[Array(vec![1]), Array(vec![0])], 0);
        assert!(matches!(thrown(&cycle), Thrown::Format(_)));
        let null_key = bplist(&[Dict(vec![(1, 2)]), Null, Ascii("value")], 0);
        assert!(matches!(thrown(&null_key), Thrown::Format(_)));
        let typical = bplist(&[Array(vec![1]), Ascii("text")], 0);
        assert!(matches!(
            thrown(&typical[..typical.len() - 1]),
            Thrown::Format(_)
        ));
        assert!(matches!(thrown(b"bplist01"), Thrown::Format(_)));
    }
}
