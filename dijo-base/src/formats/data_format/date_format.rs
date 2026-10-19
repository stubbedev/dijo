//! Dates as POI's `DataFormatter` formats them: `DateUtil`'s test for a date format and its
//! conversion of a cell value to a `Date` in a time zone and date system, and the
//! `SimpleDateFormat`, with the `en_US` symbols, that its `ExcelStyleDateFormatter` extends with
//! elapsed hours, minutes and seconds and one-letter month names.
//!
//! Where POI is wrong, these depart from it as Excel displays a date (#186): text a format
//! quotes or escapes with a backslash is written as it is, where POI hands the quotes and any
//! letters between them on to `SimpleDateFormat` as its own, so that `d"."m"."yyyy` gives
//! `15"."3"."2023` and `[h]"h" mm"m"` gives the hours in place of the `h`; and a format is a date
//! format by what it holds outside such text, and by an elapsed time anywhere in it, as Excel
//! tells, where POI fails a format such as `h "hours"` for the letters of its text and
//! `[h] "h"` for want of them.

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
/// The first of the characters a date pattern stands in for its literal texts with, each by its
/// place in them.
const FIRST_LITERAL: u32 = 0xe100;
/// How many literal texts a date pattern stands in for; any further ones are left in it.
const MAX_LITERALS: usize = 0x100;

/// A date format whose literal texts, those it quotes and the characters it escapes, are each
/// stood in for by a character of the private use area, so that nothing reads them as
/// pattern letters, and the texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Literals {
    pub(crate) format: String,
    pub(crate) texts: Vec<String>,
}

impl Literals {
    /// The literal texts of `format`: each run of text between double quotes, an unclosed one
    /// to the end, and each character after a backslash.
    pub(crate) fn of(format: &str) -> Literals {
        let mut stood_in = String::with_capacity(format.len());
        let mut texts = Vec::new();
        let mut chars = format.chars();
        while let Some(c) = chars.next() {
            let text = match c {
                '"' => chars.by_ref().take_while(|&c| c != '"').collect::<String>(),
                '\\' => match chars.next() {
                    Some(escaped) => escaped.to_string(),
                    None => {
                        stood_in.push(c);
                        break;
                    }
                },
                other => {
                    stood_in.push(other);
                    continue;
                }
            };
            match char::from_u32(FIRST_LITERAL + texts.len() as u32) {
                Some(stand_in) if texts.len() < MAX_LITERALS => {
                    stood_in.push(stand_in);
                    texts.push(text);
                }
                _ => stood_in.push_str(&text),
            }
        }
        Literals {
            format: stood_in,
            texts,
        }
    }

    /// The literal text `c` stands in for, if it stands in for one.
    fn text_of(&self, c: char) -> Option<&str> {
        let at = (c as u32).wrapping_sub(FIRST_LITERAL) as usize;
        self.texts.get(at).map(String::as_str)
    }

    /// The format without its literal texts.
    pub(crate) fn outside(&self) -> String {
        self.format
            .chars()
            .filter(|&c| self.text_of(c).is_none())
            .collect()
    }

    /// `text` with each stand-in replaced by the literal text it stands in for.
    fn restore(&self, text: &str) -> String {
        if self.texts.is_empty() {
            return text.to_owned();
        }
        let mut restored = String::with_capacity(text.len());
        for c in text.chars() {
            match self.text_of(c) {
                Some(literal) => restored.push_str(literal),
                None => restored.push(c),
            }
        }
        restored
    }
}

/// `ExcelStyleDateFormatter`: a `SimpleDateFormat` whose pattern stands in for Excel's elapsed
/// times, tenths and hundredths of seconds and one-letter months, filled in from the cell value
/// it formats. Elapsed times are counted in whole milliseconds of a double, as the rest of the
/// time is, where POI counts them in floats, which lose the hours, minutes or seconds of all but
/// the shortest times and may round them otherwise than the rest (#186). POI's fills them in from the value last given it, which a cached formatter may
/// not have been given for the date it formats (#92).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExcelStyleDateFormatter {
    format: SimpleDateFormat,
    /// The literal texts the pattern stands in for.
    literals: Literals,
}

/// The seconds a fraction of a second follows: of the minute, or elapsed. POI takes only those of
/// the minute, and writes the fraction of elapsed seconds as it is in the format (#186).
const SECONDS: &str = r"([s\x{e014}\x{e015}])";
static SECONDS_THOUSANDTHS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{SECONDS}{DOT}000")).expect("a valid pattern"));
static SECONDS_HUNDREDTHS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{SECONDS}{DOT}00")).expect("a valid pattern"));
static SECONDS_TENTHS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{SECONDS}{DOT}0")).expect("a valid pattern"));
static ONE_LETTER_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x{e001}(\p{L}|\p{P})[\p{L}\p{P}]+\x{e002}").expect("a valid pattern")
});

impl ExcelStyleDateFormatter {
    /// `new ExcelStyleDateFormatter(pattern, symbols)`; an error where `SimpleDateFormat` throws.
    #[cfg(test)]
    pub(crate) fn new(pattern: &str) -> Result<Self, String> {
        Self::with_literals(
            pattern,
            Literals {
                format: String::new(),
                texts: Vec::new(),
            },
        )
    }

    /// `new ExcelStyleDateFormatter(pattern, symbols)` of a pattern that stands in for the texts
    /// of `literals`, which it writes in their places; an error where `SimpleDateFormat` throws.
    pub(crate) fn with_literals(pattern: &str, literals: Literals) -> Result<Self, String> {
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
            .replace_all(&processed, "${1}.SSS")
            .into_owned();
        processed = SECONDS_HUNDREDTHS
            .replace_all(&processed, format!("${{1}}.{LL_BRACKET}").as_str())
            .into_owned();
        processed = SECONDS_TENTHS
            .replace_all(&processed, format!("${{1}}.{L_BRACKET}").as_str())
            .into_owned();
        Ok(Self {
            format: SimpleDateFormat::new(&processed)?,
            literals,
        })
    }

    /// `format(Date)` of the cell value `value`, the instant `epoch_millis` in `zone`.
    pub(crate) fn format(
        &self,
        value: f64,
        epoch_millis: i64,
        zone: &TimeZone,
    ) -> Result<String, FormatError> {
        let mut text = self.format.format(epoch_millis, zone)?;
        if text.contains(MMMMM_START) {
            text = ONE_LETTER_MONTH.replace_all(&text, "$1").into_owned();
        }
        let down = |value: f64, pattern: &str| {
            DecimalFormat::new(pattern, Rounding::Down)
                .expect("a valid pattern")
                .format_f64(value)
        };
        let elapsed_millis = (value * DAY_MILLISECONDS).round();
        if text.contains(H_BRACKET) || text.contains(HH_BRACKET) {
            let hours = (elapsed_millis / 3_600_000.0).floor();
            text = text
                .replace(H_BRACKET, &down(hours, "0"))
                .replace(HH_BRACKET, &down(hours, "00"));
        }
        if text.contains(M_BRACKET) || text.contains(MM_BRACKET) {
            let minutes = (elapsed_millis / 60_000.0).floor();
            text = text
                .replace(M_BRACKET, &down(minutes, "0"))
                .replace(MM_BRACKET, &down(minutes, "00"));
        }
        if text.contains(S_BRACKET) || text.contains(SS_BRACKET) {
            let seconds = (elapsed_millis / 1000.0).floor();
            text = text
                .replace(S_BRACKET, &down(seconds, "0"))
                .replace(SS_BRACKET, &down(seconds, "00"));
        }
        if text.contains(L_BRACKET) || text.contains(LL_BRACKET) {
            let fraction = value - value.floor();
            let temp = (fraction * 24.0 * 60.0 * 60.0) as f32;
            let millis = temp - (temp as i32) as f32;
            text = text
                .replace(L_BRACKET, &half_up_f64(f64::from(millis) * 10.0, "0"))
                .replace(LL_BRACKET, &half_up_f64(f64::from(millis) * 100.0, "00"));
        }
        Ok(self.literals.restore(&text))
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
    LazyLock::new(|| Regex::new(r"\[([hH]+|[mM]+|[sS]+)]").expect("a valid pattern"));
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

/// `DateUtil.isADateFormat(formatIndex, formatString)`, of what the format holds outside its
/// literal texts.
pub(crate) fn is_a_date_format(index: i32, format: &str) -> bool {
    if is_internal_date_format(index) {
        return true;
    }
    if format.is_empty() {
        return false;
    }
    let chars: Vec<char> = Literals::of(format).outside().chars().collect();
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

/// `DateUtil.getJavaDate(value, date_1904)`: the instant of the cell value in `zone`, counted
/// in days from 1 January 1904 when `date_1904`, and otherwise in the 1900 date system, with
/// Excel's 29 February 1900.
pub(crate) fn java_date(value: f64, zone: &TimeZone, date_1904: bool) -> Option<i64> {
    if !is_valid_excel_date(value) {
        return None;
    }
    let whole_days = java_int(value.floor());
    let milliseconds = java_int((value - f64::from(whole_days)) * DAY_MILLISECONDS + 0.5);
    let (year, day_adjust) = match (date_1904, whole_days < 61) {
        (true, _) => (1904, 1),
        (false, true) => (1900, 0),
        (false, false) => (1900, -1),
    };
    let fields = CalendarFields {
        year,
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
        for number in ["0.00", "#,##0", "General", "\"Date\" 0", "@", "0 \"days\""] {
            assert!(!is_a_date_format(164, number), "{number}");
        }
        assert!(is_a_date_format(14, "0.00"));
    }

    /// A format is a date format by what it holds outside the texts it quotes and the characters
    /// it escapes, as Excel tells, where POI fails one whose texts hold letters (#186).
    #[test]
    fn tests_date_formats_outside_their_literal_texts() {
        for date in [
            "h \"hours\"",
            "mm \"min\"",
            "yyyy\"年\"m\"月\"d\"日\"",
            "d\\D",
            "[h] \"h\"",
        ] {
            assert!(is_a_date_format(164, date), "{date}");
        }
    }

    /// The texts a date format quotes and the characters it escapes are written as they are,
    /// letters and all, where POI writes the quotes and reads the letters as its own (#186).
    #[test]
    fn writes_the_literal_texts_of_date_formats_as_they_are() {
        let utc = TimeZone::fixed(0);
        let millis = java_date(45000.5, &utc, false).unwrap();
        let literals = Literals::of("d\".\"M\".\"yyyy H\" h\"\\m");
        assert_eq!(literals.texts, [".", ".", " h", "m"]);
        let format =
            ExcelStyleDateFormatter::with_literals(&literals.format.clone(), literals).unwrap();
        assert_eq!(
            format.format(45000.5, millis, &utc).unwrap(),
            "15.3.2023 12 hm"
        );
    }

    #[test]
    fn formats_dates_as_simple_date_format_does() {
        let utc = TimeZone::fixed(0);
        let millis = java_date(45000.75, &utc, false).unwrap();
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
        assert_eq!(format("HH z"), "18 UTC");
        assert_eq!(format("zzzz"), "Coordinated Universal Time");
        let elsewhere = SimpleDateFormat::new("z").unwrap();
        assert!(elsewhere
            .format(millis, &TimeZone::fixed(3_600_000))
            .is_err());
        assert!(SimpleDateFormat::new("b").is_err());
        assert!(SimpleDateFormat::new("'x").is_err());
    }

    #[test]
    fn formats_elapsed_times_as_poi_does() {
        let utc = TimeZone::fixed(0);
        let format = ExcelStyleDateFormatter::new("[H]:mm:ss").unwrap();
        let millis = java_date(1.5, &utc, false).unwrap();
        assert_eq!(format.format(1.5, millis, &utc).unwrap(), "36:00:00");
    }

    #[test]
    fn counts_days_in_either_date_system() {
        let utc = TimeZone::fixed(0);
        let format = SimpleDateFormat::new("yyyy-MM-dd").unwrap();
        let date = |value: f64, date_1904: bool| {
            let millis = java_date(value, &utc, date_1904).unwrap();
            format.format(millis, &utc).unwrap()
        };
        assert_eq!(date(0.0, true), "1904-01-01");
        assert_eq!(date(45000.0, true), "2027-03-16");
        assert_eq!(date(45000.0, false), "2023-03-15");
        assert_eq!(date(61.0, false), "1900-03-01");
    }
}
