//! POI's `HwmfBitmapDib.init`: a device-independent bitmap of a metafile record read as far as
//! POI reads it, which fails where POI throws on its header or colors.

use crate::formats::metafile::stream::{array_length, safely_allocate, Stream};
use crate::formats::poi::{thrown, Read, Thrown};

/// `HwmfPicture.MAX_RECORD_LENGTH`.
pub(super) const MAX_RECORD_LENGTH: i32 = 100_000_000;

/// `BitCount`, by the bits per pixel it is read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitCount {
    Zero,
    One,
    Four,
    Eight,
    Sixteen,
    TwentyFour,
    ThirtyTwo,
}

impl BitCount {
    fn from_flag(flag: u16) -> Option<Self> {
        Some(match flag {
            0 => Self::Zero,
            1 => Self::One,
            4 => Self::Four,
            8 => Self::Eight,
            16 => Self::Sixteen,
            24 => Self::TwentyFour,
            32 => Self::ThirtyTwo,
            _ => return None,
        })
    }

    fn flag(self) -> i32 {
        match self {
            Self::Zero => 0,
            Self::One => 1,
            Self::Four => 4,
            Self::Eight => 8,
            Self::Sixteen => 16,
            Self::TwentyFour => 24,
            Self::ThirtyTwo => 32,
        }
    }
}

/// `Compression`, the ones POI's reading tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    Rgb,
    Bitfields,
    Cmyk,
    Other,
}

impl Compression {
    fn from_flag(flag: u32) -> Option<Self> {
        Some(match flag {
            0x0000 => Self::Rgb,
            0x0003 => Self::Bitfields,
            0x000B => Self::Cmyk,
            0x0001 | 0x0002 | 0x0004 | 0x0005 | 0x000C | 0x000D => Self::Other,
            _ => return None,
        })
    }
}

/// What `readHeader` reads of a bitmap's header.
struct Header {
    size: i32,
    width: i32,
    height: i32,
    planes: i32,
    bit_count: Option<BitCount>,
    compression: Option<Compression>,
    colors_used: i64,
}

fn read_header(stream: &mut Stream) -> Read<(Header, i32)> {
    let size = stream.i32()?;
    if size == 0x0C {
        let width = i32::from(stream.u16()?);
        let height = i32::from(stream.u16()?);
        let planes = i32::from(stream.u16()?);
        let bit_count = BitCount::from_flag(stream.u16()?);
        let header = Header {
            size,
            width,
            height,
            planes,
            bit_count,
            compression: None,
            colors_used: -1,
        };
        return Ok((header, 12));
    }
    let width = stream.i32()?;
    let height = stream.i32()?;
    let planes = i32::from(stream.u16()?);
    let bit_count = BitCount::from_flag(stream.u16()?);
    let compression = Compression::from_flag(stream.u32()?);
    stream.u32()?;
    stream.i32()?;
    stream.i32()?;
    let colors_used = i64::from(stream.u32()?);
    stream.u32()?;
    let header = Header {
        size: 40,
        width,
        height,
        planes,
        bit_count,
        compression,
        colors_used,
    };
    Ok((header, 40))
}

/// `readColors`: the bytes of the color table or masks.
fn read_colors(stream: &mut Stream, header: &Header) -> Read<i32> {
    let bit_count = header.bit_count.ok_or_else(|| {
        Thrown::new("NullPointerException: a bitmap of a bit count POI does not know")
    })?;
    let quads = |stream: &mut Stream, limit: i64| -> Read<i32> {
        let count = if header.colors_used == 0 {
            limit
        } else {
            header.colors_used.min(limit)
        };
        let count = array_length(count)?;
        stream.bytes(count * 4)?;
        Ok(count as i32 * 4)
    };
    let masks = |stream: &mut Stream| -> Read<i32> {
        match header.compression {
            Some(Compression::Rgb) => Ok(0),
            Some(Compression::Bitfields) => {
                stream.bytes(12)?;
                Ok(12)
            }
            Some(_) => thrown("IOException: Invalid compression option for the bit count"),
            None => thrown("NullPointerException: a bitmap of a compression POI does not know"),
        }
    };
    match bit_count {
        BitCount::Zero => Ok(0),
        BitCount::One => quads(stream, 2),
        BitCount::Four => quads(stream, 16),
        BitCount::Eight => quads(stream, 256),
        BitCount::Sixteen | BitCount::TwentyFour | BitCount::ThirtyTwo => masks(stream),
    }
}

/// `HwmfBitmapDib.init(stream, recordSize)`: the bytes the bitmap's reading counts.
pub(super) fn init(stream: &mut Stream, record_size: i32) -> Read<i32> {
    stream.mark();
    let (header, header_size) = read_header(stream)?;
    let intro_size = header_size + read_colors(stream, &header)?;
    stream.reset()?;
    let bit_count = header
        .bit_count
        .expect("read_colors fails without a bit count");
    let row = header
        .width
        .wrapping_mul(header.planes)
        .wrapping_mul(bit_count.flag())
        .wrapping_add(31)
        & !31;
    let body_size = (row / 8).wrapping_mul(header.height.wrapping_abs());
    let whole = header.size == 0x0C
        || matches!(
            header.compression,
            Some(Compression::Rgb | Compression::Bitfields | Compression::Cmyk)
        );
    if !whole {
        let length = safely_allocate(i64::from(record_size), MAX_RECORD_LENGTH)?;
        stream.bytes(length)?;
        return Ok(record_size);
    }
    let file_size = intro_size.wrapping_add(body_size).min(record_size);
    let length = safely_allocate(i64::from(file_size), MAX_RECORD_LENGTH)?;
    if intro_size as usize > length {
        return thrown(format!(
            "IndexOutOfBoundsException: Range [0, 0 + {intro_size}) out of bounds for length \
             {length}"
        ));
    }
    stream.bytes(intro_size as usize)?;
    stream.skip_fully(record_size - file_size)?;
    let read = match stream.read_some(length - intro_size as usize) {
        Some(read) => read.len() as i32,
        None => -1,
    };
    Ok(intro_size + (record_size - file_size) + read)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_header(bit_count: u16, compression: u32, colors_used: u32) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&40i32.to_le_bytes());
        header.extend_from_slice(&2i32.to_le_bytes());
        header.extend_from_slice(&2i32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&bit_count.to_le_bytes());
        header.extend_from_slice(&compression.to_le_bytes());
        header.extend_from_slice(&[0; 12]);
        header.extend_from_slice(&colors_used.to_le_bytes());
        header.extend_from_slice(&[0; 4]);
        header
    }

    #[test]
    fn reads_a_bitmap_as_far_as_its_record_allows() {
        let mut data = info_header(24, 0, 0);
        data.extend_from_slice(&[0; 16]);
        let mut stream = Stream::new(&data);
        assert_eq!(init(&mut stream, 56), Ok(56));
        assert_eq!(stream.read_index(), 56);
        assert!(init(&mut Stream::new(&data), 100).is_err());
        let mut stream = Stream::new(&data);
        assert_eq!(init(&mut stream, 50), Ok(50));
        assert_eq!(stream.read_index(), 50);
    }

    #[test]
    fn fails_where_poi_throws_on_the_header() {
        let unknown = info_header(3, 0, 0);
        assert!(init(&mut Stream::new(&unknown), 40).is_err());
        let compressed = info_header(24, 1, 0);
        assert!(init(&mut Stream::new(&compressed), 40).is_err());
        let mut core = Vec::new();
        core.extend_from_slice(&12i32.to_le_bytes());
        core.extend_from_slice(&[2, 0, 2, 0, 1, 0, 8, 0]);
        core.extend_from_slice(&[0; 64]);
        assert!(init(&mut Stream::new(&core), 70).is_err());
    }
}
