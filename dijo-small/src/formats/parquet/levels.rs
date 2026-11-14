//! The RLE/bit-packing hybrid Parquet encodes repetition and definition levels, dictionary
//! indices and RLE booleans in: runs of a repeated value and runs of values packed in bits,
//! each headed by a varint. The deprecated `BIT_PACKED` encoding of levels packs them most
//! significant bit first, without runs. Both are decoded a value at a time, in place.

use std::ops::Range;

use super::bits;
use super::footer::Encoding;
use super::Problem;

/// The run being decoded.
#[derive(Debug, Clone, Copy)]
enum Run {
    Repeated {
        value: u32,
        left: u64,
    },
    Packed {
        start: usize,
        index: usize,
        count: usize,
    },
}

/// Values of the RLE/bit-packing hybrid in a range of a page.
#[derive(Debug, Clone)]
pub struct Hybrid {
    at: usize,
    end: usize,
    width: u8,
    run: Run,
}

impl Hybrid {
    /// The values encoded in `range` of a page, each of `width` bits, at most 32.
    pub fn new(range: Range<usize>, width: u8) -> Self {
        Self {
            at: range.start,
            end: range.end,
            width,
            run: Run::Repeated { value: 0, left: 0 },
        }
    }

    /// The next value, of those in `page`.
    pub fn next(&mut self, page: &[u8]) -> Result<u32, Problem> {
        if self.width == 0 {
            return Ok(0);
        }
        loop {
            if let Some(value) = self.take(page) {
                return Ok(value);
            }
            self.start_run(page)?;
        }
    }

    /// The next value of the run, if it has one left.
    fn take(&mut self, page: &[u8]) -> Option<u32> {
        match &mut self.run {
            Run::Repeated { value, left } if *left > 0 => {
                *left -= 1;
                Some(*value)
            }
            Run::Packed {
                start,
                index,
                count,
            } if *index < *count => {
                let value = bits::unpack(page, *start, *index, self.width) as u32;
                *index += 1;
                Some(value)
            }
            Run::Repeated { .. } | Run::Packed { .. } => None,
        }
    }

    fn start_run(&mut self, page: &[u8]) -> Result<(), Problem> {
        let section = page
            .get(..self.end)
            .filter(|_| self.at < self.end)
            .ok_or_else(|| {
                Problem::Malformed(
                    "the RLE/bit-packed data ends before the values the page holds".to_owned(),
                )
            })?;
        let header = bits::varint(section, &mut self.at)?;
        let count = header >> 1;
        if header & 1 == 0 {
            let length = usize::from(self.width).div_ceil(8);
            let bytes = section.get(self.at..self.at + length).ok_or_else(|| {
                Problem::Malformed(format!(
                    "the RLE/bit-packed data ends inside a run's value at byte {}",
                    self.at
                ))
            })?;
            let mut value = [0u8; 4];
            value[..length].copy_from_slice(bytes);
            self.at += length;
            self.run = Run::Repeated {
                value: u32::from_le_bytes(value),
                left: count,
            };
            return Ok(());
        }
        let width = u128::from(self.width);
        let available = (self.end - self.at) as u128;
        let bytes = (u128::from(count) * width).min(available);
        let values = (u128::from(count) * 8).min(bytes * 8 / width);
        self.run = Run::Packed {
            start: self.at,
            index: 0,
            count: values as usize,
        };
        self.at += bytes as usize;
        Ok(())
    }
}

/// Levels packed most significant bit first, as the deprecated `BIT_PACKED` encoding packs them.
#[derive(Debug, Clone)]
pub struct MsbPacked {
    start: usize,
    index: usize,
    count: usize,
    width: u8,
}

/// The levels of a data page.
#[derive(Debug, Clone)]
pub enum Levels {
    Hybrid(Hybrid),
    MsbPacked(MsbPacked),
}

impl Levels {
    /// The levels of up to `maximum` of a data page of version 1 at `at` of `page`, `count` of
    /// them encoded in `encoding`: `at` is moved past them.
    pub fn v1(
        page: &[u8],
        at: &mut usize,
        encoding: Encoding,
        maximum: u32,
        count: u32,
    ) -> Result<Self, Problem> {
        let width = bits::width(maximum);
        let ends = || Problem::Malformed(format!("the page ends inside its levels at byte {at}"));
        match encoding {
            Encoding::Rle => {
                let length = page.get(*at..*at + 4).ok_or_else(ends)?;
                let length = u32::from_le_bytes(length.try_into().expect("four bytes")) as usize;
                let start = *at + 4;
                let end = start
                    .checked_add(length)
                    .filter(|&end| end <= page.len())
                    .ok_or_else(ends)?;
                *at = end;
                Ok(Self::Hybrid(Hybrid::new(start..end, width)))
            }
            Encoding::BitPacked => {
                let length = (u64::from(count) * u64::from(width)).div_ceil(8) as usize;
                let start = *at;
                let end = start
                    .checked_add(length)
                    .filter(|&end| end <= page.len())
                    .ok_or_else(ends)?;
                *at = end;
                Ok(Self::MsbPacked(MsbPacked {
                    start,
                    index: 0,
                    count: count as usize,
                    width,
                }))
            }
            encoding => Err(Problem::Malformed(format!(
                "levels encoded in {encoding:?}, which encodes no levels"
            ))),
        }
    }

    /// The levels of up to `maximum` of a data page of version 2, in `range` of the page.
    pub fn v2(range: Range<usize>, maximum: u32) -> Self {
        Self::Hybrid(Hybrid::new(range, bits::width(maximum)))
    }

    /// The next level, of those in `page`, which is at most `maximum`.
    pub fn next(&mut self, page: &[u8], maximum: u32) -> Result<u32, Problem> {
        let level = match self {
            Self::Hybrid(hybrid) => hybrid.next(page)?,
            Self::MsbPacked(packed) => {
                if packed.index == packed.count {
                    return Err(Problem::Malformed(
                        "the bit-packed levels end before the values the page holds".to_owned(),
                    ));
                }
                let level = bits::unpack_msb_first(page, packed.start, packed.index, packed.width);
                packed.index += 1;
                level
            }
        };
        if level > maximum {
            return Err(Problem::Malformed(format!(
                "the level {level}, more than the column's {maximum}"
            )));
        }
        Ok(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(data: &[u8], width: u8, count: usize) -> Result<Vec<u32>, Problem> {
        let mut hybrid = Hybrid::new(0..data.len(), width);
        (0..count).map(|_| hybrid.next(data)).collect()
    }

    #[test]
    fn decodes_repeated_and_packed_runs() {
        let data = [0x06, 0x02, 0x03, 0x88, 0xC6, 0xFA];
        assert_eq!(
            decoded(&data, 3, 11).unwrap(),
            [2, 2, 2, 0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert!(decoded(&data, 3, 12).is_err());
        assert_eq!(decoded(&[], 0, 3).unwrap(), [0, 0, 0]);
    }

    #[test]
    fn decodes_a_cut_packed_run_as_far_as_its_bytes_go() {
        let data = [0x05, 0xFF];
        assert_eq!(decoded(&data, 4, 2).unwrap(), [15, 15]);
        assert!(decoded(&data, 4, 3).is_err());
    }

    #[test]
    fn reads_levels_headed_by_their_length_or_packed_most_significant_bit_first() {
        let page = [2, 0, 0, 0, 0x04, 0x01, 0b0110_0000, 0xAA];
        let mut at = 0;
        let mut levels = Levels::v1(&page, &mut at, Encoding::Rle, 1, 2).unwrap();
        assert_eq!(at, 6);
        assert_eq!(levels.next(&page, 1).unwrap(), 1);
        assert_eq!(levels.next(&page, 1).unwrap(), 1);
        let mut packed = Levels::v1(&page, &mut at, Encoding::BitPacked, 3, 3).unwrap();
        assert_eq!(at, 7);
        assert_eq!(packed.next(&page, 3).unwrap(), 1);
        assert_eq!(packed.next(&page, 3).unwrap(), 2);
        assert_eq!(packed.next(&page, 3).unwrap(), 0);
        assert!(packed.next(&page, 3).is_err());
        let mut high = Levels::v2(4..6, 1);
        assert!(high.next(&[0, 0, 0, 0, 0x02, 0x03], 1).is_err());
        assert!(Levels::v1(&page[..3], &mut 0, Encoding::Rle, 1, 2).is_err());
        assert!(Levels::v1(&[9, 0, 0, 0, 1], &mut 0, Encoding::Rle, 1, 2).is_err());
        assert!(Levels::v1(&page, &mut 0, Encoding::Plain, 1, 2).is_err());
    }
}
