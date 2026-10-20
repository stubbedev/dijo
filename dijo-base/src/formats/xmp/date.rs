//! PDFBox's `DateConverter.toCalendar`: the dates of the document information dictionary and of
//! XMP, read big-endian (`D:YYYYMMDDHHmmSS` with an optional time zone, separators allowed) as
//! PDFBox reads them, its time zone an offset or a name the JDK knows. A date PDFBox would go on
//! to try its `SimpleDateFormat` patterns on is unported.

use std::sync::LazyLock;

use regex::Regex;

use crate::formats::Unported;
use crate::java::time::{CalendarFields, TimeZone};

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
        _ if crate::java::time::civil::is_leap_year(year) => 29,
        _ => 28,
    }
}

/// A date PDFBox reads: its instant, and the offset of the zone its calendar is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zoned {
    pub millis: i64,
    pub offset: i64,
}

/// `DateConverter.toCalendar(String)`: the date's instant in milliseconds, or `None` where
/// PDFBox reads no date.
pub fn to_calendar(text: &str) -> Result<Option<i64>, Unported> {
    Ok(to_zoned(text)?.map(|date| date.millis))
}

/// [`to_calendar`], with the offset of the zone the date gives, UTC where it gives none.
pub fn to_zoned(text: &str) -> Result<Option<Zoned>, Unported> {
    if crate::java::trim(text).is_empty() {
        return Ok(None);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = Cursor {
        text: &chars,
        index: 0,
    };
    cursor.skip_optionals(" ");
    cursor.skip("D:");
    if crate::java::trim(text) == "D:" {
        return Ok(None);
    }
    cursor.skip_optionals(" ");
    let Some(millis) = big_endian(&mut cursor)? else {
        return Err(simple_date_formats(text));
    };
    let rest = cursor.index;
    if rest == chars.len() {
        return Ok(Some(Zoned { millis, offset: 0 }));
    }
    let zone = time_zone(&mut cursor)?;
    match zone {
        Some(zone) if cursor.index == chars.len() => {
            let applied = zone.offset_at_utc(millis) / MILLIS_PER_MINUTE * MILLIS_PER_MINUTE;
            let millis = millis - applied;
            Ok(Some(Zoned {
                millis,
                offset: zone.offset_at_utc(millis),
            }))
        }
        _ => unread(&chars[rest..], text),
    }
}

/// A date whose time zone does not take up the rest of the text after its big-endian date.
/// After a date of four digits and more, a rest of what zone names are written with, ASCII
/// letters, `/` and `_`, is consumed by none of PDFBox's `SimpleDateFormat` patterns, nor by a
/// time zone after one, so PDFBox reads no date; any other rest is left to those patterns.
fn unread(rest: &[char], text: &str) -> Result<Option<Zoned>, Unported> {
    match rest
        .iter()
        .all(|&c| c.is_ascii_alphabetic() || c == '/' || c == '_')
    {
        true => Ok(None),
        false => Err(simple_date_formats(text)),
    }
}

/// `new DateUtils().tryToParse`, which `XmpDates.normalize` falls back to where PDFBox reads no
/// date: the first of its ISO 8601 patterns the text starts with, a date and time without a
/// zone read in Java's default zone, which the port takes to be UTC as the image parsers do
/// (#114), and a date alone at midday UTC. A text whose start is not two-digit fields after a
/// four-digit year, or whose zone `SimpleDateFormat` would look up by name or offset, is
/// unported.
pub fn try_to_parse(text: &str) -> Result<Option<i64>, Unported> {
    static DATE_TIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(\d{4})([-:])(\d{2})([-:])(\d{2})(?:[T ](\d{2}):(\d{2}):(\d{2}))?")
            .expect("the pattern compiles")
    });
    if text.encode_utf16().count() < 6 {
        return Ok(None);
    }
    let start = text.trim_start_matches([' ', '\t']);
    let unported = || Unported(format!("a date Tika's DateUtils reads leniently: {text:?}"));
    let Some(captures) = DATE_TIME.captures(start) else {
        return match start.chars().next().is_some_and(char::is_alphabetic) {
            true => Ok(None),
            false => Err(unported()),
        };
    };
    let field = |index: usize| -> i32 { captures[index].parse().expect("digits") };
    if captures[2] != captures[4] {
        return Ok(None);
    }
    let timed = &captures[2] == "-" && captures.get(6).is_some();
    let rest = &start[captures[0].len()..];
    if rest.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(unported());
    }
    let zone = rest.trim_start_matches([' ', '\t']);
    if timed
        && !rest.starts_with('Z')
        && zone.starts_with(|c: char| c == '+' || c == '-' || c.is_alphabetic())
    {
        return Err(unported());
    }
    let (hour, minute, second) = match timed {
        true => (field(6), field(7), field(8)),
        false => (12, 0, 0),
    };
    let fields = CalendarFields {
        year: field(1),
        month: field(3) - 1,
        day: field(5),
        hour,
        minute,
        second,
        millisecond: 0,
    };
    Ok(fields.epoch_millis(&TimeZone::fixed(0)))
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
    let Some(millis) = fields.epoch_millis(&TimeZone::fixed(0)) else {
        return Ok(None);
    };
    cursor.skip_optionals(" ");
    Ok(Some(millis))
}

/// `parseTZoffset`: the zone of an offset, or of a name `TimeZone.getTimeZone` knows, or `None`
/// where there is no zone.
fn time_zone(cursor: &mut Cursor) -> Result<Option<TimeZone>, Unported> {
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
        let offset =
            restrain_offset(sign * (hours * MILLIS_PER_HOUR + minutes * MILLIS_PER_MINUTE));
        return Ok(Some(TimeZone::fixed(offset / 1000)));
    }
    if had_gmt {
        return Ok(Some(TimeZone::fixed(0)));
    }
    let rest: String = cursor.text[initial..].iter().collect();
    let name = crate::java::trim(&rest);
    let zone = TimeZone::jdk_named(name).map_err(|e| {
        Unported(format!(
            "a date in a time zone PDFBox looks up by name: {e}"
        ))
    })?;
    cursor.index = match zone {
        Some(_) => cursor.text.len(),
        None => initial,
    };
    Ok(zone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::java::time::format_instant;

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

    /// Dates PDFBox reads none of, read as Tika's `DateUtils.tryToParse` reads them with Java's
    /// default zone UTC; those whose zone it looks up by name unported.
    #[test]
    fn falls_back_to_tikas_date_utils() {
        let parsed = |text| try_to_parse(text).unwrap().map(format_instant);
        assert_eq!(
            parsed("2010-07-28T11:02:12.000CEST").as_deref(),
            Some("2010-07-28T11:02:12Z")
        );
        assert_eq!(
            parsed("2010-07-28TCEST").as_deref(),
            Some("2010-07-28T12:00:00Z")
        );
        assert_eq!(
            parsed("2010:07:28 11:02:12").as_deref(),
            Some("2010-07-28T12:00:00Z")
        );
        assert_eq!(
            parsed("2010-13-28T25:02:12.0x").as_deref(),
            Some("2011-01-29T01:02:12Z")
        );
        assert_eq!(
            parsed("2010-07-28T11:02:12Zulu").as_deref(),
            Some("2010-07-28T11:02:12Z")
        );
        assert_eq!(parsed("2010-07:28"), None);
        assert_eq!(parsed("abcdef"), None);
        assert!(try_to_parse("2010-07-28 11:02:12 CEST").is_err());
        assert_eq!(
            to_zoned("2010-07-28T11:02:12.000CEST").unwrap(),
            None,
            "PDFBox knows no zone CEST"
        );
    }

    /// Dates ending in every zone name the JDK knows, and in names it does not, read as
    /// `NamedZoneDates.java` has PDFBox read them (`just xmp-date-reference`): the instant and
    /// the offset of the calendar's zone at it, or no date. The `SystemV` zones with daylight
    /// saving rules of the JDK's own are unported. Morocco's zones are left out in 2030: the
    /// bundled database, newer than the JDK's, no longer has Morocco at +01 then.
    #[test]
    fn reads_zone_names_as_pdfbox_does() {
        const NEWER_RULES: [&str; 2] = [
            "2030-10-27 02:30:00Africa/Casablanca",
            "2030-10-27 02:30:00Africa/El_Aaiun",
        ];
        use std::io::Read;
        let mut reference = String::new();
        flate2::read::GzDecoder::new(
            &include_bytes!("../../../tests/fixtures/xmp/named-zone-dates.tsv.gz")[..],
        )
        .read_to_string(&mut reference)
        .unwrap();
        let mut differences = Vec::new();
        let mut count = 0;
        for line in reference.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let expected = match fields.as_slice() {
                [_, "null"] => None,
                [_, instant, offset] => Some(format!("{instant} {offset}")),
                _ => panic!("named-zone-dates.tsv has a bad line: {line}"),
            };
            count += 1;
            if NEWER_RULES.contains(&fields[0]) {
                continue;
            }
            if fields[0].contains("SystemV/") && fields[0].ends_with("DT") {
                assert!(to_zoned(fields[0]).is_err(), "{line}");
                continue;
            }
            let actual = match to_zoned(fields[0]) {
                Err(e) => Some(format!("unported: {}", e.0)),
                Ok(date) => {
                    date.map(|date| format!("{} {}", format_instant(date.millis), date.offset))
                }
            };
            if actual != expected {
                differences.push(format!("{line}: {actual:?}"));
            }
        }
        assert!(count > 3000);
        assert!(
            differences.is_empty(),
            "{} of {count} differ:\n{}",
            differences.len(),
            differences[..differences.len().min(40)].join("\n")
        );
    }
}
