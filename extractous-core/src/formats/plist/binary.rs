//! Binary property lists as dd-plist 1.30's `BinaryPropertyListParser` reads them, for Tika's
//! `BPListDetector` and `PListParser` alike: offsets and lengths checked against the offset
//! table, with Java's exact arithmetic; nesting up to 512 objects deep; cycles refused; each
//! object parsed once however often it is referenced; and the `int` casts and array accesses
//! Java makes, which fail as a `RuntimeException` rather than a `PropertyListFormatException`.
//!
//! A port rather than a crate, as dd-plist's object model is what Tika reads and writes: a
//! dictionary key is any object's `toString`, a dictionary drops its null values, and strings
//! decode as the JDK decodes them. Unlike dd-plist, a UID is named by its value, not by the
//! number of its object, and a UTF-8 string's characters are measured by their real length,
//! where dd-plist counts two bytes for each multibyte one (#104).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::java::dtoa::java_double_to_string;
use crate::text::charset::Charset;

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

/// What dd-plist throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Thrown {
    /// A `PropertyListFormatException`.
    Format(String),
    /// A `RuntimeException`: an array index out of bounds or an illegal argument.
    Runtime(String),
}

fn format_error(reason: impl Into<String>) -> Thrown {
    Thrown::Format(reason.into())
}

/// An object of the list; `None` where dd-plist holds a null.
pub(crate) type Value = Option<Rc<Object>>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Object {
    Dict(Dict),
    Array(Vec<Value>),
    /// A set's members, each reference once, in the order of their references.
    Set(Vec<Value>),
    String(String),
    Integer(i64),
    Real(f64),
    Boolean(bool),
    Data(Vec<u8>),
    /// Seconds since 2001-01-01.
    Date(f64),
    /// A UID.
    Uid(u128),
}

/// A dictionary's key: the `toString` of its object where the port writes it, else the object.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Key {
    Text(String),
    Object(Rc<Object>),
}

/// An `NSDictionary`: its entries in the order of a `LinkedHashMap`, a key put again keeping
/// its place and taking the new value, a null value not put.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Dict {
    entries: Vec<(Key, Rc<Object>)>,
    positions: HashMap<String, usize>,
}

/// What a property list holds, as `BPListDetector.detectOnKeys` tells it by the keys of its root
/// dictionary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListKind {
    Memgraph,
    WebArchive,
    Itunes,
    Plain,
}

impl ListKind {
    /// The media type of a binary property list of this kind.
    pub(crate) fn binary_type(self) -> &'static str {
        match self {
            Self::Memgraph => "application/x-bplist-memgraph",
            Self::WebArchive => "application/x-bplist-webarchive",
            Self::Itunes => "application/x-bplist-itunes",
            Self::Plain => "application/x-bplist",
        }
    }

    /// The media type of an XML property list of this kind, `detectXMLOnKeys`.
    pub(crate) fn xml_type(self) -> &'static str {
        match self {
            Self::Memgraph => "application/x-plist-memgraph",
            Self::WebArchive => "application/x-plist-webarchive",
            Self::Itunes => "application/x-plist-itunes",
            Self::Plain => "application/x-plist",
        }
    }
}

impl Dict {
    pub(super) fn put(&mut self, key: Key, value: Value) {
        let Some(value) = value else {
            return;
        };
        if let Key::Text(text) = &key {
            if let Some(&position) = self.positions.get(text) {
                self.entries[position].1 = value;
                return;
            }
            self.positions.insert(text.clone(), self.entries.len());
        }
        self.entries.push((key, value));
    }

    pub(crate) fn entries(&self) -> &[(Key, Rc<Object>)] {
        &self.entries
    }

    /// Whether `key` is a key of the dictionary.
    fn contains(&self, key: &str) -> bool {
        self.positions.contains_key(key)
    }

    /// `BPListDetector.detectOnKeys` of the dictionary's keys.
    pub(crate) fn kind(&self) -> ListKind {
        let has = |key: &str| self.contains(key);
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

impl Object {
    /// `toString` of an object whose text the port writes: a string, a number, a UID.
    pub(crate) fn text(&self) -> Option<String> {
        Some(match self {
            Object::String(text) => text.clone(),
            Object::Integer(value) => value.to_string(),
            Object::Boolean(value) => value.to_string(),
            Object::Real(value) if value.is_nan() => "nan".to_owned(),
            Object::Real(value) if *value == f64::INFINITY => "+infinity".to_owned(),
            Object::Real(value) if *value == f64::NEG_INFINITY => "-infinity".to_owned(),
            Object::Real(value) => java_double_to_string(*value),
            Object::Uid(value) => format!("{value:x} ({value})"),
            Object::Dict(_)
            | Object::Array(_)
            | Object::Set(_)
            | Object::Data(_)
            | Object::Date(_) => return None,
        })
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

struct Parser<'a> {
    bytes: &'a [u8],
    object_ref_size: i32,
    offset_size: i32,
    num_objects: i32,
    offset_table: i32,
    parsed: HashMap<i32, Value>,
}

/// `BinaryPropertyListParser.parse`: the root object of a binary property list.
pub(crate) fn parse(bytes: &[u8]) -> Result<Value, Thrown> {
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
    let mut parser = Parser {
        bytes,
        object_ref_size: int(object_ref_size)?,
        offset_size: int(offset_size)?,
        num_objects: int(num_objects)?,
        offset_table: int(offset_table)?,
        parsed: HashMap::new(),
    };
    parser.object(&mut Vec::new(), int(top_object)?)
}

impl Parser<'_> {
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
    fn slice(&self, start: i32, end: i32) -> &[u8] {
        &self.bytes[start as usize..end as usize]
    }

    /// `parseObject`: the object numbered `object`, parsed once.
    fn object(&mut self, stack: &mut Vec<i32>, object: i32) -> Result<Value, Thrown> {
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
        if let Some(value) = self.parsed.get(&object) {
            return Ok(value.clone());
        }
        stack.push(object);
        let value = self.object_at(stack, object);
        stack.pop();
        let value = value?;
        self.parsed.insert(object, value.clone());
        Ok(value)
    }

    fn object_at(&mut self, stack: &mut Vec<i32>, object: i32) -> Result<Value, Thrown> {
        let offset = self.object_offset(object)?;
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
        let parsed = match kind {
            SIMPLE_TYPE => match info {
                0x0 => return Ok(None),
                0x8 => Object::Boolean(false),
                0x9 => Object::Boolean(true),
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
                if kind == INT_TYPE {
                    Object::Integer(long(bytes))
                } else {
                    Object::Real(match length {
                        8 => f64::from_bits(long(bytes) as u64),
                        4 => f64::from(f32::from_bits(long(bytes) as u32)),
                        _ => {
                            return Err(Thrown::Runtime(format!(
                                "endIndex - startIndex ({length}) != 4 or 8"
                            )))
                        }
                    })
                }
            }
            DATE_TYPE => {
                if info != 0x3 {
                    return Err(unsupported());
                }
                let end = self.safe_add(offset, 9, "NSDate")?;
                Object::Date(f64::from_bits(long(self.slice(offset + 1, end)) as u64))
            }
            DATA_TYPE => {
                let (length, header) = self.length_and_offset(info, offset)?;
                let start = offset + header;
                let end = self.safe_add(start, length, "NSData")?;
                Object::Data(self.slice(start, end).to_vec())
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
                Object::String(
                    Charset::named(charset)
                        .decode(self.slice(start, end))
                        .expect("the JDK's Unicode and ASCII charsets decode natively"),
                )
            }
            UID_TYPE => self.uid(offset, i32::from(info) + 1)?,
            ARRAY_TYPE | ORDERED_SET_TYPE | SET_TYPE => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let base = offset + header;
                self.validate_references(base, count)?;
                let mut members = Vec::new();
                let mut seen = HashSet::new();
                for index in 0..count {
                    let reference = self.reference(base, index)?;
                    if kind == ARRAY_TYPE || seen.insert(reference) {
                        members.push(self.object(stack, reference)?);
                    }
                }
                if kind == ARRAY_TYPE {
                    Object::Array(members)
                } else {
                    Object::Set(members)
                }
            }
            DICTIONARY_TYPE => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let keys = offset + header;
                self.validate_references(keys, count)?;
                let values = keys + count * self.object_ref_size;
                self.validate_references(values, count)?;
                let mut dict = Dict::default();
                for index in 0..count {
                    let key_reference = self.reference(keys, index)?;
                    let value_reference = self.reference(values, index)?;
                    let key = self.object(stack, key_reference)?.ok_or_else(|| {
                        format_error(format!(
                            "The key #{} of the NSDictionary at offset {offset} is NULL.",
                            index + 1
                        ))
                    })?;
                    let value = self.object(stack, value_reference)?;
                    let key = match key.text() {
                        Some(text) => Key::Text(text),
                        None => Key::Object(key),
                    };
                    dict.put(key, value);
                }
                Object::Dict(dict)
            }
            _ => return Err(unsupported()),
        };
        Ok(Some(Rc::new(parsed)))
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

    /// `parseObjectReferenceFromList`, within the list `validate_references` checked.
    fn reference(&self, base: i32, index: i32) -> Result<i32, Thrown> {
        let at = base + index * self.object_ref_size;
        Ok(unsigned_int(self.bytes, at, at + self.object_ref_size)? as i32)
    }

    /// `validateObjectReferenceListLength`.
    fn validate_references(&self, base: i32, count: i32) -> Result<(), Thrown> {
        count
            .checked_mul(self.object_ref_size)
            .and_then(|size| base.checked_add(size))
            .filter(|&end| end <= self.offset_table)
            .map(|_| ())
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

    /// `parseUid`: the UID of `length` bytes after the marker.
    fn uid(&self, offset: i32, length: i32) -> Result<Object, Thrown> {
        let end = self.safe_add(offset, length + 1, "UID")?;
        let value = self
            .slice(offset + 1, end)
            .iter()
            .fold(0u128, |value, &byte| value << 8 | u128::from(byte));
        Ok(Object::Uid(value))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An object of a binary property list to write, its references by object number.
    pub(crate) enum Item {
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
    pub(crate) fn bplist(items: &[Item], top: u8) -> Vec<u8> {
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

    fn thrown(bytes: &[u8]) -> Thrown {
        parse(bytes).unwrap_err()
    }

    #[test]
    fn reads_objects() {
        use Item::*;
        let list = bplist(
            &[
                Dict(vec![(1, 2), (3, 4), (1, 5), (6, 7), (8, 9)]),
                Ascii("key"),
                Ascii("first"),
                Int(7),
                Null,
                Ascii("second"),
                Uid(10),
                Utf8("\u{20AC}uro"),
                Utf8("na\u{ef}ve"),
                Ascii("caf\u{e9}"),
            ],
            0,
        );
        let root = parse(&list).unwrap().unwrap();
        let Object::Dict(dict) = &*root else {
            panic!("{root:?}");
        };
        let entries: Vec<(String, Object)> = dict
            .entries()
            .iter()
            .map(|(key, value)| match key {
                Key::Text(text) => (text.clone(), (**value).clone()),
                Key::Object(object) => panic!("{object:?}"),
            })
            .collect();
        assert_eq!(
            entries,
            [
                ("key".to_owned(), Object::String("second".to_owned())),
                (
                    "a (10)".to_owned(),
                    Object::String("\u{20AC}uro".to_owned())
                ),
                (
                    "na\u{ef}ve".to_owned(),
                    Object::String("caf\u{FFFD}\u{FFFD}".to_owned())
                ),
            ]
        );
        assert!(!dict.contains("7"));
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
