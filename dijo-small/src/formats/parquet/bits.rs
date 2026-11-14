//! The bit-level encodings Parquet's levels, indices and values share: ULEB128 varints and their
//! zigzag form, and values packed into bits.

use super::Problem;

/// The longest a varint of 64 bits is.
const MAX_VARINT: usize = 10;

/// The ULEB128 varint at `at` in `data`, `at` moved past it.
pub fn varint(data: &[u8], at: &mut usize) -> Result<u64, Problem> {
    let start = *at;
    let mut value = 0u64;
    for index in 0..MAX_VARINT {
        let byte = *data.get(*at).ok_or_else(|| {
            Problem::Malformed(format!("the data ends inside the varint at byte {start}"))
        })?;
        *at += 1;
        let bits = u64::from(byte & 0x7F);
        if index == MAX_VARINT - 1 && bits > 1 {
            break;
        }
        value |= bits << (7 * index);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Problem::Malformed(format!(
        "the varint at byte {start} is longer than 64 bits"
    )))
}

/// The zigzag varint at `at` in `data`, `at` moved past it.
pub fn zigzag(data: &[u8], at: &mut usize) -> Result<i64, Problem> {
    let value = varint(data, at)?;
    Ok((value >> 1) as i64 ^ -((value & 1) as i64))
}

/// The value at `index` of those packed `width` bits each, least significant bit first, from
/// byte `start` of `data`, which are 0 past its end. `width` is at most 64.
pub fn unpack(data: &[u8], start: usize, index: usize, width: u8) -> u64 {
    if width == 0 {
        return 0;
    }
    let bit = index * usize::from(width);
    let first = start + bit / 8;
    let mut word = [0u8; 16];
    let available = data.get(first..).unwrap_or_default();
    let length = available.len().min(word.len());
    word[..length].copy_from_slice(&available[..length]);
    let value = u128::from_le_bytes(word) >> (bit % 8);
    (value & (u128::MAX >> (128 - u32::from(width)))) as u64
}

/// The value at `index` of those packed `width` bits each, most significant bit first, from byte
/// `start` of `data`, as the deprecated `BIT_PACKED` encoding packs levels. `width` is at most
/// 32, and the bits are in `data`.
pub fn unpack_msb_first(data: &[u8], start: usize, index: usize, width: u8) -> u32 {
    let first = index * usize::from(width);
    (first..first + usize::from(width)).fold(0, |value, bit| {
        let byte = data[start + bit / 8];
        value << 1 | u32::from(byte >> (7 - bit % 8) & 1)
    })
}

/// The bits of a value of up to `maximum`.
pub fn width(maximum: u32) -> u8 {
    (u32::BITS - maximum.leading_zeros()) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_varints_of_up_to_64_bits() {
        let mut at = 0;
        assert_eq!(varint(&[0xE5, 0x8E, 0x26, 7], &mut at).unwrap(), 624_485);
        assert_eq!(at, 3);
        let max = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        assert_eq!(varint(&max, &mut 0).unwrap(), u64::MAX);
        let mut too_long = max;
        too_long[9] = 0x02;
        assert!(varint(&too_long, &mut 0).is_err());
        assert!(varint(&[0x80, 0x80], &mut 0).is_err());
        assert_eq!(zigzag(&[3], &mut 0).unwrap(), -2);
        assert_eq!(zigzag(&[4], &mut 0).unwrap(), 2);
    }

    #[test]
    fn unpacks_values_either_bit_first() {
        let packed = [0b1000_1000, 0b1100_0110, 0b1111_1010];
        let values: Vec<u64> = (0..8).map(|index| unpack(&packed, 0, index, 3)).collect();
        assert_eq!(values, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(unpack(&packed, 0, 9, 3), 0);
        assert_eq!(unpack(&[0xFF; 9], 0, 0, 64), u64::MAX);
        let msb = [0b0000_0101, 0b0011_1001, 0b0111_0111];
        let values: Vec<u32> = (0..8)
            .map(|index| unpack_msb_first(&msb, 0, index, 3))
            .collect();
        assert_eq!(values, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(width(0), 0);
        assert_eq!(width(1), 1);
        assert_eq!(width(4), 3);
    }
}
