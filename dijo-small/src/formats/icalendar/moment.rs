//! The dates and times of iCalendar values (RFC 5545, sections 3.3.4, 3.3.5 and 3.6.5): a date,
//! a date and time in UTC, a floating date and time, or a date and time in the zone its `TZID`
//! names, resolved to UTC by the calendar's own `VTIMEZONE` of that name, else by the zone of the
//! time zone database bundled into the binary. Times are held as seconds since 1970-01-01T00:00
//! of the wall clock they are read on; nothing depends on the machine's own zone.

use std::collections::BTreeMap;

use crate::formats::content_line::ContentLine;
use crate::java::time::{civil, instant_string, CalendarFields, TimeZone};

const SECONDS_PER_DAY: i64 = 86_400;

/// When a property says something happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Moment {
    /// A date, in days since 1970-01-01, which no zone shifts.
    Date(i64),
    /// An instant, in seconds since the epoch.
    Utc(i64),
    /// A wall-clock time in whatever zone it is read in.
    Floating(i64),
    /// A wall-clock time in a zone neither the calendar nor the time zone database defines.
    Unresolved { wall: i64, zone: String },
}

impl Moment {
    /// The moment of a `DTSTART`, `DTEND`, `DUE` or like property, its `TZID` resolved by `zones`.
    pub(super) fn of(line: &ContentLine, zones: &mut Zones) -> Result<Self, String> {
        let value = line.raw.trim();
        let is_date = line
            .parameter("VALUE")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("DATE"))
            || value.len() == 8;
        if is_date {
            return parse_date(value)
                .map(Moment::Date)
                .ok_or_else(|| format!("the {} value {value:?} is not a date", line.name));
        }
        let (wall, utc) = parse_date_time(value)
            .ok_or_else(|| format!("the {} value {value:?} is not a date-time", line.name))?;
        if utc {
            return Ok(Moment::Utc(wall));
        }
        let Some(zone) = line.parameter("TZID") else {
            return Ok(Moment::Floating(wall));
        };
        Ok(match zones.utc(zone, wall)? {
            Some(utc) => Moment::Utc(utc),
            None => Moment::Unresolved {
                wall,
                zone: zone.to_owned(),
            },
        })
    }
}

impl std::fmt::Display for Moment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Moment::Date(days) => f.write_str(&date_string(*days)),
            Moment::Utc(seconds) => f.write_str(&instant_string(*seconds, 0)),
            Moment::Floating(wall) => write!(f, "{} (floating)", wall_string(*wall)),
            Moment::Unresolved { wall, zone } => write!(f, "{} ({zone})", wall_string(*wall)),
        }
    }
}

/// `YYYY-MM-DD`.
fn date_string(days: i64) -> String {
    let (year, month, day) = civil::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DDTHH:MM:SS`, without a zone.
fn wall_string(wall: i64) -> String {
    let second = wall.rem_euclid(SECONDS_PER_DAY);
    format!(
        "{}T{:02}:{:02}:{:02}",
        date_string(wall.div_euclid(SECONDS_PER_DAY)),
        second / 3600,
        second / 60 % 60,
        second % 60
    )
}

/// The digits of `text` as a number, or `None` when it holds anything else or nothing.
fn number(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A `DATE` (`YYYYMMDD`), in days since 1970-01-01, or `None` when it is not a valid date.
fn parse_date(text: &str) -> Option<i64> {
    if text.len() != 8 || !text.is_ascii() {
        return None;
    }
    let (year, month, day) = (
        number(&text[..4])?,
        number(&text[4..6])?,
        number(&text[6..])?,
    );
    let length = match month {
        2 if civil::is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    (1..=length)
        .contains(&day)
        .then(|| civil::days_from_civil(year, month, day))
}

/// A `DATE-TIME` (`YYYYMMDDTHHMMSS`, `Z` ending one in UTC): its wall-clock seconds, and whether
/// it is in UTC.
fn parse_date_time(text: &str) -> Option<(i64, bool)> {
    let (text, utc) = match text.strip_suffix(['Z', 'z']) {
        Some(text) => (text, true),
        None => (text, false),
    };
    if text.len() != 15 || !text.is_ascii() || !text[8..9].eq_ignore_ascii_case("T") {
        return None;
    }
    let days = parse_date(&text[..8])?;
    let (hour, minute, second) = (
        number(&text[9..11])?,
        number(&text[11..13])?,
        number(&text[13..])?,
    );
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some((
        days * SECONDS_PER_DAY + hour * 3600 + minute * 60 + second,
        utc,
    ))
}

/// A UTC offset (`+HHMM` or `-HHMMSS`), in seconds east of UTC.
fn parse_offset(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if !matches!(digits.len(), 4 | 6) {
        return None;
    }
    let hours = number(&digits[..2])?;
    let minutes = number(&digits[2..4])?;
    let seconds = if digits.len() == 6 {
        number(&digits[4..])?
    } else {
        0
    };
    (minutes < 60 && seconds < 60).then_some(sign * (hours * 3600 + minutes * 60 + seconds))
}

/// The zones the times of a file's calendars are resolved in: each calendar's own `VTIMEZONE`s
/// by `TZID`, and the zones of the bundled database they name, looked up once each.
#[derive(Debug, Default)]
pub(super) struct Zones {
    /// The `VTIMEZONE`s by the number of their calendar in the file and their `TZID`.
    defined: BTreeMap<(usize, String), Zone>,
    /// The number of the calendar being read, from 1, or 0 before the first.
    calendar: usize,
    database: BTreeMap<String, Option<TimeZone>>,
}

impl Zones {
    /// Starts the file's next calendar, whose `VTIMEZONE`s are its own.
    pub(super) fn next_calendar(&mut self) {
        self.calendar += 1;
    }

    /// Goes back to before the file's first calendar, to read the file again.
    pub(super) fn rewind(&mut self) {
        self.calendar = 0;
    }

    /// Defines the `VTIMEZONE` `tzid` of the calendar being read; a second definition of a
    /// `TZID` in one calendar is an error.
    pub(super) fn define(&mut self, tzid: String, zone: Zone) -> Result<(), String> {
        let key = (self.calendar, tzid);
        if self.defined.contains_key(&key) {
            return Err(format!("the VTIMEZONE {:?} is defined twice", key.1));
        }
        self.defined.insert(key, zone);
        Ok(())
    }

    /// The instant of the wall-clock time `wall` in the zone `tzid`, or `None` when neither the
    /// calendar being read nor the database defines that zone.
    fn utc(&mut self, tzid: &str, wall: i64) -> Result<Option<i64>, String> {
        if let Some(zone) = self.defined.get(&(self.calendar, tzid.to_owned())) {
            return Ok(Some(zone.utc(wall)));
        }
        let name = tzid.strip_prefix('/').unwrap_or(tzid);
        if !self.database.contains_key(name) {
            let zone = TimeZone::jdk_named(name)
                .map_err(|e| format!("the zone {tzid:?} does not resolve: {e}"))?;
            self.database.insert(name.to_owned(), zone);
        }
        let Some(zone) = &self.database[name] else {
            return Ok(None);
        };
        let (year, month, day) = civil::civil_from_days(wall.div_euclid(SECONDS_PER_DAY));
        let second = wall.rem_euclid(SECONDS_PER_DAY);
        let field = |value: i64| {
            i32::try_from(value).map_err(|_| format!("the year {value} is out of range"))
        };
        let fields = CalendarFields {
            year: field(year)?,
            month: field(month - 1)?,
            day: field(day)?,
            hour: field(second / 3600)?,
            minute: field(second / 60 % 60)?,
            second: field(second % 60)?,
            millisecond: 0,
        };
        let millis = fields
            .epoch_millis(zone)
            .ok_or_else(|| format!("the time {} is out of range", wall_string(wall)))?;
        Ok(Some(millis.div_euclid(1000)))
    }
}

/// A `VTIMEZONE`: its observances, of which the one with the latest onset at or before a time
/// holds then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Zone {
    observances: Vec<Observance>,
}

impl Zone {
    /// The zone of `observances`, which must not be empty.
    pub(super) fn new(observances: Vec<Observance>) -> Result<Self, String> {
        if observances.is_empty() {
            return Err("it has no STANDARD or DAYLIGHT observance".to_owned());
        }
        Ok(Self { observances })
    }

    /// The instant of the wall-clock time `wall`: at the offset of the observance that began
    /// last at or before it, or, before every observance, at the offset the first one began
    /// from.
    fn utc(&self, wall: i64) -> i64 {
        let latest = self
            .observances
            .iter()
            .filter_map(|observance| Some((observance.latest_onset(wall)?, observance)))
            .fold(
                None,
                |best: Option<(i64, &Observance)>, candidate| match best {
                    Some(best) if best.0 >= candidate.0 => Some(best),
                    _ => Some(candidate),
                },
            );
        match latest {
            Some((_, observance)) => wall - observance.offset_to,
            None => {
                let first = self
                    .observances
                    .iter()
                    .min_by_key(|observance| observance.start)
                    .expect("a zone has an observance");
                wall - first.offset_from
            }
        }
    }
}

/// A `STANDARD` or `DAYLIGHT` observance: from its onsets on, the offset `offset_to` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Observance {
    /// The first onset, on the wall clock of `offset_from`.
    start: i64,
    offset_from: i64,
    offset_to: i64,
    rule: Option<YearlyRule>,
    /// The onsets `RDATE` adds.
    dates: Vec<i64>,
}

impl Observance {
    /// The latest onset at or before the wall-clock time `wall`.
    fn latest_onset(&self, wall: i64) -> Option<i64> {
        let mut latest = (self.start <= wall).then_some(self.start);
        for &date in &self.dates {
            if date <= wall {
                latest = latest.max(Some(date));
            }
        }
        if let Some(rule) = &self.rule {
            latest = latest.max(rule.latest(self, wall));
        }
        latest
    }
}

/// What an observance is built from, property by property.
#[derive(Debug, Default)]
pub(super) struct ObservanceBuilder {
    start: Option<i64>,
    offset_from: Option<i64>,
    offset_to: Option<i64>,
    rule: Option<String>,
    dates: Vec<i64>,
}

impl ObservanceBuilder {
    /// Takes in a property of the observance.
    pub(super) fn property(&mut self, line: &ContentLine) -> Result<(), String> {
        let malformed = || format!("the {} value {:?} is malformed", line.name, line.raw);
        if line.is("DTSTART") {
            self.start = Some(parse_date_time(line.raw.trim()).ok_or_else(malformed)?.0);
        } else if line.is("TZOFFSETFROM") {
            self.offset_from = Some(parse_offset(line.raw).ok_or_else(malformed)?);
        } else if line.is("TZOFFSETTO") {
            self.offset_to = Some(parse_offset(line.raw).ok_or_else(malformed)?);
        } else if line.is("RRULE") {
            if self.rule.is_some() {
                return Err("an observance has more than one RRULE".to_owned());
            }
            self.rule = Some(line.raw.to_owned());
        } else if line.is("RDATE") {
            if line
                .parameter("VALUE")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("PERIOD"))
            {
                return Err("an observance's RDATE is a PERIOD".to_owned());
            }
            for date in line.raw.split(',') {
                self.dates
                    .push(parse_date_time(date.trim()).ok_or_else(malformed)?.0);
            }
        }
        Ok(())
    }

    /// The observance, or what it lacks.
    pub(super) fn build(self) -> Result<Observance, String> {
        let missing = |name: &str| format!("an observance has no {name}");
        let start = self.start.ok_or_else(|| missing("DTSTART"))?;
        let offset_from = self.offset_from.ok_or_else(|| missing("TZOFFSETFROM"))?;
        let offset_to = self.offset_to.ok_or_else(|| missing("TZOFFSETTO"))?;
        let rule = self
            .rule
            .map(|rule| YearlyRule::parse(&rule, start, offset_from))
            .transpose()?;
        Ok(Observance {
            start,
            offset_from,
            offset_to,
            rule,
            dates: self.dates,
        })
    }
}

/// A yearly recurrence of an onset, as zones have them: a day of one month each year.
#[derive(Debug, Clone, PartialEq, Eq)]
struct YearlyRule {
    month: i64,
    day: RuleDay,
    /// The last wall-clock second, on the clock of the observance's `offset_from`, an onset may
    /// fall on.
    until: Option<i64>,
    count: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleDay {
    /// A day of the month.
    Fixed(i64),
    /// The `ordinal`th `weekday` (0 is Sunday) of the month, counted from its end when negative.
    Weekday { ordinal: i64, weekday: i64 },
    /// The first `weekday` on or after a day of the month, as `BYMONTHDAY` of seven days in a
    /// row with `BYDAY` of one weekday says.
    WeekdayFrom { day: i64, weekday: i64 },
}

const WEEKDAYS: [&str; 7] = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];

impl YearlyRule {
    /// The rule `RRULE` of an observance whose first onset is `start`, from the offset
    /// `offset_from`, says, or why it is not a yearly rule of one day of one month.
    fn parse(text: &str, start: i64, offset_from: i64) -> Result<Self, String> {
        let unread = |what: &str| format!("the observance's RRULE {text:?} {what}");
        let (_, start_month, start_day) = civil::civil_from_days(start.div_euclid(SECONDS_PER_DAY));
        let mut yearly = false;
        let mut month = start_month;
        let mut weekdays = Vec::new();
        let mut month_days = Vec::new();
        let mut until = None;
        let mut count = None;
        for part in text.trim().split(';') {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| unread("has a part without a value"))?;
            match key.to_ascii_uppercase().as_str() {
                "FREQ" => yearly = value.eq_ignore_ascii_case("YEARLY"),
                "INTERVAL" if value == "1" => {}
                "WKST" => {}
                "BYMONTH" => {
                    month = number(value)
                        .filter(|month| (1..=12).contains(month))
                        .ok_or_else(|| unread("is not of one month"))?;
                }
                "BYDAY" => {
                    for day in value.split(',') {
                        weekdays.push(parse_weekday(day).ok_or_else(|| unread("has a bad BYDAY"))?);
                    }
                }
                "BYMONTHDAY" => {
                    for day in value.split(',') {
                        month_days.push(
                            number(day)
                                .filter(|day| (1..=31).contains(day))
                                .ok_or_else(|| unread("has a bad BYMONTHDAY"))?,
                        );
                    }
                }
                "UNTIL" => {
                    until = Some(if let Some(days) = parse_date(value) {
                        (days + 1) * SECONDS_PER_DAY - 1
                    } else {
                        match parse_date_time(value).ok_or_else(|| unread("has a bad UNTIL"))? {
                            (utc, true) => utc + offset_from,
                            (wall, false) => wall,
                        }
                    });
                }
                "COUNT" => {
                    count = Some(
                        number(value)
                            .filter(|&count| count > 0)
                            .ok_or_else(|| unread("has a bad COUNT"))?,
                    );
                }
                _ => return Err(unread("has a part dijo does not read")),
            }
        }
        if !yearly {
            return Err(unread("is not yearly"));
        }
        let day = match (weekdays.as_slice(), month_days.as_slice()) {
            ([], []) => RuleDay::Fixed(start_day),
            ([], [day]) => RuleDay::Fixed(*day),
            ([(ordinal, weekday)], []) if *ordinal != 0 => RuleDay::Weekday {
                ordinal: *ordinal,
                weekday: *weekday,
            },
            ([(0, weekday)], days)
                if days.len() == 7 && days.windows(2).all(|pair| pair[1] == pair[0] + 1) =>
            {
                RuleDay::WeekdayFrom {
                    day: days[0],
                    weekday: *weekday,
                }
            }
            _ => return Err(unread("is not of one day of the month")),
        };
        Ok(Self {
            month,
            day,
            until,
            count,
        })
    }

    /// The onset in `year` at the time of day of `start`.
    fn onset(&self, year: i64, start: i64) -> i64 {
        let first = civil::days_from_civil(year, self.month, 1);
        let next = match self.month {
            12 => civil::days_from_civil(year + 1, 1, 1),
            month => civil::days_from_civil(year, month + 1, 1),
        };
        let weekday_of = |day: i64| (day + 4).rem_euclid(7);
        let day = match self.day {
            RuleDay::Fixed(day) => first + day - 1,
            RuleDay::Weekday { ordinal, weekday } if ordinal > 0 => {
                first + (weekday - weekday_of(first)).rem_euclid(7) + (ordinal - 1) * 7
            }
            RuleDay::Weekday { ordinal, weekday } => {
                let last = next - 1;
                last - (weekday_of(last) - weekday).rem_euclid(7) + (ordinal + 1) * 7
            }
            RuleDay::WeekdayFrom { day, weekday } => {
                let from = first + day - 1;
                from + (weekday - weekday_of(from)).rem_euclid(7)
            }
        };
        day * SECONDS_PER_DAY + start.rem_euclid(SECONDS_PER_DAY)
    }

    /// The latest onset of `observance` by this rule at or before the wall-clock time `wall`:
    /// one a year from the year of its first onset on, none before that onset, after `until` or
    /// past `count` onsets. With one onset a year, it is in the last year allowed or the one
    /// before.
    fn latest(&self, observance: &Observance, wall: i64) -> Option<i64> {
        let year_of = |time: i64| civil::civil_from_days(time.div_euclid(SECONDS_PER_DAY)).0;
        let first_year = year_of(observance.start);
        let mut last_year = year_of(wall);
        if let Some(count) = self.count {
            let skipped = i64::from(self.onset(first_year, observance.start) < observance.start);
            last_year = last_year.min(first_year + skipped + count - 1);
        }
        let until = self.until;
        if let Some(until) = until {
            last_year = last_year.min(year_of(until));
        }
        (first_year..=last_year)
            .rev()
            .take(2)
            .map(|year| self.onset(year, observance.start))
            .find(|&onset| {
                onset >= observance.start
                    && onset <= wall
                    && until.is_none_or(|until| onset <= until)
            })
    }
}

/// A `BYDAY` value: its ordinal, 0 for none, and its weekday, 0 for Sunday.
fn parse_weekday(text: &str) -> Option<(i64, i64)> {
    let text = text.trim();
    if !text.is_ascii() {
        return None;
    }
    let split = text.len().checked_sub(2)?;
    let weekday = WEEKDAYS
        .iter()
        .position(|day| text[split..].eq_ignore_ascii_case(day))? as i64;
    let ordinal = match &text[..split] {
        "" => 0,
        signed => {
            let (sign, digits) = match signed.strip_prefix('-') {
                Some(digits) => (-1, digits),
                None => (1, signed.strip_prefix('+').unwrap_or(signed)),
            };
            sign * number(digits).filter(|n| (1..=5).contains(n))?
        }
    };
    Some((ordinal, weekday))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(text: &str) -> i64 {
        parse_date_time(text).unwrap().0
    }

    fn observance(lines: &[&str]) -> Observance {
        let mut builder = ObservanceBuilder::default();
        for line in lines {
            builder
                .property(&ContentLine::parse(line).unwrap())
                .unwrap();
        }
        builder.build().unwrap()
    }

    fn europe_berlin() -> Zone {
        Zone::new(vec![
            observance(&[
                "DTSTART:19810329T020000",
                "TZOFFSETFROM:+0100",
                "TZOFFSETTO:+0200",
                "RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU",
            ]),
            observance(&[
                "DTSTART:19961027T030000",
                "TZOFFSETFROM:+0200",
                "TZOFFSETTO:+0100",
                "RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU",
            ]),
        ])
        .unwrap()
    }

    #[test]
    fn resolves_times_by_the_yearly_rules_of_a_vtimezone() {
        let zone = europe_berlin();
        let utc = |text: &str| instant_string(zone.utc(wall(text)), 0);
        assert_eq!(utc("20260115T090000"), "2026-01-15T08:00:00Z");
        assert_eq!(utc("20260715T090000"), "2026-07-15T07:00:00Z");
        assert_eq!(utc("20260329T015959"), "2026-03-29T00:59:59Z");
        assert_eq!(utc("20260329T030000"), "2026-03-29T01:00:00Z");
        assert_eq!(utc("20261025T040000"), "2026-10-25T03:00:00Z");
        assert_eq!(utc("19700101T000000"), "1969-12-31T23:00:00Z");
    }

    #[test]
    fn reads_rules_of_seven_month_days_and_a_weekday_and_their_ends() {
        let zone = Zone::new(vec![
            observance(&[
                "DTSTART:20070311T020000",
                "TZOFFSETFROM:-0500",
                "TZOFFSETTO:-0400",
                "RRULE:FREQ=YEARLY;BYMONTH=3;BYMONTHDAY=8,9,10,11,12,13,14;BYDAY=SU",
            ]),
            observance(&[
                "DTSTART:20071104T020000",
                "TZOFFSETFROM:-0400",
                "TZOFFSETTO:-0500",
                "RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU;UNTIL=20091101T060000Z",
            ]),
        ])
        .unwrap();
        let utc = |text: &str| instant_string(zone.utc(wall(text)), 0);
        assert_eq!(utc("20260310T120000"), "2026-03-10T16:00:00Z");
        assert_eq!(utc("20081201T120000"), "2008-12-01T17:00:00Z");
        assert_eq!(utc("20261201T120000"), "2026-12-01T16:00:00Z");
    }

    #[test]
    fn resolves_zones_of_the_bundled_database_and_leaves_unknown_ones() {
        let mut zones = Zones::default();
        assert_eq!(
            zones.utc("America/New_York", wall("20260710T090000")),
            Ok(Some(wall("20260710T130000")))
        );
        assert_eq!(zones.utc("Lighthouse Standard Time", 0), Ok(None));
    }

    #[test]
    fn reads_dates_date_times_and_offsets_and_rejects_invalid_ones() {
        assert_eq!(parse_date("20240229"), Some(19_782));
        assert_eq!(parse_date("20230229"), None);
        assert_eq!(parse_date_time("20240229T2400000"), None);
        assert_eq!(parse_date_time("20240229T240000"), None);
        assert_eq!(
            parse_date_time("20240229t010203z"),
            Some((wall("20240229T010203"), true))
        );
        assert_eq!(parse_offset("-0330"), Some(-12_600));
        assert_eq!(parse_offset("+013015"), Some(5_415));
        assert_eq!(parse_offset("0100"), None);
    }

    #[test]
    fn writes_moments_as_dates_instants_and_marked_wall_times() {
        assert_eq!(Moment::Date(19_782).to_string(), "2024-02-29");
        assert_eq!(
            Moment::Utc(wall("20240229T010203")).to_string(),
            "2024-02-29T01:02:03Z"
        );
        assert_eq!(
            Moment::Floating(wall("20240229T010203")).to_string(),
            "2024-02-29T01:02:03 (floating)"
        );
        assert_eq!(
            Moment::Unresolved {
                wall: wall("20240229T010203"),
                zone: "Keeper Time".to_owned()
            }
            .to_string(),
            "2024-02-29T01:02:03 (Keeper Time)"
        );
    }

    #[test]
    fn rejects_rules_that_are_not_yearly_on_one_day() {
        let rule = |text: &str| YearlyRule::parse(text, wall("20070311T020000"), 0);
        assert!(rule("FREQ=MONTHLY;BYDAY=1SU").is_err());
        assert!(rule("FREQ=YEARLY;BYMONTH=3;BYDAY=1SU,2SU").is_err());
        assert!(rule("FREQ=YEARLY;BYSETPOS=1").is_err());
        assert!(rule("FREQ=YEARLY;BYMONTH=13").is_err());
        assert_eq!(rule("FREQ=YEARLY;COUNT=2").unwrap().day, RuleDay::Fixed(11));
    }
}
