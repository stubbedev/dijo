//! Cell values as Tika's `TikaExcelDataFormatter` writes them: POI's `DataFormatter` in the
//! `en_US` locale, with a general format of 15 significant digits for `General` in any case,
//! dates as their cells hold them, in the document's date system.
//!
//! Each value is formatted by a format made for a value of its kind, a date or a number, and
//! a date format is given each value it formats.
//!
//! Tika departs from this (#92): it gives its general format to `General` and `general` alone,
//! with ten digits after the point, writes dates in Java's default time zone and in the 1900
//! date system whatever the document's, and caches one format per format string, made for the
//! first value it formats, its date formats keeping the last value they were given.
//!
//! Where POI writes a number otherwise than Excel displays it, the formatter writes it as Excel
//! does (#186): a `?` is a digit, shown as a space where it is an insignificant zero, where POI
//! writes the `?` itself; commas after the last digit divide by a thousand each before a
//! literal suffix too, as in `0.0,,"M"`, where POI divides only by commas that end the format,
//! and exactly, where POI rounds the quotient to the decimals of the value it divides;
//! an engineering format such as `##0.0E+0` writes its exponent in steps of its integer
//! digits and its mantissa with the digits the format gives it, where Java's `DecimalFormat`
//! writes more of them; and a format with a decimal point writes it also where no digit follows
//! it, so that `0.##` writes 5 as `5.`, where POI writes `5`. See `date_format` for the dates.

mod cell_format;
mod date_format;
mod fraction;
pub mod header_footer;
mod number_text;

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::java::decimal::{BigDecimal, Rounding};
use crate::java::decimal_format::DecimalFormat;
use crate::java::dtoa::java_double_to_string;
use crate::java::simple_date_format::Unported;
use crate::java::time::TimeZone;
use crate::java::{self, trim};
use crate::unicode::{is_letter, is_whitespace};
use cell_format::{CellFormat, CellValue};
pub(crate) use date_format::is_a_date_format;
use date_format::{
    is_valid_excel_date, java_date, Calendar, ExcelStyleDateFormatter, Literals, CALENDAR_PREFIX,
    ERA_LETTER, ERA_NAME, ERA_SHORT, ERA_YEAR, ERA_YEAR_PADDED,
};
use fraction::FractionFormat;

/// Java's `.` in a pattern: any character but a line terminator.
pub(crate) const DOT: &str = r"[^\n\r\x{85}\x{2028}\x{2029}]";
/// Java's `\s` in a pattern.
pub(crate) const SPACE: &str = r"[ \t\n\x0B\x0C\r]";

/// Why a format gave no text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// Java throws, for the reason given; `DataFormatter` and Tika's sheet handler catch it and
    /// fall back.
    Thrown(String),
    /// Java writes what this port does not.
    Unported(&'static str),
}

impl From<Unported> for FormatError {
    fn from(unported: Unported) -> Self {
        FormatError::Unported(unported.0)
    }
}

/// POI's `BuiltinFormats`, by index.
const BUILTIN_FORMATS: [&str; 50] = [
    "General",
    "0",
    "0.00",
    "#,##0",
    "#,##0.00",
    "\"$\"#,##0_);(\"$\"#,##0)",
    "\"$\"#,##0_);[Red](\"$\"#,##0)",
    "\"$\"#,##0.00_);(\"$\"#,##0.00)",
    "\"$\"#,##0.00_);[Red](\"$\"#,##0.00)",
    "0%",
    "0.00%",
    "0.00E+00",
    "# ?/?",
    "# ??/??",
    "m/d/yy",
    "d-mmm-yy",
    "d-mmm",
    "mmm-yy",
    "h:mm AM/PM",
    "h:mm:ss AM/PM",
    "h:mm",
    "h:mm:ss",
    "m/d/yy h:mm",
    "reserved-0x17",
    "reserved-0x18",
    "reserved-0x19",
    "reserved-0x1A",
    "reserved-0x1B",
    "reserved-0x1C",
    "reserved-0x1D",
    "reserved-0x1E",
    "reserved-0x1F",
    "reserved-0x20",
    "reserved-0x21",
    "reserved-0x22",
    "reserved-0x23",
    "reserved-0x24",
    "#,##0_);(#,##0)",
    "#,##0_);[Red](#,##0)",
    "#,##0.00_);(#,##0.00)",
    "#,##0.00_);[Red](#,##0.00)",
    "_(* #,##0_);_(* (#,##0);_(* \"-\"_);_(@_)",
    "_(\"$\"* #,##0_);_(\"$\"* (#,##0);_(\"$\"* \"-\"_);_(@_)",
    "_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"??_);_(@_)",
    "_(\"$\"* #,##0.00_);_(\"$\"* (#,##0.00);_(\"$\"* \"-\"??_);_(@_)",
    "mm:ss",
    "[h]:mm:ss",
    "mm:ss.0",
    "##0.0E+0",
    "@",
];

/// `BuiltinFormats.getBuiltinFormat(index)`.
pub fn builtin_format(index: i32) -> Option<&'static str> {
    usize::try_from(index)
        .ok()
        .and_then(|index| BUILTIN_FORMATS.get(index).copied())
}

/// `NumberFormat.getInstance(Locale.US).format(value)`: grouped, with up to three fraction
/// digits, rounded half even.
pub fn default_number_format(value: f64) -> String {
    static FORMAT: LazyLock<DecimalFormat> = LazyLock::new(|| {
        DecimalFormat::new("#,##0.###", Rounding::HalfEven).expect("a valid pattern")
    });
    FORMAT.format_f64(value)
}

/// The format strings `updateLocale` gives their special formats.
const ZIP_FORMATS: [&str; 2] = ["00000\\-0000", "00000-0000"];
const PHONE_FORMATS: [&str; 4] = [
    "[<=9999999]###\\-####;\\(###\\)\\ ###\\-####",
    "[<=9999999]###-####;(###) ###-####",
    "###\\-####;\\(###\\)\\ ###\\-####",
    "###-####;(###) ###-####",
];
const SSN_FORMATS: [&str; 2] = ["000\\-00\\-0000", "000-00-0000"];

/// What `DataFormatter` makes of a fraction format it finds no fraction in.
const DEFAULT_FRACTION_WHOLE_PART: &str = "#";
const DEFAULT_FRACTION_PART: &str = "#/##";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

static RANGE_CONDITIONAL: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"\A{DOT}*\[{SPACE}*(?:>|>=|<|<=|=){SPACE}*[0-9]*\.*[0-9]{DOT}*\z"
    ))
});
static COLOR: LazyLock<Regex> = LazyLock::new(|| {
    let names = [
        "BLACK", "BLUE", "CYAN", "GREEN", "MAGENTA", "RED", "WHITE", "YELLOW",
    ];
    let mut alternatives: Vec<String> = names
        .iter()
        .map(|name| format!(r"\[{}\]", ascii_case_insensitive(name)))
        .collect();
    let color = ascii_case_insensitive("COLOR");
    alternatives.push(format!(r"\[{color}{SPACE}*[0-9]\]"));
    alternatives.push(format!(r"\[{color}{SPACE}*[0-5][0-9]\]"));
    regex(&alternatives.join("|"))
});
static LOCALE_GROUP: LazyLock<Regex> = LazyLock::new(|| regex(r"(\[\$[^-\]]*-[0-9A-Z]+\])"));
static FRACTION: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"(?:([#0-9]+){SPACE}+)?(#+){SPACE}*/{SPACE}*([#0-9]+)"
    ))
});
static FRACTION_STRIPPER: LazyLock<Regex> = LazyLock::new(|| regex(r#"("[^"]*")|([^ ?#0-9/]+)"#));
static SPACES: LazyLock<Regex> = LazyLock::new(|| regex(" +"));
static NUMBER: LazyLock<Regex> = LazyLock::new(|| regex("[0#]+"));
static ALTERNATE_GROUPING: LazyLock<Regex> = LazyLock::new(|| regex(r"([#0]([^.#0])[#0]{3})"));
static DECIMAL_FORMAT_FIX: LazyLock<Regex> = LazyLock::new(|| regex(r"\A0+#\z"));
static AM_PM: LazyLock<Regex> = LazyLock::new(|| regex("[AaPp][MmPp/]*"));
static DAYS_AS_TEXT: LazyLock<Regex> = LazyLock::new(|| regex("[dD]{3,}"));

/// `word` as a pattern matching it in any ASCII case, as Java's `CASE_INSENSITIVE` does.
fn ascii_case_insensitive(word: &str) -> String {
    word.chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
            } else {
                regex::escape(&c.to_string())
            }
        })
        .collect()
}

/// Java's `Character.isLetter` of a `char`: a letter of the Basic Multilingual Plane, as the
/// surrogates of any other character are not.
fn is_java_letter(c: char) -> bool {
    c.len_utf16() == 1 && is_letter(c)
}

/// `Matcher.replaceAll(replacement)`: each match replaced by `replacement`, with Java's `\`
/// escapes and `$` group references read in it; an error where Java throws.
fn java_replace_all(pattern: &Regex, text: &str, replacement: &str) -> Result<String, FormatError> {
    let mut failure = None;
    let replaced = pattern.replace_all(text, |captures: &Captures| {
        match java_replacement(replacement, captures) {
            Ok(text) => text,
            Err(error) => {
                failure.get_or_insert(error);
                String::new()
            }
        }
    });
    match failure {
        Some(error) => Err(error),
        None => Ok(replaced.into_owned()),
    }
}

/// `Matcher.appendReplacement`'s reading of `replacement` for one match.
fn java_replacement(replacement: &str, captures: &Captures) -> Result<String, FormatError> {
    let mut text = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let escaped = chars.next().ok_or_else(|| {
                    FormatError::Thrown("character to be escaped is missing".to_owned())
                })?;
                text.push(escaped);
            }
            '$' => {
                let first = chars.next().ok_or_else(|| {
                    FormatError::Thrown(
                        "Illegal group reference: group index is missing".to_owned(),
                    )
                })?;
                let Some(mut group) = first.to_digit(10) else {
                    return Err(FormatError::Thrown("Illegal group reference".to_owned()));
                };
                if group as usize >= captures.len() {
                    return Err(FormatError::Thrown(format!("No group {group}")));
                }
                while let Some(next) = chars.peek().and_then(|c| c.to_digit(10)) {
                    let candidate = group * 10 + next;
                    if candidate as usize >= captures.len() {
                        break;
                    }
                    group = candidate;
                    chars.next();
                }
                text.push_str(captures.get(group as usize).map_or("", |m| m.as_str()));
            }
            other => text.push(other),
        }
    }
    Ok(text)
}

/// Java's line terminators, which `$` matches before at the end of the input.
const LINE_TERMINATORS: [&str; 6] = ["\r\n", "\n", "\r", "\u{85}", "\u{2028}", "\u{2029}"];

/// The run of commas `,+$` finds in `pattern`, as a range of it.
fn trailing_commas(pattern: &str) -> Option<std::ops::Range<usize>> {
    let ends = std::iter::once(pattern.len()).chain(
        LINE_TERMINATORS
            .iter()
            .find(|terminator| pattern.ends_with(*terminator))
            .map(|terminator| pattern.len() - terminator.len()),
    );
    ends.filter_map(|end| {
        let start = pattern[..end].trim_end_matches(',').len();
        (start < end).then_some(start..end)
    })
    .min_by_key(|range| range.start)
}

/// The commas that scale a number pattern by thousands: those that end it, as POI finds them,
/// else the run right after its last digit, before a literal suffix, as Excel reads it too.
fn scaling_commas(pattern: &str) -> Option<std::ops::Range<usize>> {
    trailing_commas(pattern).or_else(|| {
        let start = pattern.rfind(['0', '#', '?'])? + 1;
        let length = pattern[start..].bytes().take_while(|&b| b == b',').count();
        (length > 0).then_some(start..start + length)
    })
}

/// The digit placeholders of a number pattern with a `?`, `0`, `#` or `?` each, of its integer
/// and its fraction, which Excel shows a space for where a `?` stands for an insignificant zero.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QuestionMarks {
    integer: Vec<char>,
    fraction: Vec<char>,
}

impl QuestionMarks {
    /// The placeholders of `pattern`, if it has a `?`.
    fn of(pattern: &str) -> Option<Self> {
        if !pattern.contains('?') {
            return None;
        }
        let digits = |part: &str| -> Vec<char> {
            part.chars()
                .take_while(|&c| c != 'E')
                .filter(|c| matches!(c, '0' | '#' | '?'))
                .collect()
        };
        let (integer, fraction) = pattern.split_once('.').unwrap_or((pattern, ""));
        Some(Self {
            integer: digits(integer),
            fraction: digits(fraction),
        })
    }

    /// `text`, formatted with each `?` taken for a `#`, with a space for each `?` whose digit it
    /// leaves out: before the integer's digits and after the fraction's, which keeps its point.
    fn pad(&self, text: &str) -> String {
        let Some(first) = text.find(|c: char| c.is_ascii_digit()) else {
            return text.to_owned();
        };
        let integer_end = first
            + text[first..]
                .find(|c: char| !c.is_ascii_digit() && c != ',')
                .unwrap_or(text.len() - first);
        let shown = text[first..integer_end]
            .chars()
            .filter(char::is_ascii_digit)
            .count();
        let leading = self
            .integer
            .iter()
            .take(self.integer.len().saturating_sub(shown))
            .filter(|&&c| c == '?')
            .count();
        let mut padded = String::with_capacity(text.len() + leading + self.fraction.len());
        padded.push_str(&text[..first]);
        padded.extend(std::iter::repeat_n(' ', leading));
        padded.push_str(&text[first..integer_end]);
        let mut rest = &text[integer_end..];
        if self.fraction.contains(&'?') {
            let (fraction, after) = match rest.strip_prefix('.') {
                Some(after_point) => {
                    let end = after_point
                        .find(|c: char| !c.is_ascii_digit())
                        .unwrap_or(after_point.len());
                    after_point.split_at(end)
                }
                None => ("", rest),
            };
            let trailing = self
                .fraction
                .iter()
                .skip(fraction.len())
                .filter(|&&c| c == '?')
                .count();
            padded.push('.');
            padded.push_str(fraction);
            padded.extend(std::iter::repeat_n(' ', trailing));
            rest = after;
        }
        padded.push_str(rest);
        padded
    }
}

/// An engineering format, `##0.0E+0` and the like: the exponent a multiple of the integer's
/// digits, the mantissa written with the format's own digits after the point.
#[derive(Debug, Clone, PartialEq)]
struct Engineering {
    /// How many integer digits the format has, the step of its exponent.
    step: i32,
    mantissa: DecimalFormat,
    /// The fewest digits of the exponent.
    exponent_digits: usize,
}

static ENGINEERING: LazyLock<Regex> = LazyLock::new(|| regex(r"\A([#0]*#[#0]*)(\.[#0]*)?E(0+)\z"));

impl Engineering {
    /// The engineering format of `pattern`, as `cleanFormatForNumber` leaves it, if it is one:
    /// an exponent after more integer digits than one, not all of them `0`.
    fn of(pattern: &str) -> Option<Self> {
        let found = ENGINEERING.captures(pattern)?;
        let integer = found.get(1)?.as_str();
        let step = i32::try_from(integer.len()).ok().filter(|&step| step > 1)?;
        let fraction = found.get(2).map_or("", |m| m.as_str());
        let mantissa = DecimalFormat::new(&format!("0{fraction}"), Rounding::HalfUp)
            .ok()?
            .with_point_always_shown();
        Some(Self {
            step,
            mantissa,
            exponent_digits: found.get(3)?.as_str().len(),
        })
    }

    /// `value` with its exponent the multiple of the step at or below its own, its mantissa
    /// rounded and, where that rounds it up to the next step, taken a step on.
    fn format(&self, value: f64) -> String {
        if value == 0.0 || !value.is_finite() {
            return format!(
                "{}E{:0>width$}",
                self.mantissa.format_f64(value),
                0,
                width = self.exponent_digits
            );
        }
        let exponent = value.abs().log10().floor() as i32;
        let mut exponent = exponent.div_euclid(self.step) * self.step;
        let mut mantissa = self.mantissa.format_f64(value / 10f64.powi(exponent));
        let limit = 10f64.powi(self.step);
        if mantissa
            .trim_start_matches('-')
            .parse::<f64>()
            .unwrap_or(0.0)
            >= limit
        {
            exponent += self.step;
            mantissa = self.mantissa.format_f64(value / 10f64.powi(exponent));
        }
        let sign = if exponent < 0 { "-" } else { "" };
        format!(
            "{mantissa}E{sign}{:0>width$}",
            exponent.unsigned_abs(),
            width = self.exponent_digits
        )
    }
}

/// A `DecimalFormat` with `DataFormatter`'s scaling by thousands for trailing commas,
/// `InternalDecimalFormatWithScale`, and what Excel writes otherwise than it: spaces for the
/// insignificant zeros of `?` digits, and engineering exponents.
#[derive(Debug, Clone, PartialEq)]
struct ScaledDecimalFormat {
    format: DecimalFormat,
    /// The power of ten the trailing commas divide by.
    divider: Option<usize>,
    question_marks: Option<Box<QuestionMarks>>,
    engineering: Option<Box<Engineering>>,
}

impl ScaledDecimalFormat {
    fn new(pattern: &str, grouping: char) -> Result<Self, String> {
        let commas = scaling_commas(pattern);
        let trimmed = match &commas {
            Some(range) => format!("{}{}", &pattern[..range.start], &pattern[range.end..]),
            None => pattern.to_owned(),
        };
        let question_marks = QuestionMarks::of(&trimmed).map(Box::new);
        let trimmed = trimmed.replace('?', "#");
        let format = DecimalFormat::new(&trimmed, Rounding::HalfUp)?
            .with_grouping_separator(grouping)
            .with_point_always_shown();
        Ok(Self {
            format,
            divider: commas.map(|range| 3 * range.len()),
            question_marks,
            engineering: Engineering::of(&trimmed).map(Box::new),
        })
    }

    /// The text the format gives, `formatted`, padded for its `?` digits.
    fn padded(&self, formatted: String) -> String {
        match &self.question_marks {
            Some(question_marks) => question_marks.pad(&formatted),
            None => formatted,
        }
    }
}

/// The general number format, `TikaExcelGeneralFormat`: 15 significant digits, the most a
/// double holds. Tika's has only ten digits after the point, and is given to `General` and
/// `general` alone, where other spellings get POI's `ExcelGeneralNumberFormat` of ten
/// significant digits (#92).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct General;

impl General {
    fn is_scientific(absolute: f64) -> bool {
        (absolute > 0.0 && absolute <= 1e-15) || absolute > 1e15
    }

    fn is_whole(value: f64, absolute: f64) -> bool {
        value.floor() == value || absolute > 1e15
    }

    /// `format(Object)`: scientific for the very large and very small, whole numbers whole, and
    /// others at their 15 significant digits.
    fn format(input: Input) -> Result<String, FormatError> {
        if let Input::Date(_) = input {
            return input.format_with(&GENERAL_INTEGER);
        }
        let value = input.double_value()?;
        if value.is_infinite() || value.is_nan() {
            return input.format_with(&GENERAL_INTEGER);
        }
        let absolute = value.abs();
        if Self::is_scientific(absolute) {
            return input.format_with(&GENERAL_SCIENTIFIC);
        }
        if Self::is_whole(value, absolute) {
            return input.format_with(&GENERAL_INTEGER);
        }
        let rounded = BigDecimal::from_f64(value)
            .ok_or_else(|| FormatError::Thrown("A BigDecimal of a non-finite double".to_owned()))?
            .round_to_precision(15, Rounding::HalfUp);
        Ok(GENERAL_DECIMAL.format_decimal(&rounded))
    }
}

/// A format `DataFormatter` makes for a format string.
#[derive(Debug, Clone, PartialEq)]
enum Format {
    General,
    ZipPlusFour,
    Phone,
    SocialSecurityNumber,
    Date(ExcelStyleDateFormatter),
    Fraction(FractionFormat),
    Decimal(ScaledDecimalFormat),
}

/// What a format is given: a double, a `BigDecimal` or a `Date`.
#[derive(Debug, Clone, Copy)]
enum Input<'a> {
    Double(f64),
    Decimal(&'a BigDecimal),
    Date(i64),
}

impl Input<'_> {
    /// `Number.doubleValue`; an error for a `Date`, where Java's cast throws.
    fn double_value(&self) -> Result<f64, FormatError> {
        match self {
            Input::Double(value) => Ok(*value),
            Input::Decimal(value) => Ok(value.to_f64()),
            Input::Date(_) => Err(FormatError::Thrown("A Date is not a Number".to_owned())),
        }
    }

    /// `Number.longValue`, the epoch milliseconds `DateFormat.format(Object)` makes of a number.
    fn long_value(&self) -> i64 {
        match self {
            Input::Double(value) => *value as i64,
            Input::Decimal(value) => {
                let integer = value.integer_part();
                let mut low: u64 = 0;
                for &digit in &integer.digits {
                    low = low.wrapping_mul(10).wrapping_add(u64::from(digit));
                }
                for _ in 0..(-integer.scale).max(0) {
                    low = low.wrapping_mul(10);
                }
                let low = low as i64;
                if integer.negative {
                    low.wrapping_neg()
                } else {
                    low
                }
            }
            Input::Date(instant) => *instant,
        }
    }

    /// `DecimalFormat.format(Object)`: a double or a `BigDecimal`, and an error for a `Date`.
    fn format_with(&self, format: &DecimalFormat) -> Result<String, FormatError> {
        match self {
            Input::Double(value) => Ok(format.format_f64(*value)),
            Input::Decimal(value) => Ok(format.format_decimal(value)),
            Input::Date(_) => Err(FormatError::Thrown(
                "Cannot format given Object as a Number".to_owned(),
            )),
        }
    }
}

fn half_up(pattern: &str) -> DecimalFormat {
    DecimalFormat::new(pattern, Rounding::HalfUp).expect("a valid pattern")
}

/// The general format's patterns: for the very large and very small, for whole numbers, and
/// for the rest, with as many digits after the point as 15 significant digits above 1e-15 take.
static GENERAL_SCIENTIFIC: LazyLock<DecimalFormat> =
    LazyLock::new(|| half_up("0.##############E0"));
static GENERAL_INTEGER: LazyLock<DecimalFormat> = LazyLock::new(|| half_up("#"));
static GENERAL_DECIMAL: LazyLock<DecimalFormat> =
    LazyLock::new(|| half_up("#.##############################"));
static NINE_DIGITS: LazyLock<DecimalFormat> =
    LazyLock::new(|| DecimalFormat::new("000000000", Rounding::HalfEven).expect("a valid pattern"));
static TEN_DIGITS: LazyLock<DecimalFormat> = LazyLock::new(|| {
    DecimalFormat::new("##########", Rounding::HalfEven).expect("a valid pattern")
});

impl Format {
    /// `format(Object)` of `input`, made of the cell value `value`.
    fn format(&self, input: Input, value: f64, zone: &TimeZone) -> Result<String, FormatError> {
        match self {
            Format::General => General::format(input),
            Format::ZipPlusFour => {
                let digits = input.format_with(&NINE_DIGITS)?;
                Ok(format!(
                    "{}-{}",
                    java_substring(&digits, 0, 5)?,
                    java_substring(&digits, 5, 9)?
                ))
            }
            Format::SocialSecurityNumber => {
                let digits = input.format_with(&NINE_DIGITS)?;
                Ok(format!(
                    "{}-{}-{}",
                    java_substring(&digits, 0, 3)?,
                    java_substring(&digits, 3, 5)?,
                    java_substring(&digits, 5, 9)?
                ))
            }
            Format::Phone => phone(&input.format_with(&TEN_DIGITS)?),
            Format::Date(formatter) => formatter.format(value, input.long_value(), zone),
            Format::Fraction(fraction) => fraction.format(input.double_value()?),
            Format::Decimal(scaled) => {
                let formatted = match (input, scaled.divider, &scaled.engineering) {
                    (Input::Date(_), Some(_), _) => Err(FormatError::Thrown(
                        "cannot scaleInput of type java.util.Date".to_owned(),
                    )),
                    (Input::Double(value), Some(power), _) => {
                        let divider: f64 = format!("1e{power}")
                            .parse()
                            .expect("a power of ten is a double");
                        Ok(scaled.format.format_f64(value / divider))
                    }
                    (Input::Decimal(value), Some(power), _) => {
                        Ok(scaled.format.format_decimal(&value.move_point_left(power)))
                    }
                    (input, None, Some(engineering)) => {
                        Ok(engineering.format(input.double_value()?))
                    }
                    (input, None, None) => input.format_with(&scaled.format),
                }?;
                Ok(scaled.padded(formatted))
            }
        }
    }
}

/// `String.substring(from, to)`, an error where it throws.
fn java_substring(text: &str, from: usize, to: usize) -> Result<&str, FormatError> {
    text.get(from..to)
        .ok_or_else(|| FormatError::Thrown(format!("substring({from}, {to}) of {text:?}")))
}

/// `PhoneFormat.format`: the last ten digits as `(nnn) nnn-nnnn`, fewer as far as they go.
fn phone(digits: &str) -> Result<String, FormatError> {
    let length = digits.len();
    if length <= 4 {
        return Ok(digits.to_owned());
    }
    let third = java_substring(digits, length - 4, length)?;
    let second = java_substring(digits, length.saturating_sub(7), length - 4)?;
    let first = java_substring(digits, length.saturating_sub(10), length.saturating_sub(7))?;
    let mut text = String::new();
    if !first.chars().all(is_whitespace) {
        text.push('(');
        text.push_str(first);
        text.push_str(") ");
    }
    if !second.chars().all(is_whitespace) {
        text.push_str(second);
        text.push('-');
    }
    text.push_str(third);
    Ok(text)
}

/// What `getFormat` chose for a value.
enum Chosen {
    /// A cached format, by its key.
    Cached(String),
    /// The general number format.
    General,
    /// The text `CellFormat` gave, which `CellFormatResultWrapper` writes trimmed.
    Text(String),
    /// No format: the value is written as `Double.toString` writes it.
    None,
}

/// What a format string is cached under: a format made for dates under a key of its own. POI
/// caches one format per string, made for the first value it formats, so a later value gets
/// the format made for another: a date one for a number, or a number one for a date (#92).
fn cache_key(format: &str, date: bool) -> String {
    if date {
        format!("{format}\u{0}date")
    } else {
        format.to_owned()
    }
}

/// Cell values as text, for one document, with the formats it has made: POI's `DataFormatter`
/// as Tika's `TikaExcelDataFormatter` extends it.
pub struct DataFormatter {
    /// The time zone dates are written in: UTC, so a date is written as its cell holds it.
    /// Tika writes them in Java's default time zone, where a time a clock skips becomes
    /// another (#92).
    zone: TimeZone,
    /// Whether the document counts its dates from 1904, as Mac Excel's did. Tika counts them
    /// from 1900 whatever the document says (#92).
    date_1904: bool,
    formats: HashMap<String, Format>,
    cell_formats: HashMap<String, Result<CellFormat, String>>,
}

impl DataFormatter {
    /// A formatter for a document that counts its dates from 1904 when `date_1904`, and from
    /// 1900 otherwise.
    pub fn new(date_1904: bool) -> Self {
        let mut formats = HashMap::new();
        for format in ZIP_FORMATS {
            formats.insert(format.to_owned(), Format::ZipPlusFour);
        }
        for format in PHONE_FORMATS {
            formats.insert(format.to_owned(), Format::Phone);
        }
        for format in SSN_FORMATS {
            formats.insert(format.to_owned(), Format::SocialSecurityNumber);
        }
        Self {
            zone: TimeZone::fixed(0),
            date_1904,
            formats,
            cell_formats: HashMap::new(),
        }
    }

    /// Counts the document's dates from 1904 when `date_1904`, and from 1900 otherwise.
    pub fn set_date_1904(&mut self, date_1904: bool) {
        self.date_1904 = date_1904;
    }

    /// `formatRawCellContents(value, formatIndex, formatString, use1904Windowing)`: the text of
    /// a number cell; an error where Java throws, for which Tika writes the cell's raw value.
    pub fn format_raw(
        &mut self,
        value: f64,
        index: i32,
        format: &str,
    ) -> Result<String, FormatError> {
        if is_a_date_format(index, format) && is_valid_excel_date(value) {
            let chosen = self.get_format(value, index, format)?;
            let instant = java_date(value, &self.zone, self.date_1904).ok_or_else(|| {
                FormatError::Thrown("A valid Excel date has no Java date".to_owned())
            })?;
            return match chosen {
                Chosen::Text(text) => Ok(text),
                Chosen::None => cell_format::date_to_string(instant, &self.zone),
                Chosen::General => General::format(Input::Date(instant)),
                Chosen::Cached(key) => match self.cached(&key)? {
                    Format::Date(formatter) => {
                        let shown = formatter.rounded(value);
                        let instant =
                            java_date(shown, &self.zone, self.date_1904).ok_or_else(|| {
                                FormatError::Thrown(
                                    "A valid Excel date has no Java date".to_owned(),
                                )
                            })?;
                        formatter.format(shown, instant, &self.zone)
                    }
                    format => format.format(Input::Date(instant), value, &self.zone),
                },
            };
        }
        let chosen = self.get_format(value, index, format)?;
        let text_value = number_text::to_text(value);
        let decimal;
        let input = if text_value.contains('E') {
            Input::Double(value)
        } else {
            decimal = BigDecimal::parse(&text_value).ok_or_else(|| {
                FormatError::Thrown(format!("new BigDecimal({text_value:?}) throws"))
            })?;
            Input::Decimal(&decimal)
        };
        let result = match chosen {
            Chosen::None => return Ok(java_double_to_string(value)),
            Chosen::Text(text) => text,
            Chosen::General => General::format(input)?,
            Chosen::Cached(key) => self.cached(&key)?.format(input, value, &self.zone)?,
        };
        let lower = format.to_lowercase();
        if (lower.contains("general") || lower.contains("e+0"))
            && result.contains('E')
            && !result.contains("E-")
        {
            return Ok(result.replacen('E', "E+", 1));
        }
        Ok(result)
    }

    fn cached(&self, key: &str) -> Result<&Format, FormatError> {
        self.formats
            .get(key)
            .ok_or_else(|| FormatError::Thrown(format!("The format {key:?} was not cached")))
    }

    /// `getFormat(cellValue, formatIndex, formatStr, use1904Windowing)`.
    fn get_format(&mut self, value: f64, index: i32, format: &str) -> Result<Chosen, FormatError> {
        let format = format.replace("\\%", "'%'");
        let semicolons = format.find(';').zip(format.rfind(';'));
        if let Some((first, last)) = semicolons {
            if first != last || RANGE_CONDITIONAL.is_match(&format) {
                match self.apply_cell_format(value, index, &format) {
                    Ok(text) => return Ok(Chosen::Text(trim(&text).to_owned())),
                    Err(FormatError::Thrown(_)) => {}
                    Err(unported) => return Err(unported),
                }
            }
        }
        let key = cache_key(
            &format,
            is_a_date_format(index, &format) && is_valid_excel_date(value),
        );
        if self.formats.contains_key(&key) {
            return Ok(Chosen::Cached(key));
        }
        if format.eq_ignore_ascii_case("General") || format == "@" {
            return Ok(Chosen::General);
        }
        let created = match self.create_format(value, index, &format)? {
            Created::General => Format::General,
            Created::Format(created) => created,
            Created::None => return Ok(Chosen::None),
        };
        self.formats.insert(key.clone(), created);
        Ok(Chosen::Cached(key))
    }

    fn apply_cell_format(
        &mut self,
        value: f64,
        index: i32,
        format: &str,
    ) -> Result<String, FormatError> {
        let cell_format = self
            .cell_formats
            .entry(format.to_owned())
            .or_insert_with(|| CellFormat::new(format))
            .as_ref()
            .map_err(|reason| FormatError::Thrown(reason.clone()))?;
        let input = if value != 0.0 && is_a_date_format(index, format) {
            java_date(value, &self.zone, self.date_1904).map(CellValue::Date)
        } else {
            Some(CellValue::Number(value))
        };
        cell_format.apply(input, &self.zone)
    }

    /// `createFormat(cellValue, formatIndex, sFormat)`.
    fn create_format(&self, value: f64, index: i32, format: &str) -> Result<Created, FormatError> {
        let mut format = format.to_owned();
        while let Some(found) = COLOR.find(&format) {
            let colour = found.as_str().to_owned();
            let Some(at) = format.find(&colour) else {
                break;
            };
            format.replace_range(at..at + colour.len(), "");
        }
        let mut calendar = Calendar::Gregorian;
        while let Some(found) = LOCALE_GROUP.find(&format) {
            let matched = found.as_str();
            let dollar = matched.find('$').map_or(0, |at| at + 1);
            let dash = matched.find('-').unwrap_or(matched.len());
            if let Some(lcid) = matched.get(dash + 1..matched.len() - 1) {
                if Calendar::of_locale_id(lcid) == Calendar::JapaneseEras {
                    calendar = Calendar::JapaneseEras;
                }
            }
            let mut symbol = matched
                .get(dollar..dash)
                .ok_or_else(|| FormatError::Thrown(format!("substring of {matched:?}")))?
                .to_owned();
            if let Some(at) = symbol.find('$') {
                symbol.insert(at, '\\');
            }
            format = java_replace_all(&LOCALE_GROUP, &format, &symbol)?;
        }
        if format.chars().all(is_whitespace) {
            return Ok(Created::General);
        }
        if format.eq_ignore_ascii_case("General") || format == "@" {
            return Ok(Created::General);
        }
        if is_a_date_format(index, &format) && is_valid_excel_date(value) {
            return create_date_format(&format, calendar);
        }
        if format.contains("#/") || format.contains("?/") {
            return create_fraction_format(&format).map(Created::Format);
        }
        if NUMBER.is_match(&format) {
            return create_number_format(&format);
        }
        Ok(Created::None)
    }
}

/// What `createFormat` made.
enum Created {
    Format(Format),
    /// `DataFormatter`'s general number format, which `getFormat` caches as any other.
    General,
    None,
}

/// `createDateFormat`, the general format where the date format does not compile. The texts the
/// format quotes and the characters it escapes are written as they are, where POI unescapes a
/// few characters and hands the rest, and the quotes, on to `SimpleDateFormat` (#186). As Excel
/// shows them, where POI fails the format: a `B1` prefix names the Gregorian calendar the date
/// is written in anyway; `g`, `gg` and `ggg` write the era of the date, by its letter, first
/// character or name, and `e` and `ee` its year in the era, eras that `calendar` counts.
fn create_date_format(format: &str, calendar: Calendar) -> Result<Created, FormatError> {
    let literals = Literals::of(&format.replace(";@", ""));
    let mut format = literals.format.clone();
    if let Some(prefix) = CALENDAR_PREFIX.find(&format) {
        if prefix.as_str() == "B2" {
            return Err(FormatError::Unported("a date in the Hijri calendar"));
        }
        format.replace_range(prefix.range(), "");
    }
    let has_am_pm = AM_PM.is_match(&format);
    if has_am_pm {
        format = AM_PM.replace_all(&format, "@").into_owned();
    }
    format = format.replace('@', "a");
    if let Some(days) = DAYS_AS_TEXT.find(&format) {
        let weekday = days.as_str().to_ascii_uppercase().replace('D', "E");
        format = DAYS_AS_TEXT
            .replace_all(&format, weekday.as_str())
            .into_owned();
    }
    let chars: Vec<char> = format.chars().collect();
    let mut pattern: Vec<char> = Vec::with_capacity(chars.len());
    let mut m_is_month = true;
    let mut months: Vec<usize> = Vec::new();
    let mut is_elapsed = false;
    let mut j = 0;
    while j < chars.len() {
        let c = chars[j];
        if c == '\'' {
            pattern.push(c);
            j += 1;
            while j < chars.len() {
                let quoted = chars[j];
                pattern.push(quoted);
                if quoted == '\'' {
                    break;
                }
                j += 1;
            }
        } else if c == '[' && !is_elapsed {
            is_elapsed = true;
            m_is_month = false;
            pattern.push(c);
        } else if c == ']' && is_elapsed {
            is_elapsed = false;
            pattern.push(c);
        } else if is_elapsed {
            pattern.push(match c {
                'h' | 'H' => 'H',
                'm' | 'M' => 'm',
                's' | 'S' => 's',
                other => other,
            });
        } else if c == 'h' || c == 'H' {
            m_is_month = false;
            pattern.push(if has_am_pm { 'h' } else { 'H' });
        } else if c == 'm' || c == 'M' {
            if m_is_month {
                pattern.push('M');
                months.push(pattern.len() - 1);
            } else {
                pattern.push('m');
            }
        } else if c == 's' || c == 'S' {
            pattern.push('s');
            for &at in &months {
                if pattern[at] == 'M' {
                    pattern[at] = 'm';
                }
            }
            m_is_month = true;
            months.clear();
        } else if matches!(c, 'g' | 'G' | 'e') {
            m_is_month = true;
            months.clear();
            let run = chars[j..].iter().take_while(|&&next| next == c).count();
            pattern.push(match (c, run) {
                ('e', 1) => ERA_YEAR,
                ('e', _) => ERA_YEAR_PADDED,
                (_, 1) => ERA_LETTER,
                (_, 2) => ERA_SHORT,
                _ => ERA_NAME,
            });
            j += run;
            continue;
        } else if is_java_letter(c) {
            m_is_month = true;
            months.clear();
            pattern.push(match c {
                'y' | 'Y' => 'y',
                'd' | 'D' => 'd',
                other => other,
            });
        } else {
            if c.len_utf16() == 1 && is_whitespace(c) {
                months.clear();
            }
            pattern.push(c);
        }
        j += 1;
    }
    let pattern: String = pattern.into_iter().collect();
    Ok(
        match ExcelStyleDateFormatter::with_literals(&pattern, literals, calendar) {
            Ok(formatter) => Created::Format(Format::Date(formatter)),
            Err(_) => Created::General,
        },
    )
}

/// The fraction format `createFormat` makes of a format with a `#/` or `?/`.
fn create_fraction_format(format: &str) -> Result<Format, FormatError> {
    for chunk in java::split(format, ';') {
        let chunk = chunk.replace('?', "#");
        let chunk = FRACTION_STRIPPER.replace_all(&chunk, " ");
        let chunk = SPACES.replace_all(&chunk, " ");
        if let Some(found) = FRACTION.captures(&chunk) {
            let whole_part = if found.get(1).is_some() {
                DEFAULT_FRACTION_WHOLE_PART
            } else {
                ""
            };
            let denominator = found.get(3).map_or("", |m| m.as_str());
            return FractionFormat::new(whole_part, denominator).map(Format::Fraction);
        }
    }
    FractionFormat::new(DEFAULT_FRACTION_WHOLE_PART, DEFAULT_FRACTION_PART).map(Format::Fraction)
}

/// `createNumberFormat`, the general format where the pattern does not compile. An alternate
/// grouping separator is the whole character, where POI takes only the first half of one outside
/// the Basic Multilingual Plane and leaves the other in the format (#186).
fn create_number_format(format: &str) -> Result<Created, FormatError> {
    let mut format = clean_format_for_number(format);
    let mut grouping = ',';
    if let Some(found) = ALTERNATE_GROUPING.captures(&format) {
        let separator = found
            .get(2)
            .and_then(|m| m.as_str().chars().next())
            .ok_or_else(|| FormatError::Thrown("The grouping has no separator".to_owned()))?;
        if separator != ',' {
            grouping = separator;
            let old_part = found.get(1).map_or("", |m| m.as_str()).to_owned();
            let new_part = old_part.replace(separator, ",");
            format = format.replace(&old_part, &new_part);
        }
    }
    if DECIMAL_FORMAT_FIX.is_match(&format) {
        format = "#".to_owned();
    }
    Ok(match ScaledDecimalFormat::new(&format, grouping) {
        Ok(scaled) => Created::Format(Format::Decimal(scaled)),
        Err(_) => Created::General,
    })
}

/// `cleanFormatForNumber`: without the `_` and `*` spacing, quotes and backslashes, and the sign
/// of an exponent. The character a `_` or `*` spaces by is taken out whole, where POI takes out
/// only the first half of one outside the Basic Multilingual Plane and leaves the other in the
/// format (#186).
fn clean_format_for_number(format: &str) -> String {
    let mut chars: Vec<char> = format.replace("\\%", "'%'").chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '_' || c == '*') && !(i > 0 && chars[i - 1] == '\\') {
            if i + 1 < chars.len() {
                chars.remove(i + 1);
            }
            chars.remove(i);
            continue;
        }
        i += 1;
    }
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let sign = (c == '+' || c == '-') && i > 0 && chars[i - 1] == 'E';
        if c == '\\' || c == '"' || sign {
            chars.remove(i);
            continue;
        }
        i += 1;
    }
    chars.into_iter().collect()
}

/// The text of an error code, as POI's `FormulaError` names it; `None` for a code it lacks.
pub fn error_text(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        0x2B => "#GETTING_DATA",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMATS: &[u8] = include_bytes!("../../../tests/fixtures/xlsx/formats.tsv.gz");

    fn unescape(field: &str) -> String {
        let mut text = String::new();
        let mut chars = field.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                text.push(c);
                continue;
            }
            match chars.next() {
                Some('t') => text.push('\t'),
                Some('n') => text.push('\n'),
                Some('r') => text.push('\r'),
                Some(other) => text.push(other),
                None => panic!("formats.tsv ends a field in a backslash: {field}"),
            }
        }
        text
    }

    /// Whether the text of `value` departs from Java's `expected` as the port means it to
    /// (#92, #186): a negative number in a date format, which Java formats with the date format
    /// it cached for a value before, and which is no date, so the port formats it as a number; a
    /// date format with literal texts, which the port writes as they are; an elapsed time Java
    /// fails, or counts in floats that cannot hold its seconds; an engineering format, one of
    /// `?` digits, or one that scales by commas, which the port divides by exactly; a zero that a
    /// format of sections without a `0` digit leaves out; or
    /// a number in the general format, which the port writes to 15 significant digits where
    /// Java writes it to fewer, ten after the point or, in spellings of `General` other than
    /// `General` and `general`, in `@` and in formats that do not compile, POI's of ten
    /// significant digits, six in scientific notation; a decimal point no digit follows, which
    /// the port writes as Excel does; or a date whose time the port rounds to the fraction of a
    /// second the format shows, where Java truncates it.
    fn departs(value: f64, index: i32, format: &str, actual: &str, expected: &str) -> bool {
        if value < 0.0 && is_a_date_format(index, format) {
            return true;
        }
        if !expected.contains('.') && actual.replacen('.', "", 1) == expected {
            return true;
        }
        if is_a_date_format(index, format) {
            let digits = date_format::second_fraction_digits(&format.to_lowercase());
            let shown = value * 86_400.0 * 10f64.powi(digits);
            if shown - shown.floor() >= 0.5 - 1e-6 {
                return true;
            }
        }
        if is_a_date_format(index, format) && !Literals::of(format).texts.is_empty() {
            return true;
        }
        if !format.contains('0') && format.contains(';') && expected.replacen('0', "", 1) == actual
        {
            return true;
        }
        let cleaned = clean_format_for_number(format);
        if Engineering::of(&cleaned).is_some()
            || (QuestionMarks::of(&cleaned).is_some() && !format.contains('/'))
            || scaling_commas(&cleaned).is_some()
        {
            return true;
        }
        let lower = format.to_lowercase();
        let elapsed = ["[h", "[m", "[s"].iter().any(|start| lower.contains(start));
        if elapsed && (expected.contains('[') || value.abs() * 86_400.0 >= f64::from(1 << 24)) {
            return true;
        }
        let general = DataFormatter::new(false)
            .format_raw(value, 0, "General")
            .unwrap()
            .replacen("E+", "E", 1);
        let number = |text: &str| text.replacen("E+", "E", 1).parse::<f64>().ok();
        match (number(actual), number(expected)) {
            (Some(ours), Some(java)) => {
                actual.replacen("E+", "E", 1) == general
                    && (ours - java).abs() <= 5e-6 * ours.abs() + 5e-11
            }
            _ => false,
        }
    }

    #[test]
    fn formats_as_tika_excel_data_formatter_does() {
        use std::io::Read;
        let mut text = String::new();
        flate2::read::GzDecoder::new(FORMATS)
            .read_to_string(&mut text)
            .unwrap();
        let mut differences = Vec::new();
        let mut departed = 0;
        let mut group = None;
        let mut formatter = DataFormatter::new(false);
        let mut count = 0;
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let [number, index, format, bits, expected] = fields.as_slice() else {
                panic!("formats.tsv has a bad line: {line}");
            };
            if group != Some(*number) {
                group = Some(*number);
                formatter = DataFormatter::new(false);
            }
            let format = unescape(format);
            let value = f64::from_bits(u64::from_str_radix(bits, 16).unwrap());
            let expected = unescape(expected);
            let actual = match formatter.format_raw(value, index.parse().unwrap(), &format) {
                Ok(text) => text,
                Err(FormatError::Thrown(_)) => "!thrown".to_owned(),
                Err(FormatError::Unported(what)) => format!("!unported {what}"),
            };
            count += 1;
            if actual == expected {
                continue;
            }
            let index: i32 = index.parse().unwrap();
            if departs(value, index, &format, &actual, &expected) {
                departed += 1;
                continue;
            }
            differences.push(format!(
                "{format:?} {value:e}: {actual:?}, Java {expected:?}"
            ));
        }
        assert!(count > 9000, "formats.tsv has only {count} cases");
        assert!(departed < 1100, "{departed} cases depart from Java's");
        assert!(
            differences.is_empty(),
            "{} of {count} differ:\n{}",
            differences.len(),
            differences.join("\n")
        );
    }

    /// Each value is formatted from its own value, `General` in any case to 15 significant
    /// digits, and dates in the document's date system (#92).
    #[test]
    fn formats_each_value_as_it_is() {
        let mut formatter = DataFormatter::new(false);
        let third = 1.0 / 3.0;
        for general in ["General", "GENERAL", "@"] {
            assert_eq!(
                formatter.format_raw(third, 164, general).unwrap(),
                "0.333333333333333"
            );
        }
        assert_eq!(
            formatter
                .format_raw(0.000123456789012345, 0, "General")
                .unwrap(),
            "0.000123456789012345"
        );
        assert_eq!(
            formatter.format_raw(45000.0, 164, "yyyy-mm-dd").unwrap(),
            "2023-03-15"
        );
        assert_eq!(
            formatter.format_raw(-1.0, 164, "yyyy-mm-dd").unwrap(),
            "-1.0"
        );
        assert_eq!(formatter.format_raw(1.5, 164, "[h]:mm").unwrap(), "36:00");
        assert_eq!(formatter.format_raw(0.5, 164, "[h]:mm").unwrap(), "12:00");
        let mut formatter = DataFormatter::new(true);
        assert_eq!(
            formatter.format_raw(45000.0, 164, "yyyy-mm-dd").unwrap(),
            "2027-03-16"
        );
    }

    /// A date a section of a format writes without a date format of its own is written as
    /// `Date.toString` writes it, in UTC (#186).
    #[test]
    fn writes_dates_without_a_date_format_as_date_to_string_does() {
        assert_eq!(
            cell_format::date_to_string(1_678_838_400_000, &TimeZone::fixed(0)).unwrap(),
            "Wed Mar 15 00:00:00 UTC 2023"
        );
        let mut formatter = DataFormatter::new(false);
        assert_eq!(
            formatter
                .format_raw(45000.0, 14, "General;General;General")
                .unwrap(),
            "Wed Mar 15 00:00:00 UTC 2023"
        );
    }

    /// A character outside the Basic Multilingual Plane is spaced by and grouped with whole
    /// (#186).
    #[test]
    fn spaces_and_groups_by_characters_outside_the_basic_multilingual_plane() {
        let mut formatter = DataFormatter::new(false);
        assert_eq!(
            formatter
                .format_raw(1234.0, 164, "#,##0_\u{1F600}")
                .unwrap(),
            "1,234"
        );
        assert_eq!(
            formatter
                .format_raw(1_234_567.0, 165, "#\u{1F600}##0")
                .unwrap(),
            "1\u{1F600}234\u{1F600}567"
        );
    }

    /// Formats POI writes otherwise than Excel displays them are written as Excel does (#186):
    /// literal texts of dates, elapsed times with them, `?` digits, scaling by commas before a
    /// suffix, engineering exponents and the zero section of accounting formats.
    #[test]
    fn formats_as_excel_displays_where_poi_does_not() {
        let mut formatter = DataFormatter::new(false);
        for (value, format, excel) in [
            (45000.0, "d\".\"m\".\"yyyy", "15.3.2023"),
            (45000.0, "yyyy\"年\"m\"月\"d\"日\"", "2023年3月15日"),
            (0.5, "h \"hours\"", "12 hours"),
            (0.5, "[h]\"h\" mm\"m\"", "12h 00m"),
            (0.001, "[ss].00", "86.40"),
            (1_234_567.891, "[h]:mm:ss", "29629629:23:02"),
            (1234.5678, "0.0??", "1234.568"),
            (5.0, "??0", "  5"),
            (1234.5, "#,##0.0?", "1,234.5 "),
            (1_234_567.0, "0.0,,\"M\"", "1.2M"),
            (1_234_567.0, "#,##0,\"K\"", "1,235K"),
            (12345.678, "##0.0E+0", "12.3E+3"),
            (0.5, "##0.0E+0", "500.0E-3"),
            (999_960.0, "##0.0E+0", "1.0E+6"),
            (5.0, "0.##", "5."),
            (5.25, "0.##", "5.25"),
            (0.5, "0.##%", "50.%"),
            (5.0, "0.##E+0", "5.E+0"),
            (45000.0 + 36_059.6 / 86_400.0, "h:mm:ss", "10:01:00"),
            (45000.0 + 36_059.6 / 86_400.0, "h:mm", "10:01"),
            (45000.0 + 36_059.66 / 86_400.0, "h:mm:ss.0", "10:00:59.7"),
            (45000.0, "[$-411]ggge\"年\"m\"月\"d\"日\"", "令和5年3月15日"),
            (45000.0, "[$-411]ge.m.d", "R5.3.15"),
            (32515.0, "[$-411]ggee", "昭64"),
            (32516.0, "[$-411]ggee", "平01"),
            (45000.0, "ggge\"年\"m\"月\"d\"日\"", "2023年3月15日"),
            (45000.0, "B1yyyy-mm-dd", "2023-03-15"),
            (
                0.0,
                "_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"??_);_(@_)",
                "-",
            ),
        ] {
            assert_eq!(
                formatter.format_raw(value, 200, format).unwrap(),
                excel,
                "{format}"
            );
        }
    }

    /// Formats POI writes as Excel displays them are written as before.
    #[test]
    fn keeps_what_poi_writes_as_excel_does() {
        let mut formatter = DataFormatter::new(false);
        for (value, format, excel) in [
            (1234.5, "0.00", "1234.50"),
            (1_234_567.0, "#,##0,", "1,235"),
            (12345.678, "0.00E+00", "1.23E+04"),
            (45000.0, "yyyy\\-mm\\-dd", "2023-03-15"),
            (1.5, "[h]:mm", "36:00"),
            (0.5, "# ??/??", "1/2"),
            (
                1234.5,
                "_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"??_);_(@_)",
                "1,234.50",
            ),
            (0.0, "0;-0;0", "0"),
        ] {
            assert_eq!(
                formatter.format_raw(value, 200, format).unwrap(),
                excel,
                "{format}"
            );
        }
    }

    #[test]
    fn reads_replacements_as_java_does() {
        let pattern = regex("(b)");
        assert_eq!(java_replace_all(&pattern, "abc", r"\$x").unwrap(), "a$xc");
        assert_eq!(java_replace_all(&pattern, "abc", "[$1]").unwrap(), "a[b]c");
        assert!(java_replace_all(&pattern, "abc", "$").is_err());
        assert!(java_replace_all(&pattern, "abc", "$2").is_err());
        assert_eq!(java_replace_all(&pattern, "abc", "$10").unwrap(), "ab0c");
    }

    #[test]
    fn finds_trailing_commas_as_java_does() {
        assert_eq!(trailing_commas("#,##0,,"), Some(5..7));
        assert_eq!(trailing_commas("0,,\n"), Some(1..3));
        assert_eq!(trailing_commas("#,##0"), None);
    }
}
