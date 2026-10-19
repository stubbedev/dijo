//! `java.math.BigDecimal`, as far as the number formats use it: exact values of doubles and of
//! decimal text, rounding to a precision, scaling by powers of ten and the parts of a value.

use crate::java::bigint::BigUint;

/// Java's `RoundingMode`s the formats round with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    HalfUp,
    HalfEven,
    Down,
}

/// A decimal number: an unscaled integer, as decimal digits, over a power of ten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigDecimal {
    pub(crate) negative: bool,
    /// The unscaled value's digits, most significant first, without leading zeros; empty for
    /// zero.
    pub(crate) digits: Vec<u8>,
    pub(crate) scale: i64,
}

/// Whether dropping `dropped` digits, the first `first` of them, rounds `kept` up.
fn rounds_up(mode: Rounding, kept_last: u8, first: u8, rest_nonzero: bool) -> bool {
    match mode {
        Rounding::Down => false,
        Rounding::HalfUp => first >= 5,
        Rounding::HalfEven => first > 5 || (first == 5 && (rest_nonzero || kept_last % 2 == 1)),
    }
}

/// Adds one to decimal digits, growing them on a carry out.
fn increment(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit == 9 {
            *digit = 0;
        } else {
            *digit += 1;
            return;
        }
    }
    digits.insert(0, 1);
}

fn strip_leading_zeros(mut digits: Vec<u8>) -> Vec<u8> {
    let zeros = digits.iter().take_while(|&&d| d == 0).count();
    digits.drain(..zeros);
    digits
}

impl BigDecimal {
    pub(crate) fn zero() -> Self {
        Self {
            negative: false,
            digits: Vec::new(),
            scale: 0,
        }
    }

    /// `new BigDecimal(unscaled, scale)` of the unscaled value's decimal `digits`, negated where
    /// asked.
    pub fn from_digits(negative: bool, digits: &str, scale: i64) -> Self {
        Self::new(negative, digits.bytes().map(|b| b - b'0').collect(), scale)
    }

    /// `toString`: plain where the scale is not negative and the adjusted exponent is at least
    /// -6, else in scientific notation.
    pub fn to_java_string(&self) -> String {
        let coefficient: String = if self.digits.is_empty() {
            "0".to_owned()
        } else {
            self.digits.iter().map(|d| char::from(b'0' + d)).collect()
        };
        let mut text = String::from(if self.negative { "-" } else { "" });
        if self.scale == 0 {
            text.push_str(&coefficient);
            return text;
        }
        let adjusted = -self.scale + (coefficient.len() as i64 - 1);
        if self.scale > 0 && adjusted >= -6 {
            let pad = self.scale - coefficient.len() as i64;
            if pad >= 0 {
                text.push_str("0.");
                text.push_str(&"0".repeat(pad as usize));
                text.push_str(&coefficient);
            } else {
                let point = (-pad) as usize;
                text.push_str(&coefficient[..point]);
                text.push('.');
                text.push_str(&coefficient[point..]);
            }
            return text;
        }
        text.push_str(&coefficient[..1]);
        if coefficient.len() > 1 {
            text.push('.');
            text.push_str(&coefficient[1..]);
        }
        if adjusted != 0 {
            text.push('E');
            if adjusted > 0 {
                text.push('+');
            }
            text.push_str(&adjusted.to_string());
        }
        text
    }

    fn new(negative: bool, digits: Vec<u8>, scale: i64) -> Self {
        let digits = strip_leading_zeros(digits);
        Self {
            negative: negative && !digits.is_empty(),
            digits,
            scale,
        }
    }

    /// `new BigDecimal(text)`, or `None` where it throws.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (negative, body) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match body.find(['e', 'E']) {
            Some(at) => {
                let exponent = &body[at + 1..];
                let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                (&body[..at], exponent.parse::<i64>().ok()?)
            }
            None => (body, 0),
        };
        let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if (integer.is_empty() && fraction.is_empty())
            || !integer
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let digits = integer
            .bytes()
            .chain(fraction.bytes())
            .map(|b| b - b'0')
            .collect();
        Some(Self::new(
            negative,
            digits,
            fraction.len() as i64 - exponent,
        ))
    }

    /// `new BigDecimal(double)`: the double's exact value, or `None` for an infinity or NaN,
    /// where it throws.
    pub fn from_f64(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        let bits = value.to_bits();
        let negative = bits >> 63 == 1;
        let exponent_bits = ((bits >> 52) & 0x7FF) as i64;
        let mut significand = bits & ((1 << 52) - 1);
        let mut exponent = if exponent_bits == 0 {
            -1074
        } else {
            significand |= 1 << 52;
            exponent_bits - 1075
        };
        if significand == 0 {
            return Some(Self::zero());
        }
        while significand & 1 == 0 {
            significand >>= 1;
            exponent += 1;
        }
        let significand = BigUint::from_u64(significand);
        let (unscaled, scale) = if exponent < 0 {
            (
                significand.mul(&BigUint::pow(5, (-exponent) as u32)),
                -exponent,
            )
        } else {
            (significand.shl(exponent as u32), 0)
        };
        let digits = unscaled.to_decimal().bytes().map(|b| b - b'0').collect();
        Some(Self::new(negative, digits, scale))
    }

    /// `BigDecimal.valueOf(double)`: the value of `Double.toString`, or `None` for an infinity
    /// or NaN, where it throws.
    pub(crate) fn value_of(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        Self::parse(&crate::java::dtoa::java_double_to_string(value))
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    pub(crate) fn signum(&self) -> i32 {
        match (self.is_zero(), self.negative) {
            (true, _) => 0,
            (false, true) => -1,
            (false, false) => 1,
        }
    }

    pub(crate) fn abs(&self) -> Self {
        Self {
            negative: false,
            ..self.clone()
        }
    }

    /// `precision`: the unscaled value's digits, 1 for zero.
    pub(crate) fn precision(&self) -> usize {
        self.digits.len().max(1)
    }

    /// `doubleValue`: the nearest double.
    pub fn to_f64(&self) -> f64 {
        if self.is_zero() {
            return if self.negative { -0.0 } else { 0.0 };
        }
        let digits: String = self.digits.iter().map(|d| char::from(b'0' + d)).collect();
        let text = format!(
            "{}{}e{}",
            if self.negative { "-" } else { "" },
            digits,
            -self.scale
        );
        text.parse()
            .expect("decimal digits with an exponent are a valid double")
    }

    /// Drops `count` low digits of the unscaled value, rounding by `mode`; the scale falls by
    /// as much.
    fn drop_digits(&self, count: usize, mode: Rounding) -> Self {
        if count == 0 {
            return self.clone();
        }
        if count > self.digits.len() {
            return Self::new(self.negative, Vec::new(), self.scale - count as i64);
        }
        let split = self.digits.len() - count;
        let mut kept = self.digits[..split].to_vec();
        let first = self.digits[split];
        let rest_nonzero = self.digits[split + 1..].iter().any(|&d| d != 0);
        let last = kept.last().copied().unwrap_or(0);
        if rounds_up(mode, last, first, rest_nonzero) {
            increment(&mut kept);
        }
        Self::new(self.negative, kept, self.scale - count as i64)
    }

    /// `setScale(scale, mode)`: the value at `scale`, rounded by `mode` where that drops digits.
    pub fn set_scale(&self, scale: i64, mode: Rounding) -> Self {
        if scale >= self.scale {
            let mut digits = self.digits.clone();
            if !digits.is_empty() {
                let zeros = usize::try_from(scale - self.scale).expect("a scale difference fits");
                digits.extend(std::iter::repeat_n(0, zeros));
            }
            return Self::new(self.negative, digits, scale);
        }
        let dropped = usize::try_from(self.scale - scale).expect("a scale difference fits");
        self.drop_digits(dropped, mode)
    }

    /// `round(new MathContext(precision, mode))`.
    pub(crate) fn round_to_precision(&self, precision: usize, mode: Rounding) -> Self {
        let mut rounded = self.clone();
        while rounded.precision() > precision && !rounded.is_zero() {
            rounded = rounded.drop_digits(rounded.precision() - precision, mode);
        }
        rounded
    }

    /// `movePointLeft(power)`: the exact quotient by ten to the `power`.
    pub(crate) fn move_point_left(&self, power: usize) -> Self {
        Self {
            scale: self.scale + power as i64,
            ..self.clone()
        }
    }

    /// `multiply(BigDecimal.valueOf(factor))` for a factor of 10, 100 or 1000.
    pub(crate) fn multiply_by_power_of_ten(&self, power: usize) -> Self {
        let mut digits = self.digits.clone();
        if !digits.is_empty() {
            digits.extend(std::iter::repeat_n(0, power));
        }
        Self::new(self.negative, digits, self.scale)
    }

    /// `multiply`: the exact product, its scale the sum of the scales.
    pub(crate) fn multiply(&self, other: &Self) -> Self {
        let number = |n: &Self| {
            BigUint::from_decimal(
                &n.digits
                    .iter()
                    .map(|d| char::from(b'0' + d))
                    .collect::<String>(),
            )
        };
        let product = number(self).mul(&number(other));
        let digits = product.to_decimal().bytes().map(|b| b - b'0').collect();
        Self::new(
            self.negative != other.negative,
            digits,
            self.scale + other.scale,
        )
    }

    /// The integer part, truncated towards zero, as `toBigInteger` gives it.
    pub(crate) fn integer_part(&self) -> Self {
        if self.scale <= 0 {
            return self.clone();
        }
        let keep = self.digits.len().saturating_sub(self.scale as usize);
        Self::new(self.negative, self.digits[..keep].to_vec(), 0)
    }

    /// `remainder(BigDecimal.ONE)`: what follows the integer part, with this value's sign.
    pub(crate) fn fraction_part(&self) -> Self {
        if self.scale <= 0 {
            return Self {
                negative: false,
                digits: Vec::new(),
                scale: self.scale.max(0),
            };
        }
        let keep = self.digits.len().saturating_sub(self.scale as usize);
        Self::new(self.negative, self.digits[keep..].to_vec(), self.scale)
    }

    /// The digits of an integer value, `"0"` for zero, as `BigInteger.toString` writes them.
    pub(crate) fn integer_string(&self) -> String {
        let integer = self.integer_part();
        let mut text = String::new();
        if integer.negative {
            text.push('-');
        }
        if integer.is_zero() {
            text.push('0');
        }
        for digit in &integer.digits {
            text.push(char::from(b'0' + digit));
        }
        for _ in 0..(-integer.scale).max(0) {
            text.push('0');
        }
        text
    }

    /// The digits without trailing zeros and the position of the decimal point among them, as
    /// `DigitList.set(BigDecimal)` reads `toString`: `(digits, decimalAt)`.
    pub(crate) fn digit_list(&self) -> (Vec<u8>, i64) {
        if self.is_zero() {
            return (Vec::new(), 0);
        }
        let decimal_at = self.digits.len() as i64 - self.scale;
        let mut digits = self.digits.clone();
        while digits.len() > 1 && digits.last() == Some(&0) {
            digits.pop();
        }
        (digits, decimal_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(number: &BigDecimal) -> String {
        let digits: String = number.digits.iter().map(|d| char::from(b'0' + d)).collect();
        format!(
            "{}{}e{}",
            if number.negative { "-" } else { "" },
            digits,
            -number.scale
        )
    }

    #[test]
    fn writes_as_java_big_decimal_to_string_does() {
        let string = |negative, digits, scale| {
            BigDecimal::from_digits(negative, digits, scale).to_java_string()
        };
        assert_eq!(string(false, "12345", 0), "12345");
        assert_eq!(string(true, "12345", 2), "-123.45");
        assert_eq!(string(false, "5", 4), "0.0005");
        assert_eq!(string(false, "0", 4), "0.0000");
        assert_eq!(string(false, "1", 7), "1E-7");
        assert_eq!(string(false, "123", 9), "1.23E-7");
        assert_eq!(string(false, "123", -2), "1.23E+4");
        assert_eq!(string(true, "0", 0), "0");
    }

    #[test]
    fn computes_as_java_big_decimal_does() {
        assert_eq!(text(&BigDecimal::from_f64(0.5).unwrap()), "5e-1");
        assert_eq!(
            text(&BigDecimal::from_f64(0.1).unwrap()),
            "1000000000000000055511151231257827021181583404541015625e-55"
        );
        let value = BigDecimal::parse("1234.5").unwrap();
        assert_eq!(text(&value.move_point_left(3)), "12345e-4");
        assert_eq!(
            text(
                &BigDecimal::parse("999.96")
                    .unwrap()
                    .round_to_precision(4, Rounding::HalfUp)
            ),
            "1000e0"
        );
        assert_eq!(
            text(&BigDecimal::parse("-2.75").unwrap().fraction_part()),
            "-75e-2"
        );
        assert_eq!(BigDecimal::parse("-2.75").unwrap().integer_string(), "-2");
        assert_eq!(BigDecimal::parse("1E3").unwrap().integer_string(), "1000");
        assert!(BigDecimal::parse("1.2.3").is_none());
        let value = BigDecimal::parse("-2.675").unwrap();
        assert_eq!(text(&value.set_scale(2, Rounding::HalfUp)), "-268e-2");
        assert_eq!(text(&value.set_scale(5, Rounding::HalfUp)), "-267500e-5");
        assert_eq!(text(&value.set_scale(0, Rounding::HalfUp)), "-3e0");
        assert_eq!(
            text(
                &BigDecimal::parse("96")
                    .unwrap()
                    .set_scale(-2, Rounding::HalfUp)
            ),
            "1e2"
        );
        assert!(BigDecimal::value_of(f64::NAN).is_none());
        assert_eq!(text(&BigDecimal::value_of(0.1).unwrap()), "1e-1");
        assert_eq!(
            BigDecimal::parse("0.00120").unwrap().digit_list(),
            (vec![1, 2], -2)
        );
    }
}
