//! The numbers of 1-2-3's cells: IEEE doubles, the 80-bit extended reals of Release 3 and later,
//! and the packed 16- and 32-bit numbers, with the special values Lotus keeps in place of a number.

/// What a cell's number is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Number {
    Value(f64),
    /// `@NA`, which 1-2-3 shows as `NA`.
    NotAvailable,
    /// `ERR`, an error.
    Error,
    /// A formula's result that is a string, which a record after it holds.
    String,
    /// No value at all.
    Blank,
}

/// The double Lotus' Worksheet File Format gives for `NA`: the sign set, the exponent all ones
/// and the fraction zero. `ERR` is the same with the sign clear, and any other infinity or NaN
/// is taken for `ERR` too, as Gnumeric takes every one; Symphony's string result, a NaN of a
/// clear sign, is replaced by the `STRING` record after it.
const NOT_AVAILABLE_BITS: u64 = 0xFFF0_0000_0000_0000;

/// An IEEE double of a cell, little-endian.
pub(super) fn double(bytes: [u8; 8]) -> Number {
    let value = f64::from_le_bytes(bytes);
    if value.is_finite() {
        Number::Value(value)
    } else if value.to_bits() == NOT_AVAILABLE_BITS {
        Number::NotAvailable
    } else {
        Number::Error
    }
}

/// An 80-bit extended real of a cell, little-endian: a 64-bit significand with its integer bit,
/// then the sign and 15-bit exponent. An exponent and sign of all ones mark the special values
/// by the significand's top byte.
pub(super) fn extended(bytes: [u8; 10]) -> Number {
    let [significand @ .., low, high] = bytes;
    let sign_exponent = u16::from_le_bytes([low, high]);
    if sign_exponent == 0xFFFF {
        match significand[7] {
            0x00 => return Number::Blank,
            0xC0 => return Number::Error,
            0xD0 => return Number::NotAvailable,
            0xE0 => return Number::String,
            _ => {}
        }
    }
    let value = extended_to_double(u64::from_le_bytes(significand), sign_exponent);
    if value.is_finite() {
        Number::Value(value)
    } else {
        Number::Error
    }
}

/// The double nearest the extended real of `significand` and `sign_exponent`, rounded half to
/// even, overflowing to an infinity and underflowing through the subnormals to zero.
fn extended_to_double(significand: u64, sign_exponent: u16) -> f64 {
    let sign = u64::from(sign_exponent >> 15) << 63;
    let exponent = i32::from(sign_exponent & 0x7FFF);
    if exponent == 0x7FFF {
        return f64::from_bits(
            sign | if significand << 1 == 0 {
                f64::INFINITY.to_bits()
            } else {
                f64::NAN.to_bits()
            },
        );
    }
    if significand == 0 {
        return f64::from_bits(sign);
    }
    let shift = significand.leading_zeros();
    let normalised = significand << shift;
    let unbiased = exponent - 16383 - shift as i32 + i32::from(exponent == 0);
    if unbiased > 1023 {
        return f64::from_bits(sign | f64::INFINITY.to_bits());
    }
    let dropped = if unbiased >= -1022 {
        11
    } else {
        11 + (-1022 - unbiased) as u32
    };
    let rounded = round_shift(normalised, dropped);
    let bits = if unbiased >= -1022 {
        let field = u64::try_from(unbiased + 1022).expect("a normal exponent") << 52;
        field + rounded
    } else {
        rounded
    };
    f64::from_bits(sign | bits)
}

/// `value` shifted right by `dropped` bits, rounded half to even.
fn round_shift(value: u64, dropped: u32) -> u64 {
    if dropped > 64 {
        return 0;
    }
    if dropped == 64 {
        return u64::from(value > 1 << 63);
    }
    let kept = value >> dropped;
    let remainder = value & ((1 << dropped) - 1);
    let half = 1 << (dropped - 1);
    if remainder > half || (remainder == half && kept & 1 == 1) {
        kept + 1
    } else {
        kept
    }
}

/// A packed 16-bit number of Release 3 and later: an integer of 15 bits, or, with the low bit
/// set, a 12-bit integer multiplied or divided by one of eight factors.
pub(super) fn small(packed: i16) -> f64 {
    if packed & 1 == 0 {
        return f64::from(packed >> 1);
    }
    let integer = f64::from(packed >> 4);
    match (packed >> 1) & 7 {
        0 => integer * 5000.0,
        1 => integer * 500.0,
        2 => integer / 20.0,
        3 => integer / 200.0,
        4 => integer / 2000.0,
        5 => integer / 20_000.0,
        6 => integer / 16.0,
        _ => integer / 64.0,
    }
}

/// A packed 32-bit number of 1-2-3 97 and later: a 26-bit integer, its sign, and a power of ten
/// it is multiplied or divided by.
pub(super) fn packed(packed: u32) -> f64 {
    let magnitude = f64::from(packed >> 6);
    let power = 10f64.powi((packed & 0x0F) as i32);
    let value = if packed & 0x10 == 0 {
        magnitude * power
    } else {
        magnitude / power
    };
    if packed & 0x20 == 0 {
        value
    } else {
        -value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extended_of(value: u128) -> Number {
        extended(value.to_le_bytes()[..10].try_into().unwrap())
    }

    #[test]
    fn converts_extended_reals_exactly_or_rounded_to_even() {
        assert_eq!(extended_of(0x3FFF_8000_0000_0000_0000), Number::Value(1.0));
        assert_eq!(extended_of(0xC000_C000_0000_0000_0000), Number::Value(-3.0));
        assert_eq!(
            extended_of(0x400F_83EB_8000_0000_0000),
            Number::Value(67_543.0)
        );
        assert_eq!(extended_of(0), Number::Value(0.0));
        let third = 0x3FFD_AAAA_AAAA_AAAA_AAAB;
        assert_eq!(extended_of(third), Number::Value(1.0 / 3.0));
        let halfway = 0x3FFF_8000_0000_0000_0400;
        assert_eq!(extended_of(halfway), Number::Value(1.0));
        let above = 0x3FFF_8000_0000_0000_0C00;
        assert_eq!(extended_of(above), Number::Value(1.0 + 2f64.powi(-51)));
        assert_eq!(extended_of(0x7FFE_8000_0000_0000_0000), Number::Error);
        assert_eq!(
            extended_of(0x3C01_8000_0000_0000_0000),
            Number::Value(f64::MIN_POSITIVE)
        );
        assert_eq!(
            extended_of(0x3BCD_8000_0000_0000_0000),
            Number::Value(f64::from_bits(1))
        );
        assert_eq!(extended_of(0x3BCC_8000_0000_0000_0000), Number::Value(0.0));
    }

    #[test]
    fn reads_the_special_values() {
        assert_eq!(extended_of(0xFFFF_C000_0000_0000_0000), Number::Error);
        assert_eq!(
            extended_of(0xFFFF_D000_0000_0000_0000),
            Number::NotAvailable
        );
        assert_eq!(extended_of(0xFFFF_E000_0000_0000_0000), Number::String);
        assert_eq!(extended_of(0xFFFF_0000_0000_0000_0000), Number::Blank);
        assert_eq!(
            double(NOT_AVAILABLE_BITS.to_le_bytes()),
            Number::NotAvailable
        );
        assert_eq!(double(f64::NAN.to_le_bytes()), Number::Error);
        assert_eq!(double(f64::INFINITY.to_le_bytes()), Number::Error);
        assert_eq!(
            double(f64::NEG_INFINITY.to_le_bytes()),
            Number::NotAvailable
        );
        assert_eq!(double(2.5f64.to_le_bytes()), Number::Value(2.5));
    }

    #[test]
    fn unpacks_packed_numbers() {
        assert_eq!(small(42 << 1), 42.0);
        assert_eq!(small(-7 << 1), -7.0);
        assert_eq!(small((3 << 4) | (2 << 1) | 1), 0.15);
        assert_eq!(small((-2 << 4) | 1), -10_000.0);
        assert_eq!(packed((1234 << 6) | 0x10 | 2), 12.34);
        assert_eq!(packed((5 << 6) | 0x20 | 3), -5000.0);
    }
}
