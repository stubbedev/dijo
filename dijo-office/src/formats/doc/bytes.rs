//! Reads of Word's streams at Java `int` offsets, failing where Java's do.

use crate::formats::poi::{self, Read, Thrown};

/// A Java `int` offset as an index, failing where it is negative.
pub(super) fn index(value: i32) -> Read<usize> {
    usize::try_from(value).map_err(|_| Thrown::new(format!("Index {value} out of bounds")))
}

/// `IOUtils.safelyClone(data, offset, length, max)` at Java `int` offsets.
pub(super) fn safely_clone(data: &[u8], offset: i32, length: i32, max: i64) -> Read<Vec<u8>> {
    poi::safely_clone(data, i64::from(offset), i64::from(length), max)
}

/// `count` UTF-16LE units from `at`, as `LittleEndian.getShort` reads them one by one.
pub(super) fn utf16_units(data: &[u8], at: usize, count: usize) -> Read<Vec<u16>> {
    let bytes = count
        .checked_mul(2)
        .and_then(|length| at.checked_add(length))
        .and_then(|end| data.get(at..end))
        .ok_or_else(|| {
            Thrown::new(format!(
                "Index {at} out of bounds for length {}",
                data.len()
            ))
        })?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect())
}
