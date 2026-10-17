//! The records of an EMF picture as POI's `HemfPicture` reads them, all of them before Tika's
//! `EMFParser` looks at any: what Tika takes from the comments and the text records, and a
//! failure wherever POI throws on a record.

use std::borrow::Cow;

use crate::formats::metafile::dib::{self, MAX_RECORD_LENGTH};
use crate::formats::metafile::emf_plus;
use crate::formats::metafile::stream::{array_length, safely_allocate, to_int_exact, Stream};
use crate::formats::poi::{thrown, Read, Thrown};

/// `HemfRecordIterator.HEADER_SIZE`: a record's type and size.
const HEADER_SIZE: i64 = 8;
/// `HemfText.MAX_RECORD_LENGTH`.
const MAX_TEXT_LENGTH: i32 = 1_000_000;
/// `HemfRecordType.eof`.
const EOF: u32 = 0x0E;
/// The record signature of the header, ` EMF`.
const SIGNATURE: i32 = 0x464D_4520;
/// `HemfCommentRecordType.emfPublic`'s identifier.
const PUBLIC_COMMENT: i32 = 0x4349_4447;

/// A rectangle as `Rectangle2D.Double` holds it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct Rect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

/// A record Tika's `EMFParser` reads something of.
#[derive(Debug)]
pub(super) enum Record<'a> {
    Comment(Comment<'a>),
    Text(TextOut<'a>),
    Other,
}

/// What an `EmfComment` holds, as far as Tika tells comments apart.
#[derive(Debug)]
pub(super) enum Comment<'a> {
    /// `EmfCommentDataMultiformats`: the data of each format, none for the formats after the
    /// first whose data ran short.
    Formats(Vec<Option<Cow<'a, [u8]>>>),
    /// `EmfCommentDataWMF`: an embedded WMF picture.
    Wmf(Cow<'a, [u8]>),
    /// `EmfCommentDataGeneric`: private data.
    Generic(&'a [u8]),
    /// Any other comment.
    Other,
}

/// `EmfExtTextOutA` or `EmfExtTextOutW`.
#[derive(Debug)]
pub(super) struct TextOut<'a> {
    /// Whether the text is UTF-16 (`EMR_EXTTEXTOUTW`).
    pub(super) unicode: bool,
    /// `getBounds`: the clipping or opaquing rectangle, zero where the record has none.
    pub(super) bounds: Rect,
    /// The bounds the record says to ignore.
    pub(super) bounds_ignored: Rect,
    /// The bytes of the string, unless POI found no string where its offset points.
    pub(super) raw: Option<&'a [u8]>,
    /// The number of characters the record gives.
    pub(super) string_length: i32,
}

/// `HemfPicture.getRecords()`: every record up to the end-of-file record or the end of the
/// data.
pub(super) fn records(data: &[u8]) -> Read<Vec<Record<'_>>> {
    let mut stream = Stream::new(data);
    let mut records = Vec::new();
    loop {
        let Ok(id) = stream.u32() else {
            return Ok(records);
        };
        let Ok(size) = stream.u32() else {
            return Ok(records);
        };
        let start = stream.read_index();
        let remaining = i64::from(size) - HEADER_SIZE;
        let (record, read) = init(&mut stream, id, remaining).map_err(|Thrown(reason)| {
            Thrown(format!(
                "RecordFormatException: record {id:#x} at {:#x}: {reason}",
                start - HEADER_SIZE
            ))
        })?;
        if read > remaining {
            return thrown(format!(
                "RecordFormatException: Record limit exceeded - readBytes: {read} / remBytes: \
                 {remaining}"
            ));
        }
        stream.skip_fully((remaining - read) as i32)?;
        records.push(record);
        if id == EOF {
            return Ok(records);
        }
    }
}

/// `HemfRecord.init` of the record of type `id`: what Tika reads of it and the bytes it read,
/// given the `size` of the record but its header.
fn init<'a>(stream: &mut Stream<'a>, id: u32, size: i64) -> Read<(Record<'a>, i64)> {
    let start = stream.read_index();
    let read = match id {
        0x01 => header(stream, size)?,
        0x02 | 0x55 => poly_bezier(stream, true, id == 0x02)?,
        0x05 | 0x58 => poly_bezier(stream, false, id == 0x05)?,
        0x03 | 0x04 | 0x06 | 0x56 | 0x57 | 0x59 => polygon(stream, id < 0x10)?,
        0x07 | 0x08 => poly_polygon(stream, id == 0x08, true)?,
        0x5A | 0x5B => poly_polygon(stream, id == 0x5B, false)?,
        0x09..=0x0D | 0x1A | 0x1B | 0x36 => fixed(stream, 8)?,
        0x0E => eof(stream, size)?,
        0x0F => fixed(stream, 12)?,
        0x10..=0x16 | 0x18 | 0x19 | 0x22 | 0x25 | 0x28 | 0x30 | 0x3A | 0x43 | 0x62 => {
            fixed(stream, 4)?
        }
        0x1D | 0x1E | 0x1F | 0x20 | 0x2A | 0x2B => fixed(stream, 16)?,
        0x21 | 0x34 | 0x3B..=0x3D | 0x41 | 0x42 | 0x44 => 0,
        0x23 => fixed(stream, 24)?,
        0x24 => fixed(stream, 28)?,
        0x26 => fixed(stream, 20)?,
        0x27 => fixed(stream, 16)?,
        0x2C => fixed(stream, 24)?,
        0x2D..=0x2F | 0x37 => fixed(stream, 32)?,
        0x31 => {
            stream.u32()?;
            stream.u16()?;
            6 + palette_entries(stream, -1)?
        }
        0x32 => {
            stream.u32()?;
            stream.u32()?;
            let count = stream.u32()? as i32;
            12 + palette_entries(stream, count)?
        }
        0x33 => fixed(stream, 8)?,
        0x35 => {
            stream.bytes(12)?;
            let mode = stream.u32()?;
            if mode > 1 {
                return thrown(format!(
                    "ArrayIndexOutOfBoundsException: Index {} out of bounds for length 2",
                    mode as i32
                ));
            }
            16
        }
        0x38 | 0x5C => poly_draw(stream, id == 0x38)?,
        0x3E..=0x40 => {
            if size == 0 {
                0
            } else {
                fixed(stream, 16)?
            }
        }
        0x46 => {
            return comment(stream, size).map(|comment| {
                let read = stream.read_index() - start;
                (Record::Comment(comment), read)
            })
        }
        0x47 => {
            stream.bytes(24)?;
            24 + region_data(stream)?
        }
        0x48 => {
            stream.bytes(32)?;
            32 + region_data(stream)?
        }
        0x49 | 0x4A => {
            stream.bytes(20)?;
            20 + region_data(stream)?
        }
        0x4B => {
            stream.u32()?;
            let mode = stream.u32()?;
            8 + if mode == 5 { 0 } else { region_data(stream)? }
        }
        0x4C | 0x4D => stretch_blt(stream, size)?,
        0x50 => {
            stream.bytes(16 + 8 + 16)?;
            let fields = [stream.i32()?, stream.i32()?, stream.i32()?, stream.i32()?];
            stream.bytes(12)?;
            40 + 28 + read_bitmap(stream, start, fields)?
        }
        0x51 => {
            stream.bytes(16 + 8 + 16)?;
            let read = 40;
            let fields = [stream.i32()?, stream.i32()?, stream.i32()?, stream.i32()?];
            stream.bytes(16)?;
            read + 32 + read_bitmap(stream, start, fields)?
        }
        0x52 => {
            stream.u32()?;
            4 + i64::from(font(stream, i64::from((size - 4) as i32))?)
        }
        0x53 | 0x54 => return text_out(stream, size, id == 0x54),
        0x5D => {
            stream.bytes(8)?;
            let off_bmi = stream.u32()? as i32;
            let cb_bmi = stream.u32()? as i32;
            let off_bits = stream.u32()? as i32;
            let cb_bits = to_int_exact(i64::from(stream.u32()?))?;
            let bitmap = read_bitmap(stream, start, [off_bmi, cb_bmi, off_bits, cb_bits])?;
            i64::from(to_int_exact(24 + bitmap)?)
        }
        0x5E => {
            stream.bytes(8)?;
            let fields = [stream.i32()?, stream.i32()?, stream.i32()?, stream.i32()?];
            i64::from(to_int_exact(24 + read_bitmap(stream, start, fields)?)?)
        }
        0x5F => ext_create_pen(stream, start)?,
        0x72 => {
            stream.bytes(16 + 16 + 4 + 8 + 24 + 4 + 4)?;
            let fields = [stream.i32()?, stream.i32()?, stream.i32()?, stream.i32()?];
            stream.bytes(8)?;
            let read = 16 + 16 + 4 + 8 + 24 + 4 + 4 + 16 + 8;
            read + read_bitmap(stream, start, fields)?
        }
        0x17
        | 0x1C
        | 0x29
        | 0x39
        | 0x4E
        | 0x4F
        | 0x60
        | 0x61
        | 0x63..=0x6A
        | 0x6C..=0x71
        | 0x73
        | 0x74
        | 0x76..=0x7A => {
            let skipped = stream.skip_up_to(size)?;
            if skipped < size {
                return thrown("IOException: End of stream reached before record read");
            }
            skipped
        }
        other => {
            return thrown(format!(
                "RecordFormatException: Undefined record of type: {other}"
            ))
        }
    };
    Ok((Record::Other, read))
}

/// Reads `length` bytes of fields POI keeps but Tika does not use.
fn fixed(stream: &mut Stream, length: usize) -> Read<i64> {
    stream.bytes(length)?;
    Ok(length as i64)
}

/// `HemfHeader.init`, given the `size` of the record but its header.
fn header(stream: &mut Stream, size: i64) -> Read<i64> {
    stream.bytes(32)?;
    let signature = stream.i32()?;
    if signature != SIGNATURE {
        return thrown(format!("IOException: bad record signature: {signature}"));
    }
    stream.i32()?;
    stream.u32()?;
    stream.u32()?;
    stream.u16()?;
    stream.skip_fully(2)?;
    let descriptions = stream.u32()? as i32;
    let description_offset = stream.u32()? as i32;
    stream.u32()?;
    let mut read = 32 + 32;
    stream.bytes(16)?;
    read += 16;
    if descriptions > 0 && description_offset > 0 {
        let skip = (i64::from(description_offset) - (read + HEADER_SIZE)) as i32;
        stream.mark();
        stream.skip_fully(skip)?;
        let length = array_length(i64::from((descriptions - 1).wrapping_mul(2)))?;
        stream.bytes(length)?;
        stream.reset()?;
    }
    if read + 12 <= size {
        stream.bytes(12)?;
        read += 12;
    }
    if read + 8 <= size {
        stream.bytes(8)?;
        read += 8;
    }
    Ok(read)
}

/// A `Path2D` as far as a record builds one: whether it has a segment yet, which a line,
/// curve or close needs or Java throws `IllegalPathStateException`.
#[derive(Debug, Default)]
struct Path {
    started: bool,
}

impl Path {
    fn move_to(&mut self) {
        self.started = true;
    }

    fn segment(&self) -> Read<()> {
        if self.started {
            Ok(())
        } else {
            thrown("IllegalPathStateException: missing initial moveto in path definition")
        }
    }

    /// `new Path2D.Double(rule, capacity)`, which a negative capacity fails.
    fn with_capacity(capacity: i32) -> Read<Self> {
        array_length(i64::from(capacity))?;
        array_length(i64::from(capacity.wrapping_mul(2)))?;
        Ok(Self::default())
    }
}

/// A point of 32-bit (`PointL`) or 16-bit (`PointS`) coordinates.
fn point(stream: &mut Stream, long: bool) -> Read<i64> {
    fixed(stream, if long { 8 } else { 4 })
}

/// `EmfPolyBezier.init` and its variants: `starts` where the record gives its start point.
fn poly_bezier(stream: &mut Stream, starts: bool, long: bool) -> Read<i64> {
    stream.bytes(16)?;
    let count = stream.u32()? as i32;
    let points = count.min(16384);
    let mut read = 20;
    let mut path = Path::with_capacity(points.wrapping_add(2))?;
    let mut i = 0;
    if starts {
        if i < points {
            read += point(stream, long)?;
            path.move_to();
            i += 1;
        }
    } else {
        path.move_to();
    }
    while i + 2 < points {
        read += point(stream, long)?;
        read += point(stream, long)?;
        read += point(stream, long)?;
        path.segment()?;
        i += 3;
    }
    Ok(read)
}

/// `EmfPolygon.init` and its variants.
fn polygon(stream: &mut Stream, long: bool) -> Read<i64> {
    stream.bytes(16)?;
    let count = stream.u32()? as i32;
    let points = count.min(16384);
    Path::with_capacity(points)?;
    let mut read = 20;
    for _ in 0..points.max(0) {
        read += point(stream, long)?;
    }
    Ok(read)
}

/// `EmfPolyPolygon.init` and its variants: `closed` for polygons, whose paths are closed.
fn poly_polygon(stream: &mut Stream, closed: bool, long: bool) -> Read<i64> {
    stream.bytes(16)?;
    let polygons = i64::from(stream.u32()?);
    stream.u32()?;
    let mut read = 24 + polygons * 4;
    let polygons = array_length(i64::from(polygons as i32))?;
    let mut counts = Vec::new();
    for _ in 0..polygons {
        counts.push(i64::from(stream.u32()?));
    }
    for count in counts {
        let mut path = Path::with_capacity(count as i32)?;
        for i in 0..count {
            read += point(stream, long)?;
            if i == 0 {
                path.move_to();
            }
        }
        if closed {
            path.segment()?;
        }
    }
    Ok(read)
}

/// `EmfPolyDraw.init` and its 16-bit variant.
fn poly_draw(stream: &mut Stream, long: bool) -> Read<i64> {
    stream.bytes(16)?;
    let count = stream.u32()? as i32;
    let points = array_length(i64::from(count))?;
    let mut read = 20;
    for _ in 0..points {
        read += point(stream, long)?;
    }
    let mut path = Path::with_capacity(count)?;
    let mut i = 0;
    while i < points {
        let mut mode = stream.u8()?;
        match mode & 0x06 {
            0x02 => path.segment()?,
            0x04 => {
                stream.u8()?;
                let third = stream.u8()?;
                if i + 2 >= points {
                    return thrown(
                        "IllegalStateException: Points index causes index out of bounds",
                    );
                }
                path.segment()?;
                mode = third;
                i += 2;
            }
            0x06 => path.move_to(),
            _ => {}
        }
        if mode & 0x01 == 0x01 {
            path.segment()?;
        }
        i += 1;
    }
    Ok(read + i64::from(count))
}

/// `EmfEof.init`.
fn eof(stream: &mut Stream, size: i64) -> Read<i64> {
    let entries = stream.u32()? as i32;
    let offset = stream.u32()? as i32;
    let mut read = 8i32;
    if entries > 0 && offset > 0 {
        let gap = offset.wrapping_sub(read + HEADER_SIZE as i32);
        stream.skip_fully(gap)?;
        read = read.wrapping_add(gap);
        for _ in 0..entries {
            stream.bytes(4)?;
            read = read.wrapping_add(4);
        }
        let rest = (size - i64::from(read) - 4) as i32;
        stream.skip_fully(rest)?;
        read = read.wrapping_add(rest);
    }
    stream.u32()?;
    Ok(i64::from(read.wrapping_add(4)))
}

/// `WmfPaletteParent.readPaletteEntries`: `count` entries, or as many as a 16-bit count before
/// them gives when `count` is negative.
fn palette_entries(stream: &mut Stream, count: i32) -> Read<i64> {
    let (count, read) = if count > -1 {
        (count, 0)
    } else {
        (i32::from(stream.u16()?), 2)
    };
    let mut read = read;
    for _ in 0..count {
        stream.bytes(4)?;
        read += 4;
    }
    Ok(i64::from(read))
}

/// `HemfFill.readRgnData`.
fn region_data(stream: &mut Stream) -> Read<i64> {
    stream.u32()?;
    stream.u32()?;
    let rectangles = i64::from(stream.u32()?);
    stream.u32()?;
    stream.bytes(16)?;
    let mut read = 32;
    let mut i = 0i32;
    while i64::from(i) < rectangles {
        stream.bytes(16)?;
        read += 16;
        i = i.wrapping_add(1);
    }
    Ok(read)
}

/// `EmfStretchBlt.init`, also of `EmfBitBlt`.
fn stretch_blt(stream: &mut Stream, size: i64) -> Read<i64> {
    let start = stream.read_index();
    stream.bytes(16 + 16 + 4 + 8 + 24 + 4 + 4)?;
    let off_bmi = stream.u32()? as i32;
    let cb_bmi = stream.u32()? as i32;
    let mut read = 16 + 16 + 4 + 8 + 24 + 4 + 12;
    if read >= size {
        return Ok(read);
    }
    let off_bits = stream.u32()? as i32;
    let cb_bits = stream.u32()? as i32;
    read += 8;
    if read >= size {
        return Ok(read);
    }
    stream.bytes(8)?;
    read += 8;
    Ok(read + read_bitmap(stream, start, [off_bmi, cb_bmi, off_bits, cb_bits])?)
}

/// `HemfFill.readBitmap` of the record whose fields start at read index `start`, given the
/// offsets and sizes of its bitmap's header and bits.
fn read_bitmap(
    stream: &mut Stream,
    start: i64,
    [off_bmi, cb_bmi, off_bits, cb_bits]: [i32; 4],
) -> Read<i64> {
    if off_bmi == 0 {
        return Ok(0);
    }
    let current = (stream.read_index() - (start - HEADER_SIZE)) as i32;
    let gap = off_bmi.wrapping_sub(current);
    if gap < 0 {
        return Ok(0);
    }
    let second_gap = off_bits
        .wrapping_sub(current)
        .wrapping_sub(cb_bmi)
        .wrapping_sub(gap);
    stream.skip_fully(gap)?;
    if cb_bmi == 0 || cb_bits == 0 {
        return Ok(i64::from(gap));
    }
    let dib_size = cb_bmi.wrapping_add(cb_bits);
    if second_gap == 0 {
        return Ok(i64::from(gap) + i64::from(dib::init(stream, dib_size)?));
    }
    let mut copied = Vec::new();
    stream.copy(i64::from(cb_bmi), &mut copied);
    stream.skip_fully(second_gap)?;
    stream.copy(i64::from(cb_bits), &mut copied);
    dib::init(&mut Stream::new(&copied), dib_size)?;
    Ok(i64::from(gap) + i64::from(cb_bmi) + i64::from(second_gap) + i64::from(cb_bits))
}

/// `EmfExtCreatePen.init`.
fn ext_create_pen(stream: &mut Stream, start: i64) -> Read<i64> {
    stream.u32()?;
    let fields = [
        stream.u32()? as i32,
        stream.u32()? as i32,
        stream.u32()? as i32,
        stream.u32()? as i32,
    ];
    stream.bytes(8 + 4 + 4 + 4)?;
    let entries = stream.u32()? as i32;
    let read = 44i32;
    let count = array_length(i64::from(entries))?;
    stream.bytes(count.saturating_mul(4))?;
    let read = read
        .checked_add(entries.wrapping_mul(4))
        .ok_or_else(|| Thrown::new("ArithmeticException: integer overflow"))?;
    let bitmap = read_bitmap(stream, start, fields)?;
    Ok(i64::from(to_int_exact(i64::from(read) + bitmap)?))
}

/// `HemfFont.init`, given the size it is told the font has.
fn font(stream: &mut Stream, size: i64) -> Read<i32> {
    stream.bytes(20 + 8)?;
    let mut read = 28;
    read += wide_name(stream, 32, "facename")?;
    if size <= 92 {
        return Ok(read);
    }
    read += wide_name(stream, 64, "fullname")?;
    read += wide_name(stream, 32, "style")?;
    if size == 320 {
        stream.bytes(24)?;
        const VALUES: [u8; 10] = [6, 16, 12, 10, 10, 9, 12, 16, 14, 8];
        for values in VALUES {
            let index = stream.u8()?;
            if index >= values {
                return thrown(format!(
                    "ArrayIndexOutOfBoundsException: Index {index} out of bounds for length \
                     {values}"
                ));
            }
        }
        let skipped = stream.skip_up_to(2)?;
        if skipped != 2 {
            return thrown(format!("IOException: Didn't skip 2: {skipped}"));
        }
        return Ok(read + 24 + 10 + 2);
    }
    read += wide_name(stream, 32, "script")?;
    stream.i32()?;
    let axes = stream.i32()?;
    read += 8;
    if (0..=16).contains(&axes) {
        stream.bytes(axes as usize * 4)?;
        read += axes * 4;
    }
    Ok(read)
}

/// `HemfFont.readString`: a name of `limit` UTF-16 units, which must end before the limit, at a
/// zero unit or a byte of 0xFF.
fn wide_name(stream: &mut Stream, limit: usize, what: &str) -> Read<i32> {
    let bytes = stream.bytes(limit * 2)?;
    let ends = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .any(|&[first, second]| (first, second) == (0, 0) || first == 0xFF || second == 0xFF);
    if !ends {
        return thrown(format!("IOException: Font {what} can't be determined."));
    }
    Ok(limit as i32 * 2)
}

/// `EmfExtTextOutA.init`, also of `EmfExtTextOutW`, given the `size` of the record but its
/// header.
fn text_out<'a>(stream: &mut Stream<'a>, size: i64, unicode: bool) -> Read<(Record<'a>, i64)> {
    if !(0..i64::from(i32::MAX)).contains(&size) {
        return thrown(
            "RecordFormatException: recordSize must be a positive integer (0-0x7FFFFFFF)",
        );
    }
    let bounds_ignored = rect_l(stream)?;
    let mode = stream.i32()?;
    if !(1..=2).contains(&mode) {
        return thrown(format!(
            "ArrayIndexOutOfBoundsException: Index {} out of bounds for length 2",
            mode.wrapping_sub(1)
        ));
    }
    stream.bytes(8 + 8)?;
    let string_length = stream.u32()? as i32;
    let off_string = stream.u32()? as i32;
    let options = stream.u32()? as i32;
    let mut read = 16 + 4 + 8 + 8 + 8 + 4;
    let bounds = if options & 0x0006 != 0 {
        read += 16;
        rect_l(stream)?
    } else {
        Rect::default()
    };
    let off_dx = stream.u32()? as i32;
    read += 4;
    let dx_first = off_dx < off_string;
    let mut string_end = if i64::from(off_dx) <= HEADER_SIZE {
        size
    } else {
        i64::from(off_dx) - HEADER_SIZE
    } as i32;
    let mut raw = None;
    for dx in if dx_first {
        [true, false]
    } else {
        [false, true]
    } {
        if dx {
            let gap = (i64::from(off_dx) - (read + HEADER_SIZE)) as i32;
            if off_dx > 0 && gap >= 0 && i64::from(off_dx) - HEADER_SIZE < size {
                stream.skip_fully(gap)?;
                read += i64::from(gap);
                let end = if dx_first {
                    i64::from(off_string) - HEADER_SIZE
                } else {
                    size
                };
                let max = end.min(size) as i32;
                while read <= i64::from(max.wrapping_sub(4)) {
                    stream.u32()?;
                    read += 4;
                }
            }
            string_end = to_int_exact(size)?;
            continue;
        }
        let gap = to_int_exact(i64::from(off_string) - (read + HEADER_SIZE))?;
        if off_string > 0 && gap >= 0 && i64::from(off_string) - HEADER_SIZE < size {
            stream.skip_fully(gap)?;
            read += i64::from(gap);
            let length = to_int_exact(size.min(i64::from(string_end)) - read)?;
            let length = safely_allocate(i64::from(length), MAX_TEXT_LENGTH)?;
            raw = Some(stream.bytes(length)?);
            read += length as i64;
        }
    }
    let text = TextOut {
        unicode,
        bounds,
        bounds_ignored,
        raw,
        string_length,
    };
    Ok((Record::Text(text), read))
}

/// `HemfDraw.readRectL`.
fn rect_l(stream: &mut Stream) -> Read<Rect> {
    let left = f64::from(stream.i32()?);
    let top = f64::from(stream.i32()?);
    let right = f64::from(stream.i32()?);
    let bottom = f64::from(stream.i32()?);
    Ok(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// `EmfComment.init`: the comment data `EmfCommentDataIterator` reads first, the rest of the
/// record skipped.
fn comment<'a>(stream: &mut Stream<'a>, size: i64) -> Read<Comment<'a>> {
    let limit = size as i32;
    let data_size = i64::from(stream.u32()?);
    stream.mark();
    let identifier = stream.u32()? as i32;
    let public_identifier = stream.u32()? as i32;
    stream.reset()?;
    let start = stream.read_index();
    let comment = if identifier == PUBLIC_COMMENT {
        match public_identifier as u32 {
            0x0000_0002 => {
                public_type(stream)?;
                stream.bytes(16)?;
                let length = stream.u32()? as i32;
                let length = safely_allocate(i64::from(length) * 2, MAX_RECORD_LENGTH)?;
                stream.bytes(length)?;
                Comment::Other
            }
            0x0000_0003 => {
                public_type(stream)?;
                Comment::Other
            }
            0x4000_0004 => multiformats(stream)?,
            0x8000_0001 => wmf(stream)?,
            0x0000_0040 | 0x0000_0080 => {
                return thrown(
                    "RecordFormatException: UNICODE_STRING/UNICODE_END values are reserved in \
                     CommentPublic records",
                )
            }
            _ => generic(stream, data_size)?,
        }
    } else if identifier == 0x2B46_4D45 {
        public_type(stream)?;
        emf_plus::read_records(stream, (data_size as i32).wrapping_sub(4))?;
        Comment::Other
    } else {
        generic(stream, data_size)?
    };
    let read = stream.read_index() - start;
    stream.skip_fully((i64::from(limit) - 4 - read) as i32)?;
    Ok(comment)
}

/// `EmfComment.validateCommentType`: the comment's identifier, and the public type after the
/// identifier of a public comment.
fn public_type(stream: &mut Stream) -> Read<()> {
    if stream.u32()? as i32 == PUBLIC_COMMENT {
        stream.u32()?;
    }
    Ok(())
}

/// `EmfCommentDataGeneric.init`.
fn generic<'a>(stream: &mut Stream<'a>, data_size: i64) -> Read<Comment<'a>> {
    let length = safely_allocate(data_size, MAX_RECORD_LENGTH)?;
    Ok(Comment::Generic(stream.bytes(length)?))
}

/// The data of `length` bytes a single `read(byte[])` fills: as many as there are, the rest of
/// the array zero.
fn read_into<'a>(stream: &mut Stream<'a>, length: usize) -> (Cow<'a, [u8]>, i64) {
    match stream.read_some(length) {
        Some(read) if read.len() == length => (Cow::Borrowed(read), length as i64),
        Some(read) => {
            let mut data = read.to_vec();
            data.resize(length, 0);
            (Cow::Owned(data), read.len() as i64)
        }
        None => (
            Cow::Owned(vec![0; length]),
            if length == 0 { 0 } else { -1 },
        ),
    }
}

/// `EmfCommentDataMultiformats.init`.
fn multiformats<'a>(stream: &mut Stream<'a>) -> Read<Comment<'a>> {
    let start = stream.read_index();
    public_type(stream)?;
    stream.bytes(16)?;
    let count = stream.u32()? as i32;
    let mut formats = Vec::new();
    for _ in 0..count.max(0) {
        stream.i32()?;
        stream.i32()?;
        let size = stream.i32()?;
        let offset = stream.i32()?;
        if size < 0 {
            return thrown("RecordFormatException: size for emrformat must be > 0");
        }
        if offset < 0 {
            return thrown("RecordFormatException: offset for emrformat must be > 0");
        }
        formats.push((size, offset));
    }
    let mut data = vec![None; formats.len()];
    for ((size, offset), data) in formats.into_iter().zip(&mut data) {
        let skip = offset.wrapping_sub((stream.read_index() - start) as i32);
        stream.skip_fully(skip)?;
        let length = safely_allocate(i64::from(size), MAX_RECORD_LENGTH)?;
        let (read, count) = read_into(stream, length);
        *data = Some(read);
        if count < i64::from(size) {
            break;
        }
    }
    Ok(Comment::Formats(data))
}

/// `EmfCommentDataWMF.init`.
fn wmf<'a>(stream: &mut Stream<'a>) -> Read<Comment<'a>> {
    public_type(stream)?;
    stream.u16()?;
    stream.skip_fully(2)?;
    stream.i32()?;
    stream.i32()?;
    let size = stream.u32()? as i32;
    let length = safely_allocate(i64::from(size), MAX_RECORD_LENGTH)?;
    Ok(Comment::Wmf(read_into(stream, length).0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u32, data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&(8 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn reads_records_up_to_the_end_of_file_record() {
        let mut data = record(0x21, &[]);
        data.extend(record(0x0E, &[0; 12]));
        data.extend(record(0x99, &[]));
        let records = records(&data).unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn ends_quietly_where_a_record_header_is_cut_short() {
        let mut data = record(0x21, &[]);
        data.extend_from_slice(&[0x21, 0, 0]);
        assert_eq!(records(&data).unwrap().len(), 1);
    }

    #[test]
    fn fails_on_an_unknown_record_and_one_that_reads_past_its_size() {
        assert!(records(&record(0x45, &[])).is_err());
        let mut short = Vec::new();
        short.extend_from_slice(&0x25u32.to_le_bytes());
        short.extend_from_slice(&8u32.to_le_bytes());
        short.extend_from_slice(&[0; 4]);
        assert!(records(&short).is_err());
    }

    #[test]
    fn a_poly_polygon_with_an_empty_polygon_fails_as_its_path_does() {
        let mut data = vec![0; 16];
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        assert!(records(&record(0x08, &data)).is_err());
        assert!(records(&record(0x07, &data)).is_ok());
    }

    #[test]
    fn reads_the_text_of_a_text_record() {
        let mut data = Vec::new();
        for value in [1i32, 2, 3, 4] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&[0; 8]);
        data.extend_from_slice(&[0; 8]);
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&76u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&[0; 16]);
        data.extend_from_slice(b"H\0i\0");
        let data = record(0x54, &data);
        let records = records(&data).unwrap();
        let [Record::Text(text)] = records.as_slice() else {
            panic!("one text record: {records:?}");
        };
        assert!(text.unicode);
        assert_eq!(text.raw, Some(&b"H\0i\0"[..]));
        assert_eq!(text.string_length, 2);
        assert_eq!(
            text.bounds_ignored,
            Rect {
                x: 1.0,
                y: 2.0,
                width: 2.0,
                height: 2.0
            }
        );
    }
}
