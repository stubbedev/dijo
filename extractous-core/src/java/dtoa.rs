//! The decimal digits of a double as the JDK writes them. `java.text.DecimalFormat` takes them
//! from the old `FloatingDecimal` conversion, ported here step for step with whether its last
//! digit was rounded up and whether the digits are exact; `Double.toString` uses the shortest
//! digits that read back as the double.

use super::bigint::BigUint;

const EXP_SHIFT: i32 = 52;
const FRACT_HOB: u64 = 1 << EXP_SHIFT;
const EXP_ONE: u64 = 0x3FF0_0000_0000_0000;
const SIGNIF_BIT_MASK: u64 = FRACT_HOB - 1;
const MAX_SMALL_BIN_EXP: i32 = 62;
const MIN_SMALL_BIN_EXP: i32 = -21;
const LONG_5_POW_LENGTH: i32 = 27;
const N_5_BITS: [i32; 27] = [
    0, 3, 5, 7, 10, 12, 14, 17, 19, 21, 24, 26, 28, 31, 33, 35, 38, 40, 42, 45, 47, 49, 52, 54, 56,
    59, 61,
];
const INSIGNIFICANT_DIGITS: [i32; 64] = [
    0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9,
    9, 9, 10, 10, 10, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 14, 14, 14, 15, 15, 15, 15, 16, 16,
    16, 17, 17, 17, 18, 18, 18, 19,
];

/// A positive, finite double's digits, as `FloatingDecimal.BinaryToASCIIBuffer` makes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conversion {
    /// The digits, most significant first.
    pub(crate) digits: Vec<u8>,
    /// Where the decimal point is: the value is `0.digits × 10^exponent`.
    pub(crate) exponent: i32,
    /// `digitsRoundedUp`.
    pub(crate) rounded_up: bool,
    /// `decimalDigitsExact`.
    pub(crate) exact: bool,
}

fn five_power(exponent: i32) -> u64 {
    5u64.pow(exponent as u32)
}

/// `FloatingDecimal.getBinaryToASCIIConverter(value)` for a finite value other than zero, its
/// The decimal logarithm of two as `FloatingDecimal` writes it, a digit short of the double's.
#[allow(clippy::approx_constant)]
const LOG10_2_IN_JAVA: f64 = 0.301029995663981;

/// sign ignored.
pub(crate) fn convert(value: f64) -> Conversion {
    let bits = value.to_bits();
    let mut fract_bits = bits & SIGNIF_BIT_MASK;
    let mut bin_exp = ((bits >> EXP_SHIFT) & 0x7FF) as i32;
    let significant_bits;
    if bin_exp == 0 {
        let leading_zeros = fract_bits.leading_zeros() as i32;
        let shift = leading_zeros - (63 - EXP_SHIFT);
        fract_bits <<= shift;
        bin_exp = 1 - shift;
        significant_bits = 64 - leading_zeros;
    } else {
        fract_bits |= FRACT_HOB;
        significant_bits = EXP_SHIFT + 1;
    }
    bin_exp -= 1023;
    dtoa(bin_exp, fract_bits, significant_bits)
}

fn develop_long_digits(mut dec_exponent: i32, mut value: u64, insignificant: i32) -> Conversion {
    if insignificant != 0 {
        let pow10 = five_power(insignificant) << insignificant;
        let residue = value % pow10;
        value /= pow10;
        dec_exponent += insignificant;
        if residue >= pow10 >> 1 {
            value += 1;
        }
    }
    let mut reversed = Vec::new();
    let mut c = value % 10;
    value /= 10;
    while c == 0 {
        dec_exponent += 1;
        c = value % 10;
        value /= 10;
    }
    while value != 0 {
        reversed.push(c as u8);
        dec_exponent += 1;
        c = value % 10;
        value /= 10;
    }
    reversed.push(c as u8);
    reversed.reverse();
    Conversion {
        digits: reversed,
        exponent: dec_exponent + 1,
        rounded_up: false,
        exact: false,
    }
}

fn estimate_dec_exp(fract_bits: u64, bin_exp: i32) -> i32 {
    let d2 = f64::from_bits(EXP_ONE | (fract_bits & SIGNIF_BIT_MASK));
    let d = (d2 - 1.5) * 0.289529654 + 0.176091259 + f64::from(bin_exp) * LOG10_2_IN_JAVA;
    let d_bits = d.to_bits();
    let exponent = ((d_bits >> EXP_SHIFT) & 0x7FF) as i32 - 1023;
    let negative = d_bits >> 63 == 1;
    if (0..52).contains(&exponent) {
        let mask = SIGNIF_BIT_MASK >> exponent;
        let r = (((d_bits & SIGNIF_BIT_MASK) | FRACT_HOB) >> (EXP_SHIFT - exponent)) as i32;
        if negative {
            if mask & d_bits == 0 {
                -r
            } else {
                -r - 1
            }
        } else {
            r
        }
    } else if exponent < 0 {
        if d_bits & !(1 << 63) == 0 {
            0
        } else if negative {
            -1
        } else {
            0
        }
    } else {
        d as i32
    }
}

fn dtoa(bin_exp: i32, mut fract_bits: u64, significant_bits: i32) -> Conversion {
    let tail_zeros = fract_bits.trailing_zeros() as i32;
    let fract_bit_count = EXP_SHIFT + 1 - tail_zeros;
    let tiny_bits = (fract_bit_count - bin_exp - 1).max(0);
    if (MIN_SMALL_BIN_EXP..=MAX_SMALL_BIN_EXP).contains(&bin_exp)
        && tiny_bits < LONG_5_POW_LENGTH
        && fract_bit_count + N_5_BITS[tiny_bits as usize] < 64
        && tiny_bits == 0
    {
        let insignificant = if bin_exp > significant_bits {
            let p2 = bin_exp - significant_bits - 1;
            if p2 > 1 && (p2 as usize) < INSIGNIFICANT_DIGITS.len() {
                INSIGNIFICANT_DIGITS[p2 as usize]
            } else {
                0
            }
        } else {
            0
        };
        if bin_exp >= EXP_SHIFT {
            fract_bits <<= bin_exp - EXP_SHIFT;
        } else {
            fract_bits >>= EXP_SHIFT - bin_exp;
        }
        return develop_long_digits(0, fract_bits, insignificant);
    }
    let mut dec_exp = estimate_dec_exp(fract_bits, bin_exp);
    let b5 = (-dec_exp).max(0);
    let mut b2 = b5 + tiny_bits + bin_exp;
    let s5 = dec_exp.max(0);
    let mut s2 = s5 + tiny_bits;
    let m5 = b5;
    let mut m2 = b2 - significant_bits;
    fract_bits >>= tail_zeros;
    b2 -= fract_bit_count - 1;
    let common = b2.min(s2);
    b2 -= common;
    s2 -= common;
    m2 -= common;
    if fract_bit_count == 1 {
        m2 -= 1;
    }
    if m2 < 0 {
        b2 -= m2;
        s2 -= m2;
        m2 = 0;
    }
    let bits_of_5 = |p5: i32| {
        if (p5 as usize) < N_5_BITS.len() {
            N_5_BITS[p5 as usize]
        } else {
            p5 * 3
        }
    };
    let b_bits = fract_bit_count + b2 + bits_of_5(b5);
    let ten_s_bits = s2 + 1 + bits_of_5(s5 + 1);
    let strict = b_bits < 64 && ten_s_bits < 64;
    let small = b_bits < 32 && ten_s_bits < 32;
    let (digits, exponent_adjust, low, high, low_digit_difference, exact) = if small {
        let pow52 = |p5: i32, p2: i32| (five_power(p5) as i32).wrapping_shl(p2 as u32);
        generate(
            (fract_bits as i32)
                .wrapping_mul(five_power(b5) as i32)
                .wrapping_shl(b2 as u32),
            pow52(s5, s2),
            pow52(m5, m2),
            dec_exp,
        )
    } else if strict {
        let pow52 = |p5: i32, p2: i32| (five_power(p5) as i64).wrapping_shl(p2 as u32);
        generate(
            (fract_bits as i64)
                .wrapping_mul(five_power(b5) as i64)
                .wrapping_shl(b2 as u32),
            pow52(s5, s2),
            pow52(m5, m2),
            dec_exp,
        )
    } else {
        let pow52 = |p5: i32, p2: i32| BigUint::pow(5, p5 as u32).shl(p2 as u32);
        generate(
            BigUint::from_u64(fract_bits).mul(&pow52(b5, b2)),
            pow52(s5, s2),
            pow52(m5, m2),
            dec_exp,
        )
    };
    dec_exp += exponent_adjust;
    let mut conversion = Conversion {
        digits,
        exponent: dec_exp + 1,
        rounded_up: false,
        exact,
    };
    if high {
        if low {
            match low_digit_difference {
                std::cmp::Ordering::Equal => {
                    if conversion.digits.last().is_some_and(|d| d % 2 != 0) {
                        round_up(&mut conversion);
                    }
                }
                std::cmp::Ordering::Greater => round_up(&mut conversion),
                std::cmp::Ordering::Less => {}
            }
        } else {
            round_up(&mut conversion);
        }
    }
    conversion
}

/// The arithmetic the digit loop needs, on Java's `int`s and `long`s, which wrap, or on its
/// `FDBigInteger`s.
trait Arithmetic: Clone + Ord {
    /// Whether this is Java's fixed-width arithmetic, whose `m` can overflow.
    const FIXED: bool;
    fn times_ten(&self) -> Self;
    fn plus(&self, other: &Self) -> Self;
    fn is_positive(&self) -> bool;
    fn is_zero(&self) -> bool;
    /// The quotient digit and the remainder.
    fn quotient_digit(&self, divisor: &Self) -> (u8, Self);
    /// `(b << 1) - tens` compared with zero, as each arithmetic computes it.
    fn half_difference(&self, ten_s: &Self) -> std::cmp::Ordering;
}

macro_rules! fixed_arithmetic {
    ($type:ty) => {
        impl Arithmetic for $type {
            const FIXED: bool = true;

            fn times_ten(&self) -> Self {
                self.wrapping_mul(10)
            }

            fn plus(&self, other: &Self) -> Self {
                self.wrapping_add(*other)
            }

            fn is_positive(&self) -> bool {
                *self > 0
            }

            fn is_zero(&self) -> bool {
                *self == 0
            }

            fn quotient_digit(&self, divisor: &Self) -> (u8, Self) {
                ((self / divisor) as u8, self % divisor)
            }

            fn half_difference(&self, ten_s: &Self) -> std::cmp::Ordering {
                self.wrapping_shl(1).wrapping_sub(*ten_s).cmp(&0)
            }
        }
    };
}

fixed_arithmetic!(i32);
fixed_arithmetic!(i64);

impl Arithmetic for BigUint {
    const FIXED: bool = false;

    fn times_ten(&self) -> Self {
        self.mul_small(10)
    }

    fn plus(&self, other: &Self) -> Self {
        self.add(other)
    }

    fn is_positive(&self) -> bool {
        !BigUint::is_zero(self)
    }

    fn is_zero(&self) -> bool {
        BigUint::is_zero(self)
    }

    fn quotient_digit(&self, divisor: &Self) -> (u8, Self) {
        let (quotient, remainder) = self.div_rem(divisor);
        (quotient.low_u64() as u8, remainder)
    }

    fn half_difference(&self, ten_s: &Self) -> std::cmp::Ordering {
        self.shl(1).cmp(ten_s)
    }
}

/// The digit loop of `dtoa`: the digits, how the decimal exponent moved, `low`, `high`, how the
/// remainder compares with half a unit, and whether the digits are exact. Java's fixed-width
/// loops test `b + m > tens` and give up when `m` overflows; its big loop tests `>=`.
fn generate<T: Arithmetic>(
    mut b: T,
    s: T,
    mut m: T,
    dec_exp: i32,
) -> (Vec<u8>, i32, bool, bool, std::cmp::Ordering, bool) {
    let ten_s = s.times_ten();
    let is_high = |b: &T, m: &T| {
        let sum = b.plus(m);
        if T::FIXED {
            sum > ten_s
        } else {
            sum >= ten_s
        }
    };
    let mut digits = Vec::new();
    let mut adjust = 0;
    let (q, remainder) = b.quotient_digit(&s);
    b = remainder.times_ten();
    m = m.times_ten();
    let mut low = b < m;
    let mut high = is_high(&b, &m);
    if q == 0 && !high {
        adjust = -1;
    } else {
        digits.push(q);
    }
    let dec_exp = dec_exp + adjust;
    if !(-3..8).contains(&dec_exp) {
        high = false;
        low = false;
    }
    while !low && !high {
        let (q, remainder) = b.quotient_digit(&s);
        b = remainder.times_ten();
        m = m.times_ten();
        if !T::FIXED || m.is_positive() {
            low = b < m;
            high = is_high(&b, &m);
        } else {
            low = true;
            high = true;
        }
        digits.push(q);
    }
    let difference = b.half_difference(&ten_s);
    let exact = b.is_zero();
    (digits, adjust, low, high, difference, exact)
}

fn round_up(conversion: &mut Conversion) {
    let digits = &mut conversion.digits;
    let mut i = digits.len() - 1;
    let mut q = digits[i];
    if q == 9 {
        while q == 9 && i > 0 {
            digits[i] = 0;
            i -= 1;
            q = digits[i];
        }
        if q == 9 {
            conversion.exponent += 1;
            digits[0] = 1;
            return;
        }
    }
    digits[i] = q + 1;
    conversion.rounded_up = true;
}

/// A Java floating-point type, `double` or `float`, written with its own shortest digits.
pub(crate) trait JavaFloat:
    Copy + PartialEq + PartialOrd + std::fmt::LowerExp + std::str::FromStr
{
    const ZERO: Self;
    fn is_nan(self) -> bool;
    fn is_infinite(self) -> bool;
    fn is_sign_negative(self) -> bool;
    fn abs(self) -> Self;
    fn to_f64(self) -> f64;
}

impl JavaFloat for f64 {
    const ZERO: Self = 0.0;
    fn is_nan(self) -> bool {
        f64::is_nan(self)
    }
    fn is_infinite(self) -> bool {
        f64::is_infinite(self)
    }
    fn is_sign_negative(self) -> bool {
        f64::is_sign_negative(self)
    }
    fn abs(self) -> Self {
        f64::abs(self)
    }
    fn to_f64(self) -> f64 {
        self
    }
}

impl JavaFloat for f32 {
    const ZERO: Self = 0.0;
    fn is_nan(self) -> bool {
        f32::is_nan(self)
    }
    fn is_infinite(self) -> bool {
        f32::is_infinite(self)
    }
    fn is_sign_negative(self) -> bool {
        f32::is_sign_negative(self)
    }
    fn abs(self) -> Self {
        f32::abs(self)
    }
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
}

/// `Double.toString`: the shortest digits that read back as the value, plainly between 10^-3 and
/// 10^7, else as `d.dddE±n`, with at least one digit after the point.
pub(crate) fn java_double_to_string(value: f64) -> String {
    java_to_string(value)
}

/// `Float.toString`, which lays out the shortest digits that read back as the float as
/// `Double.toString` lays out those of a double.
pub(crate) fn java_float_to_string(value: f32) -> String {
    java_to_string(value)
}

fn java_to_string<F: JavaFloat>(value: F) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > F::ZERO {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_owned();
    }
    if value == F::ZERO {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }
    let (digits, exponent) = shortest_digits(value.abs());
    let mut text = String::new();
    if value < F::ZERO {
        text.push('-');
    }
    let magnitude = value.abs().to_f64();
    if (1e-3..1e7).contains(&magnitude) {
        if exponent <= 0 {
            text.push_str("0.");
            text.push_str(&"0".repeat((-exponent) as usize));
            text.push_str(&digits);
        } else if exponent as usize >= digits.len() {
            text.push_str(&digits);
            text.push_str(&"0".repeat(exponent as usize - digits.len()));
            text.push_str(".0");
        } else {
            text.push_str(&digits[..exponent as usize]);
            text.push('.');
            text.push_str(&digits[exponent as usize..]);
        }
    } else {
        text.push_str(&digits[..1]);
        text.push('.');
        if digits.len() > 1 {
            text.push_str(&digits[1..]);
        } else {
            text.push('0');
        }
        text.push('E');
        text.push_str(&(exponent - 1).to_string());
    }
    text
}

/// The digits `Double.toString` and `java.util.Formatter` choose for a positive finite double,
/// with the exponent of `0.digits × 10^n`: of the shortest decimals that read back as the value,
/// the one closest to it, an even last digit breaking a tie, two digits considered where one
/// would do.
pub(crate) fn shortest_digits<F: JavaFloat>(value: F) -> (String, i32) {
    let split = |text: &str| -> (String, i32) {
        let (mantissa, exponent) = text
            .split_once('e')
            .expect("Rust writes an exponent in scientific notation");
        let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        let exponent: i32 = exponent
            .parse()
            .expect("Rust writes the exponent as an integer");
        (digits, exponent + 1)
    };
    let (shortest, exponent) = split(&format!("{value:e}"));
    let length = shortest.len().max(2);
    let closest = format!("{value:.*e}", length - 1);
    let (digits, exponent) = if closest.parse::<F>().ok() == Some(value) {
        split(&closest)
    } else {
        (shortest, exponent)
    };
    let digits = digits.trim_end_matches('0');
    (
        if digits.is_empty() { "0" } else { digits }.to_owned(),
        exponent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digits(value: f64) -> (String, i32, bool, bool) {
        let conversion = convert(value);
        let text: String = conversion
            .digits
            .iter()
            .map(|d| char::from(b'0' + d))
            .collect();
        (
            text,
            conversion.exponent,
            conversion.rounded_up,
            conversion.exact,
        )
    }

    #[test]
    fn converts_as_floating_decimal_does() {
        assert_eq!(digits(1.234567).0, "1234567");
        assert_eq!(digits(1.234567).1, 1);
        assert_eq!(digits(100.0), ("1".to_owned(), 3, false, false));
        assert_eq!(digits(0.5).0, "5");
        assert_eq!(digits(0.1).0, "1");
    }

    #[test]
    fn prints_doubles_as_java_does() {
        assert_eq!(
            java_double_to_string(f64::from_bits(0x430c_6bf5_2634_0002)),
            "1.0000000000000002E15"
        );
        assert_eq!(java_double_to_string(f64::from_bits(1)), "4.9E-324");
        assert_eq!(java_double_to_string(1e23), "1.0E23");
        assert_eq!(java_double_to_string(1.0), "1.0");
        assert_eq!(java_double_to_string(0.001), "0.001");
        assert_eq!(java_double_to_string(1234567.0), "1234567.0");
        assert_eq!(java_double_to_string(1e7), "1.0E7");
        assert_eq!(java_double_to_string(-1.5e-10), "-1.5E-10");
        assert_eq!(java_double_to_string(0.0001), "1.0E-4");
    }

    #[test]
    fn prints_floats_as_java_does() {
        assert_eq!(java_float_to_string(0.1), "0.1");
        assert_eq!(java_float_to_string(444.555), "444.555");
        assert_eq!(java_float_to_string(-2.5), "-2.5");
        assert_eq!(java_float_to_string(16_777_216.0), "1.6777216E7");
        assert_eq!(
            java_float_to_string(f32::MIN_POSITIVE / 8_388_608.0),
            "1.4E-45"
        );
        assert_eq!(java_float_to_string(f32::MAX), "3.4028235E38");
        assert_eq!(java_float_to_string(-0.0), "-0.0");
        assert_eq!(java_float_to_string(f32::NAN), "NaN");
    }
}
