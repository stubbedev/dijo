//! The dates of an ISO 9660 image, in milliseconds since the epoch, UTC: the 7-byte form of
//! directory records and Rock Ridge's short `TF` stamps (ECMA-119 9.1.5), and the 17-byte form
//! of the volume descriptors and Rock Ridge's long `TF` stamps (ECMA-119 8.4.26.1), both with
//! the offset from Greenwich in 15-minute intervals. A date left unspecified, all zeros, or
//! naming no real time is none.

use crate::java::time::civil;

const MILLIS_PER_QUARTER_HOUR: i64 = 15 * 60 * 1000;

/// The 7-byte date `bytes`: years since 1900, month, day, hour, minute, second and offset.
pub(super) fn short(bytes: &[u8; 7]) -> Option<i64> {
    let [year, month, day, hour, minute, second, offset] = *bytes;
    if bytes[..6] == [0; 6] {
        return None;
    }
    utc_millis(
        Fields {
            year: 1900 + i64::from(year),
            month,
            day,
            hour,
            minute,
            second,
            hundredths: 0,
        },
        offset as i8,
    )
}

/// The 17-byte date `bytes`: sixteen digits, `YYYYMMDDHHMMSShh`, and the offset.
pub(super) fn long(bytes: &[u8; 17]) -> Option<i64> {
    let digits = &bytes[..16];
    if digits.iter().all(|&b| b == b'0' || b == 0) {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<u16> {
        digits[from..to].iter().try_fold(0u16, |value, &digit| {
            digit
                .is_ascii_digit()
                .then(|| value * 10 + u16::from(digit - b'0'))
        })
    };
    let two = |from: usize| number(from, from + 2).and_then(|value| u8::try_from(value).ok());
    utc_millis(
        Fields {
            year: i64::from(number(0, 4)?),
            month: two(4)?,
            day: two(6)?,
            hour: two(8)?,
            minute: two(10)?,
            second: two(12)?,
            hundredths: two(14)?,
        },
        bytes[16] as i8,
    )
}

/// A local date and time as an image records it.
struct Fields {
    year: i64,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    hundredths: u8,
}

/// The instant of `fields` at `offset` quarter hours east of Greenwich, if they name one.
fn utc_millis(fields: Fields, offset: i8) -> Option<i64> {
    let Fields {
        year,
        month,
        day,
        hour,
        minute,
        second,
        hundredths,
    } = fields;
    let month = i64::from(month);
    if !(1..=12).contains(&month) || !(-48..=52).contains(&offset) {
        return None;
    }
    let first = civil::days_from_civil(year, month, 1);
    let next = if month == 12 {
        civil::days_from_civil(year + 1, 1, 1)
    } else {
        civil::days_from_civil(year, month + 1, 1)
    };
    let day = i64::from(day);
    if day < 1 || day > next - first || hour > 23 || minute > 59 || second > 59 || hundredths > 99 {
        return None;
    }
    let seconds = (i64::from(hour) * 60 + i64::from(minute)) * 60 + i64::from(second);
    Some(
        (first + day - 1) * 86_400_000 + seconds * 1000 + i64::from(hundredths) * 10
            - i64::from(offset) * MILLIS_PER_QUARTER_HOUR,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::java::time::format_instant;

    #[test]
    fn reads_both_forms_to_utc() {
        let short_date = short(&[126, 11, 12, 20, 41, 48, 4]).map(format_instant);
        assert_eq!(short_date.as_deref(), Some("2026-11-12T19:41:48Z"));
        let long_date = long(b"2026111220414899\xF8").map(format_instant);
        assert_eq!(long_date.as_deref(), Some("2026-11-12T22:41:48Z"));
    }

    #[test]
    fn leaves_unspecified_and_impossible_dates_out() {
        assert_eq!(short(&[0; 7]), None);
        assert_eq!(short(&[126, 2, 30, 0, 0, 0, 0]), None);
        assert_eq!(short(&[126, 1, 1, 0, 0, 0, 60]), None);
        assert_eq!(long(b"0000000000000000\0"), None);
        assert_eq!(long(&[0; 17]), None);
        assert_eq!(long(b"20261312000000000"), None);
        assert_eq!(long(b"2026x112000000000"), None);
    }
}
