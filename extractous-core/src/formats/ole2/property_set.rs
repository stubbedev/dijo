//! A property set stream as POI's HPSF reads it: `PropertySet` with its sections, each
//! `Section` with its dictionary and properties, and each `Property` with the value
//! `VariantSupport` makes of it, down to the bytes POI keeps of a type it does not convert and
//! the exceptions it throws on bytes it cannot read.

use std::collections::BTreeMap;

use super::codepage;
use super::reader::{allocation, Reader, Thrown};
use crate::formats::Unported;

/// The format id of the summary information, as `ClassID` holds it.
pub(super) const SUMMARY_INFORMATION: [u8; 16] = [
    0xF2, 0x9F, 0x85, 0xE0, 0x4F, 0xF9, 0x10, 0x68, 0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9,
];
/// The format id of the document summary information.
pub(super) const DOCUMENT_SUMMARY_INFORMATION: [u8; 16] = [
    0xD5, 0xCD, 0xD5, 0x02, 0x2E, 0x9C, 0x10, 0x1B, 0x93, 0x97, 0x08, 0x00, 0x2B, 0x2C, 0xF9, 0xAE,
];
/// The format id of the user-defined properties of the document summary information.
pub(super) const USER_PROPERTIES: [u8; 16] = [
    0xD5, 0xCD, 0xD5, 0x05, 0x2E, 0x9C, 0x10, 0x1B, 0x93, 0x97, 0x08, 0x00, 0x2B, 0x2C, 0xF9, 0xAE,
];

const PID_DICTIONARY: u32 = 0;
const PID_CODEPAGE: u32 = 1;
const PID_MAX: u32 = 0x1F;
const VT_I2: u32 = 2;
/// `CodePageString`'s and the dictionary's largest record.
const MAX_STRING_LENGTH: i32 = 100_000;
const MAX_BLOB_LENGTH: i32 = 10_000_000;
const MAX_CLIPBOARD_LENGTH: i32 = 100_000_000;
const MAX_ARRAY_SCALARS: i32 = 100_000;
/// The longest dictionary entry POI reads, in bytes.
const MAX_DICTIONARY_ENTRY: i32 = 0xFF_FFFF;

/// Milliseconds from 1601-01-01, where file times count from, to 1970-01-01.
const FILETIME_EPOCH_MILLIS: i64 = 11_644_473_600_000;

/// Why POI reads no further: the Java exception, by the class its callers catch, or what this
/// port does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fault {
    /// `UnsupportedOperationException`, which `VariantSupport.read` catches.
    UnsupportedOperation,
    /// Any other runtime exception.
    Runtime,
    /// `UnsupportedEncodingException`, an `IOException`.
    Encoding,
    Unported(String),
}

impl From<Thrown> for Fault {
    fn from(_: Thrown) -> Self {
        Fault::Runtime
    }
}

type Read<T> = Result<T, Fault>;

/// A property's value as `Property.getValue` gives it, by its Java class; `None` is Java's
/// `null`.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    Byte(i8),
    Integer(i32),
    Long(i64),
    BigInteger(u64),
    Float(f32),
    Double(f64),
    Boolean(bool),
    String(String),
    /// A `java.util.Date`, in milliseconds since the epoch.
    Date(i64),
    /// The bytes POI keeps of a value it does not convert.
    Bytes(Vec<u8>),
}

/// A section: its format id and its properties in the order of POI's `LinkedHashMap`, the
/// dictionary aside.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Section {
    format_id: [u8; 16],
    properties: Vec<(u32, Option<Value>)>,
    dictionary: Option<BTreeMap<u32, String>>,
}

/// A property set's sections.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PropertySet {
    pub(super) sections: Vec<Section>,
}

impl PropertySet {
    /// `isSummaryInformation`: the first section's format id is the summary information's, in
    /// either byte order.
    pub(super) fn is_summary_information(&self) -> bool {
        self.first_matches(&[SUMMARY_INFORMATION])
    }

    /// `isDocumentSummaryInformation`.
    pub(super) fn is_document_summary_information(&self) -> bool {
        self.first_matches(&[DOCUMENT_SUMMARY_INFORMATION, USER_PROPERTIES])
    }

    fn first_matches(&self, ids: &[[u8; 16]]) -> bool {
        self.sections.first().is_some_and(|section| {
            let inverted = invert(&section.format_id);
            ids.iter()
                .any(|id| *id == section.format_id || *id == inverted)
        })
    }

    pub(super) fn first_section(&self) -> &Section {
        &self.sections[0]
    }
}

/// The byte order `ClassID` reads a class id in from a stream, which reversed is the stream's.
fn invert(raw: &[u8; 16]) -> [u8; 16] {
    let mut bytes = *raw;
    bytes[..4].reverse();
    bytes[4..6].reverse();
    bytes[6..8].reverse();
    bytes
}

/// `new PropertySet(stream)`: `Ok(None)` where the stream is no property set or POI throws on
/// it, which Tika passes over.
pub(super) fn read(stream: &[u8]) -> Result<Option<PropertySet>, Unported> {
    if !is_property_set_stream(stream) {
        return Ok(None);
    }
    match init(stream) {
        Ok(set) => Ok(Some(set)),
        Err(Fault::Unported(reason)) => Err(Unported(reason)),
        Err(_) => Ok(None),
    }
}

/// `isPropertySetStream` on the first 50 bytes, padded with zeros.
fn is_property_set_stream(stream: &[u8]) -> bool {
    let byte = |at: usize| stream.get(at).copied().unwrap_or(0);
    !stream.is_empty()
        && u16::from_le_bytes([byte(0), byte(1)]) == 0xFFFE
        && u16::from_le_bytes([byte(2), byte(3)]) == 0
}

fn slice(src: &[u8], offset: usize, length: usize) -> Read<&[u8]> {
    src.get(offset..offset.checked_add(length).ok_or(Fault::Runtime)?)
        .ok_or(Fault::Runtime)
}

fn byte_at(src: &[u8], offset: i64) -> Read<u8> {
    usize::try_from(offset)
        .ok()
        .and_then(|at| src.get(at).copied())
        .ok_or(Fault::Runtime)
}

fn init(src: &[u8]) -> Read<PropertySet> {
    let header = slice(src, 0, 28)?;
    let section_count = i32::from_le_bytes(header[24..28].try_into().expect("four bytes"));
    if section_count < 0 {
        return Err(Fault::Runtime);
    }
    let mut sections = Vec::new();
    let mut offset = 28usize;
    for _ in 0..section_count {
        sections.push(read_section(src, offset)?);
        offset += 20;
    }
    Ok(PropertySet { sections })
}

/// Section offsets and property ids, unique both ways as in commons' `TreeBidiMap`: a pair
/// replaces any with its offset or its id.
#[derive(Default)]
struct OffsetIds {
    by_offset: BTreeMap<u32, u32>,
    by_id: BTreeMap<u32, u32>,
}

impl OffsetIds {
    fn put(&mut self, offset: u32, id: u32) {
        if let Some(old) = self.by_offset.remove(&offset) {
            self.by_id.remove(&old);
        }
        if let Some(old) = self.by_id.remove(&id) {
            self.by_offset.remove(&old);
        }
        self.by_offset.insert(offset, id);
        self.by_id.insert(id, offset);
    }
}

/// `Math.toIntExact`.
fn to_int(value: i64) -> Read<i32> {
    i32::try_from(value).map_err(|_| Fault::Runtime)
}

fn read_section(src: &[u8], offset: usize) -> Read<Section> {
    let raw: [u8; 16] = slice(src, offset, 16)?.try_into().expect("16 bytes");
    let format_id = invert(&raw);
    let location: [u8; 4] = slice(src, offset + 16, 4)?.try_into().expect("four bytes");
    let mut off_fix = i64::from(u32::from_le_bytes(location) as i32);
    if byte_at(src, off_fix)? == 0 {
        for _ in 0..3 {
            if byte_at(src, off_fix)? != 0 {
                break;
            }
            off_fix += 1;
        }
        for _ in 0..3 {
            if byte_at(src, off_fix + 3)? == 0
                && byte_at(src, off_fix + 7)? == 0
                && byte_at(src, off_fix + 11)? == 0
            {
                break;
            }
            off_fix -= 1;
        }
    }
    let section_offset = off_fix;
    let mut reader = Reader::new(src, section_offset as usize);
    let size = i64::from(reader.read_u32()?).min(src.len() as i64 - section_offset);
    let property_count = reader.read_u32()? as i32;
    let mut offsets = OffsetIds::default();
    for _ in 0..property_count.max(0) {
        let id = reader.read_u32()?;
        let property_offset = reader.read_u32()?;
        offsets.put(property_offset, id);
    }
    let mut section = Section {
        format_id,
        properties: Vec::new(),
        dictionary: None,
    };
    let mut codepage = -1;
    if let Some(&codepage_offset) = offsets.by_id.get(&PID_CODEPAGE) {
        reader.set_position(i64::from(to_int(
            section_offset + i64::from(codepage_offset),
        )?))?;
        if reader.read_u32()? != VT_I2 {
            return Err(Fault::Runtime);
        }
        codepage = i32::from(reader.read_u16()?);
        section.set(PID_CODEPAGE, Some(Value::Integer(codepage)));
    }
    let last_id = offsets.by_id.keys().next_back().copied();
    let pairs: Vec<(u32, u32)> = offsets.by_offset.iter().map(|(&o, &i)| (o, i)).collect();
    for (index, &(property_offset, id)) in pairs.iter().enumerate() {
        if id == PID_CODEPAGE {
            continue;
        }
        let end = pairs
            .get(index + 1)
            .map_or(size, |&(next, _)| i64::from(next));
        let length = to_int(end - i64::from(property_offset))?;
        reader.set_position(i64::from(to_int(
            section_offset + i64::from(property_offset),
        )?))?;
        if id != PID_DICTIONARY {
            let value = read_property(&mut reader, length, codepage)?;
            section.set(id, value);
            continue;
        }
        let mark = reader.position();
        if read_dictionary(&mut section, &mut reader, codepage)? {
            continue;
        }
        reader.set_position(mark)?;
        let id = PID_MAX.max(last_id.unwrap_or(0)).wrapping_add(1);
        match read_property(&mut reader, length, codepage) {
            Ok(value) => section.set(id, value),
            Err(Fault::Runtime) => {}
            Err(fault) => return Err(fault),
        }
    }
    Ok(section)
}

impl Section {
    /// `setProperty`: a property already there keeps its place.
    fn set(&mut self, id: u32, value: Option<Value>) {
        match self.properties.iter_mut().find(|(known, _)| *known == id) {
            Some((_, known)) => *known = value,
            None => self.properties.push((id, value)),
        }
    }

    /// `getProperty`.
    pub(super) fn get(&self, id: u32) -> Option<&Value> {
        self.properties
            .iter()
            .find(|(known, _)| *known == id)
            .and_then(|(_, value)| value.as_ref())
    }

    /// The properties in POI's order, with their ids.
    pub(super) fn properties(&self) -> impl Iterator<Item = (u32, Option<&Value>)> {
        self.properties
            .iter()
            .map(|(id, value)| (*id, value.as_ref()))
    }

    /// `getDictionary`.
    pub(super) fn dictionary(&self) -> Option<&BTreeMap<u32, String>> {
        self.dictionary.as_ref()
    }

    /// `setDictionary`: the code page property set to 1252 where the section has none.
    fn set_dictionary(&mut self, dictionary: BTreeMap<u32, String>) {
        self.dictionary
            .get_or_insert_with(BTreeMap::new)
            .extend(dictionary);
        if self.get(PID_CODEPAGE).is_none() {
            self.set(PID_CODEPAGE, Some(Value::Integer(codepage::DEFAULT)));
        }
    }
}

/// `Section.readDictionary`: whether the dictionary read whole. The entries read before one POI
/// gives up on are kept.
fn read_dictionary(section: &mut Section, reader: &mut Reader<'_>, codepage: i32) -> Read<bool> {
    let entries = reader.read_u32()?;
    let codepage = if codepage == -1 {
        codepage::DEFAULT
    } else {
        codepage
    };
    let width = if codepage == codepage::UNICODE { 2 } else { 1 };
    let mut dictionary = BTreeMap::new();
    let mut whole = true;
    for _ in 0..entries {
        let id = reader.read_u32()?;
        let length = i64::from(reader.read_u32()?);
        let bytes = to_int((length - 1) * width)?;
        if bytes > MAX_DICTIONARY_ENTRY {
            whole = false;
            break;
        }
        match dictionary_entry(reader, bytes, codepage) {
            Ok(name) => {
                dictionary.insert(id, name);
            }
            Err(Fault::Unported(reason)) => return Err(Fault::Unported(reason)),
            Err(_) => {
                whole = false;
                break;
            }
        }
    }
    section.set_dictionary(dictionary);
    Ok(whole)
}

fn dictionary_entry(reader: &mut Reader<'_>, bytes: i32, codepage: i32) -> Read<String> {
    let length = allocation(i64::from(bytes), MAX_STRING_LENGTH)?;
    let name = codepage::decode(reader.take(length)?, codepage)?;
    let padding = if codepage == codepage::UNICODE {
        2 + ((4 - ((bytes + 2) & 3)) & 3)
    } else {
        1
    };
    reader.skip(padding as usize);
    Ok(name)
}

/// `new Property(id, stream, length, codepage)`: the value `VariantSupport.read` gives.
fn read_property(reader: &mut Reader<'_>, length: i32, codepage: i32) -> Read<Option<Value>> {
    let variant = reader.read_u32()? as i32;
    let start = reader.position();
    let typed = match read_value(reader, variant) {
        Ok(typed) => typed,
        Err(Fault::UnsupportedOperation) => {
            return Ok(Some(Value::Bytes(
                reader.read_bounded(length, MAX_STRING_LENGTH)?,
            )))
        }
        Err(fault) => return Err(fault),
    };
    Ok(match typed {
        Typed::Null => None,
        Typed::Value(value) => Some(value),
        Typed::CodePageString(bytes) => {
            let codepage = if codepage == -1 {
                codepage::DEFAULT
            } else {
                codepage
            };
            Some(Value::String(until_nul(codepage::decode(
                &bytes, codepage,
            )?)))
        }
        Typed::UnicodeString(bytes) => {
            if bytes.is_empty() {
                None
            } else {
                Some(Value::String(until_nul(codepage::decode(
                    &bytes[..bytes.len() & !1],
                    codepage::UNICODE,
                )?)))
            }
        }
        Typed::Unconverted => {
            let consumed = reader.position() - start;
            let length = allocation(consumed, MAX_STRING_LENGTH)?;
            reader.set_position(start)?;
            Some(Value::Bytes(reader.take(length)?.to_vec()))
        }
    })
}

/// The text up to its first NUL, as `getJavaValue` and `toJavaString` cut it.
fn until_nul(mut text: String) -> String {
    if let Some(at) = text.find('\0') {
        text.truncate(at);
    }
    text
}

/// What `TypedPropertyValue.readValue` read, as far as `VariantSupport.read` converts it.
enum Typed {
    Null,
    Value(Value),
    CodePageString(Vec<u8>),
    UnicodeString(Vec<u8>),
    /// A type `VariantSupport.read` gives the bytes of.
    Unconverted,
}

mod vt {
    pub(super) const EMPTY: i32 = 0;
    pub(super) const NULL: i32 = 1;
    pub(super) const I2: i32 = 2;
    pub(super) const I4: i32 = 3;
    pub(super) const R4: i32 = 4;
    pub(super) const R8: i32 = 5;
    pub(super) const CY: i32 = 6;
    pub(super) const DATE: i32 = 7;
    pub(super) const BSTR: i32 = 8;
    pub(super) const ERROR: i32 = 10;
    pub(super) const BOOL: i32 = 11;
    pub(super) const VARIANT: i32 = 12;
    pub(super) const DECIMAL: i32 = 14;
    pub(super) const I1: i32 = 16;
    pub(super) const UI1: i32 = 17;
    pub(super) const UI2: i32 = 18;
    pub(super) const UI4: i32 = 19;
    pub(super) const I8: i32 = 20;
    pub(super) const UI8: i32 = 21;
    pub(super) const INT: i32 = 22;
    pub(super) const UINT: i32 = 23;
    pub(super) const LPSTR: i32 = 30;
    pub(super) const LPWSTR: i32 = 31;
    pub(super) const FILETIME: i32 = 64;
    pub(super) const BLOB: i32 = 65;
    pub(super) const STREAM: i32 = 66;
    pub(super) const STORAGE: i32 = 67;
    pub(super) const STREAMED_OBJECT: i32 = 68;
    pub(super) const STORED_OBJECT: i32 = 69;
    pub(super) const BLOB_OBJECT: i32 = 70;
    pub(super) const CF: i32 = 71;
    pub(super) const CLSID: i32 = 72;
    pub(super) const VERSIONED_STREAM: i32 = 73;
    pub(super) const VECTOR: i32 = 0x1000;
    pub(super) const ARRAY: i32 = 0x2000;
    /// The element types of a vector POI reads.
    pub(super) const VECTOR_ELEMENTS: [i32; 21] = [
        I2, I4, R4, R8, CY, DATE, BSTR, ERROR, BOOL, VARIANT, I1, UI1, UI2, UI4, I8, UI8, LPSTR,
        LPWSTR, FILETIME, CF, CLSID,
    ];
    /// The element types of an array POI reads.
    pub(super) const ARRAY_ELEMENTS: [i32; 17] = [
        I2, I4, R4, R8, CY, DATE, BSTR, ERROR, BOOL, VARIANT, DECIMAL, I1, UI1, UI2, UI4, INT, UINT,
    ];
}

/// `TypedPropertyValue.readValue` for a value of type `variant`.
fn read_value(reader: &mut Reader<'_>, variant: i32) -> Read<Typed> {
    let value = |value: Value| Ok(Typed::Value(value));
    match variant {
        vt::EMPTY | vt::NULL => Ok(if variant == vt::EMPTY {
            Typed::Null
        } else {
            Typed::Unconverted
        }),
        vt::I1 => value(Value::Byte(reader.read_i8()?)),
        vt::UI1 => value(Value::Integer(i32::from(reader.read_u8()?))),
        vt::I2 => value(Value::Integer(i32::from(reader.read_i16()?))),
        vt::UI2 => value(Value::Integer(i32::from(reader.read_u16()?))),
        vt::I4 => value(Value::Integer(reader.read_i32()?)),
        vt::INT => {
            reader.read_i32()?;
            Ok(Typed::Unconverted)
        }
        vt::UI4 => value(Value::Long(i64::from(reader.read_u32()?))),
        vt::UINT | vt::ERROR => {
            reader.read_u32()?;
            Ok(Typed::Unconverted)
        }
        vt::I8 => value(Value::Long(reader.read_i64()?)),
        vt::UI8 => value(Value::BigInteger(reader.read_i64()? as u64)),
        vt::R4 => value(Value::Float(f32::from_bits(reader.read_i32()? as u32))),
        vt::R8 => value(Value::Double(f64::from_bits(reader.read_i64()? as u64))),
        vt::FILETIME => value(Value::Date(filetime_to_millis(reader.read_i64()?))),
        vt::BOOL => value(Value::Boolean(reader.read_i16()? != 0)),
        vt::LPSTR => Ok(Typed::CodePageString(code_page_string(reader)?)),
        vt::LPWSTR => Ok(Typed::UnicodeString(unicode_string(reader)?)),
        vt::CF => value(Value::Bytes(clipboard_data(reader)?)),
        _ => {
            skip_value(reader, variant)?;
            Ok(Typed::Unconverted)
        }
    }
}

/// `readValue` for the types `VariantSupport.read` gives the bytes of, read past.
fn skip_value(reader: &mut Reader<'_>, variant: i32) -> Read<()> {
    match variant {
        vt::CY | vt::DATE => {
            reader.take(8)?;
        }
        vt::DECIMAL | vt::CLSID => {
            reader.take(16)?;
        }
        vt::BSTR | vt::STREAM | vt::STORAGE | vt::STREAMED_OBJECT | vt::STORED_OBJECT => {
            code_page_string(reader)?;
        }
        vt::BLOB | vt::BLOB_OBJECT => {
            let size = reader.read_i32()?;
            let length = allocation(i64::from(size), MAX_BLOB_LENGTH)?;
            reader.take(length)?;
        }
        vt::VERSIONED_STREAM => {
            reader.take(16)?;
            code_page_string(reader)?;
        }
        _ if variant & !0x0FFF == vt::VECTOR
            && vt::VECTOR_ELEMENTS.contains(&(variant & 0x0FFF)) =>
        {
            read_vector(reader, variant & 0x0FFF)?;
        }
        _ if variant & !0x0FFF == vt::ARRAY && vt::ARRAY_ELEMENTS.contains(&(variant & 0x0FFF)) => {
            read_array(reader)?;
        }
        _ => return Err(Fault::UnsupportedOperation),
    }
    Ok(())
}

/// `TypedPropertyValue.read`: a type, two bytes of padding and a value of that type.
fn read_typed(reader: &mut Reader<'_>) -> Read<()> {
    let variant = i32::from(reader.read_i16()?);
    reader.read_i16()?;
    match variant {
        vt::EMPTY
        | vt::NULL
        | vt::I1
        | vt::UI1
        | vt::I2
        | vt::UI2
        | vt::I4
        | vt::INT
        | vt::UI4
        | vt::UINT
        | vt::ERROR
        | vt::I8
        | vt::UI8
        | vt::R4
        | vt::R8
        | vt::FILETIME
        | vt::BOOL
        | vt::LPSTR
        | vt::LPWSTR
        | vt::CF => read_value(reader, variant).map(drop),
        _ => skip_value(reader, variant),
    }
}

fn read_vector(reader: &mut Reader<'_>, element: i32) -> Read<()> {
    let length = reader.read_u32()?;
    if length > i32::MAX as u32 {
        return Err(Fault::UnsupportedOperation);
    }
    for _ in 0..length {
        if element == vt::VARIANT {
            read_typed(reader)?;
        } else {
            read_value(reader, element)?;
        }
    }
    Ok(())
}

fn read_array(reader: &mut Reader<'_>) -> Read<()> {
    let element = reader.read_i32()?;
    let dimensions = reader.read_u32()?;
    if !(1..=31).contains(&dimensions) {
        return Err(Fault::Runtime);
    }
    let mut scalars: i64 = 1;
    for _ in 0..dimensions {
        scalars = scalars.wrapping_mul(i64::from(reader.read_u32()?));
        reader.read_i32()?;
    }
    if scalars > i64::from(i32::MAX) {
        return Err(Fault::UnsupportedOperation);
    }
    allocation(i64::from(scalars as i32), MAX_ARRAY_SCALARS)?;
    for _ in 0..scalars {
        read_typed(reader)?;
        if element != vt::VARIANT {
            reader.skip_padding();
        }
    }
    Ok(())
}

/// `CodePageString.read`: the bytes, padding past them unless there are none.
fn code_page_string(reader: &mut Reader<'_>) -> Read<Vec<u8>> {
    let size = reader.read_i32()?;
    let length = allocation(i64::from(size), MAX_STRING_LENGTH)?;
    if length == 0 {
        return Ok(Vec::new());
    }
    let bytes = reader.take(length)?.to_vec();
    reader.skip_padding();
    Ok(bytes)
}

/// `UnicodeString.read`: the bytes, which must end in a NUL character.
fn unicode_string(reader: &mut Reader<'_>) -> Read<Vec<u8>> {
    let characters = reader.read_i32()?;
    let length = allocation(i64::from(characters.wrapping_mul(2)), MAX_STRING_LENGTH)?;
    if characters == 0 {
        return Ok(Vec::new());
    }
    let bytes = reader.take(length)?.to_vec();
    if bytes.len() < 2 || bytes[bytes.len() - 2..] != [0, 0] {
        return Err(Fault::Runtime);
    }
    reader.skip_padding();
    Ok(bytes)
}

/// `ClipboardData.read` and `toByteArray`: the size, the format and the data.
fn clipboard_data(reader: &mut Reader<'_>) -> Read<Vec<u8>> {
    let size = i64::from(reader.read_i32()?);
    let (format, data) = if size < 4 {
        (0, Vec::new())
    } else {
        let format = reader.read_i32()?;
        let length = allocation(size - 4, MAX_CLIPBOARD_LENGTH)?;
        (format, reader.take(length)?.to_vec())
    };
    let mut bytes = Vec::with_capacity(8 + data.len());
    bytes.extend_from_slice(&(4 + data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&format.to_le_bytes());
    bytes.extend_from_slice(&data);
    Ok(bytes)
}

/// `Filetime.filetimeToDate`: the file time read unsigned, in milliseconds since the epoch.
pub(super) fn filetime_to_millis(filetime: i64) -> i64 {
    ((filetime as u64) / 10_000) as i64 - FILETIME_EPOCH_MILLIS
}

/// `Filetime.dateToFileTime`, wrapping as `BigInteger.longValue` does.
pub(super) fn millis_to_filetime(millis: i64) -> i64 {
    millis
        .wrapping_add(FILETIME_EPOCH_MILLIS)
        .wrapping_mul(10_000)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A property set of one section with `format_id` and the given properties, each an id
    /// and the bytes after its offset, the type first.
    pub(in crate::formats::ole2) fn property_set(
        format_id: [u8; 16],
        properties: &[(u32, Vec<u8>)],
    ) -> Vec<u8> {
        sections(&[(format_id, properties.to_vec())])
    }

    /// A property set of the given sections.
    /// A section's format id and its properties.
    pub(in crate::formats::ole2) type SectionBytes = ([u8; 16], Vec<(u32, Vec<u8>)>);

    pub(in crate::formats::ole2) fn sections(sections: &[SectionBytes]) -> Vec<u8> {
        let mut stream = vec![0xFE, 0xFF, 0, 0, 0x04, 0x0A, 0x02, 0];
        stream.extend([0u8; 16]);
        stream.extend((sections.len() as i32).to_le_bytes());
        let mut bodies = Vec::new();
        let mut offset = 28 + 20 * sections.len();
        for (format_id, properties) in sections {
            stream.extend(invert(format_id));
            stream.extend((offset as u32).to_le_bytes());
            let body = section(properties);
            offset += body.len();
            bodies.push(body);
        }
        for body in bodies {
            stream.extend(body);
        }
        stream
    }

    fn section(properties: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut values = Vec::new();
        let mut table = Vec::new();
        let header = 8 + 8 * properties.len();
        for (id, bytes) in properties {
            table.extend(id.to_le_bytes());
            table.extend(((header + values.len()) as u32).to_le_bytes());
            values.extend(bytes);
            while values.len() % 4 != 0 {
                values.push(0);
            }
        }
        let mut body = ((header + values.len()) as u32).to_le_bytes().to_vec();
        body.extend((properties.len() as u32).to_le_bytes());
        body.extend(table);
        body.extend(values);
        body
    }

    pub(in crate::formats::ole2) fn lpstr(text: &str) -> Vec<u8> {
        let mut bytes = (vt::LPSTR as u32).to_le_bytes().to_vec();
        bytes.extend(((text.len() + 1) as i32).to_le_bytes());
        bytes.extend(text.as_bytes());
        bytes.push(0);
        bytes
    }

    pub(in crate::formats::ole2) fn lpwstr(text: &str) -> Vec<u8> {
        let mut bytes = (vt::LPWSTR as u32).to_le_bytes().to_vec();
        let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
        bytes.extend((units.len() as i32).to_le_bytes());
        bytes.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        bytes
    }

    pub(in crate::formats::ole2) fn typed(variant: i32, value: &[u8]) -> Vec<u8> {
        let mut bytes = (variant as u32).to_le_bytes().to_vec();
        bytes.extend(value);
        bytes
    }

    pub(in crate::formats::ole2) fn codepage(codepage: u16) -> Vec<u8> {
        typed(vt::I2, &codepage.to_le_bytes())
    }

    /// A dictionary of names in code page 1252.
    pub(in crate::formats::ole2) fn dictionary(names: &[(u32, &str)]) -> Vec<u8> {
        let mut bytes = (names.len() as u32).to_le_bytes().to_vec();
        for (id, name) in names {
            bytes.extend(id.to_le_bytes());
            bytes.extend(((name.len() + 1) as u32).to_le_bytes());
            bytes.extend(name.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn reads_values_as_variant_support_converts_them() {
        let stream = property_set(
            SUMMARY_INFORMATION,
            &[
                (1, codepage(1252)),
                (2, lpstr("caf\u{e9}")),
                (3, lpwstr("tea")),
                (4, typed(vt::I2, &(-2i16).to_le_bytes())),
                (5, typed(vt::UI4, &u32::MAX.to_le_bytes())),
                (
                    6,
                    typed(vt::FILETIME, &116_444_736_000_000_000i64.to_le_bytes()),
                ),
                (7, typed(vt::BOOL, &[0xFF, 0xFF])),
                (8, typed(vt::INT, &7i32.to_le_bytes())),
                (9, typed(0x7777, &[1, 2])),
                (10, typed(vt::I4, &5i32.to_le_bytes())),
            ],
        );
        let set = read(&stream).unwrap().unwrap();
        assert!(set.is_summary_information());
        assert!(!set.is_document_summary_information());
        let section = set.first_section();
        assert_eq!(section.get(1), Some(&Value::Integer(1252)));
        assert_eq!(
            section.get(2),
            Some(&Value::String("caf\u{c3}\u{a9}".into()))
        );
        assert_eq!(section.get(3), Some(&Value::String("tea".into())));
        assert_eq!(section.get(4), Some(&Value::Integer(-2)));
        assert_eq!(section.get(5), Some(&Value::Long(i64::from(u32::MAX))));
        assert_eq!(section.get(6), Some(&Value::Date(0)));
        assert_eq!(section.get(7), Some(&Value::Boolean(true)));
        assert_eq!(section.get(8), Some(&Value::Bytes(vec![7, 0, 0, 0])));
        assert_eq!(
            section.get(9),
            Some(&Value::Bytes(vec![1, 2, 0, 0, 3, 0, 0, 0])),
            "an unknown type keeps the property's length from after its type"
        );
        assert_eq!(section.get(10), Some(&Value::Integer(5)));
    }

    #[test]
    fn fails_where_an_unknown_type_reads_past_the_end() {
        let stream = property_set(SUMMARY_INFORMATION, &[(9, typed(0x7777, &[1, 2]))]);
        assert_eq!(read(&stream), Ok(None));
    }

    #[test]
    fn reads_dictionaries_and_their_code_page() {
        let stream = sections(&[
            (DOCUMENT_SUMMARY_INFORMATION, vec![]),
            (
                USER_PROPERTIES,
                vec![
                    (0, dictionary(&[(2, "Client"), (3, "Due")])),
                    (2, lpstr("Acme")),
                ],
            ),
        ]);
        let set = read(&stream).unwrap().unwrap();
        assert!(set.is_document_summary_information());
        let custom = &set.sections[1];
        let names: Vec<&str> = custom
            .dictionary()
            .unwrap()
            .values()
            .map(String::as_str)
            .collect();
        assert_eq!(names, ["Client", "Due"]);
        assert_eq!(custom.get(1), Some(&Value::Integer(1252)));
        let ids: Vec<u32> = custom.properties().map(|(id, _)| id).collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn passes_over_what_poi_throws_on() {
        assert_eq!(read(b""), Ok(None));
        assert_eq!(read(b"\xFE\xFF\0\0"), Ok(None));
        let mut stream = property_set(SUMMARY_INFORMATION, &[(2, lpwstr("x"))]);
        let at = stream.len() - 2;
        stream[at] = b'y';
        assert_eq!(read(&stream), Ok(None));
    }

    #[test]
    fn converts_file_times_as_poi_does() {
        assert_eq!(filetime_to_millis(116_444_736_000_000_000), 0);
        assert_eq!(
            filetime_to_millis(-1),
            (u64::MAX / 10_000) as i64 - FILETIME_EPOCH_MILLIS
        );
        assert_eq!(
            millis_to_filetime(filetime_to_millis(123_456_789)),
            123_450_000
        );
    }
}
