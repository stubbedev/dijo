//! POI's `PropertiesChunk`: the fixed-length properties of a message, a recipient or an
//! attachment, read from its properties stream until a property POI cannot read, each value as
//! `PropertyValue` and its subclasses give it.

use super::types::{usual_type, Type, MULTIVALUED_FLAG};

/// The number of milliseconds from 1601 to 1970, which FILETIMEs count from.
const FILETIME_OFFSET: i64 = 1000 * 60 * 60 * 24 * (365 * 369 + 89);

/// A property's value, as `PropertyValue` gives it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    /// `ChunkBasedPropertyValue`: a value of variable length, whose chunk POI never matches.
    Pointer,
    Null,
    Boolean(bool),
    Short(i16),
    Long(i32),
    LongLong(i64),
    Float(f32),
    Double(f64),
    /// The value divided by 10,000, as a `BigInteger`.
    Currency(i64),
    /// Milliseconds since 1970.
    Time(i64),
    /// Any other type's bytes.
    Bytes(Vec<u8>),
}

/// A property and its value.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Property {
    pub(super) id: i32,
    /// Whether POI knows the property, which it then keeps one value of.
    known: bool,
    /// `getActualType`, without the flag of a multi-valued type.
    pub(super) kind: Type,
    /// Whether the property is multi-valued, its values kept in chunks of their own.
    pub(super) multi: bool,
    pub(super) value: Value,
}

/// A properties chunk: the properties in the order they were read, a known property's last
/// value in the place of its first.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PropertySet {
    pub(super) properties: Vec<Property>,
}

impl PropertySet {
    /// `getProperties().get(property)`'s first value, of a property POI knows.
    pub(super) fn get(&self, id: i32) -> Option<&Value> {
        self.properties
            .iter()
            .find(|property| property.known && property.id == id)
            .map(|property| &property.value)
            .filter(|value| **value != Value::Pointer)
    }
}

/// The properties stream of a message, a recipient or an attachment.
pub(super) const PROPERTIES: &str = "__properties_version1.0";
/// The length of the header of a message's properties stream.
pub(super) const MESSAGE_HEADER: usize = 32;
/// The length of the header of the properties stream of a message embedded in another, which
/// lacks the last reserved eight bytes of a message's.
pub(super) const EMBEDDED_MESSAGE_HEADER: usize = 24;

/// Reads a properties stream: a message's after its header, an embedded message's after its
/// shorter one, a storage's after 8 bytes, `message` saying which and whether embedded.
pub(super) fn read_properties(data: &[u8], message: Option<bool>) -> PropertySet {
    let header = match message {
        Some(true) => EMBEDDED_MESSAGE_HEADER,
        Some(false) => MESSAGE_HEADER,
        None => 8,
    };
    let mut set = PropertySet::default();
    let Some(mut rest) = data.get(header..) else {
        return set;
    };
    while let Some(head) = rest.get(..8) {
        let type_id = u16::from_le_bytes([head[0], head[1]]);
        let id = u16::from_le_bytes([head[2], head[3]]);
        rest = &rest[8..];
        let mut type_id = i32::from(type_id);
        let id = i32::from(id);
        let multi = type_id & MULTIVALUED_FLAG != 0;
        if multi {
            type_id -= MULTIVALUED_FLAG;
        }
        let Some(kind) = Type::built_in(type_id) else {
            break;
        };
        let known_type = usual_type(id);
        let usual = known_type.unwrap_or(kind);
        if usual != kind {
            let strings = [Type::ASCII_STRING, Type::UNICODE_STRING];
            if !(strings.contains(&usual) && strings.contains(&kind)) && usual != Type::UNKNOWN {
                break;
            }
        }
        let pointer = kind.is_pointer() || multi;
        let length = if pointer { 8 } else { kind.length() as usize };
        let data = take(&mut rest, length);
        if length < 8 {
            take(&mut rest, 8 - length);
        }
        let value = if pointer {
            Value::Pointer
        } else {
            value(kind, &data)
        };
        let property = Property {
            id,
            known: known_type.is_some(),
            kind,
            multi,
            value,
        };
        match set
            .properties
            .iter_mut()
            .find(|existing| property.known && existing.known && existing.id == id)
        {
            Some(existing) => *existing = property,
            None => set.properties.push(property),
        }
    }
    set
}

/// `IOUtils.readFully`: as many bytes as there are, up to `length`, the rest zero.
fn take(rest: &mut &[u8], length: usize) -> Vec<u8> {
    let available = length.min(rest.len());
    let mut data = rest[..available].to_vec();
    data.resize(length, 0);
    *rest = &rest[available..];
    data
}

/// The value of a property of the fixed-length `kind`, of its bytes.
pub(super) fn value(kind: Type, data: &[u8]) -> Value {
    let short = || i16::from_le_bytes([data[0], data[1]]);
    let int = || i32::from_le_bytes(data[..4].try_into().expect("four bytes"));
    let long = || i64::from_le_bytes(data[..8].try_into().expect("eight bytes"));
    match kind {
        Type::NULL => Value::Null,
        Type::BOOLEAN => Value::Boolean(short() > 0),
        Type::SHORT => Value::Short(short()),
        Type::LONG => Value::Long(int()),
        Type::LONG_LONG => Value::LongLong(long()),
        Type::FLOAT => Value::Float(f32::from_bits(int() as u32)),
        Type::DOUBLE => Value::Double(f64::from_bits(long() as u64)),
        Type::CURRENCY => Value::Currency(long() / 10_000),
        Type::TIME => Value::Time(long() / 10 / 1000 - FILETIME_OFFSET),
        _ => Value::Bytes(data.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property(kind: u16, id: u16, value: &[u8]) -> Vec<u8> {
        let mut bytes = kind.to_le_bytes().to_vec();
        bytes.extend(id.to_le_bytes());
        bytes.extend([6, 0, 0, 0]);
        let mut padded = value.to_vec();
        padded.resize(8, 0);
        bytes.extend(padded);
        bytes
    }

    #[test]
    fn reads_properties_until_one_poi_cannot_read() {
        let mut data = vec![0; 8];
        data.extend(property(0x0003, 0x3FDE, &65001i32.to_le_bytes()));
        data.extend(property(
            0x0040,
            0x0039,
            &116_444_736_000_000_000i64.to_le_bytes(),
        ));
        data.extend(property(0x001F, 0x0037, &[]));
        data.extend(property(0x0003, 0x0037, &[]));
        data.extend(property(0x0003, 0x3FDE, &1i32.to_le_bytes()));
        let set = read_properties(&data, None);
        assert_eq!(set.get(0x3FDE), Some(&Value::Long(65001)));
        assert_eq!(set.get(0x0039), Some(&Value::Time(0)));
        assert_eq!(set.get(0x0037), None);
        assert_eq!(set.properties.len(), 3);
    }

    #[test]
    fn keeps_every_value_of_a_named_property_and_the_last_of_a_known_one() {
        let mut data = vec![0; 8];
        data.extend(property(0x0003, 0x8001, &1i32.to_le_bytes()));
        data.extend(property(0x0003, 0x8001, &2i32.to_le_bytes()));
        data.extend(property(0x000B, 0x0E1F, &1i16.to_le_bytes()));
        data.extend(property(0x000B, 0x0E1F, &0i16.to_le_bytes()));
        let set = read_properties(&data, None);
        assert_eq!(set.properties.len(), 3);
        assert_eq!(set.get(0x0E1F), Some(&Value::Boolean(false)));
        assert_eq!(set.get(0x8001), None);
    }
}
