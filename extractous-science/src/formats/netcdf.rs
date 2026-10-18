//! NetCDF classic files, as Tika's `NetCDFParser` reads them through netcdf-java: the file type
//! and the global attributes as metadata, then a list of the dimensions and one of the
//! variables, each with its attributes. Tika's path leaves them empty, as its native image lacks
//! the scientific module (#172). The header is read in place, the data never: the classic
//! format (CDF-1), the 64-bit offset format (CDF-2) and the 64-bit data format (CDF-5).
//!
//! Numeric global attributes are written whole, each value as Java writes it, where
//! `NetCDFParser` writes the integer part of the first value only.

use crate::config::ParserConfigs;
use crate::formats::bytes::{malformed, text_to_nul, Bytes, Cursor, Order, Read};
use crate::formats::cdm::{
    add_global_attribute, write_dimensions, write_variables, Attribute, Axis, DataType, Dimension,
    Number, Values, Variable,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Footprint, Parser, Prepared};

const MEDIA_TYPE: &str = "application/x-netcdf";
/// `NetCDFParser.FILE_TYPE_DESCRIPTION`.
const FILE_TYPE_DESCRIPTION: &str = "netcdf:file-type-description";
/// `NetCDFParser.NETCDF`, the prefix of the global attributes' names.
const NETCDF: &str = "netcdf:";
/// `N3iosp.getFileTypeDescription`.
const DESCRIPTION: &str = "NetCDF-3/CDM";

const MAGIC: &[u8; 3] = b"CDF";
const NC_DIMENSION: u32 = 0x0A;
const NC_VARIABLE: u32 = 0x0B;
const NC_ATTRIBUTE: u32 = 0x0C;

/// Tika's `NetCDFParser`.
pub struct NetCdfParser;

impl Parser for NetCdfParser {
    fn name(&self) -> &'static str {
        "extractous::NetCDFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.netcdf.NetCDFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let header = Header::read(Bytes::new(input))?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set(FILE_TYPE_DESCRIPTION, DESCRIPTION);
            for attribute in &header.attributes {
                add_global_attribute(metadata, NETCDF, attribute);
            }
            xhtml.newline()?;
            write_dimensions(xhtml, &header.dimensions)?;
            write_variables(xhtml, &header.variables)?;
            Ok(())
        }))
    }
}

/// The variant of the format, by the version byte after the magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    /// CDF-1: 32-bit counts and offsets.
    Classic,
    /// CDF-2: 32-bit counts, 64-bit offsets.
    Offset64,
    /// CDF-5: 64-bit counts and offsets.
    Data64,
}

impl Version {
    /// The bytes of a count: of elements, a dimension's length, a dimension's index and a
    /// variable's size.
    fn count_bytes(self) -> usize {
        match self {
            Self::Classic | Self::Offset64 => 4,
            Self::Data64 => 8,
        }
    }

    /// The bytes of a variable's offset.
    fn offset_bytes(self) -> usize {
        match self {
            Self::Classic => 4,
            Self::Offset64 | Self::Data64 => 8,
        }
    }

    /// The count of records a file being written leaves unknown.
    fn streaming(self) -> u64 {
        match self {
            Self::Classic | Self::Offset64 => u64::from(u32::MAX),
            Self::Data64 => u64::MAX,
        }
    }
}

/// An external type of the format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NcType {
    Byte,
    Char,
    Short,
    Int,
    Float,
    Double,
    UByte,
    UShort,
    UInt,
    Int64,
    UInt64,
}

impl NcType {
    fn of(code: u32, cursor: &Cursor) -> Read<Self> {
        Ok(match code {
            1 => Self::Byte,
            2 => Self::Char,
            3 => Self::Short,
            4 => Self::Int,
            5 => Self::Float,
            6 => Self::Double,
            7 => Self::UByte,
            8 => Self::UShort,
            9 => Self::UInt,
            10 => Self::Int64,
            11 => Self::UInt64,
            _ => return Err(cursor.malformed(format!("an unknown type {code}"))),
        })
    }

    fn size(self) -> u64 {
        match self {
            Self::Byte | Self::Char | Self::UByte => 1,
            Self::Short | Self::UShort => 2,
            Self::Int | Self::Float | Self::UInt => 4,
            Self::Double | Self::Int64 | Self::UInt64 => 8,
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Self::Byte => DataType::Byte,
            Self::Char => DataType::Char,
            Self::Short => DataType::Short,
            Self::Int => DataType::Int,
            Self::Float => DataType::Float,
            Self::Double => DataType::Double,
            Self::UByte => DataType::UByte,
            Self::UShort => DataType::UShort,
            Self::UInt => DataType::UInt,
            Self::Int64 => DataType::Long,
            Self::UInt64 => DataType::ULong,
        }
    }

    /// The number of `bytes` in big-endian order.
    fn number(self, bytes: &[u8]) -> Number {
        let value = crate::formats::bytes::uint(bytes, Order::Big);
        match self {
            Self::Byte => Number::Signed(i64::from(value as u8 as i8)),
            Self::Short => Number::Signed(i64::from(value as u16 as i16)),
            Self::Int => Number::Signed(i64::from(value as u32 as i32)),
            Self::Int64 => Number::Signed(value as i64),
            Self::Char | Self::UByte | Self::UShort | Self::UInt | Self::UInt64 => {
                Number::Unsigned(value)
            }
            Self::Float => Number::Float(f32::from_bits(value as u32)),
            Self::Double => Number::Double(f64::from_bits(value)),
        }
    }
}

/// A variable as the header declares it.
struct Declared {
    name: String,
    dimensions: Vec<usize>,
    attributes: Vec<Attribute>,
    nc_type: NcType,
    size: u64,
    begin: u64,
}

/// What the header of a file declares.
struct Header {
    dimensions: Vec<Dimension>,
    attributes: Vec<Attribute>,
    variables: Vec<Variable>,
}

impl Header {
    fn read(bytes: Bytes) -> Read<Self> {
        let mut cursor = bytes.cursor_from(0, Order::Big, "the NetCDF header")?;
        if cursor.take(3)? != MAGIC {
            return Err(cursor.malformed("no NetCDF magic"));
        }
        let version = match cursor.u8()? {
            1 => Version::Classic,
            2 => Version::Offset64,
            5 => Version::Data64,
            other => return Err(cursor.malformed(format!("an unknown version {other}"))),
        };
        let counts = version.count_bytes();
        let records = cursor.uint(counts)?;
        let dimensions = list(&mut cursor, version, NC_DIMENSION, |cursor| {
            Ok((name(cursor, version)?, cursor.uint(counts)?))
        })?;
        let attributes = attributes(&mut cursor, version)?;
        let declared = list(&mut cursor, version, NC_VARIABLE, |cursor| {
            declared(cursor, version, dimensions.len())
        })?;
        let records = if records == version.streaming() {
            streamed_records(bytes.len(), &dimensions, &declared)?
        } else {
            records
        };
        let dimensions: Vec<Dimension> = dimensions
            .into_iter()
            .map(|(name, length)| Dimension {
                name,
                length: if length == 0 { records } else { length },
            })
            .collect();
        let variables = declared
            .into_iter()
            .map(|variable| Variable {
                data_type: variable.nc_type.data_type(),
                name: variable.name,
                axes: variable
                    .dimensions
                    .iter()
                    .map(|&index| Axis::Shared(dimensions[index].clone()))
                    .collect(),
                attributes: variable.attributes,
            })
            .collect();
        Ok(Self {
            dimensions,
            attributes,
            variables,
        })
    }
}

/// A list of the header, `tag` and its elements, each as `element` reads it, or none where it is
/// absent: two zeros.
fn list<T>(
    cursor: &mut Cursor,
    version: Version,
    tag: u32,
    mut element: impl FnMut(&mut Cursor) -> Read<T>,
) -> Read<Vec<T>> {
    let found = cursor.u32()?;
    let count = cursor.size(version.count_bytes(), "a list's count")?;
    if found == 0 && count == 0 {
        return Ok(Vec::new());
    }
    if found != tag {
        return Err(cursor.malformed(format!("a list tagged {found} where {tag} belongs")));
    }
    let mut elements = Vec::with_capacity(count.min(cursor.remaining()));
    for _ in 0..count {
        elements.push(element(cursor)?);
    }
    Ok(elements)
}

/// A name: its length, then its UTF-8 bytes, padded to four.
fn name(cursor: &mut Cursor, version: Version) -> Read<String> {
    let length = cursor.size(version.count_bytes(), "a name's length")?;
    let name = String::from_utf8_lossy(cursor.take(length)?).into_owned();
    cursor.align(4)?;
    Ok(name)
}

fn attributes(cursor: &mut Cursor, version: Version) -> Read<Vec<Attribute>> {
    list(cursor, version, NC_ATTRIBUTE, |cursor| {
        let name = name(cursor, version)?;
        let nc_type = NcType::of(cursor.u32()?, cursor)?;
        let count = cursor.size(version.count_bytes(), "an attribute's count")?;
        let length = count
            .checked_mul(nc_type.size() as usize)
            .ok_or_else(|| cursor.malformed(format!("attribute {name} of {count} values")))?;
        let bytes = cursor.take(length)?;
        cursor.align(4)?;
        let values = match nc_type {
            NcType::Char => Values::Text(vec![text_to_nul(bytes)]),
            _ => Values::Numbers(
                nc_type.data_type(),
                bytes
                    .chunks_exact(nc_type.size() as usize)
                    .map(|value| nc_type.number(value))
                    .collect(),
            ),
        };
        Ok(Attribute { name, values })
    })
}

fn declared(cursor: &mut Cursor, version: Version, dimensions: usize) -> Read<Declared> {
    let counts = version.count_bytes();
    let name = name(cursor, version)?;
    let rank = cursor.size(counts, "a variable's rank")?;
    let mut indexes = Vec::with_capacity(rank.min(cursor.remaining()));
    for _ in 0..rank {
        let index = cursor.size(counts, "a dimension's index")?;
        if index >= dimensions {
            return Err(cursor.malformed(format!(
                "variable {name} has dimension {index} of {dimensions}"
            )));
        }
        indexes.push(index);
    }
    let attributes = attributes(cursor, version)?;
    let nc_type = NcType::of(cursor.u32()?, cursor)?;
    let size = cursor.uint(counts)?;
    let begin = cursor.uint(version.offset_bytes())?;
    Ok(Declared {
        name,
        dimensions: indexes,
        attributes,
        nc_type,
        size,
        begin,
    })
}

/// The records of a file whose header leaves their count unknown, as netcdf-java counts them:
/// the bytes after the first record variable starts, divided by the bytes of a record, which is
/// the padded sizes of the record variables, or the unpadded size of the only one.
fn streamed_records(
    file_length: u64,
    dimensions: &[(String, u64)],
    declared: &[Declared],
) -> Read<u64> {
    let record: Vec<&Declared> = declared
        .iter()
        .filter(|variable| {
            variable
                .dimensions
                .first()
                .is_some_and(|&index| dimensions[index].1 == 0)
        })
        .collect();
    let Some(start) = record.iter().map(|variable| variable.begin).min() else {
        return Ok(0);
    };
    let record_bytes = match record.as_slice() {
        [only] => only.dimensions[1..]
            .iter()
            .try_fold(only.nc_type.size(), |size, &index| {
                size.checked_mul(dimensions[index].1)
            }),
        _ => record
            .iter()
            .try_fold(0u64, |size, variable| size.checked_add(variable.size)),
    }
    .ok_or_else(|| malformed("the NetCDF header: a record of more bytes than a file holds"))?;
    if record_bytes == 0 {
        return Ok(0);
    }
    Ok(file_length.saturating_sub(start) / record_bytes)
}

#[cfg(test)]
mod tests;
