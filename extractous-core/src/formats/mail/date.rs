//! The dates of mail: a date normalised with the regular expressions of Tika's
//! `MailDateParser`, then parsed with the forms of its formatters, RFC 5322's and its common
//! variants, case-insensitive, the first that parses winning. Each form is read strictly: its
//! fields of the widths it gives, a month, day, hour, minute and second within their ranges,
//! and an hour of the morning or afternoon from 1 to 12 with `AM` or `PM` after it. A date
//! without a time is the day alone, and what does not parse is no date.
//!
//! Tika parses leniently into dates nobody wrote (#80): `12:30 PM` the next day at 00:30, a
//! number in place of `AM` or `PM` a count of half days, `10-10-2000` the year 10, 31 February
//! 3 March, and a date without a time 12:00 UTC.
//!
//! A time zone the date names by a region or by a name, whose rules java.time looks up in its
//! time-zone database, leaves the message to Tika.

use super::Unported;
use crate::java::time::{civil, format_instant};

const MAX_OFFSET_SECONDS: i64 = 18 * 3600;

/// A date of mail: an instant, or a day without a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailDate {
    /// Milliseconds since the epoch.
    Instant(i64),
    /// Days since the epoch.
    Day(i64),
}

impl MailDate {
    /// The date as metadata gives it: an instant in ISO 8601 in UTC, a day as `yyyy-MM-dd`.
    pub(crate) fn format(self) -> String {
        match self {
            MailDate::Instant(millis) => format_instant(millis),
            MailDate::Day(epoch_day) => {
                let (year, month, day) = civil::civil_from_days(epoch_day);
                format!("{year:04}-{month:02}-{day:02}")
            }
        }
    }
}

/// The date `text` gives, `None` when it does not parse.
pub(crate) fn parse_date(text: &str) -> Result<Option<MailDate>, Unported> {
    let normalized: Vec<char> = normalize(text).chars().collect();
    for formatter in DATE_TIME_FORMATTERS.iter().chain(&DATE_FORMATTERS) {
        if let Some(date) = formatter.parse(&normalized)? {
            return Ok(Some(date));
        }
    }
    Ok(None)
}

/// Java's `\s`.
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{B}' | '\u{C}' | '\r')
}

fn is_digit(c: Option<&char>) -> bool {
    c.is_some_and(char::is_ascii_digit)
}

/// `MailDateParser.normalize`.
fn normalize(text: &str) -> String {
    let mut chars: Vec<char> = text.to_uppercase().chars().filter(|&c| c != ',').collect();
    if let Some(offset) = find_offset(&chars) {
        chars.truncate(offset.start);
        chars.extend(offset.replacement().chars());
    }
    if let Some((start, end, offset)) = find_localized_offset(&chars) {
        let rest: Vec<char> = chars[end..].to_vec();
        chars.truncate(start);
        chars.extend(offset.replacement().chars());
        chars.extend(rest);
    }
    insert_space_before_am_pm(&mut chars);
    let chars = remove_days_of_week(&chars);
    let text: String = chars.into_iter().collect();
    let text = text.replace(" AT ", " ");
    let mut collapsed = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if is_space(c) {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    crate::java::trim(&collapsed).to_owned()
}

/// An offset a pattern found: its sign, hours and minutes.
struct FoundOffset {
    start: usize,
    sign: char,
    hours: String,
    minutes: Option<String>,
}

impl FoundOffset {
    /// The offset as `sign HH ":" MM`, the hours padded to two digits.
    fn replacement(&self) -> String {
        format!(
            "{}{:0>2}:{}",
            self.sign,
            self.hours,
            self.minutes.as_deref().unwrap_or("00")
        )
    }
}

/// `[-+]\s*(\d?\d):?(\d\d)` at `at`, the hour's width and the colon tried greedily first.
fn match_offset(chars: &[char], at: usize) -> Option<(FoundOffset, usize)> {
    let sign = *chars.get(at).filter(|&&c| c == '+' || c == '-')?;
    let mut j = at + 1;
    while chars.get(j).is_some_and(|&c| is_space(c)) {
        j += 1;
    }
    for width in [2, 1] {
        if !(0..width).all(|i| is_digit(chars.get(j + i))) {
            continue;
        }
        let after_hours = j + width;
        let colon = chars.get(after_hours) == Some(&':');
        for with_colon in [true, false] {
            if with_colon && !colon {
                continue;
            }
            let minutes = after_hours + usize::from(with_colon);
            if is_digit(chars.get(minutes)) && is_digit(chars.get(minutes + 1)) {
                let found = FoundOffset {
                    start: at,
                    sign,
                    hours: chars[j..after_hours].iter().collect(),
                    minutes: Some(chars[minutes..minutes + 2].iter().collect()),
                };
                return Some((found, minutes + 2));
            }
        }
    }
    None
}

/// The first match of `(?:(?:-\d\d-\d{4})|([-+])\s*(\d?\d):?(\d\d))` that is an offset, the
/// date-like matches of the first alternative skipped.
fn find_offset(chars: &[char]) -> Option<FoundOffset> {
    let mut at = 0;
    while at < chars.len() {
        let date_like = chars[at] == '-'
            && is_digit(chars.get(at + 1))
            && is_digit(chars.get(at + 2))
            && chars.get(at + 3) == Some(&'-')
            && (4..8).all(|i| is_digit(chars.get(at + i)));
        if date_like {
            at += 8;
            continue;
        }
        if let Some((found, _)) = match_offset(chars, at) {
            return Some(found);
        }
        at += 1;
    }
    None
}

/// Java's `\Z`: the end, or a final line terminator.
fn at_end(chars: &[char], at: usize) -> bool {
    matches!(
        &chars[at.min(chars.len())..],
        [] | ['\n'] | ['\r'] | ['\r', '\n'] | ['\u{85}'] | ['\u{2028}'] | ['\u{2029}']
    )
}

/// The first match of `(?:UTC|GMT)\s*([-+])\s*(\d?\d):?(\d\d)?\Z`.
fn find_localized_offset(chars: &[char]) -> Option<(usize, usize, FoundOffset)> {
    for start in 0..chars.len() {
        let prefix: String = chars[start..chars.len().min(start + 3)].iter().collect();
        if prefix != "UTC" && prefix != "GMT" {
            continue;
        }
        let mut at = start + 3;
        while chars.get(at).is_some_and(|&c| is_space(c)) {
            at += 1;
        }
        let Some(&sign) = chars.get(at).filter(|&&c| c == '+' || c == '-') else {
            continue;
        };
        let mut j = at + 1;
        while chars.get(j).is_some_and(|&c| is_space(c)) {
            j += 1;
        }
        for width in [2, 1] {
            if !(0..width).all(|i| is_digit(chars.get(j + i))) {
                continue;
            }
            let after_hours = j + width;
            let colon = chars.get(after_hours) == Some(&':');
            for with_colon in [true, false] {
                if with_colon && !colon {
                    continue;
                }
                let minutes = after_hours + usize::from(with_colon);
                let has_minutes = is_digit(chars.get(minutes)) && is_digit(chars.get(minutes + 1));
                for take_minutes in [true, false] {
                    if take_minutes && !has_minutes {
                        continue;
                    }
                    let end = if take_minutes { minutes + 2 } else { minutes };
                    if at_end(chars, end) {
                        let found = FoundOffset {
                            start,
                            sign,
                            hours: chars[j..after_hours].iter().collect(),
                            minutes: take_minutes
                                .then(|| chars[minutes..minutes + 2].iter().collect()),
                        };
                        return Some((start, end, found));
                    }
                }
            }
        }
    }
    None
}

/// Java's `\b` test of a character: a letter, a digit or an underscore.
fn is_word(c: Option<&char>) -> bool {
    c.is_some_and(|&c| c.is_alphanumeric() || c == '_')
}

/// `(?i)(\d)([ap]m)\b` replaced by `$1 $2` at its first match.
fn insert_space_before_am_pm(chars: &mut Vec<char>) {
    for at in 0..chars.len() {
        let matches = chars[at].is_ascii_digit()
            && chars
                .get(at + 1)
                .is_some_and(|c| matches!(c, 'A' | 'a' | 'P' | 'p'))
            && chars.get(at + 2).is_some_and(|c| matches!(c, 'M' | 'm'))
            && !is_word(chars.get(at + 3));
        if matches {
            chars.insert(at + 1, ' ');
            return;
        }
    }
}

const DAYS_OF_WEEK: [&str; 17] = [
    "MON",
    "MONDAY",
    "TUE",
    "TUES",
    "TUESDAY",
    "WED",
    "WEDNESDAY",
    "THU",
    "THUR",
    "THURS",
    "THURSDAY",
    "FRI",
    "FRIDAY",
    "SAT",
    "SATURDAY",
    "SUN",
    "SUNDAY",
];

/// The length of `DAY ` at `at` for the first day name of the alternation that matches.
fn day_name_at(chars: &[char], at: usize) -> Option<usize> {
    DAYS_OF_WEEK.iter().find_map(|name| {
        let length = name.chars().count();
        let matches = chars.len() > at + length
            && chars[at..at + length].iter().copied().eq(name.chars())
            && chars[at + length] == ' ';
        matches.then_some(length + 1)
    })
}

/// `(?:\A| )(MON|...|SUNDAY) ` replaced by a space wherever it matches.
fn remove_days_of_week(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut at = 0;
    while at < chars.len() {
        let matched = if at == 0 { day_name_at(chars, 0) } else { None }.or_else(|| {
            (chars[at] == ' ')
                .then(|| day_name_at(chars, at + 1).map(|length| length + 1))
                .flatten()
        });
        match matched {
            Some(length) => {
                out.push(' ');
                at += length;
            }
            None => {
                out.push(chars[at]);
                at += 1;
            }
        }
    }
    out
}

/// A field a formatter parses, `ChronoField`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Year,
    Month,
    Day,
    Hour,
    HourOfAmPm,
    AmPm,
    Minute,
    Second,
    /// The fraction of a second, in nanoseconds.
    Nano,
    OffsetSeconds,
}

/// A printer-parser of a java.time formatter.
#[derive(Debug, Clone, Copy)]
enum Element {
    /// A number of the field, of the least to the most digits given, without a sign.
    Value(Field, usize, usize),
    /// A year of two to four digits, one of two or three read as RFC 5322 reads them.
    ReducedYear,
    Literal(char),
    /// The short month names, `MMM`.
    MonthText,
    AmPmText,
    Optional(&'static [Element]),
    /// `MailDateParser.TIME_ZONE_FORMATTER`.
    TimeZone,
}

/// What a formatter makes of what it parsed.
#[derive(Debug, Clone, Copy)]
enum Resolution {
    /// An instant, in UTC without a zone.
    DateTime,
    /// A day.
    Date,
}

struct Formatter {
    elements: &'static [Element],
    /// `parseCaseInsensitive()`; without it a formatter is case-sensitive.
    case_insensitive: bool,
    resolution: Resolution,
}

use Element::{Literal, MonthText, Optional, ReducedYear, TimeZone, Value};
use Field::*;

const SECONDS_MILLIS_ZONE: [Element; 3] = [
    Optional(&[Literal(':'), Value(Second, 1, 2)]),
    Optional(&[Literal('.'), Value(Nano, 3, 5)]),
    Optional(&[TimeZone]),
];

const SECONDS_MILLIS_AM_PM_ZONE: [Element; 5] = [
    Optional(&[Literal(':'), Value(Second, 1, 2)]),
    Optional(&[Literal('.'), Value(Nano, 3, 5)]),
    Optional(&[Literal(' ')]),
    Element::AmPmText,
    Optional(&[TimeZone]),
];

const fn concat<const A: usize, const B: usize, const N: usize>(
    a: [Element; A],
    b: [Element; B],
) -> [Element; N] {
    let mut out = [Literal(' '); N];
    let mut i = 0;
    while i < A {
        out[i] = a[i];
        i += 1;
    }
    while i < N {
        out[i] = b[i - A];
        i += 1;
    }
    out
}

const RFC_5322_LENIENT: [Element; 12] = concat(
    [
        Value(Day, 1, 2),
        Literal(' '),
        MonthText,
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(Hour, 1, 2),
        Literal(':'),
        Value(Minute, 1, 2),
    ],
    SECONDS_MILLIS_ZONE,
);

const RFC_5322_AM_PM_LENIENT: [Element; 14] = concat(
    [
        Value(Day, 1, 2),
        Literal(' '),
        MonthText,
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(HourOfAmPm, 1, 2),
        Literal(':'),
        Value(Minute, 1, 2),
    ],
    SECONDS_MILLIS_AM_PM_ZONE,
);

const MMM_D_YYYY_HH_MM: [Element; 12] = concat(
    [
        MonthText,
        Literal(' '),
        Value(Day, 1, 2),
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(Hour, 1, 2),
        Literal(':'),
        Value(Minute, 1, 2),
    ],
    SECONDS_MILLIS_ZONE,
);

const MMM_D_YYYY_HH_MM_AM_PM: [Element; 14] = concat(
    [
        MonthText,
        Literal(' '),
        Value(Day, 1, 2),
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(HourOfAmPm, 1, 2),
        Literal(':'),
        Value(Minute, 1, 2),
    ],
    SECONDS_MILLIS_AM_PM_ZONE,
);

const YYYY_MM_DD_HH_MM: [Element; 12] = concat(
    [
        Value(Year, 4, 4),
        Literal('-'),
        Value(Month, 2, 2),
        Literal('-'),
        Value(Day, 2, 2),
        Literal(' '),
        Value(Hour, 1, 2),
        Literal(':'),
        Value(Minute, 1, 2),
    ],
    SECONDS_MILLIS_ZONE,
);

const SLASHED_DATE: [Element; 6] = [
    Value(Month, 1, 2),
    Literal('/'),
    Value(Day, 1, 2),
    Literal('/'),
    ReducedYear,
    Literal(' '),
];

const MM_SLASH_DD_SLASH_YY_HH_MM: [Element; 11] = concat(
    concat::<6, 2, 8>(
        SLASHED_DATE,
        [
            Value(Hour, 1, 2),
            Optional(&[Literal(':'), Value(Minute, 1, 2)]),
        ],
    ),
    SECONDS_MILLIS_ZONE,
);

const MM_SLASH_DD_SLASH_YY_HH_MM_AM_PM: [Element; 13] = concat(
    concat::<6, 2, 8>(
        SLASHED_DATE,
        [
            Value(HourOfAmPm, 1, 2),
            Optional(&[Literal(':'), Value(Minute, 1, 2)]),
        ],
    ),
    SECONDS_MILLIS_AM_PM_ZONE,
);

const DATE_TIME_FORMATTERS: [Formatter; 7] = [
    Formatter {
        elements: &RFC_5322_LENIENT,
        case_insensitive: true,
        resolution: Resolution::DateTime,
    },
    Formatter {
        elements: &RFC_5322_AM_PM_LENIENT,
        case_insensitive: true,
        resolution: Resolution::DateTime,
    },
    Formatter {
        elements: &MMM_D_YYYY_HH_MM,
        case_insensitive: true,
        resolution: Resolution::DateTime,
    },
    Formatter {
        elements: &MMM_D_YYYY_HH_MM_AM_PM,
        case_insensitive: true,
        resolution: Resolution::DateTime,
    },
    Formatter {
        elements: &YYYY_MM_DD_HH_MM,
        case_insensitive: true,
        resolution: Resolution::DateTime,
    },
    Formatter {
        elements: &MM_SLASH_DD_SLASH_YY_HH_MM,
        case_insensitive: true,
        resolution: Resolution::DateTime,
    },
    Formatter {
        elements: &MM_SLASH_DD_SLASH_YY_HH_MM_AM_PM,
        case_insensitive: true,
        resolution: Resolution::DateTime,
    },
];

/// A month or day of a slashed date: one or two digits. Tika reads up to nineteen.
const fn pattern_value(field: Field) -> Element {
    Value(field, 1, 2)
}

const DATE_FORMATTERS: [Formatter; 6] = [
    Formatter {
        elements: &[
            Value(Day, 1, 2),
            Literal(' '),
            MonthText,
            Literal(' '),
            ReducedYear,
        ],
        case_insensitive: true,
        resolution: Resolution::Date,
    },
    Formatter {
        elements: &[
            MonthText,
            Literal(' '),
            Value(Day, 1, 2),
            Literal(' '),
            ReducedYear,
        ],
        case_insensitive: true,
        resolution: Resolution::Date,
    },
    Formatter {
        elements: &[
            Value(Year, 4, 4),
            Literal('-'),
            Value(Month, 2, 2),
            Literal('-'),
            Value(Day, 2, 2),
        ],
        case_insensitive: true,
        resolution: Resolution::Date,
    },
    Formatter {
        elements: &[
            pattern_value(Month),
            Literal('/'),
            pattern_value(Day),
            Literal('/'),
            ReducedYear,
        ],
        case_insensitive: false,
        resolution: Resolution::Date,
    },
    Formatter {
        elements: &[
            pattern_value(Day),
            Literal('/'),
            pattern_value(Month),
            Literal('/'),
            ReducedYear,
        ],
        case_insensitive: false,
        resolution: Resolution::Date,
    },
    Formatter {
        elements: &[
            ReducedYear,
            Literal('/'),
            pattern_value(Month),
            Literal('/'),
            pattern_value(Day),
        ],
        case_insensitive: false,
        resolution: Resolution::Date,
    },
];

/// Why parsing stopped: a mismatch an optional section recovers from, an exception that fails
/// the whole formatter, or a time zone the port does not resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Mismatch,
    Thrown,
    Unported,
}

/// A zone as java.time keeps it; every zone the port resolves has a fixed offset.
type ZoneOffsetSeconds = i64;

/// java.time's `Parsed`: the fields and the zone a parse found.
#[derive(Debug, Clone, Default)]
struct Parsed {
    fields: Vec<(Field, i64)>,
    zone: Option<ZoneOffsetSeconds>,
}

impl Parsed {
    fn get(&self, field: Field) -> Option<i64> {
        self.fields
            .iter()
            .find(|(present, _)| *present == field)
            .map(|&(_, value)| value)
    }

    /// `setParsedField`: a field parsed twice must have the same value.
    fn set(&mut self, field: Field, value: i64, success: usize) -> Result<usize, Stop> {
        match self.get(field) {
            Some(old) if old != value => Err(Stop::Mismatch),
            Some(_) => Ok(success),
            None => {
                self.fields.push((field, value));
                Ok(success)
            }
        }
    }
}

/// `Character.toUpperCase` and `toLowerCase` comparison, as a case-insensitive parse compares.
fn chars_equal(a: char, b: char, case_insensitive: bool) -> bool {
    a == b
        || (case_insensitive
            && (a.to_uppercase().eq(b.to_uppercase()) || a.to_lowercase().eq(b.to_lowercase())))
}

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

/// The month names a parse tries, longest first: the full and the short names. Tika also
/// reads a month's initial, and a number in place of the name (#80).
fn month_names() -> Vec<(String, i64)> {
    let mut names: Vec<(String, i64)> = Vec::new();
    for (index, name) in MONTHS.iter().enumerate() {
        names.push(((*name).to_owned(), index as i64 + 1));
    }
    for (index, name) in MONTHS.iter().enumerate() {
        names.push((name[..3].to_owned(), index as i64 + 1));
    }
    names
}

const AM_PM_NAMES: [(&str, i64); 4] = [("AM", 0), ("PM", 1), ("a", 0), ("p", 1)];

/// The year RFC 5322 reads of a year of two or three digits: two up to 49 in the 2000s, two
/// from 50 and three in the 1900s. Tika reads two digits in the century from 1970 on.
fn rfc_5322_year(year: i64, digits: usize) -> i64 {
    match digits {
        2 if year < 50 => year + 2000,
        2 | 3 => year + 1900,
        _ => year,
    }
}

impl Formatter {
    /// The date this formatter parses from the whole text.
    fn parse(&self, text: &[char]) -> Result<Option<MailDate>, Unported> {
        let mut parsed = Parsed::default();
        let end = match self.parse_elements(self.elements, &mut parsed, text, 0) {
            Ok(end) => end,
            Err(Stop::Unported) => return Err(Unported::TimeZone),
            Err(Stop::Mismatch | Stop::Thrown) => return Ok(None),
        };
        if end != text.len() {
            return Ok(None);
        }
        Ok(resolve(&parsed, self.resolution))
    }

    fn parse_elements(
        &self,
        elements: &[Element],
        parsed: &mut Parsed,
        text: &[char],
        mut pos: usize,
    ) -> Result<usize, Stop> {
        for element in elements {
            pos = self.parse_element(*element, parsed, text, pos)?;
        }
        Ok(pos)
    }

    fn parse_element(
        &self,
        element: Element,
        parsed: &mut Parsed,
        text: &[char],
        pos: usize,
    ) -> Result<usize, Stop> {
        match element {
            Value(field, min, max) => parse_number(field, min, max, parsed, text, pos),
            ReducedYear => parse_number(Year, 2, 4, parsed, text, pos),
            Literal(literal) => match text.get(pos) {
                Some(&c) if chars_equal(c, literal, self.case_insensitive) => Ok(pos + 1),
                _ => Err(Stop::Mismatch),
            },
            MonthText => {
                let names = month_names();
                let candidates: Vec<(&str, i64)> = names
                    .iter()
                    .map(|(text, value)| (text.as_str(), *value))
                    .collect();
                self.parse_text(Month, &candidates, parsed, text, pos)
            }
            Element::AmPmText => self.parse_text(AmPm, &AM_PM_NAMES, parsed, text, pos),
            Optional(elements) => {
                let saved = parsed.clone();
                match self.parse_elements(elements, parsed, text, pos) {
                    Ok(end) => Ok(end),
                    Err(Stop::Mismatch) => {
                        *parsed = saved;
                        Ok(pos)
                    }
                    Err(stop) => Err(stop),
                }
            }
            TimeZone => self.parse_time_zone(parsed, text, pos),
        }
    }

    /// The first of the names that matches. Tika reads a number where none does, so that a
    /// number in place of `AM` or `PM` counts half days (#80).
    fn parse_text(
        &self,
        field: Field,
        names: &[(&str, i64)],
        parsed: &mut Parsed,
        text: &[char],
        pos: usize,
    ) -> Result<usize, Stop> {
        for &(name, value) in names {
            let length = name.chars().count();
            let matches = text.len() >= pos + length
                && text[pos..pos + length]
                    .iter()
                    .zip(name.chars())
                    .all(|(&a, b)| chars_equal(a, b, self.case_insensitive));
            if matches {
                return parsed.set(field, value, pos + length);
            }
        }
        Err(Stop::Mismatch)
    }

    /// `TIME_ZONE_FORMATTER`: an optional space, then optionally a zone id, an offset and a zone
    /// name, each tried in turn.
    fn parse_time_zone(
        &self,
        parsed: &mut Parsed,
        text: &[char],
        pos: usize,
    ) -> Result<usize, Stop> {
        let mut pos = self.parse_element(Optional(&[Literal(' ')]), parsed, text, pos)?;
        let saved = parsed.clone();
        match self.parse_zone_id(parsed, text, pos) {
            Ok(end) => pos = end,
            Err(Stop::Mismatch) => *parsed = saved,
            Err(stop) => return Err(stop),
        }
        let saved = parsed.clone();
        match parse_offset(text, pos, 'Z', OffsetPattern::HoursMinutes) {
            Ok(Some((seconds, end))) => match parsed.set(OffsetSeconds, seconds, end) {
                Ok(end) => pos = end,
                Err(_) => *parsed = saved,
            },
            Ok(None) => {}
            Err(stop) => return Err(stop),
        }
        if text.get(pos).is_some_and(|c| c.is_alphabetic()) && may_name_zone(&text[pos..]) {
            return Err(Stop::Unported);
        }
        Ok(pos)
    }

    /// `ZoneIdPrinterParser.parse`: an offset, `UT`, `UTC` or `GMT` with an optional offset,
    /// `Z`, or a region id, which the port does not resolve.
    fn parse_zone_id(&self, parsed: &mut Parsed, text: &[char], pos: usize) -> Result<usize, Stop> {
        let eq = |at: usize, c: char| text.get(at).is_some_and(|&t| chars_equal(t, c, true));
        let Some(&first) = text.get(pos) else {
            return Err(Stop::Mismatch);
        };
        if first == '+' || first == '-' {
            return self.parse_offset_based(parsed, text, pos, pos, OffsetZero::Z);
        }
        if text.len() >= pos + 2 {
            if eq(pos, 'U') && eq(pos + 1, 'T') {
                let prefix_end = if eq(pos + 2, 'C') { pos + 3 } else { pos + 2 };
                return self.parse_offset_based(parsed, text, pos, prefix_end, OffsetZero::Digit);
            }
            if eq(pos, 'G') && eq(pos + 1, 'M') && eq(pos + 2, 'T') {
                if eq(pos + 3, '0') {
                    parsed.zone = Some(0);
                    return Ok(pos + 4);
                }
                return self.parse_offset_based(parsed, text, pos, pos + 3, OffsetZero::Digit);
            }
        }
        if first.is_alphabetic() {
            let next_is_letter = text.get(pos + 1).is_some_and(|c| c.is_alphabetic());
            if eq(pos, 'Z') && !next_is_letter {
                parsed.zone = Some(0);
                return Ok(pos + 1);
            }
            if may_name_zone(&text[pos..]) {
                return Err(Stop::Unported);
            }
        }
        Err(Stop::Mismatch)
    }

    /// `parseOffsetBased`: a prefix (none, `UT`, `UTC` or `GMT`) and the offset after it.
    fn parse_offset_based(
        &self,
        parsed: &mut Parsed,
        text: &[char],
        prefix_start: usize,
        pos: usize,
        zero: OffsetZero,
    ) -> Result<usize, Stop> {
        let prefixed = pos > prefix_start;
        let Some(&next) = text.get(pos) else {
            return self.fixed_zone(parsed, prefixed, pos);
        };
        if next == '0' || chars_equal(next, 'Z', true) {
            return self.fixed_zone(parsed, prefixed, pos);
        }
        let no_offset = match zero {
            OffsetZero::Z => 'Z',
            OffsetZero::Digit => '0',
        };
        match parse_offset(text, pos, no_offset, OffsetPattern::HoursMinutesSeconds)? {
            None if zero == OffsetZero::Z => Err(Stop::Mismatch),
            None => self.fixed_zone(parsed, prefixed, pos),
            Some((seconds, end)) => {
                if seconds.abs() > MAX_OFFSET_SECONDS {
                    return Err(Stop::Mismatch);
                }
                parsed.zone = Some(seconds);
                Ok(end)
            }
        }
    }

    /// `ZoneId.of(prefix)`: `UT`, `UTC` and `GMT` are UTC; no prefix is not a zone id.
    fn fixed_zone(&self, parsed: &mut Parsed, prefixed: bool, pos: usize) -> Result<usize, Stop> {
        if !prefixed {
            return Err(Stop::Thrown);
        }
        parsed.zone = Some(0);
        Ok(pos)
    }
}

/// Whether the rest of a date, from where a zone may start, could be a zone id or zone name
/// java.time reads to the end: one token of the characters zone ids and short names use, and
/// not a marker of the morning or afternoon, which no zone is called. A zone that does not
/// reach the end fails the formatter whatever it is.
fn may_name_zone(rest: &[char]) -> bool {
    let token: String = rest.iter().collect();
    let am_pm = ["AM", "PM", "A", "P"]
        .iter()
        .any(|marker| marker.eq_ignore_ascii_case(&token));
    !am_pm
        && rest
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-' | ':' | '.'))
}

/// The text standing for a zero offset in an `OffsetIdPrinterParser`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OffsetZero {
    Z,
    Digit,
}

/// The two offset patterns the formatters use, both read leniently with an optional minute
/// and second: `+HH:MM:ss` reads a colon before each, `+HHmm` none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OffsetPattern {
    HoursMinutesSeconds,
    HoursMinutes,
}

/// `OffsetIdPrinterParser.parse` in lenient mode: the offset in seconds and where it ends,
/// `None` for no offset; hours past 23 throw.
fn parse_offset(
    text: &[char],
    pos: usize,
    no_offset: char,
    pattern: OffsetPattern,
) -> Result<Option<(i64, usize)>, Stop> {
    let Some(&sign) = text.get(pos) else {
        return Ok(None);
    };
    if chars_equal(sign, no_offset, true) {
        return Ok(Some((0, pos + 1)));
    }
    if sign != '+' && sign != '-' {
        return Ok(None);
    }
    let colon = pattern == OffsetPattern::HoursMinutesSeconds;
    let two_digits = |at: usize| -> Option<i64> {
        let (a, b) = (text.get(at)?, text.get(at + 1)?);
        let value = i64::from(a.to_digit(10).filter(|_| a.is_ascii_digit())?) * 10
            + i64::from(b.to_digit(10).filter(|_| b.is_ascii_digit())?);
        (value <= 59).then_some(value)
    };
    let Some(hours) = two_digits(pos + 1) else {
        return Ok(None);
    };
    let mut end = pos + 3;
    let mut parts = [0i64; 2];
    for part in &mut parts {
        let at = if colon {
            if text.get(end) != Some(&':') {
                break;
            }
            end + 1
        } else {
            end
        };
        match two_digits(at) {
            Some(value) => {
                *part = value;
                end = at + 2;
            }
            None => break,
        }
    }
    if hours > 23 {
        return Err(Stop::Thrown);
    }
    let seconds = hours * 3600 + parts[0] * 60 + parts[1];
    Ok(Some((if sign == '-' { -seconds } else { seconds }, end)))
}

/// A number of `field` of `min` to `max` ASCII digits, without a sign: a year of two or three
/// digits as RFC 5322 reads it, a fraction of a second in nanoseconds. Tika's lenient
/// formatters read one to nine digits with a sign, so that `10-10-2000` is the year 10 (#80).
fn parse_number(
    field: Field,
    min: usize,
    max: usize,
    parsed: &mut Parsed,
    text: &[char],
    pos: usize,
) -> Result<usize, Stop> {
    let digits = text[pos.min(text.len())..]
        .iter()
        .take(max)
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits < min {
        return Err(Stop::Mismatch);
    }
    let mut value = text[pos..pos + digits]
        .iter()
        .fold(0i64, |value, c| value * 10 + i64::from(*c as u8 - b'0'));
    match field {
        Year => value = rfc_5322_year(value, digits),
        Nano => value *= 10i64.pow(9 - digits as u32),
        _ => {}
    }
    parsed.set(field, value, pos + digits)
}

/// The days of a month.
fn month_length(year: i64, month: i64) -> i64 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if civil::is_leap_year(year) => 29,
        2 => 28,
        _ => 31,
    }
}

/// The date of the fields a formatter parsed, each within its range, or `None`: a day of the
/// month it is in, an hour of a day, or of the morning or afternoon from 1 to 12, and a minute
/// and second of their hour and minute, a leap second the last of its minute. Tika resolves the
/// fields leniently, so that 31 February is 3 March and `12:30 PM` 00:30 the next day, and
/// gives a date without a time as 12:00 UTC (#80).
fn resolve(parsed: &Parsed, resolution: Resolution) -> Option<MailDate> {
    let (year, month, day) = (parsed.get(Year)?, parsed.get(Month)?, parsed.get(Day)?);
    if !(1..=12).contains(&month) || !(1..=month_length(year, month)).contains(&day) {
        return None;
    }
    let epoch_day = civil::days_from_civil(year, month, day);
    if let Resolution::Date = resolution {
        return Some(MailDate::Day(epoch_day));
    }
    let hour = match (parsed.get(Hour), parsed.get(HourOfAmPm), parsed.get(AmPm)) {
        (Some(hour), None, None) if (0..=23).contains(&hour) => hour,
        (None, Some(hour), Some(am_pm)) if (1..=12).contains(&hour) => hour % 12 + 12 * am_pm,
        _ => return None,
    };
    let minute = parsed.get(Minute).unwrap_or(0);
    let second = parsed.get(Second).unwrap_or(0);
    if !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    let offset = match parsed.get(OffsetSeconds) {
        Some(seconds) if seconds.abs() > MAX_OFFSET_SECONDS => return None,
        Some(seconds) => seconds,
        None => parsed.zone.unwrap_or(0),
    };
    let epoch_second = epoch_day * 86_400 + hour * 3600 + minute * 60 + second.min(59) - offset;
    let nano = parsed.get(Nano).unwrap_or(0);
    Some(MailDate::Instant(epoch_second * 1000 + nano / 1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Option<String> {
        parse_date(text).unwrap().map(MailDate::format)
    }

    #[test]
    fn normalizes_as_mail_date_parser_does() {
        assert_eq!(
            normalize("Wed, 26 Jan 2022 09:14:37 +0100 (CET)"),
            "26 JAN 2022 09:14:37 +01:00"
        );
        assert_eq!(
            normalize("Mon, 9 May 2016 7:32:00 UTC+6"),
            "9 MAY 2016 7:32:00 +06:00"
        );
        assert_eq!(
            normalize("16 May 2016 at 09:30:32  GMT+1"),
            "16 MAY 2016 09:30:32 +01:00"
        );
        assert_eq!(
            normalize("July 9 2012 10:10:10am"),
            "JULY 9 2012 10:10:10 AM"
        );
        assert_eq!(normalize("10-10-2000 -0500"), "10-10-2000 -05:00");
    }

    #[test]
    fn parses_dates_strictly() {
        assert_eq!(
            parse("Tue, 05 Mar 2024 14:30:00 +0100").as_deref(),
            Some("2024-03-05T13:30:00Z")
        );
        assert_eq!(
            parse("5 Mar 24 14:30 GMT").as_deref(),
            Some("2024-03-05T14:30:00Z")
        );
        assert_eq!(
            parse("5 Mar 99 14:30 +0000").as_deref(),
            Some("1999-03-05T14:30:00Z")
        );
        assert_eq!(
            parse("Mar 5 2024 12:30 PM").as_deref(),
            Some("2024-03-05T12:30:00Z")
        );
        assert_eq!(
            parse("Mar 5 2024 12:30 AM").as_deref(),
            Some("2024-03-05T00:30:00Z")
        );
        assert_eq!(parse("Mar 5 2024 10:30 +1"), None);
        assert_eq!(parse("Mar 5 2024 13:30 PM"), None);
        assert_eq!(parse("2012-10-10 10:10:10.5"), None);
        assert_eq!(
            parse("2012-10-10 10:10:10.250").as_deref(),
            Some("2012-10-10T10:10:10Z")
        );
        assert_eq!(parse("10-10-2000 10:10:10"), None);
        assert_eq!(parse("7/9/2012").as_deref(), Some("2012-07-09"));
        assert_eq!(parse("13/9/2012").as_deref(), Some("2012-09-13"));
        assert_eq!(parse("31 Feb 2021 10:00"), None);
        assert_eq!(parse("29 Feb 2024").as_deref(), Some("2024-02-29"));
        assert_eq!(parse("5 3 2024 10:00"), None);
        assert_eq!(
            parse("5 Mar 2024 23:59:60 +0000").as_deref(),
            Some("2024-03-05T23:59:59Z")
        );
        assert_eq!(parse("not a date"), None);
        assert_eq!(parse_date("5 Mar 2024 14:30 EST"), Err(Unported::TimeZone));
    }
}
