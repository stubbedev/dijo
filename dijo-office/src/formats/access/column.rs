//! Jackcess's `ColumnImpl` and its subclasses as they read: a column's definition in a table
//! definition, and the value its bytes in a row hold, which for a memo or OLE column lie in a
//! long value on other pages.

use super::buffer::{
    Buffer, Thrown, ARRAY_INDEX_OUT_OF_BOUNDS, BUFFER_UNDERFLOW, NEGATIVE_ARRAY_SIZE,
};
use super::format::{Charset, Format};
use super::pages::Pages;
use super::table::{row_end, row_start};

const FIXED_LEN_FLAG_MASK: u8 = 0x01;
const AUTO_NUMBER_FLAG_MASK: u8 = 0x04;
const AUTO_NUMBER_GUID_FLAG_MASK: u8 = 0x40;
const CALCULATED_EXT_FLAG_MASK: u8 = 0xC0;
const COMPLEX_FK_EXT_FLAG_MASK: u8 = 0x08;
const TEXT_COMPRESSION_HEADER: [u8; 2] = [0xFF, 0xFE];
const LONG_VALUE_TYPE_MASK: u32 = 0xC000_0000;
const LONG_VALUE_TYPE_THIS_PAGE: u8 = 0x80;
const LONG_VALUE_TYPE_OTHER_PAGE: u8 = 0x40;
const LONG_VALUE_TYPE_OTHER_PAGES: u8 = 0x00;
const CALC_DATA_LEN_OFFSET: usize = 16;
const CALC_DATA_OFFSET: usize = CALC_DATA_LEN_OFFSET + 4;
const GENERAL_SORT_ORDER_VALUE: i16 = 1033;

/// A text column's collating sort order, as `ColumnImpl.readSortOrder` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SortOrder {
    value: i16,
    version: i16,
}

impl SortOrder {
    pub(super) const GENERAL_97: SortOrder = SortOrder::general(-1);
    pub(super) const GENERAL_LEGACY: SortOrder = SortOrder::general(0);
    pub(super) const GENERAL: SortOrder = SortOrder::general(1);

    const fn general(version: i16) -> Self {
        SortOrder {
            value: GENERAL_SORT_ORDER_VALUE,
            version,
        }
    }

    /// Whether an index over text in this order is one Jackcess can look entries up in.
    pub(super) fn is_supported_by_indexes(self) -> bool {
        [Self::GENERAL, Self::GENERAL_LEGACY, Self::GENERAL_97].contains(&self)
    }

    /// `readSortOrder` at `position`.
    fn read(buffer: &Buffer<'_>, position: i64, format: &Format) -> Result<Self, Thrown> {
        let value = buffer.i16_at(position)?;
        if value == 0 {
            return Ok(format.default_sort_order);
        }
        let version = if format.size_sort_order == 4 {
            i16::from(buffer.u8_at(position + 3)? as i8)
        } else {
            format.default_sort_order.version
        };
        Ok(SortOrder { value, version })
    }
}

/// Jackcess's `DataType`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DataType {
    Boolean,
    Byte,
    Int,
    Long,
    Money,
    Float,
    Double,
    ShortDateTime,
    Binary,
    Text,
    Ole,
    Memo,
    Guid,
    Numeric,
    BigBinary,
    ComplexType,
    BigInt,
    ExtDateTime,
    UnsupportedFixedLength,
    UnsupportedVariableLength,
}

impl DataType {
    /// `DataType.fromByte`: the supported type of a type byte, or `None` where it throws.
    pub(super) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x01 => Self::Boolean,
            0x02 => Self::Byte,
            0x03 => Self::Int,
            0x04 => Self::Long,
            0x05 => Self::Money,
            0x06 => Self::Float,
            0x07 => Self::Double,
            0x08 => Self::ShortDateTime,
            0x09 => Self::Binary,
            0x0A => Self::Text,
            0x0B => Self::Ole,
            0x0C => Self::Memo,
            0x0F => Self::Guid,
            0x10 => Self::Numeric,
            0x11 => Self::BigBinary,
            0x12 => Self::ComplexType,
            0x13 => Self::BigInt,
            0x14 => Self::ExtDateTime,
            _ => return None,
        })
    }

    /// `DataType.fromByte` where it throws, as an exception.
    pub(super) fn from_byte_or_throw(byte: u8) -> Result<Self, Thrown> {
        Self::from_byte(byte)
            .ok_or_else(|| Thrown::io(format!("Unrecognized data type: {}", byte as i8)))
    }

    fn fixed_size(self) -> Option<i16> {
        Some(match self {
            Self::Boolean | Self::Byte => 1,
            Self::Int => 2,
            Self::Long | Self::Float | Self::ComplexType => 4,
            Self::Money | Self::Double | Self::ShortDateTime | Self::BigInt => 8,
            Self::Guid => 16,
            Self::Numeric => 17,
            Self::BigBinary => 3992,
            Self::ExtDateTime => 42,
            Self::Binary
            | Self::Text
            | Self::Ole
            | Self::Memo
            | Self::UnsupportedFixedLength
            | Self::UnsupportedVariableLength => return None,
        })
    }

    /// `DataType.isVariableLength`.
    fn is_variable_length(self) -> bool {
        matches!(
            self,
            Self::Binary
                | Self::Text
                | Self::Ole
                | Self::Memo
                | Self::Numeric
                | Self::UnsupportedVariableLength
        )
    }
}

/// How a column's subclass reads its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `ColumnImpl`, `TextColumnImpl` and `NumericColumnImpl`: by the type alone.
    Plain,
    /// `LongValueColumnImpl` and `MemoColumnImpl`: a long value.
    LongValue,
    /// `UnsupportedColumnImpl`: the bytes as they are.
    Unsupported,
    /// `CalculatedColumnUtil`'s columns: the value wrapped in a calculation header.
    Calculated,
}

/// A column of a table definition.
#[derive(Debug, Clone)]
pub(super) struct Column {
    pub(super) name: String,
    pub(super) data_type: DataType,
    pub(super) number: i16,
    pub(super) variable_length: bool,
    pub(super) var_len_index: i16,
    pub(super) fixed_offset: i16,
    length: i16,
    /// `getScale`: a numeric column's own; no other type reads it.
    scale: i8,
    /// A text or memo column's collating sort order.
    pub(super) sort_order: Option<SortOrder>,
    pub(super) auto_number: bool,
    /// Whether the column keys a complex column's values in a flat table.
    pub(super) complex_foreign_key: bool,
    /// What a complex column's supporting tables make it.
    pub(super) complex: Option<Complex>,
    kind: Kind,
}

/// What a complex column's supporting tables make it, as far as reading it goes: the kinds of
/// `ComplexColumnInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Complex {
    /// A multi-valued field, whose properties are also those of the value column of its flat
    /// table, and so read with the flat table's.
    MultiValue { flat_table_page: i32 },
    /// A memo's version history, which names the memo by its value column, if it has one.
    VersionHistory { value_column: Option<String> },
    /// Attachments, or a kind Jackcess does not support.
    Other,
}

/// What the column definition at `offset` holds before its type is known: `ColumnImpl.InitArgs`.
pub(super) struct Definition {
    pub(super) type_byte: u8,
    flags: u8,
    ext_flags: u8,
    pub(super) calculated: bool,
}

impl Definition {
    pub(super) fn read(buffer: &Buffer<'_>, offset: i64, format: &Format) -> Result<Self, Thrown> {
        let type_byte = buffer.u8_at(offset + format.offset_column_type as i64)?;
        let flags = buffer.u8_at(offset + format.offset_column_flags as i64)?;
        let ext_flags = match format.offset_column_ext_flags {
            Some(ext) => buffer.u8_at(offset + ext as i64)?,
            None => 0,
        };
        Ok(Self {
            type_byte,
            flags,
            ext_flags,
            calculated: ext_flags & CALCULATED_EXT_FLAG_MASK != 0,
        })
    }
}

impl Column {
    /// `ColumnImpl.create` of the definition at `offset`, whose type is `type_byte` or, for a
    /// calculated column, the result type its properties name.
    pub(super) fn create(
        buffer: &Buffer<'_>,
        offset: i64,
        format: &Format,
        name: String,
        definition: &Definition,
        type_byte: u8,
    ) -> Result<Self, Thrown> {
        let variable_length = definition.flags & FIXED_LEN_FLAG_MASK == 0;
        let (data_type, kind) = match DataType::from_byte(type_byte) {
            None if variable_length => (DataType::UnsupportedVariableLength, Kind::Unsupported),
            None => (DataType::UnsupportedFixedLength, Kind::Unsupported),
            Some(data_type) if definition.calculated => (data_type, Kind::Calculated),
            Some(data_type @ (DataType::Memo | DataType::Ole)) => (data_type, Kind::LongValue),
            Some(data_type) => (data_type, Kind::Plain),
        };
        let at = |field: usize| offset + field as i64;
        let number = buffer.i16_at(at(format.offset_column_number))?;
        let length = buffer.i16_at(at(format.offset_column_length))?;
        let var_len_index = buffer.i16_at(at(format.offset_column_variable_table_index))?;
        let fixed_offset = buffer.i16_at(at(format.offset_column_fixed_data_offset))?;
        let scale = if data_type == DataType::Numeric {
            buffer.u8_at(at(format.offset_column_scale))? as i8
        } else {
            0
        };
        let sort_order =
            match data_type {
                DataType::Text | DataType::Memo if kind != Kind::Unsupported => Some(
                    SortOrder::read(buffer, at(format.offset_column_sort_order), format)?,
                ),
                _ => None,
            };
        Ok(Self {
            name,
            data_type,
            number,
            variable_length,
            var_len_index,
            fixed_offset,
            length,
            scale,
            sort_order,
            auto_number: definition.flags & (AUTO_NUMBER_FLAG_MASK | AUTO_NUMBER_GUID_FLAG_MASK)
                != 0,
            complex_foreign_key: definition.ext_flags & COMPLEX_FK_EXT_FLAG_MASK != 0,
            complex: None,
            kind,
        })
    }

    pub(super) fn is_calculated(&self) -> bool {
        self.kind == Kind::Calculated
    }

    /// `storeInNullMask`: a boolean, not calculated, is its bit in the null mask.
    pub(super) fn stores_in_null_mask(&self) -> bool {
        self.data_type == DataType::Boolean && self.kind != Kind::Calculated
    }

    /// `getFixedDataSize`.
    pub(super) fn fixed_data_size(&self) -> i16 {
        match self.data_type.fixed_size() {
            Some(size) => size.max(self.length),
            None => self.length,
        }
    }

    /// `read(byte[])`: the value of the column's bytes in a row.
    pub(super) fn read(&self, data: &[u8], pages: &Pages<'_>) -> Result<Option<Value>, Thrown> {
        let charset = pages.charset;
        match self.kind {
            Kind::Unsupported => Ok(Some(Value::Binary(data.to_vec()))),
            Kind::LongValue if data.is_empty() => Ok(None),
            Kind::LongValue => {
                let bytes = read_long_value(data, pages)?;
                Ok(Some(match self.data_type {
                    DataType::Memo => Value::Text(decode_text(&bytes, charset)),
                    _ => Value::Binary(bytes),
                }))
            }
            Kind::Plain => read_plain(self.data_type, data, self.scale, charset).map(Some),
            Kind::Calculated => self.read_calculated(data, pages),
        }
    }

    /// `CalculatedColumnUtil`'s columns' `read`.
    fn read_calculated(&self, data: &[u8], pages: &Pages<'_>) -> Result<Option<Value>, Thrown> {
        let charset = pages.charset;
        match self.data_type {
            DataType::Boolean => {
                let data = unwrap_calculated(data)?;
                Ok(Some(Value::Boolean(data.first().is_some_and(|&b| b != 0))))
            }
            DataType::Text => Ok(Some(Value::Text(decode_text(
                unwrap_calculated(data)?,
                charset,
            )))),
            DataType::Memo if data.is_empty() => Ok(None),
            DataType::Memo => {
                let bytes = read_long_value(data, pages)?;
                Ok(Some(Value::Text(decode_text(
                    unwrap_calculated(&bytes)?,
                    charset,
                ))))
            }
            DataType::Numeric => {
                let data = unwrap_calculated(data)?;
                if data.is_empty() {
                    return Ok(None);
                }
                read_calculated_numeric(data).map(Some)
            }
            data_type => {
                let data = unwrap_calculated(data)?;
                if data.is_empty() && !data_type.is_variable_length() {
                    return Ok(None);
                }
                read_plain(data_type, data, self.scale, charset).map(Some)
            }
        }
    }
}

/// A value as Jackcess reads it, by the Java class it would be.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    Boolean(bool),
    Byte(i8),
    Int(i16),
    Long(i32),
    BigInt(i64),
    /// A currency amount in ten-thousandths.
    Money(i64),
    Float(f32),
    Double(f64),
    /// A date as the bits of the double Access keeps it as.
    Date(i64),
    /// An extended date, which the extractor never writes, so never reads either. Jackcess
    /// parses its digits into a `long` that wraps, and fails the table on a date past
    /// `LocalDateTime`'s range.
    ExtDate,
    Numeric(Decimal),
    Guid(String),
    Text(String),
    Binary(Vec<u8>),
    /// A complex column's key into its supporting table.
    Complex(i32),
}

fn underflow() -> Thrown {
    Thrown::bare(BUFFER_UNDERFLOW)
}

fn bytes<const N: usize>(data: &[u8]) -> Result<[u8; N], Thrown> {
    data.get(..N)
        .map(|bytes| bytes.try_into().expect("N bytes"))
        .ok_or_else(underflow)
}

/// `ColumnImpl.read`: the value of `data` by its type alone.
fn read_plain(
    data_type: DataType,
    data: &[u8],
    scale: i8,
    charset: Charset,
) -> Result<Value, Thrown> {
    Ok(match data_type {
        DataType::Byte => Value::Byte(bytes::<1>(data)?[0] as i8),
        DataType::Int => Value::Int(i16::from_le_bytes(bytes(data)?)),
        DataType::Long => Value::Long(i32::from_le_bytes(bytes(data)?)),
        DataType::Double => Value::Double(f64::from_le_bytes(bytes(data)?)),
        DataType::Float => Value::Float(f32::from_le_bytes(bytes(data)?)),
        DataType::ShortDateTime => Value::Date(i64::from_le_bytes(bytes(data)?)),
        DataType::Binary | DataType::BigBinary => Value::Binary(data.to_vec()),
        DataType::Text => Value::Text(decode_text(data, charset)),
        DataType::Money => {
            if data.len() != 8 {
                return Err(Thrown::io("Invalid money value"));
            }
            Value::Money(i64::from_le_bytes(bytes(data)?))
        }
        DataType::Numeric => {
            let negate = *data.first().ok_or_else(underflow)? != 0;
            let mut magnitude: [u8; 16] = bytes(data.get(1..).unwrap_or_default())?;
            for word in magnitude.chunks_mut(4) {
                word.reverse();
            }
            Value::Numeric(numeric(&magnitude, negate, i64::from(scale)))
        }
        DataType::Guid => Value::Guid(guid(&bytes(data)?)),
        DataType::ExtDateTime => Value::ExtDate,
        DataType::ComplexType => Value::Complex(i32::from_le_bytes(bytes(data)?)),
        DataType::BigInt => Value::BigInt(i64::from_le_bytes(bytes(data)?)),
        DataType::Boolean => {
            return Err(Thrown::io(
                "Tried to read a boolean from data instead of null mask.",
            ))
        }
        DataType::Ole
        | DataType::Memo
        | DataType::UnsupportedFixedLength
        | DataType::UnsupportedVariableLength => {
            return Err(Thrown::io(format!("Unrecognized data type: {data_type:?}")))
        }
    })
}

/// What a property column (`PropertyMaps.PropColumn`) of `data_type` reads `data` as: by the
/// type alone, a numeric at the type's default scale of 0.
pub(super) fn read_property(
    data_type: DataType,
    data: &[u8],
    charset: Charset,
) -> Result<Value, Thrown> {
    read_plain(data_type, data, 0, charset)
}

/// A decimal number: a magnitude, negated where asked, over ten to the `scale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Decimal {
    pub(super) negative: bool,
    pub(super) magnitude: u128,
    pub(super) scale: i64,
}

impl Decimal {
    /// The number in positional notation, as many digits after the point as the scale says.
    /// Zero has no sign.
    pub(super) fn plain(self) -> String {
        let digits = self.magnitude.to_string();
        let sign = if self.negative && self.magnitude != 0 {
            "-"
        } else {
            ""
        };
        let Ok(scale) = usize::try_from(self.scale) else {
            if self.magnitude == 0 {
                return digits;
            }
            let zeros = "0".repeat(self.scale.unsigned_abs() as usize);
            return format!("{sign}{digits}{zeros}");
        };
        if scale == 0 {
            return format!("{sign}{digits}");
        }
        let digits = format!("{digits:0>width$}", width = scale + 1);
        let (integer, fraction) = digits.split_at(digits.len() - scale);
        format!("{sign}{integer}.{fraction}")
    }
}

/// `toBigDecimal`: a big-endian magnitude, negated where asked, over ten to the `scale`.
fn numeric(magnitude: &[u8], negate: bool, scale: i64) -> Decimal {
    let mut value: u128 = 0;
    for &byte in magnitude {
        value = value << 8 | u128::from(byte);
    }
    Decimal {
        negative: negate,
        magnitude: value,
        scale,
    }
}

/// `readGUIDValue`: the bytes as a GUID in braces, its first three groups little-endian.
fn guid(data: &[u8; 16]) -> String {
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02X}")).collect() };
    let reversed = |bytes: &[u8]| -> Vec<u8> { bytes.iter().rev().copied().collect() };
    format!(
        "{{{}-{}-{}-{}-{}}}",
        hex(&reversed(&data[0..4])),
        hex(&reversed(&data[4..6])),
        hex(&reversed(&data[6..8])),
        hex(&data[8..10]),
        hex(&data[10..16])
    )
}

/// `decodeTextValue`: text in the charset, or, in Unicode, compressed after the header `FF FE`,
/// its runs between zero bytes alternately one byte per character and in UTF-16LE. Jet 3, which
/// has no compression, keeps `FF FE` as the characters they are in its code page.
pub(super) fn decode_text(data: &[u8], charset: Charset) -> String {
    if charset != Charset::Unicode || !data.starts_with(&TEXT_COMPRESSION_HEADER) {
        return charset.decode(data);
    }
    let mut text = String::with_capacity(data.len());
    let mut compressed = true;
    let mut segment = |bytes: &[u8], compressed: bool| {
        if bytes.is_empty() {
            return;
        }
        if compressed {
            let expanded: Vec<u8> = bytes.iter().flat_map(|&byte| [byte, 0]).collect();
            text.push_str(&charset.decode(&expanded));
        } else {
            text.push_str(&charset.decode(bytes));
        }
    };
    for run in data[2..].split(|&byte| byte == 0) {
        segment(run, compressed);
        compressed = !compressed;
    }
    text
}

/// `LongValueColumnImpl.readLongValue`: the bytes of a long value from its definition.
fn read_long_value(definition: &[u8], pages: &Pages<'_>) -> Result<Vec<u8>, Thrown> {
    let format = pages.format;
    let mut def = Buffer::new(definition);
    let with_flags = def.i32()? as u32;
    let length = (with_flags & !LONG_VALUE_TYPE_MASK) as usize;
    let value_type = ((with_flags & LONG_VALUE_TYPE_MASK) >> 24) as u8;
    if value_type == LONG_VALUE_TYPE_THIS_PAGE {
        def.i32()?;
        def.i32()?;
        let available = def.remaining();
        return Ok(def.take(length.min(available) as i64)?.to_vec());
    }
    if definition.len() != format.size_long_value_def {
        return Err(Thrown::io(format!(
            "Expected {} bytes in long value definition, but found {}",
            format.size_long_value_def,
            definition.len()
        )));
    }
    let mut row = i32::from(def.u8()?);
    let mut page_number = def.u24_at(def.position() as i64)?;
    match value_type {
        LONG_VALUE_TYPE_OTHER_PAGE => {
            let page = pages.page(page_number)?;
            let mut buffer = Buffer::new(&page);
            let start = row_start(&buffer, row, format)?;
            let end = row_end(&buffer, row, format)?;
            let row_length = i64::from(end) - i64::from(start);
            let length = if row_length < length as i64 {
                row_length
            } else {
                length as i64
            };
            buffer.set_position(i64::from(start))?;
            Ok(buffer.take(length)?.to_vec())
        }
        LONG_VALUE_TYPE_OTHER_PAGES => {
            if length > pages.file_len() {
                return Err(Thrown::unported(format!(
                    "a long value of {length} bytes, more than the file holds, which Jackcess \
                     allocates before reading it"
                )));
            }
            let mut value = Vec::with_capacity(length);
            let mut remaining = length as i64;
            while remaining > 0 {
                let page = pages.page(page_number)?;
                let mut buffer = Buffer::new(&page);
                let start = row_start(&buffer, row, format)?;
                let mut end = row_end(&buffer, row, format)?;
                buffer.set_position(i64::from(start))?;
                row = i32::from(buffer.u8()?);
                page_number = buffer.u24()?;
                let mut chunk = i64::from(end) - i64::from(start) - 4;
                if chunk <= 0 {
                    return Err(Thrown::unported(
                        "a long value chunk that adds nothing, over which Jackcess may loop for \
                         ever",
                    ));
                }
                if chunk > remaining {
                    end = (i64::from(end) - (chunk - remaining)) as i16;
                    chunk = remaining;
                }
                remaining -= chunk;
                buffer.set_limit(i64::from(end))?;
                let rest = &buffer.bytes()[buffer.position()..buffer.limit()];
                if value.len() + rest.len() > length {
                    return Err(Thrown::bare("java.nio.BufferOverflowException"));
                }
                value.extend_from_slice(rest);
            }
            Ok(value)
        }
        other => Err(Thrown::io(format!(
            "Unrecognized long value type: {}",
            other as i8
        ))),
    }
}

/// `unwrapCalculatedValue`: the value after a calculation header, its length capped by what is
/// there; a negative length throws as `new byte[length]` does.
fn unwrap_calculated(data: &[u8]) -> Result<&[u8], Thrown> {
    if data.len() < CALC_DATA_OFFSET {
        return Ok(data);
    }
    let length = i32::from_le_bytes(
        data[CALC_DATA_LEN_OFFSET..CALC_DATA_OFFSET]
            .try_into()
            .expect("four bytes"),
    );
    let available = data.len() - CALC_DATA_OFFSET;
    let take = usize::try_from(length)
        .map_err(|_| Thrown::runtime(NEGATIVE_ARRAY_SIZE, length.to_string()))?
        .min(available);
    Ok(&data[CALC_DATA_OFFSET..CALC_DATA_OFFSET + take])
}

/// `readCalcNumericValue`: a calculated numeric's length, scale, sign and magnitude.
fn read_calculated_numeric(data: &[u8]) -> Result<Value, Thrown> {
    let mut buffer = Buffer::new(data);
    let total = buffer.i16()?;
    let length = if total > 0 {
        i64::from(total)
    } else {
        buffer.remaining() as i64
    } - 2;
    let length = (length / 4 * 4).min(16);
    let scale = buffer.u8()? as i8;
    let negate = buffer.u8()? != 0;
    let mut magnitude = buffer.take(length)?.to_vec();
    if magnitude.is_empty() {
        return Err(Thrown::index_out_of_bounds(
            ARRAY_INDEX_OUT_OF_BOUNDS,
            Some("Index 0 out of bounds for length 0".to_owned()),
        ));
    }
    let mut position = 0;
    if magnitude.len() % 8 != 0 {
        magnitude[0..4].reverse();
        position = 4;
    }
    while position < magnitude.len() {
        magnitude[position..position + 8].reverse();
        position += 8;
    }
    Ok(Value::Numeric(numeric(
        &magnitude,
        negate,
        i64::from(scale),
    )))
}
