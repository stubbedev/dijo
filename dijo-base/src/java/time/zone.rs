//! A `java.util.TimeZone` as `sun.util.calendar.ZoneInfo` answers for it: a fixed offset, or a
//! zone of the JDK's time zone database by name, with its rules from the database bundled into
//! the binary. Like `ZoneInfo`, it knows no local mean time: before 1900 a zone is at its
//! current standard offset. No reading depends on the machine's own zone: where Java reads in
//! its default zone, the port reads in UTC.

use super::civil;

/// 1900-01-01T00:00:00Z, where `ZoneInfo`'s transitions begin.
const UTC_1900: i64 = -2_208_988_800;
/// 2100-01-01T00:00:00Z less a second, past which `ZoneInfo` keeps no transitions.
const UTC_2100: i64 = 4_133_980_799;
/// The last year `ZoneInfo` lists transitions for; later ones follow its last rules.
const LAST_YEAR: i64 = 2037;
const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_HOUR: i64 = 3_600;
/// The three-letter IDs `ZoneInfoFile.oldMappings` maps to regions.
const JDK_ALIASES: [(&str, &str); 25] = [
    ("ACT", "Australia/Darwin"),
    ("AET", "Australia/Sydney"),
    ("AGT", "America/Argentina/Buenos_Aires"),
    ("ART", "Africa/Cairo"),
    ("AST", "America/Anchorage"),
    ("BET", "America/Sao_Paulo"),
    ("BST", "Asia/Dhaka"),
    ("CAT", "Africa/Harare"),
    ("CNT", "America/St_Johns"),
    ("CST", "America/Chicago"),
    ("CTT", "Asia/Shanghai"),
    ("EAT", "Africa/Addis_Ababa"),
    ("ECT", "Europe/Paris"),
    ("IET", "America/Indiana/Indianapolis"),
    ("IST", "Asia/Kolkata"),
    ("JST", "Asia/Tokyo"),
    ("MIT", "Pacific/Apia"),
    ("NET", "Asia/Yerevan"),
    ("NST", "Pacific/Auckland"),
    ("PLT", "Asia/Karachi"),
    ("PNT", "America/Phoenix"),
    ("PRT", "America/Puerto_Rico"),
    ("PST", "America/Los_Angeles"),
    ("SST", "Pacific/Guadalcanal"),
    ("VST", "Asia/Ho_Chi_Minh"),
];
/// The `SystemV` zones the JDK keeps at a fixed offset, in hours.
const JDK_FIXED_ZONES: [(&str, i64); 7] = [
    ("SystemV/AST4", -4),
    ("SystemV/EST5", -5),
    ("SystemV/CST6", -6),
    ("SystemV/MST7", -7),
    ("SystemV/PST8", -8),
    ("SystemV/YST9", -9),
    ("SystemV/HST10", -10),
];
/// The `SystemV` zones the JDK keeps with daylight saving rules of its own, which no region has.
const JDK_SYSTEM_V_DAYLIGHT_ZONES: [&str; 6] = [
    "SystemV/AST4ADT",
    "SystemV/EST5EDT",
    "SystemV/CST6CDT",
    "SystemV/MST7MDT",
    "SystemV/PST8PDT",
    "SystemV/YST9YDT",
];
/// The names of the time zone database the JDK leaves out of its own.
const NOT_IN_JDK: [&str; 4] = ["Factory", "GMT+0", "GMT-0", "ROC"];

/// A time zone as `ZoneInfo` models it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZone {
    /// The current standard offset, in seconds east of UTC.
    raw_offset: i64,
    /// From 1900 to 2037: when the wall offset changed, and to what.
    transitions: Vec<Transition>,
    /// The daylight saving rule that holds after the last transition, if any.
    rule: Option<DaylightRule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Transition {
    at: i64,
    offset_after: i64,
}

/// A POSIX `TZ` rule's daylight saving time, as Java's `SimpleTimeZone` applies its last rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DaylightRule {
    standard: i64,
    daylight: i64,
    start: RuleDate,
    start_time: i64,
    end: RuleDate,
    end_time: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleDate {
    /// `Jn`: day 1 to 365, February 29 never counted.
    Julian(i64),
    /// `n`: day 0 to 365, February 29 counted in leap years.
    ZeroBased(i64),
    /// `Mm.w.d`: day `d` (0 is Sunday) of week `w` (5 is the last) of month `m`.
    MonthWeekDay { month: i64, week: i64, weekday: i64 },
}

impl TimeZone {
    /// A zone always `offset` seconds east of UTC, such as UTC itself or the offset a parsed
    /// date names.
    pub fn fixed(offset: i64) -> Self {
        Self {
            raw_offset: offset,
            transitions: Vec::new(),
            rule: None,
        }
    }

    /// The zone of the JDK's time zone database named `id`, as `TimeZone.getTimeZone` finds it
    /// by a region ID or an alias, with the rules of the database bundled into the binary, or
    /// `None` for a name the JDK does not know, for which it answers GMT. Custom `GMT` offsets
    /// are not names and are not looked up here.
    pub fn jdk_named(id: &str) -> Result<Option<Self>, String> {
        if let Some((_, hours)) = JDK_FIXED_ZONES.iter().find(|(name, _)| *name == id) {
            return Ok(Some(Self::fixed(hours * SECONDS_PER_HOUR)));
        }
        if JDK_SYSTEM_V_DAYLIGHT_ZONES.contains(&id) {
            return Err(format!(
                "the JDK's zone {id}, whose daylight saving rules the bundled database lacks"
            ));
        }
        if NOT_IN_JDK.contains(&id) {
            return Ok(None);
        }
        let region = JDK_ALIASES
            .iter()
            .find(|(alias, _)| *alias == id)
            .map_or(id, |(_, region)| *region);
        match jiff_tzdb::get(region) {
            Some((name, tzif)) if name == region => Self::from_tzif_bytes(tzif, region),
            _ => Ok(None),
        }
    }

    /// The zone the TZif data `bytes`, which `what` names in errors, describes, or `None` when
    /// they are not TZif data Java reads: not TZif at all, or with leap seconds.
    pub(super) fn from_tzif_bytes(bytes: &[u8], what: &str) -> Result<Option<Self>, String> {
        if !bytes.starts_with(TZIF_MAGIC) {
            return Ok(None);
        }
        let tzif =
            Tzif::parse(bytes).map_err(|e| format!("The time zone {what} is malformed: {e}"))?;
        if tzif.leap_seconds {
            return Ok(None);
        }
        Ok(Some(Self::from_tzif(&tzif)))
    }

    /// The zone `ZoneInfoFile` builds from the same data: transitions from 1900, with the offset
    /// in effect then as the first, continued by the last rule up to 2037.
    fn from_tzif(tzif: &Tzif) -> Self {
        let rule = tzif.footer.and_then(|footer| footer.daylight);
        let raw_offset = match (tzif.footer, rule) {
            (_, Some(rule)) => rule.standard.min(rule.daylight),
            (Some(footer), None) => footer.standard,
            (None, None) => tzif.last_standard_offset(),
        };
        let first = tzif
            .transitions
            .iter()
            .position(|transition| transition.at >= UTC_1900);
        let mut transitions = Vec::new();
        if let Some(first) = first {
            let at_1900 = match first {
                0 => tzif.initial_offset,
                _ => tzif.transitions[first - 1].offset_after,
            };
            transitions.push(Transition {
                at: UTC_1900,
                offset_after: at_1900,
            });
            transitions.extend(
                tzif.transitions[first..]
                    .iter()
                    .take_while(|transition| transition.at <= UTC_2100),
            );
            if let Some(rule) = rule {
                let last = tzif.transitions.len() - 1;
                let offset_before = match last {
                    0 => tzif.initial_offset,
                    _ => tzif.transitions[last - 1].offset_after,
                };
                let wall = tzif.transitions[last].at + offset_before;
                let last_year = civil::year_of_day(wall.div_euclid(SECONDS_PER_DAY));
                let last_at = tzif.transitions[last].at;
                for year in last_year..=LAST_YEAR {
                    transitions.extend(
                        rule.transitions(year)
                            .into_iter()
                            .filter(|transition| transition.at > last_at),
                    );
                }
            }
        }
        Self {
            raw_offset,
            transitions,
            rule,
        }
    }

    /// `getRawOffset`: the current standard offset, in milliseconds.
    pub fn raw_offset(&self) -> i64 {
        self.raw_offset * 1000
    }

    /// The offset, in milliseconds, of the wall time `wall` (milliseconds since the epoch as if
    /// it were UTC), as `ZoneInfo.getOffsetsByWall` resolves it: in a gap, the offset before the
    /// change; in an overlap, the offset after it.
    pub(crate) fn offset_by_wall(&self, wall: i64) -> i64 {
        let wall = wall.div_euclid(1000);
        let seconds = if self.transitions.is_empty() {
            self.raw_offset
        } else {
            match self.transition_index(wall) {
                None => self.raw_offset,
                Some(index) if index < self.transitions.len() => {
                    self.transitions[index].offset_after
                }
                Some(_) => match self.rule {
                    Some(rule) => rule.offset_by_wall(wall, self.raw_offset),
                    None => self.transitions[self.transitions.len() - 1].offset_after,
                },
            }
        };
        seconds * 1000
    }

    /// The offset, in milliseconds, at the instant `utc` (milliseconds since the epoch), as
    /// `ZoneInfo.getOffsets` gives it for UTC time.
    pub fn offset_at_utc(&self, utc: i64) -> i64 {
        let utc = utc.div_euclid(1000);
        let seconds = if self.transitions.is_empty() {
            self.raw_offset
        } else {
            match self.transition_index_by(utc, |transition| transition.at) {
                None => self.raw_offset,
                Some(index) if index < self.transitions.len() => {
                    self.transitions[index].offset_after
                }
                Some(_) => match self.rule {
                    Some(rule) => rule.offset_at(utc),
                    None => self.transitions[self.transitions.len() - 1].offset_after,
                },
            }
        };
        seconds * 1000
    }

    /// `ZoneInfo.getTransitionIndex` for a wall time: the transition in effect, `None` before
    /// the first, and the transitions' length past the last.
    fn transition_index(&self, wall: i64) -> Option<usize> {
        self.transition_index_by(wall, |transition| transition.at + transition.offset_after)
    }

    fn transition_index_by(&self, time: i64, key: impl Fn(&Transition) -> i64) -> Option<usize> {
        let wall = time;
        let mut low: isize = 0;
        let mut high: isize = self.transitions.len() as isize - 1;
        while low <= high {
            let mid = (low + high) / 2;
            let transition = self.transitions[mid as usize];
            let at = key(&transition);
            if at < wall {
                low = mid + 1;
            } else if at > wall {
                high = mid - 1;
            } else {
                return Some(mid as usize);
            }
        }
        if low as usize >= self.transitions.len() {
            return Some(self.transitions.len());
        }
        usize::try_from(low - 1).ok()
    }
}

impl DaylightRule {
    /// The rule's two transitions in `year`, in the order they happen.
    fn transitions(&self, year: i64) -> [Transition; 2] {
        let start = Transition {
            at: self.start.day(year) * SECONDS_PER_DAY + self.start_time - self.standard,
            offset_after: self.daylight,
        };
        let end = Transition {
            at: self.end.day(year) * SECONDS_PER_DAY + self.end_time - self.daylight,
            offset_after: self.standard,
        };
        if start.at <= end.at {
            [start, end]
        } else {
            [end, start]
        }
    }

    /// The offset at the instant `utc`.
    fn offset_at(&self, utc: i64) -> i64 {
        let year = civil::year_of_day(utc.div_euclid(SECONDS_PER_DAY));
        let [first, second] = self.transitions(year);
        if utc < first.at {
            second.offset_after
        } else if utc < second.at {
            first.offset_after
        } else {
            second.offset_after
        }
    }

    /// `ZoneInfo`'s answer past its transitions, from its `SimpleTimeZone`: the wall time read
    /// as standard time, and standard time in a change to daylight saving time.
    fn offset_by_wall(&self, wall: i64, raw_offset: i64) -> i64 {
        let instant = wall - raw_offset;
        let daylight = self.offset_at(instant) - raw_offset;
        if daylight > 0 && self.offset_at(instant - daylight) == raw_offset {
            raw_offset
        } else {
            raw_offset + daylight
        }
    }
}

impl RuleDate {
    /// The day, counted from the epoch, this date falls on in `year`.
    fn day(self, year: i64) -> i64 {
        match self {
            RuleDate::Julian(day) => {
                let leap_day = i64::from(civil::is_leap_year(year) && day >= 60);
                civil::days_from_civil(year, 1, 1) + day - 1 + leap_day
            }
            RuleDate::ZeroBased(day) => civil::days_from_civil(year, 1, 1) + day,
            RuleDate::MonthWeekDay {
                month,
                week,
                weekday,
            } => {
                let first = civil::days_from_civil(year, month, 1);
                let first_weekday = (first + 4).rem_euclid(7);
                let mut day = first + (weekday - first_weekday).rem_euclid(7) + (week - 1) * 7;
                let next_month = match month {
                    12 => civil::days_from_civil(year + 1, 1, 1),
                    _ => civil::days_from_civil(year, month + 1, 1),
                };
                while day >= next_month {
                    day -= 7;
                }
                day
            }
        }
    }
}

const TZIF_MAGIC: &[u8] = b"TZif";

/// The parts of a TZif file (RFC 8536) a Java zone is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tzif {
    initial_offset: i64,
    transitions: Vec<Transition>,
    standard_after: Vec<bool>,
    footer: Option<Footer>,
    leap_seconds: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Footer {
    standard: i64,
    daylight: Option<DaylightRule>,
}

struct Counts {
    is_ut: usize,
    is_std: usize,
    leap: usize,
    time: usize,
    types: usize,
    chars: usize,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let slice = self.bytes.get(self.at..self.at + length).ok_or_else(|| {
            format!(
                "it ends at byte {} of {} needed",
                self.bytes.len(),
                self.at + length
            )
        })?;
        self.at += length;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<usize, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
    }

    fn header(&mut self) -> Result<(u8, Counts), String> {
        if self.take(4)? != TZIF_MAGIC {
            return Err("a header lacks the TZif magic".to_owned());
        }
        let version = self.take(16)?[0];
        Ok((
            version,
            Counts {
                is_ut: self.u32()?,
                is_std: self.u32()?,
                leap: self.u32()?,
                time: self.u32()?,
                types: self.u32()?,
                chars: self.u32()?,
            },
        ))
    }
}

impl Counts {
    fn data_length(&self, time_size: usize) -> usize {
        self.time * time_size
            + self.time
            + self.types * 6
            + self.chars
            + self.leap * (time_size + 4)
            + self.is_std
            + self.is_ut
    }
}

impl Tzif {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut cursor = Cursor { bytes, at: 0 };
        let (version, v1) = cursor.header()?;
        if version < b'2' {
            return Err("version 1 files are not supported".to_owned());
        }
        cursor.take(v1.data_length(4))?;
        let (_, counts) = cursor.header()?;
        let times = cursor.take(counts.time * 8)?;
        let indices = cursor.take(counts.time)?;
        let types: Vec<(i64, bool)> = cursor
            .take(counts.types * 6)?
            .as_chunks::<6>()
            .0
            .iter()
            .map(|record| {
                let offset = i32::from_be_bytes([record[0], record[1], record[2], record[3]]);
                (i64::from(offset), record[4] != 0)
            })
            .collect();
        if types.is_empty() {
            return Err("it has no local time types".to_owned());
        }
        cursor.take(counts.chars + counts.leap * 12 + counts.is_std + counts.is_ut)?;
        let footer_text = match cursor.take(1)? {
            b"\n" => {
                let rest = &bytes[cursor.at..];
                let end = rest
                    .iter()
                    .position(|&b| b == b'\n')
                    .ok_or("its footer is not ended by a newline")?;
                std::str::from_utf8(&rest[..end])
                    .map_err(|e| format!("its footer is not text: {e}"))?
            }
            _ => return Err("its footer does not start with a newline".to_owned()),
        };
        let footer = match footer_text {
            "" => None,
            text => Some(Footer::parse(text).map_err(|e| format!("its footer {text:?} {e}"))?),
        };
        let mut transitions = Vec::with_capacity(counts.time);
        let mut standard_after = Vec::with_capacity(counts.time);
        for (time, &index) in times.as_chunks::<8>().0.iter().zip(indices) {
            let at = i64::from_be_bytes(*time);
            let &(offset_after, daylight) = types
                .get(usize::from(index))
                .ok_or_else(|| format!("a transition names local time type {index}"))?;
            transitions.push(Transition { at, offset_after });
            standard_after.push(!daylight);
        }
        Ok(Self {
            initial_offset: types[0].0,
            transitions,
            standard_after,
            footer,
            leap_seconds: counts.leap > 0,
        })
    }

    fn last_standard_offset(&self) -> i64 {
        self.transitions
            .iter()
            .zip(&self.standard_after)
            .rev()
            .find(|(_, &standard)| standard)
            .map_or(self.initial_offset, |(transition, _)| {
                transition.offset_after
            })
    }
}

impl Footer {
    /// Reads a POSIX `TZ` string: `std offset [dst [offset] [,start[/time],end[/time]]]`.
    fn parse(text: &str) -> Result<Self, String> {
        let mut rest = text;
        skip_name(&mut rest)?;
        let standard = -parse_offset(&mut rest)?;
        if rest.is_empty() {
            return Ok(Self {
                standard,
                daylight: None,
            });
        }
        skip_name(&mut rest)?;
        let daylight = if rest.is_empty() || rest.starts_with(',') {
            standard + SECONDS_PER_HOUR
        } else {
            -parse_offset(&mut rest)?
        };
        let rules = match rest {
            "" => "M3.2.0,M11.1.0",
            rest => rest
                .strip_prefix(',')
                .ok_or_else(|| format!("has {rest:?} where rules should start"))?,
        };
        let (start, end) = rules
            .split_once(',')
            .ok_or("has a daylight saving time without an end")?;
        let (start, start_time) = parse_rule(start)?;
        let (end, end_time) = parse_rule(end)?;
        Ok(Self {
            standard,
            daylight: Some(DaylightRule {
                standard,
                daylight,
                start,
                start_time,
                end,
                end_time,
            }),
        })
    }
}

fn skip_name(rest: &mut &str) -> Result<(), String> {
    let length = if let Some(quoted) = rest.strip_prefix('<') {
        quoted.find('>').ok_or("has an unterminated <name>")? + 2
    } else {
        rest.find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len())
    };
    if length < 3 {
        return Err("has a zone name shorter than three letters".to_owned());
    }
    *rest = &rest[length..];
    Ok(())
}

/// Reads `[+-]hh[:mm[:ss]]` into seconds.
fn parse_offset(rest: &mut &str) -> Result<i64, String> {
    let negative = rest.starts_with('-');
    let digits = rest.strip_prefix(['+', '-']).unwrap_or(rest);
    let length = digits
        .find(|c: char| !c.is_ascii_digit() && c != ':')
        .unwrap_or(digits.len());
    let mut seconds = 0;
    let mut scale = SECONDS_PER_HOUR;
    for part in digits[..length].split(':') {
        let value: i64 = part
            .parse()
            .map_err(|e| format!("has a bad time {part:?}: {e}"))?;
        seconds += value * scale;
        scale /= 60;
    }
    *rest = &digits[length..];
    Ok(if negative { -seconds } else { seconds })
}

fn parse_rule(text: &str) -> Result<(RuleDate, i64), String> {
    let (date, time) = match text.split_once('/') {
        Some((date, time)) => {
            let mut time = time;
            (date, parse_offset(&mut time)?)
        }
        None => (text, 2 * SECONDS_PER_HOUR),
    };
    let number = |text: &str| -> Result<i64, String> {
        text.parse()
            .map_err(|e| format!("has a bad rule date {date:?}: {e}"))
    };
    let date = if let Some(julian) = date.strip_prefix('J') {
        RuleDate::Julian(number(julian)?)
    } else if let Some(fields) = date.strip_prefix('M') {
        let fields: Vec<&str> = fields.split('.').collect();
        let [month, week, weekday] = fields.as_slice() else {
            return Err(format!("has a bad rule date {date:?}"));
        };
        RuleDate::MonthWeekDay {
            month: number(month)?,
            week: number(week)?,
            weekday: number(weekday)?,
        }
    } else {
        RuleDate::ZeroBased(number(date)?)
    };
    Ok((date, time))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_rules_of_a_zoneinfo_file() {
        let zone = super::super::tests::zone("Europe/Copenhagen");
        assert_eq!(zone.raw_offset, 3600);
        assert_eq!(
            zone.transitions[0],
            Transition {
                at: UTC_1900,
                offset_after: 3600
            }
        );
        assert!(zone.rule.is_some());
    }

    #[test]
    fn reads_posix_rules() {
        let footer = Footer::parse("<+0330>-3:30").unwrap();
        assert_eq!(footer.standard, 12_600);
        assert!(footer.daylight.is_none());
        let footer = Footer::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        let rule = footer.daylight.unwrap();
        assert_eq!((rule.standard, rule.daylight), (-18_000, -14_400));
        let [start, end] = rule.transitions(2030);
        assert_eq!(
            start.at,
            civil::days_from_civil(2030, 3, 10) * SECONDS_PER_DAY + 7 * 3600
        );
        assert_eq!(
            end.at,
            civil::days_from_civil(2030, 11, 3) * SECONDS_PER_DAY + 6 * 3600
        );
        assert!(Footer::parse("X1").is_err());
    }
}
