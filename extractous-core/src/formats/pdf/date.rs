//! PDFBox's `DateConverter.toCalendar`: the dates of the document information dictionary and of
//! XMP, read big-endian (`D:YYYYMMDDHHmmSS` with an optional time zone, separators allowed) as
//! PDFBox reads them. A date PDFBox would go on to try its `SimpleDateFormat` patterns or a time
//! zone name on is unported.

use super::Unported;
use crate::formats::java_time::{CalendarFields, TimeZone};

const MILLIS_PER_MINUTE: i64 = 60_000;
const MILLIS_PER_HOUR: i64 = 60 * MILLIS_PER_MINUTE;
const HALF_DAY: i64 = 12 * MILLIS_PER_HOUR;
const DAY: i64 = 2 * HALF_DAY;

/// A position in a date being parsed, in characters.
struct Cursor<'a> {
    text: &'a [char],
    index: usize,
}

impl Cursor<'_> {
    /// `parseTimeField`: up to `max` ASCII digits, or `remedy` when there are none.
    fn field(&mut self, max: usize, remedy: i64) -> i64 {
        let limit = self.index + max.min(self.text.len() - self.index.min(self.text.len()));
        let start = self.index;
        let mut value = 0i64;
        while self.index < limit && self.text[self.index].is_ascii_digit() {
            value = value * 10 + i64::from(u32::from(self.text[self.index]) - u32::from('0'));
            self.index += 1;
        }
        if self.index == start {
            remedy
        } else {
            value
        }
    }

    /// `skipOptionals`: the characters of `optionals`, and the last of them not a space.
    fn skip_optionals(&mut self, optionals: &str) -> char {
        let mut last = ' ';
        while let Some(&c) = self.text.get(self.index) {
            if !optionals.contains(c) {
                break;
            }
            if c != ' ' {
                last = c;
            }
            self.index += 1;
        }
        last
    }

    /// `skipString`.
    fn skip(&mut self, victim: &str) -> bool {
        let victim: Vec<char> = victim.chars().collect();
        if self.text[self.index.min(self.text.len())..].starts_with(&victim) {
            self.index += victim.len();
            return true;
        }
        false
    }
}

/// `restrainTZoffset`.
fn restrain_offset(proposed: i64) -> i64 {
    if (-14 * MILLIS_PER_HOUR..=14 * MILLIS_PER_HOUR).contains(&proposed) {
        return proposed;
    }
    let mut proposed = ((proposed + HALF_DAY) % DAY + DAY) % DAY;
    if proposed == 0 {
        return HALF_DAY;
    }
    proposed = (proposed - HALF_DAY) % HALF_DAY;
    proposed
}

/// The days of a month of the Gregorian calendar.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if crate::formats::java_time::civil::is_leap_year(year) => 29,
        _ => 28,
    }
}

/// A date PDFBox reads: its instant, and the offset of the zone its calendar is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Zoned {
    pub(super) millis: i64,
    pub(super) offset: i64,
}

/// `DateConverter.toCalendar(String)`: the date's instant in milliseconds, or `None` where
/// PDFBox reads no date.
pub(super) fn to_calendar(text: &str) -> Result<Option<i64>, Unported> {
    Ok(to_zoned(text)?.map(|date| date.millis))
}

/// [`to_calendar`], with the offset of the zone the date gives, UTC where it gives none.
pub(super) fn to_zoned(text: &str) -> Result<Option<Zoned>, Unported> {
    if crate::formats::java::trim(text).is_empty() {
        return Ok(None);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = Cursor {
        text: &chars,
        index: 0,
    };
    cursor.skip_optionals(" ");
    cursor.skip("D:");
    if crate::formats::java::trim(text) == "D:" {
        return Ok(None);
    }
    cursor.skip_optionals(" ");
    let Some(millis) = big_endian(&mut cursor)? else {
        return Err(simple_date_formats(text));
    };
    let mut date = Zoned { millis, offset: 0 };
    if cursor.index != chars.len() {
        match time_zone(&mut cursor)? {
            Some(offset) => {
                date.millis -= offset;
                date.offset = offset;
            }
            None => return Err(simple_date_formats(text)),
        }
    }
    if cursor.index != chars.len() {
        return Err(simple_date_formats(text));
    }
    Ok(Some(date))
}

fn simple_date_formats(text: &str) -> Unported {
    Unported(format!(
        "a date PDFBox reads with its SimpleDateFormat patterns: {text:?}"
    ))
}

/// `parseBigEndianDate`: the date's instant in UTC, or `None` where it is no such date.
fn big_endian(cursor: &mut Cursor) -> Result<Option<i64>, Unported> {
    let start = cursor.index;
    let year = cursor.field(4, 0);
    if cursor.index != start + 4 {
        return Ok(None);
    }
    cursor.skip_optionals("/- ");
    let month = cursor.field(2, 1) - 1;
    cursor.skip_optionals("/- ");
    let day = cursor.field(2, 1);
    cursor.skip_optionals(" T");
    let hour = cursor.field(2, 0);
    cursor.skip_optionals(": ");
    let minute = cursor.field(2, 0);
    cursor.skip_optionals(": ");
    let second = cursor.field(2, 0);
    if cursor.skip_optionals(".") == '.' {
        cursor.field(19, 0);
    }
    if year == 0 {
        return Ok(None);
    }
    if year <= 1582 {
        return Err(Unported(format!(
            "a date of the year {year}, which Java's calendar counts in the Julian calendar"
        )));
    }
    let valid = (0..12).contains(&month)
        && day >= 1
        && day <= days_in_month(year, month + 1)
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..60).contains(&second);
    if !valid {
        return Ok(None);
    }
    let fields = CalendarFields {
        year: year as i32,
        month: month as i32,
        day: day as i32,
        hour: hour as i32,
        minute: minute as i32,
        second: second as i32,
        millisecond: 0,
    };
    let millis = fields.epoch_millis(&TimeZone::fixed(0));
    cursor.skip_optionals(" ");
    Ok(Some(millis))
}

/// `parseTZoffset`: the zone's offset in milliseconds, or `None` where there is no zone.
fn time_zone(cursor: &mut Cursor) -> Result<Option<i64>, Unported> {
    let initial = cursor.index;
    let mut sign = cursor.skip_optionals("Z+- ");
    let had_gmt = sign == 'Z' || cursor.skip("GMT") || cursor.skip("UTC");
    if had_gmt {
        sign = cursor.skip_optionals("+- ");
    }
    let hours = cursor.field(2, -999);
    cursor.skip_optionals("': ");
    let minutes = cursor.field(2, 0);
    cursor.skip_optionals("' ");
    if hours != -999 {
        let sign = if sign == '-' { -1 } else { 1 };
        return Ok(Some(restrain_offset(
            sign * (hours * MILLIS_PER_HOUR + minutes * MILLIS_PER_MINUTE),
        )));
    }
    if had_gmt {
        return Ok(Some(0));
    }
    let rest: String = cursor.text[initial..].iter().collect();
    if crate::formats::java::trim(&rest).is_empty() {
        cursor.index = initial;
        return Ok(None);
    }
    Err(Unported(format!(
        "a date whose time zone PDFBox looks up by name: {rest:?}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::java_time::format_instant;

    fn formatted(text: &str) -> Option<String> {
        to_calendar(text).unwrap().map(format_instant)
    }

    #[test]
    fn reads_pdf_dates_as_pdfbox_does() {
        assert_eq!(
            formatted("D:20261013193018Z").as_deref(),
            Some("2026-10-13T19:30:18Z")
        );
        assert_eq!(
            formatted("D:20261013213018+02'00'").as_deref(),
            Some("2026-10-13T19:30:18Z")
        );
        assert_eq!(
            formatted("2022-07-29T10:03:21.123-04:00").as_deref(),
            Some("2022-07-29T14:03:21Z")
        );
        assert_eq!(formatted("D:2022").as_deref(), Some("2022-01-01T00:00:00Z"));
        assert_eq!(formatted(""), None);
        assert_eq!(formatted("D:"), None);
        assert!(to_calendar("D:20220230").is_err());
        assert!(to_calendar("Tuesday, May 26 2000").is_err());
    }
}
