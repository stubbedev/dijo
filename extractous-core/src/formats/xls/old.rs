//! Excel 2 to 95 workbooks (BIFF2 to BIFF5) as POI's `OldExcelExtractor` reads them for Tika's
//! `OldExcelParser`: the records of the workbook stream in order, each sheet's name, label,
//! string and number written on a line of its own, the text decoded in the code page the
//! workbook last named, and the numbers as Java writes a `double`.

use super::records::{decode_rk, read_cell, read_formula, sid, Cached};
use super::stream::RecordStream;
use crate::formats::ole2::decode_codepage;
use crate::formats::poi::{thrown, Read, Thrown};
use crate::formats::Unported;
use crate::java::dtoa::java_double_to_string;

/// The types of the records `getText` reads.
mod old_sid {
    pub(super) const BOF_BIFF2: u16 = 0x0009;
    pub(super) const BOF_BIFF3: u16 = 0x0209;
    pub(super) const BOF_BIFF4: u16 = 0x0409;
    pub(super) const SHEET: u16 = 0x0085;
    pub(super) const LABEL_BIFF2: u16 = 0x0004;
    pub(super) const LABEL: u16 = 0x0204;
    pub(super) const STRING_BIFF2: u16 = 0x0007;
    pub(super) const STRING: u16 = 0x0207;
    pub(super) const FORMULA_BIFF3: u16 = 0x0206;
    pub(super) const FORMULA_BIFF4: u16 = 0x0406;
    pub(super) const CODEPAGE: u16 = 0x0042;
}

/// `Property.DEFAULT_CODEPAGE`, which text is read in until a `CODEPAGE` record names another.
const DEFAULT_CODEPAGE: i32 = 1252;

/// The BIFF version the first record names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Biff {
    Two,
    ThreeOrFour,
    Five,
}

/// Why the workbook is left to Tika where POI or its decoding throws.
fn throws(Thrown(reason): Thrown) -> Unported {
    Unported(format!(
        "POI throws reading the old workbook, which fails Tika's parse: {reason}"
    ))
}

/// `OldExcelExtractor.getText` of a workbook stream, from its first record.
pub(super) fn text(stream: &[u8]) -> Result<String, Unported> {
    let mut stream = RecordStream::new(stream);
    let biff = prepare(&mut stream).map_err(throws)?;
    let mut text = String::new();
    let mut codepage = DEFAULT_CODEPAGE;
    while stream.has_next_record().map_err(throws)? {
        stream.next_record().map_err(throws)?;
        let line = match stream.sid() {
            sid::FILE_PASS => {
                return Err(Unported(
                    "an encrypted old workbook, which POI refuses and Tika fails on".to_owned(),
                ))
            }
            old_sid::SHEET => {
                let name = read_sheet_name(&mut stream).map_err(throws)?;
                Some(format!("Sheet: {}", decode_codepage(&name, codepage)?))
            }
            kind @ (old_sid::LABEL_BIFF2 | old_sid::LABEL) => {
                let biff2 = kind == old_sid::LABEL_BIFF2;
                read_old_cell(&mut stream, biff2).map_err(throws)?;
                let bytes = read_string(&mut stream, biff2).map_err(throws)?;
                Some(decode_codepage(&bytes, codepage)?)
            }
            kind @ (old_sid::STRING_BIFF2 | old_sid::STRING) => {
                let bytes =
                    read_string(&mut stream, kind == old_sid::STRING_BIFF2).map_err(throws)?;
                Some(decode_codepage(&bytes, codepage)?)
            }
            kind @ (sid::FORMULA | old_sid::FORMULA_BIFF3 | old_sid::FORMULA_BIFF4) => {
                let number = if biff == Biff::Five {
                    match read_formula(&mut stream).map_err(throws)? {
                        (_, Cached::Number(value)) => Some(value),
                        (_, Cached::Special(_)) => None,
                    }
                } else {
                    read_old_formula(&mut stream, kind == sid::FORMULA).map_err(throws)?
                };
                number.map(java_double_to_string)
            }
            sid::NUMBER => {
                read_cell(&mut stream).map_err(throws)?;
                Some(java_double_to_string(stream.read_f64().map_err(throws)?))
            }
            sid::RK => {
                read_cell(&mut stream).map_err(throws)?;
                Some(java_double_to_string(decode_rk(
                    stream.read_i32().map_err(throws)?,
                )))
            }
            old_sid::CODEPAGE => {
                codepage = i32::from(stream.read_i16().map_err(throws)? as u16);
                None
            }
            _ => {
                stream.read_remainder().map_err(throws)?;
                None
            }
        };
        if let Some(line) = line {
            text.push_str(&line);
            text.push('\n');
        }
    }
    Ok(text)
}

/// `prepare`: the BIFF version of the first record, a BOF record.
fn prepare(stream: &mut RecordStream<'_>) -> Read<Biff> {
    if !stream.has_next_record()? {
        return thrown("File contains no records!");
    }
    stream.next_record()?;
    let biff = match stream.sid() {
        old_sid::BOF_BIFF2 => Biff::Two,
        old_sid::BOF_BIFF3 | old_sid::BOF_BIFF4 => Biff::ThreeOrFour,
        sid::BOF => Biff::Five,
        other => {
            return thrown(format!(
                "File does not begin with a BOF, found sid of {other}"
            ))
        }
    };
    stream.read_i16()?;
    stream.read_i16()?;
    for size in [2, 2, 4, 4] {
        if stream.remaining() >= size {
            stream.read_fully(size as usize)?;
        }
    }
    Ok(biff)
}

/// `OldSheetRecord`'s name: its bytes past a leading zero byte, as many of the length it gives
/// as the record holds, the rest zeros.
fn read_sheet_name(stream: &mut RecordStream<'_>) -> Read<Vec<u8>> {
    stream.read_i32()?;
    stream.read_u8()?;
    stream.read_u8()?;
    let length = usize::from(stream.read_u8()?);
    if length > 0 && stream.peek_u8()? == 0 {
        stream.read_u8()?;
    }
    let mut name = stream.read_up_to(length)?;
    name.resize(length, 0);
    Ok(name)
}

/// `OldCellRecord`: the row, column and attributes or style of a cell.
fn read_old_cell(stream: &mut RecordStream<'_>, biff2: bool) -> Read<()> {
    stream.read_u16()?;
    stream.read_i16()?;
    if biff2 {
        stream.read_u16()?;
        stream.read_u8()?;
    } else {
        stream.read_i16()?;
    }
    Ok(())
}

/// The bytes of an `OldLabelRecord` or `OldStringRecord`: as many of the length it gives as
/// the record holds, the rest zeros.
fn read_string(stream: &mut RecordStream<'_>, biff2: bool) -> Read<Vec<u8>> {
    let length = if biff2 {
        i32::from(stream.read_u8()?)
    } else {
        i32::from(stream.read_i16()?)
    };
    let length = usize::try_from(length)
        .map_err(|_| Thrown(format!("Can't allocate an array of length {length}")))?;
    let mut bytes = stream.read_up_to(length)?;
    bytes.resize(length, 0);
    Ok(bytes)
}

/// `OldFormulaRecord`'s number, `None` where its cached value is of another type.
fn read_old_formula(stream: &mut RecordStream<'_>, biff2: bool) -> Read<Option<f64>> {
    read_old_cell(stream, biff2)?;
    let number = if biff2 {
        Some(stream.read_f64()?)
    } else {
        match Cached::read(stream.read_i64()?)? {
            Cached::Number(value) => Some(value),
            Cached::Special(_) => None,
        }
    };
    if biff2 {
        stream.read_u8()?;
    } else {
        stream.read_i16()?;
    }
    stream.read_i16()?;
    stream.read_remainder()?;
    Ok(number)
}

#[cfg(test)]
mod tests;
