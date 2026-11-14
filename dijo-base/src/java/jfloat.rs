//! Java's `float` operations where they differ from Rust's: `Float.compare`, which tells -0.0
//! from 0.0 and takes NaN for equal to itself, `Math.min` and `Math.max`, which return NaN for a
//! NaN argument, `Math.round`, and the fixed decimals of `String.format`.

use super::dtoa::{java_double_to_string, shortest_digits};

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

/// `String.format(Locale.ROOT, "%.<precision>f", value)` for a `float`: the shortest digits of
/// the value widened to a `double`, rounded half up.
pub fn format_fixed(value: f32, precision: usize) -> String {
    let value = f64::from(value);
    if !value.is_finite() {
        return java_double_to_string(value);
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let (digits, exponent) = shortest_digits(value.abs());
    let digits = digits.as_bytes();
    let integer_digits = exponent + precision as i32;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(format_fixed(-0.125, 2), "-0.13");
        assert_eq!(format_fixed(f32::NAN, 2), "NaN");
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
