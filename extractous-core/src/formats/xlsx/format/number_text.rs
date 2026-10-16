//! POI's `NumberToTextConverter`: a double as Excel displays it in a cell of General format, to
//! 15 significant digits, with Excel's own rounding of the binary value, its switch to scientific
//! notation and its odd text for some special values.

use crate::formats::java_text::bigint::BigUint;

const EXCEL_NAN_BITS: u64 = 0xFFFF_0420_003C_0000;
const MAX_TEXT_LEN: usize = 20;
const EXPONENT_OFFSET: i32 = 14;
const LOG_BASE_10_OF_2_TIMES_2_POW_20: i32 = 315_653;
const C_2_POW_19: i32 = 1 << 19;
const FRAC_HALF: i32 = 0x80_0000;
const MAX_REP_WHOLE_PART: i64 = 0x0003_8D7E_A4C6_8000;
const C_64: u32 = 64;
const MIN_PRECISION: u32 = 72;

/// `NumberToTextConverter.toText`.
pub(crate) fn to_text(value: f64) -> String {
    let mut raw = value.to_bits();
    let mut negative = raw >> 63 == 1;
    if negative {
        raw &= 0x7FFF_FFFF_FFFF_FFFF;
    }
    if raw == 0 {
        return if negative { "-0" } else { "0" }.to_owned();
    }
    let (significand, binary_exponent) = expand(raw);
    if binary_exponent < -1022 {
        return if negative { "-0" } else { "0" }.to_owned();
    }
    if binary_exponent == 1024 {
        if value.to_bits() == EXCEL_NAN_BITS {
            return "3.484840871308E+308".to_owned();
        }
        negative = false;
    }
    let decimal = normalise_base_ten(significand, binary_exponent);
    let mut text = String::with_capacity(MAX_TEXT_LEN + 1);
    if negative {
        text.push('-');
    }
    convert_to_text(&mut text, decimal);
    text
}

/// `ExpandedDouble`: the significand as a 64-bit number with its top bit set, and the exponent.
fn expand(raw: u64) -> (BigUint, i32) {
    let biased = (raw >> 52) as i32;
    let fraction = raw & ((1 << 52) - 1);
    if biased == 0 {
        let frac = BigUint::from_u64(fraction);
        let adjust = 64 - frac.bit_length() as i32;
        (frac.shl(adjust as u32), -1023 - adjust)
    } else {
        let frac = BigUint::from_u64(fraction | (1 << 52)).shl(11);
        ((frac), (biased & 0x7FF) - 1023)
    }
}

/// `NormalisedDecimal`: a whole part of 15 or 16 digits, a 24-bit fraction, and the decimal
/// exponent relative to 10^14.
#[derive(Debug, Clone, Copy)]
struct NormalisedDecimal {
    whole: i64,
    fraction: i32,
    exponent: i32,
}

struct MutableNumber {
    significand: BigUint,
    binary_exponent: i32,
}

impl MutableNumber {
    fn normalise_64bit(&mut self) {
        let old_length = self.significand.bit_length() as i32;
        let mut shift = old_length - C_64 as i32;
        if shift == 0 {
            return;
        }
        assert!(shift > 0, "not enough precision");
        self.binary_exponent += shift;
        let mut old_length = old_length;
        if shift > 32 {
            let high_shift = (shift - 1) & 0xFF_FFE0;
            self.significand = self.significand.shr(high_shift as u32);
            shift -= high_shift;
            old_length -= high_shift;
        }
        assert!(shift >= 1);
        self.significand = self
            .significand
            .add(&BigUint::from_u64(1u64 << (shift - 1)));
        if self.significand.bit_length() as i32 > old_length {
            shift += 1;
            self.binary_exponent += 1;
        }
        self.significand = self.significand.shr(shift as u32);
    }

    fn normalised_exponent(&self) -> i32 {
        self.binary_exponent + self.significand.bit_length() as i32 - C_64 as i32
    }

    fn compare_shifted(&self, base_hex: &str) -> std::cmp::Ordering {
        let base =
            BigUint::from_decimal(&u128::from_str_radix(base_hex, 16).unwrap_or(0).to_string());
        let shift = self.significand.bit_length() - C_64;
        self.significand.cmp(&base.shl(shift))
    }

    fn is_below_max_rep(&self) -> bool {
        self.compare_shifted("0E35FA9319FFFE000").is_lt()
    }

    fn is_above_min_rep(&self) -> bool {
        self.compare_shifted("0B5E620F47FFFE666").is_gt()
    }

    fn create_normalised_decimal(&self, pow10: i32) -> NormalisedDecimal {
        let missing_under_bits = self.binary_exponent - 39;
        let low = self.significand.low_u64() as i32;
        let fraction = low.wrapping_shl(missing_under_bits as u32) & 0xFF_FF80;
        let whole = self
            .significand
            .shr((C_64 as i32 - self.binary_exponent - 1) as u32)
            .low_u64() as i64;
        NormalisedDecimal {
            whole,
            fraction,
            exponent: pow10,
        }
    }

    fn multiply_by_power_of_ten(&mut self, pow10: i32) {
        let (multiplicand, shift) = ten_power(pow10.unsigned_abs(), pow10 < 0);
        self.significand = self.significand.mul(&multiplicand);
        self.binary_exponent += shift;
        let excess = (self.significand.bit_length() as i32 - MIN_PRECISION as i32) & !31;
        if excess > 0 {
            self.significand = self.significand.shr(excess as u32);
            self.binary_exponent += excess;
        }
    }
}

/// `TenPower`: the multiplicand and the shift that multiply by 10^index, or divide by it.
fn ten_power(index: u32, divide: bool) -> (BigUint, i32) {
    let five = BigUint::pow(5, index);
    let five_bits = five.bit_length() as i32;
    if divide {
        let px = 80 + five_bits;
        let (fx, _) = BigUint::from_u64(1).shl(px as u32).div_rem(&five);
        let adjust = fx.bit_length() as i32 - 80;
        let divisor = fx.shr(adjust as u32);
        let bits = five_bits - adjust;
        (divisor, -(bits + index as i32 + 80))
    } else {
        let shift = five_bits - 68;
        if shift > 0 {
            (five.shr(shift as u32), index as i32 + shift)
        } else {
            (five, index as i32)
        }
    }
}

fn normalise_base_ten(significand: BigUint, binary_exponent: i32) -> NormalisedDecimal {
    let mut pow10 = if !(46..=49).contains(&binary_exponent) {
        let x = (29 << 19) - binary_exponent * LOG_BASE_10_OF_2_TIMES_2_POW_20 + C_2_POW_19;
        -(x >> 20)
    } else {
        0
    };
    let mut number = MutableNumber {
        significand,
        binary_exponent,
    };
    if pow10 != 0 {
        number.multiply_by_power_of_ten(-pow10);
    }
    match number.normalised_exponent() {
        46 if number.is_above_min_rep() => {}
        44..=46 => {
            number.multiply_by_power_of_ten(1);
            pow10 -= 1;
        }
        47 | 48 => {}
        49 if number.is_below_max_rep() => {}
        49 | 50 => {
            number.multiply_by_power_of_ten(-1);
            pow10 += 1;
        }
        other => panic!("bad binary exponent {other}"),
    }
    number.normalise_64bit();
    number.create_normalised_decimal(pow10)
}

fn convert_to_text(text: &mut String, decimal: NormalisedDecimal) {
    let mut whole = decimal.whole;
    if decimal.fraction >= FRAC_HALF {
        whole += 1;
    }
    let (whole, relative) = if whole < MAX_REP_WHOLE_PART {
        (whole, decimal.exponent)
    } else {
        (whole / 10, decimal.exponent + 1)
    };
    let mut exponent = relative + EXPONENT_OFFSET;
    let digits = if exponent.abs() > 98 {
        let mut rounded = (whole + 5).to_string().into_bytes();
        if let Some(last) = rounded.last_mut() {
            *last = b'0';
        }
        let rounded = String::from_utf8(rounded).unwrap_or_default();
        if rounded.len() == 16 {
            exponent += 1;
        }
        rounded
    } else {
        whole.to_string()
    };
    let significant = digits.trim_end_matches('0').len();
    assert!(significant > 0, "no non-zero digits");
    if exponent < 0 {
        less_than_one(text, &digits, exponent, significant);
    } else {
        greater_than_one(text, &digits, exponent, significant);
    }
}

fn less_than_one(text: &mut String, digits: &str, exponent: i32, significant: usize) {
    let leading_zeros = (-exponent - 1) as usize;
    if 2 + leading_zeros + significant > MAX_TEXT_LEN {
        text.push_str(&digits[..1]);
        if significant > 1 {
            text.push('.');
            text.push_str(&digits[1..significant]);
        }
        text.push_str("E-");
        append_exponent(text, -exponent);
        return;
    }
    text.push_str("0.");
    text.push_str(&"0".repeat(leading_zeros));
    text.push_str(&digits[..significant]);
}

fn greater_than_one(text: &mut String, digits: &str, exponent: i32, significant: usize) {
    if exponent > 19 {
        text.push_str(&digits[..1]);
        if significant > 1 {
            text.push('.');
            text.push_str(&digits[1..significant]);
        }
        text.push_str("E+");
        append_exponent(text, exponent);
        return;
    }
    let exponent = exponent as usize;
    let fraction_digits = significant as i64 - exponent as i64 - 1;
    if fraction_digits > 0 {
        text.push_str(&digits[..=exponent]);
        text.push('.');
        text.push_str(&digits[exponent + 1..significant]);
        return;
    }
    text.push_str(&digits[..significant]);
    text.push_str(&"0".repeat((-fraction_digits) as usize));
}

fn append_exponent(text: &mut String, value: i32) {
    if value < 10 {
        text.push('0');
    }
    text.push_str(&value.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_numbers_as_excel_does() {
        assert_eq!(to_text(0.0), "0");
        assert_eq!(to_text(1.0), "1");
        assert_eq!(to_text(1.5), "1.5");
        assert_eq!(to_text(0.1), "0.1");
        assert_eq!(to_text(-123.456), "-123.456");
        assert_eq!(to_text(1234567.891), "1234567.891");
        assert_eq!(to_text(1e21), "1E+21");
        assert_eq!(to_text(0.1 + 0.2), "0.3");
        assert_eq!(to_text(1.0 / 3.0), "0.333333333333333");
        assert_eq!(to_text(123456789012345678.0), "123456789012346000");
        assert_eq!(to_text(1e-25), "1E-25");
    }
}
