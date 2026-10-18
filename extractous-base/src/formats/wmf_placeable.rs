//! `META_PLACEABLE`, the header before a placeable WMF picture ([MS-WMF] 2.3.2.3): its key, the
//! picture's bounds, its units per inch and a checksum of the words before it.

use std::ops::Range;

/// The key a placeable header starts with.
pub const KEY: u32 = 0x9AC6_CDD7;
/// The bytes of a placeable header.
pub const LENGTH: usize = 22;
const BOUNDS: usize = 6;
const UNITS_PER_INCH: Range<usize> = 14..16;
const CHECKSUM: Range<usize> = 20..22;

/// Whether `data` starts with a placeable header.
pub fn starts(data: &[u8]) -> bool {
    data.len() >= LENGTH && data.starts_with(&KEY.to_le_bytes())
}

/// The placeable header of a picture of `bounds` (left, top, right, bottom) in `units_per_inch`.
pub fn header(bounds: [u16; 4], units_per_inch: u16) -> [u8; LENGTH] {
    let mut out = [0; LENGTH];
    out[..4].copy_from_slice(&KEY.to_le_bytes());
    for (index, value) in bounds.into_iter().enumerate() {
        let at = BOUNDS + 2 * index;
        out[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    set_units_per_inch(&mut out, units_per_inch);
    out
}

/// Gives the placeable header `data` starts with `units_per_inch`, with the checksum that goes
/// with them.
///
/// # Panics
/// If `data` does not start with a placeable header.
pub fn set_units_per_inch(data: &mut [u8], units_per_inch: u16) {
    assert!(starts(data), "a placeable header");
    data[UNITS_PER_INCH].copy_from_slice(&units_per_inch.to_le_bytes());
    let checksum = data[..CHECKSUM.start]
        .as_chunks::<2>()
        .0
        .iter()
        .fold(0u16, |sum, &word| sum ^ u16::from_le_bytes(word));
    data[CHECKSUM].copy_from_slice(&checksum.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_the_words_before_it() {
        let header = header([0, 0, 100, 50], 1440);
        assert!(starts(&header));
        assert_eq!(&header[..4], &[0xD7, 0xCD, 0xC6, 0x9A]);
        assert_eq!(&header[14..16], &1440u16.to_le_bytes());
        let checksum = 0xCDD7u16 ^ 0x9AC6 ^ 100 ^ 50 ^ 1440;
        assert_eq!(&header[20..22], &checksum.to_le_bytes());
        assert!(!starts(&header[..21]));
    }
}
