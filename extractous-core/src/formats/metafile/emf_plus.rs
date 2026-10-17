//! The EMF+ records of an EMF comment, read as POI's `HemfPlusRecordIterator` reads them, which
//! fails the picture where POI throws on one. Nothing of them reaches Tika's output.

use crate::formats::metafile::stream::{array_length, safely_allocate, to_int_exact, Stream};
use crate::formats::poi::{thrown, Read, Thrown};

/// `HemfPlusObject.MAX_OBJECT_SIZE` and `HemfPlusImage.EmfPlusImage.MAX_OBJECT_SIZE`.
const MAX_OBJECT_SIZE: i32 = 50_000_000;
/// `HemfPlusDraw.MAX_OBJECT_SIZE` and `HemfPlusBrush.EmfPlusBrush.MAX_OBJECT_SIZE`.
const MAX_DRAW_SIZE: i32 = 1_000_000;
/// `StringUtil.MAX_RECORD_LENGTH`.
const MAX_STRING_LENGTH: i32 = 10_000_000;
/// The signature of the graphics version of an object that is not continued.
const METAFILE_SIGNATURE: i32 = 0xDBC01;

/// `HemfPlusRecordType`, by the reading POI gives each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Header,
    Eof,
    FlagOnly,
    Unimplemented,
    Object,
    FillRects,
    DrawRects,
    BrushId,
    DrawImage,
    DrawImagePoints,
    TwoInts,
    OneInt,
    Transform,
    OneFloat,
    RectF,
    DrawDriverString,
}

fn kind(id: u16) -> Option<Kind> {
    Some(match id {
        0x4001 => Kind::Header,
        0x4002 => Kind::Eof,
        0x4004 | 0x401E | 0x401F | 0x4021 | 0x4022 | 0x4023 | 0x4024 | 0x402B | 0x4031 | 0x4033
        | 0x4034 => Kind::FlagOnly,
        0x4003
        | 0x4005
        | 0x4006
        | 0x4007
        | 0x4009
        | 0x400C..=0x4012
        | 0x4016..=0x4019
        | 0x401C
        | 0x4020
        | 0x4027
        | 0x0428
        | 0x4029
        | 0x402D..=0x402F
        | 0x4035
        | 0x4037
        | 0x4038
        | 0x4039
        | 0x403A => Kind::Unimplemented,
        0x4008 => Kind::Object,
        0x400A => Kind::FillRects,
        0x400B => Kind::DrawRects,
        0x4013..=0x4015 => Kind::BrushId,
        0x401A => Kind::DrawImage,
        0x401B => Kind::DrawImagePoints,
        0x401D => Kind::TwoInts,
        0x4025 | 0x4026 => Kind::OneInt,
        0x402A | 0x402C => Kind::Transform,
        0x4030 => Kind::OneFloat,
        0x4032 => Kind::RectF,
        0x4036 => Kind::DrawDriverString,
        _ => return None,
    })
}

/// `EmfCommentDataPlus.init`'s records: `new HemfPlusRecordIterator(leis, limit)` read to its
/// end.
pub(super) fn read_records(stream: &mut Stream, limit: i32) -> Read<()> {
    let start = stream.read_index();
    let mut last = read_record(stream)?;
    while last != Kind::Eof {
        let read = stream.read_index() - start;
        if limit == -1 || read + 12 > i64::from(limit) {
            return Ok(());
        }
        last = read_record(stream)?;
    }
    Ok(())
}

/// `HemfPlusRecordIterator._next`: one record, its unread rest skipped.
fn read_record(stream: &mut Stream) -> Read<Kind> {
    let id = stream.u16()?;
    let flags = stream.u16()?;
    let record_size = stream.u32()? as i32;
    let data_size = stream.u32()? as i32;
    let kind = kind(id).ok_or_else(|| {
        Thrown::new(format!(
            "RecordFormatException: Undefined record of type:{id}"
        ))
    })?;
    let read = init(stream, kind, i64::from(data_size), flags)?;
    stream.skip_fully((i64::from(record_size) - 12 - read) as i32)?;
    Ok(kind)
}

fn init(stream: &mut Stream, kind: Kind, data_size: i64, flags: u16) -> Read<i64> {
    let compressed = flags & 0x4000 != 0;
    let rect = if compressed { 8 } else { 16 };
    Ok(match kind {
        Kind::Header => {
            stream.bytes(16)?;
            16
        }
        Kind::Eof | Kind::FlagOnly => 0,
        Kind::Unimplemented => {
            let skipped = stream.skip_up_to(data_size)?;
            if skipped < data_size {
                return thrown("IOException: End of stream reached before record read");
            }
            skipped
        }
        Kind::Object => object(stream, data_size, flags)?,
        Kind::FillRects => {
            stream.i32()?;
            let count = stream.i32()?;
            rects(stream, 8, count, rect)?
        }
        Kind::DrawRects => {
            let count = stream.i32()?;
            rects(stream, 4, count, rect)?
        }
        Kind::BrushId | Kind::OneInt | Kind::OneFloat => {
            stream.bytes(4)?;
            4
        }
        Kind::TwoInts => {
            stream.bytes(8)?;
            8
        }
        Kind::Transform => {
            stream.bytes(24)?;
            24
        }
        Kind::RectF => {
            stream.bytes(16)?;
            16
        }
        Kind::DrawImage => {
            stream.bytes(8 + 16)?;
            stream.bytes(rect)?;
            (24 + rect) as i64
        }
        Kind::DrawImagePoints => draw_image_points(stream, flags)?,
        Kind::DrawDriverString => draw_driver_string(stream)?,
    })
}

/// The `count` rectangles of `size` bytes each a record reads after its first `read` bytes: the
/// bytes the record counts, as Java's `int` sums them.
fn rects(stream: &mut Stream, read: i32, count: i32, size: usize) -> Read<i64> {
    let mut total = read;
    for _ in 0..count.max(0) {
        stream.bytes(size)?;
        total = total.wrapping_add(size as i32);
    }
    Ok(i64::from(total))
}

/// `HemfPlusDraw.readPointR`: two EMF+ integers of one or two bytes each.
fn relative_point(stream: &mut Stream) -> Read<(f64, f64, i64)> {
    let coordinate = |stream: &mut Stream| -> Read<(f64, i64)> {
        let first = stream.i8()?;
        if first as u8 & 0x80 == 0 {
            return Ok((f64::from(first), 1));
        }
        let second = stream.u8()?;
        let value = ((i32::from(first) << 8) | i32::from(second)) & 0x7FFF;
        Ok((f64::from(value), 2))
    };
    let (x, x_size) = coordinate(stream)?;
    let (y, y_size) = coordinate(stream)?;
    Ok((x, y, x_size + y_size))
}

/// A point of an EMF+ record: relative, compressed to shorts, or of floats.
fn point(stream: &mut Stream, relative: bool, compressed: bool) -> Read<(f64, f64, i64)> {
    if relative {
        return relative_point(stream);
    }
    if compressed {
        let x = f64::from(stream.i16()?);
        let y = f64::from(stream.i16()?);
        return Ok((x, y, 4));
    }
    let x = stream.f32()?;
    let y = stream.f32()?;
    Ok((x, y, 8))
}

/// `EmfPlusDrawImagePoints.init`, whose transform from the source rectangle to the
/// parallelogram fails on a singular rectangle and on values that are not finite, as
/// commons-math's `LUDecomposition` and `BigDecimal.valueOf` fail.
fn draw_image_points(stream: &mut Stream, flags: u16) -> Read<i64> {
    stream.i32()?;
    stream.i32()?;
    let x = stream.f32()?;
    let y = stream.f32()?;
    let width = stream.f32()?;
    let height = stream.f32()?;
    stream.i32()?;
    let mut size = 8 + 16 + 4;
    let relative = flags & 0x0800 != 0;
    let compressed = flags & 0x4000 != 0;
    let mut corners = [(0.0, 0.0); 3];
    for corner in &mut corners {
        let (px, py, read) = point(stream, relative, compressed)?;
        *corner = (px, py);
        size += read;
    }
    let [lower_left, lower_right, upper_left] = corners;
    let parallelogram = [
        [lower_left.0, lower_right.0, upper_left.0],
        [lower_left.1, lower_right.1, upper_left.1],
        [1.0, 1.0, 1.0],
    ];
    let (min_x, max_x, min_y, max_y) = (x, x + width, y, y + height);
    let rectangle = [
        [min_x, max_x, min_x],
        [min_y, min_y, max_y],
        [1.0, 1.0, 1.0],
    ];
    let inverse = inverse(rectangle)
        .ok_or_else(|| Thrown::new("SingularMatrixException: matrix is singular"))?;
    let product = multiply(&parallelogram, &inverse);
    let used = [
        product[0][0],
        product[1][0],
        product[0][1],
        product[1][1],
        product[0][2],
        product[1][2],
    ];
    if used.iter().any(|value| !value.is_finite()) {
        return thrown("NumberFormatException: Character N is neither a decimal digit number");
    }
    Ok(size)
}

type Matrix = [[f64; 3]; 3];

/// commons-math's `LUDecomposition(matrix).getSolver().getInverse()`, or `None` where it finds
/// the matrix singular.
fn inverse(matrix: Matrix) -> Option<Matrix> {
    const TOO_SMALL: f64 = 1e-11;
    let mut lu = matrix;
    let mut pivot = [0usize, 1, 2];
    let reduced = |lu: &Matrix, row: usize, col: usize, terms: usize| {
        (0..terms).fold(lu[row][col], |sum, i| sum - lu[row][i] * lu[i][col])
    };
    for col in 0..3 {
        for row in 0..col {
            lu[row][col] = reduced(&lu, row, col, row);
        }
        let mut max = col;
        let mut largest = f64::NEG_INFINITY;
        for row in col..3 {
            let sum = reduced(&lu, row, col, col);
            lu[row][col] = sum;
            if sum.abs() > largest {
                largest = sum.abs();
                max = row;
            }
        }
        if lu[max][col].abs() < TOO_SMALL {
            return None;
        }
        if max != col {
            lu.swap(max, col);
            pivot.swap(max, col);
        }
        let diagonal = lu[col][col];
        for row in lu.iter_mut().skip(col + 1) {
            row[col] /= diagonal;
        }
    }
    let mut solved = [[0.0; 3]; 3];
    for (row, solved_row) in solved.iter_mut().enumerate() {
        for (col, value) in solved_row.iter_mut().enumerate() {
            *value = if pivot[row] == col { 1.0 } else { 0.0 };
        }
    }
    for col in 0..3 {
        let column = solved[col];
        for i in col + 1..3 {
            let factor = lu[i][col];
            for j in 0..3 {
                solved[i][j] -= column[j] * factor;
            }
        }
    }
    for col in (0..3).rev() {
        let diagonal = lu[col][col];
        for value in &mut solved[col] {
            *value /= diagonal;
        }
        let column = solved[col];
        for i in 0..col {
            let factor = lu[i][col];
            for j in 0..3 {
                solved[i][j] -= column[j] * factor;
            }
        }
    }
    Some(solved)
}

/// `AbstractRealMatrix.multiply`.
fn multiply(left: &Matrix, right: &Matrix) -> Matrix {
    let mut product = [[0.0; 3]; 3];
    for (row, product_row) in product.iter_mut().enumerate() {
        for (col, value) in product_row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for i in 0..3 {
                sum += left[row][i] * right[i][col];
            }
            *value = sum;
        }
    }
    product
}

/// `EmfPlusDrawDriverString.init`.
fn draw_driver_string(stream: &mut Stream) -> Read<i64> {
    stream.i32()?;
    stream.i32()?;
    let matrix = stream.i32()?;
    let glyphs = stream.i32()?;
    let buffer = stream.byte_array(glyphs.wrapping_mul(2), MAX_DRAW_SIZE)?;
    let mut size = 16i32.wrapping_add(buffer.len() as i32);
    for _ in 0..glyphs.max(0) {
        stream.bytes(8)?;
        size = size.wrapping_add(8);
    }
    if matrix != 0 {
        stream.bytes(24)?;
        size = size.wrapping_add(24);
    }
    Ok(i64::from(size))
}

/// The object types of `EmfPlusObjectType`, by the reading POI gives each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectType {
    Unknown,
    Brush,
    Pen,
    Path,
    Region,
    Image,
    Font,
    ImageAttributes,
}

/// `EmfPlusObject.init`.
fn object(stream: &mut Stream, data_size: i64, flags: u16) -> Read<i64> {
    let object_type = match (flags & 0x7F00) >> 8 {
        0 | 7 | 9 => ObjectType::Unknown,
        1 => ObjectType::Brush,
        2 => ObjectType::Pen,
        3 => ObjectType::Path,
        4 => ObjectType::Region,
        5 => ObjectType::Image,
        6 => ObjectType::Font,
        8 => ObjectType::ImageAttributes,
        other => {
            return thrown(format!(
                "NullPointerException: an EMF+ object of type {other}, which POI does not know"
            ))
        }
    };
    let mut size = 0i64;
    let mut data_size = data_size as i32;
    if flags & 0x8000 != 0 {
        stream.i32()?;
        size += 4;
        data_size = data_size.wrapping_sub(4);
    }
    let data_size = i64::from(data_size);
    size += match object_type {
        ObjectType::Unknown => {
            stream.i32()?;
            stream.byte_array((data_size - 4) as i32, MAX_OBJECT_SIZE)?;
            data_size
        }
        ObjectType::Brush => brush(stream, data_size)?,
        ObjectType::Pen => pen(stream, data_size)?,
        ObjectType::Path => path(stream)?,
        ObjectType::Region => {
            stream.i32()?;
            stream.i32()?;
            8 + region_node(stream)?
        }
        ObjectType::Image => image(stream, data_size)?,
        ObjectType::Font => {
            stream.i32()?;
            stream.bytes(12)?;
            stream.skip_fully(4)?;
            let length = stream.i32()?;
            let bytes = safely_allocate(i64::from(length) * 2, MAX_STRING_LENGTH)?;
            stream.bytes(bytes)?;
            24 + i64::from(length) * 2
        }
        ObjectType::ImageAttributes => {
            stream.i32()?;
            stream.skip_fully(4)?;
            stream.bytes(12)?;
            stream.skip_fully(4)?;
            24
        }
    };
    to_int_exact(size).map(i64::from)
}

/// `EmfPlusGraphicsVersion.init` and `isContinuedRecord`: whether the object continues an
/// earlier one.
fn continued(stream: &mut Stream) -> Read<bool> {
    let value = stream.i32()?;
    let signature = (value as u32 >> 12) as i32;
    let version = value & 0xFFF;
    Ok(!(version == 1 || version == 2) || signature != METAFILE_SIGNATURE)
}

/// `EmfPlusBrush.init`.
fn brush(stream: &mut Stream, data_size: i64) -> Read<i64> {
    stream.mark();
    let mut size = 4;
    if continued(stream)? {
        stream.reset()?;
        size = 0;
    } else {
        stream.i32()?;
        size += 4;
    }
    stream.byte_array(to_int_exact(data_size - size)?, MAX_DRAW_SIZE)?;
    Ok(data_size)
}

/// What a pen's data flag adds to its data.
#[derive(Debug, Clone, Copy)]
enum PenField {
    /// A value of this many bytes.
    Fixed(usize),
    /// A count of floats, at most 1000, and the floats.
    Floats(&'static str),
    /// A custom line cap.
    CustomCap,
}

/// The fields of `EmfPlusPen`, in the order they follow its pen width, by the flag that says
/// the pen has them.
const PEN_FIELDS: [(i32, PenField); 13] = [
    (0x0001, PenField::Fixed(24)),
    (0x0002, PenField::Fixed(4)),
    (0x0004, PenField::Fixed(4)),
    (0x0008, PenField::Fixed(4)),
    (0x0010, PenField::Fixed(4)),
    (0x0020, PenField::Fixed(4)),
    (0x0040, PenField::Fixed(4)),
    (0x0080, PenField::Fixed(4)),
    (0x0100, PenField::Floats("dash")),
    (0x0200, PenField::Fixed(4)),
    (0x0400, PenField::Floats("compound line")),
    (0x0800, PenField::CustomCap),
    (0x1000, PenField::CustomCap),
];

/// `EmfPlusPen.init`.
fn pen(stream: &mut Stream, data_size: i64) -> Read<i64> {
    stream.i32()?;
    stream.i32()?;
    let pen_flags = stream.i32()?;
    stream.i32()?;
    stream.f32()?;
    let mut size = 20i64;
    for (bit, field) in PEN_FIELDS {
        if pen_flags & bit == 0 {
            continue;
        }
        size += match field {
            PenField::Fixed(length) => {
                stream.bytes(length)?;
                length as i64
            }
            PenField::Floats(what) => {
                let count = stream.i32()?;
                if !(0..=1000).contains(&count) {
                    return thrown(format!("IllegalStateException: Invalid {what} data size"));
                }
                stream.bytes(count as usize * 4)?;
                4 * (i64::from(count) + 1)
            }
            PenField::CustomCap => custom_cap(stream)?,
        };
    }
    size += brush(stream, data_size - size)?;
    Ok(size)
}

/// `EmfPlusPen.initCustomCap`.
fn custom_cap(stream: &mut Stream) -> Read<i64> {
    stream.i32()?;
    stream.i32()?;
    let adjustable = stream.i32()? != 0;
    let mut size = 12i64;
    let base = 20 + 16;
    if adjustable {
        stream.bytes(16 + base)?;
        size += (16 + base) as i64;
    } else {
        let data_flags = stream.i32()?;
        stream.bytes(8 + base)?;
        size += (12 + base) as i64;
        for bit in [0x01, 0x02] {
            if data_flags & bit != 0 {
                stream.i32()?;
                size += 4 + path(stream)?;
            }
        }
    }
    to_int_exact(size).map(i64::from)
}

/// `EmfPlusPath.init`.
fn path(stream: &mut Stream) -> Read<i64> {
    stream.i32()?;
    let count = stream.i32()?;
    let point_flags = stream.i16()?;
    stream.skip_fully(2)?;
    let mut size = 12i64;
    let relative = point_flags & 0x0800 != 0;
    let compressed = point_flags & 0x4000 != 0;
    let count = array_length(i64::from(count))?;
    for _ in 0..count {
        size += point(stream, relative, compressed)?.2;
    }
    if point_flags & 0x1000 != 0 {
        if count > 0 {
            loop {
                let run = stream.i8()? & 0x3F;
                stream.i8()?;
                if run > 0 {
                    return thrown(format!(
                        "ArrayIndexOutOfBoundsException: Array index out of range: {}",
                        count + run as usize
                    ));
                }
            }
        }
    } else {
        stream.bytes(count)?;
        size += count as i64;
    }
    let padding = ((4 - size % 4) % 4) as i32;
    stream.skip_fully(padding)?;
    Ok(size + i64::from(padding))
}

/// `HemfPlusRegion.readNode`: a region's tree of nodes, read depth first.
fn region_node(stream: &mut Stream) -> Read<i64> {
    let mut size = 0i64;
    let mut pending = 1usize;
    while pending > 0 {
        pending -= 1;
        let kind = stream.i32()?;
        size += 4;
        match kind {
            1..=5 => pending += 2,
            0x1000_0000 => {
                stream.bytes(16)?;
                size += 16;
            }
            0x1000_0001 => {
                stream.i32()?;
                size += 4 + path(stream)?;
            }
            0x1000_0002 | 0x1000_0003 => {}
            other => {
                return thrown(format!(
                    "NullPointerException: a region node of type {other}, which POI does not know"
                ))
            }
        }
    }
    Ok(size)
}

/// `EmfPlusImage.init`.
fn image(stream: &mut Stream, data_size: i64) -> Read<i64> {
    stream.mark();
    let mut size = 4i64;
    let data_type = if continued(stream)? {
        stream.reset()?;
        size = 0;
        -1
    } else {
        size += 4;
        stream.i32()?
    };
    let file_size = match data_type {
        1 => {
            stream.bytes(20)?;
            size += 20;
            (data_size - size) as i32
        }
        2 => {
            stream.bytes(8)?;
            size += 8;
            (data_size - size) as i32
        }
        _ => data_size as i32,
    };
    stream.byte_array(file_size, MAX_OBJECT_SIZE)?;
    Ok(size + i64::from(file_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u16, flags: u16, data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(&(12 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn reads_records_to_the_limit_or_the_end_of_file_record() {
        let mut data = record(
            0x4001,
            0,
            &[0x02, 0x10, 0xC0, 0xDB, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        data.extend(record(0x4002, 0, &[]));
        data.extend(record(0x4001, 0, &[]));
        let mut stream = Stream::new(&data);
        assert_eq!(read_records(&mut stream, data.len() as i32), Ok(()));
        assert_eq!(stream.read_index(), 40);
    }

    #[test]
    fn fails_on_a_record_type_poi_does_not_know() {
        let data = record(0x4028, 0, &[]);
        assert!(read_records(&mut Stream::new(&data), data.len() as i32).is_err());
    }

    #[test]
    fn inverts_as_commons_math_and_fails_on_a_singular_matrix() {
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(inverse(identity), Some(identity));
        assert_eq!(inverse([[1.0, 1.0, 1.0]; 3]), None);
        let scaled = [[2.0, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(
            inverse(scaled),
            Some([[0.5, 0.0, 0.0], [0.0, 0.25, 0.0], [0.0, 0.0, 1.0]])
        );
    }

    #[test]
    fn fails_on_a_run_length_encoded_path_with_points() {
        let mut data = vec![0x02, 0x10, 0xC0, 0xDB];
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&0x5000i16.to_le_bytes());
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0]);
        assert!(path(&mut Stream::new(&data)).is_err());
    }
}
