//! Dates as POI's `DataFormatter` formats them: `DateUtil`'s test for a date format and its
//! conversion of a cell value to a `Date` in the default time zone, and the `SimpleDateFormat`,
//! with the `en_US` symbols, that its `ExcelStyleDateFormatter` extends with elapsed hours,
//! minutes and seconds and one-letter month names.

use std::sync::LazyLock;

use regex::Regex;

use super::{FormatError, DOT};
use crate::formats::java_text::decimal::Rounding;
use crate::formats::java_text::decimal_format::DecimalFormat;
use crate::formats::java_time::{self, CalendarFields, LocalFields, TimeZone};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const SHORT_WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const PATTERN_LETTERS: &str = "GyMdkHmsSEDFwWahKzZYuXL";
const DAY_MILLISECONDS: f64 = 86_400_000.0;
pub(crate) const MMMMM_START: char = '\u{e001}';
pub(crate) const MMMMM_TRUNCATE: char = '\u{e002}';
const H_BRACKET: char = '\u{e010}';
const HH_BRACKET: char = '\u{e011}';
const M_BRACKET: char = '\u{e012}';
const MM_BRACKET: char = '\u{e013}';
const S_BRACKET: char = '\u{e014}';
const SS_BRACKET: char = '\u{e015}';
const L_BRACKET: char = '\u{e016}';
const LL_BRACKET: char = '\u{e017}';

/// A field of a compiled `SimpleDateFormat` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    Literal(String),
    Pattern(char, usize),
}

/// `java.text.SimpleDateFormat` for formatting, with the `en_US` `DateFormatSymbols`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SimpleDateFormat {
    fields: Vec<Field>,
}

impl SimpleDateFormat {
    /// `SimpleDateFormat.compile`: an error where the pattern throws.
    pub(crate) fn new(pattern: &str) -> Result<Self, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut fields: Vec<Field> = Vec::new();
        let mut literal = String::new();
        let mut in_quote = false;
        let mut last: Option<(char, usize)> = None;
        let flush_pattern = |fields: &mut Vec<Field>, last: &mut Option<(char, usize)>| {
            if let Some((letter, count)) = last.take() {
                fields.push(Field::Pattern(letter, count));
            }
        };
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    i += 1;
                    flush_pattern(&mut fields, &mut last);
                    literal.push('\'');
                } else {
                    flush_pattern(&mut fields, &mut last);
                    in_quote = !in_quote;
                }
                i += 1;
                continue;
            }
            if in_quote || !c.is_ascii_alphabetic() {
                flush_pattern(&mut fields, &mut last);
                literal.push(c);
                i += 1;
                continue;
            }
            if !PATTERN_LETTERS.contains(c) {
                return Err(format!("Illegal pattern character '{c}'"));
            }
            if !literal.is_empty() {
                fields.push(Field::Literal(std::mem::take(&mut literal)));
            }
            match &mut last {
                Some((letter, count)) if *letter == c => *count += 1,
                _ => {
                    flush_pattern(&mut fields, &mut last);
                    last = Some((c, 1));
                }
            }
            i += 1;
        }
        if in_quote {
            return Err("Unterminated quote".to_owned());
        }
        flush_pattern(&mut fields, &mut last);
        if !literal.is_empty() {
            fields.push(Field::Literal(literal));
        }
        Ok(Self { fields })
    }

    /// The instant `epoch_millis` as the pattern writes it in `zone`; an error for a time
    /// zone name, which this port does not write.
    pub(crate) fn format(&self, epoch_millis: i64, zone: &TimeZone) -> Result<String, FormatError> {
        Ok(self
            .pieces(epoch_millis, zone)?
            .into_iter()
            .map(|(piece, _)| piece)
            .collect())
    }

    /// `formatToCharacterIterator`: each piece of the text with the pattern letter that wrote
    /// it, or `None` for a literal.
    pub(crate) fn pieces(
        &self,
        epoch_millis: i64,
        zone: &TimeZone,
    ) -> Result<Vec<(String, Option<char>)>, FormatError> {
        let local = java_time::local_fields(epoch_millis, zone);
        self.fields
            .iter()
            .map(|field| match field {
                Field::Literal(literal) => Ok((literal.clone(), None)),
                Field::Pattern(letter, count) => {
                    format_field(*letter, *count, &local).map(|piece| (piece, Some(*letter)))
                }
            })
            .collect()
    }
}

/// `zeroPaddingNumber`: at least `min` digits, at most the last `max`.
fn zero_padding(value: i64, min: usize, max: usize) -> String {
    let digits = value.unsigned_abs().to_string();
    let digits = if digits.len() > max {
        &digits[digits.len() - max..]
    } else {
        &digits[..]
    };
    let mut text = String::new();
    if value < 0 {
        text.push('-');
    }
    for _ in digits.len()..min {
        text.push('0');
    }
    text.push_str(digits);
    text
}

fn format_field(letter: char, count: usize, local: &LocalFields) -> Result<String, FormatError> {
    let number = |value: i64| zero_padding(value, count, usize::MAX);
    let piece = match letter {
        'G' => (if local.before_christ { "BC" } else { "AD" }).to_owned(),
        'y' | 'Y' => {
            let value = if letter == 'y' {
                local.year
            } else {
                local.week_year
            };
            if count == 2 {
                zero_padding(value, 2, 2)
            } else {
                number(value)
            }
        }
        'M' | 'L' => match count {
            4.. => MONTHS[local.month as usize].to_owned(),
            3 => SHORT_MONTHS[local.month as usize].to_owned(),
            _ => number(local.month + 1),
        },
        'd' => number(local.day),
        'k' => number(if local.hour == 0 { 24 } else { local.hour }),
        'H' => number(local.hour),
        'm' => number(local.minute),
        's' => number(local.second),
        'S' => number(local.millisecond),
        'E' => {
            let index = (local.day_of_week - 1) as usize;
            if count >= 4 {
                WEEKDAYS[index].to_owned()
            } else {
                SHORT_WEEKDAYS[index].to_owned()
            }
        }
        'u' => number(if local.day_of_week == 1 {
            7
        } else {
            local.day_of_week - 1
        }),
        'D' => number(local.day_of_year),
        'F' => number(local.day_of_week_in_month),
        'w' => number(local.week_of_year),
        'W' => number(local.week_of_month),
        'a' => (if local.hour >= 12 { "PM" } else { "AM" }).to_owned(),
        'h' => number(if local.hour % 12 == 0 {
            12
        } else {
            local.hour % 12
        }),
        'K' => number(local.hour % 12),
        'Z' => {
            let minutes = local.offset / 60_000;
            let sign = if minutes < 0 { '-' } else { '+' };
            let minutes = minutes.abs();
            format!("{sign}{:02}{:02}", minutes / 60, minutes % 60)
        }
        'X' => {
            let minutes = local.offset / 60_000;
            if minutes == 0 {
                "Z".to_owned()
            } else {
                let sign = if minutes < 0 { '-' } else { '+' };
                let minutes = minutes.abs();
                match count {
                    1 => format!("{sign}{:02}", minutes / 60),
                    2 => format!("{sign}{:02}{:02}", minutes / 60, minutes % 60),
                    _ => format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60),
                }
            }
        }
        _ => {
            return Err(FormatError::Unported(
                "a time zone's name, pattern letter z",
            ))
        }
    };
    Ok(piece)
}

/// `ExcelStyleDateFormatter`: a `SimpleDateFormat` whose pattern stands in for Excel's elapsed
/// times, tenths and hundredths of seconds and one-letter months, filled in from the cell value
/// last given it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExcelStyleDateFormatter {
    format: SimpleDateFormat,
    pub(crate) date_to_be_formatted: f64,
}

static SECONDS_THOUSANDTHS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("s{DOT}000")).expect("a valid pattern"));
static SECONDS_HUNDREDTHS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("s{DOT}00")).expect("a valid pattern"));
static SECONDS_TENTHS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("s{DOT}0")).expect("a valid pattern"));
static ONE_LETTER_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x{e001}(\p{L}|\p{P})[\p{L}\p{P}]+\x{e002}").expect("a valid pattern")
});

impl ExcelStyleDateFormatter {
    /// `new ExcelStyleDateFormatter(pattern, symbols)`; an error where `SimpleDateFormat` throws.
    pub(crate) fn new(pattern: &str) -> Result<Self, String> {
        let mut processed = pattern.replace("MMMMM", &format!("{MMMMM_START}MMM{MMMMM_TRUNCATE}"));
        for (from, to) in [
            ("[H]", H_BRACKET),
            ("[HH]", HH_BRACKET),
            ("[m]", M_BRACKET),
            ("[mm]", MM_BRACKET),
            ("[s]", S_BRACKET),
            ("[ss]", SS_BRACKET),
        ] {
            processed = processed.replace(from, &to.to_string());
        }
        processed = processed.replace('T', "'T'").replace("''T''", "'T'");
        processed = SECONDS_THOUSANDTHS
            .replace_all(&processed, "s.SSS")
            .into_owned();
        processed = SECONDS_HUNDREDTHS
            .replace_all(&processed, format!("s.{LL_BRACKET}").as_str())
            .into_owned();
        processed = SECONDS_TENTHS
            .replace_all(&processed, format!("s.{L_BRACKET}").as_str())
            .into_owned();
        Ok(Self {
            format: SimpleDateFormat::new(&processed)?,
            date_to_be_formatted: 0.0,
        })
    }

    /// `format(Date)`.
    pub(crate) fn format(&self, epoch_millis: i64, zone: &TimeZone) -> Result<String, FormatError> {
        let mut text = self.format.format(epoch_millis, zone)?;
        if text.contains(MMMMM_START) {
            text = ONE_LETTER_MONTH.replace_all(&text, "$1").into_owned();
        }
        let down = |value: f32, pattern: &str| {
            DecimalFormat::new(pattern, Rounding::Down)
                .expect("a valid pattern")
                .format_f64(f64::from(value))
        };
        if text.contains(H_BRACKET) || text.contains(HH_BRACKET) {
            let hours = (self.date_to_be_formatted as f32) * 24.0;
            text = text
                .replace(H_BRACKET, &down(hours, "0"))
                .replace(HH_BRACKET, &down(hours, "00"));
        }
        if text.contains(M_BRACKET) || text.contains(MM_BRACKET) {
            let minutes = (self.date_to_be_formatted as f32) * 24.0 * 60.0;
            text = text
                .replace(M_BRACKET, &down(minutes, "0"))
                .replace(MM_BRACKET, &down(minutes, "00"));
        }
        if text.contains(S_BRACKET) || text.contains(SS_BRACKET) {
            let seconds = (self.date_to_be_formatted * 24.0 * 60.0 * 60.0) as f32;
            text = text
                .replace(S_BRACKET, &down(seconds, "0"))
                .replace(SS_BRACKET, &down(seconds, "00"));
        }
        if text.contains(L_BRACKET) || text.contains(LL_BRACKET) {
            let fraction = self.date_to_be_formatted - self.date_to_be_formatted.floor();
            let temp = (fraction * 24.0 * 60.0 * 60.0) as f32;
            let millis = temp - (temp as i32) as f32;
            text = text
                .replace(L_BRACKET, &half_up_f64(f64::from(millis) * 10.0, "0"))
                .replace(LL_BRACKET, &half_up_f64(f64::from(millis) * 100.0, "00"));
        }
        Ok(text)
    }
}

fn half_up_f64(value: f64, pattern: &str) -> String {
    DecimalFormat::new(pattern, Rounding::HalfUp)
        .expect("a valid pattern")
        .format_f64(value)
}

/// `DateUtil.isInternalDateFormat`: the built-in formats 14 to 22 and 45 to 47.
pub(crate) fn is_internal_date_format(index: i32) -> bool {
    matches!(index, 0x0e..=0x16 | 0x2d..=0x2f)
}

static ELAPSED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[([hH]+|[mM]+|[sS]+)]$").expect("a valid pattern"));
static DBNUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[DBNum([123])]").expect("a valid pattern"));
static LOCALE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^\[\$-{DOT}*?]")).expect("a valid pattern"));
static BRACKETED_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[[a-zA-Z]+]").expect("a valid pattern"));
static DATE_LETTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[yYmMdDhHsS]").expect("a valid pattern"));
static DATE_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("^[\\[\\]yYmMdDhHsS\\-T/\u{5e74}\u{6708}\u{65e5},. :\"\\\\]+0* ?[ampAMP/]*$")
        .expect("a valid pattern")
});

/// `DateUtil.isADateFormat(formatIndex, formatString)`.
pub(crate) fn is_a_date_format(index: i32, format: &str) -> bool {
    if is_internal_date_format(index) {
        return true;
    }
    if format.is_empty() {
        return false;
    }
    let chars: Vec<char> = format.chars().collect();
    let mut cleaned = String::with_capacity(format.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if i + 1 < chars.len() {
            let next = chars[i + 1];
            if c == '\\' && matches!(next, '-' | ',' | '.' | ' ' | '\\') {
                i += 1;
                continue;
            }
            if c == ';' && next == '@' {
                i += 2;
                continue;
            }
        }
        cleaned.push(c);
        i += 1;
    }
    if ELAPSED.is_match(&cleaned) {
        return true;
    }
    let cleaned = DBNUM.replace_all(&cleaned, "").into_owned();
    let cleaned = LOCALE_PREFIX.replace_all(&cleaned, "").into_owned();
    let mut cleaned = BRACKETED_WORD.replace_all(&cleaned, "").into_owned();
    let characters: Vec<char> = cleaned.chars().collect();
    if let Some(separator) = characters.iter().position(|&c| c == ';') {
        if 0 < separator && separator < characters.len() - 1 {
            cleaned = characters[..separator].iter().collect();
        }
    }
    if !DATE_LETTER.is_match(&cleaned) {
        return false;
    }
    DATE_ONLY.is_match(&cleaned)
}

/// `DateUtil.isValidExcelDate`: anything above `-Double.MIN_VALUE`.
pub(crate) fn is_valid_excel_date(value: f64) -> bool {
    value > -f64::from_bits(1)
}

/// Java's `(int)` of a double: saturating, and 0 for NaN.
pub(crate) fn java_int(value: f64) -> i32 {
    value as i32
}

/// `DateUtil.getJavaDate(value, false)`: the instant of the cell value in `zone`, the 1900
/// date system, with Excel's 29 February 1900.
pub(crate) fn java_date(value: f64, zone: &TimeZone) -> Option<i64> {
    if !is_valid_excel_date(value) {
        return None;
    }
    let whole_days = java_int(value.floor());
    let milliseconds = java_int((value - f64::from(whole_days)) * DAY_MILLISECONDS + 0.5);
    let day_adjust = if whole_days < 61 { 0 } else { -1 };
    let fields = CalendarFields {
        year: 1900,
        month: 0,
        day: whole_days.wrapping_add(day_adjust),
        hour: 0,
        minute: 0,
        second: 0,
        millisecond: milliseconds,
    };
    Some(fields.epoch_millis(zone))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_date_formats_as_poi_does() {
        for date in [
            "m/d/yy",
            "dd/mm/yyyy;@",
            "[h]:mm:ss",
            "yyyy\\-mm\\-dd",
            "[$-409]mmmm d, yyyy",
            "h:mm AM/PM",
            "[Red]dd.mm.yy",
        ] {
            assert!(is_a_date_format(164, date), "{date}");
        }
        for number in ["0.00", "#,##0", "General", "\"Date\" 0", "@", "mm \"min\""] {
            assert!(!is_a_date_format(164, number), "{number}");
        }
        assert!(is_a_date_format(14, "0.00"));
    }

    #[test]
    fn formats_dates_as_simple_date_format_does() {
        let utc = TimeZone::fixed(0);
        let millis = java_date(45000.75, &utc).unwrap();
        let format = |pattern: &str| {
            SimpleDateFormat::new(pattern)
                .unwrap()
                .format(millis, &utc)
                .unwrap()
        };
        assert_eq!(format("M/d/yy H:mm"), "3/15/23 18:00");
        assert_eq!(format("EEEE d MMMM yyyy"), "Wednesday 15 March 2023");
        assert_eq!(format("hh:mm a"), "06:00 PM");
        assert_eq!(format("yy-'T'-''"), "23-T-'");
        assert!(SimpleDateFormat::new("b").is_err());
        assert!(SimpleDateFormat::new("'x").is_err());
    }

    #[test]
    fn formats_elapsed_times_as_poi_does() {
        let utc = TimeZone::fixed(0);
        let mut format = ExcelStyleDateFormatter::new("[H]:mm:ss").unwrap();
        format.date_to_be_formatted = 1.5;
        let millis = java_date(1.5, &utc).unwrap();
        assert_eq!(format.format(millis, &utc).unwrap(), "36:00:00");
    }
}
