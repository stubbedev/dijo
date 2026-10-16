//! The dates metadata-extractor reads from text and writes: `Directory.getDate`, which parses with
//! a strict `SimpleDateFormat` in Java's default time zone, `GpsDirectory.getGpsDate`, which parses
//! leniently in UTC, and the text `TagDescriptor` and `Date.toString` make of a date.

use std::sync::LazyLock;

use regex::Regex;

use crate::java::simple_date_format::SimpleDateFormat;
use crate::java::time::{self, CalendarFields, TimeZone};
use crate::java::{digit, parse_double};

/// The patterns `getDate` tries, in its order.
const PATTERNS: [&str; 12] = [
    "yyyy:MM:dd HH:mm:ss",
    "yyyy:MM:dd HH:mm",
    "yyyy-MM-dd HH:mm:ss",
    "yyyy-MM-dd HH:mm",
    "yyyy.MM.dd HH:mm:ss",
    "yyyy.MM.dd HH:mm",
    "yyyy-MM-dd'T'HH:mm:ss",
    "yyyy-MM-dd'T'HH:mm",
    "yyyy-MM-dd",
    "yyyy-MM",
    "yyyyMMdd",
    "yyyy",
];

static SUBSECOND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9][0-9]:[0-9][0-9]:[0-9][0-9])(\.[0-9]+)").expect("the pattern compiles")
});

/// A part of a parse pattern: a numeric field, by its letter, or literal text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Field(char, usize),
    Literal(char),
}

fn compile(pattern: &str) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    let mut quoted = false;
    for character in pattern.chars() {
        if character == '\'' {
            quoted = !quoted;
            continue;
        }
        if !quoted && character.is_ascii_alphabetic() {
            match parts.last_mut() {
                Some(Part::Field(letter, count)) if *letter == character => *count += 1,
                _ => parts.push(Part::Field(character, 1)),
            }
        } else {
            parts.push(Part::Literal(character));
        }
    }
    parts
}

/// The fields a parse found, the month counted from 1.
#[derive(Debug, Default, Clone, Copy)]
struct Parsed {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    hour: Option<i64>,
    minute: Option<i64>,
    second: Option<i64>,
    millisecond: Option<i64>,
}

/// `SimpleDateFormat.parse(String)` of the numeric fields of `parts`: text after the pattern is
/// ignored; a field may start after spaces and tabs, and holds as many digits as follow it unless
/// the next part is a field too, when it holds exactly as many as its letters.
fn parse_fields(text: &str, parts: &[Part]) -> Option<Parsed> {
    let chars: Vec<char> = text.chars().collect();
    let mut position = 0;
    let mut parsed = Parsed::default();
    for (index, part) in parts.iter().enumerate() {
        match part {
            Part::Literal(literal) => {
                if chars.get(position) != Some(literal) {
                    return None;
                }
                position += 1;
            }
            Part::Field(letter, count) => {
                while matches!(chars.get(position), Some(' ' | '\t')) {
                    position += 1;
                }
                let abutting = matches!(parts.get(index + 1), Some(Part::Field(..)));
                let (value, consumed) = number(&chars[position..], abutting.then_some(*count))?;
                position += consumed;
                let slot = match letter {
                    'y' => &mut parsed.year,
                    'M' => &mut parsed.month,
                    'd' => &mut parsed.day,
                    'H' => &mut parsed.hour,
                    'm' => &mut parsed.minute,
                    's' => &mut parsed.second,
                    'S' => &mut parsed.millisecond,
                    other => unreachable!("metadata-extractor parses no {other} field"),
                };
                *slot = Some(value);
            }
        }
    }
    Some(parsed)
}

/// A number as `DecimalFormat.parse` reads one for a date field: an optional minus sign and
/// decimal digits of any script, at most `limit` characters when the field is abutted.
fn number(chars: &[char], limit: Option<usize>) -> Option<(i64, usize)> {
    let window = &chars[..limit.map_or(chars.len(), |limit| limit.min(chars.len()))];
    let negative = window.first() == Some(&'-');
    let start = usize::from(negative);
    let mut value: i64 = 0;
    let mut length = 0;
    for character in &window[start..] {
        let Some(digit) = digit(*character) else {
            break;
        };
        value = value.checked_mul(10)?.checked_add(i64::from(digit))?;
        length += 1;
    }
    if length == 0 {
        return None;
    }
    Some((if negative { -value } else { value }, start + length))
}

impl Parsed {
    fn calendar(self) -> Option<CalendarFields> {
        let field = |value: Option<i64>, default: i64| i32::try_from(value.unwrap_or(default)).ok();
        Some(CalendarFields {
            year: field(self.year, 1970)?,
            month: field(self.month, 1)? - 1,
            day: field(self.day, 1)?,
            hour: field(self.hour, 0)?,
            minute: field(self.minute, 0)?,
            second: field(self.second, 0)?,
            millisecond: field(self.millisecond, 0)?,
        })
    }
}

/// A strict parse: the fields as set, and the instant they give reading back as them, which a
/// field out of its range or a time skipped by a change of offset does not.
fn strict(parsed: Parsed, zone: &TimeZone) -> Option<i64> {
    let fields = parsed.calendar()?;
    let millis = fields.epoch_millis(zone);
    let local = time::local_fields(millis, zone);
    let matches = !local.before_christ
        && local.year == i64::from(fields.year)
        && local.month == i64::from(fields.month)
        && local.day == i64::from(fields.day)
        && local.hour == i64::from(fields.hour)
        && local.minute == i64::from(fields.minute)
        && local.second == i64::from(fields.second);
    matches.then_some(millis)
}

/// The offset of a zone `getDate` reads at the end of a date: `Z`, `±hh:mm` or `±hhmm`, which
/// `TimeZone.getTimeZone("GMT" + it)` reads, as GMT where the hours or minutes are out of range.
fn trailing_zone(text: &str) -> Option<(usize, i64)> {
    let body = java_end(text);
    let suffix = &text[..body];
    if suffix.ends_with('Z') {
        return Some((body - 1, 0));
    }
    let bytes = suffix.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = bytes.get(range)?;
        slice.iter().all(u8::is_ascii_digit).then(|| {
            slice
                .iter()
                .fold(0, |value, byte| value * 10 + i64::from(byte - b'0'))
        })
    };
    let signed = |at: usize| matches!(bytes.get(at), Some(b'+' | b'-'));
    let length = bytes.len();
    let (start, hours, minutes) = if length >= 6
        && signed(length - 6)
        && bytes[length - 3] == b':'
        && digits(length - 5..length - 3).is_some()
        && digits(length - 2..length).is_some()
    {
        (
            length - 6,
            digits(length - 5..length - 3)?,
            digits(length - 2..length)?,
        )
    } else if length >= 5 && signed(length - 5) && digits(length - 4..length).is_some() {
        (
            length - 5,
            digits(length - 4..length - 2)?,
            digits(length - 2..length)?,
        )
    } else {
        return None;
    };
    let sign = if bytes[start] == b'-' { -1 } else { 1 };
    let offset = if hours > 23 || minutes > 59 {
        0
    } else {
        sign * (hours * 3600 + minutes * 60)
    };
    Some((start, offset))
}

/// Where `$` matches in Java's regular expressions: the end of the text, or before a line
/// terminator that ends it.
fn java_end(text: &str) -> usize {
    for terminator in ["\r\n", "\n", "\r", "\u{85}", "\u{2028}", "\u{2029}"] {
        if let Some(body) = text.strip_suffix(terminator) {
            return body.len();
        }
    }
    text.len()
}

/// `Directory.getDate(tag)` of a text: the instant, in milliseconds since the epoch, of a date
/// and time in one of metadata-extractor's patterns, read in `zone` unless it names its own
/// offset, with its fraction of a second.
pub(crate) fn parse_text_date(text: &str, zone: &TimeZone) -> Option<i64> {
    if text == "0000:00:00 00:00:00" {
        return None;
    }
    let mut text = text.to_owned();
    let mut subsecond = None;
    if let Some(captures) = SUBSECOND.captures(&text) {
        subsecond = Some(captures[2][1..].to_owned());
        text = SUBSECOND.replace_all(&text, "$1").into_owned();
    }
    let named_zone;
    let zone = match trailing_zone(&text) {
        Some((start, offset)) => {
            let end = java_end(&text);
            text.replace_range(start..end, "");
            named_zone = TimeZone::fixed(offset);
            &named_zone
        }
        None => zone,
    };
    let date = PATTERNS
        .iter()
        .find_map(|pattern| strict(parse_fields(&text, &compile(pattern))?, zone))?;
    let Some(subsecond) = subsecond else {
        return Some(date);
    };
    let Some(fraction) = parse_double(&format!(".{subsecond}")) else {
        return Some(date);
    };
    let millisecond = (fraction * 1000.0) as i32;
    if (0..1000).contains(&millisecond) {
        Some(date + i64::from(millisecond))
    } else {
        Some(date)
    }
}

/// `GpsDirectory.getGpsDate`: the date stamp and the time stamp's hours, minutes and seconds,
/// written as `%s %02d:%02d:%02.3f UTC` and read back leniently with `yyyy:MM:dd HH:mm:ss.S z`.
pub(crate) fn parse_gps_date(date: &str, hours: i32, minutes: i32, seconds: f64) -> Option<i64> {
    let text = format!(
        "{date} {}:{}:{} UTC",
        crate::java::printf::integer(i64::from(hours), 2),
        crate::java::printf::integer(i64::from(minutes), 2),
        crate::java::printf::fixed(seconds, 2, 3, true)
    );
    let parts = compile("yyyy:MM:dd HH:mm:ss.S");
    let parsed = parse_fields(&text, &parts)?;
    let consumed = consumed_length(&text, &parts)?;
    let rest = &text[consumed..];
    if !rest.starts_with(" UTC") {
        return None;
    }
    Some(parsed.calendar()?.epoch_millis(&TimeZone::fixed(0)))
}

/// How many bytes of `text` a successful [`parse_fields`] of `parts` reads.
fn consumed_length(text: &str, parts: &[Part]) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let mut position = 0;
    for (index, part) in parts.iter().enumerate() {
        match part {
            Part::Literal(_) => position += 1,
            Part::Field(_, count) => {
                while matches!(chars.get(position), Some(' ' | '\t')) {
                    position += 1;
                }
                let abutting = matches!(parts.get(index + 1), Some(Part::Field(..)));
                position += number(&chars[position..], abutting.then_some(*count))?.1;
            }
        }
    }
    Some(chars[..position].iter().map(|c| c.len_utf8()).sum())
}

/// `TagDescriptor.getDescription` of a date: `EEE MMM dd HH:mm:ss Z yyyy` with a colon put into
/// the offset.
pub(crate) fn describe_date(millis: i64, zone: &TimeZone) -> String {
    let text = SimpleDateFormat::new("EEE MMM dd HH:mm:ss Z yyyy")
        .expect("the description pattern compiles")
        .format(millis, zone)
        .expect("the description pattern names no zone");
    let (before_year, year) = text.rsplit_once(' ').expect("the pattern ends with a year");
    let (head, offset) = before_year
        .rsplit_once(' ')
        .expect("the pattern has an offset before the year");
    format!("{head} {}:{} {year}", &offset[..3], &offset[3..])
}

/// `Date.toString` as the native image writes it: `EEE MMM dd HH:mm:ss`, the zone named by its
/// offset (`+02:00`), and the year.
pub(crate) fn date_to_string(millis: i64, zone: &TimeZone) -> String {
    let local = time::local_fields(millis, zone);
    let text = SimpleDateFormat::new("EEE MMM dd HH:mm:ss Z")
        .expect("the Date.toString pattern compiles")
        .format(millis, zone)
        .expect("the Date.toString pattern names no zone");
    let (head, offset) = text
        .rsplit_once(' ')
        .expect("the pattern ends with an offset");
    let year = if local.before_christ {
        1 - local.year
    } else {
        local.year
    };
    format!("{head} {}:{} {year}", &offset[..3], &offset[3..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exif_dates_in_the_zone_and_strictly() {
        let utc = TimeZone::fixed(0);
        let copenhagen_summer = TimeZone::fixed(7200);
        assert_eq!(
            parse_text_date("2009:08:11 09:09:45", &copenhagen_summer),
            Some(1_249_974_585_000)
        );
        assert_eq!(
            parse_text_date("2009:02:30 09:09:45", &utc),
            Some(1_230_768_000_000)
        );
        assert_eq!(parse_text_date("0000:00:00 00:00:00", &utc), None);
        assert_eq!(
            parse_text_date("2009:08:11 09:09:45.5", &utc),
            Some(1_249_981_785_500)
        );
        assert_eq!(
            parse_text_date("2009-08-11T09:09:45+02:00", &utc),
            Some(1_249_974_585_000)
        );
        assert_eq!(parse_text_date("20090811", &utc), Some(1_249_948_800_000));
        assert_eq!(parse_text_date("2009", &utc), Some(1_230_768_000_000));
        assert_eq!(parse_text_date("    :  :     :  :  ", &utc), None);
    }

    #[test]
    fn describes_dates_with_their_offset() {
        let zone = TimeZone::fixed(7200);
        assert_eq!(
            describe_date(1_791_836_869_000, &zone),
            "Mon Oct 12 22:27:49 +02:00 2026"
        );
        assert_eq!(
            date_to_string(1_791_836_869_000, &zone),
            "Mon Oct 12 22:27:49 +02:00 2026"
        );
    }

    #[test]
    fn reads_gps_dates_leniently_in_utc() {
        assert_eq!(
            parse_gps_date("2018:02:05", 7, 11, 43.07),
            Some(1_517_814_703_070)
        );
        assert_eq!(parse_gps_date("bad", 7, 11, 43.07), None);
    }
}
