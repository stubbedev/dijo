//! The records of a WMF picture as POI's `HwmfPicture` reads them, all of them before Tika's
//! `WMFParser` looks at any: the font charsets and texts Tika takes, and a failure wherever POI
//! throws on a record.

use crate::formats::metafile::dib;
use crate::formats::metafile::stream::{array_length, safely_allocate, Stream};
use crate::formats::poi::{thrown, Read, Thrown};

/// `HwmfText.MAX_RECORD_LENGTH`.
const MAX_TEXT_LENGTH: i32 = 1_000_000;
/// `HwmfEscape.MAX_OBJECT_SIZE`.
const MAX_ESCAPE_SIZE: i32 = 0xFFFF;
/// `HwmfPlaceableHeader.WMF_HEADER_MAGIC`.
const PLACEABLE_MAGIC: u32 = 0x9AC6_CDD7;
/// `WmfEscapeEMF.EMF_COMMENT_IDENTIFIER`.
const EMF_COMMENT: i32 = 0x4346_4D57;

/// A record Tika's `WMFParser` reads something of.
#[derive(Debug)]
pub(super) enum Record<'a> {
    /// `WmfCreateFontIndirect`, with the `FontCharset` id of its font.
    Font {
        charset: u8,
    },
    /// `WmfExtTextOut` or `WmfTextOut`.
    Text(Text<'a>),
    Other,
}

/// The string of a text record.
#[derive(Debug)]
pub(super) struct Text<'a> {
    /// Whether it is a `WmfExtTextOut`, whose text is cut to its length as decoded, rather than
    /// a `WmfTextOut`, whose bytes are.
    pub(super) extended: bool,
    pub(super) raw: &'a [u8],
    pub(super) string_length: i16,
}

/// `new HwmfPicture(stream).getRecords()`: the records up to the end-of-file record, the end of
/// the data or a record size POI cannot hold.
pub(super) fn records(data: &[u8]) -> Read<Vec<Record<'_>>> {
    let mut stream = Stream::new(data);
    placeable_header(&mut stream)?;
    header(&mut stream)?;
    let mut records = Vec::new();
    loop {
        let Ok(size) = stream.u32() else {
            return Ok(records);
        };
        let size = i64::from(size) * 2;
        if size > i64::from(i32::MAX) {
            return Ok(records);
        }
        let size = size as i32;
        let Ok(function) = stream.i16() else {
            return Ok(records);
        };
        if function == 0 {
            return Ok(records);
        }
        let (record, read) = init(&mut stream, function, size)
            .map_err(|Thrown(reason)| Thrown(format!("record {function:#06x}: {reason}")))?;
        let consumed = 6i32.wrapping_add(read);
        let remaining = size.wrapping_sub(consumed);
        if remaining < 0 {
            return thrown(format!(
                "RecordFormatException: read too many bytes. record size: {size}; comsumed \
                 size: {consumed}"
            ));
        }
        if remaining > 0 {
            let skipped = stream.skip_up_to(i64::from(remaining))?;
            if skipped != i64::from(remaining) {
                return thrown(format!(
                    "RecordFormatException: Tried to skip {remaining} but skipped: {skipped}"
                ));
            }
        }
        records.push(record);
    }
}

/// `HwmfPlaceableHeader.readHeader`.
fn placeable_header(stream: &mut Stream) -> Read<()> {
    stream.mark();
    if stream.u32()? != PLACEABLE_MAGIC {
        return stream.reset();
    }
    stream.bytes(18)?;
    stream.mark();
    if stream.i16()? != 0 {
        stream.reset()?;
    }
    Ok(())
}

/// `new HwmfHeader(stream)`.
fn header(stream: &mut Stream) -> Read<()> {
    let kind = stream.u16()?;
    if !(1..=2).contains(&kind) {
        return thrown(format!(
            "ArrayIndexOutOfBoundsException: Index {} out of bounds for length 2",
            i32::from(kind) - 1
        ));
    }
    let size = i32::from(stream.u16()?);
    stream.bytes(2 + 4 + 2 + 4 + 2)?;
    let left = size * 2 - 4 - 14;
    if left > 0 {
        stream.skip(i64::from(left));
    }
    Ok(())
}

/// `HwmfRecord.init` of the record with `function`, given its `size`: what Tika reads of it and
/// the bytes it read.
fn init<'a>(stream: &mut Stream<'a>, function: i16, size: i32) -> Read<(Record<'a>, i32)> {
    let read = match function as u16 {
        0x0436 | 0x00f7 | 0x0037 => {
            stream.u16()?;
            let count = stream.u16()?;
            stream.bytes(usize::from(count) * 4)?;
            2 + 2 + i32::from(count) * 4
        }
        0x0817 | 0x0830 | 0x081a => fixed(stream, 16)?,
        0x0922 => bit_blt(stream, size, function, Source::Bitmap16)?,
        0x0b23 => stretch_blt(stream, size, function, Source::Bitmap16)?,
        0x0940 => bit_blt(stream, size, function, Source::Dib)?,
        0x0b41 => stretch_blt(stream, size, function, Source::Dib)?,
        0x02fc => fixed(stream, 8)?,
        0x02fb => return font(stream),
        0x01f9 => bitmap16(stream, true)?,
        0x02fa => fixed(stream, 10)?,
        0x06ff => create_region(stream)?,
        0x01f0 | 0x012a | 0x012b | 0x0139 | 0x0127 | 0x012c | 0x012d | 0x0234 | 0x0102 | 0x0103
        | 0x0106 | 0x0104 | 0x0107 | 0x012e | 0x0108 => fixed(stream, 2)?,
        0x0142 => dib_pattern_brush(stream, size)?,
        0x0418 | 0x0415 | 0x0416 | 0x0419 | 0x0429 | 0x041b | 0x0412 | 0x0410 | 0x041f => {
            fixed(stream, 8)?
        }
        0x0626 => escape(stream)?,
        0x0548 => {
            let mode = stream.u16()?;
            if mode > 1 {
                return thrown(format!(
                    "ArrayIndexOutOfBoundsException: Index {mode} out of bounds for length 2"
                ));
            }
            fixed(stream, 8)? + 2
        }
        0x0a32 => return ext_text_out(stream, size),
        0x0228 | 0x0213 | 0x0214 | 0x0220 | 0x0211 | 0x020f | 0x0201 | 0x0149 | 0x0231 | 0x0209
        | 0x020a | 0x020e | 0x020d | 0x020c | 0x020b => fixed(stream, 4)?,
        0x061d | 0x061c => fixed(stream, 12)?,
        0x0324 | 0x0325 => {
            let points = stream.i16()?;
            array_length(i64::from(points))?;
            for _ in 0..points {
                stream.bytes(4)?;
            }
            2 + i32::from(points) * 4
        }
        0x0538 => poly_polygon(stream)?,
        0x0035 | 0x001e | 0x0105 => 0,
        0x0d33 => {
            stream.bytes(18)?;
            18 + dib::init(stream, size.wrapping_sub(6 + 18))?
        }
        0x0f43 => {
            stream.bytes(22)?;
            22 + dib::init(stream, size.wrapping_sub(6 + 22))?
        }
        0x0521 => return text_out(stream),
        _ => return thrown(format!("IOException: unexpected record type: {function}")),
    };
    Ok((Record::Other, read))
}

/// Reads `length` bytes of fields POI keeps but Tika does not use.
fn fixed(stream: &mut Stream, length: usize) -> Read<i32> {
    stream.bytes(length)?;
    Ok(length as i32)
}

/// The bitmap a blit record carries, when it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `HwmfBitmap16`, of `WmfBitBlt` and `WmfStretchBlt`.
    Bitmap16,
    /// `HwmfBitmapDib`, of `WmfDibBitBlt` and `WmfDibStretchBlt`.
    Dib,
}

/// `hasBitmap`: whether a blit record of this size carries a bitmap.
fn has_bitmap(size: i64, function: i16) -> bool {
    size > i64::from((i32::from(function) >> 8) + 3)
}

/// The bitmap of a blit record, after the `read` bytes before it.
fn blit_bitmap(stream: &mut Stream, source: Source, size: i32, read: i32) -> Read<i32> {
    Ok(match source {
        Source::Bitmap16 => bitmap16(stream, false)?,
        Source::Dib => dib::init(stream, size.wrapping_sub(6).wrapping_sub(read))?,
    })
}

/// `WmfBitBlt.init` and `WmfDibBitBlt.init`.
fn bit_blt(stream: &mut Stream, size: i32, function: i16, source: Source) -> Read<i32> {
    let bitmap = has_bitmap(i64::from(size / 2), function);
    stream.bytes(4 + 4)?;
    let mut read = 8;
    if !bitmap {
        stream.i16()?;
        read += 2;
    }
    stream.bytes(8)?;
    read += 8;
    if bitmap {
        read += blit_bitmap(stream, source, size, read)?;
    }
    Ok(read)
}

/// `WmfStretchBlt.init` and `WmfDibStretchBlt.init`.
fn stretch_blt(stream: &mut Stream, size: i32, function: i16, source: Source) -> Read<i32> {
    let bitmap = has_bitmap(i64::from(size), function);
    stream.bytes(4 + 8)?;
    let mut read = 12;
    if !bitmap {
        stream.i16()?;
        read += 2;
    }
    stream.bytes(8)?;
    read += 8;
    if bitmap {
        read += blit_bitmap(stream, source, size, read)?;
    }
    Ok(read)
}

/// `HwmfBitmap16.init`, which counts none of the bits it reads.
fn bitmap16(stream: &mut Stream, partial: bool) -> Read<i32> {
    stream.i16()?;
    let width = i32::from(stream.i16()?);
    let height = i32::from(stream.i16()?);
    stream.i16()?;
    stream.u8()?;
    let bits = i32::from(stream.u8()?);
    let mut read = 10;
    if partial {
        stream.skip(4);
        stream.skip(18);
        read += 22;
    }
    let length = ((width.wrapping_mul(bits).wrapping_add(15) >> 4) << 1).wrapping_mul(height);
    stream.byte_array(length, i32::MAX)?;
    Ok(read)
}

/// `WmfCreateRegion.init`.
fn create_region(stream: &mut Stream) -> Read<i32> {
    stream.bytes(2 + 2 + 4 + 2)?;
    let scans = stream.i16()?;
    stream.bytes(2 + 8)?;
    let mut read = 22;
    array_length(i64::from(scans))?;
    for _ in 0..scans {
        let count = stream.u16()?;
        stream.bytes(4)?;
        stream.bytes(usize::from(count / 2) * 4)?;
        stream.u16()?;
        read += 6 + i32::from(count / 2) * 4 + 2;
    }
    Ok(read)
}

/// `WmfDibCreatePatternBrush.init`.
fn dib_pattern_brush(stream: &mut Stream, size: i32) -> Read<i32> {
    let style = stream.u16()?;
    stream.u16()?;
    let read = 4;
    Ok(match style {
        0 | 1 | 2 | 3 | 5 | 6 => read + dib::init(stream, size.wrapping_sub(6 + read))?,
        4 | 7 | 8 | 9 => return thrown("IllegalStateException: pattern not supported"),
        0x100 => read,
        other => {
            return thrown(format!(
                "NullPointerException: a brush style {other}, which POI does not know"
            ))
        }
    })
}

/// `HwmfEscape.init`.
fn escape(stream: &mut Stream) -> Read<i32> {
    let function = stream.u16()?;
    let count = i32::from(stream.u16()?);
    let read = 4;
    if function != 0x000F {
        stream.byte_array(count, MAX_ESCAPE_SIZE)?;
        return Ok(read + count);
    }
    if count < 4 {
        return Ok(read);
    }
    if stream.i32()? != EMF_COMMENT {
        stream.byte_array(count - 4, MAX_ESCAPE_SIZE)?;
        return Ok(read + count);
    }
    stream.bytes(4 + 4 + 2 + 4 + 4)?;
    let current = stream.i32()?;
    stream.bytes(8)?;
    let data = stream.byte_array(current, MAX_ESCAPE_SIZE)?;
    Ok(read + 34 + data.len() as i32)
}

/// `WmfPolyPolygon.init`, whose every polygon's path is closed, which fails for one of no
/// points.
fn poly_polygon(stream: &mut Stream) -> Read<i32> {
    let polygons = stream.u16()?;
    let mut counts = Vec::with_capacity(usize::from(polygons));
    for _ in 0..polygons {
        counts.push(stream.u16()?);
    }
    let mut read = 2 + 2 * i32::from(polygons);
    for count in counts {
        stream.bytes(usize::from(count) * 4)?;
        read += 4 * i32::from(count);
        if count == 0 {
            return thrown("IllegalPathStateException: missing initial moveto in path definition");
        }
    }
    Ok(read)
}

/// `WmfCreateFontIndirect.init`: the font's charset.
fn font<'a>(stream: &mut Stream<'a>) -> Read<(Record<'a>, i32)> {
    stream.bytes(10 + 3)?;
    let charset = stream.u8()?;
    stream.bytes(4)?;
    let mut read = 0i32;
    loop {
        if read == 32 {
            return thrown("IOException: Font facename can't be determined.");
        }
        let byte = stream.u8()?;
        read += 1;
        if byte == 0 || byte == 0xFF {
            break;
        }
    }
    Ok((Record::Font { charset }, 18 + read))
}

/// `WmfExtTextOut.init`.
fn ext_text_out<'a>(stream: &mut Stream<'a>, size: i32) -> Read<(Record<'a>, i32)> {
    let remaining = size.wrapping_sub(6);
    stream.bytes(4)?;
    let string_length = stream.i16()?;
    let options = stream.u16()?;
    let mut read = 8i32;
    if options & 0x0006 != 0 && read + 8 <= remaining {
        stream.bytes(8)?;
        read += 8;
    }
    let raw = text_bytes(stream, string_length)?;
    read += raw.len() as i32;
    if read < remaining {
        let dx = i32::from(string_length).min((remaining - read) / 2);
        for _ in 0..dx {
            stream.i16()?;
            read += 2;
        }
    }
    let text = Text {
        extended: true,
        raw,
        string_length,
    };
    Ok((Record::Text(text), read))
}

/// `WmfTextOut.init`.
fn text_out<'a>(stream: &mut Stream<'a>) -> Read<(Record<'a>, i32)> {
    let string_length = stream.i16()?;
    let raw = text_bytes(stream, string_length)?;
    stream.bytes(4)?;
    let read = 6 + raw.len() as i32;
    let text = Text {
        extended: false,
        raw,
        string_length,
    };
    Ok((Record::Text(text), read))
}

/// The bytes of a string of `length` bytes, padded to an even length.
fn text_bytes<'a>(stream: &mut Stream<'a>, length: i16) -> Read<&'a [u8]> {
    let length = i64::from(length) + i64::from(length & 1);
    let length = safely_allocate(length, MAX_TEXT_LENGTH)?;
    stream.bytes(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(records: &[Vec<u8>]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&9u16.to_le_bytes());
        data.extend_from_slice(&[0; 14]);
        for record in records {
            data.extend_from_slice(record);
        }
        data
    }

    fn record(function: u16, data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((6 + data.len() as u32) / 2).to_le_bytes());
        bytes.extend_from_slice(&function.to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn reads_the_texts_and_fonts_up_to_the_end_of_file_record() {
        let mut text = Vec::new();
        text.extend_from_slice(&2i16.to_le_bytes());
        text.extend_from_slice(b"Hi");
        text.extend_from_slice(&[0; 4]);
        let mut font = vec![0; 13];
        font.push(204);
        font.extend_from_slice(&[0; 4]);
        font.extend_from_slice(b"Arial\0");
        let data = picture(&[
            record(0x02fb, &font),
            record(0x0521, &text),
            record(0x0000, &[]),
            record(0x0521, &text),
        ]);
        let records = records(&data).unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(records[0], Record::Font { charset: 204 }));
        let Record::Text(text) = &records[1] else {
            panic!("a text record: {records:?}");
        };
        assert_eq!(text.raw, b"Hi");
        assert!(!text.extended);
    }

    #[test]
    fn fails_on_a_header_of_an_unknown_type_and_an_unknown_record() {
        let mut data = picture(&[]);
        data[0] = 3;
        assert!(records(&data).is_err());
        assert!(records(&picture(&[record(0x0999, &[])])).is_err());
    }

    #[test]
    fn a_polygon_of_no_points_fails_as_its_closed_path_does() {
        let data = [1u16.to_le_bytes(), 0u16.to_le_bytes()].concat();
        assert!(records(&picture(&[record(0x0538, &data)])).is_err());
    }
}
