//! Java's dates as Tika's parsers make them: a lenient `GregorianCalendar` in the default time
//! zone, with the Julian calendar before October 15, 1582, and the ISO 8601 form `Instant`
//! prints, which `Metadata.set(Property, Date)` stores.

mod zone;

pub use zone::TimeZone;

use super::simple_date_format::SimpleDateFormat;

const MILLIS_PER_DAY: i64 = 86_400_000;
/// The fixed date (days from January 1, 1 Gregorian) of January 1, 1970.
const EPOCH_FIXED_DATE: i64 = 719_163;
/// The fixed date of October 15, 1582, the first Gregorian day.
const CUTOVER_FIXED_DATE: i64 = 577_736;
const CUTOVER_YEAR: i32 = 1582;
/// The fixed date of January 1, 1 in the Julian calendar.
const JULIAN_EPOCH: i64 = -1;
const SECONDS_PER_10000_YEARS: i64 = 146_097 * 25 * 86_400;
const SECONDS_0000_TO_1970: i64 = (146_097 * 5 - (30 * 365 + 7)) * 86_400;

/// A wall-clock time as `Calendar.set(year, month, date, hourOfDay, minute, second)` takes it:
/// any values, which a lenient calendar carries over, with the month counted from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarFields {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    pub millisecond: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalendarSystem {
    Gregorian,
    Julian,
}

impl CalendarFields {
    /// The milliseconds since the epoch `GregorianCalendar.getTimeInMillis()` gives for these
    /// fields in `zone`. Java's `long` arithmetic wraps, and so does this.
    pub fn epoch_millis(self, zone: &TimeZone) -> i64 {
        let time_of_day = ((i64::from(self.hour) * 60 + i64::from(self.minute)) * 60
            + i64::from(self.second))
            * 1000
            + i64::from(self.millisecond);
        let mut fixed_date = time_of_day / MILLIS_PER_DAY;
        let mut time_of_day = time_of_day % MILLIS_PER_DAY;
        while time_of_day < 0 {
            time_of_day += MILLIS_PER_DAY;
            fixed_date -= 1;
        }
        let fixed_date = if self.year > CUTOVER_YEAR {
            let gregorian = fixed_date + self.fixed_date(CalendarSystem::Gregorian);
            if gregorian >= CUTOVER_FIXED_DATE {
                gregorian
            } else {
                fixed_date + self.fixed_date(CalendarSystem::Julian)
            }
        } else if self.year < CUTOVER_YEAR {
            fixed_date + self.fixed_date(CalendarSystem::Julian)
        } else {
            let gregorian = fixed_date + self.fixed_date(CalendarSystem::Gregorian);
            if gregorian >= CUTOVER_FIXED_DATE {
                gregorian
            } else {
                fixed_date + self.fixed_date(CalendarSystem::Julian)
            }
        };
        let wall = (fixed_date - EPOCH_FIXED_DATE)
            .wrapping_mul(MILLIS_PER_DAY)
            .wrapping_add(time_of_day);
        wall.wrapping_sub(zone.offset_by_wall(wall))
    }

    /// `GregorianCalendar.getFixedDate` for a month and day of month: the month carried into the
    /// year, then the day counted from the month's first.
    fn fixed_date(self, system: CalendarSystem) -> i64 {
        let mut year = self.year;
        let mut month = self.month;
        if month > 11 {
            year = year.wrapping_add(month / 12);
            month %= 12;
        } else if month < 0 {
            year = year.wrapping_add(month.div_euclid(12));
            month = month.rem_euclid(12);
        }
        let first = match system {
            CalendarSystem::Gregorian => gregorian_fixed_date(year, month + 1),
            CalendarSystem::Julian => julian_fixed_date(year, month + 1),
        };
        first + i64::from(self.day) - 1
    }
}

/// `BaseCalendar.getFixedDate` of the first of `month` (1 to 12) in `year`.
fn gregorian_fixed_date(year: i32, month: i32) -> i64 {
    let previous = i64::from(year) - 1;
    let month_days = i64::from((367 * month - 362) / 12);
    let mut days = 1 + 365 * previous + previous.div_euclid(4) - previous.div_euclid(100)
        + previous.div_euclid(400)
        + month_days;
    if month > 2 {
        days -= if civil::is_leap_year(i64::from(year)) {
            1
        } else {
            2
        };
    }
    days
}

/// `JulianCalendar.getFixedDate` of the first of `month` (1 to 12) in `year`, numbered as
/// astronomers do (0 is 1 BC).
fn julian_fixed_date(year: i32, month: i32) -> i64 {
    let year = i64::from(year);
    let mut days = JULIAN_EPOCH - 1 + 365 * (year - 1) + 1 + (year - 1).div_euclid(4);
    days += i64::from((367 * month - 362) / 12);
    if month > 2 {
        days -= if year.rem_euclid(4) == 0 { 1 } else { 2 };
    }
    days
}

/// The calendar fields `GregorianCalendar.computeFields` gives an instant in a zone, with the
/// first day of the week Sunday and one day enough for a first week, as in `en_US`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalFields {
    /// Whether the year is before Christ.
    pub before_christ: bool,
    /// The year of the era, from 1.
    pub year: i64,
    /// The month, from 0.
    pub month: i64,
    pub day: i64,
    /// The day of the week, from 1 for Sunday.
    pub(crate) day_of_week: i64,
    pub(crate) day_of_year: i64,
    pub(crate) day_of_week_in_month: i64,
    pub(crate) week_of_year: i64,
    pub(crate) week_of_month: i64,
    /// The year the week of the year belongs to, numbered as astronomers do.
    pub(crate) week_year: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub(crate) millisecond: i64,
    /// The zone's offset at the instant, in milliseconds.
    pub(crate) offset: i64,
}

const FIRST_DAY_OF_WEEK: i64 = 1;
const MINIMAL_DAYS_IN_FIRST_WEEK: i64 = 1;

/// `BaseCalendar.getDayOfWeekDateOnOrBefore`: the fixed date of the `day_of_week` on or before
/// `fixed_date`.
fn day_of_week_on_or_before(fixed_date: i64, day_of_week: i64) -> i64 {
    let fixed = fixed_date - (day_of_week - 1);
    fixed - fixed.rem_euclid(7)
}

/// `getWeekNumber`: the week of `fixed_date` in the period that starts on `fixed_day1`.
fn week_number(fixed_day1: i64, fixed_date: i64) -> i64 {
    let mut first = day_of_week_on_or_before(fixed_day1 + 6, FIRST_DAY_OF_WEEK);
    if first - fixed_day1 >= MINIMAL_DAYS_IN_FIRST_WEEK {
        first -= 7;
    }
    (fixed_date - first).div_euclid(7) + 1
}

/// The year, month from 1 and day of a fixed date, in the Gregorian calendar from the cutover
/// and the Julian calendar before it, the year numbered as astronomers do.
fn date_of_fixed(fixed_date: i64) -> (i64, i64, i64, CalendarSystem) {
    if fixed_date >= CUTOVER_FIXED_DATE {
        let (year, month, day) = civil::civil_from_days(fixed_date - EPOCH_FIXED_DATE);
        return (year, month, day, CalendarSystem::Gregorian);
    }
    let fd = 4 * (fixed_date - JULIAN_EPOCH) + 1464;
    let year = fd.div_euclid(1461);
    let year32 = year as i32;
    let jan1 = julian_fixed_date(year32, 1);
    let mut prior_days = fixed_date - jan1;
    if fixed_date >= julian_fixed_date(year32, 3) {
        prior_days += if year.rem_euclid(4) == 0 { 1 } else { 2 };
    }
    let month = (12 * prior_days + 373).div_euclid(367);
    let day = fixed_date - julian_fixed_date(year32, month as i32) + 1;
    (year, month, day, CalendarSystem::Julian)
}

fn fixed_of(year: i64, month: i64, system: CalendarSystem) -> i64 {
    match system {
        CalendarSystem::Gregorian => gregorian_fixed_date(year as i32, month as i32),
        CalendarSystem::Julian => julian_fixed_date(year as i32, month as i32),
    }
}

fn is_leap(year: i64, system: CalendarSystem) -> bool {
    match system {
        CalendarSystem::Gregorian => civil::is_leap_year(year),
        CalendarSystem::Julian => year.rem_euclid(4) == 0,
    }
}

/// The calendar fields of the instant `epoch_millis` in `zone`.
pub fn local_fields(epoch_millis: i64, zone: &TimeZone) -> LocalFields {
    let offset = zone.offset_at_utc(epoch_millis);
    let mut fixed_date = offset / MILLIS_PER_DAY + epoch_millis / MILLIS_PER_DAY;
    let mut time_of_day = offset % MILLIS_PER_DAY + epoch_millis % MILLIS_PER_DAY;
    if time_of_day >= MILLIS_PER_DAY {
        time_of_day -= MILLIS_PER_DAY;
        fixed_date += 1;
    } else {
        while time_of_day < 0 {
            time_of_day += MILLIS_PER_DAY;
            fixed_date -= 1;
        }
    }
    fixed_date += EPOCH_FIXED_DATE;
    let (normalized_year, month, day, system) = date_of_fixed(fixed_date);
    let jan1 = fixed_of(normalized_year, 1, system);
    let day_of_year = fixed_date - jan1 + 1;
    let mut week_year = normalized_year;
    let mut week_of_year = week_number(jan1, fixed_date);
    if week_of_year == 0 {
        let previous = normalized_year - 1;
        let previous_jan1 = jan1 - if is_leap(previous, system) { 366 } else { 365 };
        week_of_year = week_number(previous_jan1, jan1 - 1);
        week_year = previous;
    } else if week_of_year >= 52 {
        let next_jan1 = jan1
            + if is_leap(normalized_year, system) {
                366
            } else {
                365
            };
        let next_first = day_of_week_on_or_before(next_jan1 + 6, FIRST_DAY_OF_WEEK);
        if next_first - next_jan1 >= MINIMAL_DAYS_IN_FIRST_WEEK && fixed_date >= next_first - 7 {
            week_of_year = 1;
            week_year = normalized_year + 1;
        }
    }
    let month1 = fixed_date - day + 1;
    let (before_christ, year) = if normalized_year <= 0 {
        (true, 1 - normalized_year)
    } else {
        (false, normalized_year)
    };
    LocalFields {
        before_christ,
        year,
        month: month - 1,
        day,
        day_of_week: fixed_date.rem_euclid(7) + 1,
        day_of_year,
        day_of_week_in_month: (day - 1) / 7 + 1,
        week_of_year,
        week_of_month: week_number(month1, fixed_date),
        week_year,
        hour: time_of_day / 3_600_000,
        minute: time_of_day / 60_000 % 60,
        second: time_of_day / 1000 % 60,
        millisecond: time_of_day % 1000,
        offset,
    }
}

/// What `DateUtils.formatDate` makes of a `Date`: its instant truncated to the second, as
/// `Instant.toString` writes it.
pub fn format_instant(epoch_millis: i64) -> String {
    instant_string(epoch_millis.div_euclid(1000), 0)
}

/// `Date.toString` in a zone of a custom offset, as Java writes it in the `en_US` locale: `EEE MMM
/// dd HH:mm:ss`, the zone named by its offset (`+02:00`), and the year of the era.
pub fn date_to_string(epoch_millis: i64, zone: &TimeZone) -> String {
    let local = local_fields(epoch_millis, zone);
    let minutes = local.offset / 60_000;
    let sign = if minutes < 0 { '-' } else { '+' };
    let minutes = minutes.abs();
    let name = format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60);
    date_text(epoch_millis, zone, &local, &name)
}

/// `Date.toString` where Java's default time zone is UTC: `EEE MMM dd HH:mm:ss UTC` and the year
/// of the era.
pub fn utc_date_to_string(epoch_millis: i64) -> String {
    let utc = TimeZone::fixed(0);
    let local = local_fields(epoch_millis, &utc);
    date_text(epoch_millis, &utc, &local, "UTC")
}

/// `Date.toString` of the instant `epoch_millis` in `zone`, whose fields are `local`, the zone
/// named `name`.
fn date_text(epoch_millis: i64, zone: &TimeZone, local: &LocalFields, name: &str) -> String {
    let head = SimpleDateFormat::new("EEE MMM dd HH:mm:ss")
        .expect("the Date.toString pattern compiles")
        .format(epoch_millis, zone)
        .expect("the Date.toString pattern names no zone");
    format!("{head} {name} {}", local.year)
}

/// `Instant.toString`: the UTC date and time, with a fraction of three, six or nine digits when
/// the nanoseconds need them.
pub fn instant_string(seconds: i64, nanos: u32) -> String {
    let mut text = String::with_capacity(32);
    if seconds >= -SECONDS_0000_TO_1970 {
        let zero = seconds - SECONDS_PER_10000_YEARS + SECONDS_0000_TO_1970;
        let high = zero.div_euclid(SECONDS_PER_10000_YEARS) + 1;
        let low = zero.rem_euclid(SECONDS_PER_10000_YEARS);
        if high > 0 {
            text.push('+');
            text.push_str(&high.to_string());
        }
        push_date_time(&mut text, low - SECONDS_0000_TO_1970);
    } else {
        let zero = seconds + SECONDS_0000_TO_1970;
        let high = zero / SECONDS_PER_10000_YEARS;
        let low = zero % SECONDS_PER_10000_YEARS;
        let year = push_date_time(&mut text, low - SECONDS_0000_TO_1970);
        if high < 0 {
            if year == -10_000 {
                text.replace_range(..2, &(high - 1).to_string());
            } else if low == 0 {
                text.insert_str(0, &high.to_string());
            } else {
                text.insert_str(1, &high.abs().to_string());
            }
        }
    }
    if nanos > 0 {
        if nanos.is_multiple_of(1_000_000) {
            text.push_str(&format!(".{:03}", nanos / 1_000_000));
        } else if nanos.is_multiple_of(1000) {
            text.push_str(&format!(".{:06}", nanos / 1000));
        } else {
            text.push_str(&format!(".{nanos:09}"));
        }
    }
    text.push('Z');
    text
}

/// Writes the UTC date and time of `epoch_second` as `LocalDateTime.toString` does, with the
/// seconds always shown, and returns its year.
fn push_date_time(text: &mut String, epoch_second: i64) -> i64 {
    let day = epoch_second.div_euclid(86_400);
    let second_of_day = epoch_second.rem_euclid(86_400);
    let (year, month, day_of_month) = civil::civil_from_days(day);
    if year.abs() < 1000 {
        if year < 0 {
            text.push('-');
        }
        text.push_str(&format!("{:04}", year.abs()));
    } else {
        if year > 9999 {
            text.push('+');
        }
        text.push_str(&year.to_string());
    }
    text.push_str(&format!(
        "-{month:02}-{day_of_month:02}T{:02}:{:02}:{:02}",
        second_of_day / 3600,
        second_of_day / 60 % 60,
        second_of_day % 60
    ));
    year
}

/// Proleptic Gregorian dates, counted in days from 1970-01-01.
pub mod civil {
    pub fn is_leap_year(year: i64) -> bool {
        year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
    }

    pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let shifted_month = if month > 2 { month - 3 } else { month + 9 };
        let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
        let days = days + 719_468;
        let era = days.div_euclid(146_097);
        let day_of_era = days - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        (year, month, day)
    }

    pub(super) fn year_of_day(days: i64) -> i64 {
        civil_from_days(days).0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Read;
    use std::path::PathBuf;

    const DATES: &[u8] = include_bytes!("../../../tests/fixtures/rtf/dates.tsv.gz");

    /// A zone of the zoneinfo files the reference dates were made with.
    pub(crate) fn zone(id: &str) -> TimeZone {
        let directory =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rtf/zoneinfo");
        TimeZone::by_id(id, &[directory])
            .unwrap()
            .unwrap_or_else(|| panic!("the fixtures have no zone {id}"))
    }

    #[test]
    fn writes_dates_as_javas_date_to_string_does() {
        assert_eq!(
            date_to_string(1_791_836_869_000, &TimeZone::fixed(7200)),
            "Mon Oct 12 22:27:49 +02:00 2026"
        );
        assert_eq!(
            date_to_string(1_791_836_869_000, &TimeZone::fixed(-34_200)),
            "Mon Oct 12 10:57:49 -09:30 2026"
        );
        assert_eq!(
            utc_date_to_string(i64::MAX),
            "Sun Aug 17 07:12:55 UTC 292278994"
        );
        assert_eq!(
            utc_date_to_string(-1_000_000_000_000_000),
            "Tue Nov 16 22:13:20 UTC 29720"
        );
    }

    #[test]
    fn computes_dates_as_javas_lenient_gregorian_calendar_does() {
        let mut text = String::new();
        flate2::read::GzDecoder::new(DATES)
            .read_to_string(&mut text)
            .unwrap();
        let mut zones = std::collections::BTreeMap::new();
        let mut differences = Vec::new();
        let mut count = 0;
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let (millis, expected) = match fields.as_slice() {
                ["instant", millis, expected] => (millis.parse().unwrap(), *expected),
                [id, year, month, day, hour, minute, expected] => {
                    let zone = zones.entry(*id).or_insert_with(|| zone(id));
                    let fields = CalendarFields {
                        year: year.parse().unwrap(),
                        month: month.parse().unwrap(),
                        day: day.parse().unwrap(),
                        hour: hour.parse().unwrap(),
                        minute: minute.parse().unwrap(),
                        second: 0,
                        millisecond: 0,
                    };
                    (fields.epoch_millis(zone), *expected)
                }
                _ => panic!("dates.tsv has a bad line: {line}"),
            };
            count += 1;
            let actual = format_instant(millis);
            if actual != expected {
                differences.push(format!("{line}: {actual}"));
            }
        }
        assert!(count > 10_000);
        assert!(
            differences.is_empty(),
            "{} of {count} differ:\n{}",
            differences.len(),
            differences[..differences.len().min(40)].join("\n")
        );
    }

    #[test]
    fn counts_days_both_ways() {
        for days in [-1_000_000, -719_468, -1, 0, 1, 59, 60, 11_016, 2_932_896] {
            let (year, month, day) = civil::civil_from_days(days);
            assert_eq!(civil::days_from_civil(year, month, day), days);
        }
        assert_eq!(civil::civil_from_days(0), (1970, 1, 1));
        assert_eq!(gregorian_fixed_date(1970, 1), EPOCH_FIXED_DATE);
        assert_eq!(gregorian_fixed_date(1582, 10) + 14, CUTOVER_FIXED_DATE);
        assert_eq!(julian_fixed_date(1582, 10) + 4, CUTOVER_FIXED_DATE);
    }
}
