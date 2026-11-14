//! The structures of Parquet's Thrift definitions (`parquet.thrift`) that reading a file's values
//! takes: the file's metadata, its schema's elements, its row groups' column chunks and the
//! headers of their pages. Fields not needed are skipped; a required field missing fails.

use super::codec::Compression;
use super::thrift::{Field, Reader, Type};
use super::Problem;

/// `FileMetaData`.
#[derive(Debug)]
pub struct FileMetaData {
    pub schema: Vec<SchemaElement>,
    pub num_rows: i64,
    /// Where in the footer each `RowGroup` starts, read as its rows are written.
    pub row_groups: Vec<usize>,
    pub key_value: Vec<(String, Option<String>)>,
    pub created_by: Option<String>,
    /// Whether the footer names the algorithm its encrypted columns are encrypted with.
    pub encrypted_columns: bool,
}

fn missing(what: &str, field: &str) -> Problem {
    Problem::Malformed(format!("the {what} has no {field}"))
}

impl FileMetaData {
    /// The metadata of the footer `footer`.
    pub fn read(footer: &[u8]) -> Result<Self, Problem> {
        let mut reader = Reader::new(footer, 0);
        let mut schema = None;
        let mut num_rows = None;
        let mut row_groups = None;
        let mut key_value = Vec::new();
        let mut created_by = None;
        let mut encrypted_columns = false;
        reader.read_struct(|reader, field| {
            match field.id {
                2 => {
                    let mut elements = Vec::new();
                    reader.list(field, |reader, kind| {
                        elements.push(SchemaElement::read(reader, kind)?);
                        Ok(())
                    })?;
                    schema = Some(elements);
                }
                3 => num_rows = Some(reader.i64(field)?),
                4 => {
                    let mut starts = Vec::new();
                    reader.list(field, |reader, kind| {
                        expect_struct(kind, "row group")?;
                        starts.push(reader.position());
                        reader.skip_value(kind)
                    })?;
                    row_groups = Some(starts);
                }
                5 => reader.list(field, |reader, kind| {
                    expect_struct(kind, "key-value pair")?;
                    key_value.push(key_value_pair(reader)?);
                    Ok(())
                })?,
                6 => created_by = Some(reader.string(field)?),
                8 => {
                    encrypted_columns = true;
                    reader.skip(field)?;
                }
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        Ok(Self {
            schema: schema.ok_or_else(|| missing("file metadata", "schema"))?,
            num_rows: num_rows.ok_or_else(|| missing("file metadata", "row count"))?,
            row_groups: row_groups.ok_or_else(|| missing("file metadata", "row groups"))?,
            key_value,
            created_by,
            encrypted_columns,
        })
    }
}

fn expect_struct(kind: Type, what: &str) -> Result<(), Problem> {
    if kind != Type::Struct {
        return Err(Problem::Malformed(format!(
            "a {what} is a Thrift {kind:?}, not a struct"
        )));
    }
    Ok(())
}

/// A `KeyValue`: its key, and its value if it has one.
fn key_value_pair(reader: &mut Reader<'_>) -> Result<(String, Option<String>), Problem> {
    let mut key = None;
    let mut value = None;
    reader.read_struct(|reader, field| {
        match field.id {
            1 => key = Some(reader.string(field)?),
            2 => value = Some(reader.string(field)?),
            _ => reader.skip(field)?,
        }
        Ok(())
    })?;
    Ok((key.ok_or_else(|| missing("key-value pair", "key"))?, value))
}

/// `TimeUnit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Millis,
    Micros,
    Nanos,
}

impl Unit {
    /// The nanoseconds of one of the unit.
    pub fn nanos(self) -> i64 {
        match self {
            Self::Millis => 1_000_000,
            Self::Micros => 1_000,
            Self::Nanos => 1,
        }
    }

    fn read(reader: &mut Reader<'_>, field: Field) -> Result<Self, Problem> {
        let mut unit = None;
        reader.structure(field, |reader, field| {
            unit = match field.id {
                1 => Some(Self::Millis),
                2 => Some(Self::Micros),
                3 => Some(Self::Nanos),
                id => {
                    return Err(Problem::Malformed(format!(
                        "the time unit {id}, which Parquet does not define"
                    )))
                }
            };
            reader.skip(field)
        })?;
        unit.ok_or_else(|| missing("time unit", "unit"))
    }
}

/// `LogicalType`, a union of the annotations of a schema element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logical {
    String,
    Map,
    List,
    Enum,
    Decimal {
        scale: i32,
    },
    Date,
    Time {
        unit: Unit,
    },
    Timestamp {
        utc: bool,
        unit: Unit,
    },
    Integer {
        bits: i8,
        signed: bool,
    },
    Unknown,
    Json,
    Bson,
    Uuid,
    Float16,
    Variant,
    Geometry,
    Geography,
    /// An annotation of an id Parquet defined after this reader, which is read by the physical
    /// type and the converted type.
    Unrecognized,
}

impl Logical {
    fn read(reader: &mut Reader<'_>, field: Field) -> Result<Self, Problem> {
        let mut logical = None;
        reader.structure(field, |reader, field| {
            let read = match field.id {
                5 => Self::decimal(reader, field)?,
                7 | 8 => Self::time(reader, field)?,
                10 => Self::integer(reader, field)?,
                id => {
                    reader.skip(field)?;
                    match id {
                        1 => Self::String,
                        2 => Self::Map,
                        3 => Self::List,
                        4 => Self::Enum,
                        6 => Self::Date,
                        11 => Self::Unknown,
                        12 => Self::Json,
                        13 => Self::Bson,
                        14 => Self::Uuid,
                        15 => Self::Float16,
                        16 => Self::Variant,
                        17 => Self::Geometry,
                        18 => Self::Geography,
                        _ => Self::Unrecognized,
                    }
                }
            };
            logical = Some(read);
            Ok(())
        })?;
        logical.ok_or_else(|| missing("logical type", "annotation"))
    }

    fn decimal(reader: &mut Reader<'_>, field: Field) -> Result<Self, Problem> {
        let mut scale = None;
        reader.structure(field, |reader, field| {
            match field.id {
                1 => scale = Some(reader.i32(field)?),
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        Ok(Self::Decimal {
            scale: scale.ok_or_else(|| missing("decimal type", "scale"))?,
        })
    }

    /// A `TimeType` (field 7) or a `TimestampType` (field 8).
    fn time(reader: &mut Reader<'_>, field: Field) -> Result<Self, Problem> {
        let mut utc = None;
        let mut unit = None;
        reader.structure(field, |reader, field| {
            match field.id {
                1 => utc = Some(reader.bool(field)?),
                2 => unit = Some(Unit::read(reader, field)?),
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        let what = if field.id == 7 {
            "time type"
        } else {
            "timestamp type"
        };
        let utc = utc.ok_or_else(|| missing(what, "adjustment to UTC"))?;
        let unit = unit.ok_or_else(|| missing(what, "unit"))?;
        Ok(if field.id == 7 {
            Self::Time { unit }
        } else {
            Self::Timestamp { utc, unit }
        })
    }

    fn integer(reader: &mut Reader<'_>, field: Field) -> Result<Self, Problem> {
        let mut bits = None;
        let mut signed = None;
        reader.structure(field, |reader, field| {
            match field.id {
                1 => bits = Some(reader.i8(field)?),
                2 => signed = Some(reader.bool(field)?),
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        Ok(Self::Integer {
            bits: bits.ok_or_else(|| missing("integer type", "bit width"))?,
            signed: signed.ok_or_else(|| missing("integer type", "signedness"))?,
        })
    }
}

/// `SchemaElement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaElement {
    pub physical: Option<i32>,
    pub type_length: Option<i32>,
    pub repetition: Option<i32>,
    pub name: String,
    pub num_children: Option<i32>,
    pub converted: Option<i32>,
    pub scale: Option<i32>,
    pub logical: Option<Logical>,
}

impl SchemaElement {
    fn read(reader: &mut Reader<'_>, kind: Type) -> Result<Self, Problem> {
        expect_struct(kind, "schema element")?;
        let mut physical = None;
        let mut type_length = None;
        let mut repetition = None;
        let mut name = None;
        let mut num_children = None;
        let mut converted = None;
        let mut scale = None;
        let mut logical = None;
        reader.read_struct(|reader, field| {
            match field.id {
                1 => physical = Some(reader.i32(field)?),
                2 => type_length = Some(reader.i32(field)?),
                3 => repetition = Some(reader.i32(field)?),
                4 => name = Some(reader.string(field)?),
                5 => num_children = Some(reader.i32(field)?),
                6 => converted = Some(reader.i32(field)?),
                7 => scale = Some(reader.i32(field)?),
                10 => logical = Some(Logical::read(reader, field)?),
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        Ok(Self {
            physical,
            type_length,
            repetition,
            name: name.ok_or_else(|| missing("schema element", "name"))?,
            num_children,
            converted,
            scale,
            logical,
        })
    }
}

/// A `RowGroup`: its rows and the chunks of its columns.
#[derive(Debug)]
pub struct RowGroup {
    pub num_rows: i64,
    pub columns: Vec<ColumnChunk>,
}

impl RowGroup {
    /// The row group at `at` in `footer`, which [`FileMetaData::read`] found there.
    pub fn read(footer: &[u8], at: usize) -> Result<Self, Problem> {
        let mut reader = Reader::new(footer, at);
        let mut num_rows = None;
        let mut columns = None;
        reader.read_struct(|reader, field| {
            match field.id {
                1 => {
                    let mut chunks = Vec::new();
                    reader.list(field, |reader, kind| {
                        expect_struct(kind, "column chunk")?;
                        chunks.push(ColumnChunk::read(reader)?);
                        Ok(())
                    })?;
                    columns = Some(chunks);
                }
                3 => num_rows = Some(reader.i64(field)?),
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        Ok(Self {
            num_rows: num_rows.ok_or_else(|| missing("row group", "row count"))?,
            columns: columns.ok_or_else(|| missing("row group", "columns"))?,
        })
    }
}

/// A `ColumnChunk`, as far as reading its pages takes it.
#[derive(Debug)]
pub struct ColumnChunk {
    /// Whether the chunk is in another file, which its `file_path` names.
    pub elsewhere: bool,
    /// Whether its metadata or its pages are encrypted.
    pub encrypted: bool,
    pub meta: Option<ColumnMetaData>,
}

impl ColumnChunk {
    fn read(reader: &mut Reader<'_>) -> Result<Self, Problem> {
        let mut chunk = Self {
            elsewhere: false,
            encrypted: false,
            meta: None,
        };
        reader.read_struct(|reader, field| {
            match field.id {
                1 => {
                    chunk.elsewhere = true;
                    reader.skip(field)?;
                }
                3 => chunk.meta = Some(ColumnMetaData::read(reader, field)?),
                8 | 9 => {
                    chunk.encrypted = true;
                    reader.skip(field)?;
                }
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        Ok(chunk)
    }
}

/// `ColumnMetaData`, as far as reading the chunk's pages takes it.
#[derive(Debug)]
pub struct ColumnMetaData {
    pub physical: i32,
    pub compression: Compression,
    pub total_compressed_size: i64,
    pub data_page_offset: i64,
    pub dictionary_page_offset: Option<i64>,
}

impl ColumnMetaData {
    fn read(reader: &mut Reader<'_>, field: Field) -> Result<Self, Problem> {
        let mut physical = None;
        let mut compression = None;
        let mut total_compressed_size = None;
        let mut data_page_offset = None;
        let mut dictionary_page_offset = None;
        reader.structure(field, |reader, field| {
            match field.id {
                1 => physical = Some(reader.i32(field)?),
                4 => compression = Some(Compression::of(reader.i32(field)?)?),
                7 => total_compressed_size = Some(reader.i64(field)?),
                9 => data_page_offset = Some(reader.i64(field)?),
                11 => dictionary_page_offset = Some(reader.i64(field)?),
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        let what = "column metadata";
        Ok(Self {
            physical: physical.ok_or_else(|| missing(what, "type"))?,
            compression: compression.ok_or_else(|| missing(what, "codec"))?,
            total_compressed_size: total_compressed_size
                .ok_or_else(|| missing(what, "compressed size"))?,
            data_page_offset: data_page_offset.ok_or_else(|| missing(what, "data page offset"))?,
            dictionary_page_offset,
        })
    }
}

/// `Encoding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Plain,
    PlainDictionary,
    Rle,
    BitPacked,
    DeltaBinaryPacked,
    DeltaLengthByteArray,
    DeltaByteArray,
    RleDictionary,
    ByteStreamSplit,
}

impl Encoding {
    fn of(code: i32) -> Result<Self, Problem> {
        Ok(match code {
            0 => Self::Plain,
            2 => Self::PlainDictionary,
            3 => Self::Rle,
            4 => Self::BitPacked,
            5 => Self::DeltaBinaryPacked,
            6 => Self::DeltaLengthByteArray,
            7 => Self::DeltaByteArray,
            8 => Self::RleDictionary,
            9 => Self::ByteStreamSplit,
            _ => {
                return Err(Problem::Malformed(format!(
                    "the encoding {code}, which Parquet does not define"
                )))
            }
        })
    }

    fn read(reader: &mut Reader<'_>, field: Field) -> Result<Self, Problem> {
        Self::of(reader.i32(field)?)
    }
}

/// What a page holds, by its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// A data page of version 1: its levels and values compressed together, the levels headed
    /// by their lengths.
    Data {
        num_values: u32,
        encoding: Encoding,
        definition: Encoding,
        repetition: Encoding,
    },
    /// A data page of version 2: its levels, of the lengths given, uncompressed before its
    /// values, which are compressed if `compressed`.
    DataV2 {
        num_values: u32,
        encoding: Encoding,
        definition_length: u32,
        repetition_length: u32,
        compressed: bool,
    },
    Dictionary {
        num_values: u32,
        encoding: Encoding,
    },
    /// An index page, or a page of a type Parquet defined after this reader, passed over.
    Other,
}

/// A `PageHeader`: what the page holds and its sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
    pub kind: PageKind,
    pub uncompressed: usize,
    pub compressed: usize,
}

/// A count of a header, which is not negative.
fn count(value: i32, what: &str) -> Result<u32, Problem> {
    u32::try_from(value)
        .map_err(|_| Problem::Malformed(format!("the page header's {what} is {value}")))
}

impl PageHeader {
    /// The header at the start of `input`, and how many bytes it takes.
    pub fn read(input: &[u8]) -> Result<(Self, usize), Problem> {
        let mut reader = Reader::new(input, 0);
        let mut kind = None;
        let mut uncompressed = None;
        let mut compressed = None;
        let mut data = None;
        let mut data_v2 = None;
        let mut dictionary = None;
        reader.read_struct(|reader, field| {
            match field.id {
                1 => kind = Some(reader.i32(field)?),
                2 => uncompressed = Some(count(reader.i32(field)?, "uncompressed size")?),
                3 => compressed = Some(count(reader.i32(field)?, "compressed size")?),
                5 => data = Some(data_header(reader, field)?),
                7 => dictionary = Some(dictionary_header(reader, field)?),
                8 => data_v2 = Some(data_header_v2(reader, field)?),
                _ => reader.skip(field)?,
            }
            Ok(())
        })?;
        let what = "page header";
        let kind = match kind.ok_or_else(|| missing(what, "type"))? {
            0 => data.ok_or_else(|| missing(what, "data page header"))?,
            2 => dictionary.ok_or_else(|| missing(what, "dictionary page header"))?,
            3 => data_v2.ok_or_else(|| missing(what, "data page header of version 2"))?,
            _ => PageKind::Other,
        };
        let header = Self {
            kind,
            uncompressed: uncompressed.ok_or_else(|| missing(what, "uncompressed size"))? as usize,
            compressed: compressed.ok_or_else(|| missing(what, "compressed size"))? as usize,
        };
        Ok((header, reader.position()))
    }
}

fn data_header(reader: &mut Reader<'_>, field: Field) -> Result<PageKind, Problem> {
    let mut num_values = None;
    let mut encoding = None;
    let mut definition = None;
    let mut repetition = None;
    reader.structure(field, |reader, field| {
        match field.id {
            1 => num_values = Some(count(reader.i32(field)?, "value count")?),
            2 => encoding = Some(Encoding::read(reader, field)?),
            3 => definition = Some(Encoding::read(reader, field)?),
            4 => repetition = Some(Encoding::read(reader, field)?),
            _ => reader.skip(field)?,
        }
        Ok(())
    })?;
    let what = "data page header";
    Ok(PageKind::Data {
        num_values: num_values.ok_or_else(|| missing(what, "value count"))?,
        encoding: encoding.ok_or_else(|| missing(what, "encoding"))?,
        definition: definition.ok_or_else(|| missing(what, "definition level encoding"))?,
        repetition: repetition.ok_or_else(|| missing(what, "repetition level encoding"))?,
    })
}

fn data_header_v2(reader: &mut Reader<'_>, field: Field) -> Result<PageKind, Problem> {
    let mut num_values = None;
    let mut encoding = None;
    let mut definition_length = None;
    let mut repetition_length = None;
    let mut compressed = true;
    reader.structure(field, |reader, field| {
        match field.id {
            1 => num_values = Some(count(reader.i32(field)?, "value count")?),
            4 => encoding = Some(Encoding::read(reader, field)?),
            5 => {
                definition_length = Some(count(reader.i32(field)?, "definition levels' length")?);
            }
            6 => {
                repetition_length = Some(count(reader.i32(field)?, "repetition levels' length")?);
            }
            7 => compressed = reader.bool(field)?,
            _ => reader.skip(field)?,
        }
        Ok(())
    })?;
    let what = "data page header of version 2";
    Ok(PageKind::DataV2 {
        num_values: num_values.ok_or_else(|| missing(what, "value count"))?,
        encoding: encoding.ok_or_else(|| missing(what, "encoding"))?,
        definition_length: definition_length
            .ok_or_else(|| missing(what, "definition levels' length"))?,
        repetition_length: repetition_length
            .ok_or_else(|| missing(what, "repetition levels' length"))?,
        compressed,
    })
}

fn dictionary_header(reader: &mut Reader<'_>, field: Field) -> Result<PageKind, Problem> {
    let mut num_values = None;
    let mut encoding = None;
    reader.structure(field, |reader, field| {
        match field.id {
            1 => num_values = Some(count(reader.i32(field)?, "value count")?),
            2 => encoding = Some(Encoding::read(reader, field)?),
            _ => reader.skip(field)?,
        }
        Ok(())
    })?;
    let what = "dictionary page header";
    Ok(PageKind::Dictionary {
        num_values: num_values.ok_or_else(|| missing(what, "value count"))?,
        encoding: encoding.ok_or_else(|| missing(what, "encoding"))?,
    })
}
