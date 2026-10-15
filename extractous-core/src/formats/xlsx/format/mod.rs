//! Cell values as Tika's `TikaExcelDataFormatter` writes them: POI's `DataFormatter` in the
//! `en_US` locale the native image formats in, with Tika's `General` format of 15 significant
//! digits, dates in Java's default time zone and the 1900 date system, as the three-argument
//! `formatRawCellContents` Tika's sheet handler calls always uses.
//!
//! The formats a format string makes are cached by it, as `DataFormatter` caches them, so a
//! format made for one value is the one later values of that string get, and a date format
//! keeps the last cell value it was given.

mod bigint;
mod cell_format;
pub(crate) mod date_format;
mod decimal;
mod decimal_format;
pub(crate) mod dtoa;
mod fraction;
mod number_text;
mod printf;

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::formats::java::trim;
use crate::formats::java_time::TimeZone;
use crate::unicode::{is_letter, is_whitespace};
use cell_format::{CellFormat, CellValue};
use date_format::{is_a_date_format, is_valid_excel_date, java_date, ExcelStyleDateFormatter};
use decimal::{BigDecimal, Rounding};
use decimal_format::DecimalFormat;
use dtoa::java_double_to_string;
use fraction::FractionFormat;

/// Java's `.` in a pattern: any character but a line terminator.
pub(crate) const DOT: &str = r"[^\n\r\x{85}\x{2028}\x{2029}]";
/// Java's `\s` in a pattern.
pub(crate) const SPACE: &str = r"[ \t\n\x0B\x0C\r]";

/// Why a format gave no text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FormatError {
    /// Java throws, for the reason given; `DataFormatter` and Tika's sheet handler catch it and
    /// fall back.
    Thrown(String),
    /// Java writes what this port does not.
    Unported(&'static str),
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
pub(crate) fn builtin_format(index: i32) -> Option<&'static str> {
    usize::try_from(index)
        .ok()
        .and_then(|index| BUILTIN_FORMATS.get(index).copied())
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
/// The format strings `TikaExcelDataFormatter` gives Tika's general format.
const TIKA_GENERAL_FORMATS: [&str; 2] = ["General", "general"];
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

/// A `DecimalFormat` with `DataFormatter`'s scaling by thousands for trailing commas,
/// `InternalDecimalFormatWithScale`.
#[derive(Debug, Clone, PartialEq)]
struct ScaledDecimalFormat {
    format: DecimalFormat,
    /// The power of ten the trailing commas divide by.
    divider: Option<usize>,
}

impl ScaledDecimalFormat {
    fn new(pattern: &str, grouping: char) -> Result<Self, String> {
        let commas = trailing_commas(pattern);
        let trimmed = match &commas {
            Some(range) => format!("{}{}", &pattern[..range.start], &pattern[range.end..]),
            None => pattern.to_owned(),
        };
        let format =
            DecimalFormat::new(&trimmed, Rounding::HalfUp)?.with_grouping_separator(grouping);
        Ok(Self {
            format,
            divider: commas.map(|range| 3 * range.len()),
        })
    }
}

/// A general number format: Tika's or POI's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum General {
    /// `TikaExcelGeneralFormat`: 15 significant digits.
    Tika,
    /// `ExcelGeneralNumberFormat`, `DataFormatter`'s general number format: 10.
    Excel,
}

impl General {
    fn formats(self) -> &'static GeneralFormats {
        match self {
            General::Tika => &TIKA_GENERAL,
            General::Excel => &EXCEL_GENERAL,
        }
    }

    fn is_scientific(self, absolute: f64) -> bool {
        let tiny = absolute > 0.0
            && match self {
                General::Tika => absolute <= 1e-15,
                General::Excel => absolute <= 1e-10,
            };
        tiny || match self {
            General::Tika => absolute > 1e15,
            General::Excel => absolute >= 1e11,
        }
    }

    fn is_whole(self, value: f64, absolute: f64) -> bool {
        value.floor() == value
            || match self {
                General::Tika => absolute > 1e15,
                General::Excel => absolute >= 1e10,
            }
    }

    /// The value at the format's significant digits.
    fn round(self, value: f64) -> Result<f64, FormatError> {
        let non_finite = || FormatError::Thrown("A BigDecimal of a non-finite double".to_owned());
        Ok(match self {
            General::Tika => BigDecimal::from_f64(value)
                .ok_or_else(non_finite)?
                .round_to_precision(15, Rounding::HalfUp),
            General::Excel => BigDecimal::value_of(value)
                .ok_or_else(non_finite)?
                .round_to_precision(10, Rounding::HalfUp),
        }
        .to_f64())
    }

    /// `format(Object)`: scientific for the very large and very small, whole numbers whole, and
    /// others at their significant digits.
    fn format(self, input: Input) -> Result<String, FormatError> {
        let formats = self.formats();
        if let Input::Date(_) = input {
            return input.format_with(&formats.integer);
        }
        let value = input.double_value()?;
        if value.is_infinite() || value.is_nan() {
            return input.format_with(&formats.integer);
        }
        let absolute = value.abs();
        if self.is_scientific(absolute) {
            return input.format_with(&formats.scientific);
        }
        if self.is_whole(value, absolute) {
            return input.format_with(&formats.integer);
        }
        Ok(formats.decimal.format_f64(self.round(value)?))
    }
}

/// A format `DataFormatter` makes for a format string.
#[derive(Debug, Clone, PartialEq)]
enum Format {
    General(General),
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

/// The formats of Tika's and POI's general formats.
struct GeneralFormats {
    scientific: DecimalFormat,
    integer: DecimalFormat,
    decimal: DecimalFormat,
}

static TIKA_GENERAL: LazyLock<GeneralFormats> =
    LazyLock::new(|| general_formats("0.##############E0"));
static EXCEL_GENERAL: LazyLock<GeneralFormats> = LazyLock::new(|| general_formats("0.#####E0"));
static NINE_DIGITS: LazyLock<DecimalFormat> =
    LazyLock::new(|| DecimalFormat::new("000000000", Rounding::HalfEven).expect("a valid pattern"));
static TEN_DIGITS: LazyLock<DecimalFormat> = LazyLock::new(|| {
    DecimalFormat::new("##########", Rounding::HalfEven).expect("a valid pattern")
});

fn general_formats(scientific: &str) -> GeneralFormats {
    let format =
        |pattern: &str| DecimalFormat::new(pattern, Rounding::HalfUp).expect("a valid pattern");
    GeneralFormats {
        scientific: format(scientific),
        integer: format("#"),
        decimal: format("#.##########"),
    }
}

impl Format {
    /// `format(Object)`.
    fn format(&self, input: Input, zone: &TimeZone) -> Result<String, FormatError> {
        match self {
            Format::General(general) => general.format(input),
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
            Format::Date(formatter) => formatter.format(input.long_value(), zone),
            Format::Fraction(fraction) => fraction.format(input.double_value()?),
            Format::Decimal(scaled) => match (input, scaled.divider) {
                (Input::Date(_), Some(_)) => Err(FormatError::Thrown(
                    "cannot scaleInput of type java.util.Date".to_owned(),
                )),
                (Input::Double(value), Some(power)) => {
                    let divider: f64 = format!("1e{power}")
                        .parse()
                        .expect("a power of ten is a double");
                    Ok(scaled.format.format_f64(value / divider))
                }
                (Input::Decimal(value), Some(power)) => Ok(scaled
                    .format
                    .format_decimal(&value.divide_by_power_of_ten(power, Rounding::HalfUp))),
                (input, None) => input.format_with(&scaled.format),
            },
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
    /// A cached format, by its format string.
    Cached(String),
    /// The general number format, `DataFormatter`'s own, uncached.
    General,
    /// The text `CellFormat` gave, which `CellFormatResultWrapper` writes trimmed.
    Text(String),
    /// No format: the value is written as `Double.toString` writes it.
    None,
}

/// `TikaExcelDataFormatter` for one document, with the formats it has made.
pub(crate) struct DataFormatter {
    zone: &'static TimeZone,
    formats: HashMap<String, Format>,
    cell_formats: HashMap<String, Result<CellFormat, String>>,
}

impl DataFormatter {
    /// A formatter writing dates in `zone`, Java's default time zone.
    pub(crate) fn new(zone: &'static TimeZone) -> Self {
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
        for format in TIKA_GENERAL_FORMATS {
            formats.insert(format.to_owned(), Format::General(General::Tika));
        }
        Self {
            zone,
            formats,
            cell_formats: HashMap::new(),
        }
    }

    /// `formatRawCellContents(value, formatIndex, formatString)`: the text of a number cell; an
    /// error where Java throws, for which Tika writes the cell's raw value.
    pub(crate) fn format_raw(
        &mut self,
        value: f64,
        index: i32,
        format: &str,
    ) -> Result<String, FormatError> {
        if is_a_date_format(index, format) && is_valid_excel_date(value) {
            let chosen = self.get_format(value, index, format)?;
            let instant = java_date(value, self.zone).ok_or_else(|| {
                FormatError::Thrown("A valid Excel date has no Java date".to_owned())
            })?;
            return match chosen {
                Chosen::Text(text) => Ok(text),
                Chosen::None => Err(FormatError::Unported(
                    "a date in the default format, which writes its time zone's name",
                )),
                Chosen::General => General::Excel.format(Input::Date(instant)),
                Chosen::Cached(key) => {
                    let zone = self.zone;
                    let format = self.cached(&key)?;
                    if let Format::Date(formatter) = format {
                        formatter.date_to_be_formatted = value;
                    }
                    format.format(Input::Date(instant), zone)
                }
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
        let zone = self.zone;
        let result = match chosen {
            Chosen::None => return Ok(java_double_to_string(value)),
            Chosen::Text(text) => text,
            Chosen::General => General::Excel.format(input)?,
            Chosen::Cached(key) => self.cached(&key)?.format(input, zone)?,
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

    fn cached(&mut self, key: &str) -> Result<&mut Format, FormatError> {
        self.formats
            .get_mut(key)
            .ok_or_else(|| FormatError::Thrown(format!("The format {key:?} was not cached")))
    }

    /// `getFormat(cellValue, formatIndex, formatStr, false)`.
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
        if self.formats.contains_key(&format) {
            return Ok(Chosen::Cached(format));
        }
        if format.eq_ignore_ascii_case("General") || format == "@" {
            return Ok(Chosen::General);
        }
        let created = match self.create_format(value, index, &format)? {
            Created::General => Format::General(General::Excel),
            Created::Format(created) => created,
            Created::None => return Ok(Chosen::None),
        };
        self.formats.insert(format.clone(), created);
        Ok(Chosen::Cached(format))
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
            java_date(value, self.zone).map(CellValue::Date)
        } else {
            Some(CellValue::Number(value))
        };
        cell_format.apply(input, self.zone)
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
        while let Some(found) = LOCALE_GROUP.find(&format) {
            let matched = found.as_str();
            let dollar = matched.find('$').map_or(0, |at| at + 1);
            let dash = matched.find('-').unwrap_or(matched.len());
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
            return Ok(create_date_format(&format));
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

/// `createDateFormat`, the general format where the date format does not compile.
fn create_date_format(format: &str) -> Created {
    let mut format = format
        .replace("\\-", "-")
        .replace("\\,", ",")
        .replace("\\.", ".")
        .replace("\\ ", " ")
        .replace("\\/", "/")
        .replace(";@", "")
        .replace("\"/\"", "/")
        .replace("\"\"", "'")
        .replace("\\T", "'T'");
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
    match ExcelStyleDateFormatter::new(&pattern) {
        Ok(formatter) => Created::Format(Format::Date(formatter)),
        Err(_) => Created::General,
    }
}

/// The fraction format `createFormat` makes of a format with a `#/` or `?/`.
fn create_fraction_format(format: &str) -> Result<Format, FormatError> {
    for chunk in java_split(format, ';') {
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

/// `String.split(separator)`: the pieces between the separators, trailing empty ones dropped.
fn java_split(text: &str, separator: char) -> Vec<&str> {
    let mut pieces: Vec<&str> = text.split(separator).collect();
    while pieces.len() > 1 && pieces.last().is_some_and(|piece| piece.is_empty()) {
        pieces.pop();
    }
    if pieces == [""] && !text.is_empty() {
        pieces.clear();
    }
    pieces
}

/// `createNumberFormat`, the general format where the pattern does not compile.
fn create_number_format(format: &str) -> Result<Created, FormatError> {
    let mut format = clean_format_for_number(format)?;
    let mut grouping = ',';
    if let Some(found) = ALTERNATE_GROUPING.captures(&format) {
        let separator = found
            .get(2)
            .and_then(|m| m.as_str().chars().next())
            .ok_or_else(|| FormatError::Thrown("The grouping has no separator".to_owned()))?;
        if separator != ',' {
            if separator.len_utf16() != 1 {
                return Err(FormatError::Unported(
                    "a grouping separator outside the Basic Multilingual Plane",
                ));
            }
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
/// of an exponent.
fn clean_format_for_number(format: &str) -> Result<String, FormatError> {
    let mut units: Vec<u16> = format.replace("\\%", "'%'").encode_utf16().collect();
    let backslash = u16::from(b'\\');
    let mut i = 0;
    while i < units.len() {
        let c = units[i];
        if (c == u16::from(b'_') || c == u16::from(b'*')) && !(i > 0 && units[i - 1] == backslash) {
            if i + 1 < units.len() {
                units.remove(i + 1);
            }
            units.remove(i);
            continue;
        }
        i += 1;
    }
    let mut i = 0;
    while i < units.len() {
        let c = units[i];
        let sign = (c == u16::from(b'+') || c == u16::from(b'-'))
            && i > 0
            && units[i - 1] == u16::from(b'E');
        if c == backslash || c == u16::from(b'"') || sign {
            units.remove(i);
            continue;
        }
        i += 1;
    }
    String::from_utf16(&units)
        .map_err(|_| FormatError::Unported("a number format left with half a surrogate pair"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMATS: &[u8] = include_bytes!("../../../../tests/fixtures/xlsx/formats.tsv.gz");

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

    #[test]
    fn formats_as_tika_excel_data_formatter_does() {
        use std::io::Read;
        static COPENHAGEN: LazyLock<TimeZone> =
            LazyLock::new(|| crate::formats::java_time::tests::zone("Europe/Copenhagen"));
        let mut text = String::new();
        flate2::read::GzDecoder::new(FORMATS)
            .read_to_string(&mut text)
            .unwrap();
        let mut differences = Vec::new();
        let mut group = None;
        let mut formatter = DataFormatter::new(&COPENHAGEN);
        let mut count = 0;
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let [number, index, format, bits, expected] = fields.as_slice() else {
                panic!("formats.tsv has a bad line: {line}");
            };
            if group != Some(*number) {
                group = Some(*number);
                formatter = DataFormatter::new(&COPENHAGEN);
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
            if actual != expected {
                differences.push(format!(
                    "{format:?} {value:e}: {actual:?}, Java {expected:?}"
                ));
            }
        }
        assert!(count > 9000, "formats.tsv has only {count} cases");
        assert!(
            differences.is_empty(),
            "{} of {count} differ:\n{}",
            differences.len(),
            differences.join("\n")
        );
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
