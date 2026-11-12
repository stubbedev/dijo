//! iCalendar files (RFC 5545), read through an [`AutoDetectReader`] as Tika reads them as text,
//! their content lines unfolded and their values unescaped. Each `VEVENT`, `VTODO` and `VJOURNAL`
//! is a section, `<div class="vevent">` and the like: its summary as a heading, then its start,
//! end or due time, duration, recurrence rule (as written), location, categories, organiser and
//! attendees, and its descriptions a paragraph a line. Times in UTC stay in UTC; times in a zone
//! are resolved to UTC by the calendar's own `VTIMEZONE` of their `TZID`, else by the bundled
//! time zone database, else written as they are with the zone's name; floating times are written
//! as they are and marked floating; dates stay dates. The calendar's name (`X-WR-CALNAME`) is its
//! `dc:title`, its description (`X-WR-CALDESC`) its `dc:description`, and its product
//! (`PRODID`) its `xmp:CreatorTool`.
//!
//! A `VTIMEZONE` may follow the components that use it, so the zones are read in a pass of their
//! own before the components are written as they end, line by line in bounded memory.

mod moment;

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::content_line::{unfold, ContentLine};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{unless_different, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use moment::{Moment, ObservanceBuilder, Zone, Zones};

/// How deep components may nest: RFC 5545 nests them three deep, as an observance in a
/// `VTIMEZONE` in a `VCALENDAR`.
const MAX_DEPTH: usize = 16;

pub struct ICalendarParser;

impl Parser for ICalendarParser {
    fn name(&self) -> &'static str {
        "dijo::ICalendarParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::ICalendarParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["text/calendar"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        unless_different(
            Box::new(move |xhtml| write(input, pages, xhtml)),
            AutoDetectReader::difference(input, metadata.get("Content-Type")),
        )
    }
}

/// Writes the calendar `input`, decoded as Tika's `AutoDetectReader` decodes it: its zones read
/// first, then its components written, the `pages` of `input` released as they are read.
fn write<'a>(input: &'a [u8], pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let content_type = xhtml
        .metadata_mut()
        .get("Content-Type")
        .ok_or_else(|| Failure::Malformed("the document has no Content-Type".into()))?
        .to_owned();
    let mut zones = Zones::default();
    let (_, text) = AutoDetectReader::stream(input, Some(&content_type), pages);
    walk(text.lines(), &mut Pass::Zones(&mut zones))?;
    zones.rewind();
    let (detection, text) = AutoDetectReader::stream(input, Some(&content_type), pages);
    let metadata = xhtml.metadata_mut();
    auto_detect_reader::set_metadata(&detection, metadata);
    metadata.set("Content-Encoding", detection.decode_as.name());
    walk(
        text.lines(),
        &mut Pass::Write {
            zones: &mut zones,
            xhtml,
        },
    )
}

/// What a reading of the calendar does: read its zones, or write its components with the zones
/// read before.
enum Pass<'p, 'h> {
    Zones(&'p mut Zones),
    Write {
        zones: &'p mut Zones,
        xhtml: &'p mut Xhtml<'h>,
    },
}

impl Pass<'_, '_> {
    fn zones(&mut self) -> &mut Zones {
        match self {
            Pass::Zones(zones) | Pass::Write { zones, .. } => zones,
        }
    }
}

/// A component begun and not yet ended.
struct Open {
    name: String,
    line: usize,
    frame: Frame,
}

/// What is gathered of an open component.
enum Frame {
    Calendar,
    Entry(Box<Entry>),
    Zone {
        tzid: Option<String>,
        observances: Vec<moment::Observance>,
    },
    Observance(ObservanceBuilder),
    /// A component the pass reads nothing of, such as an alarm.
    Skipped,
}

/// Reads the content lines of `lines`, the physical lines of a calendar, component by component.
fn walk(lines: impl Iterator<Item = String>, pass: &mut Pass) -> Result<(), Failure> {
    let mut open: Vec<Open> = Vec::new();
    for logical in unfold(lines) {
        let logical = logical?;
        let at =
            |message: String| Failure::Malformed(format!("line {}: {message}", logical.number));
        let line = ContentLine::parse(&logical.text).map_err(at)?;
        if line.is("BEGIN") {
            if open.len() == MAX_DEPTH {
                return Err(at(format!(
                    "components nest deeper than {MAX_DEPTH} at {}",
                    line.raw
                )));
            }
            let frame =
                begin(open.last().map(|parent| &parent.frame), line.raw, pass).map_err(at)?;
            if matches!(frame, Frame::Calendar) {
                pass.zones().next_calendar();
            }
            open.push(Open {
                name: line.raw.to_owned(),
                line: logical.number,
                frame,
            });
        } else if line.is("END") {
            let ended = open
                .pop()
                .ok_or_else(|| at(format!("END:{} ends no component", line.raw)))?;
            if !ended.name.eq_ignore_ascii_case(line.raw) {
                return Err(at(format!(
                    "END:{} ends the {} begun at line {}",
                    line.raw, ended.name, ended.line
                )));
            }
            end(ended, open.last_mut(), pass).map_err(|failure| match failure {
                Failure::Malformed(message) => at(message),
                other => other,
            })?;
        } else {
            let Some(current) = open.last_mut() else {
                return Err(at(format!(
                    "the {} line is outside any component",
                    line.name
                )));
            };
            property(&mut current.frame, &line, pass).map_err(at)?;
        }
    }
    match open.last() {
        Some(unended) => Err(Failure::Malformed(format!(
            "the calendar ends inside the {} begun at line {}",
            unended.name, unended.line
        ))),
        None => Ok(()),
    }
}

/// The frame of the component `name` begun in the component of `parent`, if any.
fn begin(parent: Option<&Frame>, name: &str, pass: &Pass) -> Result<Frame, String> {
    let writing = matches!(pass, Pass::Write { .. });
    let Some(parent) = parent else {
        return match name.eq_ignore_ascii_case("VCALENDAR") {
            true => Ok(Frame::Calendar),
            false => Err(format!("the {name} is outside a VCALENDAR")),
        };
    };
    Ok(match (parent, EntryKind::named(name)) {
        (Frame::Calendar, Some(kind)) if writing => Frame::Entry(Box::new(Entry::new(kind))),
        (Frame::Calendar, None) if !writing && name.eq_ignore_ascii_case("VTIMEZONE") => {
            Frame::Zone {
                tzid: None,
                observances: Vec::new(),
            }
        }
        (Frame::Zone { .. }, None)
            if name.eq_ignore_ascii_case("STANDARD") || name.eq_ignore_ascii_case("DAYLIGHT") =>
        {
            Frame::Observance(ObservanceBuilder::default())
        }
        _ => Frame::Skipped,
    })
}

/// Takes in the property `line` of the component of `frame`.
fn property(frame: &mut Frame, line: &ContentLine, pass: &mut Pass) -> Result<(), String> {
    match (frame, pass) {
        (Frame::Calendar, Pass::Write { xhtml, .. }) => {
            calendar_property(line, xhtml.metadata_mut())
        }
        (Frame::Entry(entry), Pass::Write { zones, .. }) => entry.property(line, zones),
        (Frame::Zone { tzid, .. }, Pass::Zones(_)) if line.is("TZID") => {
            *tzid = Some(line.text()?.into_owned());
            Ok(())
        }
        (Frame::Observance(observance), Pass::Zones(_)) => observance.property(line),
        _ => Ok(()),
    }
}

/// Sets the metadata the calendar's own property `line` gives.
fn calendar_property(line: &ContentLine, metadata: &mut TikaMetadata) -> Result<(), String> {
    let key = if line.is("X-WR-CALNAME") {
        "dc:title"
    } else if line.is("X-WR-CALDESC") {
        "dc:description"
    } else if line.is("PRODID") {
        "xmp:CreatorTool"
    } else {
        return Ok(());
    };
    metadata.set(key, line.text()?.into_owned());
    Ok(())
}

/// Ends the component `ended`, within the component `parent` if any: writes an entry, defines a
/// zone, or adds an observance to its zone.
fn end(ended: Open, parent: Option<&mut Open>, pass: &mut Pass) -> Result<(), Failure> {
    let in_component =
        |message: String| Failure::Malformed(format!("the {}: {message}", ended.name));
    match (ended.frame, pass) {
        (Frame::Entry(entry), Pass::Write { xhtml, .. }) => Ok(entry.write(xhtml)?),
        (Frame::Zone { tzid, observances }, Pass::Zones(zones)) => {
            let tzid = tzid.ok_or_else(|| in_component("it has no TZID".to_owned()))?;
            let zone = Zone::new(observances)
                .map_err(|e| in_component(format!("the zone {tzid:?}: {e}")))?;
            zones.define(tzid, zone).map_err(in_component)
        }
        (Frame::Observance(builder), Pass::Zones(_)) => {
            let observance = builder.build().map_err(in_component)?;
            match parent.map(|parent| &mut parent.frame) {
                Some(Frame::Zone { observances, .. }) => {
                    observances.push(observance);
                    Ok(())
                }
                _ => unreachable!("an observance is only begun in a zone"),
            }
        }
        _ => Ok(()),
    }
}

/// The components written as sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Event,
    Todo,
    Journal,
}

impl EntryKind {
    fn named(name: &str) -> Option<Self> {
        [Self::Event, Self::Todo, Self::Journal]
            .into_iter()
            .find(|kind| name.eq_ignore_ascii_case(kind.name()))
    }

    fn name(self) -> &'static str {
        match self {
            Self::Event => "VEVENT",
            Self::Todo => "VTODO",
            Self::Journal => "VJOURNAL",
        }
    }

    /// The class of the section, as hCalendar names the component.
    fn class(self) -> &'static str {
        match self {
            Self::Event => "vevent",
            Self::Todo => "vtodo",
            Self::Journal => "vjournal",
        }
    }
}

/// Someone an entry names: an organiser or an attendee.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Person {
    name: Option<String>,
    address: String,
}

impl Person {
    /// The person of an `ORGANIZER` or `ATTENDEE` line: its `CN`, and its address, a `mailto:`
    /// URI as the email address it names.
    fn of(line: &ContentLine) -> Self {
        let uri = line.raw.trim();
        let address = match uri.get(..7) {
            Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => &uri[7..],
            _ => uri,
        };
        Self {
            name: line.parameter("CN").map(str::to_owned),
            address: address.to_owned(),
        }
    }
}

impl std::fmt::Display for Person {
    /// `Name <address>`, as mail writes a mailbox, or the address alone.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.name, self.address.is_empty()) {
            (Some(name), true) => f.write_str(name),
            (Some(name), false) => write!(f, "{name} <{}>", self.address),
            (None, _) => f.write_str(&self.address),
        }
    }
}

/// What is written of an event, a to-do or a journal entry.
struct Entry {
    kind: EntryKind,
    summary: Option<String>,
    start: Option<Moment>,
    end: Option<Moment>,
    due: Option<Moment>,
    duration: Option<String>,
    rule: Option<String>,
    location: Option<String>,
    categories: Vec<String>,
    organiser: Option<Person>,
    attendees: Vec<Person>,
    descriptions: Vec<String>,
}

impl Entry {
    fn new(kind: EntryKind) -> Self {
        Self {
            kind,
            summary: None,
            start: None,
            end: None,
            due: None,
            duration: None,
            rule: None,
            location: None,
            categories: Vec::new(),
            organiser: None,
            attendees: Vec::new(),
            descriptions: Vec::new(),
        }
    }

    /// Takes in a property of the entry, its times resolved by `zones`.
    fn property(&mut self, line: &ContentLine, zones: &mut Zones) -> Result<(), String> {
        let text = || line.text().map(|text| text.into_owned());
        if line.is("SUMMARY") {
            self.summary = Some(text()?);
        } else if line.is("DESCRIPTION") {
            self.descriptions.push(text()?);
        } else if line.is("LOCATION") {
            self.location = Some(text()?);
        } else if line.is("CATEGORIES") {
            self.categories.extend(line.texts(',')?);
        } else if line.is("DTSTART") {
            self.start = Some(Moment::of(line, zones)?);
        } else if line.is("DTEND") {
            self.end = Some(Moment::of(line, zones)?);
        } else if line.is("DUE") {
            self.due = Some(Moment::of(line, zones)?);
        } else if line.is("DURATION") {
            self.duration = Some(line.raw.trim().to_owned());
        } else if line.is("RRULE") {
            self.rule = Some(line.raw.trim().to_owned());
        } else if line.is("ORGANIZER") {
            self.organiser = Some(Person::of(line));
        } else if line.is("ATTENDEE") {
            self.attendees.push(Person::of(line));
        }
        Ok(())
    }

    /// Writes the entry as a section.
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        xhtml.start_element_with("div", &[("class", self.kind.class())])?;
        if let Some(summary) = &self.summary {
            xhtml.element("h2", summary)?;
        }
        let categories = self.categories.join(", ");
        let fields: [(&str, Option<&dyn std::fmt::Display>); 8] = [
            ("Starts", self.start.as_ref().map(|value| value as _)),
            ("Ends", self.end.as_ref().map(|value| value as _)),
            ("Due", self.due.as_ref().map(|value| value as _)),
            ("Duration", self.duration.as_ref().map(|value| value as _)),
            ("Repeats", self.rule.as_ref().map(|value| value as _)),
            ("Location", self.location.as_ref().map(|value| value as _)),
            (
                "Categories",
                (!categories.is_empty()).then_some(&categories as _),
            ),
            ("Organiser", self.organiser.as_ref().map(|value| value as _)),
        ];
        for (label, value) in fields {
            if let Some(value) = value {
                xhtml.element("p", &format!("{label}: {value}"))?;
            }
        }
        if !self.attendees.is_empty() {
            xhtml.element("p", "Attendees:")?;
            xhtml.start_element("ul")?;
            for attendee in &self.attendees {
                xhtml.element("li", &attendee.to_string())?;
            }
            xhtml.end_element("ul")?;
        }
        for description in &self.descriptions {
            for paragraph in description.lines() {
                xhtml.element("p", paragraph.trim())?;
            }
        }
        xhtml.end_element("div")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text and metadata the native path extracts from `calendar`.
    fn read(calendar: &str) -> crate::ExtractResult<(String, crate::Metadata)> {
        crate::Extractor::new().extract_bytes_to_string(calendar.as_bytes())
    }

    /// Why reading the zones of `calendar` fails.
    fn zones_failure(calendar: &str) -> String {
        let mut zones = Zones::default();
        match walk(
            calendar.lines().map(str::to_owned),
            &mut Pass::Zones(&mut zones),
        ) {
            Err(Failure::Malformed(message)) => message,
            other => panic!("{other:?}"),
        }
    }

    const CALENDAR: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Lighthouse//Keeper 1.0//EN\r\n\
X-WR-CALNAME:Lighthouse duties\r\n\
BEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Lamp \r\n check\r\nDTSTART;TZID=Europe/Oslo:20260710T200000\r\n\
DTEND;TZID=Keeper:20260710T210000\r\nRRULE:FREQ=WEEKLY;BYDAY=FR\r\n\
ORGANIZER;CN=\"Keeper, The\":mailto:keeper@example.org\r\n\
ATTENDEE;CN=Ada;ROLE=REQ-PARTICIPANT:MAILTO:ada@example.org\r\n\
ATTENDEE:mailto:bo@example.org\r\n\
DESCRIPTION:Trim the wick\\, fill the oil.\\nLight at dusk\\; log it.\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\n\
BEGIN:VTODO\r\nSUMMARY:Order oil\r\nDUE;VALUE=DATE:20260801\r\nDTSTART:20260720T090000\r\nEND:VTODO\r\n\
BEGIN:VTIMEZONE\r\nTZID:Keeper\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\n\
TZOFFSETFROM:+0300\r\nTZOFFSETTO:+0300\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n";

    #[test]
    fn writes_each_entry_as_a_section_with_its_times_in_utc() {
        let (text, metadata) = read(CALENDAR).unwrap();
        assert_eq!(
            text.trim(),
            "Lamp check\n\n\
             Starts: 2026-07-10T18:00:00Z\n\n\
             Ends: 2026-07-10T18:00:00Z\n\n\
             Repeats: FREQ=WEEKLY;BYDAY=FR\n\n\
             Organiser: Keeper, The <keeper@example.org>\n\n\
             Attendees:\n\n\
             \tAda <ada@example.org>\n\n\
             \tbo@example.org\n\n\n\
             Trim the wick, fill the oil.\n\n\
             Light at dusk; log it.\n\n\n\
             Order oil\n\n\
             Starts: 2026-07-20T09:00:00 (floating)\n\n\
             Due: 2026-08-01"
        );
        assert_eq!(metadata["Content-Type"], ["text/calendar"]);
        assert_eq!(metadata["dc:title"], ["Lighthouse duties"]);
        assert_eq!(
            metadata["xmp:CreatorTool"],
            ["-//Lighthouse//Keeper 1.0//EN"]
        );
    }

    #[test]
    fn resolves_a_tzid_by_the_vtimezone_of_its_own_calendar() {
        let calendar = |offset: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Keeper\r\n\
                 BEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETFROM:{offset}\r\n\
                 TZOFFSETTO:{offset}\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\n\
                 DTSTART;TZID=Keeper:20260710T120000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };
        let (text, _) = read(&(calendar("+0200") + &calendar("-0300"))).unwrap();
        assert_eq!(
            text.trim(),
            "Starts: 2026-07-10T10:00:00Z\n\n\nStarts: 2026-07-10T15:00:00Z"
        );
    }

    #[test]
    fn fails_with_the_line_of_a_malformed_structure() {
        assert_eq!(
            zones_failure("BEGIN:VCALENDAR\nBEGIN:VEVENT\nEND:VTODO\n"),
            "line 3: END:VTODO ends the VEVENT begun at line 2"
        );
        assert_eq!(
            zones_failure("BEGIN:VCALENDAR\nEND:VCALENDAR\nEND:VCALENDAR\n"),
            "line 3: END:VCALENDAR ends no component"
        );
        assert_eq!(
            zones_failure("BEGIN:VCALENDAR\nBEGIN:VEVENT\nSUMMARY:x\n"),
            "the calendar ends inside the VEVENT begun at line 2"
        );
        assert_eq!(
            zones_failure("SUMMARY:x\n"),
            "line 1: the SUMMARY line is outside any component"
        );
        let deep = "BEGIN:VCALENDAR\n".to_owned() + &"BEGIN:X\n".repeat(MAX_DEPTH);
        assert_eq!(
            zones_failure(&deep),
            format!(
                "line {}: components nest deeper than {MAX_DEPTH} at X",
                MAX_DEPTH + 1
            )
        );
        assert_eq!(
            zones_failure(
                "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:A\nBEGIN:STANDARD\nTZOFFSETTO:+0100\n\
                 END:STANDARD\nEND:VTIMEZONE\nEND:VCALENDAR\n"
            ),
            "line 6: the STANDARD: an observance has no DTSTART"
        );
    }

    #[test]
    fn fails_with_the_line_of_a_malformed_time() {
        let error = read(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nDTSTART:20261301T000000\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("line 4: the DTSTART value \"20261301T000000\" is not a date-time"),
            "{error}"
        );
    }
}
