//! Java's default `TimeZone`, as `java.util.TimeZone.getDefault()` finds it on a Unix system and
//! `sun.util.calendar.ZoneInfo` answers for it: the zone named by `TZ`, `/etc/timezone` or the
//! `/etc/localtime` link, with its rules read from the system's zoneinfo files. Like `ZoneInfo`,
//! it knows no local mean time: before 1900 a zone is at its current standard offset.
#![cfg_attr(
    all(not(unix), not(test)),
    expect(
        dead_code,
        reason = "Java reads the default zone from the registry on Windows, which is not ported, \
                  so only the tests read zoneinfo files there"
    )
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::civil;

/// 1900-01-01T00:00:00Z, where `ZoneInfo`'s transitions begin.
const UTC_1900: i64 = -2_208_988_800;
/// 2100-01-01T00:00:00Z less a second, past which `ZoneInfo` keeps no transitions.
const UTC_2100: i64 = 4_133_980_799;
/// The last year `ZoneInfo` lists transitions for; later ones follow its last rules.
const LAST_YEAR: i64 = 2037;
const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_HOUR: i64 = 3_600;
const ZONEINFO_DIRECTORIES: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/etc/zoneinfo",
    "/usr/lib/zoneinfo",
    "/usr/share/lib/zoneinfo",
];
const LOCALTIME: &str = "/etc/localtime";
const ETC_TIMEZONE: &str = "/etc/timezone";
const GMT: &str = "GMT";

/// A time zone as `ZoneInfo` models it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimeZone {
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

static DEFAULT: LazyLock<Result<TimeZone, String>> = LazyLock::new(TimeZone::system_default);

impl TimeZone {
    /// Java's default time zone, found once as `TimeZone.getDefault()` finds it once per process.
    pub(crate) fn java_default() -> Result<&'static TimeZone, String> {
        DEFAULT.as_ref().map_err(Clone::clone)
    }

    fn fixed(offset: i64) -> Self {
        Self {
            raw_offset: offset,
            transitions: Vec::new(),
            rule: None,
        }
    }

    #[cfg(not(unix))]
    fn system_default() -> Result<Self, String> {
        Err(
            "Java reads its default time zone from the Windows registry, which is not ported"
                .to_owned(),
        )
    }

    #[cfg(unix)]
    fn system_default() -> Result<Self, String> {
        match system_zone_id() {
            ZoneSource::File(path) => Self::from_file(&path)?
                .ok_or_else(|| format!("{} is not a zoneinfo file", path.display())),
            ZoneSource::Id(id) => match Self::by_id(&id, &zoneinfo_directories())? {
                Some(zone) => Ok(zone),
                None => Ok(Self::fixed(system_gmt_offset()?)),
            },
        }
    }

    /// The zone Java knows by `id`, with its rules from the first of `directories` that has
    /// them, or `None` when Java would not know it.
    pub(super) fn by_id(id: &str, directories: &[PathBuf]) -> Result<Option<Self>, String> {
        if let Some(offset) = custom_offset(id) {
            return Ok(Some(Self::fixed(offset)));
        }
        if id.starts_with('/') || id.split('/').any(|part| part.is_empty() || part == "..") {
            return Ok(None);
        }
        for directory in directories {
            if let Some(zone) = Self::from_file(&directory.join(id))? {
                return Ok(Some(zone));
            }
        }
        Ok(None)
    }

    /// The zone of a TZif file, or `None` when there is no such file or it is not one Java has
    /// a zone for: not TZif, or counting leap seconds.
    fn from_file(path: &Path) -> Result<Option<Self>, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::IsADirectory => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "Could not read the time zone {}: {e}",
                    path.display()
                ))
            }
        };
        if !bytes.starts_with(TZIF_MAGIC) {
            return Ok(None);
        }
        let tzif = Tzif::parse(&bytes)
            .map_err(|e| format!("The time zone {} is malformed: {e}", path.display()))?;
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

    /// `ZoneInfo.getTransitionIndex` for a wall time: the transition in effect, `None` before
    /// the first, and the transitions' length past the last.
    fn transition_index(&self, wall: i64) -> Option<usize> {
        let mut low: isize = 0;
        let mut high: isize = self.transitions.len() as isize - 1;
        while low <= high {
            let mid = (low + high) / 2;
            let transition = self.transitions[mid as usize];
            let at = transition.at + transition.offset_after;
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

/// Where the default zone's rules are.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum ZoneSource {
    /// A zone ID: from `TZ` or `/etc/timezone`, or the name `/etc/localtime` links to.
    Id(String),
    /// `/etc/localtime` itself, a copy of a zoneinfo file rather than a link naming one.
    File(PathBuf),
}

/// What the JDK's `getSystemTimeZoneID` finds: `TZ` without a leading `:` (or, on Linux,
/// `posix/`), else on Linux the zone `/etc/timezone` names, else the zone of `/etc/localtime`,
/// else `GMT`.
#[cfg(unix)]
fn system_zone_id() -> ZoneSource {
    if let Some(tz) = std::env::var_os("TZ").filter(|tz| !tz.is_empty()) {
        let tz = tz.to_string_lossy();
        let tz = tz.strip_prefix(':').unwrap_or(&tz);
        let tz = match cfg!(target_os = "linux") {
            true => tz.strip_prefix("posix/").unwrap_or(tz),
            false => tz,
        };
        return ZoneSource::Id(tz.to_owned());
    }
    if cfg!(target_os = "linux") {
        if let Some(id) = fs::read_to_string(ETC_TIMEZONE)
            .ok()
            .and_then(|text| text.lines().next().map(str::to_owned))
            .filter(|line| !line.is_empty())
        {
            return ZoneSource::Id(id);
        }
    }
    let Ok(metadata) = fs::symlink_metadata(LOCALTIME) else {
        return ZoneSource::Id(GMT.to_owned());
    };
    if metadata.file_type().is_symlink() {
        if let Some(id) = fs::read_link(LOCALTIME)
            .ok()
            .and_then(|target| zone_name(&target.to_string_lossy()))
        {
            return ZoneSource::Id(id);
        }
    }
    ZoneSource::File(PathBuf::from(LOCALTIME))
}

/// The part of a zoneinfo path after `zoneinfo/`, as the JDK's `getZoneName` takes it.
fn zone_name(target: &str) -> Option<String> {
    let mut collapsed = String::with_capacity(target.len());
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                collapsed.truncate(collapsed.rfind('/').unwrap_or(0));
            }
            part => {
                collapsed.push('/');
                collapsed.push_str(part);
            }
        }
    }
    collapsed
        .find("zoneinfo/")
        .map(|index| collapsed[index + "zoneinfo/".len()..].to_owned())
}

/// The directories zoneinfo files are looked for in: `TZDIR`, the usual places, and the one
/// `/etc/localtime` links into.
#[cfg(unix)]
fn zoneinfo_directories() -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = std::env::var_os("TZDIR")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    directories.extend(ZONEINFO_DIRECTORIES.iter().map(PathBuf::from));
    if let Ok(target) = fs::canonicalize(LOCALTIME) {
        let target = target.to_string_lossy().into_owned();
        if let Some(index) = target.find("/zoneinfo/") {
            directories.push(PathBuf::from(&target[..index + "/zoneinfo".len()]));
        }
    }
    directories
}

/// The offset of a custom zone ID such as `GMT+1`, `GMT-0530` or `GMT+05:30`, as
/// `TimeZone.parseCustomTimeZone` reads it.
fn custom_offset(id: &str) -> Option<i64> {
    if id == GMT {
        return Some(0);
    }
    let rest = id.strip_prefix(GMT)?;
    if rest.len() < 2 {
        return None;
    }
    let (negative, digits) = match rest.as_bytes()[0] {
        b'-' => (true, &rest[1..]),
        b'+' => (false, &rest[1..]),
        _ => return None,
    };
    let groups: Vec<&str> = digits.split(':').collect();
    if groups.len() > 3
        || groups
            .iter()
            .any(|group| group.is_empty() || !group.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let number = |group: &str| group.parse::<i64>().ok();
    let (hours, minutes, seconds) = match groups.as_slice() {
        [all] if all.len() <= 2 => (number(all)?, 0, 0),
        [all] if all.len() <= 4 => {
            let value = number(all)?;
            (value / 100, value % 100, 0)
        }
        [hours, minutes] if hours.len() <= 2 && minutes.len() == 2 => {
            (number(hours)?, number(minutes)?, 0)
        }
        [hours, minutes, seconds]
            if hours.len() <= 2 && minutes.len() <= 2 && seconds.len() == 2 =>
        {
            (number(hours)?, number(minutes)?, number(seconds)?)
        }
        _ => return None,
    };
    if hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    let offset = hours * SECONDS_PER_HOUR + minutes * 60 + seconds;
    Some(if negative { -offset } else { offset })
}

/// The offset the system's clock is at now, which Java falls back to as a fixed zone when it
/// does not know the zone the system names.
#[cfg(unix)]
fn system_gmt_offset() -> Result<i64, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("The system clock is before 1970: {e}"))?;
    let now = libc::time_t::try_from(now.as_secs())
        .map_err(|e| format!("The system clock does not fit a time_t: {e}"))?;
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    let result = unsafe { libc::localtime_r(&now, local.as_mut_ptr()) };
    if result.is_null() {
        return Err("The system's local time is unknown".to_owned());
    }
    let offset: libc::c_long = unsafe { local.assume_init() }.tm_gmtoff;
    #[allow(
        clippy::useless_conversion,
        reason = "a c_long has 32 bits on some Unix targets"
    )]
    Ok(i64::from(offset))
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
    fn reads_custom_zone_ids_as_java_does() {
        assert_eq!(custom_offset("GMT"), Some(0));
        assert_eq!(custom_offset("GMT+1"), Some(3600));
        assert_eq!(custom_offset("GMT-0530"), Some(-19_800));
        assert_eq!(custom_offset("GMT+05:30"), Some(19_800));
        assert_eq!(custom_offset("GMT+05:30:15"), Some(19_815));
        assert_eq!(custom_offset("GMT+24"), None);
        assert_eq!(custom_offset("GMT+123:00"), None);
        assert_eq!(custom_offset("GMT+"), None);
        assert_eq!(custom_offset("UTC"), None);
    }

    #[test]
    fn names_zones_as_the_jdk_reads_links() {
        assert_eq!(
            zone_name("/etc/zoneinfo/Europe/Copenhagen").as_deref(),
            Some("Europe/Copenhagen")
        );
        assert_eq!(
            zone_name("../usr/share//zoneinfo/./America/New_York").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(zone_name("/etc/localtime-copy"), None);
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
