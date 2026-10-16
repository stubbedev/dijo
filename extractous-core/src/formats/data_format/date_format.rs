//! Dates as POI's `DataFormatter` formats them: `DateUtil`'s test for a date format and its
//! conversion of a cell value to a `Date` in the default time zone, and the `SimpleDateFormat`,
//! with the `en_US` symbols, that its `ExcelStyleDateFormatter` extends with elapsed hours,
//! minutes and seconds and one-letter month names.

use std::sync::LazyLock;

use regex::Regex;

use super::{FormatError, DOT};
use crate::java::decimal::Rounding;
use crate::java::decimal_format::DecimalFormat;
use crate::java::simple_date_format::SimpleDateFormat;
use crate::java::time::{CalendarFields, TimeZone};

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
