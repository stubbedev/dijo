//! `PNGMetadata.parseEncodedTime`: a `Creation Time` text as `OffsetDateTime.parse` reads it
//! with `RFC_1123_DATE_TIME`, else as `ISO_DATE_TIME` parses it best, else no time. The port
//! reads the forms the formatters print and the texts neither can start to read; a text the
//! lenient parsing of either might still read is unported.

use crate::formats::Unported;

/// The year, month, day, hour, minute and second the text gives, as written in its offset.
pub(crate) type Fields = [i32; 6];

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The time a `Creation Time` text gives, or none when neither formatter parses it.
pub(crate) fn parse(text: &str) -> Result<Option<Fields>, Unported> {
    if let Some(parsed) = rfc_1123(text) {
        return parsed;
    }
    if let Some(parsed) = iso(text) {
        return Ok(parsed);
    }
    if starts_unreadable(text) {
        return Ok(None);
    }
    Err(unported(text))
}

fn unported(text: &str) -> Unported {
    Unported::new(format!(
        "a PNG creation time {text:?}, which java.time's lenient parsing might read"
    ))
}

/// Neither formatter reads a text that starts with neither a number, a sign, nor a day of the
/// week and its comma.
fn starts_unreadable(text: &str) -> bool {
    let numeric = text
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_digit() || first == '+' || first == '-');
    let weekday = DAYS.iter().any(|day| {
        text.get(..5)
            .is_some_and(|start| start.eq_ignore_ascii_case(&format!("{day}, ")))
    });
    !numeric && !weekday
}

fn digits(text: &str) -> Option<i32> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn valid(fields: Fields) -> bool {
    let [year, month, day, hour, minute, second] = fields;
    (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && (0..=23).contains(&hour)
        && (0..=59).contains(&minute)
        && (0..=59).contains(&second)
}

/// The ISO day of the week, Monday as zero.
fn day_of_week([year, month, day, ..]: Fields) -> usize {
    let (year, month) = if month < 3 {
        (i64::from(year) - 1, i64::from(month) + 12)
    } else {
        (i64::from(year), i64::from(month))
    };
    let zeller = (i64::from(day) + 13 * (month + 1) / 5 + year + year / 4 - year / 100
        + year / 400)
        .rem_euclid(7);
    ((zeller + 5) % 7) as usize
}

/// `HH:mm` with optional `:ss`.
fn time(text: &str) -> Option<[i32; 3]> {
    let parts: Vec<&str> = text.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|part| part.len() != 2) {
        return None;
    }
    Some([
        digits(parts[0])?,
        digits(parts[1])?,
        parts.get(2).map_or(Some(0), |second| digits(second))?,
    ])
}

/// The form `RFC_1123_DATE_TIME` prints, `[Ddd, ]d MMM yyyy HH:mm[:ss] (+HHMM|GMT)`, in any
/// case: none when the text is not of it, no time when its day of the week is not the date's,
/// which the resolver's cross check rejects, and unported when its values are ones the `SMART`
/// resolver would adjust or reject.
fn rfc_1123(text: &str) -> Option<Result<Option<Fields>, Unported>> {
    let (weekday, rest) = match DAYS.iter().position(|day| {
        text.get(..5)
            .is_some_and(|start| start.eq_ignore_ascii_case(&format!("{day}, ")))
    }) {
        Some(index) => (Some(index), &text[5..]),
        None => (None, text),
    };
    let parts: Vec<&str> = rest.split(' ').collect();
    let [day, month, year, clock, offset] = parts.as_slice() else {
        return None;
    };
    if !(1..=2).contains(&day.len()) || year.len() != 4 {
        return None;
    }
    let month = MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(month))?;
    let offset_valid = offset.eq_ignore_ascii_case("GMT")
        || (offset.len() == 5
            && matches!(offset.as_bytes()[0], b'+' | b'-')
            && digits(&offset[1..]).is_some_and(|hhmm| hhmm / 100 <= 18 && hhmm % 100 <= 59));
    if !offset_valid {
        return None;
    }
    let [hour, minute, second] = time(clock)?;
    let fields = [
        digits(year)?,
        month as i32 + 1,
        digits(day)?,
        hour,
        minute,
        second,
    ];
    if !valid(fields) {
        return Some(Err(unported(text)));
    }
    Some(Ok((weekday
        .is_none_or(|weekday| weekday == day_of_week(fields)))
    .then_some(fields)))
}

/// The form `ISO_DATE_TIME` prints without a zone region, `yyyy-MM-dd'T'HH:mm[:ss[.fraction]]`
/// with an optional `Z` or `+HH:MM[:ss]` offset: none when the text is not of it, no time when
/// the `STRICT` resolver rejects its values.
fn iso(text: &str) -> Option<Option<Fields>> {
    let bytes = text.as_bytes();
    if bytes.len() < 16
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || !text.is_char_boundary(16)
    {
        return None;
    }
    let (date, rest) = (&text[..10], &text[11..]);
    let year = digits(&date[..4])?;
    let month = digits(&date[5..7])?;
    let day = digits(&date[8..10])?;
    let clock_end = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
    let (clock, offset) = rest.split_at(clock_end);
    if !(offset.is_empty() || offset == "Z" || offset_id(offset)) {
        return None;
    }
    let (clock, fraction) = match clock.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (clock, None),
    };
    if let Some(fraction) = fraction {
        if clock.len() != 8 || fraction.len() > 9 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let [hour, minute, second] = time(clock)?;
    let fields = [year, month, day, hour, minute, second];
    Some(valid(fields).then_some(fields))
}

/// `appendOffsetId`'s `+HH:MM:ss` as it parses strictly: hours and minutes, seconds optional.
fn offset_id(offset: &str) -> bool {
    let Some(rest) = offset.strip_prefix(['+', '-']) else {
        return false;
    };
    let parts: Vec<&str> = rest.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|part| part.len() == 2 && digits(part).is_some())
        && digits(parts[0]).is_some_and(|hours| hours <= 18)
        && parts[1..]
            .iter()
            .all(|part| digits(part).is_some_and(|value| value <= 59))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_forms_java_time_prints() {
        assert_eq!(
            parse("Mon, 4 Nov 2024 10:16:25 +0100").unwrap(),
            Some([2024, 11, 4, 10, 16, 25])
        );
        assert_eq!(
            parse("tue, 05 nov 2024 10:16 GMT").unwrap(),
            Some([2024, 11, 5, 10, 16, 0])
        );
        assert_eq!(
            parse("2024-11-04T10:16:25.123+01:00").unwrap(),
            Some([2024, 11, 4, 10, 16, 25])
        );
        assert_eq!(
            parse("2024-11-04t10:16").unwrap(),
            Some([2024, 11, 4, 10, 16, 0])
        );
        assert_eq!(
            parse("2024-11-04T10:16:25.").unwrap(),
            Some([2024, 11, 4, 10, 16, 25])
        );
        assert_eq!(
            parse("2024-11-04T10:16:25-05:30:15").unwrap(),
            Some([2024, 11, 4, 10, 16, 25])
        );
        assert_eq!(
            parse("4 Nov 2024 10:16:25 GMT").unwrap(),
            Some([2024, 11, 4, 10, 16, 25])
        );
    }

    #[test]
    fn reads_no_time_from_texts_neither_formatter_reads() {
        assert_eq!(parse("Mon 04 Nov 2024 10:16:25 AM CET").unwrap(), None);
        assert_eq!(parse("2024-02-30T10:16:25Z").unwrap(), None);
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("Mon, 05 Nov 2024 10:16:25 +0100").unwrap(), None);
    }

    #[test]
    fn leaves_texts_lenient_parsing_might_read_unported() {
        assert!(parse("2024:11:04 10:16:25").is_err());
        assert!(parse("Mon, 31 Apr 2024 10:16:25 +0100").is_err());
        assert!(parse("Sat, 4 Nov 2024 10:16:25 CET").is_err());
        assert!(parse("2024-11-04T10:16:25[Europe/Copenhagen]").is_err());
    }
}
