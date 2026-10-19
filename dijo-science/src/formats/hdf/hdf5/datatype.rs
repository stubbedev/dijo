//! HDF5's datatype and dataspace messages, and the values of an attribute as they decode: what
//! the CDM makes of each type, and the numbers and texts of the types CDL writes.

use crate::formats::bytes::{text_to_nul, uint, unported, Cursor, Order, Read};
use crate::formats::cdm::{Axis, DataType, Number, Values};

/// A datatype, as far as the CDM names it and its values are decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Datatype {
    Fixed {
        size: usize,
        order: Order,
        signed: bool,
    },
    Float {
        size: usize,
        order: Order,
    },
    /// A string of `size` bytes, padded as `padding` says.
    Text {
        size: usize,
        padding: Padding,
    },
    /// A string of variable length, held in the global heap.
    VariableText,
    /// A sequence of variable length of the `base` type, held in the global heap.
    Sequence {
        base: Box<Datatype>,
    },
    /// An array of `dimensions` of the `base` type.
    Array {
        dimensions: Vec<u64>,
        base: Box<Datatype>,
    },
    /// An enumeration, whose values are those of its integer `base` type.
    Enumeration {
        base: Box<Datatype>,
    },
    /// A type CDL writes no literals of.
    Unwritten(DataType),
}

/// How a string of fixed size is padded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Padding {
    /// Ended by a NUL.
    NulTerminated,
    /// Padded with NULs.
    NulPadded,
    /// Padded with spaces.
    SpacePadded,
}

impl Datatype {
    /// Reads a datatype message at the cursor.
    pub(super) fn read(cursor: &mut Cursor) -> Read<Self> {
        let class_and_version = cursor.u8()?;
        let bits = cursor.take(3)?;
        let size = cursor.size(4, "a datatype's size")?;
        let class = class_and_version & 0x0F;
        let version = class_and_version >> 4;
        let order = if bits[0] & 1 == 0 {
            Order::Little
        } else {
            Order::Big
        };
        Ok(match class {
            0 => Self::Fixed {
                size,
                order,
                signed: bits[0] & 0x08 != 0,
            },
            1 => {
                if bits[0] & 0x40 != 0 {
                    return Err(unported("a floating-point type of VAX byte order"));
                }
                Self::Float { size, order }
            }
            2 | 4 | 5 | 7 => Self::Unwritten(DataType::Opaque),
            3 => Self::Text {
                size,
                padding: Padding::of(bits[0])?,
            },
            6 => Self::Unwritten(DataType::Structure),
            8 => Self::Enumeration {
                base: Box::new(Self::read(cursor)?),
            },
            9 => {
                let base = Self::read(cursor)?;
                if bits[0] & 0x0F == 1 {
                    Self::VariableText
                } else {
                    Self::Sequence {
                        base: Box::new(base),
                    }
                }
            }
            10 => {
                let rank = usize::from(cursor.u8()?);
                if version < 3 {
                    cursor.skip(3)?;
                }
                let mut dimensions = Vec::with_capacity(rank);
                for _ in 0..rank {
                    dimensions.push(u64::from(cursor.u32()?));
                }
                if version < 3 {
                    cursor.skip(4 * rank)?;
                }
                Self::Array {
                    dimensions,
                    base: Box::new(Self::read(cursor)?),
                }
            }
            _ => return Err(cursor.malformed(format!("a datatype of unknown class {class}"))),
        })
    }

    /// The CDM data type of a variable of the type, with the dimensions the type adds after the
    /// variable's own: a string's characters, an array's dimensions, a sequence's variable
    /// length.
    pub(super) fn cdm(&self) -> (DataType, Vec<Axis>) {
        match self {
            Self::Fixed { size, signed, .. } => (integer(*size, *signed), Vec::new()),
            Self::Float { size: 4, .. } => (DataType::Float, Vec::new()),
            Self::Float { .. } => (DataType::Double, Vec::new()),
            Self::Text { size: 1, .. } => (DataType::Char, Vec::new()),
            Self::Text { size, .. } => (DataType::Char, vec![Axis::Anonymous(*size as u64)]),
            Self::VariableText => (DataType::String, Vec::new()),
            Self::Sequence { base } => {
                let (data_type, mut axes) = base.cdm();
                axes.insert(0, Axis::VariableLength);
                (data_type, axes)
            }
            Self::Array { dimensions, base } => {
                let (data_type, inner) = base.cdm();
                let mut axes: Vec<Axis> = dimensions.iter().copied().map(Axis::Anonymous).collect();
                axes.extend(inner);
                (data_type, axes)
            }
            Self::Enumeration { base } => (
                match **base {
                    Self::Fixed { size: 1, .. } => DataType::Enum1,
                    Self::Fixed { size: 2, .. } => DataType::Enum2,
                    _ => DataType::Enum4,
                },
                Vec::new(),
            ),
            Self::Unwritten(data_type) => (*data_type, Vec::new()),
        }
    }
}

impl Padding {
    fn of(bits: u8) -> Read<Self> {
        match bits & 0x0F {
            0 => Ok(Self::NulTerminated),
            1 => Ok(Self::NulPadded),
            2 => Ok(Self::SpacePadded),
            other => Err(unported(format!("a string padding of type {other}"))),
        }
    }

    /// The text of a string of fixed size, as the CDM reads it: up to its first NUL, and
    /// without the spaces that pad it.
    pub(super) fn text(self, bytes: &[u8]) -> String {
        let text = text_to_nul(bytes);
        match self {
            Self::SpacePadded => text.trim_end_matches(' ').to_owned(),
            Self::NulTerminated | Self::NulPadded => text,
        }
    }
}

/// The CDM type of an integer of `size` bytes.
fn integer(size: usize, signed: bool) -> DataType {
    match (size, signed) {
        (1, true) => DataType::Byte,
        (1, false) => DataType::UByte,
        (2, true) => DataType::Short,
        (2, false) => DataType::UShort,
        (4, true) => DataType::Int,
        (4, false) => DataType::UInt,
        (_, true) => DataType::Long,
        (_, false) => DataType::ULong,
    }
}

/// The shape of a dataspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Dataspace {
    Scalar,
    Null,
    Simple(Vec<u64>),
}

impl Dataspace {
    /// Reads a dataspace message at the cursor, its dimensions of `lengths` bytes.
    pub(super) fn read(cursor: &mut Cursor, lengths: usize) -> Read<Self> {
        let version = cursor.u8()?;
        let rank = usize::from(cursor.u8()?);
        cursor.u8()?;
        let kind = match version {
            1 => {
                cursor.skip(5)?;
                if rank == 0 {
                    0
                } else {
                    1
                }
            }
            2 => cursor.u8()?,
            _ => return Err(cursor.malformed(format!("a dataspace of unknown version {version}"))),
        };
        match kind {
            0 => Ok(Self::Scalar),
            2 => Ok(Self::Null),
            1 => {
                let mut dimensions = Vec::with_capacity(rank);
                for _ in 0..rank {
                    dimensions.push(cursor.uint(lengths)?);
                }
                Ok(Self::Simple(dimensions))
            }
            _ => Err(cursor.malformed(format!("a dataspace of unknown type {kind}"))),
        }
    }

    /// The elements it holds, where they can be counted.
    pub(super) fn elements(&self) -> Option<u64> {
        match self {
            Self::Scalar => Some(1),
            Self::Null => Some(0),
            Self::Simple(dimensions) => dimensions
                .iter()
                .try_fold(1u64, |count, &length| count.checked_mul(length)),
        }
    }

    pub(super) fn axes(&self) -> Vec<Axis> {
        match self {
            Self::Scalar | Self::Null => Vec::new(),
            Self::Simple(dimensions) => dimensions.iter().copied().map(Axis::Anonymous).collect(),
        }
    }
}

/// Where a value of variable length lies in the global heap: its length, and the object
/// `index` of the collection at `collection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HeapReference {
    pub(super) length: u64,
    pub(super) collection: u64,
    pub(super) index: u32,
}

/// Decodes the `count` values of `datatype` from `data`, a string of variable length read from
/// the global heap by `heap`.
pub(super) fn values(
    datatype: &Datatype,
    data: &mut Cursor,
    count: usize,
    offsets: usize,
    heap: &dyn Fn(HeapReference) -> Read<Vec<u8>>,
) -> Read<Values> {
    match datatype {
        Datatype::Fixed {
            size,
            order,
            signed,
        } => {
            let mut numbers = Vec::with_capacity(count.min(data.remaining()));
            for _ in 0..count {
                let value = uint(data.take(*size)?, *order);
                numbers.push(if *signed {
                    Number::Signed(sign_extend(value, *size))
                } else {
                    Number::Unsigned(value)
                });
            }
            Ok(Values::Numbers(integer(*size, *signed), numbers))
        }
        Datatype::Float { size, order } => {
            let mut numbers = Vec::with_capacity(count.min(data.remaining()));
            for _ in 0..count {
                let value = uint(data.take(*size)?, *order);
                numbers.push(match size {
                    4 => Number::Float(f32::from_bits(value as u32)),
                    8 => Number::Double(f64::from_bits(value)),
                    _ => return Err(unported(format!("a floating-point value of {size} bytes"))),
                });
            }
            let data_type = if *size == 4 {
                DataType::Float
            } else {
                DataType::Double
            };
            Ok(Values::Numbers(data_type, numbers))
        }
        Datatype::Text { size, padding } => {
            let mut texts = Vec::with_capacity(count.min(data.remaining()));
            for _ in 0..count {
                texts.push(padding.text(data.take(*size)?));
            }
            Ok(Values::Text(texts))
        }
        Datatype::VariableText => {
            let mut texts = Vec::with_capacity(count.min(data.remaining()));
            for _ in 0..count {
                let reference = HeapReference {
                    length: u64::from(data.u32()?),
                    collection: data.uint(offsets)?,
                    index: data.u32()?,
                };
                texts.push(text_to_nul(&heap(reference)?));
            }
            Ok(Values::Text(texts))
        }
        Datatype::Array { dimensions, base } => {
            let per_element = dimensions
                .iter()
                .try_fold(1usize, |product, &length| {
                    product.checked_mul(usize::try_from(length).ok()?)
                })
                .and_then(|product| product.checked_mul(count))
                .ok_or_else(|| {
                    data.malformed("an array attribute of more values than a file holds")
                })?;
            values(base, data, per_element, offsets, heap)
        }
        Datatype::Enumeration { base } => match values(base, data, count, offsets, heap)? {
            Values::Numbers(_, numbers) => {
                let (data_type, _) = datatype.cdm();
                Ok(Values::Numbers(data_type, numbers))
            }
            other => Ok(other),
        },
        Datatype::Sequence { .. } => Ok(Values::Unwritten(DataType::Sequence)),
        Datatype::Unwritten(data_type) => Ok(Values::Unwritten(*data_type)),
    }
}

/// `value`, the `size` bytes of a signed integer, sign-extended.
fn sign_extend(value: u64, size: usize) -> i64 {
    let unused = 64 - 8 * size.min(8) as u32;
    ((value << unused) as i64) >> unused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_datatypes_as_the_cdm_names_them() {
        let int16 = [0x10, 0x08, 0, 0, 2, 0, 0, 0, 0, 0, 16, 0];
        let datatype = Datatype::read(&mut Cursor::new(&int16, 0, Order::Little, "test")).unwrap();
        assert_eq!(datatype.cdm(), (DataType::Short, Vec::new()));
        let text = [0x13, 0x02, 0, 0, 4, 0, 0, 0];
        let datatype = Datatype::read(&mut Cursor::new(&text, 0, Order::Little, "test")).unwrap();
        assert_eq!(datatype.cdm(), (DataType::Char, vec![Axis::Anonymous(4)]));
        let mut data = Cursor::new(b"ab  cd\0\0", 0, Order::Little, "test");
        let read = values(&datatype, &mut data, 2, 8, &|_| unreachable!()).unwrap();
        assert_eq!(read, Values::Text(vec!["ab".to_owned(), "cd".to_owned()]));
        assert_eq!(sign_extend(0xFFFE, 2), -2);
    }
}
