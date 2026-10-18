//! HDF4 files, read in place for the metadata of their scientific data sets, as netcdf-java's
//! HDF4 reader models them: the data descriptors, then the vgroup of class `CDF0.0` that holds
//! the file's attributes, dimensions and variables, each variable a vgroup of class `Var0.0`
//! with its dimensions, attributes and the data descriptor (SDD) of its shape and type. The data
//! is never read.

use std::collections::{BTreeMap, BTreeSet};

use super::Contents;
use crate::formats::bytes::{malformed, text_to_nul, uint, unported, Bytes, Cursor, Order, Read};
use crate::formats::cdm::{Attribute, Axis, DataType, Dimension, Number, Values, Variable};

/// The magic number an HDF4 file starts with.
pub(super) const MAGIC: &[u8; 4] = b"\x0e\x03\x13\x01";

const NULL: u16 = 1;
const NUMBER_TYPE: u16 = 106;
const DIMENSIONS: u16 = 701;
const VDATA_HEADER: u16 = 1962;
const VDATA: u16 = 1963;
const VGROUP: u16 = 1965;
/// The bit of a tag that marks a special element: compressed, chunked, linked or external.
const SPECIAL: u16 = 0x4000;
/// The bit of a number type that marks its values little-endian.
const LITTLE_ENDIAN: u16 = 0x4000;
/// The bit of a number type that marks its values in the byte order of the machine that wrote
/// them, which the file does not record.
const NATIVE: u16 = 0x1000;

const FILE_CLASS: &str = "CDF0.0";
const VARIABLE_CLASS: &str = "Var0.0";
const DIMENSION_CLASSES: [&str; 2] = ["Dim0.0", "UDim0.0"];
const ATTRIBUTE_CLASS: &str = "Attr0.0";
const DIMENSION_VALUES_CLASS: &str = "DimVal0.0";
const DIMENSION_LENGTH_CLASS: &str = "DimVal0.1";

/// A data element, by its tag and reference number.
type Key = (u16, u16);

/// The data descriptors: where each element lies.
struct Descriptors<'a> {
    bytes: Bytes<'a>,
    elements: BTreeMap<Key, (u64, u64)>,
}

impl<'a> Descriptors<'a> {
    fn read(bytes: Bytes<'a>) -> Read<Self> {
        let mut elements = BTreeMap::new();
        let mut block = 4u64;
        let mut visited = BTreeSet::new();
        while block != 0 {
            if !visited.insert(block) {
                return Err(malformed(format!(
                    "HDF4 data descriptor blocks that loop back to offset {block}"
                )));
            }
            let mut header = bytes.cursor(block, 6, Order::Big, "an HDF4 data descriptor block")?;
            let count = u64::from(header.u16()?);
            let next = u64::from(header.u32()?);
            let mut entries = bytes.cursor(
                block + 6,
                count * 12,
                Order::Big,
                "an HDF4 data descriptor block",
            )?;
            for _ in 0..count {
                let tag = entries.u16()?;
                let reference = entries.u16()?;
                let offset = u64::from(entries.u32()?);
                let length = u64::from(entries.u32()?);
                if tag != NULL {
                    elements.insert((tag, reference), (offset, length));
                }
            }
            block = next;
        }
        Ok(Self { bytes, elements })
    }

    /// The cursor on element `key`, which holds `what`.
    fn element(&self, key: Key, what: &'static str) -> Read<Cursor<'a>> {
        let Some(&(offset, length)) = self.elements.get(&key) else {
            if self.elements.contains_key(&(key.0 | SPECIAL, key.1)) {
                return Err(unported(format!("{what} stored as a special element")));
            }
            return Err(malformed(format!(
                "{what}: no element of tag {} and reference {}",
                key.0, key.1
            )));
        };
        self.bytes.cursor(offset, length, Order::Big, what)
    }

    fn vgroups(&self) -> impl Iterator<Item = u16> + '_ {
        self.elements
            .keys()
            .filter(|(tag, _)| *tag == VGROUP)
            .map(|&(_, reference)| reference)
    }
}

/// A vgroup: its name and class, and the elements it holds.
struct Vgroup {
    name: String,
    class: String,
    members: Vec<Key>,
}

impl Vgroup {
    fn read(descriptors: &Descriptors, reference: u16) -> Read<Self> {
        let mut cursor = descriptors.element((VGROUP, reference), "an HDF4 vgroup")?;
        let count = usize::from(cursor.u16()?);
        let tags = cursor.take(2 * count)?;
        let references = cursor.take(2 * count)?;
        let members = tags
            .as_chunks::<2>()
            .0
            .iter()
            .zip(references.as_chunks::<2>().0)
            .map(|(&tag, &reference)| (u16::from_be_bytes(tag), u16::from_be_bytes(reference)))
            .collect();
        let name = counted_text(&mut cursor)?;
        let class = counted_text(&mut cursor)?;
        Ok(Self {
            name,
            class,
            members,
        })
    }

    fn holds(&self, tag: u16) -> impl Iterator<Item = u16> + '_ {
        self.members
            .iter()
            .filter(move |(member, _)| *member == tag)
            .map(|&(_, reference)| reference)
    }
}

/// A vdata's header: its records and their single field's type, and its name and class.
struct Vdata {
    records: u64,
    field_type: u16,
    field_size: usize,
    order: usize,
    name: String,
    class: String,
}

impl Vdata {
    fn read(descriptors: &Descriptors, reference: u16) -> Read<Self> {
        let mut cursor = descriptors.element((VDATA_HEADER, reference), "an HDF4 vdata header")?;
        cursor.u16()?;
        let records = u64::from(cursor.u32()?);
        cursor.u16()?;
        let fields = usize::from(cursor.u16()?);
        let types = cursor.take(2 * fields)?;
        let sizes = cursor.take(2 * fields)?;
        cursor.skip(2 * fields)?;
        let orders = cursor.take(2 * fields)?;
        for _ in 0..fields {
            counted_text(&mut cursor)?;
        }
        let name = counted_text(&mut cursor)?;
        let class = counted_text(&mut cursor)?;
        let first = |values: &[u8]| values.get(..2).map(|value| uint(value, Order::Big));
        let (Some(field_type), Some(field_size), Some(order)) =
            (first(types), first(sizes), first(orders))
        else {
            return Err(cursor.malformed(format!("vdata {name} of no field")));
        };
        Ok(Self {
            records,
            field_type: field_type as u16,
            field_size: field_size as usize,
            order: order as usize,
            name,
            class,
        })
    }

    /// The values of an attribute vdata.
    fn values(&self, descriptors: &Descriptors, reference: u16) -> Read<Values> {
        let number_type = NumberType::of(self.field_type)?;
        let count = usize::try_from(self.records)
            .ok()
            .and_then(|records| records.checked_mul(self.order))
            .ok_or_else(|| malformed(format!("attribute {} of too many values", self.name)))?;
        let mut data = descriptors.element((VDATA, reference), "an HDF4 attribute's values")?;
        if let NumberType::Text = number_type {
            let length = usize::try_from(self.records)
                .ok()
                .and_then(|records| records.checked_mul(self.field_size))
                .ok_or_else(|| malformed(format!("attribute {} of too many values", self.name)))?;
            return Ok(Values::Text(vec![text_to_nul(data.take(length)?)]));
        }
        let size = number_type.size();
        if self.order == 0 || self.field_size != size * self.order {
            return Err(data.malformed(format!(
                "attribute {} has {} values of {} bytes in a field of {}",
                self.name, self.order, size, self.field_size
            )));
        }
        let mut numbers = Vec::with_capacity(count.min(data.remaining()));
        for _ in 0..count {
            numbers.push(number_type.number(data.take(size)?));
        }
        Ok(Values::Numbers(number_type.data_type(), numbers))
    }
}

/// A name or class: its length, then its bytes.
fn counted_text(cursor: &mut Cursor) -> Read<String> {
    let length = usize::from(cursor.u16()?);
    Ok(String::from_utf8_lossy(cursor.take(length)?).into_owned())
}

/// An HDF4 number type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumberType {
    Text,
    Float(Order),
    Double(Order),
    Integer {
        size: usize,
        signed: bool,
        order: Order,
    },
}

impl NumberType {
    fn of(code: u16) -> Read<Self> {
        if code & NATIVE != 0 {
            return Err(unported(format!(
                "values of the native number type {code:#x}"
            )));
        }
        let order = if code & LITTLE_ENDIAN != 0 {
            Order::Little
        } else {
            Order::Big
        };
        let integer = |size, signed| Self::Integer {
            size,
            signed,
            order,
        };
        Ok(match code & 0x0FFF {
            3 | 4 => Self::Text,
            5 => Self::Float(order),
            6 => Self::Double(order),
            20 => integer(1, true),
            21 => integer(1, false),
            22 => integer(2, true),
            23 => integer(2, false),
            24 => integer(4, true),
            25 => integer(4, false),
            26 => integer(8, true),
            27 => integer(8, false),
            other => return Err(malformed(format!("an unknown HDF4 number type {other}"))),
        })
    }

    fn size(self) -> usize {
        match self {
            Self::Text => 1,
            Self::Float(_) => 4,
            Self::Double(_) => 8,
            Self::Integer { size, .. } => size,
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Self::Text => DataType::Char,
            Self::Float(_) => DataType::Float,
            Self::Double(_) => DataType::Double,
            Self::Integer { size, signed, .. } => match (size, signed) {
                (1, true) => DataType::Byte,
                (1, false) => DataType::UByte,
                (2, true) => DataType::Short,
                (2, false) => DataType::UShort,
                (4, true) => DataType::Int,
                (4, false) => DataType::UInt,
                (_, true) => DataType::Long,
                (_, false) => DataType::ULong,
            },
        }
    }

    fn number(self, bytes: &[u8]) -> Number {
        match self {
            Self::Text => Number::Unsigned(uint(bytes, Order::Big)),
            Self::Float(order) => Number::Float(f32::from_bits(uint(bytes, order) as u32)),
            Self::Double(order) => Number::Double(f64::from_bits(uint(bytes, order))),
            Self::Integer {
                size,
                signed: true,
                order,
            } => {
                let unused = 64 - 8 * size as u32;
                Number::Signed(((uint(bytes, order) << unused) as i64) >> unused)
            }
            Self::Integer { order, .. } => Number::Unsigned(uint(bytes, order)),
        }
    }
}

/// Reads the scientific data sets of the vgroup of class `CDF0.0`, if the file has one.
pub(super) fn read(bytes: Bytes) -> Read<Contents> {
    let descriptors = Descriptors::read(bytes)?;
    let mut contents = Contents::default();
    for reference in descriptors.vgroups() {
        let group = Vgroup::read(&descriptors, reference)?;
        if group.class != FILE_CLASS {
            continue;
        }
        contents.attributes = attributes(&descriptors, &group)?;
        let mut dimensions = Vec::new();
        for member in group.holds(VGROUP) {
            let child = Vgroup::read(&descriptors, member)?;
            if DIMENSION_CLASSES.contains(&child.class.as_str()) {
                dimensions.push(Dimension {
                    length: dimension_length(&descriptors, &child)?,
                    name: child.name,
                });
            } else if child.class == VARIABLE_CLASS {
                contents.variables.push(variable(&descriptors, child)?);
            }
        }
        contents.dimensions = dimensions;
        return Ok(contents);
    }
    Ok(contents)
}

/// The attributes a vgroup holds: its vdatas of class `Attr0.0`.
fn attributes(descriptors: &Descriptors, group: &Vgroup) -> Read<Vec<Attribute>> {
    let mut attributes = Vec::new();
    for member in group.holds(VDATA_HEADER) {
        let vdata = Vdata::read(descriptors, member)?;
        if vdata.class == ATTRIBUTE_CLASS {
            attributes.push(Attribute {
                values: vdata.values(descriptors, member)?,
                name: vdata.name,
            });
        }
    }
    Ok(attributes)
}

/// A dimension's length, as its vdata of class `DimVal0.0` counts it in records, or one of
/// class `DimVal0.1` holds it as its value.
fn dimension_length(descriptors: &Descriptors, dimension: &Vgroup) -> Read<u64> {
    for member in dimension.holds(VDATA_HEADER) {
        let vdata = Vdata::read(descriptors, member)?;
        if vdata.class == DIMENSION_VALUES_CLASS {
            return Ok(vdata.records);
        }
        if vdata.class == DIMENSION_LENGTH_CLASS {
            if let Values::Numbers(_, numbers) = vdata.values(descriptors, member)? {
                if let Some(&Number::Signed(length)) = numbers.first() {
                    return u64::try_from(length).map_err(|_| {
                        malformed(format!("dimension {} of length {length}", dimension.name))
                    });
                }
            }
            return Err(malformed(format!(
                "dimension {} holds no integer length",
                dimension.name
            )));
        }
    }
    Err(malformed(format!(
        "dimension {} holds no vdata of its length",
        dimension.name
    )))
}

/// A variable: a vgroup of class `Var0.0`, its dimensions in order, its attributes, and the
/// shape and number type of its SDD.
fn variable(descriptors: &Descriptors, group: Vgroup) -> Read<Variable> {
    let shape = group
        .holds(DIMENSIONS)
        .next()
        .ok_or_else(|| malformed(format!("variable {} holds no dimension record", group.name)))?;
    let mut record = descriptors.element((DIMENSIONS, shape), "an HDF4 dimension record")?;
    let rank = usize::from(record.u16()?);
    let mut lengths = Vec::with_capacity(rank);
    for _ in 0..rank {
        lengths.push(u64::from(record.u32()?));
    }
    let type_tag = record.u16()?;
    let type_reference = record.u16()?;
    if type_tag != NUMBER_TYPE {
        return Err(record.malformed(format!("a number type of tag {type_tag}")));
    }
    let mut number_type =
        descriptors.element((NUMBER_TYPE, type_reference), "an HDF4 number type")?;
    number_type.u8()?;
    let code = u16::from(number_type.u8()?);
    let data_type = NumberType::of(code)?.data_type();
    let mut names = Vec::new();
    for member in group.holds(VGROUP) {
        let dimension = Vgroup::read(descriptors, member)?;
        if DIMENSION_CLASSES.contains(&dimension.class.as_str()) {
            names.push(dimension.name);
        }
    }
    if names.len() != lengths.len() {
        return Err(malformed(format!(
            "variable {} has {} dimensions and a shape of {}",
            group.name,
            names.len(),
            lengths.len()
        )));
    }
    let axes = names
        .into_iter()
        .zip(lengths)
        .map(|(name, length)| Axis::Shared(Dimension { name, length }))
        .collect();
    Ok(Variable {
        data_type,
        attributes: attributes(descriptors, &group)?,
        name: group.name,
        axes,
    })
}
