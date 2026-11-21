//! Tika's `TikaDates`, the one parser of the date strings documents hold: a value is routed by its
//! leading shape to a family (ISO 8601 and W3C-DTF, PDF's `D:`, Exif, RFC 5322 and other forms
//! naming the month, ctime, slash dates, strings of 8, 12 or 14 digits), which it must match
//! whole.
//!
//! Years outside [`MIN_YEAR`]..=[`MAX_YEAR`] and offsets outside -12:00..=+14:00 are rejected; the
//! calendar is the proleptic Gregorian one as written; slash dates are month first unless the
//! first field is over 12 (day first) or over 31 (`yy/M/d`); two-digit years 00 to 49 are 20xx;
//! named zones are fixed offsets and an unknown name of two to five letters leaves the zone
//! unknown; a date without a time has no zone, even where one was written.
//!
//! Java's regular expressions are matched with `regex`'s, which find what Java's backtracking
//! finds for these patterns: `\d` and `\s` are spelled out as Java reads them, `$` as Java's end
//! before a final line terminator, and the one back reference and look-behind, in the ISO
//! pattern's time, as the two alternatives they stand for.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::java::time::civil;
use crate::java::{is_digit, to_upper_case, trim, utf16_length};
use crate::unicode::is_letter;

/// `TikaDates.MIN_YEAR`.
pub const MIN_YEAR: i64 = 1000;
/// `TikaDates.MAX_YEAR`.
pub const MAX_YEAR: i64 = 2200;
/// `TikaDates.MAX_LENGTH`: a longer value is rejected unread.
const MAX_LENGTH: usize = 256;
const SECONDS_PER_DAY: i64 = 86_400;
const MILLIS_PER_DAY: i64 = SECONDS_PER_DAY * 1000;

/// How much of a date a value gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precision {
    Year,
    Month,
    Day,
    Minute,
    Second,
}

/// A `LocalDateTime` to the second, valid in the proleptic Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
}

/// Java's `DateTimeException` or `NumberFormatException`: the value is no date at all, which
/// stops the routing, where a family that does not match it lets the next one try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Invalid;

/// What a family makes of a value: a date, nothing, or [`Invalid`].
type Routed = Result<Option<ParsedDate>, Invalid>;

impl LocalDateTime {
    /// `LocalDateTime.of`.
    fn of(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
    ) -> Result<Self, Invalid> {
        let days_in_month = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if civil::is_leap_year(year) => 29,
            2 => 28,
            _ => return Err(Invalid),
        };
        if !(1..=days_in_month).contains(&day)
            || !(0..24).contains(&hour)
            || !(0..60).contains(&minute)
            || !(0..60).contains(&second)
        {
            return Err(Invalid);
        }
        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// The seconds since the epoch this local time is at `offset` seconds east of UTC.
    fn epoch_second(self, offset: i64) -> i64 {
        civil::days_from_civil(self.year, self.month, self.day) * SECONDS_PER_DAY
            + (self.hour * 60 + self.minute) * 60
            + self.second
            - offset
    }

    fn date_text(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn date_time_text(self) -> String {
        format!(
            "{}T{:02}:{:02}:{:02}",
            self.date_text(),
            self.hour,
            self.minute,
            self.second
        )
    }
}

/// `TikaDates.ParsedDate`: the local date and time written, the offset written, in seconds east
/// of UTC, if any, and how much of a date it gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedDate {
    local: LocalDateTime,
    offset: Option<i64>,
    precision: Precision,
}

impl ParsedDate {
    pub fn local(&self) -> LocalDateTime {
        self.local
    }

    /// The offset written, in seconds east of UTC.
    pub fn offset(&self) -> Option<i64> {
        self.offset
    }

    pub fn has_zone(&self) -> bool {
        self.offset.is_some()
    }

    pub fn precision(&self) -> Precision {
        self.precision
    }

    /// `isFullPrecision`: a day or finer.
    pub fn is_full_precision(&self) -> bool {
        self.precision >= Precision::Day
    }

    /// `toInstant` in seconds since the epoch: a value without a zone is read as UTC.
    pub fn epoch_second(&self) -> i64 {
        self.local.epoch_second(self.offset.unwrap_or(0))
    }

    /// `toInstant` in milliseconds since the epoch.
    pub fn epoch_millis(&self) -> i64 {
        self.epoch_second() * 1000
    }

    /// `toCanonicalString`: `yyyy-MM-dd'T'HH:mm:ss'Z'` in UTC.
    pub fn to_canonical_string(&self) -> String {
        let second = self.epoch_second();
        let (year, month, day) = civil::civil_from_days(second.div_euclid(SECONDS_PER_DAY));
        let of_day = second.rem_euclid(SECONDS_PER_DAY);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            of_day / 3600,
            of_day / 60 % 60,
            of_day % 60
        )
    }

    /// `toMetadataString`: `yyyy-MM-dd` for a date alone, else the canonical form in UTC where a
    /// zone was written, else `yyyy-MM-dd'T'HH:mm:ss` as written.
    pub fn to_metadata_string(&self) -> String {
        if self.precision == Precision::Day {
            return self.local.date_text();
        }
        if self.offset.is_some() {
            return self.to_canonical_string();
        }
        self.local.date_time_text()
    }
}

/// `TikaDates.parse`.
pub fn parse(raw: &str) -> Option<ParsedDate> {
    let trimmed = trim(raw);
    if utf16_length(trimmed) > MAX_LENGTH {
        return None;
    }
    let collapsed = WHITESPACE.replace_all(trimmed, " ");
    let s = to_upper_case(&collapsed);
    if s.is_empty() {
        return None;
    }
    route(&s)
        .ok()
        .flatten()
        .filter(|date| in_year_bounds(date.epoch_millis()))
}

/// `TikaDates.normalize`: the canonical UTC form of a date of a day or finer.
pub fn normalize(raw: &str) -> Option<String> {
    parse(raw)
        .filter(ParsedDate::is_full_precision)
        .map(|date| date.to_canonical_string())
}

/// `TikaDates.toMetadataString`: the stored form of a date of a day or finer.
pub fn to_metadata_string(raw: &str) -> Option<String> {
    parse(raw)
        .filter(ParsedDate::is_full_precision)
        .map(|date| date.to_metadata_string())
}

/// `TikaDates.toMetadataStringKeepPartial`: [`to_metadata_string`], keeping a year alone or a
/// year and month.
pub fn to_metadata_string_keep_partial(raw: &str) -> Option<String> {
    parse(raw).map(|date| match date.precision {
        Precision::Year => format!("{:04}", date.local.year),
        Precision::Month => format!("{:04}-{:02}", date.local.year, date.local.month),
        _ => date.to_metadata_string(),
    })
}

/// `TikaDates.inYearBounds` of the instant `epoch_millis`: whether its year in UTC lies within
/// [`MIN_YEAR`]..=[`MAX_YEAR`].
pub fn in_year_bounds(epoch_millis: i64) -> bool {
    let (year, _, _) = civil::civil_from_days(epoch_millis.div_euclid(MILLIS_PER_DAY));
    (MIN_YEAR..=MAX_YEAR).contains(&year)
}

/// Java's `\s`.
const S: &str = r"[ \t\n\x0B\f\r]";
/// Java's `.`.
const DOT: &str = r"[^\n\r\x{85}\x{2028}\x{2029}]";
/// Java's `$`: the end, after a final line terminator, which it leaves out of the match, if any;
/// the terminator is captured as `end` to be put back.
const END: &str = r"(?P<end>[\x{85}\x{2028}\x{2029}]?)\z";

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("TikaDates' patterns compile")
}

/// A pattern matched as `Matcher.matches` matches it: whole.
fn whole(pattern: &str) -> Regex {
    compile(&format!(r"\A(?:{pattern})\z"))
}

fn offset_pattern() -> String {
    r"(?:(?:GMT|UTC)?([+-])([0-9]{1,2})(?::?([0-9]{2}))?)".to_owned()
}

/// `ZONE`: four groups, the offset's sign, hours and minutes, and a zone's name.
fn zone_pattern() -> String {
    format!(r"(?:{S}*(?:{}|(Z|[A-Z]{{1,5}})))?", offset_pattern())
}

/// `TIME`: four groups, hours, minutes, seconds and the half of the day.
fn time_pattern() -> String {
    format!(
        r"([0-9]{{1,2}}):([0-9]{{1,2}})(?::([0-9]{{1,2}})(?:[.,][0-9]{{1,9}})?)?(?:{S}*([AP])\.?M\.?)?"
    )
}

/// `ISO`, its time's separator repeated or, after `HH:mm`, left out spelled as two alternatives:
/// the year, month and day; the hours, minutes and seconds separated by colons; those separated
/// by points; the half of the day; then [`zone_pattern`]'s groups from 11.
static ISO: LazyLock<Regex> = LazyLock::new(|| {
    let fraction = r"(?::[0-9]{2})?(?:[.,][0-9]{1,9})?";
    whole(&format!(
        r"([0-9]{{4}})(?:-([0-9]{{1,2}})(?:-([0-9]{{1,2}})(?:(?:T|{S}+)(?:([0-9]{{1,2}}):([0-9]{{2}})(?::?([0-9]{{1,2}}){fraction})?|([0-9]{{1,2}})\.([0-9]{{2}})(?:\.([0-9]{{1,2}}){fraction})?)(?:{S}*([AP])\.?M\.?)?)?)?)?{}",
        zone_pattern()
    ))
});
static PDF: LazyLock<Regex> = LazyLock::new(|| {
    whole(&format!(
        r"(D:)?{S}*([0-9]{{4}})([0-9]{{2}})?([0-9]{{2}})?([0-9]{{2}})?([0-9]{{2}})?([0-9]{{2}})?(?:(Z)(?:00'?00'?|')?|([+-])([0-9]{{1,2}})(?:'?([0-9]{{2}})'{{0,2}})?)?"
    ))
});
static EXIF: LazyLock<Regex> = LazyLock::new(|| {
    whole(&format!(
        r"([0-9]{{4}}):([0-9]{{2}}):([0-9]{{2}})(?:[ T]([0-9]{{2}}):([0-9]{{2}})(?::([0-9]{{2}})(?:\.[0-9]{{1,9}})?)?)?{}",
        zone_pattern()
    ))
});
static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    whole(&format!(
        r"([0-9]{{1,2}})[ -]([A-Z]{{3,9}})\.?,?[ -]([0-9]{{4}}|[0-9]{{2}})(?:,?{S}+{})?{}",
        time_pattern(),
        zone_pattern()
    ))
});
static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| {
    whole(&format!(
        r"([A-Z]{{3,9}})\.?{S}+([0-9]{{1,2}})(?:ST|ND|RD|TH)?,?{S}+([0-9]{{4}})(?:,?{S}+{})?{}",
        time_pattern(),
        zone_pattern()
    ))
});
static CTIME: LazyLock<Regex> = LazyLock::new(|| {
    whole(&format!(
        r"([A-Z]{{3,9}}){S}+([0-9]{{1,2}}){S}+{}(?:{S}+([A-Z]{{1,5}}))?{S}+([0-9]{{4}}){}",
        time_pattern(),
        zone_pattern()
    ))
});
static SLASH: LazyLock<Regex> = LazyLock::new(|| {
    whole(&format!(
        r"([0-9]{{1,2}})/([0-9]{{1,2}})/([0-9]{{4}}|[0-9]{{1,2}})(?:,?{S}+{})?{}",
        time_pattern(),
        zone_pattern()
    ))
});
static SLASH_YMD: LazyLock<Regex> = LazyLock::new(|| {
    whole(&format!(
        r"([0-9]{{4}})/([0-9]{{1,2}})/([0-9]{{1,2}})(?:{S}+{})?{}",
        time_pattern(),
        zone_pattern()
    ))
});
static WEEKDAY: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"\A(?:MON|TUE|TUES|WED|THU|THUR|THURS|FRI|SAT|SUN)(?:DAY|NESDAY|SDAY|RSDAY|URDAY)?\.?,?{S}+"
    ))
});
static TRAILING_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!(r"{S}*\([^()]*\){END}")));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| compile(&format!("{S}+")));
static Z_ZERO: LazyLock<Regex> = LazyLock::new(|| compile("Z00:?00"));
static OFFSET_THEN_NAME: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"([+-][0-9]{{2}}:?[0-9]{{2}}){S}+[A-Z]{{2,5}}{END}"
    ))
});
static AFTER_OFFSET: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"[0-9]:[0-9]{{2}}(?::[0-9]{{2}})?{S}*([+-][0-9]{{4}})[^0-9:\n\r\x{{85}}\x{{2028}}\x{{2029}}]{DOT}*{END}"
    ))
});
static UNSIGNED_ZERO: LazyLock<Regex> = LazyLock::new(|| compile(&format!(r"{S}0000{END}")));

/// The zone names `ZONE_NAMES` reads as fixed offsets, in hours.
const ZONE_NAMES: [(&str, i64); 27] = [
    ("Z", 0),
    ("UT", 0),
    ("UTC", 0),
    ("GMT", 0),
    ("EST", -5),
    ("EDT", -4),
    ("CST", -6),
    ("CDT", -5),
    ("MST", -7),
    ("MDT", -6),
    ("PST", -8),
    ("PDT", -7),
    ("CET", 1),
    ("CEST", 2),
    ("MET", 1),
    ("MEST", 2),
    ("WET", 0),
    ("WEST", 1),
    ("EET", 2),
    ("EEST", 3),
    ("MSK", 3),
    ("JST", 9),
    ("HST", -10),
    ("AKST", -9),
    ("AKDT", -8),
    ("NZST", 12),
    ("NZDT", 13),
];

/// `Month.name()` of each month.
const MONTHS: [&str; 12] = [
    "JANUARY",
    "FEBRUARY",
    "MARCH",
    "APRIL",
    "MAY",
    "JUNE",
    "JULY",
    "AUGUST",
    "SEPTEMBER",
    "OCTOBER",
    "NOVEMBER",
    "DECEMBER",
];

/// `charAt(index)`: the UTF-16 code unit at `index`, as a `char` where it is not half of a
/// surrogate pair.
fn char_at(s: &str, index: usize) -> Option<char> {
    s.encode_utf16()
        .nth(index)
        .map(|unit| char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}'))
}

/// `indexOf(c)` of an ASCII `c`, in UTF-16 code units.
fn index_of(s: &str, c: char) -> Option<usize> {
    s.find(c).map(|at| utf16_length(&s[..at]))
}

/// `replaceFirst` of a pattern ending in [`END`], whose terminator is put back.
fn replace_first_at_end(pattern: &Regex, s: &str, replacement: &str) -> String {
    pattern
        .replacen(s, 1, format!("{replacement}${{end}}").as_str())
        .into_owned()
}

fn route(s: &str) -> Routed {
    let s = replace_first_at_end(&TRAILING_COMMENT, s, "");
    let Some(c) = s.chars().next() else {
        return Ok(None);
    };
    if c == 'D' && s.starts_with("D:") {
        return if char_at(&s, 6) == Some('-') {
            iso(&s[2..])
        } else {
            pdf(&s)
        };
    }
    if is_digit(c) {
        let length = utf16_length(&s);
        if length > 4 && (1..4).all(|index| char_at(&s, index).is_some_and(is_digit)) {
            return match char_at(&s, 4) {
                Some('-') => iso(&s),
                Some(':') => exif(&s),
                Some('/') => slash_ymd(&s),
                Some(sep) if is_digit(sep) => pdf(&s),
                _ => Ok(None),
            };
        }
        if length == 4 {
            return iso(&s);
        }
        if index_of(&s, '/').is_some_and(|at| at > 0 && at < 3) {
            return slash(&s);
        }
        return day_month(&strip_mail_noise(&s));
    }
    let t = WEEKDAY.replacen(&s, 1, "");
    if t.is_empty() || (t.as_ref() == s.as_str() && !is_letter(c)) {
        return Ok(None);
    }
    if t.chars().next().is_some_and(is_digit) {
        return route(&t);
    }
    let t = strip_mail_noise(&t);
    match month_day(&t)? {
        Some(date) => Ok(Some(date)),
        None => ctime(&t),
    }
}

fn strip_mail_noise(s: &str) -> String {
    let mut s = match AFTER_OFFSET.captures(s) {
        Some(junk) => s[..junk.get(1).expect("the offset is captured").end()].to_owned(),
        None => s.to_owned(),
    };
    s = replace_first_at_end(&TRAILING_COMMENT, &s, "");
    s = s.replace(" AT ", " ");
    s = replace_first_at_end(&OFFSET_THEN_NAME, &s, "${1}");
    replace_first_at_end(&UNSIGNED_ZERO, &s, " +0000")
}

/// The text of group `index`.
fn group<'h>(m: &Captures<'h>, index: usize) -> Option<&'h str> {
    m.get(index).map(|found| found.as_str())
}

/// `Integer.parseInt` of a group of ASCII digits.
fn number(text: &str) -> i64 {
    text.parse()
        .expect("the patterns capture numbers of a few ASCII digits")
}

fn number_of(m: &Captures, index: usize) -> i64 {
    number(group(m, index).expect("the group takes part in the match"))
}

fn iso(s: &str) -> Routed {
    let unquoted = s.replace('\'', "");
    let text = Z_ZERO.replacen(&unquoted, 1, "Z");
    let Some(m) = ISO.captures(&text) else {
        return Ok(None);
    };
    let year = number_of(&m, 1);
    if m.get(2).is_none() {
        return partial(year, 1, Precision::Year).map(Some);
    }
    if m.get(3).is_none() {
        return partial(year, number_of(&m, 2), Precision::Month).map(Some);
    }
    let (hour, minute, second) = if m.get(4).is_some() {
        (group(&m, 4), group(&m, 5), group(&m, 6))
    } else {
        (group(&m, 7), group(&m, 8), group(&m, 9))
    };
    build(
        (year, number_of(&m, 2), number_of(&m, 3)),
        (hour, minute, second, group(&m, 10)),
        zone(&m, 11)?,
    )
}

fn pdf(s: &str) -> Routed {
    let Some(m) = PDF.captures(s) else {
        return Ok(None);
    };
    let prefixed = m.get(1).is_some();
    let digits = 4 + 2 * (3..=7).take_while(|&index| m.get(index).is_some()).count();
    if !prefixed && digits != 8 && digits != 12 && digits != 14 {
        return Ok(None);
    }
    let year = number_of(&m, 2);
    if m.get(3).is_none() {
        return partial(year, 1, Precision::Year).map(Some);
    }
    if m.get(4).is_none() {
        return partial(year, number_of(&m, 3), Precision::Month).map(Some);
    }
    if m.get(5).is_some() && m.get(6).is_none() {
        return Ok(None);
    }
    let offset = if m.get(8).is_some() {
        Some(0)
    } else if let Some(sign) = group(&m, 9) {
        Some(offset(
            sign,
            group(&m, 10).expect("the hours follow the sign"),
            group(&m, 11),
        )?)
    } else {
        None
    };
    build(
        (year, number_of(&m, 3), number_of(&m, 4)),
        (group(&m, 5), group(&m, 6), group(&m, 7), None),
        Some(offset),
    )
}

fn exif(s: &str) -> Routed {
    let Some(m) = EXIF.captures(s) else {
        return Ok(None);
    };
    build(
        (number_of(&m, 1), number_of(&m, 2), number_of(&m, 3)),
        (group(&m, 4), group(&m, 5), group(&m, 6), None),
        zone(&m, 7)?,
    )
}

fn day_month(s: &str) -> Routed {
    let Some(m) = DAY_MONTH.captures(s) else {
        return Ok(None);
    };
    let Some(month) = month(group(&m, 2).expect("the month is captured")) else {
        return Ok(None);
    };
    build(
        (
            year(group(&m, 3).expect("the year is captured")),
            month,
            number_of(&m, 1),
        ),
        (group(&m, 4), group(&m, 5), group(&m, 6), group(&m, 7)),
        zone(&m, 8)?,
    )
}

fn month_day(s: &str) -> Routed {
    let Some(m) = MONTH_DAY.captures(s) else {
        return Ok(None);
    };
    let Some(month) = month(group(&m, 1).expect("the month is captured")) else {
        return Ok(None);
    };
    build(
        (number_of(&m, 3), month, number_of(&m, 2)),
        (group(&m, 4), group(&m, 5), group(&m, 6), group(&m, 7)),
        zone(&m, 8)?,
    )
}

fn ctime(s: &str) -> Routed {
    let Some(m) = CTIME.captures(s) else {
        return Ok(None);
    };
    let Some(month) = month(group(&m, 1).expect("the month is captured")) else {
        return Ok(None);
    };
    let Some(mut zone) = zone(&m, 9)? else {
        return Ok(None);
    };
    if let Some(name) = group(&m, 7) {
        let Some(named) = named_or_unknown(name) else {
            return Ok(None);
        };
        match (zone, named) {
            (None, _) => zone = named,
            (Some(numeric), Some(named)) if numeric != named => return Ok(None),
            _ => {}
        }
    }
    build(
        (number_of(&m, 8), month, number_of(&m, 2)),
        (group(&m, 3), group(&m, 4), group(&m, 5), group(&m, 6)),
        Some(zone),
    )
}

fn slash(s: &str) -> Routed {
    let Some(m) = SLASH.captures(s) else {
        return Ok(None);
    };
    let a = number_of(&m, 1);
    let b = number_of(&m, 2);
    let time = (group(&m, 4), group(&m, 5), group(&m, 6), group(&m, 7));
    if a > 31 {
        return build(
            (
                year(group(&m, 1).expect("the year is captured")),
                b,
                number_of(&m, 3),
            ),
            time,
            zone(&m, 8)?,
        );
    }
    let third = group(&m, 3).expect("the year is captured");
    if third.len() == 1 {
        return Ok(None);
    }
    let (month, day) = if a > 12 { (b, a) } else { (a, b) };
    build((year(third), month, day), time, zone(&m, 8)?)
}

fn slash_ymd(s: &str) -> Routed {
    let Some(m) = SLASH_YMD.captures(s) else {
        return Ok(None);
    };
    build(
        (number_of(&m, 1), number_of(&m, 2), number_of(&m, 3)),
        (group(&m, 4), group(&m, 5), group(&m, 6), group(&m, 7)),
        zone(&m, 8)?,
    )
}

/// `zone`: the zone of [`zone_pattern`]'s groups from `g`, of which none means the value is
/// rejected and `Some(None)` that the zone is unknown.
fn zone(m: &Captures, g: usize) -> Result<Option<Option<i64>>, Invalid> {
    if let Some(sign) = group(m, g) {
        let hours = group(m, g + 1).expect("the hours follow the sign");
        return Ok(Some(Some(offset(sign, hours, group(m, g + 2))?)));
    }
    if let Some(name) = group(m, g + 3) {
        return Ok(named_or_unknown(name));
    }
    Ok(Some(None))
}

/// `namedOrUnknown`: the offset of a known name; an unknown one of two letters or more leaves
/// the zone unknown, and one of one letter rejects the value.
fn named_or_unknown(name: &str) -> Option<Option<i64>> {
    if let Some(&(_, hours)) = ZONE_NAMES.iter().find(|(known, _)| *known == name) {
        return Some(Some(hours * 3600));
    }
    (name.len() >= 2).then_some(None)
}

/// `offset`: in seconds east of UTC, within -12:00..=+14:00.
fn offset(sign: &str, hours: &str, minutes: Option<&str>) -> Result<i64, Invalid> {
    let hours = number(hours);
    let minutes = minutes.map_or(0, number);
    let total = hours * 60 + minutes;
    let negative = sign == "-";
    if minutes > 59 || total > if negative { 12 * 60 } else { 14 * 60 } {
        return Err(Invalid);
    }
    Ok(if negative { -total * 60 } else { total * 60 })
}

/// `month`: the month a name of three letters or more begins, from 1.
fn month(name: &str) -> Option<i64> {
    if name.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|full| full[..3] == name[..3] && full.starts_with(name))
        .map(|index| index as i64 + 1)
}

/// `year`: a year of two digits from 1950 to 2049.
fn year(text: &str) -> i64 {
    let value = number(text);
    if text.len() == 2 {
        if value < 50 {
            2000 + value
        } else {
            1900 + value
        }
    } else {
        value
    }
}

fn partial(year: i64, month: i64, precision: Precision) -> Result<ParsedDate, Invalid> {
    Ok(ParsedDate {
        local: LocalDateTime::of(year, month, 1, 12, 0, 0)?,
        offset: None,
        precision,
    })
}

/// `build`: the date of `(year, month, day)` at the time of `(hours, minutes, seconds, half of
/// the day)` in `zone`, of which none rejects the value; a date without a time is at midday, of
/// no zone.
fn build(
    (year, month, day): (i64, i64, i64),
    (hours, minutes, seconds, half): (Option<&str>, Option<&str>, Option<&str>, Option<&str>),
    zone: Option<Option<i64>>,
) -> Routed {
    let Some(offset) = zone else {
        return Ok(None);
    };
    let Some(hours) = hours else {
        return Ok(Some(ParsedDate {
            local: LocalDateTime::of(year, month, day, 12, 0, 0)?,
            offset: None,
            precision: Precision::Day,
        }));
    };
    let mut hour = number(hours);
    if let Some(half) = half {
        if !(1..=12).contains(&hour) {
            return Ok(None);
        }
        hour = hour % 12 + if half == "P" { 12 } else { 0 };
    }
    let minute = number(minutes.expect("the minutes follow the hours"));
    let second = seconds.map_or(0, number);
    Ok(Some(ParsedDate {
        local: LocalDateTime::of(year, month, day, hour, minute, second)?,
        offset,
        precision: if seconds.is_some() {
            Precision::Second
        } else {
            Precision::Minute
        },
    }))
}

#[cfg(test)]
mod tests;
