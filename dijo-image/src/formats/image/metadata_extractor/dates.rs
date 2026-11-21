//! The text `TagDescriptor` makes of a date metadata-extractor read.

use crate::java::simple_date_format::SimpleDateFormat;
use crate::java::time::TimeZone;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_dates_with_their_offset() {
        let zone = TimeZone::fixed(7200);
        assert_eq!(
            describe_date(1_791_836_869_000, &zone),
            "Mon Oct 12 22:27:49 +02:00 2026"
        );
    }
}
