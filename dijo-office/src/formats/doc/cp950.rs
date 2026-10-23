//! POI's `LittleEndianCP950Reader`, which `HWPFOldDocument` reads the text of a Word 6 or 95
//! document in Big5 with: two bytes a character, the trailing byte first, a leading byte below
//! 0x81 giving the trailing byte as it is, the user-defined areas of code page 950 as Microsoft
//! maps them into the private use area, the row 0xF9 that Big5 lacks as code page 950 has it, and
//! any other pair as Big5 decodes it, U+FFFD where it has no character. POI writes `?` there,
//! which reads as text that was never in the document.

use crate::text::charset::Charset;

/// The character of a pair Big5 has none of.
const UNMAPPABLE: u16 = 0xFFFD;

/// The user-defined areas of code page 950, by the pairs they span, and the first private use
/// character each maps to.
const USER_DEFINED: [(u16, u16, i32); 4] = [
    (0x8140, 0x8DFE, 0xEEB8),
    (0x8E40, 0xA0FE, 0xE311),
    (0xC6A1, 0xC8FE, 0xF672),
    (0xFA40, 0xFEFE, 0xE000),
];

/// Row 0xF9 of code page 950, which Big5 lacks: the characters of the trailing bytes 0x40 to
/// 0x7E and 0xA1 to 0xFE.
const ROW_F9: [u16; 157] = [
    0x7e98, 0x7e9b, 0x7e99, 0x81e0, 0x81e1, 0x8646, 0x8647, 0x8648, 0x8979, 0x897a, 0x897c, 0x897b,
    0x89ff, 0x8b98, 0x8b99, 0x8ea5, 0x8ea4, 0x8ea3, 0x946e, 0x946d, 0x946f, 0x9471, 0x9473, 0x9749,
    0x9872, 0x995f, 0x9c68, 0x9c6e, 0x9c6d, 0x9e0b, 0x9e0d, 0x9e10, 0x9e0f, 0x9e12, 0x9e11, 0x9ea1,
    0x9ef5, 0x9f09, 0x9f47, 0x9f78, 0x9f7b, 0x9f7a, 0x9f79, 0x571e, 0x7066, 0x7c6f, 0x883c, 0x8db2,
    0x8ea6, 0x91c3, 0x9474, 0x9478, 0x9476, 0x9475, 0x9a60, 0x9c74, 0x9c73, 0x9c71, 0x9c75, 0x9e14,
    0x9e13, 0x9ef6, 0x9f0a, 0x9fa4, 0x7068, 0x7065, 0x7cf7, 0x866a, 0x883e, 0x883d, 0x883f, 0x8b9e,
    0x8c9c, 0x8ea9, 0x8ec9, 0x974b, 0x9873, 0x9874, 0x98cc, 0x9961, 0x99ab, 0x9a64, 0x9a66, 0x9a67,
    0x9b24, 0x9e15, 0x9e17, 0x9f48, 0x6207, 0x6b1e, 0x7227, 0x864c, 0x8ea8, 0x9482, 0x9480, 0x9481,
    0x9a69, 0x9a68, 0x9b2e, 0x9e19, 0x7229, 0x864b, 0x8b9f, 0x9483, 0x9c79, 0x9eb7, 0x7675, 0x9a6b,
    0x9c7a, 0x9e1d, 0x7069, 0x706a, 0x9ea4, 0x9f7e, 0x9f49, 0x9f98, 0x7881, 0x92b9, 0x88cf, 0x58bb,
    0x6052, 0x7ca7, 0x5afa, 0x2554, 0x2566, 0x2557, 0x2560, 0x256c, 0x2563, 0x255a, 0x2569, 0x255d,
    0x2552, 0x2564, 0x2555, 0x255e, 0x256a, 0x2561, 0x2558, 0x2567, 0x255b, 0x2553, 0x2565, 0x2556,
    0x255f, 0x256b, 0x2562, 0x2559, 0x2568, 0x255c, 0x2551, 0x2550, 0x256d, 0x256e, 0x2570, 0x256f,
    0x2593,
];

/// `DoubleByteUtil.cp950ToString`: a character of each pair of `bytes`, handed to `each` in
/// order, a last odd byte left out.
pub(super) fn decode(bytes: &[u8], mut each: impl FnMut(u16)) {
    let big5 = Charset::named("Big5");
    for &[trailing, leading] in bytes.as_chunks::<2>().0 {
        each(character(big5, trailing, leading));
    }
}

/// Whether the pair `trailing`, `leading` is the character `unit`, below 0x80.
pub(super) fn is_ascii_unit(trailing: u8, leading: u8, unit: u8) -> bool {
    leading < 0x81 && trailing == unit
}

/// `read()`: the character of the pair whose trailing byte comes first.
fn character(big5: Charset, trailing: u8, leading: u8) -> u16 {
    if leading < 0x81 {
        return u16::from(trailing);
    }
    let offset = |trailing: u8| {
        let trailing = i32::from(trailing);
        if trailing < 0x80 {
            trailing - 0x40
        } else {
            trailing - 0x62
        }
    };
    if leading == 0xF9 {
        let index = match trailing {
            0x40..=0x7E => usize::from(trailing - 0x40),
            0xA1..=0xFE => usize::from(trailing - 0xA1) + 63,
            _ => return UNMAPPABLE,
        };
        return ROW_F9[index];
    }
    let pair = u16::from_be_bytes([leading, trailing]);
    if let Some(&(low, _, first)) = USER_DEFINED
        .iter()
        .find(|&&(low, high, _)| (low..=high).contains(&pair))
    {
        let row = i32::from(leading) - i32::from((low >> 8) as u8);
        return (first + 157 * row + offset(trailing)) as u16;
    }
    let decoded = big5
        .decode(&[leading, trailing])
        .expect("the JDK decodes Big5");
    let mut units = decoded.encode_utf16();
    match (units.next(), units.next()) {
        (Some(unit), None) => unit,
        _ => UNMAPPABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(bytes: &[u8]) -> Vec<u16> {
        let mut units = Vec::new();
        decode(bytes, |unit| units.push(unit));
        units
    }

    /// Pairs decode as POI's reader decodes them: ASCII padded with a zero byte, Big5, row 0xF9
    /// and the user-defined areas of code page 950, and U+FFFD where Big5 has no character.
    /// POI writes `?` for those.
    #[test]
    fn decodes_pairs_as_pois_reader_does() {
        assert_eq!(decoded(b"a\0b\0c"), [0x61, 0x62]);
        assert_eq!(decoded(&[0x40, 0xA4]), [0x4E00]);
        assert_eq!(decoded(&[0x40, 0xF9, 0xFE, 0xF9]), [0x7E98, 0x2593]);
        assert_eq!(decoded(&[0x40, 0x81, 0xA1, 0x81]), [0xEEB8, 0xEEF7]);
        assert_eq!(decoded(&[0x40, 0xFA]), [0xE000]);
        assert_eq!(decoded(&[0x30, 0xF9]), [0xFFFD]);
        assert_eq!(decoded(&[0xFF, 0xA3]), [0xFFFD]);
        assert!(is_ascii_unit(0x0D, 0x00, 0x0D));
        assert!(is_ascii_unit(0x0D, 0x80, 0x0D));
        assert!(!is_ascii_unit(0x0D, 0x81, 0x0D));
    }
}
