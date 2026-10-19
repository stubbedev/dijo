//! Java's `float` operations where they differ from Rust's: `Float.compare`, which tells -0.0
//! from 0.0 and takes NaN for equal to itself, `Math.min` and `Math.max`, which return NaN for a
//! NaN argument, `Math.round`, the shortest decimal `Float.toString` and `Double.toString`
//! write, and the fixed decimals of `String.format`.

/// `Float.compare(a, b) == 0`.
pub fn same(a: f32, b: f32) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

/// `Math.min(float, float)`.
pub fn min(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() {
            a
        } else {
            b
        }
    } else if a <= b {
        a
    } else {
        b
    }
}

/// `Math.max(float, float)`.
pub fn max(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() {
            b
        } else {
            a
        }
    } else if a >= b {
        a
    } else {
        b
    }
}

/// `Math.round(float)`: the closest `int`, ties up, NaN 0, saturating.
pub fn round(value: f32) -> i32 {
    if value.is_nan() {
        return 0;
    }
    (f64::from(value) + 0.5).floor() as i32
}

/// `Float.toString`.
pub fn float_to_string(value: f32) -> String {
    java_decimal(format!("{value:e}"), f64::from(value).abs())
}

/// `Double.toString`.
pub fn double_to_string(value: f64) -> String {
    java_decimal(format!("{value:e}"), value.abs())
}

/// `String.format(Locale.ROOT, "%.<precision>f", value)` for a `float`: the shortest digits of
/// the value widened to a `double`, rounded half up.
pub fn format_fixed(value: f32, precision: usize) -> String {
    let value = f64::from(value);
    if !value.is_finite() {
        return double_to_string(value);
    }
    let exponential = format!("{value:e}");
    let (sign, digits, exponent) = parts(&exponential);
    let digits = digits.as_bytes();
    let integer_digits = exponent + 1 + precision as i32;
    let digit = |index: i32| {
        usize::try_from(index)
            .ok()
            .and_then(|index| digits.get(index))
            .map_or(0, |digit| digit - b'0')
    };
    let mut scaled: Vec<u8> = (0..integer_digits.max(0)).map(digit).collect();
    if digit(integer_digits) >= 5 {
        let mut position = scaled.len();
        loop {
            if position == 0 {
                scaled.insert(0, 1);
                break;
            }
            position -= 1;
            if scaled[position] == 9 {
                scaled[position] = 0;
            } else {
                scaled[position] += 1;
                break;
            }
        }
    }
    while scaled.len() <= precision {
        scaled.insert(0, 0);
    }
    let point = scaled.len() - precision;
    let text: String = scaled
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect();
    if precision == 0 {
        return format!("{sign}{text}");
    }
    format!("{sign}{}.{}", &text[..point], &text[point..])
}

/// The sign, digits and decimal exponent of what Rust's `{:e}` writes for a finite value.
fn parts(exponential: &str) -> (&'static str, String, i32) {
    let (mantissa, exponent) = exponential
        .split_once('e')
        .unwrap_or_else(|| unreachable!("`{{:e}}` always writes an exponent"));
    let exponent: i32 = exponent
        .parse()
        .unwrap_or_else(|_| unreachable!("`{{:e}}` writes a decimal exponent"));
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    (
        sign,
        mantissa.chars().filter(char::is_ascii_digit).collect(),
        exponent,
    )
}

/// The shortest digits Rust's `{:e}` gives, written as Java writes a decimal: plainly from
/// 10⁻³ up to 10⁷, with at least one fractional digit, else in computerized scientific
/// notation.
fn java_decimal(exponential: String, magnitude: f64) -> String {
    match exponential.as_str() {
        "NaN" => return "NaN".to_owned(),
        "inf" => return "Infinity".to_owned(),
        "-inf" => return "-Infinity".to_owned(),
        _ => {}
    }
    let (sign, digits, exponent) = parts(&exponential);
    if magnitude == 0.0 {
        return format!("{sign}0.0");
    }
    if (1e-3..1e7).contains(&magnitude) {
        let point = exponent + 1;
        let (integer, fraction) = if point <= 0 {
            (
                "0".to_owned(),
                format!("{}{digits}", "0".repeat((-point) as usize)),
            )
        } else if point as usize >= digits.len() {
            (
                format!("{digits}{}", "0".repeat(point as usize - digits.len())),
                String::new(),
            )
        } else {
            (
                digits[..point as usize].to_owned(),
                digits[point as usize..].to_owned(),
            )
        };
        let fraction = if fraction.is_empty() {
            "0".to_owned()
        } else {
            fraction
        };
        return format!("{sign}{integer}.{fraction}");
    }
    let fraction = if digits.len() > 1 { &digits[1..] } else { "0" };
    format!("{sign}{}.{fraction}E{exponent}", &digits[..1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_decimals_as_java_does() {
        assert_eq!(float_to_string(1.7), "1.7");
        assert_eq!(float_to_string(1.0), "1.0");
        assert_eq!(float_to_string(0.0), "0.0");
        assert_eq!(double_to_string(f64::from(0.1f32)), "0.10000000149011612");
        assert_eq!(double_to_string(0.0), "0.0");
        assert_eq!(double_to_string(1e7), "1.0E7");
        assert_eq!(double_to_string(1.25e-4), "1.25E-4");
        assert_eq!(double_to_string(0.001), "0.001");
        assert_eq!(double_to_string(-12345.5), "-12345.5");
        assert_eq!(double_to_string(100.0), "100.0");
    }

    #[test]
    fn formats_fixed_decimals_as_java_does() {
        assert_eq!(format_fixed(0.125, 2), "0.13");
        assert_eq!(format_fixed(0.3, 2), "0.30");
        assert_eq!(format_fixed(1.0 / 3.0, 2), "0.33");
        assert_eq!(format_fixed(0.995, 2), "1.00");
        assert_eq!(format_fixed(0.004, 2), "0.00");
        assert_eq!(format_fixed(0.005, 2), "0.00");
        assert_eq!(format_fixed(0.375, 2), "0.38");
        assert_eq!(format_fixed(0.0, 2), "0.00");
        assert_eq!(format_fixed(12.5, 0), "13");
    }

    #[test]
    fn compares_and_rounds_as_java_does() {
        assert!(!same(-0.0, 0.0));
        assert!(same(f32::NAN, f32::NAN));
        assert_eq!(round(2.5), 3);
        assert_eq!(round(-2.5), -2);
        assert!(min(f32::NAN, 1.0).is_nan());
        assert_eq!(max(-0.0, 0.0).to_bits(), 0.0f32.to_bits());
    }
}
