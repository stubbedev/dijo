//! Tika's `MailDateParser.parseDateLenient`: a date normalised with a few regular expressions,
//! then parsed with java.time formatters, case-insensitive and lenient, as java.time parses,
//! the first that parses winning. A time zone the date names by a region or by a name, whose
//! rules java.time looks up in its time-zone database, leaves the message to Tika.

use super::Unported;
use crate::java::time::civil;

const NANOS_PER_DAY: i64 = 86_400_000_000_000;
const MAX_OFFSET_SECONDS: i64 = 18 * 3600;
const MAX_YEAR: i64 = 999_999_999;
const BASE_YEAR: i64 = 1970;

/// The milliseconds since the epoch of the date `parseDateLenient` returns, `None` for null.
pub(super) fn parse_date_lenient(text: &str) -> Result<Option<i64>, Unported> {
    let normalized: Vec<char> = normalize(text).chars().collect();
    for formatter in DATE_TIME_FORMATTERS {
        if let Some(millis) = formatter.parse(&normalized)? {
            return Ok(Some(millis));
        }
    }
    for formatter in DATE_FORMATTERS {
        if let Some(millis) = formatter.parse(&normalized)? {
            return Ok(Some(millis));
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
    Milli,
    OffsetSeconds,
}

/// `SignStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sign {
    Normal,
    NotNegative,
    Never,
}

impl Sign {
    /// `SignStyle.parse`: whether a sign may be read.
    fn allows(self, positive: bool, strict: bool, fixed_width: bool) -> bool {
        match self {
            Sign::Normal => !positive || !strict,
            Sign::NotNegative | Sign::Never => !strict && !fixed_width,
        }
    }
}

/// A printer-parser of a java.time formatter.
#[derive(Debug, Clone, Copy)]
enum Element {
    Value(Field, usize, usize, Sign),
    /// `appendValueReduced(YEAR, 2, 4, 1970)`.
    ReducedYear,
    Literal(char),
    /// The short month names, `MMM`.
    MonthText,
    AmPmText,
    Optional(&'static [Element]),
    /// `MailDateParser.TIME_ZONE_FORMATTER`.
    TimeZone,
}

/// How a formatter resolves what it parsed.
#[derive(Debug, Clone, Copy)]
enum Resolution {
    /// `ResolverStyle.LENIENT` with resolver fields, as a `ZonedDateTime` or, without a zone,
    /// a `LocalDateTime` in UTC.
    DateTime(&'static [Field]),
    /// `ResolverStyle.SMART`, as a `LocalDate` at the start of the day in GMT-12:00.
    Date,
    /// `ResolverStyle.LENIENT`, as a `LocalDate` at the start of the day in GMT-12:00.
    LenientDate,
}

struct Formatter {
    elements: &'static [Element],
    /// `parseCaseInsensitive().parseLenient()`; without them a formatter is case-sensitive and
    /// strict.
    lenient: bool,
    resolution: Resolution,
}

use Element::{Literal, MonthText, Optional, ReducedYear, TimeZone, Value};
use Field::*;

const TIME_FIELDS: &[Field] = &[Day, Month, Year, Hour, Minute, Second, Milli, OffsetSeconds];
const AM_PM_FIELDS: &[Field] = &[
    Day,
    Month,
    Year,
    HourOfAmPm,
    AmPm,
    Minute,
    Second,
    Milli,
    OffsetSeconds,
];

const SECONDS_MILLIS_ZONE: [Element; 3] = [
    Optional(&[Literal(':'), Value(Second, 2, 2, Sign::NotNegative)]),
    Optional(&[Literal('.'), Value(Milli, 3, 5, Sign::Never)]),
    Optional(&[TimeZone]),
];

const SECONDS_MILLIS_AM_PM_ZONE: [Element; 5] = [
    Optional(&[Literal(':'), Value(Second, 2, 2, Sign::NotNegative)]),
    Optional(&[Literal('.'), Value(Milli, 3, 5, Sign::Never)]),
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
        Value(Day, 1, 2, Sign::Never),
        Literal(' '),
        MonthText,
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(Hour, 1, 2, Sign::Never),
        Literal(':'),
        Value(Minute, 1, 2, Sign::Never),
    ],
    SECONDS_MILLIS_ZONE,
);

const RFC_5322_AM_PM_LENIENT: [Element; 14] = concat(
    [
        Value(Day, 1, 2, Sign::Never),
        Literal(' '),
        MonthText,
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(HourOfAmPm, 1, 2, Sign::Never),
        Literal(':'),
        Value(Minute, 1, 2, Sign::Never),
    ],
    SECONDS_MILLIS_AM_PM_ZONE,
);

const MMM_D_YYYY_HH_MM: [Element; 12] = concat(
    [
        MonthText,
        Literal(' '),
        Value(Day, 1, 2, Sign::Never),
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(Hour, 1, 2, Sign::Never),
        Literal(':'),
        Value(Minute, 1, 2, Sign::Never),
    ],
    SECONDS_MILLIS_ZONE,
);

const MMM_D_YYYY_HH_MM_AM_PM: [Element; 14] = concat(
    [
        MonthText,
        Literal(' '),
        Value(Day, 1, 2, Sign::Never),
        Literal(' '),
        ReducedYear,
        Literal(' '),
        Value(HourOfAmPm, 1, 2, Sign::Never),
        Literal(':'),
        Value(Minute, 1, 2, Sign::Never),
    ],
    SECONDS_MILLIS_AM_PM_ZONE,
);

const YYYY_MM_DD_HH_MM: [Element; 12] = concat(
    [
        Value(Year, 4, 4, Sign::NotNegative),
        Literal('-'),
        Value(Month, 2, 2, Sign::Never),
        Literal('-'),
        Value(Day, 2, 2, Sign::Never),
        Literal(' '),
        Value(Hour, 1, 2, Sign::Never),
        Literal(':'),
        Value(Minute, 1, 2, Sign::Never),
    ],
    SECONDS_MILLIS_ZONE,
);

const SLASHED_DATE: [Element; 6] = [
    Value(Month, 1, 2, Sign::Never),
    Literal('/'),
    Value(Day, 1, 2, Sign::Never),
    Literal('/'),
    ReducedYear,
    Literal(' '),
];

const MM_SLASH_DD_SLASH_YY_HH_MM: [Element; 11] = concat(
    concat::<6, 2, 8>(
        SLASHED_DATE,
        [
            Value(Hour, 1, 2, Sign::Never),
            Optional(&[Literal(':'), Value(Minute, 1, 2, Sign::Never)]),
        ],
    ),
    SECONDS_MILLIS_ZONE,
);

const MM_SLASH_DD_SLASH_YY_HH_MM_AM_PM: [Element; 13] = concat(
    concat::<6, 2, 8>(
        SLASHED_DATE,
        [
            Value(HourOfAmPm, 1, 2, Sign::Never),
            Optional(&[Literal(':'), Value(Minute, 1, 2, Sign::Never)]),
        ],
    ),
    SECONDS_MILLIS_AM_PM_ZONE,
);

const DATE_TIME_FORMATTERS: [Formatter; 7] = [
    Formatter {
        elements: &RFC_5322_LENIENT,
        lenient: true,
        resolution: Resolution::DateTime(TIME_FIELDS),
    },
    Formatter {
        elements: &RFC_5322_AM_PM_LENIENT,
        lenient: true,
        resolution: Resolution::DateTime(AM_PM_FIELDS),
    },
    Formatter {
        elements: &MMM_D_YYYY_HH_MM,
        lenient: true,
        resolution: Resolution::DateTime(TIME_FIELDS),
    },
    Formatter {
        elements: &MMM_D_YYYY_HH_MM_AM_PM,
        lenient: true,
        resolution: Resolution::DateTime(AM_PM_FIELDS),
    },
    Formatter {
        elements: &YYYY_MM_DD_HH_MM,
        lenient: true,
        resolution: Resolution::DateTime(TIME_FIELDS),
    },
    Formatter {
        elements: &MM_SLASH_DD_SLASH_YY_HH_MM,
        lenient: true,
        resolution: Resolution::DateTime(TIME_FIELDS),
    },
    Formatter {
        elements: &MM_SLASH_DD_SLASH_YY_HH_MM_AM_PM,
        lenient: true,
        resolution: Resolution::DateTime(AM_PM_FIELDS),
    },
];

/// `appendValue(field)` of a pattern letter: one to nineteen digits, `SignStyle.NORMAL`.
const fn pattern_value(field: Field) -> Element {
    Value(field, 1, 19, Sign::Normal)
}

const DATE_FORMATTERS: [Formatter; 6] = [
    Formatter {
        elements: &[
            Value(Day, 1, 2, Sign::Never),
            Literal(' '),
            MonthText,
            Literal(' '),
            ReducedYear,
        ],
        lenient: true,
        resolution: Resolution::Date,
    },
    Formatter {
        elements: &[
            MonthText,
            Literal(' '),
            Value(Day, 1, 2, Sign::Never),
            Literal(' '),
            ReducedYear,
        ],
        lenient: true,
        resolution: Resolution::Date,
    },
    Formatter {
        elements: &[
            Value(Year, 4, 4, Sign::NotNegative),
            Literal('-'),
            Value(Month, 2, 2, Sign::Never),
            Literal('-'),
            Value(Day, 2, 2, Sign::Never),
        ],
        lenient: true,
        resolution: Resolution::LenientDate,
    },
    Formatter {
        elements: &[
            pattern_value(Month),
            Literal('/'),
            pattern_value(Day),
            Literal('/'),
            ReducedYear,
        ],
        lenient: false,
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
        lenient: false,
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
        lenient: false,
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

/// The month names of `Locale.US` a lenient parse tries, longest first: the full and short
/// names, then the narrow initials, each standing for the last month that has it.
fn month_names() -> Vec<(String, i64)> {
    let mut names: Vec<(String, i64)> = Vec::new();
    for (index, name) in MONTHS.iter().enumerate() {
        names.push(((*name).to_owned(), index as i64 + 1));
    }
    for (index, name) in MONTHS.iter().enumerate() {
        names.push((name[..3].to_owned(), index as i64 + 1));
    }
    let mut initials: Vec<(String, i64)> = Vec::new();
    for (index, name) in MONTHS.iter().enumerate() {
        let initial = name[..1].to_owned();
        match initials.iter_mut().find(|(text, _)| *text == initial) {
            Some(entry) => entry.1 = index as i64 + 1,
            None => initials.push((initial, index as i64 + 1)),
        }
    }
    names.extend(initials);
    names.sort_by_key(|(text, _)| std::cmp::Reverse(text.len()));
    names
}

const AM_PM_NAMES: [(&str, i64); 4] = [("AM", 0), ("PM", 1), ("a", 0), ("p", 1)];

impl Formatter {
    /// The date this formatter parses from the whole text, as `parseDateLenient` takes it.
    fn parse(&self, text: &[char]) -> Result<Option<i64>, Unported> {
        let mut parsed = Parsed::default();
        let end = match self.parse_elements(self.elements, &mut parsed, text, 0) {
            Ok(end) => end,
            Err(Stop::Unported) => return Err(Unported::TimeZone),
            Err(Stop::Mismatch | Stop::Thrown) => return Ok(None),
        };
        if end != text.len() {
            return Ok(None);
        }
        Ok(match self.resolution {
            Resolution::DateTime(fields) => resolve_date_time(&parsed, fields),
            Resolution::Date => resolve_date(&parsed),
            Resolution::LenientDate => resolve_lenient_date(&parsed),
        })
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
            Value(field, min, max, sign) => {
                self.parse_number(field, min, max, sign, false, parsed, text, pos)
            }
            ReducedYear => {
                self.parse_number(Year, 2, 4, Sign::NotNegative, true, parsed, text, pos)
            }
            Literal(literal) => match text.get(pos) {
                Some(&c) if chars_equal(c, literal, self.lenient) => Ok(pos + 1),
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

    /// `NumberPrinterParser.parse`, or with `reduced` `ReducedPrinterParser`'s.
    #[allow(clippy::too_many_arguments)]
    fn parse_number(
        &self,
        field: Field,
        min: usize,
        max: usize,
        sign: Sign,
        reduced: bool,
        parsed: &mut Parsed,
        text: &[char],
        pos: usize,
    ) -> Result<usize, Stop> {
        let strict = !self.lenient;
        let mut position = pos;
        let mut negative = false;
        match text.get(position) {
            None => return Err(Stop::Mismatch),
            Some('+') => {
                if !sign.allows(true, strict, min == max) {
                    return Err(Stop::Mismatch);
                }
                position += 1;
            }
            Some('-') => {
                if !sign.allows(false, strict, min == max) {
                    return Err(Stop::Mismatch);
                }
                negative = true;
                position += 1;
            }
            Some(_) => {}
        }
        let (min_width, max_width) = if strict { (min, max) } else { (1, 9) };
        if position + min_width > text.len() {
            return Err(Stop::Mismatch);
        }
        let mut end = position;
        let mut total: i128 = 0;
        while end < text.len().min(position + max_width) {
            let Some(digit) = text[end]
                .to_digit(10)
                .filter(|_| text[end].is_ascii_digit())
            else {
                break;
            };
            total = total * 10 + i128::from(digit);
            end += 1;
        }
        if end - position < min_width {
            return Err(Stop::Mismatch);
        }
        if total > i128::from(i64::MAX) {
            total /= 10;
            end -= 1;
        }
        let mut value = total as i64;
        if negative {
            if value == 0 && strict {
                return Err(Stop::Mismatch);
            }
            value = -value;
        }
        if reduced && end - position == 2 && value >= 0 {
            value += BASE_YEAR - BASE_YEAR % 100;
            if value < BASE_YEAR {
                value += 100;
            }
        }
        parsed.set(field, value, end)
    }

    /// `TextPrinterParser.parse` in lenient mode: the first of the names that matches, else a
    /// number.
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
                    .all(|(&a, b)| chars_equal(a, b, self.lenient));
            if matches {
                return parsed.set(field, value, pos + length);
            }
        }
        self.parse_number(field, 1, 19, Sign::Normal, false, parsed, text, pos)
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

/// `LocalDate.of(year, 1, 1).plusMonths(months).plusDays(days)` as an epoch day, or `None`
/// where java.time throws.
fn lenient_epoch_day(year: i64, months: i64, days: i64) -> Option<i64> {
    let month_count = year.checked_mul(12)?.checked_add(months)?;
    let new_year = month_count.div_euclid(12);
    if new_year.abs() > MAX_YEAR {
        return None;
    }
    let month = month_count.rem_euclid(12) + 1;
    let epoch_day = civil::days_from_civil(new_year, month, 1).checked_add(days)?;
    in_range(epoch_day).then_some(epoch_day)
}

/// Whether an epoch day is within `LocalDate`'s years.
fn in_range(epoch_day: i64) -> bool {
    let (year, _, _) = civil::civil_from_days(epoch_day);
    year.abs() <= MAX_YEAR
}

/// `Date.from(instant)`: the milliseconds of an epoch second and nanosecond, or `None` where the
/// conversion overflows.
fn epoch_millis(epoch_second: i64, nano: i64) -> Option<i64> {
    epoch_second
        .checked_mul(1000)?
        .checked_add(nano / 1_000_000)
}

/// The lenient resolution of a date-time formatter's fields, restricted to `fields`.
fn resolve_date_time(parsed: &Parsed, fields: &[Field]) -> Option<i64> {
    let get = |field: Field| parsed.get(field).filter(|_| fields.contains(&field));
    let date = match (get(Year), get(Month), get(Day)) {
        (Some(year), Some(month), Some(day)) => {
            if year.abs() > MAX_YEAR {
                return None;
            }
            Some(lenient_epoch_day(
                year,
                month.checked_sub(1)?,
                day.checked_sub(1)?,
            )?)
        }
        _ => None,
    };
    let mut hour = get(Hour);
    if let (Some(am_pm), Some(hour_of_am_pm)) = (get(AmPm), get(HourOfAmPm)) {
        let from_am_pm = am_pm.checked_mul(12)?.checked_add(hour_of_am_pm)?;
        if hour.is_some_and(|hour| hour != from_am_pm) {
            return None;
        }
        hour = Some(from_am_pm);
    }
    let nano = get(Milli).map(|milli| milli.wrapping_mul(1000).wrapping_mul(1000));
    let (minute, second) = (get(Minute), get(Second));
    let time = match hour {
        Some(hour) => {
            let invalid = (minute.is_none() && (second.is_some() || nano.is_some()))
                || (minute.is_some() && second.is_none() && nano.is_some());
            if invalid {
                None
            } else {
                let total = hour
                    .checked_mul(3_600_000_000_000)?
                    .checked_add(minute.unwrap_or(0).checked_mul(60_000_000_000)?)?
                    .checked_add(second.unwrap_or(0).checked_mul(1_000_000_000)?)?
                    .checked_add(nano.unwrap_or(0))?;
                Some((
                    total.div_euclid(NANOS_PER_DAY),
                    total.rem_euclid(NANOS_PER_DAY),
                ))
            }
        }
        None => None,
    };
    let (date, (excess_days, nano_of_day)) = (date?, time?);
    let epoch_day = date.checked_add(excess_days)?;
    if !in_range(epoch_day) {
        return None;
    }
    let offset = match get(OffsetSeconds) {
        Some(seconds) if seconds.abs() > MAX_OFFSET_SECONDS => return None,
        Some(seconds) => seconds,
        None => parsed.zone.unwrap_or(0),
    };
    let epoch_second = epoch_day * 86_400 + nano_of_day / 1_000_000_000 - offset;
    epoch_millis(epoch_second, nano_of_day % 1_000_000_000)
}

/// The lenient resolution of a date formatter's fields, at the start of the day in GMT-12:00.
fn resolve_lenient_date(parsed: &Parsed) -> Option<i64> {
    let year = parsed.get(Year)?;
    if year.abs() > MAX_YEAR {
        return None;
    }
    let month = parsed.get(Month)?.checked_sub(1)?;
    let day = parsed.get(Day)?.checked_sub(1)?;
    let epoch_day = lenient_epoch_day(year, month, day)?;
    epoch_millis(epoch_day.checked_mul(86_400)?.checked_add(12 * 3600)?, 0)
}

/// The smart resolution of a date formatter's fields, at the start of the day in GMT-12:00.
fn resolve_date(parsed: &Parsed) -> Option<i64> {
    let year = parsed.get(Year)?;
    let month = parsed.get(Month)?;
    let mut day = parsed.get(Day)?;
    if year.abs() > MAX_YEAR || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let length = match month {
        4 | 6 | 9 | 11 => 30,
        2 if civil::is_leap_year(year) => 29,
        2 => 28,
        _ => 31,
    };
    day = day.min(length);
    let epoch_day = civil::days_from_civil(year, month, day);
    epoch_millis(epoch_day * 86_400 + 12 * 3600, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::java::time::format_instant;

    fn parse(text: &str) -> Option<String> {
        parse_date_lenient(text).unwrap().map(format_instant)
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
    fn parses_dates_as_mail_date_parser_does() {
        assert_eq!(
            parse("Tue, 05 Mar 2024 14:30:00 +0100").as_deref(),
            Some("2024-03-05T13:30:00Z")
        );
        assert_eq!(
            parse("5 Mar 24 14:30 GMT").as_deref(),
            Some("2024-03-05T14:30:00Z")
        );
        assert_eq!(
            parse("Mar 5 2024 12:30 PM").as_deref(),
            Some("2024-03-06T00:30:00Z")
        );
        assert_eq!(
            parse("2012-10-10 10:10:10").as_deref(),
            Some("2012-10-10T10:10:10Z")
        );
        assert_eq!(parse("7/9/2012").as_deref(), Some("2012-07-09T12:00:00Z"));
        assert_eq!(
            parse("31 Feb 2021 10:00").as_deref(),
            Some("2021-03-03T10:00:00Z")
        );
        assert_eq!(parse("not a date"), None);
        assert_eq!(
            parse_date_lenient("5 Mar 2024 14:30 EST"),
            Err(Unported::TimeZone)
        );
    }
}
