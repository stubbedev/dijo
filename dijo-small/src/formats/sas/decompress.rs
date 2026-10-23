//! The decompressors of the rows of a compressed SAS7BDAT file: the run-length encoding
//! `SASYZCRL` names, as ReadStat reads it, and parso's `BinDecompressor`, the Ross Data
//! Compression `SASYZCR2` names, failing where their array accesses throw.

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

    /// `count` bytes of `value` from `start`, failing at the first index outside the row.
    fn fill(&mut self, start: i64, count: i64, value: u8) -> Result<(), Thrown> {
        if count <= 0 {
            return Ok(());
        }
        let length = self.bytes.len();
        let size = i64::try_from(length).expect("an array's length fits");
        let end = start + count;
        if start < 0 {
            return Err(out_of_bounds(start, length));
        }
        if end > size {
            return Err(out_of_bounds(start.max(size), length));
        }
        let start = usize::try_from(start).expect("not negative");
        let end = usize::try_from(end).expect("not negative");
        self.bytes[start..end].fill(value);
        Ok(())
    }
}

/// The run-length decompression of `SASYZCRL`, by the commands ReadStat reads, where parso's
/// `CharDecompressor` misreads the lengths of two of them (#155): a copy of 96 or more literal
/// bytes, which it reads as a copy of a length in the next byte, and an insertion of 18 or more
/// of a byte, whose high length bits it counts sixteen rather than 256. A command that runs past
/// the compressed row or the row, or that no SAS file holds, fails it.
fn char_row(page: &[u8], offset: i64, length: i64, row: &mut Row) -> Result<(), Thrown> {
    let end = offset + length;
    let mut read = offset;
    let mut written = 0i64;
    while read < end {
        let control = at(page, read)?;
        read += 1;
        let low = i64::from(control & 0x0F);
        let mut next = || -> Result<u8, Thrown> {
            if read >= end {
                return Err(run_past_row(control));
            }
            let byte = at(page, read)?;
            read += 1;
            Ok(byte)
        };
        let (copied, inserted) = match control >> 4 {
            0x0 => (i64::from(next()?) + 64 + low * 256, None),
            0x1 => (i64::from(next()?) + 64 + low * 256 + 4096, None),
            0x2 => (low + 96, None),
            0x4 => {
                let count = i64::from(next()?) + 18 + low * 256;
                (0, Some((count, next()?)))
            }
            0x5 => (0, Some((i64::from(next()?) + 17 + low * 256, b'@'))),
            0x6 => (0, Some((i64::from(next()?) + 17 + low * 256, b' '))),
            0x7 => (0, Some((i64::from(next()?) + 17 + low * 256, 0))),
            0x8 => (low + 1, None),
            0x9 => (low + 17, None),
            0xA => (low + 33, None),
            0xB => (low + 49, None),
            0xC => (0, Some((low + 3, next()?))),
            0xD => (0, Some((low + 2, b'@'))),
            0xE => (0, Some((low + 2, b' '))),
            0xF => (0, Some((low + 2, 0))),
            _ => {
                return Err(Thrown::Io(Exception::new(
                    "java.io.IOException",
                    format!("Unknown run-length command {control:#04x} in a compressed row"),
                )))
            }
        };
        if copied > 0 {
            if read + copied > end {
                return Err(run_past_row(control));
            }
            let source = range(read, copied, page.len())?;
            let target = range(written, copied, row.bytes.len())?;
            row.bytes[target].copy_from_slice(&page[source]);
            read += copied;
            written += copied;
        }
        if let Some((count, value)) = inserted {
            row.fill(written, count, value)?;
            written += count;
        }
    }
    Ok(())
}

/// The failure of a run-length command `control` that reads past the compressed row.
fn run_past_row(control: u8) -> Thrown {
    Thrown::Io(Exception::new(
        "java.io.IOException",
        format!("The run-length command {control:#04x} runs past the end of its compressed row"),
    ))
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
    let (start, length) = checked_range(bytes, from, to)?;
    let mut copy = vec![0; length];
    let available = (bytes.len() - start).min(length);
    copy[..available].copy_from_slice(&bytes[start..start + available]);
    Ok(copy)
}

/// The first `N` bytes of [`copy_of_range`]'s copy, zeros past where it ends too, and how many
/// of them it holds, without the copy; it throws as `copy_of_range` does.
pub(super) fn head_of_range<const N: usize>(
    bytes: &[u8],
    from: i64,
    to: i64,
) -> Result<([u8; N], usize), Thrown> {
    let (start, length) = checked_range(bytes, from, to)?;
    let kept = length.min(N);
    let mut head = [0; N];
    let available = (bytes.len() - start).min(kept);
    head[..available].copy_from_slice(&bytes[start..start + available]);
    Ok((head, kept))
}

/// The start and length of `Arrays.copyOfRange(bytes, from, to)`, or what it throws.
fn checked_range(bytes: &[u8], from: i64, to: i64) -> Result<(usize, usize), Thrown> {
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
    Ok((start, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompresses_runs() {
        let page = [0x82, b'a', b'b', b'c', 0xC1, b'x', 0xD0, 0xF1];
        let row = Compression::Char.decompress(&page, 0, 8, 12).unwrap();
        assert_eq!(row, b"abcxxxx@@\0\0\0");
        let page = [0x00, 0x00, b'h', b'e', b'j'];
        assert!(Compression::Char.decompress(&page, 0, 5, 64).is_err());
        assert!(Compression::Char.decompress(&page, 0, 5, -1).is_err());
        assert!(Compression::Char
            .decompress(&[0x83, b'a'], 0, 2, 4)
            .is_err());
        assert!(Compression::Char.decompress(&[0x30], 0, 1, 4).is_err());
    }

    #[test]
    fn decompresses_the_runs_parso_miscounts() {
        let mut page = vec![0x21];
        page.extend((0..97u8).map(|byte| byte % 26 + b'a'));
        page.extend([0x41, 0x02, b'z']);
        let length = i32::try_from(page.len()).unwrap();
        let row = Compression::Char
            .decompress(&page, 0, length, 97 + 276)
            .unwrap();
        assert_eq!(row[..97], page[1..98]);
        assert!(row[97..].iter().all(|&byte| byte == b'z'));
    }

    #[test]
    fn decompresses_ross_data_compression_as_parso_does() {
        let page = [0x88, 0x00, 0x00, b'a', b'z', b'b', b'c', 0x33, 0x00];
        let row = Compression::Binary.decompress(&page, 0, 9, 10).unwrap();
        assert_eq!(row, b"aaazbcaaa\0");
        assert_eq!(copy_of_range(b"abc", 1, 5).unwrap(), b"bc\0\0");
        assert!(copy_of_range(b"abc", 4, 5).is_err());
    }

    #[test]
    fn reads_heads_of_ranges_as_their_copies_begin() {
        for length in 0..12 {
            let bytes = (1..=length).collect::<Vec<u8>>();
            for from in -3..16 {
                for to in -3..24 {
                    let copy = copy_of_range(&bytes, from, to);
                    let head = head_of_range::<8>(&bytes, from, to);
                    match (copy, head) {
                        (Ok(copy), Ok((head, kept))) => {
                            assert_eq!(kept, copy.len().min(8), "{length} {from} {to}");
                            assert_eq!(head[..kept], copy[..kept], "{length} {from} {to}");
                            assert!(head[kept..].iter().all(|&byte| byte == 0));
                        }
                        (copy, head) => {
                            assert_eq!(copy.err(), head.err(), "{length} {from} {to}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fills_runs_as_byte_by_byte_writes_do() {
        for length in 0..5 {
            for start in -3..8 {
                for count in -2..8 {
                    let mut filled = Row {
                        bytes: vec![1; length],
                    };
                    let mut written = Row {
                        bytes: vec![1; length],
                    };
                    let fill = filled.fill(start, count, 7);
                    let set = (start..start + count).try_for_each(|index| written.set(index, 7));
                    assert_eq!(fill, set, "{length} {start} {count}");
                    if set.is_ok() {
                        assert_eq!(filled.bytes, written.bytes, "{length} {start} {count}");
                    }
                }
            }
        }
    }
}
