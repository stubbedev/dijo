//! The encodings of a page's values, each decoded a value at a time in place: `PLAIN`, indices
//! into the column chunk's dictionary (`PLAIN_DICTIONARY`, `RLE_DICTIONARY`), RLE booleans,
//! `DELTA_BINARY_PACKED` integers, `DELTA_LENGTH_BYTE_ARRAY` and `DELTA_BYTE_ARRAY` byte arrays,
//! and `BYTE_STREAM_SPLIT` values.

use std::ops::Range;

use super::bits;
use super::footer::Encoding;
use super::levels::Hybrid;
use super::schema::Physical;
use super::Problem;

/// A value as its physical type stores it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Raw<'p> {
    Boolean(bool),
    Int32(i32),
    Int64(i64),
    Int96(&'p [u8; 12]),
    Float(f32),
    Double(f64),
    Bytes(&'p [u8]),
}

/// The bytes every value of `physical` takes: none for BOOLEAN, packed in bits, and BYTE_ARRAY,
/// of values of their own lengths.
fn width(physical: Physical) -> Option<usize> {
    match physical {
        Physical::Int32 | Physical::Float => Some(4),
        Physical::Int64 | Physical::Double => Some(8),
        Physical::Int96 => Some(12),
        Physical::Fixed(length) => Some(length),
        Physical::Boolean | Physical::ByteArray => None,
    }
}

/// The value of `physical` stored in `bytes`, which are as many as [`width`] gives.
fn fixed(bytes: &[u8], physical: Physical) -> Raw<'_> {
    match physical {
        Physical::Int32 => Raw::Int32(i32::from_le_bytes(bytes.try_into().expect("four bytes"))),
        Physical::Int64 => Raw::Int64(i64::from_le_bytes(bytes.try_into().expect("eight bytes"))),
        Physical::Int96 => Raw::Int96(bytes.try_into().expect("twelve bytes")),
        Physical::Float => Raw::Float(f32::from_le_bytes(bytes.try_into().expect("four bytes"))),
        Physical::Double => Raw::Double(f64::from_le_bytes(bytes.try_into().expect("eight bytes"))),
        Physical::Fixed(_) | Physical::Boolean | Physical::ByteArray => Raw::Bytes(bytes),
    }
}

/// `length` bytes at `at` of `data`, `at` moved past them.
fn take<'p>(data: &'p [u8], at: &mut usize, length: usize) -> Result<&'p [u8], Problem> {
    let bytes = at
        .checked_add(length)
        .and_then(|end| data.get(*at..end))
        .ok_or_else(|| {
            Problem::Malformed(format!(
                "the values end inside a value of {length} bytes at byte {at}"
            ))
        })?;
    *at += length;
    Ok(bytes)
}

/// The `PLAIN` value of `physical`, not a BOOLEAN, at `at` of `data`, `at` moved past it.
fn plain<'p>(data: &'p [u8], at: &mut usize, physical: Physical) -> Result<Raw<'p>, Problem> {
    match width(physical) {
        Some(width) => Ok(fixed(take(data, at, width)?, physical)),
        None => {
            let length = take(data, at, 4)?;
            let length = u32::from_le_bytes(length.try_into().expect("four bytes"));
            Ok(Raw::Bytes(take(data, at, length as usize)?))
        }
    }
}

/// The bit at `index` of the bits from `start` of `data`.
fn bit(data: &[u8], start: usize, index: usize) -> bool {
    data[start + index / 8] >> (index % 8) & 1 == 1
}

/// A column chunk's dictionary: its values, `PLAIN` encoded, which indices refer to.
#[derive(Debug)]
pub struct Dictionary {
    bytes: Vec<u8>,
    physical: Physical,
    count: usize,
    /// Where each BYTE_ARRAY value starts, at its length.
    starts: Vec<u32>,
}

impl Dictionary {
    /// The dictionary of `count` values of `physical` in `bytes`.
    pub fn new(bytes: Vec<u8>, physical: Physical, count: u32) -> Result<Self, Problem> {
        let count = count as usize;
        let room = match width(physical) {
            Some(width) => bytes.len() / width.max(1),
            None if physical == Physical::Boolean => bytes.len() * 8,
            None => bytes.len() / 4,
        };
        if count > room {
            return Err(Problem::Malformed(format!(
                "a dictionary page of {} bytes holds {count} {physical:?} values",
                bytes.len()
            )));
        }
        let mut starts = Vec::new();
        if physical == Physical::ByteArray {
            starts.reserve_exact(count);
            let mut at = 0;
            for _ in 0..count {
                starts.push(at as u32);
                plain(&bytes, &mut at, physical)?;
            }
        }
        Ok(Self {
            bytes,
            physical,
            count,
            starts,
        })
    }

    /// The bytes it holds.
    pub fn held(&self) -> usize {
        self.bytes.capacity() + self.starts.capacity() * 4
    }

    /// The value at `index`.
    pub fn get(&self, index: u32) -> Result<Raw<'_>, Problem> {
        let index = index as usize;
        if index >= self.count {
            return Err(Problem::Malformed(format!(
                "the dictionary index {index}, past the dictionary's {} values",
                self.count
            )));
        }
        match (width(self.physical), self.physical) {
            (Some(width), physical) => Ok(fixed(
                &self.bytes[index * width..(index + 1) * width],
                physical,
            )),
            (None, Physical::Boolean) => Ok(Raw::Boolean(bit(&self.bytes, 0, index))),
            (None, physical) => plain(&self.bytes, &mut (self.starts[index] as usize), physical),
        }
    }
}

/// `DELTA_BINARY_PACKED` integers: a header of the block size, the miniblocks a block, the
/// count and the first value, then blocks of the least delta and the bit widths of its
/// miniblocks, each of whose deltas less that least is packed in its width.
#[derive(Debug, Clone)]
pub struct Delta {
    at: usize,
    end: usize,
    block: usize,
    miniblocks: usize,
    per_miniblock: usize,
    /// The values not read yet.
    left: u64,
    first: bool,
    last: i64,
    min_delta: i64,
    /// Where the current block's bit widths are.
    widths: usize,
    miniblock: usize,
    /// Where the current miniblock's deltas are, of what width, and the next one's index.
    data: usize,
    width: u8,
    index: usize,
}

impl Delta {
    /// The integers in `range` of `page`, and where they end.
    pub fn new(page: &[u8], range: Range<usize>) -> Result<(Self, usize), Problem> {
        let data = section(page, &range)?;
        let mut at = range.start;
        let block = bits::varint(data, &mut at)?;
        let miniblocks = bits::varint(data, &mut at)?;
        let left = bits::varint(data, &mut at)?;
        let first = bits::zigzag(data, &mut at)?;
        let valid = block > 0
            && block % 128 == 0
            && miniblocks > 0
            && block % miniblocks == 0
            && (block / miniblocks) % 32 == 0;
        let (Some(block), Some(miniblocks)) = (
            usize::try_from(block).ok().filter(|_| valid),
            usize::try_from(miniblocks).ok(),
        ) else {
            return Err(Problem::Malformed(format!(
                "DELTA_BINARY_PACKED blocks of {block} values in {miniblocks} miniblocks"
            )));
        };
        let per_miniblock = block / miniblocks;
        let delta = Self {
            at,
            end: range.end,
            block,
            miniblocks,
            per_miniblock,
            left,
            first: true,
            last: first,
            min_delta: 0,
            widths: 0,
            miniblock: miniblocks - 1,
            data: 0,
            width: 0,
            index: per_miniblock,
        };
        let end = delta.end(data)?;
        Ok((delta, end))
    }

    /// Where the blocks of the values end, read from the start.
    fn end(&self, data: &[u8]) -> Result<usize, Problem> {
        let mut at = self.at;
        let mut left = self.left.saturating_sub(1);
        while left > 0 {
            bits::zigzag(data, &mut at)?;
            let widths = take(data, &mut at, self.miniblocks)?;
            let values = left.min(self.block as u64);
            let needed = values.div_ceil(self.per_miniblock as u64) as usize;
            for &width in &widths[..needed] {
                take(data, &mut at, self.miniblock_length(width)?)?;
            }
            left -= values;
        }
        Ok(at)
    }

    /// The bytes of a miniblock of deltas of `width` bits.
    fn miniblock_length(&self, width: u8) -> Result<usize, Problem> {
        if width > 64 {
            return Err(Problem::Malformed(format!(
                "DELTA_BINARY_PACKED deltas of {width} bits"
            )));
        }
        Ok(self.per_miniblock / 8 * usize::from(width))
    }

    /// The next integer, of those in `page`.
    pub fn next(&mut self, page: &[u8]) -> Result<i64, Problem> {
        if self.left == 0 {
            return Err(Problem::Malformed(
                "more values than the DELTA_BINARY_PACKED data holds".to_owned(),
            ));
        }
        self.left -= 1;
        if self.first {
            self.first = false;
            return Ok(self.last);
        }
        if self.index == self.per_miniblock {
            let data = &page[..self.end];
            self.miniblock += 1;
            if self.miniblock == self.miniblocks {
                self.min_delta = bits::zigzag(data, &mut self.at)?;
                self.widths = self.at;
                take(data, &mut self.at, self.miniblocks)?;
                self.miniblock = 0;
            }
            self.width = data[self.widths + self.miniblock];
            self.data = self.at;
            let length = self.miniblock_length(self.width)?;
            take(data, &mut self.at, length)?;
            self.index = 0;
        }
        let delta = bits::unpack(page, self.data, self.index, self.width) as i64;
        self.index += 1;
        self.last = self.last.wrapping_add(self.min_delta).wrapping_add(delta);
        Ok(self.last)
    }
}

/// `range` of `page`, which must hold it.
fn section<'p>(page: &'p [u8], range: &Range<usize>) -> Result<&'p [u8], Problem> {
    page.get(..range.end)
        .filter(|_| range.start <= range.end)
        .ok_or_else(|| {
            Problem::Malformed(format!(
                "the values at bytes {range:?} of a page of {} bytes",
                page.len()
            ))
        })
}

/// The values of a page being decoded.
#[derive(Debug)]
pub enum Values {
    Plain {
        at: usize,
        end: usize,
    },
    PlainBoolean {
        start: usize,
        index: usize,
        count: usize,
    },
    RleBoolean(Hybrid),
    Dictionary(Hybrid),
    DeltaBinaryPacked(Delta),
    DeltaLength {
        lengths: Delta,
        at: usize,
        end: usize,
    },
    DeltaBytes {
        prefixes: Delta,
        suffixes: Delta,
        at: usize,
        end: usize,
        last: Vec<u8>,
    },
    ByteStreamSplit {
        start: usize,
        width: usize,
        count: usize,
        index: usize,
        scratch: Vec<u8>,
    },
}

impl Values {
    /// The values in `range` of `page`, of a column of `physical`, encoded in `encoding`; a
    /// dictionary's indices need the column chunk to have a dictionary.
    pub fn new(
        page: &[u8],
        range: Range<usize>,
        encoding: Encoding,
        physical: Physical,
        has_dictionary: bool,
    ) -> Result<Self, Problem> {
        let data = section(page, &range)?;
        let mismatch =
            || Problem::Malformed(format!("{physical:?} values encoded in {encoding:?}"));
        Ok(match encoding {
            Encoding::Plain if physical == Physical::Boolean => Self::PlainBoolean {
                start: range.start,
                index: 0,
                count: range.len() * 8,
            },
            Encoding::Plain => Self::Plain {
                at: range.start,
                end: range.end,
            },
            Encoding::PlainDictionary | Encoding::RleDictionary => {
                if !has_dictionary {
                    return Err(Problem::Malformed(
                        "dictionary indices in a column chunk of no dictionary page".to_owned(),
                    ));
                }
                let width = *data.get(range.start).ok_or_else(|| {
                    Problem::Malformed("dictionary indices of no bit width".to_owned())
                })?;
                if width > 32 {
                    return Err(Problem::Malformed(format!(
                        "dictionary indices of {width} bits"
                    )));
                }
                Self::Dictionary(Hybrid::new(range.start + 1..range.end, width))
            }
            Encoding::Rle if physical == Physical::Boolean => {
                let mut at = range.start;
                let length = take(data, &mut at, 4)?;
                let length = u32::from_le_bytes(length.try_into().expect("four bytes")) as usize;
                take(data, &mut at, length)?;
                Self::RleBoolean(Hybrid::new(range.start + 4..at, 1))
            }
            Encoding::DeltaBinaryPacked
                if matches!(physical, Physical::Int32 | Physical::Int64) =>
            {
                Self::DeltaBinaryPacked(Delta::new(page, range)?.0)
            }
            Encoding::DeltaLengthByteArray if physical == Physical::ByteArray => {
                let (lengths, at) = Delta::new(page, range.clone())?;
                Self::DeltaLength {
                    lengths,
                    at,
                    end: range.end,
                }
            }
            Encoding::DeltaByteArray
                if matches!(physical, Physical::ByteArray | Physical::Fixed(_)) =>
            {
                let (prefixes, suffixes_at) = Delta::new(page, range.clone())?;
                let (suffixes, at) = Delta::new(page, suffixes_at..range.end)?;
                Self::DeltaBytes {
                    prefixes,
                    suffixes,
                    at,
                    end: range.end,
                    last: Vec::new(),
                }
            }
            Encoding::ByteStreamSplit => {
                let width = match physical {
                    Physical::Int96 | Physical::Boolean | Physical::ByteArray => {
                        return Err(mismatch())
                    }
                    physical => width(physical).expect("a type of values of one width"),
                };
                if width == 0 || !range.len().is_multiple_of(width) {
                    return Err(Problem::Malformed(format!(
                        "BYTE_STREAM_SPLIT values of {width} bytes in {} bytes",
                        range.len()
                    )));
                }
                Self::ByteStreamSplit {
                    start: range.start,
                    width,
                    count: range.len() / width,
                    index: 0,
                    scratch: Vec::with_capacity(width),
                }
            }
            _ => return Err(mismatch()),
        })
    }

    /// The next value, of a column of `physical`, of those in `page`, a dictionary's index
    /// looked up in `dictionary`.
    pub fn next<'p>(
        &'p mut self,
        page: &'p [u8],
        physical: Physical,
        dictionary: Option<&'p Dictionary>,
    ) -> Result<Raw<'p>, Problem> {
        let exhausted = || Problem::Malformed("the values end before the page's levels".to_owned());
        match self {
            Self::Plain { at, end } => plain(&page[..*end], at, physical),
            Self::PlainBoolean {
                start,
                index,
                count,
            } => {
                if index == count {
                    return Err(exhausted());
                }
                *index += 1;
                Ok(Raw::Boolean(bit(page, *start, *index - 1)))
            }
            Self::RleBoolean(hybrid) => match hybrid.next(page)? {
                0 => Ok(Raw::Boolean(false)),
                1 => Ok(Raw::Boolean(true)),
                other => Err(Problem::Malformed(format!("the RLE boolean {other}"))),
            },
            Self::Dictionary(indices) => {
                let index = indices.next(page)?;
                dictionary
                    .ok_or_else(|| {
                        Problem::Malformed("dictionary indices of no dictionary".to_owned())
                    })?
                    .get(index)
            }
            Self::DeltaBinaryPacked(delta) => {
                let value = delta.next(page)?;
                Ok(match physical {
                    Physical::Int32 => Raw::Int32(value as i32),
                    _ => Raw::Int64(value),
                })
            }
            Self::DeltaLength { lengths, at, end } => {
                let length = byte_length(lengths.next(page)?)?;
                Ok(Raw::Bytes(take(&page[..*end], at, length)?))
            }
            Self::DeltaBytes {
                prefixes,
                suffixes,
                at,
                end,
                last,
            } => {
                let prefix = byte_length(prefixes.next(page)?)?;
                let suffix = byte_length(suffixes.next(page)?)?;
                if prefix > last.len() {
                    return Err(Problem::Malformed(format!(
                        "a DELTA_BYTE_ARRAY prefix of {prefix} bytes of a value of {}",
                        last.len()
                    )));
                }
                let suffix = take(&page[..*end], at, suffix)?;
                last.truncate(prefix);
                last.extend_from_slice(suffix);
                Ok(Raw::Bytes(last))
            }
            Self::ByteStreamSplit {
                start,
                width,
                count,
                index,
                scratch,
            } => {
                if index == count {
                    return Err(exhausted());
                }
                scratch.clear();
                scratch.extend((0..*width).map(|stream| page[*start + stream * *count + *index]));
                *index += 1;
                Ok(fixed(scratch, physical))
            }
        }
    }
}

/// A length of a DELTA_LENGTH_BYTE_ARRAY or DELTA_BYTE_ARRAY value, which is not negative.
fn byte_length(length: i64) -> Result<usize, Problem> {
    usize::try_from(length)
        .map_err(|_| Problem::Malformed(format!("a byte array of {length} bytes")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DELTA_BINARY_PACKED header of blocks of 128 values in 4 miniblocks, of `count` values
    /// from `first`.
    fn header(count: u8, first: u8) -> Vec<u8> {
        vec![0x80, 0x01, 0x04, count, first]
    }

    fn all(values: &mut Values, page: &[u8], physical: Physical) -> Vec<String> {
        let mut read = Vec::new();
        while let Ok(raw) = values.next(page, physical, None) {
            read.push(format!("{raw:?}"));
        }
        read
    }

    #[test]
    fn decodes_delta_binary_packed_integers() {
        let mut page = header(5, 2);
        page.extend([0x02, 1, 0, 0, 0]);
        page.extend([0b0000_1010, 0, 0, 0]);
        let end = page.len();
        let (mut delta, at) = Delta::new(&page, 0..end).unwrap();
        assert_eq!(at, end);
        let read: Vec<i64> = (0..5).map(|_| delta.next(&page).unwrap()).collect();
        assert_eq!(read, [1, 2, 4, 5, 7]);
        assert!(delta.next(&page).is_err());
        assert!(Delta::new(&page[..end - 1], 0..end - 1).is_err());
        assert!(Delta::new(&[0x40, 0x04, 1, 0], 0..4).is_err());
        let mut wide = header(2, 0);
        wide.extend([0, 65, 0, 0, 0]);
        assert!(Delta::new(&wide, 0..wide.len()).is_err());
    }

    #[test]
    fn decodes_byte_arrays_by_their_lengths_and_shared_prefixes() {
        let mut lengths = header(3, 12);
        lengths.extend([0x05, 2, 0, 0, 0]);
        lengths.extend([0b0000_1100, 0, 0, 0, 0, 0, 0, 0]);
        lengths.extend(b"Skagenfyrhav");
        let mut values = Values::new(
            &lengths,
            0..lengths.len(),
            Encoding::DeltaLengthByteArray,
            Physical::ByteArray,
            false,
        )
        .unwrap();
        assert_eq!(
            all(&mut values, &lengths, Physical::ByteArray),
            [
                format!("{:?}", Raw::Bytes(b"Skagen")),
                format!("{:?}", Raw::Bytes(b"fyr")),
                format!("{:?}", Raw::Bytes(b"hav")),
            ]
        );
        let mut page = header(2, 0);
        page.extend([0x0E, 0, 0, 0, 0]);
        page.extend(header(2, 14));
        page.extend([0x0B, 0, 0, 0, 0]);
        page.extend(b"herrings");
        let mut values = Values::new(
            &page,
            0..page.len(),
            Encoding::DeltaByteArray,
            Physical::ByteArray,
            false,
        )
        .unwrap();
        assert_eq!(
            all(&mut values, &page, Physical::ByteArray),
            [
                format!("{:?}", Raw::Bytes(b"herring")),
                format!("{:?}", Raw::Bytes(b"herrings")),
            ]
        );
        let mut longer = header(2, 0);
        longer.extend([0x10, 0, 0, 0, 0]);
        longer.extend(header(2, 14));
        longer.extend([0x0B, 0, 0, 0, 0]);
        longer.extend(b"herrings");
        let mut values = Values::new(
            &longer,
            0..longer.len(),
            Encoding::DeltaByteArray,
            Physical::ByteArray,
            false,
        )
        .unwrap();
        assert!(values.next(&longer, Physical::ByteArray, None).is_ok());
        assert!(values.next(&longer, Physical::ByteArray, None).is_err());
    }

    #[test]
    fn decodes_split_streams_and_booleans() {
        let floats = [1.5f32, -0.25];
        let bytes: Vec<[u8; 4]> = floats.iter().map(|value| value.to_le_bytes()).collect();
        let page: Vec<u8> = (0..4)
            .flat_map(|k| bytes.iter().map(move |b| b[k]))
            .collect();
        let mut values = Values::new(
            &page,
            0..8,
            Encoding::ByteStreamSplit,
            Physical::Float,
            false,
        )
        .unwrap();
        assert_eq!(
            all(&mut values, &page, Physical::Float),
            ["Float(1.5)", "Float(-0.25)"]
        );
        assert!(Values::new(
            &page,
            0..7,
            Encoding::ByteStreamSplit,
            Physical::Float,
            false
        )
        .is_err());
        let page = [2, 0, 0, 0, 0x03, 0b0000_0101];
        let mut values = Values::new(&page, 0..6, Encoding::Rle, Physical::Boolean, false).unwrap();
        let read: Vec<String> = (0..3)
            .map(|_| format!("{:?}", values.next(&page, Physical::Boolean, None).unwrap()))
            .collect();
        assert_eq!(read, ["Boolean(true)", "Boolean(false)", "Boolean(true)"]);
        let mut plain =
            Values::new(&[0b10], 0..1, Encoding::Plain, Physical::Boolean, false).unwrap();
        assert_eq!(all(&mut plain, &[0b10], Physical::Boolean).len(), 8);
        assert!(Values::new(&page, 0..6, Encoding::Rle, Physical::Int32, false).is_err());
        assert!(Values::new(&page, 0..6, Encoding::RleDictionary, Physical::Int32, false).is_err());
    }

    #[test]
    fn looks_values_up_in_the_dictionary() {
        let mut bytes = Vec::new();
        for word in ["Ellen Marie", "Havørn"] {
            bytes.extend((word.len() as u32).to_le_bytes());
            bytes.extend(word.as_bytes());
        }
        let dictionary = Dictionary::new(bytes.clone(), Physical::ByteArray, 2).unwrap();
        assert_eq!(dictionary.get(1).unwrap(), Raw::Bytes("Havørn".as_bytes()));
        assert!(dictionary.get(2).is_err());
        assert!(Dictionary::new(bytes.clone(), Physical::ByteArray, 3).is_err());
        assert!(Dictionary::new(bytes, Physical::Int64, 4).is_err());
        let page = [1, 0x02, 0x00, 0x03, 0b0000_0001];
        let mut values = Values::new(
            &page,
            0..page.len(),
            Encoding::RleDictionary,
            Physical::ByteArray,
            true,
        )
        .unwrap();
        let read: Vec<String> = (0..3)
            .map(|_| {
                let raw = values.next(&page, Physical::ByteArray, Some(&dictionary));
                format!("{:?}", raw.unwrap())
            })
            .collect();
        let expected = |word: &str| format!("{:?}", Raw::Bytes(word.as_bytes()));
        assert_eq!(
            read,
            [
                expected("Ellen Marie"),
                expected("Havørn"),
                expected("Ellen Marie")
            ]
        );
    }
}
