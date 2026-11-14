//! A note: a 100-byte header, the descriptors of its items, eight bytes each of the unique name
//! key of the item's name and type, its flags and the size of its value, and the values in the
//! order of the descriptors: those of summary items after the descriptors, the others in the
//! note's non-summary record, which a summary bucket's slot or a file position holds, after its
//! 64-byte header and four bytes more. A note shorter than its header is a deleted note's stub.
//!
//! The values are those the Notes C API documents: text in LMBCS, a NUL breaking its lines; a
//! text list of its count, the sizes of its entries and the entries; a number as an IEEE double;
//! a time as a TIMEDATE, the hundredths of a second since midnight GMT and the Julian day, which
//! all ones leave out; a range of numbers or times as the counts of its entries and of its pairs,
//! then those; rich text as CD records ([`composite`]); and a file attachment as a FILEOBJECT,
//! after its type.

use super::composite;
use super::ods::{Name, BSID_FLAG, BUCKET_INDEX_MASK};
use super::{offset, span, u16_at, u32_at, Problem};
use crate::formats::lmbcs;
use crate::formats::xhtml::Xhtml;
use crate::formats::Failure;

pub(super) const NOTE_SIGNATURE: u16 = 0x0004;
const NON_SUMMARY_SIGNATURE: u16 = 0x0010;
const HEADER_SIZE: usize = 100;
const DESCRIPTOR_SIZE: usize = 8;
const NON_SUMMARY_VALUES_AT: usize = 68;
const ITEM_SUMMARY: u16 = 0x0004;
const NON_SUMMARY_SLOT_MASK: u32 = 0x7F;

/// The name of the items of file attachments.
const ATTACHMENT: &str = "$FILE";
/// What starts the names Notes reserves for its own items.
const RESERVED: char = '$';
const OBJECT_FILE: u16 = 0;
/// The flags an object's type may carry: not to be copied, preserved, public.
const OBJECT_FLAGS: u16 = 0xE000;
const FILE_OBJECT_SIZE: usize = 38;

const TICKS_PER_DAY: u32 = 8_640_000;
/// The ticks of a TIMEDATE of a date alone.
const ALL_DAY: u32 = u32::MAX;
/// The date half of a TIMEDATE of a time of day alone.
const ANY_DAY: u32 = u32::MAX;
const JULIAN_DAY_MASK: u32 = 0x00FF_FFFF;
const TIMEDATE_SIZE: usize = 8;
const NUMBER_SIZE: usize = 8;

/// Item types, their class in the high byte.
mod kind {
    pub(super) const COMPOSITE: u16 = 0x0001;
    pub(super) const OBJECT: u16 = 0x0003;
    pub(super) const SEAL: u16 = 0x0009;
    pub(super) const SEAL_DATA: u16 = 0x000A;
    pub(super) const SEAL_LIST: u16 = 0x000B;
    pub(super) const NUMBER: u16 = 0x0300;
    pub(super) const NUMBER_RANGE: u16 = 0x0301;
    pub(super) const TIME: u16 = 0x0400;
    pub(super) const TIME_RANGE: u16 = 0x0401;
    pub(super) const TEXT: u16 = 0x0500;
    pub(super) const TEXT_LIST: u16 = 0x0501;
}

/// Where a note's non-summary record lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NonSummaryAt {
    /// A slot of a non-summary bucket, both counted from 1.
    Slot {
        bucket: u32,
        slot: u16,
    },
    Position(usize),
}

/// A note's header, its item descriptors and summary values.
pub(super) struct Header<'a> {
    descriptors: &'a [u8],
    summary: &'a [u8],
    pub(super) non_summary_at: Option<NonSummaryAt>,
    pub(super) non_summary_size: u32,
}

impl<'a> Header<'a> {
    /// The note `record` starts with, none for a deleted note's stub.
    pub(super) fn read(record: &'a [u8]) -> Result<Option<Self>, Problem> {
        let what = "the note";
        if u16_at(record, 0, what)? != NOTE_SIGNATURE {
            return Err(Problem::Malformed(
                "its record does not start with a note's signature".to_owned(),
            ));
        }
        let size = u32_at(record, 2, what)? as usize;
        let record = span(record, 0, size, what)?;
        if size < HEADER_SIZE {
            return Ok(None);
        }
        let count = usize::from(u16_at(record, 50, what)?);
        let descriptors = span(record, HEADER_SIZE, count * DESCRIPTOR_SIZE, "its items")?;
        let summary = &record[HEADER_SIZE + descriptors.len()..];
        let non_summary_at = match u32_at(record, 56, what)? {
            0 => None,
            id if id & BSID_FLAG != 0 => Some(NonSummaryAt::Slot {
                bucket: id & BUCKET_INDEX_MASK,
                slot: (id >> 24 & NON_SUMMARY_SLOT_MASK) as u16,
            }),
            position => Some(NonSummaryAt::Position(offset(position)?)),
        };
        Ok(Some(Header {
            descriptors,
            summary,
            non_summary_at,
            non_summary_size: u32_at(record, 60, what)?,
        }))
    }

    /// The item values of the non-summary record `record` of `size` bytes.
    pub(super) fn non_summary_values(record: &'a [u8], size: u32) -> Result<&'a [u8], Problem> {
        let what = "a non-summary record";
        if u16_at(record, 0, what)? != NON_SUMMARY_SIGNATURE || u32_at(record, 2, what)? != size {
            return Err(Problem::Malformed(format!(
                "a note's non-summary record is not the one of {size} bytes it names"
            )));
        }
        span(record, 0, size as usize, what)?
            .get(NON_SUMMARY_VALUES_AT..)
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "a non-summary record of {size} bytes is shorter than its header"
                ))
            })
    }
}

/// A note, the values of its items at hand.
pub(super) struct Note<'a> {
    descriptors: &'a [u8],
    summary: &'a [u8],
    non_summary: &'a [u8],
    pub(super) non_summary_at: Option<NonSummaryAt>,
}

impl<'a> Note<'a> {
    pub(super) fn new(header: Header<'a>, non_summary: &'a [u8]) -> Self {
        Note {
            descriptors: header.descriptors,
            summary: header.summary,
            non_summary,
            non_summary_at: header.non_summary_at,
        }
    }

    /// The items, their names and types among `names`.
    pub(super) fn items<'n>(&'n self, names: &'n [Name]) -> Items<'n> {
        Items {
            names,
            descriptors: self.descriptors,
            summary: self.summary,
            non_summary: self.non_summary,
            failed: false,
        }
    }
}

/// A note's items in order, up to the first that is not well-formed.
pub(super) struct Items<'n> {
    names: &'n [Name],
    descriptors: &'n [u8],
    summary: &'n [u8],
    non_summary: &'n [u8],
    failed: bool,
}

impl<'n> Iterator for Items<'n> {
    type Item = Result<Item<'n>, Problem>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.descriptors.len() < DESCRIPTOR_SIZE {
            return None;
        }
        let (descriptor, rest) = self.descriptors.split_at(DESCRIPTOR_SIZE);
        self.descriptors = rest;
        let key = usize::from(u16::from_le_bytes([descriptor[0], descriptor[1]]));
        let flags = u16::from_le_bytes([descriptor[2], descriptor[3]]);
        let size = usize::from(u16::from_le_bytes([descriptor[4], descriptor[5]]));
        let names: &'n [Name] = self.names;
        let (area, which) = match flags & ITEM_SUMMARY {
            0 => (&mut self.non_summary, "non-summary"),
            _ => (&mut self.summary, "summary"),
        };
        let values: &'n [u8] = area;
        let item = match names.get(key) {
            None => Err(Problem::Malformed(format!(
                "an item names the key {key} of the {} the database holds",
                names.len()
            ))),
            Some(name) if size > values.len() => Err(Problem::Malformed(format!(
                "the item {} of {size} bytes runs past the note's {which} values",
                name.text
            ))),
            Some(name) => {
                let (value, rest) = values.split_at(size);
                *area = rest;
                Ok(Item { name, value })
            }
        };
        self.failed = item.is_err();
        Some(item)
    }
}

/// An item: its name and type, and its value.
pub(super) struct Item<'n> {
    pub(super) name: &'n Name,
    value: &'n [u8],
}

impl Item<'_> {
    /// Whether the item is written: one of a type that shows text, of a name Notes does not
    /// reserve, and not empty.
    pub(super) fn is_written(&self) -> bool {
        matches!(
            self.name.kind,
            kind::TEXT
                | kind::TEXT_LIST
                | kind::NUMBER
                | kind::NUMBER_RANGE
                | kind::TIME
                | kind::TIME_RANGE
                | kind::COMPOSITE
        ) && !self.name.text.starts_with(RESERVED)
            && !self.value.is_empty()
    }

    /// Whether the item is one of those that seal an encrypted note.
    pub(super) fn is_sealed(&self) -> bool {
        matches!(
            self.name.kind,
            kind::SEAL | kind::SEAL_DATA | kind::SEAL_LIST
        )
    }

    /// The file attachment the item describes, if it is one.
    pub(super) fn attachment(&self) -> Result<Option<FileObject>, Problem> {
        if self.name.kind != kind::OBJECT || self.name.text != ATTACHMENT {
            return Ok(None);
        }
        FileObject::read(self.value).map(Some)
    }

    /// Writes the values of the item, each a `<dd>`.
    pub(super) fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let value = self.value;
        match self.name.kind {
            kind::TEXT => definition_lines(value, xhtml),
            kind::TEXT_LIST => {
                for entry in text_list(value)? {
                    definition_lines(entry, xhtml)?;
                }
                Ok(())
            }
            kind::NUMBER => definition(&number(value)?, xhtml),
            kind::NUMBER_RANGE => range(value, NUMBER_SIZE, number, xhtml),
            kind::TIME => definition(&time(value)?, xhtml),
            kind::TIME_RANGE => range(value, TIMEDATE_SIZE, time, xhtml),
            kind::COMPOSITE => {
                xhtml.start_element("dd")?;
                composite::write(value, xhtml)?;
                xhtml.end_element("dd")?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// How an attachment is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Compression {
    None,
    Huffman,
    Lz1,
}

impl Compression {
    fn of(kind: u16) -> Result<Self, Problem> {
        match kind {
            0 => Ok(Compression::None),
            1 => Ok(Compression::Huffman),
            2 => Ok(Compression::Lz1),
            kind => Err(Problem::Malformed(format!(
                "an attachment of the compression type {kind}"
            ))),
        }
    }

    /// Fails the compression of the attachment `name` unless it is none.
    pub(super) fn check(self, name: &str) -> Result<(), Problem> {
        let how = match self {
            Compression::None => return Ok(()),
            Compression::Huffman => "Huffman coding",
            Compression::Lz1 => "LZ1",
        };
        Err(Problem::Unread(format!(
            "the attachment {name} compressed with {how}, which no documentation describes"
        )))
    }
}

/// A file attachment: the file's name, size and compression, and the object that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileObject {
    pub(super) name: String,
    pub(super) object: u32,
    pub(super) size: u32,
    pub(super) compression: Compression,
}

impl FileObject {
    /// The FILEOBJECT of the value `value`, after its type.
    fn read(value: &[u8]) -> Result<Self, Problem> {
        let what = "a file attachment";
        span(value, 0, FILE_OBJECT_SIZE, what)?;
        if u16_at(value, 0, what)? != kind::OBJECT {
            return Err(Problem::Malformed(
                "a file attachment does not start with the type of an object".to_owned(),
            ));
        }
        let object_type = u16_at(value, 2, what)? & !OBJECT_FLAGS;
        if object_type != OBJECT_FILE {
            return Err(Problem::Malformed(format!(
                "a file attachment holds an object of the type {object_type}"
            )));
        }
        let name_size = usize::from(u16_at(value, 8, what)?);
        Ok(FileObject {
            name: lmbcs::decode(
                span(value, FILE_OBJECT_SIZE, name_size, what)?,
                lmbcs::LATIN,
            ),
            object: u32_at(value, 4, what)?,
            size: u32_at(value, 18, what)?,
            compression: Compression::of(u16_at(value, 12, what)?)?,
        })
    }
}

/// Writes the LMBCS text `bytes`, a line break for each NUL that breaks its lines.
pub(super) fn write_lines(bytes: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    for (index, line) in lmbcs::decode_lines(bytes, lmbcs::LATIN).iter().enumerate() {
        if index > 0 {
            xhtml.start_element("br")?;
            xhtml.end_element("br")?;
        }
        xhtml.characters(line)?;
    }
    Ok(())
}

/// Writes `text` as a `<dd>`.
fn definition(text: &str, xhtml: &mut Xhtml) -> Result<(), Failure> {
    xhtml.start_element("dd")?;
    xhtml.characters(text)?;
    xhtml.end_element("dd")?;
    Ok(())
}

/// Writes the LMBCS text `bytes` as a `<dd>`.
fn definition_lines(bytes: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    xhtml.start_element("dd")?;
    write_lines(bytes, xhtml)?;
    xhtml.end_element("dd")?;
    Ok(())
}

/// The entries of the text list `value`.
fn text_list(value: &[u8]) -> Result<Vec<&[u8]>, Problem> {
    let what = "a text list";
    let count = usize::from(u16_at(value, 0, what)?);
    let mut at = 2 + 2 * count;
    (0..count)
        .map(|index| {
            let size = usize::from(u16_at(value, 2 + 2 * index, what)?);
            let entry = span(value, at, size, what)?;
            at += size;
            Ok(entry)
        })
        .collect()
}

/// Writes the range `value` of entries `size` bytes long, shown by `show`: each entry, then each
/// pair, a `<dd>` each.
fn range(
    value: &[u8],
    size: usize,
    show: fn(&[u8]) -> Result<String, Problem>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let what = "a range";
    let entries = usize::from(u16_at(value, 0, what)?);
    let pairs = usize::from(u16_at(value, 2, what)?);
    let listed = span(value, 4, entries * size, what)?;
    let paired = span(value, 4 + listed.len(), pairs * 2 * size, what)?;
    for entry in listed.chunks_exact(size) {
        definition(&show(entry)?, xhtml)?;
    }
    for pair in paired.chunks_exact(2 * size) {
        let (lower, upper) = pair.split_at(size);
        definition(&format!("{} - {}", show(lower)?, show(upper)?), xhtml)?;
    }
    Ok(())
}

/// The number `bytes` holds, an IEEE double, in its shortest form.
fn number(bytes: &[u8]) -> Result<String, Problem> {
    let bytes = <[u8; NUMBER_SIZE]>::try_from(bytes)
        .map_err(|_| Problem::Malformed(format!("a number of {} bytes", bytes.len())))?;
    Ok(f64::from_le_bytes(bytes).to_string())
}

/// The TIMEDATE `bytes` holds, in ISO 8601: a date and time in UTC, a date or a time of day.
fn time(bytes: &[u8]) -> Result<String, Problem> {
    let bytes = <[u8; TIMEDATE_SIZE]>::try_from(bytes)
        .map_err(|_| Problem::Malformed(format!("a time of {} bytes", bytes.len())))?;
    let ticks = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let date = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let clock = match ticks {
        ALL_DAY => None,
        ticks if ticks < TICKS_PER_DAY => Some(clock(ticks)),
        ticks => {
            return Err(Problem::Malformed(format!(
                "a time of {ticks} hundredths of a second past midnight"
            )))
        }
    };
    let day = (date != ANY_DAY).then(|| civil(date & JULIAN_DAY_MASK));
    match (day, clock) {
        (Some(day), Some(clock)) => Ok(format!("{day}T{clock}Z")),
        (Some(day), None) => Ok(day),
        (None, Some(clock)) => Ok(clock),
        (None, None) => Err(Problem::Malformed(
            "a time of neither a date nor a time of day".to_owned(),
        )),
    }
}

/// The time of day `ticks` hundredths of a second past midnight: `HH:MM:SS`, and its hundredths
/// after a point unless they are none.
fn clock(ticks: u32) -> String {
    let seconds = ticks / 100;
    let hundredths = ticks % 100;
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    match hundredths {
        0 => time,
        hundredths => format!("{time}.{hundredths:02}"),
    }
}

/// The date of the Julian day `day` in the proleptic Gregorian calendar, `YYYY-MM-DD`, by
/// Richards' algorithm.
fn civil(day: u32) -> String {
    let day = i64::from(day);
    let f = day + 1401 + (4 * day + 274_277) / 146_097 * 3 / 4 - 38;
    let e = 4 * f + 3;
    let h = 5 * (e % 1461 / 4) + 2;
    let month = (h / 153 + 2) % 12 + 1;
    let year = e / 1461 - 4716 + (14 - month) / 12;
    format!("{year:04}-{month:02}-{:02}", h % 153 / 5 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timedate(ticks: u32, date: u32) -> Vec<u8> {
        [ticks.to_le_bytes(), date.to_le_bytes()].concat()
    }

    #[test]
    fn writes_times_dates_and_times_of_day_in_iso_8601() {
        assert_eq!(
            time(&timedate(0x006C_DCC0, 0x8525_63FC)).unwrap(),
            "1996-12-10T19:49:04Z"
        );
        assert_eq!(
            time(&timedate(6_510_093, 2_461_114)).unwrap(),
            "2026-03-14T18:05:00.93Z"
        );
        assert_eq!(time(&timedate(ALL_DAY, 2_461_115)).unwrap(), "2026-03-15");
        assert_eq!(time(&timedate(6_480_000, ANY_DAY)).unwrap(), "18:00:00");
        assert!(time(&timedate(TICKS_PER_DAY, 2_461_115)).is_err());
        assert!(time(&timedate(ALL_DAY, ANY_DAY)).is_err());
        assert!(time(&[0; 7]).is_err());
    }

    #[test]
    fn writes_numbers_in_their_shortest_form() {
        assert_eq!(number(&23.0f64.to_le_bytes()).unwrap(), "23");
        assert_eq!(number(&41.23f64.to_le_bytes()).unwrap(), "41.23");
        assert!(number(&[0; 4]).is_err());
    }

    #[test]
    fn reads_text_lists_up_to_their_value_and_no_further() {
        let list = [&[2, 0, 4, 0, 2, 0][..], b"Anna", b"Ov"].concat();
        assert_eq!(text_list(&list).unwrap(), [&b"Anna"[..], b"Ov"]);
        assert!(text_list(&list[..list.len() - 1]).is_err());
    }
}
