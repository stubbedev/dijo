//! Fractions as POI writes them: `SimpleFraction`, the continued fraction nearest a value under
//! a largest denominator, and `FractionFormat`, which `DataFormatter` makes for a format with a
//! `#/` or `?/` in it.

use std::sync::LazyLock;

use regex::Regex;

use super::bigint::BigUint;
use super::decimal::BigDecimal;
use super::dtoa::java_double_to_string;
use super::FormatError;
use crate::formats::java::{parse_int, round};

const MAX_ITERATIONS: i32 = 100;
const MAX_DENOMINATOR_POWER: usize = 4;

static DENOMINATOR_FORMAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(#+)|([0-9]+)").expect("a valid pattern"));

/// A numerator over a denominator, `SimpleFraction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fraction {
    pub(crate) numerator: i32,
    pub(crate) denominator: i32,
}

/// `buildFractionExactDenominator`: `value` in `denominator`ths, rounded.
pub(crate) fn exact_denominator(value: f64, denominator: i32) -> Fraction {
    Fraction {
        numerator: round(value * f64::from(denominator)) as i32,
        denominator,
    }
}

/// `buildFractionMaxDenominator(value, 0, maxDenominator, 100)`: the last convergent of the
/// continued fraction of `value` whose denominator is below `max_denominator`; an error where
/// it throws.
pub(crate) fn max_denominator(value: f64, max_denominator: i32) -> Result<Fraction, FormatError> {
    let overflow = i64::from(i32::MAX);
    let mut r0 = value;
    let mut a0 = r0.floor() as i64;
    if a0 > overflow {
        return Err(FormatError::Thrown(format!(
            "Overflow trying to convert {} to fraction ({a0}/1)",
            java_double_to_string(value)
        )));
    }
    let (mut p0, mut q0, mut p1, mut q1): (i64, i64, i64, i64) = (1, 0, a0, 1);
    let mut n = 0;
    let max = i64::from(max_denominator);
    let (p2, q2) = loop {
        n += 1;
        let r1 = 1.0 / (r0 - a0 as f64);
        let a1 = r1.floor() as i64;
        let p2 = a1.wrapping_mul(p1).wrapping_add(p0);
        let q2 = a1.wrapping_mul(q1).wrapping_add(q0);
        if max_denominator > 0 && q2.wrapping_abs() > max && q1.wrapping_abs() < max {
            return Ok(Fraction {
                numerator: p1 as i32,
                denominator: q1 as i32,
            });
        }
        if p2 > overflow || q2 > overflow {
            return Err(FormatError::Thrown(format!(
                "Overflow trying to convert {} to fraction ({p2}/{q2})",
                java_double_to_string(value)
            )));
        }
        let convergent = p2 as f64 / q2 as f64;
        if n < MAX_ITERATIONS && (convergent - value).abs() > 0.0 && q2 < max {
            p0 = p1;
            p1 = p2;
            q0 = q1;
            q1 = q2;
            a0 = a1;
            r0 = r1;
        } else {
            break (p2, q2);
        }
    };
    if n >= MAX_ITERATIONS {
        return Err(FormatError::Thrown(format!(
            "Unable to convert {} to fraction after {MAX_ITERATIONS} iterations",
            java_double_to_string(value)
        )));
    }
    Ok(if q2 < max {
        Fraction {
            numerator: p2 as i32,
            denominator: q2 as i32,
        }
    } else {
        Fraction {
            numerator: p1 as i32,
            denominator: q1 as i32,
        }
    })
}

/// How a `FractionFormat` picks its denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Denominator {
    Exact(i32),
    Max(i32),
}

/// `FractionFormat`: a whole part, if it has one, and a fraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FractionFormat {
    whole_part: bool,
    denominator: Denominator,
}

impl FractionFormat {
    /// `new FractionFormat(wholePartFormatString, denomFormatString)`; an error where it throws.
    pub(crate) fn new(whole_part: &str, denominator: &str) -> Result<Self, FormatError> {
        let mut exact = -1;
        let mut max = -1;
        if let Some(found) = DENOMINATOR_FORMAT.captures(denominator) {
            if let Some(digits) = found.get(2) {
                exact = parse_int(digits.as_str()).ok_or_else(|| {
                    FormatError::Thrown(format!(
                        "The denominator {} is not an int",
                        digits.as_str()
                    ))
                })?;
                if exact == 0 {
                    exact = -1;
                }
            } else if let Some(hashes) = found.get(1) {
                max = 10i32.pow(hashes.as_str().len().min(MAX_DENOMINATOR_POWER) as u32);
            }
        }
        let denominator = if exact <= 0 && max <= 0 {
            Denominator::Exact(100)
        } else if exact > 0 {
            Denominator::Exact(exact)
        } else {
            Denominator::Max(max)
        };
        Ok(Self {
            whole_part: !whole_part.is_empty(),
            denominator,
        })
    }

    /// `format(Number)` of a double; an error where it throws.
    pub(crate) fn format(&self, value: f64) -> Result<String, FormatError> {
        let decimal = BigDecimal::from_f64(value).ok_or_else(|| {
            FormatError::Thrown(format!(
                "new BigDecimal({}) throws",
                java_double_to_string(value)
            ))
        })?;
        let negative = decimal.signum() < 0;
        let absolute = decimal.abs();
        let whole = absolute.integer_part();
        let fraction = absolute.fraction_part();
        if whole.is_zero() && fraction.is_zero() {
            return Ok("0".to_owned());
        }
        let sign = if negative { "-" } else { "" };
        let whole_digits = whole.integer_string();
        if fraction.is_zero() {
            return Ok(format!("{sign}{whole_digits}"));
        }
        let fraction_value = fraction.to_f64();
        let fract = match self.denominator {
            Denominator::Exact(denominator) => Ok(exact_denominator(fraction_value, denominator)),
            Denominator::Max(denominator) => max_denominator(fraction_value, denominator),
        };
        let Ok(fract) = fract else {
            return Ok(java_double_to_string(decimal.to_f64()));
        };
        let whole_number = BigUint::from_decimal(&whole_digits);
        if !self.whole_part {
            let true_numerator = signed_sum(
                &whole_number.mul(&signed_magnitude(fract.denominator)),
                fract.denominator < 0,
                fract.numerator,
            );
            return Ok(format!("{sign}{true_numerator}/{}", fract.denominator));
        }
        if fract.numerator == 0 {
            return Ok(format!("{sign}{whole_digits}"));
        }
        if fract.numerator == fract.denominator {
            return Ok(format!(
                "{sign}{}",
                whole_number.add(&BigUint::from_u64(1)).to_decimal()
            ));
        }
        let whole_text = if whole_number.is_zero() {
            String::new()
        } else {
            format!("{whole_digits} ")
        };
        Ok(format!(
            "{sign}{whole_text}{}/{}",
            fract.numerator, fract.denominator
        ))
    }
}

fn signed_magnitude(value: i32) -> BigUint {
    BigUint::from_u64(u64::from(value.unsigned_abs()))
}

/// The decimal text of `±magnitude + addend`, the sign of the magnitude by `negative`.
fn signed_sum(magnitude: &BigUint, negative: bool, addend: i32) -> String {
    let addend_magnitude = signed_magnitude(addend);
    let addend_negative = addend < 0;
    let (negative, digits) = if negative == addend_negative {
        (negative, magnitude.add(&addend_magnitude))
    } else if magnitude >= &addend_magnitude {
        (negative, magnitude.sub(&addend_magnitude))
    } else {
        (addend_negative, addend_magnitude.sub(magnitude))
    };
    if negative && !digits.is_zero() {
        format!("-{}", digits.to_decimal())
    } else {
        digits.to_decimal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fractions_as_simple_fraction_does() {
        assert_eq!(
            max_denominator(0.333, 10),
            Ok(Fraction {
                numerator: 1,
                denominator: 3
            })
        );
        assert_eq!(
            max_denominator(0.5, 100),
            Ok(Fraction {
                numerator: 1,
                denominator: 2
            })
        );
        assert_eq!(exact_denominator(0.26, 4).numerator, 1);
    }

    #[test]
    fn formats_as_fraction_format_does() {
        let format = |whole: &str, denominator: &str, value: f64| {
            FractionFormat::new(whole, denominator)
                .unwrap()
                .format(value)
                .unwrap()
        };
        assert_eq!(format("#", "#/##", 1.25), "1 1/4");
        assert_eq!(format("", "#/##", 1.25), "5/4");
        assert_eq!(format("#", "8", -2.5), "-2 4/8");
        assert_eq!(format("#", "#/#", 0.0), "0");
        assert_eq!(format("#", "#/#", 3.0), "3");
        assert_eq!(format("#", "#/#", 0.999), "1");
    }
}
