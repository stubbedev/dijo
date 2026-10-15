//! `java.text.DecimalFormat` with the `en_US` symbols the native image formats in: a pattern of
//! `#`, `0`, `,`, `.`, `E`, `%` and quoted text compiled as `applyPattern` compiles it, and
//! numbers formatted through `DigitList` as its slow path does, doubles from the digits of the
//! JDK's `FloatingDecimal` and `BigDecimal`s from their exact digits.

use super::decimal::{BigDecimal, Rounding};
use super::dtoa;

const MAXIMUM_INTEGER_DIGITS: i64 = i32::MAX as i64;
const DOUBLE_INTEGER_DIGITS: i64 = 309;
const DOUBLE_FRACTION_DIGITS: i64 = 340;
const QUOTE: char = '\'';
const CURRENCY_SIGN: char = '\u{a4}';
const PER_MILLE: char = '\u{2030}';
const INFINITY: &str = "\u{221e}";
const NAN: &str = "NaN";

/// A compiled `DecimalFormat`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecimalFormat {
    positive_prefix: String,
    positive_suffix: String,
    negative_prefix: String,
    negative_suffix: String,
    multiplier: i64,
    grouping_size: i64,
    grouping_used: bool,
    grouping_separator: char,
    decimal_always_shown: bool,
    exponential: bool,
    min_exponent_digits: i64,
    min_integer: i64,
    max_integer: i64,
    min_fraction: i64,
    max_fraction: i64,
    rounding: Rounding,
}

/// What the integer and fraction digit settings are for a kind of number: a `double` is held to
/// the 309 and 340 digits a double can have, a `BigDecimal` is not.
#[derive(Debug, Clone, Copy)]
struct Limits {
    min_integer: i64,
    max_integer: i64,
    min_fraction: i64,
    max_fraction: i64,
}

impl DecimalFormat {
    /// `new DecimalFormat(pattern, symbols)` with `setRoundingMode(rounding)`; an error where the
    /// pattern throws `IllegalArgumentException`.
    pub(crate) fn new(pattern: &str, rounding: Rounding) -> Result<Self, String> {
        let malformed = |reason: &str| Err(format!("{reason} in pattern {pattern:?}"));
        let chars: Vec<char> = pattern.chars().collect();
        let mut format = DecimalFormat {
            positive_prefix: String::new(),
            positive_suffix: String::new(),
            negative_prefix: String::new(),
            negative_suffix: String::new(),
            multiplier: 1,
            grouping_size: 3,
            grouping_used: true,
            grouping_separator: ',',
            decimal_always_shown: false,
            exponential: false,
            min_exponent_digits: 0,
            min_integer: 1,
            max_integer: MAXIMUM_INTEGER_DIGITS,
            min_fraction: 0,
            max_fraction: 3,
            rounding,
        };
        let mut got_negative = false;
        let mut positive_prefix_pattern = String::new();
        let mut positive_suffix_pattern = String::new();
        let mut negative_prefix_pattern = String::new();
        let mut negative_suffix_pattern = String::new();
        let mut start = 0;
        let mut j: i32 = 1;
        while j >= 0 && start < chars.len() {
            let mut in_quote = false;
            let mut prefix = String::new();
            let mut suffix = String::new();
            let mut decimal_pos: i64 = -1;
            let mut multiplier = 1;
            let (mut digit_left, mut zero_digits, mut digit_right): (i64, i64, i64) = (0, 0, 0);
            let mut grouping_count: i8 = -1;
            let mut phase = 0;
            let mut in_suffix = false;
            let mut pos = start;
            while pos < chars.len() {
                let ch = chars[pos];
                let affix = if in_suffix { &mut suffix } else { &mut prefix };
                match phase {
                    0 | 2 => {
                        if in_quote {
                            if ch == QUOTE {
                                if chars.get(pos + 1) == Some(&QUOTE) {
                                    pos += 1;
                                    affix.push_str("''");
                                } else {
                                    in_quote = false;
                                }
                                pos += 1;
                                continue;
                            }
                        } else {
                            if matches!(ch, '#' | '0' | ',' | '.') {
                                phase = 1;
                                continue;
                            } else if ch == CURRENCY_SIGN {
                                let doubled = chars.get(pos + 1) == Some(&CURRENCY_SIGN);
                                if doubled {
                                    pos += 1;
                                }
                                affix.push_str(if doubled { "'\u{a4}\u{a4}" } else { "'\u{a4}" });
                                pos += 1;
                                continue;
                            } else if ch == QUOTE {
                                if chars.get(pos + 1) == Some(&QUOTE) {
                                    pos += 1;
                                    affix.push_str("''");
                                } else {
                                    in_quote = true;
                                }
                                pos += 1;
                                continue;
                            } else if ch == ';' {
                                if phase == 0 || j == 0 {
                                    return malformed("Unquoted special character ';'");
                                }
                                start = pos + 1;
                                pos = chars.len();
                                continue;
                            } else if ch == '%' {
                                if multiplier != 1 {
                                    return malformed("Too many percent/per mille characters");
                                }
                                multiplier = 100;
                                affix.push_str("'%");
                                pos += 1;
                                continue;
                            } else if ch == PER_MILLE {
                                if multiplier != 1 {
                                    return malformed("Too many percent/per mille characters");
                                }
                                multiplier = 1000;
                                affix.push_str("'\u{2030}");
                                pos += 1;
                                continue;
                            } else if ch == '-' {
                                affix.push_str("'-");
                                pos += 1;
                                continue;
                            }
                        }
                        affix.push(ch);
                        pos += 1;
                    }
                    _ => {
                        if j == 0 {
                            while pos < chars.len() {
                                let c = chars[pos];
                                if matches!(c, '#' | '0' | ',' | '.' | 'E') {
                                    pos += 1;
                                } else {
                                    phase = 2;
                                    in_suffix = true;
                                    break;
                                }
                            }
                            continue;
                        }
                        match ch {
                            '#' => {
                                if zero_digits > 0 {
                                    digit_right += 1;
                                } else {
                                    digit_left += 1;
                                }
                                if grouping_count >= 0 && decimal_pos < 0 {
                                    grouping_count = grouping_count.wrapping_add(1);
                                }
                            }
                            '0' => {
                                if digit_right > 0 {
                                    return malformed("Unexpected '0'");
                                }
                                zero_digits += 1;
                                if grouping_count >= 0 && decimal_pos < 0 {
                                    grouping_count = grouping_count.wrapping_add(1);
                                }
                            }
                            ',' => grouping_count = 0,
                            '.' => {
                                if decimal_pos >= 0 {
                                    return malformed("Multiple decimal separators");
                                }
                                decimal_pos = digit_left + zero_digits + digit_right;
                            }
                            'E' => {
                                if format.exponential {
                                    return malformed("Multiple exponential symbols");
                                }
                                format.exponential = true;
                                format.min_exponent_digits = 0;
                                pos += 1;
                                while chars.get(pos) == Some(&'0') {
                                    format.min_exponent_digits += 1;
                                    pos += 1;
                                }
                                if digit_left + zero_digits < 1 || format.min_exponent_digits < 1 {
                                    return malformed("Malformed exponential");
                                }
                                phase = 2;
                                in_suffix = true;
                                continue;
                            }
                            _ => {
                                phase = 2;
                                in_suffix = true;
                                continue;
                            }
                        }
                        pos += 1;
                    }
                }
            }
            if zero_digits == 0 && digit_left > 0 && decimal_pos >= 0 {
                let mut n = decimal_pos;
                if n == 0 {
                    n += 1;
                }
                digit_right = digit_left - n;
                digit_left = n - 1;
                zero_digits = 1;
            }
            if (decimal_pos < 0 && digit_right > 0)
                || (decimal_pos >= 0
                    && (decimal_pos < digit_left || decimal_pos > digit_left + zero_digits))
                || grouping_count == 0
                || in_quote
            {
                return malformed("Malformed pattern");
            }
            if j == 1 {
                positive_prefix_pattern = prefix.clone();
                positive_suffix_pattern = suffix.clone();
                negative_prefix_pattern = positive_prefix_pattern.clone();
                negative_suffix_pattern = positive_suffix_pattern.clone();
                let total = digit_left + zero_digits + digit_right;
                let effective_decimal = if decimal_pos >= 0 { decimal_pos } else { total };
                format.set_min_integer(effective_decimal - digit_left);
                let max_integer = if format.exponential {
                    digit_left + format.min_integer
                } else {
                    MAXIMUM_INTEGER_DIGITS
                };
                format.set_max_integer(max_integer);
                format.set_max_fraction(if decimal_pos >= 0 {
                    total - decimal_pos
                } else {
                    0
                });
                format.set_min_fraction(if decimal_pos >= 0 {
                    digit_left + zero_digits - decimal_pos
                } else {
                    0
                });
                format.grouping_used = grouping_count > 0;
                format.grouping_size = if grouping_count > 0 {
                    i64::from(grouping_count)
                } else {
                    0
                };
                format.multiplier = multiplier;
                format.decimal_always_shown = decimal_pos == 0 || decimal_pos == total;
            } else {
                negative_prefix_pattern = prefix;
                negative_suffix_pattern = suffix;
                got_negative = true;
            }
            j -= 1;
        }
        if chars.is_empty() {
            format.set_min_integer(0);
            format.set_max_integer(MAXIMUM_INTEGER_DIGITS);
            format.set_min_fraction(0);
            format.set_max_fraction(DOUBLE_FRACTION_DIGITS);
        }
        if !got_negative
            || (negative_prefix_pattern == positive_prefix_pattern
                && negative_suffix_pattern == positive_suffix_pattern)
        {
            negative_suffix_pattern = positive_suffix_pattern.clone();
            negative_prefix_pattern = format!("'-{positive_prefix_pattern}");
        }
        format.positive_prefix = expand_affix(&positive_prefix_pattern);
        format.positive_suffix = expand_affix(&positive_suffix_pattern);
        format.negative_prefix = expand_affix(&negative_prefix_pattern);
        format.negative_suffix = expand_affix(&negative_suffix_pattern);
        Ok(format)
    }

    fn set_min_integer(&mut self, value: i64) {
        self.min_integer = value.clamp(0, MAXIMUM_INTEGER_DIGITS);
        if self.min_integer > self.max_integer {
            self.max_integer = self.min_integer;
        }
    }

    fn set_max_integer(&mut self, value: i64) {
        self.max_integer = value.clamp(0, MAXIMUM_INTEGER_DIGITS);
        if self.min_integer > self.max_integer {
            self.min_integer = self.max_integer;
        }
    }

    fn set_min_fraction(&mut self, value: i64) {
        self.min_fraction = value.clamp(0, MAXIMUM_INTEGER_DIGITS);
        if self.min_fraction > self.max_fraction {
            self.max_fraction = self.min_fraction;
        }
    }

    fn set_max_fraction(&mut self, value: i64) {
        self.max_fraction = value.clamp(0, MAXIMUM_INTEGER_DIGITS);
        if self.min_fraction > self.max_fraction {
            self.min_fraction = self.max_fraction;
        }
    }

    /// The format with `separator` as its symbols' grouping separator.
    pub(crate) fn with_grouping_separator(mut self, separator: char) -> Self {
        self.grouping_separator = separator;
        self
    }

    fn double_limits(&self) -> Limits {
        Limits {
            min_integer: self.min_integer.min(DOUBLE_INTEGER_DIGITS),
            max_integer: self.max_integer.min(DOUBLE_INTEGER_DIGITS),
            min_fraction: self.min_fraction.min(DOUBLE_FRACTION_DIGITS),
            max_fraction: self.max_fraction.min(DOUBLE_FRACTION_DIGITS),
        }
    }

    fn big_limits(&self) -> Limits {
        Limits {
            min_integer: self.min_integer,
            max_integer: self.max_integer,
            min_fraction: self.min_fraction,
            max_fraction: self.max_fraction,
        }
    }

    /// `format(double)`.
    pub(crate) fn format_f64(&self, value: f64) -> String {
        if value.is_nan() || (value.is_infinite() && self.multiplier == 0) {
            return NAN.to_owned();
        }
        let negative =
            (value < 0.0 || (value == 0.0 && value.is_sign_negative())) ^ (self.multiplier < 0);
        let mut number = value;
        if self.multiplier != 1 {
            number *= self.multiplier as f64;
        }
        if number.is_infinite() {
            let (prefix, suffix) = self.affixes(negative);
            return format!("{prefix}{INFINITY}{suffix}");
        }
        let number = number.abs();
        let limits = self.double_limits();
        let maximum = if self.exponential {
            limits.max_integer + limits.max_fraction
        } else {
            limits.max_fraction
        };
        let digits = DigitList::from_f64(number, maximum, !self.exponential, self.rounding);
        self.subformat(digits, negative, limits)
    }

    /// `format(BigDecimal)`.
    pub(crate) fn format_decimal(&self, value: &BigDecimal) -> String {
        let mut number = value.clone();
        match self.multiplier {
            1 => {}
            100 => number = number.multiply_by_power_of_ten(2),
            1000 => number = number.multiply_by_power_of_ten(3),
            other => unreachable!("a pattern multiplier is 1, 100 or 1000, not {other}"),
        }
        let negative = number.signum() == -1;
        let number = number.abs();
        let limits = self.big_limits();
        let maximum = if self.exponential {
            let maximum = limits.max_integer + limits.max_fraction;
            if maximum > MAXIMUM_INTEGER_DIGITS {
                MAXIMUM_INTEGER_DIGITS
            } else {
                maximum
            }
        } else {
            limits.max_fraction
        };
        let digits = DigitList::from_decimal(&number, maximum, !self.exponential, self.rounding);
        self.subformat(digits, negative, limits)
    }

    fn affixes(&self, negative: bool) -> (&str, &str) {
        if negative {
            (&self.negative_prefix, &self.negative_suffix)
        } else {
            (&self.positive_prefix, &self.positive_suffix)
        }
    }

    fn subformat(&self, mut digits: DigitList, negative: bool, limits: Limits) -> String {
        let (prefix, suffix) = self.affixes(negative);
        let mut result = String::from(prefix);
        if digits.is_zero() {
            digits.decimal_at = 0;
        }
        if self.exponential {
            self.exponential_number(&mut result, &digits, limits);
        } else {
            self.fixed_number(&mut result, &digits, limits);
        }
        result.push_str(suffix);
        result
    }

    fn exponential_number(&self, result: &mut String, digits: &DigitList, limits: Limits) {
        let mut exponent = digits.decimal_at;
        let repeat = limits.max_integer;
        let mut minimum_integer = limits.min_integer;
        if repeat > 1 && repeat > limits.min_integer {
            exponent = if exponent >= 1 {
                ((exponent - 1) / repeat) * repeat
            } else {
                ((exponent - repeat) / repeat) * repeat
            };
            minimum_integer = 1;
        } else {
            exponent -= minimum_integer;
        }
        let mut minimum_digits = limits.min_integer + limits.min_fraction;
        if minimum_digits < 0 {
            minimum_digits = i64::from(i32::MAX);
        }
        let integer_digits = if digits.is_zero() {
            minimum_integer
        } else {
            digits.decimal_at - exponent
        };
        if minimum_digits < integer_digits {
            minimum_digits = integer_digits;
        }
        let total = (digits.digits.len() as i64).max(minimum_digits);
        for i in 0..total {
            if i == integer_digits {
                result.push('.');
            }
            result.push(digits.char_at(i));
        }
        if self.decimal_always_shown && total == integer_digits {
            result.push('.');
        }
        result.push('E');
        if digits.is_zero() {
            exponent = 0;
        }
        if exponent < 0 {
            exponent = -exponent;
            result.push('-');
        }
        let text = exponent.to_string();
        for _ in (text.len() as i64)..self.min_exponent_digits {
            result.push('0');
        }
        result.push_str(&text);
    }

    fn fixed_number(&self, result: &mut String, digits: &DigitList, limits: Limits) {
        let mut count = limits.min_integer;
        let mut index: i64 = 0;
        if digits.decimal_at > 0 && count < digits.decimal_at {
            count = digits.decimal_at;
        }
        if count > limits.max_integer {
            count = limits.max_integer;
            index = digits.decimal_at - count;
        }
        let before = result.len();
        let length = digits.digits.len() as i64;
        let mut i = count - 1;
        while i >= 0 {
            if i < digits.decimal_at && index < length {
                result.push(digits.char_at(index));
                index += 1;
            } else {
                result.push('0');
            }
            if self.grouping_used && i > 0 && self.grouping_size != 0 && i % self.grouping_size == 0
            {
                result.push(self.grouping_separator);
            }
            i -= 1;
        }
        let fraction_present = limits.min_fraction > 0 || index < length;
        if !fraction_present && result.len() == before {
            result.push('0');
        }
        if self.decimal_always_shown || fraction_present {
            result.push('.');
        }
        let mut i = 0;
        while i < limits.max_fraction {
            if i >= limits.min_fraction && index >= length {
                break;
            }
            if -1 - i > digits.decimal_at - 1 {
                result.push('0');
                i += 1;
                continue;
            }
            if index < length {
                result.push(digits.char_at(index));
                index += 1;
            } else {
                result.push('0');
            }
            i += 1;
        }
    }
}

/// `expandAffix`: the quoted specials `'¤`, `'%`, `'‰` and `'-` as their `en_US` symbols.
fn expand_affix(pattern: &str) -> String {
    let mut expanded = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == QUOTE {
            match chars.next() {
                Some(CURRENCY_SIGN) => {
                    if chars.peek() == Some(&CURRENCY_SIGN) {
                        chars.next();
                        expanded.push_str("USD");
                    } else {
                        expanded.push('$');
                    }
                }
                Some('%') => expanded.push('%'),
                Some(PER_MILLE) => expanded.push(PER_MILLE),
                Some('-') => expanded.push('-'),
                Some(other) => expanded.push(other),
                None => {}
            }
            continue;
        }
        expanded.push(c);
    }
    expanded
}

/// `DigitList`: significant digits without trailing zeros, and where the decimal point is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DigitList {
    pub(crate) digits: Vec<u8>,
    pub(crate) decimal_at: i64,
}

impl DigitList {
    fn char_at(&self, index: i64) -> char {
        self.digits
            .get(index as usize)
            .map_or('0', |digit| char::from(b'0' + digit))
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.digits.iter().all(|&digit| digit == 0)
    }

    /// `set(isNegative, double, maximumDigits, fixedPoint)` for a non-negative double.
    pub(crate) fn from_f64(
        value: f64,
        maximum: i64,
        fixed_point: bool,
        rounding: Rounding,
    ) -> Self {
        if value == 0.0 {
            return Self {
                digits: Vec::new(),
                decimal_at: 0,
            };
        }
        let conversion = dtoa::convert(value);
        Self::set(
            conversion.digits,
            i64::from(conversion.exponent),
            conversion.rounded_up,
            conversion.exact,
            maximum,
            fixed_point,
            rounding,
        )
    }

    /// `set(isNegative, BigDecimal, maximumDigits, fixedPoint)` for a non-negative value.
    pub(crate) fn from_decimal(
        value: &BigDecimal,
        maximum: i64,
        fixed_point: bool,
        rounding: Rounding,
    ) -> Self {
        let (digits, decimal_at) = value.digit_list();
        if digits.is_empty() {
            return Self {
                digits,
                decimal_at: 0,
            };
        }
        Self::set(
            digits,
            decimal_at,
            false,
            true,
            maximum,
            fixed_point,
            rounding,
        )
    }

    fn set(
        mut digits: Vec<u8>,
        decimal_at: i64,
        rounded_up: bool,
        exact: bool,
        maximum: i64,
        fixed_point: bool,
        rounding: Rounding,
    ) -> Self {
        let leading = digits.iter().take_while(|&&d| d == 0).count();
        digits.drain(..leading);
        let decimal_at = decimal_at - leading as i64;
        while digits.len() > 1 && digits.last() == Some(&0) {
            digits.pop();
        }
        let mut list = Self { digits, decimal_at };
        if fixed_point {
            if -list.decimal_at > maximum {
                list.digits.clear();
                return list;
            }
            if -list.decimal_at == maximum {
                if list.should_round_up(0, rounded_up, exact, rounding) {
                    list.digits = vec![1];
                    list.decimal_at += 1;
                } else {
                    list.digits.clear();
                }
                return list;
            }
        }
        let maximum_digits = if fixed_point {
            maximum + list.decimal_at
        } else {
            maximum
        };
        list.round(maximum_digits, rounded_up, exact, rounding);
        list
    }

    fn should_round_up(
        &self,
        maximum: i64,
        rounded_up: bool,
        exact: bool,
        rounding: Rounding,
    ) -> bool {
        let count = self.digits.len() as i64;
        if maximum >= count {
            return false;
        }
        let digit = self.digits[maximum as usize];
        match rounding {
            Rounding::Down => false,
            Rounding::HalfUp | Rounding::HalfEven => {
                if digit > 5 {
                    return true;
                }
                if digit != 5 {
                    return false;
                }
                if maximum == count - 1 {
                    if exact {
                        rounding == Rounding::HalfUp
                            || (maximum > 0 && !self.digits[maximum as usize - 1].is_multiple_of(2))
                    } else {
                        !rounded_up
                    }
                } else {
                    self.digits[maximum as usize + 1..].iter().any(|&d| d != 0)
                }
            }
        }
    }

    fn round(&mut self, maximum: i64, rounded_up: bool, exact: bool, rounding: Rounding) {
        if maximum < 0 || maximum >= self.digits.len() as i64 {
            return;
        }
        let mut count = maximum;
        if self.should_round_up(maximum, rounded_up, exact, rounding) {
            let mut i = maximum;
            loop {
                i -= 1;
                if i < 0 {
                    self.digits[0] = 1;
                    self.decimal_at += 1;
                    i = 0;
                    break;
                }
                self.digits[i as usize] += 1;
                if self.digits[i as usize] <= 9 {
                    break;
                }
            }
            count = i + 1;
        }
        self.digits.truncate(count as usize);
        while self.digits.len() > 1 && self.digits.last() == Some(&0) {
            self.digits.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(pattern: &str, value: f64) -> String {
        DecimalFormat::new(pattern, Rounding::HalfUp)
            .unwrap()
            .format_f64(value)
    }

    fn format_text(pattern: &str, value: &str) -> String {
        DecimalFormat::new(pattern, Rounding::HalfUp)
            .unwrap()
            .format_decimal(&BigDecimal::parse(value).unwrap())
    }

    #[test]
    fn formats_as_java_decimal_format_does() {
        assert_eq!(format_text("#,##0.00", "1234567.891"), "1,234,567.89");
        assert_eq!(format_text("0.00%", "0.12345"), "12.35%");
        assert_eq!(format_text("#", "0"), "0");
        assert_eq!(format_text("#.##########", "3.14159"), "3.14159");
        assert_eq!(format_text("0.00E0", "12345"), "1.23E4");
        assert_eq!(format_text("$#,##0;($#,##0)", "-1234.5"), "($1,235)");
        assert_eq!(format_text("0.0", "-0.04"), "-0.0");
        assert_eq!(format("#.##########", 0.125), "0.125");
        assert_eq!(format("0", 2.5), "3");
        assert_eq!(format("0.##############E0", 1.5e20), "1.5E20");
        assert!(DecimalFormat::new("0#", Rounding::HalfUp).is_err());
        assert_eq!(
            DecimalFormat::new("#,##0", Rounding::HalfUp)
                .unwrap()
                .with_grouping_separator(' ')
                .format_f64(1234567.0),
            "1 234 567"
        );
        assert!(DecimalFormat::new("#0#.0#0", Rounding::HalfUp).is_err());
    }
}
