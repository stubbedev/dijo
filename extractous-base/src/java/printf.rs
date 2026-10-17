//! The `java.util.Formatter` conversions POI's cell formatters use, in `en_US`: `%0Nd` for
//! whole numbers, and `%W.Pf` and `%W.PE` for doubles, which round the shortest decimal digits
//! of the double half up, as the JDK's `FormattedFPDecimal` does.

use crate::java::dtoa::shortest_digits;

/// A double's digits as `FormattedFPDecimal` holds them: `f × 10^e`, `f` of `n` digits.
#[derive(Debug, Clone, Copy)]
struct Decimal {
    f: u64,
    e: i32,
    n: i32,
}

impl Decimal {
    fn of(value: f64) -> Self {
        if value == 0.0 {
            return Self { f: 0, e: 0, n: 0 };
        }
        let (digits, exponent) = shortest_digits(value.abs());
        let n = digits.len() as i32;
        Self {
            f: digits.parse().unwrap_or(0),
            e: exponent - n,
            n,
        }
    }

    fn exponent_rounded(&self) -> i32 {
        self.n + self.e - 1
    }

    /// `round(pp)`: keeps `pp` digits, half up.
    fn round(&mut self, pp: i64) {
        if self.n == 0 || pp < 0 {
            *self = Self { f: 0, e: 0, n: 1 };
            return;
        }
        if pp >= i64::from(self.n) {
            return;
        }
        let p = pp as i32;
        self.e += self.n - p;
        let pow10 = 10u64.pow((self.n - p) as u32);
        self.f = (self.f + (pow10 >> 1)) / pow10;
        if p == 0 {
            self.n = 1;
            if self.f == 0 {
                self.e = 0;
            }
            return;
        }
        self.n = p;
        if self.f == 10u64.pow(p as u32) {
            self.f /= 10;
            self.e += 1;
        }
    }

    fn digit_string(&self) -> String {
        let text = self.f.to_string();
        format!(
            "{}{}",
            "0".repeat((self.n as usize).saturating_sub(text.len())),
            text
        )
    }

    /// `plainChars`.
    fn plain(&self) -> String {
        let digits = self.digit_string();
        if self.e >= 0 {
            format!("{digits}{}", "0".repeat(self.e as usize))
        } else if self.n + self.e > 0 {
            let split = (self.n + self.e) as usize;
            format!("{}.{}", &digits[..split], &digits[split..])
        } else {
            format!("0.{}{digits}", "0".repeat((-self.e - self.n) as usize))
        }
    }
}

/// `addZeros`: the mantissa given `precision` digits after its point.
fn add_zeros(mantissa: &mut String, precision: usize) {
    let (point, need_dot) = match mantissa.find('.') {
        Some(point) => (point, false),
        None => (mantissa.len(), true),
    };
    let present = mantissa.len() - point - usize::from(!need_dot);
    if present >= precision {
        return;
    }
    if need_dot {
        mantissa.push('.');
    }
    mantissa.push_str(&"0".repeat(precision - present));
}

/// Pads `body`, after its sign, with zeros to `width` characters.
fn zero_pad(negative: bool, body: &str, width: usize) -> String {
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    let length = text.len() + body.chars().count();
    text.push_str(&"0".repeat(width.saturating_sub(length)));
    text.push_str(body);
    text
}

/// `%0W.Pf`, zero-padded when `zero_pad` is set, or `%W.Pf` padded with spaces.
pub fn fixed(value: f64, width: usize, precision: usize, zero_padded: bool) -> String {
    let negative = value.is_sign_negative() && !value.is_nan();
    if value.is_nan() || value.is_infinite() {
        let text = if value.is_nan() {
            "NaN"
        } else if negative {
            "-Infinity"
        } else {
            "Infinity"
        };
        return format!("{text:>width$}");
    }
    let mut decimal = Decimal::of(value);
    decimal.round(i64::from(decimal.n + decimal.e) + precision as i64);
    let mut mantissa = decimal.plain();
    add_zeros(&mut mantissa, precision);
    if zero_padded {
        zero_pad(negative, &mantissa, width)
    } else {
        let text = format!("{}{mantissa}", if negative { "-" } else { "" });
        format!("{text:>width$}")
    }
}

/// `%W.PE`.
pub(crate) fn scientific(value: f64, precision: usize) -> String {
    let negative = value.is_sign_negative() && !value.is_nan();
    if value.is_nan() || value.is_infinite() {
        return if value.is_nan() {
            "NaN".to_owned()
        } else if negative {
            "-Infinity".to_owned()
        } else {
            "Infinity".to_owned()
        };
    }
    let mut decimal = Decimal::of(value);
    decimal.round(precision as i64 + 1);
    let digits = decimal.digit_string();
    let mut mantissa = if precision != 0 {
        format!("{}.{}", &digits[..1], &digits[1..])
    } else {
        digits[..1].to_owned()
    };
    add_zeros(&mut mantissa, precision);
    let exponent = if value == 0.0 {
        0
    } else {
        decimal.exponent_rounded()
    };
    let sign = if exponent >= 0 { '+' } else { '-' };
    format!(
        "{}{mantissa}E{sign}{:02}",
        if negative { "-" } else { "" },
        exponent.abs()
    )
}

/// `%0Nd`.
pub fn integer(value: i64, width: usize) -> String {
    zero_pad(value < 0, &value.unsigned_abs().to_string(), width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_java_formatter_does() {
        assert_eq!(fixed(2.675, 1, 2, false), "2.68");
        assert_eq!(fixed(3.5, 1, 9, false), "3.500000000");
        assert_eq!(fixed(1234.5, 8, 0, true), "00001235");
        assert_eq!(fixed(0.0, 4, 2, true), "0.00");
        assert_eq!(fixed(-0.004, 1, 2, false), "-0.00");
        assert_eq!(scientific(12345.678, 5), "1.23457E+04");
        assert_eq!(scientific(0.0, 5), "0.00000E+00");
        assert_eq!(scientific(1e-100, 2), "1.00E-100");
        assert_eq!(integer(7, 3), "007");
    }
}
