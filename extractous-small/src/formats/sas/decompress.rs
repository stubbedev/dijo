//! parso's decompressors of the rows of a compressed SAS7BDAT file: `CharDecompressor`, the run
//! length encoding `SASYZCRL` names, and `BinDecompressor`, the Ross Data Compression `SASYZCR2`
//! names, failing where their array accesses throw.

use crate::formats::thrown::{Exception, Thrown};

/// The compression a file's rows are in, by the name its first text block gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Compression {
    Char,
    Binary,
}

impl Compression {
    /// `LITERALS_TO_DECOMPRESSOR`: the compression `name` is the literal of, if any.
    pub(super) fn named(name: &str) -> Option<Self> {
        match name {
            "SASYZCRL" => Some(Self::Char),
            "SASYZCR2" => Some(Self::Binary),
            _ => None,
        }
    }

    /// `decompressRow`: the row of `result_length` bytes that the `length` bytes at `offset` of
    /// `page` hold.
    pub(super) fn decompress(
        self,
        page: &[u8],
        offset: i32,
        length: i32,
        result_length: i32,
    ) -> Result<Vec<u8>, Thrown> {
        let result = usize::try_from(result_length).map_err(|_| {
            Thrown::Runtime(Exception::new(
                "java.lang.NegativeArraySizeException",
                result_length.to_string(),
            ))
        })?;
        let mut row = Row {
            bytes: vec![0; result],
        };
        match self {
            Self::Char => char_row(page, i64::from(offset), i64::from(length), &mut row)?,
            Self::Binary => binary_row(page, i64::from(offset), i64::from(length), &mut row)?,
        }
        Ok(row.bytes)
    }
}

fn out_of_bounds(index: i64, length: usize) -> Thrown {
    Thrown::Runtime(Exception::new(
        "java.lang.ArrayIndexOutOfBoundsException",
        format!("Index {index} out of bounds for length {length}"),
    ))
}

/// The byte of `bytes` at `index`, which Java's array access throws on outside it.
fn at(bytes: &[u8], index: i64) -> Result<u8, Thrown> {
    usize::try_from(index)
        .ok()
        .and_then(|index| bytes.get(index).copied())
        .ok_or_else(|| out_of_bounds(index, bytes.len()))
}

/// `System.arraycopy`'s range of `length` bytes from `start` of an array of `size`, which it
/// throws on, copying nothing, where it does not lie within the array.
fn range(start: i64, length: i64, size: usize) -> Result<std::ops::Range<usize>, Thrown> {
    let end = start + length;
    if start < 0 || length < 0 || end > i64::try_from(size).expect("an array's length fits") {
        return Err(Thrown::Runtime(Exception::new(
            "java.lang.ArrayIndexOutOfBoundsException",
            format!("arraycopy: last source index {end} out of bounds for length {size}"),
        )));
    }
    let start = usize::try_from(start).expect("not negative");
    Ok(start..start + usize::try_from(length).expect("not negative"))
}

/// The row being decompressed.
struct Row {
    bytes: Vec<u8>,
}

impl Row {
    fn set(&mut self, index: i64, value: u8) -> Result<(), Thrown> {
        let length = self.bytes.len();
        let slot = usize::try_from(index)
            .ok()
            .and_then(|index| self.bytes.get_mut(index))
            .ok_or_else(|| out_of_bounds(index, length))?;
        *slot = value;
        Ok(())
    }

    /// `count` bytes of `value` from `start`.
    fn fill(&mut self, start: i64, count: i64, value: u8) -> Result<(), Thrown> {
        for index in start..start + count {
            self.set(index, value)?;
        }
        Ok(())
    }
}

/// `CharDecompressor.decompressRow`, its copies of literal runs short of the bytes they take, as
/// parso's are.
fn char_row(page: &[u8], offset: i64, length: i64, row: &mut Row) -> Result<(), Thrown> {
    let mut index = 0i64;
    let mut written = 0i64;
    while index < length {
        let first = at(page, offset + index)?;
        let control = first & 0xF0;
        let low = i64::from(first & 0x0F);
        match control {
            0x00..=0x30 => {
                if index != length - 1 {
                    let count = i64::from(at(page, offset + index + 1)?)
                        + 64
                        + i64::from(first.cast_signed()) * 256;
                    let source = range(offset + index + 2, count, page.len())?;
                    let target = range(written, count, row.bytes.len())?;
                    row.bytes[target].copy_from_slice(&page[source]);
                    index += count + 1;
                    written += count;
                }
            }
            0x40 => {
                let count = low * 16 + i64::from(at(page, offset + index + 1)?) + 18;
                for _ in 0..count {
                    let value = at(page, offset + index + 2)?;
                    row.set(written, value)?;
                    written += 1;
                }
                index += 2;
            }
            0x50 | 0x60 | 0x70 => {
                let count = low * 256 + i64::from(at(page, offset + index + 1)?) + 17;
                let value = match control {
                    0x50 => 0x40,
                    0x60 => 0x20,
                    _ => 0x00,
                };
                row.fill(written, count, value)?;
                written += count;
                index += 1;
            }
            0x80..=0xB0 => {
                let count = (low + 1 + i64::from(control - 0x80)).min(length - (index + 1));
                let source = range(offset + index + 1, count, page.len())?;
                let target = range(written, count, row.bytes.len())?;
                row.bytes[target].copy_from_slice(&page[source]);
                index += count;
                written += count;
            }
            0xC0 => {
                for _ in 0..low + 3 {
                    let value = at(page, offset + index + 1)?;
                    row.set(written, value)?;
                    written += 1;
                }
                index += 1;
            }
            _ => {
                let value = match control {
                    0xD0 => 0x40,
                    0xE0 => 0x20,
                    _ => 0x00,
                };
                row.fill(written, low + 2, value)?;
                written += low + 2;
            }
        }
        index += 1;
    }
    Ok(())
}

/// `BinDecompressor.decompressRow`: the `length` bytes from `offset` of `page`, zeros past its
/// end, as `Arrays.copyOfRange` takes them, then their control words, literals, runs and copies
/// of what was written before.
fn binary_row(page: &[u8], offset: i64, length: i64, row: &mut Row) -> Result<(), Thrown> {
    let source = copy_of_range(page, offset, offset + length)?;
    let mut read = 0i64;
    let mut written = 0i64;
    let (mut bits, mut mask) = (0u32, 0u32);
    while read < length {
        mask >>= 1;
        if mask == 0 {
            bits = u32::from(at(&source, read)?) << 8 | u32::from(at(&source, read + 1)?);
            read += 2;
            mask = 0x8000;
        }
        if bits & mask == 0 {
            row.set(written, at(&source, read)?)?;
            written += 1;
            read += 1;
            continue;
        }
        let first = at(&source, read)?;
        let command = i64::from(first >> 4);
        let mut count = i64::from(first & 0x0F);
        read += 1;
        match command {
            0 | 1 => {
                if command == 1 {
                    count += i64::from(at(&source, read)?) << 4;
                    read += 1;
                    count += 19;
                } else {
                    count += 3;
                }
                let value = at(&source, read)?;
                row.fill(written, count, value)?;
                read += 1;
                written += count;
            }
            _ => {
                let distance = count + 3 + (i64::from(at(&source, read)?) << 4);
                read += 1;
                let copied = if command == 2 {
                    let copied = i64::from(at(&source, read)?) + 16;
                    read += 1;
                    copied
                } else {
                    command
                };
                let from = range(written - distance, copied, row.bytes.len())?;
                let to = range(written, copied, row.bytes.len())?;
                row.bytes.copy_within(from, to.start);
                written += copied;
            }
        }
    }
    Ok(())
}

/// `Arrays.copyOfRange(bytes, from, to)`: zeros past the end of `bytes`, throwing where `from`
/// lies outside it or after `to`.
pub(super) fn copy_of_range(bytes: &[u8], from: i64, to: i64) -> Result<Vec<u8>, Thrown> {
    if from > to {
        return Err(Thrown::Runtime(Exception::new(
            "java.lang.IllegalArgumentException",
            format!("{from} > {to}"),
        )));
    }
    let start = usize::try_from(from)
        .ok()
        .filter(|&start| start <= bytes.len())
        .ok_or_else(|| out_of_bounds(from, bytes.len()))?;
    let length = usize::try_from(to - from).expect("not negative");
    let mut copy = vec![0; length];
    let available = (bytes.len() - start).min(length);
    copy[..available].copy_from_slice(&bytes[start..start + available]);
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompresses_runs_as_parso_does() {
        let page = [0x82, b'a', b'b', b'c', 0xC1, b'x', 0xD0, 0xF1];
        let row = Compression::Char.decompress(&page, 0, 8, 12).unwrap();
        assert_eq!(row, b"abcxxxx@@\0\0\0");
        let page = [0x00, 0x00, b'h', b'e', b'j'];
        assert!(Compression::Char.decompress(&page, 0, 5, 64).is_err());
        assert!(Compression::Char.decompress(&page, 0, 5, -1).is_err());
    }

    #[test]
    fn decompresses_ross_data_compression_as_parso_does() {
        let page = [0x88, 0x00, 0x00, b'a', b'z', b'b', b'c', 0x33, 0x00];
        let row = Compression::Binary.decompress(&page, 0, 9, 10).unwrap();
        assert_eq!(row, b"aaazbcaaa\0");
        assert_eq!(copy_of_range(b"abc", 1, 5).unwrap(), b"bc\0\0");
        assert!(copy_of_range(b"abc", 4, 5).is_err());
    }
}
