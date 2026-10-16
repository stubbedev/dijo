//! Tika's `TikaNameIdChunks` and `ExtendedMetadataExtractor`: the named properties of a message,
//! their storage ids mapped to the names and property sets its name-id streams give them, and
//! those Tika's table of MAPI properties knows written as `mapi:property:` metadata.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::chunks::{Chunk, Message};
use super::properties::Value;
use super::types::Type;
use crate::formats::metadata::TikaMetadata;
use crate::formats::poi::{Read, Thrown};
use crate::java::is_blank;
use crate::java::time::format_instant;
use crate::text::charset::Charset;

pub(super) const PROPERTY_PREFIX: &str = "mapi:property:";
const PROPERTY_TABLE: &str = include_str!("props_table.txt");
const PS_MAPI: &str = "00020328-0000-0000-C000-000000000046";
const PS_PUBLIC_STRINGS: &str = "00020329-0000-0000-C000-000000000046";
const PS_INTERNET_HEADERS: &str = "00020386-0000-0000-C000-000000000046";

/// A class id, in the order of its UUID's bytes.
type ClassId = [u8; 16];

/// `new ClassID(String)`: the hexadecimal digits of a UUID, braces and dashes dropped.
fn parse_class_id(text: &str) -> ClassId {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !matches!(byte, b'{' | b'}' | b'-'))
        .collect();
    let mut id = [0u8; 16];
    for (slot, pair) in id.iter_mut().zip(digits.chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII digits"), 16)
            .expect("a class id of hexadecimal digits");
    }
    id
}

/// `new ClassID(bytes, offset)`: a class id as stored, its first three fields little-endian.
fn stored_class_id(bytes: &[u8]) -> ClassId {
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes[..16]);
    id[..4].reverse();
    id[4..6].reverse();
    id[6..8].reverse();
    id
}

/// A named property's tag: the id or string offset it is named by, and its property set.
#[derive(Debug, Clone)]
struct Tag {
    id: i32,
    class_id: Option<ClassId>,
}

/// `TikaNameIdChunks.loadTags`: the tags of each storage id.
fn tags(chunks: &[Chunk]) -> Read<HashMap<i64, Vec<Tag>>> {
    let binary = |id: i32| {
        chunks
            .iter()
            .rev()
            .find(|chunk| chunk.kind == Type::BINARY && chunk.id == id)
            .and_then(Chunk::bytes)
    };
    let mut tags: HashMap<i64, Vec<Tag>> = HashMap::new();
    let (Some(guids), Some(entries), Some(strings)) = (binary(2), binary(3), binary(4)) else {
        return Ok(tags);
    };
    for entry in entries.as_chunks::<8>().0 {
        let name_offset = u32::from_le_bytes(entry[..4].try_into().expect("four bytes"));
        let mut guid_index = i32::from(u16::from_le_bytes([entry[4], entry[5]]));
        let kind = guid_index & 1;
        guid_index >>= 1;
        let property_index = i64::from(u16::from_le_bytes([entry[6], entry[7]]));
        let guid = property_guid(guid_index, guids);
        let mut name_crc: i64 = -1;
        let stream_id = if kind == 0 {
            0x1000 + i64::from((name_offset as i32 ^ (guid_index << 1)) % 0x1F)
        } else {
            let offset = name_offset as i32;
            if (strings.len() as i64) > i64::from(offset) {
                let at = usize::try_from(offset).map_err(|_| {
                    Thrown(format!(
                        "Index {offset} out of bounds for length {}",
                        strings.len()
                    ))
                })?;
                let length = u32::from_le_bytes(
                    strings
                        .get(at..at + 4)
                        .ok_or_else(|| Thrown(format!("Index {at} out of bounds")))?
                        .try_into()
                        .expect("four bytes"),
                );
                if strings.len() as i64 >= i64::from(offset) + 4 + i64::from(length) {
                    let start = at + 4;
                    let name = &strings[start..start + length as usize];
                    name_crc = if guid == Some(parse_class_id(PS_INTERNET_HEADERS)) {
                        let decoded = Charset::named("UTF-16LE")
                            .decode(name)
                            .expect("the JDK decodes UTF-16LE");
                        let lower: Vec<u8> = crate::java::to_lower_case(&decoded)
                            .encode_utf16()
                            .flat_map(u16::to_le_bytes)
                            .collect();
                        crc(&lower)
                    } else {
                        crc(name)
                    };
                }
            }
            0x1000 + (name_crc ^ i64::from((guid_index << 1) | 1)) % 0x1F
        };
        let tag = if kind == 1 && name_crc < 0 {
            0x8000 + property_index
        } else {
            property_tag(chunks, stream_id, i64::from(name_offset), name_crc)
        };
        if tag > 0 && tag < i64::from(i32::MAX) {
            tags.entry(tag).or_default().push(Tag {
                id: name_offset as i32,
                class_id: guid,
            });
        }
    }
    Ok(tags)
}

/// `getPropertyTag(streamID, nameOffset, propertyNameCRC32)`.
fn property_tag(chunks: &[Chunk], stream_id: i64, name_offset: i64, name_crc: i64) -> i64 {
    for chunk in chunks {
        if chunk.kind != Type::BINARY || i64::from(chunk.id) != stream_id {
            continue;
        }
        let Some(bytes) = chunk.bytes() else {
            continue;
        };
        for entry in bytes.as_chunks::<8>().0 {
            let crc = i64::from(u32::from_le_bytes(
                entry[..4].try_into().expect("four bytes"),
            ));
            let guid_index = u16::from_le_bytes([entry[4], entry[5]]);
            let property_index = i64::from(u16::from_le_bytes([entry[6], entry[7]]));
            let expected = if guid_index & 1 == 0 {
                name_offset
            } else {
                name_crc
            };
            if crc == expected {
                return 0x8000 + property_index;
            }
        }
    }
    0
}

/// `getPropertyGUID`.
fn property_guid(index: i32, guids: &[u8]) -> Option<ClassId> {
    match index {
        1 => Some(parse_class_id(PS_MAPI)),
        2 => Some(parse_class_id(PS_PUBLIC_STRINGS)),
        index if index >= 3 => {
            let offset = (index as usize - 3) * 0x10;
            guids.get(offset..offset + 0x10).map(stored_class_id)
        }
        _ => None,
    }
}

/// `calculateCRC32`: the CRC-32 of four bytes 0xFF and the data, inverted.
fn crc(data: &[u8]) -> i64 {
    let mut crc = flate2::Crc::new();
    crc.update(&[0xFF; 4]);
    crc.update(data);
    i64::from(!crc.sum())
}

/// A MAPI property Tika's table names.
#[derive(Debug, Clone)]
struct Known {
    name: String,
    class_id: Option<ClassId>,
    types: Vec<Type>,
}

/// Tika's table of MAPI properties, by short id and by long id.
struct Table {
    short: HashMap<i32, Vec<Known>>,
    long: HashMap<i32, Vec<Known>>,
}

/// `ExtendedMetadataExtractor.loadProperties`.
fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Table {
            short: HashMap::new(),
            long: HashMap::new(),
        };
        for line in PROPERTY_TABLE.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let columns: Vec<&str> = line.split('|').collect();
            assert_eq!(columns.len(), 11, "column count must == 11: {line}");
            let known = Known {
                name: columns[1].trim().to_owned(),
                class_id: table_class_id(columns[3]),
                types: columns[7].split(';').filter_map(table_type).collect(),
            };
            let (short, long) = (columns[5], columns[6]);
            if !is_blank(short) {
                let id = i32::from_str_radix(&short[2..], 16).expect("a short id");
                table.short.entry(id).or_default().push(known);
            } else if !is_blank(long) {
                let id = i64::from_str_radix(&long[2..], 16).expect("a long id");
                let id = i32::try_from(id).expect("id must actually be within int range");
                table.long.entry(id).or_default().push(known);
            }
        }
        table
    })
}

/// `parseClassId`: the class id after the set's name.
fn table_class_id(text: &str) -> Option<ClassId> {
    if is_blank(text) {
        return None;
    }
    let space = text.find(' ')?;
    Some(parse_class_id(text[space..].trim()))
}

/// `parseDataType`: the type of `PtypName, 0x0003`.
fn table_type(text: &str) -> Option<Type> {
    if is_blank(text) {
        return None;
    }
    let (_, number) = text
        .split_once(", ")
        .expect("expected two parts of a data type");
    let number = number.strip_prefix("0x").unwrap_or(number);
    Some(Type(
        i32::from_str_radix(number, 16).expect("a hexadecimal type"),
    ))
}

/// `ExtendedMetadataExtractor.extract`: the message's named properties Tika's table knows.
pub(super) fn extract(message: &Message, metadata: &mut TikaMetadata) -> Read<()> {
    let Some(name_ids) = &message.name_ids else {
        return Ok(());
    };
    let length: usize = name_ids
        .chunks
        .iter()
        .filter_map(Chunk::bytes)
        .map(<[u8]>::len)
        .sum();
    if length == 0 {
        return Ok(());
    }
    let tags = tags(&name_ids.chunks)?;
    let Some(properties) = message.main.properties() else {
        return Ok(());
    };
    let table = table();
    for property in &properties.properties {
        let mut pair = None;
        for tag in tags.get(&i64::from(property.id)).into_iter().flatten() {
            let candidates = table.long.get(&tag.id).or_else(|| table.short.get(&tag.id));
            let Some(candidates) = candidates else {
                continue;
            };
            let class_id = tag
                .class_id
                .ok_or_else(|| Thrown::new("a named property of no property set"))?;
            pair = candidates.iter().find(|known| {
                known.class_id == Some(class_id) && known.types.contains(&property.kind)
            });
            if pair.is_some() {
                break;
            }
        }
        let Some(known) = pair else {
            continue;
        };
        let kind = property.kind;
        if kind == Type::BINARY
            || kind == Type::UNKNOWN
            || kind == Type::UNSPECIFIED
            || kind == Type::DIRECTORY
            || kind.is_pointer()
        {
            continue;
        }
        let text = match &property.value {
            Value::Time(millis) if [Type::TIME, Type::MV_TIME].contains(&kind) => {
                format_instant(*millis)
            }
            _ if [Type::TIME, Type::MV_TIME, Type::APP_TIME, Type::MV_APP_TIME].contains(&kind) => {
                return Err(Thrown::new(
                    "an application time Tika casts to a calendar, which fails",
                ));
            }
            Value::Boolean(value) => value.to_string(),
            Value::Float(_) => {
                return Err(Thrown::new(
                    "a named float property, whose Float.toString this port does not write",
                ))
            }
            value => {
                let text = value_string(value);
                if is_blank(&text) {
                    continue;
                }
                text
            }
        };
        metadata.add_prefixed(PROPERTY_PREFIX, &known.name, text);
    }
    Ok(())
}

/// `PropertyValue.toString`.
fn value_string(value: &Value) -> String {
    match value {
        Value::Pointer | Value::Null => "(No value available)".to_owned(),
        Value::Boolean(value) => value.to_string(),
        Value::Short(value) => value.to_string(),
        Value::Long(value) => value.to_string(),
        Value::LongLong(value) => value.to_string(),
        Value::Float(_) => unreachable!("a float is left to Tika"),
        Value::Double(value) => crate::java::dtoa::java_double_to_string(*value),
        Value::Currency(value) => value.to_string(),
        Value::Time(_) => unreachable!("a time is written as an instant"),
        Value::Bytes(bytes) => debug_string(bytes),
    }
}

/// `ByteChunk.toDebugFriendlyString`.
pub(super) fn debug_string(bytes: &[u8]) -> String {
    let limit = if bytes.len() > 16 { 12 } else { bytes.len() };
    let shown: Vec<String> = bytes[..limit]
        .iter()
        .map(|&byte| (byte as i8).to_string())
        .collect();
    let more = if bytes.len() > 16 { ",...." } else { "" };
    format!("Bytes len={} [{}{more}]", bytes.len(), shown.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tikas_table_of_mapi_properties() {
        let table = table();
        let address = table
            .long
            .get(&0x8029)
            .expect("PidLidAddressBookProviderArrayType");
        assert_eq!(address[0].name, "PidLidAddressBookProviderArrayType");
        assert_eq!(
            address[0].class_id,
            Some(parse_class_id("00062004-0000-0000-C000-000000000046"))
        );
        assert_eq!(address[0].types, [Type::LONG]);
    }

    #[test]
    fn writes_values_as_poi_does() {
        assert_eq!(debug_string(&[1, 255]), "Bytes len=2 [1,-1]");
        assert_eq!(
            debug_string(&[0; 17]),
            "Bytes len=17 [0,0,0,0,0,0,0,0,0,0,0,0,....]"
        );
        assert_eq!(value_string(&Value::Null), "(No value available)");
        assert_eq!(
            stored_class_id(&[3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15]),
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    #[test]
    fn computes_the_crc_poi_names_properties_by() {
        assert_eq!(crc(b""), 0);
    }
}
