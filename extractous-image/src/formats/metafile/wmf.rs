//! The records of a WMF picture, read as [MS-WMF] lays them out: each by its size, the text
//! records, the objects the table of objects holds, of which the fonts the text is written in,
//! and which of them is selected. Every other record is skipped by its size. POI's
//! `HwmfPicture`, which Tika reads the picture with, parses every record and fails the picture on
//! many it cannot read, bitmaps among them, none of which give text (#127).

use super::{slice, u16_at, u32_at, Picture};

/// `META_PLACEABLE`'s key, before the header of a placeable picture.
const PLACEABLE_KEY: u32 = 0x9AC6_CDD7;
/// The bytes of `META_PLACEABLE`.
const PLACEABLE_LENGTH: usize = 22;
/// `META_EOF`.
const EOF: u16 = 0x0000;
/// `META_SELECTOBJECT`.
const SELECT_OBJECT: u16 = 0x012D;
/// `META_DELETEOBJECT`.
const DELETE_OBJECT: u16 = 0x01F0;
/// `META_CREATEFONTINDIRECT`.
const CREATE_FONT_INDIRECT: u16 = 0x02FB;
/// The records that make an object other than a font, each of which takes an index of the
/// object table: `META_CREATEPALETTE`, `META_DIBCREATEPATTERNBRUSH`, `META_CREATEPATTERNBRUSH`,
/// `META_CREATEPENINDIRECT`, `META_CREATEBRUSHINDIRECT` and `META_CREATEREGION`.
const CREATE_OTHER_OBJECT: [u16; 6] = [0x00F7, 0x0142, 0x01F9, 0x02FA, 0x02FC, 0x06FF];
/// `META_TEXTOUT`.
const TEXT_OUT: u16 = 0x0521;
/// `META_EXTTEXTOUT`.
const EXT_TEXT_OUT: u16 = 0x0A32;
/// `ETO_OPAQUE` and `ETO_CLIPPED`, with which an `META_EXTTEXTOUT` has a rectangle.
const BOUNDED: u16 = 0x0002 | 0x0004;
/// Where a `Font`'s charset stands in a `META_CREATEFONTINDIRECT` record's body.
const FONT_CHARSET_AT: usize = 13;

/// A record Tika's `WMFParser` reads something of.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Record<'a> {
    /// A font made, in a `Font` charset, at the lowest free index of the object table.
    Font { charset: u8 },
    /// Another object made at the lowest free index of the object table.
    Object,
    /// The object at an index of the object table selected.
    Select(u16),
    /// The object at an index of the object table deleted.
    Delete(u16),
    /// The string of a `META_TEXTOUT` or `META_EXTTEXTOUT`, as many bytes as its length says.
    Text(&'a [u8]),
}

/// The records of the WMF picture `data` up to its end-of-file record or its end.
pub(super) fn records(data: &[u8]) -> Picture<Record<'_>> {
    let mut picture = Picture::default();
    let mut at = if u32_at(data, 0, "the placeable key") == Ok(PLACEABLE_KEY) {
        PLACEABLE_LENGTH
    } else {
        0
    };
    let header = u16_at(data, at, "the header's type").and_then(|kind| {
        if !(1..=2).contains(&kind) {
            return Err(format!("a header of type {kind}"));
        }
        Ok(usize::from(u16_at(data, at + 2, "the header's size")?) * 2)
    });
    match header {
        Ok(size) => at += size,
        Err(reason) => {
            picture.broken = Some(format!("the data starts with no WMF header: {reason}"));
            return picture;
        }
    }
    while at < data.len() {
        let read = u32_at(data, at, "a record's size").and_then(|words| {
            let function = u16_at(data, at + 4, "a record's function")?;
            let size = (words as usize)
                .checked_mul(2)
                .filter(|&size| size >= 6)
                .ok_or_else(|| format!("a record of function {function:#06x} of {words} words"))?;
            let body = slice(data, at + 6, size - 6, "a record")?;
            Ok((function, size, record(function, body)?))
        });
        match read {
            Ok((EOF, ..)) => break,
            Ok((_, size, record)) => {
                picture.records.extend(record);
                at += size;
            }
            Err(reason) => {
                picture.broken = Some(format!("the WMF record at {at:#x}: {reason}"));
                break;
            }
        }
    }
    picture
}

/// What Tika reads of the record of `function`, whose bytes after its size and function are
/// `body`.
fn record(function: u16, body: &[u8]) -> Result<Option<Record<'_>>, String> {
    Ok(Some(match function {
        CREATE_FONT_INDIRECT => Record::Font {
            charset: slice(body, FONT_CHARSET_AT, 1, "the font's charset")?[0],
        },
        SELECT_OBJECT => Record::Select(u16_at(body, 0, "the selected object's index")?),
        DELETE_OBJECT => Record::Delete(u16_at(body, 0, "the deleted object's index")?),
        TEXT_OUT => {
            let length = usize::from(u16_at(body, 0, "the string's length")?);
            Record::Text(slice(body, 2, length, "the string")?)
        }
        EXT_TEXT_OUT => {
            let length = usize::from(u16_at(body, 4, "the string's length")?);
            let options = u16_at(body, 6, "the text options")?;
            let start = if options & BOUNDED == 0 { 8 } else { 16 };
            Record::Text(slice(body, start, length, "the string")?)
        }
        other if CREATE_OTHER_OBJECT.contains(&other) => Record::Object,
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::testing::{wmf, wmf_ext_text_out, wmf_font, wmf_record, wmf_text_out};
    use super::*;

    #[test]
    fn reads_the_texts_and_objects_up_to_the_end_of_file_record() {
        let mut data = wmf(&[
            wmf_font(204),
            wmf_record(SELECT_OBJECT, &0u16.to_le_bytes()),
            wmf_text_out(b"Hi!"),
            wmf_record(0x02FA, &[0; 10]),
            wmf_ext_text_out(b"Ok"),
            wmf_record(0x0999, &[1, 2]),
            wmf_record(DELETE_OBJECT, &1u16.to_le_bytes()),
        ]);
        data.extend(wmf_text_out(b"after the end"));
        let picture = records(&data);
        assert_eq!(picture.broken, None);
        assert_eq!(
            picture.records,
            [
                Record::Font { charset: 204 },
                Record::Select(0),
                Record::Text(b"Hi!"),
                Record::Object,
                Record::Text(b"Ok"),
                Record::Delete(1),
            ]
        );
    }

    #[test]
    fn stops_at_a_header_of_an_unknown_type_and_a_record_past_the_end() {
        let mut data = wmf(&[]);
        data[0] = 3;
        assert!(records(&data).broken.unwrap().contains("type 3"));
        let data = wmf(&[wmf_text_out(b"kept"), wmf_text_out(b"cut")]);
        let picture = records(&data[..data.len() - 8]);
        assert_eq!(picture.records, [Record::Text(b"kept")]);
        assert!(picture.broken.unwrap().contains("a record"));
    }
}
