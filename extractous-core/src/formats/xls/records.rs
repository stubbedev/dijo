//! The BIFF8 records Tika's Excel listener reads, as POI's record classes read them from a
//! [`RecordStream`], and every other record POI knows read past whole, as its constructors do on
//! well-formed records.

use super::stream::{java_string, RecordStream, MAX_STRING_LENGTH};
use crate::formats::poi::{thrown, Read};

/// Record types, as POI's record classes name them.
pub(super) mod sid {
    pub(in crate::formats::xls) const FORMULA: u16 = 0x0006;
    pub(in crate::formats::xls) const EOF: u16 = 0x000A;
    pub(in crate::formats::xls) const PROTECT: u16 = 0x0012;
    pub(in crate::formats::xls) const HEADER: u16 = 0x0014;
    pub(in crate::formats::xls) const FOOTER: u16 = 0x0015;
    pub(in crate::formats::xls) const NOTE: u16 = 0x001C;
    pub(in crate::formats::xls) const FILE_PASS: u16 = 0x002F;
    pub(in crate::formats::xls) const CONTINUE: u16 = 0x003C;
    pub(in crate::formats::xls) const OBJ: u16 = 0x005D;
    pub(in crate::formats::xls) const COLUMN_INFO: u16 = 0x007D;
    pub(in crate::formats::xls) const BOUND_SHEET: u16 = 0x0085;
    pub(in crate::formats::xls) const WRITE_PROTECT: u16 = 0x0086;
    pub(in crate::formats::xls) const MUL_RK: u16 = 0x00BD;
    pub(in crate::formats::xls) const DB_CELL: u16 = 0x00D7;
    pub(in crate::formats::xls) const EXTENDED_FORMAT: u16 = 0x00E0;
    pub(in crate::formats::xls) const DRAWING_GROUP: u16 = 0x00EB;
    pub(in crate::formats::xls) const DRAWING: u16 = 0x00EC;
    pub(in crate::formats::xls) const SST: u16 = 0x00FC;
    pub(in crate::formats::xls) const TEXT_OBJECT: u16 = 0x01B6;
    pub(in crate::formats::xls) const HYPERLINK: u16 = 0x01B8;
    pub(in crate::formats::xls) const NUMBER: u16 = 0x0203;
    pub(in crate::formats::xls) const LABEL: u16 = 0x0204;
    pub(in crate::formats::xls) const STRING: u16 = 0x0207;
    pub(in crate::formats::xls) const ROW: u16 = 0x0208;
    pub(in crate::formats::xls) const RK: u16 = 0x027E;
    pub(in crate::formats::xls) const FORMAT: u16 = 0x041E;
    pub(in crate::formats::xls) const BOF: u16 = 0x0809;
    pub(in crate::formats::xls) const LABEL_SST: u16 = 0x00FD;
    pub(in crate::formats::xls) const SERIES_TEXT: u16 = 0x100D;
}

/// The record types POI's `HSSFRecordTypes` parses; any other is an `UnknownRecord`.
const PARSEABLE: [u16; 139] = [
    0x0006, 0x000A, 0x000C, 0x000D, 0x000E, 0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x0014, 0x0015,
    0x0017, 0x0018, 0x0019, 0x001A, 0x001B, 0x001C, 0x001D, 0x0022, 0x0023, 0x0026, 0x0027, 0x0028,
    0x0029, 0x002A, 0x002B, 0x002F, 0x0031, 0x003C, 0x003D, 0x0040, 0x0041, 0x0042, 0x0051, 0x0055,
    0x0059, 0x005A, 0x005B, 0x005C, 0x005D, 0x005E, 0x005F, 0x0063, 0x007D, 0x0080, 0x0081, 0x0082,
    0x0083, 0x0084, 0x0085, 0x0086, 0x008C, 0x008D, 0x0092, 0x009C, 0x009D, 0x00A1, 0x00B0, 0x00B1,
    0x00B6, 0x00BD, 0x00BE, 0x00C1, 0x00C5, 0x00D5, 0x00D7, 0x00DA, 0x00DD, 0x00E0, 0x00E1, 0x00E2,
    0x00E3, 0x00E5, 0x00EB, 0x00EC, 0x00ED, 0x00FC, 0x00FD, 0x00FF, 0x0100, 0x013D, 0x0160, 0x0161,
    0x01AA, 0x01AB, 0x01AE, 0x01AF, 0x01B0, 0x01B1, 0x01B2, 0x01B6, 0x01B7, 0x01B8, 0x01BC, 0x01BE,
    0x01C1, 0x0200, 0x0201, 0x0203, 0x0204, 0x0205, 0x0207, 0x0208, 0x020B, 0x0221, 0x0225, 0x0236,
    0x023E, 0x027E, 0x0293, 0x041E, 0x04BC, 0x0809, 0x0850, 0x0852, 0x0853, 0x0854, 0x0855, 0x0856,
    0x0867, 0x0868, 0x0879, 0x087A, 0x088E, 0x0894, 0x089C, 0x1002, 0x1003, 0x1006, 0x100D, 0x1015,
    0x101A, 0x101F, 0x1033, 0x1034, 0x1045, 0x1050, 0x1051,
];

/// The BOF types Tika's listener tells apart.
pub(super) mod bof {
    pub(in crate::formats::xls) const WORKBOOK: i16 = 0x05;
    pub(in crate::formats::xls) const WORKSHEET: i16 = 0x10;
    pub(in crate::formats::xls) const CHART: i16 = 0x20;
}

/// A cell's place and style, as `CellValueRecordInterface` gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CellRef {
    pub(super) row: i32,
    pub(super) column: i16,
    pub(super) xf: i16,
}

/// A string of the shared string table: its text, and the phonetic text of its extended
/// string data, if it has any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SharedString {
    pub(super) text: String,
    pub(super) phonetic: Option<String>,
}

/// A record as POI reads it, as far as Tika's listener and POI's record stream look at it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Record {
    Bof {
        kind: i16,
    },
    Eof,
    BoundSheet {
        name: String,
        hidden: bool,
        very_hidden: bool,
    },
    Sst(Vec<SharedString>),
    Formula {
        cell: CellRef,
        value: f64,
        string_result: bool,
    },
    String(String),
    Label {
        cell: CellRef,
        text: String,
    },
    LabelSst {
        cell: CellRef,
        index: i32,
    },
    Number {
        cell: CellRef,
        value: f64,
    },
    Rk {
        cell: CellRef,
        value: f64,
    },
    MulRk {
        row: i32,
        first_column: i16,
        last_column: i16,
        values: Vec<(i16, f64)>,
    },
    Hyperlink {
        first_row: i32,
        first_column: i32,
        address: Option<String>,
    },
    TextObject(String),
    SeriesText(String),
    Format {
        index: i32,
        format: String,
    },
    ExtendedFormat {
        format_index: i16,
    },
    DrawingGroup(Vec<u8>),
    Drawing(Vec<u8>),
    Continue(Vec<u8>),
    Protect(bool),
    ColumnInfo {
        hidden: bool,
    },
    Row {
        zero_height: bool,
    },
    Note {
        author: String,
    },
    Header(String),
    Footer(String),
    Obj,
    FilePass,
    WriteProtect,
    DbCell,
    /// A record of a type POI does not parse.
    Unknown,
    /// A record of another type POI parses, read past.
    Other,
}

/// `RecordFactory.createSingleRecord` for the current record of `stream`.
pub(super) fn read_record(stream: &mut RecordStream<'_>) -> Read<Record> {
    let sid = stream.sid();
    if !PARSEABLE.contains(&sid) {
        stream.read_remainder()?;
        return Ok(Record::Unknown);
    }
    Ok(match sid {
        sid::BOF => {
            stream.read_i16()?;
            let kind = stream.read_i16()?;
            for size in [2, 2, 4, 4] {
                if stream.remaining() >= size {
                    stream.read_fully(size as usize)?;
                }
            }
            Record::Bof { kind }
        }
        sid::EOF => Record::Eof,
        sid::BOUND_SHEET => {
            stream.read_fully(4)?;
            let flags = stream.read_u16()?;
            let length = i32::from(stream.read_u8()?);
            let multibyte = stream.read_u8()? & 1 != 0;
            let name = if multibyte {
                stream.read_unicode(length)?
            } else {
                stream.read_compressed(length)?
            };
            Record::BoundSheet {
                name: java_string(&name),
                hidden: flags & 1 != 0,
                very_hidden: flags & 2 != 0,
            }
        }
        sid::SST => Record::Sst(read_shared_strings(stream)?),
        sid::FORMULA => {
            let cell = read_cell(stream)?;
            let bits = stream.read_i64()?;
            stream.read_i16()?;
            let special = special_cached_value(bits)?;
            stream.read_i32()?;
            stream.read_i16()?;
            let available = stream.remaining();
            stream.read_fully(available.max(0) as usize)?;
            Record::Formula {
                cell,
                value: if special.is_none() {
                    f64::from_bits(bits as u64)
                } else {
                    0.0
                },
                string_result: special == Some(0),
            }
        }
        sid::STRING => {
            let length = i32::from(stream.read_u16()?);
            let wide = stream.read_u8()? != 0;
            let text = if wide {
                stream.read_unicode(length)?
            } else {
                stream.read_compressed(length)?
            };
            Record::String(java_string(&text))
        }
        sid::LABEL => {
            let row = i32::from(stream.read_u16()?);
            let column = stream.read_i16()?;
            let xf = stream.read_i16()?;
            let length = stream.read_i16()?;
            let flag = stream.read_u8()?;
            let text = if length > 0 {
                if flag & 1 != 0 {
                    stream.read_unicode(i32::from(length))?
                } else {
                    stream.read_compressed(i32::from(length))?
                }
            } else {
                Vec::new()
            };
            if stream.remaining() > 0 {
                stream.read_remainder()?;
            }
            Record::Label {
                cell: CellRef { row, column, xf },
                text: java_string(&text),
            }
        }
        sid::LABEL_SST => Record::LabelSst {
            cell: read_cell(stream)?,
            index: stream.read_i32()?,
        },
        sid::NUMBER => Record::Number {
            cell: read_cell(stream)?,
            value: stream.read_f64()?,
        },
        sid::RK => Record::Rk {
            cell: read_cell(stream)?,
            value: decode_rk(stream.read_i32()?),
        },
        sid::MUL_RK => {
            let row = i32::from(stream.read_u16()?);
            let first_column = stream.read_i16()?;
            let items = (stream.remaining() - 2) / 6;
            let mut values = Vec::new();
            for _ in 0..items.max(0) {
                let xf = stream.read_i16()?;
                values.push((xf, decode_rk(stream.read_i32()?)));
            }
            let last_column = stream.read_i16()?;
            Record::MulRk {
                row,
                first_column,
                last_column,
                values,
            }
        }
        sid::HYPERLINK => read_hyperlink(stream)?,
        sid::TEXT_OBJECT => Record::TextObject(read_text_object(stream)?),
        sid::SERIES_TEXT => {
            stream.read_u16()?;
            let length = i32::from(stream.read_u8()?);
            let wide = stream.read_u8()? & 1 != 0;
            let text = if wide {
                stream.read_unicode(length)?
            } else {
                stream.read_compressed(length)?
            };
            Record::SeriesText(java_string(&text))
        }
        sid::FORMAT => read_format(stream)?,
        sid::EXTENDED_FORMAT => {
            stream.read_i16()?;
            let format_index = stream.read_i16()?;
            for _ in 0..5 {
                stream.read_i16()?;
            }
            stream.read_i32()?;
            stream.read_i16()?;
            Record::ExtendedFormat { format_index }
        }
        sid::DRAWING_GROUP => Record::DrawingGroup(stream.read_remainder()?),
        sid::DRAWING => Record::Drawing(stream.read_remainder()?),
        sid::CONTINUE => Record::Continue(stream.read_remainder()?),
        sid::PROTECT => Record::Protect(stream.read_i16()? & 1 != 0),
        sid::COLUMN_INFO => {
            for _ in 0..4 {
                stream.read_u16()?;
            }
            let options = stream.read_u16()?;
            match stream.remaining() {
                2 => {
                    stream.read_u16()?;
                }
                1 => {
                    stream.read_u8()?;
                }
                0 => {}
                other => return thrown(format!("Unusual record size remaining=({other})")),
            }
            Record::ColumnInfo {
                hidden: options & 1 != 0,
            }
        }
        sid::ROW => {
            stream.read_u16()?;
            for _ in 0..5 {
                stream.read_i16()?;
            }
            let flags = stream.read_i16()?;
            stream.read_i16()?;
            Record::Row {
                zero_height: flags & 0x20 != 0,
            }
        }
        sid::NOTE => read_note(stream)?,
        sid::HEADER => Record::Header(read_header_footer(stream)?),
        sid::FOOTER => Record::Footer(read_header_footer(stream)?),
        sid::OBJ => {
            stream.read_remainder()?;
            Record::Obj
        }
        sid::FILE_PASS => {
            stream.read_remainder()?;
            Record::FilePass
        }
        sid::WRITE_PROTECT => {
            if stream.remaining() == 2 {
                stream.read_i16()?;
            }
            Record::WriteProtect
        }
        sid::DB_CELL => {
            stream.read_u16()?;
            for _ in 0..stream.remaining() / 2 {
                stream.read_i16()?;
            }
            Record::DbCell
        }
        _ => {
            stream.read_remainder()?;
            Record::Other
        }
    })
}

fn read_cell(stream: &mut RecordStream<'_>) -> Read<CellRef> {
    let row = i32::from(stream.read_u16()?);
    let column = stream.read_u16()? as i16;
    let xf = stream.read_u16()? as i16;
    Ok(CellRef { row, column, xf })
}

/// `FormulaSpecialCachedValue.create`: the type of a value that is no number, failing on an
/// unknown one.
fn special_cached_value(bits: i64) -> Read<Option<i8>> {
    const MARKER: u64 = 0xFFFF_0000_0000_0000;
    if bits as u64 & MARKER != MARKER {
        return Ok(None);
    }
    let kind = bits as i8;
    if !(0..=3).contains(&kind) {
        return thrown(format!("Bad special value code ({kind})"));
    }
    Ok(Some(kind))
}

/// `RKUtil.decodeNumber`.
pub(super) fn decode_rk(number: i32) -> f64 {
    let raw = i64::from(number) >> 2;
    let mut value = if number & 2 == 2 {
        raw as f64
    } else {
        f64::from_bits((raw << 34) as u64)
    };
    if number & 1 == 1 {
        value /= 100.0;
    }
    value
}

/// `SSTRecord`: the strings, as many as the record says are unique, stopping early where the
/// data runs out.
fn read_shared_strings(stream: &mut RecordStream<'_>) -> Read<Vec<SharedString>> {
    let total = stream.read_i32()?;
    let unique = stream.read_i32()?;
    let mut strings = Vec::new();
    if total == 0 {
        return Ok(strings);
    }
    for _ in 0..unique.max(0) {
        if stream.remaining() == 0
            && (!stream.has_next_record()? || stream.next_sid() != Some(sid::CONTINUE))
        {
            break;
        }
        strings.push(read_unicode_string(stream)?);
    }
    Ok(strings)
}

/// `UnicodeString(RecordInputStream)`.
fn read_unicode_string(stream: &mut RecordStream<'_>) -> Read<SharedString> {
    let count = i32::from(stream.read_u16()?);
    let flags = stream.read_u8()?;
    let rich = flags & 0x8 != 0;
    let extended = flags & 0x4 != 0;
    let runs = if rich { stream.read_i16()? } else { 0 };
    let extension = if extended { stream.read_i32()? } else { 0 };
    let text = if flags & 1 == 0 {
        stream.read_compressed(count)?
    } else {
        stream.read_unicode(count)?
    };
    if rich {
        for _ in 0..runs.max(0) {
            stream.read_i16()?;
            stream.read_i16()?;
        }
    }
    let phonetic = if extended && extension > 0 {
        read_phonetic(stream, extension)?
    } else {
        None
    };
    Ok(SharedString {
        text: java_string(&text),
        phonetic,
    })
}

/// `ExtRst`: the phonetic text, `None` where POI makes the extension empty.
fn read_phonetic(stream: &mut RecordStream<'_>, expected: i32) -> Read<Option<String>> {
    let reserved = stream.read_i16()?;
    if reserved == -1 {
        return Ok(Some(String::new()));
    }
    if reserved != 1 {
        for _ in 0..expected - 2 {
            stream.read_u8()?;
        }
        return Ok(Some(String::new()));
    }
    let size = stream.read_i16()?;
    stream.read_i16()?;
    stream.read_i16()?;
    read_split_u16(stream)?;
    let first = stream.read_i16()?;
    let mut second = stream.read_i16()?;
    if first == 0 && second > 0 {
        second = 0;
    }
    if first != second {
        return thrown(format!(
            "The two length fields of the Phonetic Text don't agree! {first} vs {second}"
        ));
    }
    let bytes = i64::from(first) * 2;
    if !(0..=MAX_STRING_LENGTH).contains(&bytes) {
        return thrown(format!("Tried to allocate an array of length {bytes}"));
    }
    let raw = stream.read_fully(bytes as usize)?;
    let units: Vec<u16> = utf16_le(&raw);
    let phonetic = java_string(&units);
    let run_data = i32::from(size) - 4 - 6 - 2 * units.len() as i32;
    let runs = run_data / 6;
    for _ in 0..runs.max(0) {
        for _ in 0..3 {
            read_split_u16(stream)?;
        }
    }
    let extra = (run_data - runs * 6).max(0);
    for _ in 0..extra {
        stream.read_u8()?;
    }
    Ok(Some(phonetic))
}

/// `ContinuableRecordInput.readUShort`: two bytes, each of which may start a `CONTINUE`.
fn read_split_u16(stream: &mut RecordStream<'_>) -> Read<u16> {
    let low = stream.read_u8()?;
    let high = stream.read_u8()?;
    Ok(u16::from_le_bytes([low, high]))
}

/// `HyperlinkRecord`'s reading, as far as `getAddress` needs it.
fn read_hyperlink(stream: &mut RecordStream<'_>) -> Read<Record> {
    const LINK_URL: i32 = 0x01;
    const LINK_PLACE: i32 = 0x08;
    const LINK_LABEL: i32 = 0x14;
    const LINK_TARGET_FRAME: i32 = 0x80;
    const LINK_UNC_PATH: i32 = 0x100;
    const URL_MONIKER: &str = "{79EAC9E0-BAF9-11CE-8C82-00AA004BA90B}";
    const FILE_MONIKER: &str = "{00000303-0000-0000-C000-000000000046}";
    const STD_MONIKER: &str = "{79EAC9D0-BAF9-11CE-8C82-00AA004BA90B}";
    const TAIL_SIZE: i32 = 24;
    if stream.remaining() < 8 {
        return thrown("Ran out of data reading CellRangeAddress");
    }
    let first_row = i32::from(stream.read_u16()?);
    stream.read_u16()?;
    let first_column = i32::from(stream.read_u16()?);
    stream.read_u16()?;
    stream.read_fully(16)?;
    let version = stream.read_i32()?;
    if version != 2 {
        return thrown(format!("Stream Version must be 0x2 but found {version}"));
    }
    let options = stream.read_i32()?;
    if options & LINK_LABEL != 0 {
        let length = stream.read_i32()?;
        stream.read_unicode(length)?;
    }
    if options & LINK_TARGET_FRAME != 0 {
        let length = stream.read_i32()?;
        stream.read_unicode(length)?;
    }
    let mut address = None;
    let mut short_file_name = None;
    let mut text_mark = None;
    let mut file_moniker = false;
    if options & LINK_URL != 0 && options & LINK_UNC_PATH != 0 {
        let length = stream.read_i32()?;
        address = Some(java_string(&stream.read_unicode(length)?));
    }
    if options & LINK_URL != 0 && options & LINK_UNC_PATH == 0 {
        let raw: [u8; 16] = stream.read_fully(16)?.try_into().expect("16 bytes");
        let moniker = crate::ole2::class_id_string(&raw);
        if moniker == URL_MONIKER {
            let length = stream.read_i32()?;
            let remaining = stream.remaining();
            if i64::from(length) == remaining {
                address = Some(java_string(&stream.read_unicode(length / 2)?));
            } else {
                address = Some(java_string(&stream.read_unicode((length - TAIL_SIZE) / 2)?));
                stream.read_fully(TAIL_SIZE as usize)?;
            }
        } else if moniker == FILE_MONIKER {
            file_moniker = true;
            stream.read_i16()?;
            let length = stream.read_i32()?;
            if !(0..=MAX_STRING_LENGTH).contains(&i64::from(length)) {
                return thrown(format!("Tried to allocate an array of length {length}"));
            }
            let bytes = stream.read_fully(length as usize)?;
            short_file_name = Some(
                bytes
                    .iter()
                    .map(|&byte| char::from(byte))
                    .collect::<String>(),
            );
            stream.read_fully(TAIL_SIZE as usize)?;
            let size = stream.read_i32()?;
            if size > 0 {
                let characters = stream.read_i32()? / 2;
                stream.read_u16()?;
                let bytes = i64::from(characters) * 2;
                if !(0..=MAX_STRING_LENGTH).contains(&bytes) {
                    return thrown(format!("Tried to allocate an array of length {bytes}"));
                }
                let raw = stream.read_fully(bytes as usize)?;
                let units: Vec<u16> = utf16_le(&raw);
                address = Some(java_string(&units));
            }
        } else if moniker == STD_MONIKER {
            stream.read_i16()?;
            let length = stream.read_i32()?;
            if !(0..=super::stream::MAX_RECORD_LENGTH).contains(&i64::from(length)) {
                return thrown(format!("Tried to allocate an array of length {length}"));
            }
            let bytes = stream.read_fully(length as usize)?;
            address = Some(utf8(&bytes));
        }
    }
    if options & LINK_PLACE != 0 {
        let length = stream.read_i32()?;
        text_mark = Some(java_string(&stream.read_unicode(length)?));
    }
    if stream.remaining() > 0 {
        stream.read_remainder()?;
    }
    let chosen = if options & LINK_URL != 0 && file_moniker {
        address.or(short_file_name)
    } else if options & LINK_PLACE != 0 {
        text_mark
    } else {
        address
    };
    Ok(Record::Hyperlink {
        first_row,
        first_column,
        address: chosen.map(|text| until_nul(&text).to_owned()),
    })
}

/// Little-endian UTF-16 units.
fn utf16_le(raw: &[u8]) -> Vec<u16> {
    raw.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect()
}

/// Java's UTF-8 decoding, malformed input replaced.
fn utf8(bytes: &[u8]) -> String {
    crate::text::charset::Charset::named("UTF-8")
        .decode(bytes)
        .expect("the JDK's UTF-8 decodes")
}

/// `HyperlinkRecord.cleanString`: the text up to its first NUL.
fn until_nul(text: &str) -> &str {
    text.find('\0').map_or(text, |at| &text[..at])
}

/// `TextObjectRecord`: the text, which the `CONTINUE` records after it hold, checked against
/// its formatting runs.
fn read_text_object(stream: &mut RecordStream<'_>) -> Read<String> {
    for _ in 0..5 {
        stream.read_u16()?;
    }
    let length = stream.read_u16()?;
    let formatting = stream.read_u16()?;
    stream.read_i32()?;
    if stream.remaining() > 0 {
        return thrown(
            "a text object linked to a cell by a formula, which this port does not read",
        );
    }
    let text = if length > 0 {
        let flag = stream.read_u8()?;
        if flag & 1 == 0 {
            stream.read_compressed(i32::from(length))?
        } else {
            stream.read_unicode(i32::from(length))?
        }
    } else {
        Vec::new()
    };
    if formatting > 0 {
        if formatting % 8 != 0 {
            return thrown(format!("Bad format run data length {formatting})"));
        }
        let text_length = text.len() as i32;
        for _ in 0..formatting / 8 {
            let index = i32::from(stream.read_i16()?);
            stream.read_i16()?;
            stream.read_i32()?;
            if index > text_length {
                return thrown("Start index must be less than end index.");
            }
            if index < 0 {
                return thrown("Start and end index not in range.");
            }
        }
    }
    Ok(java_string(&text))
}

/// `FormatRecord`: the format string, read as far as the record goes whatever length it gives.
fn read_format(stream: &mut RecordStream<'_>) -> Read<Record> {
    let index = i32::from(stream.read_i16()?);
    let length = i32::from(stream.read_u16()?);
    let compressed = stream.read_u8()? & 1 == 0;
    let available = if compressed {
        stream.remaining()
    } else {
        stream.remaining() / 2
    };
    let count = if i64::from(length) == available {
        length as usize
    } else {
        available.max(0) as usize
    };
    let mut units = Vec::with_capacity(count);
    for _ in 0..count {
        units.push(if compressed {
            u16::from(stream.read_u8()?)
        } else {
            stream.read_i16()? as u16
        });
    }
    if stream.remaining() == 1 {
        units.push(u16::from(stream.read_u8()?));
    }
    while stream.remaining() > 0 {
        stream.read_u8()?;
    }
    Ok(Record::Format {
        index,
        format: java_string(&units),
    })
}

/// `NoteRecord`: the author.
fn read_note(stream: &mut RecordStream<'_>) -> Read<Record> {
    stream.read_u16()?;
    stream.read_i16()?;
    stream.read_i16()?;
    stream.read_u16()?;
    let length = stream.read_i16()?;
    let wide = stream.read_u8()? != 0;
    let bytes = i64::from(length) * if wide { 2 } else { 1 };
    if !(0..=MAX_STRING_LENGTH).contains(&bytes) {
        return thrown(format!("Tried to allocate an array of length {bytes}"));
    }
    let raw = stream.read_fully(bytes as usize)?;
    let author = if wide {
        let units: Vec<u16> = utf16_le(&raw);
        java_string(&units)
    } else {
        raw.iter().map(|&byte| char::from(byte)).collect()
    };
    if stream.remaining() == 1 {
        stream.read_u8()?;
    } else if stream.remaining() == 2 && length == 0 {
        stream.read_u8()?;
        stream.read_u8()?;
    }
    Ok(Record::Note { author })
}

/// `HeaderFooterBase`: the text.
fn read_header_footer(stream: &mut RecordStream<'_>) -> Read<String> {
    if stream.remaining() == 0 {
        return Ok(String::new());
    }
    let length = stream.read_i16()?;
    if length == 0 && stream.remaining() == 0 {
        return Ok(String::new());
    }
    let wide = stream.read_u8()? != 0;
    let text = if wide {
        stream.read_unicode(i32::from(length))?
    } else {
        stream.read_compressed(i32::from(length))?
    };
    Ok(java_string(&text))
}

#[cfg(test)]
mod tests {
    use super::super::stream::tests::record;
    use super::*;

    fn read(sid: u16, data: &[u8]) -> Read<Record> {
        let bytes = record(sid, data);
        let mut stream = RecordStream::new(&bytes);
        stream.has_next_record()?;
        stream.next_record()?;
        let record = read_record(&mut stream)?;
        stream.has_next_record()?;
        Ok(record)
    }

    #[test]
    fn decodes_rk_numbers_as_poi_does() {
        assert_eq!(decode_rk(0x3FF0_0000), 1.0);
        assert_eq!(decode_rk((1234 << 2) | 2), 1234.0);
        assert_eq!(decode_rk((1234 << 2) | 3), 12.34);
    }

    #[test]
    fn reads_cells_as_poi_does() {
        let mut label = vec![1, 0, 2, 0, 15, 0, 2, 0, 0];
        label.extend(b"hi");
        assert_eq!(
            read(sid::LABEL, &label).unwrap(),
            Record::Label {
                cell: CellRef {
                    row: 1,
                    column: 2,
                    xf: 15
                },
                text: "hi".to_owned()
            }
        );
        let mut formula = vec![0, 0, 0, 0, 0, 0];
        formula.extend([0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
        formula.extend([0; 8]);
        formula.extend([1, 0, 0x1E]);
        let Record::Formula { string_result, .. } = read(sid::FORMULA, &formula).unwrap() else {
            panic!("a formula");
        };
        assert!(string_result);
    }

    #[test]
    fn reads_shared_strings_with_their_phonetic_text() {
        let mut sst = vec![1, 0, 0, 0, 1, 0, 0, 0];
        sst.extend([1, 0, 0x4]);
        sst.extend(18i32.to_le_bytes());
        sst.push(b'x');
        sst.extend([1, 0, 12, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, b'y', 0]);
        let Record::Sst(strings) = read(sid::SST, &sst).unwrap() else {
            panic!("a table");
        };
        assert_eq!(
            strings,
            [SharedString {
                text: "x".to_owned(),
                phonetic: Some("y".to_owned())
            }]
        );
    }

    #[test]
    fn fails_where_poi_fails() {
        assert!(read(sid::EXTENDED_FORMAT, &[0; 20]).is_ok());
        assert!(read(sid::EXTENDED_FORMAT, &[0; 22]).is_err());
        assert!(read(sid::COLUMN_INFO, &[0; 15]).is_err());
        assert_eq!(read(0x7777, &[1, 2, 3]).unwrap(), Record::Unknown);
    }
}
