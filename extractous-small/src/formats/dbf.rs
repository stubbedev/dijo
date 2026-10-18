//! dBase tables, as Tika's `DBFParser` reads them: the header's version as the type and its date
//! as the modification date, the columns' names as a table head, every record but a deleted one
//! as a row, and the text decoded in the code page the header's language driver names, or,
//! where it names none, the charset detected from the first ten records' character data. Tika's
//! always detects (#118).

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::java;
use crate::java::time::{civil, format_instant};
use crate::text::charset::{self, Charset};

/// `ROWS_TO_BUFFER_FOR_CHARSET_DETECTION`.
const ROWS_FOR_DETECTION: usize = 10;
/// `MAX_CHARS_FOR_CHARSET_DETECTION`.
const MAX_BYTES_FOR_DETECTION: usize = 20_000;
/// The character data a charset is detected from only when there is more of it.
const MIN_BYTES_FOR_DETECTION: usize = 20;
/// The byte that ends the header.
const END_OF_HEADER: u8 = 13;
/// `MILLIS_PER_DAY`.
const SECONDS_PER_DAY: i64 = 86_400;
/// The mark that begins a deleted record.
const DELETED: u8 = b'*';

/// The JDK charset of the code page a header's language driver ID names, as dBase, FoxPro and
/// Clipper number them.
fn code_page(driver: u8) -> Option<&'static str> {
    Some(match driver {
        0x01 | 0x09 | 0x0B | 0x0D | 0x0F | 0x11 | 0x15 | 0x18 | 0x19 | 0x1B => "IBM437",
        0x02 | 0x0A | 0x0E | 0x10 | 0x12 | 0x14 | 0x16 | 0x1A | 0x1D | 0x25 | 0x37 => "IBM850",
        0x03 | 0x57 | 0x58 | 0x59 => "windows-1252",
        0x04 => "x-MacRoman",
        0x08 | 0x17 | 0x66 => "IBM865",
        0x13 | 0x7B => "windows-31j",
        0x1C | 0x6C => "IBM863",
        0x1F | 0x22 | 0x23 | 0x40 | 0x64 | 0x87 => "IBM852",
        0x24 => "IBM860",
        0x26 | 0x65 => "IBM866",
        0x4D | 0x7A => "GBK",
        0x4E | 0x79 => "x-windows-949",
        0x4F | 0x78 => "x-windows-950",
        0x50 | 0x7C => "x-windows-874",
        0x67 => "IBM861",
        0x6A | 0x86 => "x-IBM737",
        0x6B | 0x88 => "IBM857",
        0x96 => "x-MacCyrillic",
        0x97 => "x-MacCentralEurope",
        0x98 => "x-MacGreek",
        0xC8 => "windows-1250",
        0xC9 => "windows-1251",
        0xCA => "windows-1254",
        0xCB => "windows-1253",
        0xCC => "windows-1257",
        _ => return None,
    })
}

/// `DBFReader.Version`: the table's format and type, by its first byte.
fn version(first: u8) -> Option<(&'static str, &'static str)> {
    Some(match first {
        0x02 | 0xFB => ("FoxBASE", ""),
        0x03 => ("FoxBASE_plus", ""),
        0x30 => ("Visual_FoxPro", ""),
        0x31 => ("Visual_FoxPro", "autoincrement"),
        0x32 => ("Visual_FoxPro", "Varchar_or_Varbinary"),
        0x43 => ("dBASE_IV_SQL", "table"),
        0x63 => ("dBASE_IV_SQL", "system"),
        0x83 => ("FoxBASE_plus", "memo"),
        0x8B => ("dBASE_IV", "memo"),
        0xCB => ("dBASE_IV_SQL", "table_with_memo"),
        0xF5 => ("FoxPro_2.x", "memo"),
        0xE5 => ("HiPer-Siz", "SMT_memo"),
        _ => return None,
    })
}

/// How a column's bytes are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Character data, in the detected charset.
    Character,
    /// A date, `yyyyMMdd`, written `MM/dd/yyyy`.
    Date,
    /// A Visual FoxPro date and time: the Julian day number and the milliseconds since midnight,
    /// little-endian, written `yyyy-MM-ddTHH:mm:ss`, a wall-clock time of no zone. Tika's writes
    /// the day alone, from a lenient Julian-Gregorian calendar, in the JVM's time zone, as if it
    /// were UTC (#118).
    DateTime,
    /// Anything else, as ASCII.
    Other,
}

impl Kind {
    /// `DBFColumnHeader.setType`: the kinds of the types Tika knows.
    fn of(code: u8) -> Option<Self> {
        Some(match code {
            b'C' => Self::Character,
            b'D' => Self::Date,
            b'T' => Self::DateTime,
            b'Y' | b'B' | b'I' | b'G' | b'P' | b'F' | b'L' | b'M' | b'N' | b'+' | b'@' | b'O'
            | b'0' => Self::Other,
            _ => return None,
        })
    }
}

/// A column of the header.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Column {
    name: [u8; 11],
    kind: Kind,
    length: usize,
}

/// What the header gives.
#[derive(Debug)]
struct Header {
    content_type: String,
    modified: Option<i64>,
    /// The charset of the code page the language driver names, if it names one.
    code_page: Option<Charset>,
    columns: Vec<Column>,
}

/// A record of the table: whether its mark deletes it, and its cells.
#[derive(Debug)]
struct Record<'a> {
    deleted: bool,
    cells: Vec<&'a [u8]>,
}

/// What reading the next record finds.
#[derive(Debug)]
enum Next<'a> {
    Record(Record<'a>),
    /// No more records.
    End,
    /// The exception that ends the records.
    Thrown(String),
}

/// Tika's `DBFParser`.
pub struct DbfParser;

impl Parser for DbfParser {
    fn name(&self) -> &'static str {
        "extractous::DBFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.dbf.DBFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-dbf"]
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

    /// Reads the header, failing where Tika's reader throws on it; the records are read as
    /// they are written.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let mut stream = Stream {
            bytes: input,
            at: 0,
            paging: Paging::new(pages),
        };
        let header =
            header(&mut stream).map_err(|thrown| Decline::Fails(Error::ParseError(thrown)))?;
        Ok(Box::new(move |xhtml| write(&header, stream, xhtml)))
    }
}

/// The input as Tika's stream reads it, whose pages it releases as it reads on.
struct Stream<'a> {
    bytes: &'a [u8],
    at: usize,
    paging: Paging<'a>,
}

impl<'a> Stream<'a> {
    fn read(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.at).copied()?;
        self.at += 1;
        self.paging.read(1);
        Some(byte)
    }

    /// `IOUtils.readFully` and `skipFully`.
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let taken = self
            .bytes
            .get(self.at..self.at.saturating_add(count))
            .ok_or_else(|| "EOFException: the table ends early".to_owned())?;
        self.at += count;
        self.paging.read(count);
        Ok(taken)
    }

    /// `IOUtils.read`: as many bytes as are left, up to `count`.
    fn some(&mut self, count: usize) -> &'a [u8] {
        let end = self.bytes.len().min(self.at + count);
        let read = &self.bytes[self.at..end];
        self.at = end;
        self.paging.read(read.len());
        read
    }

    fn number<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)
            .map(|bytes| bytes.try_into().expect("N bytes"))
            .map_err(|_| "BufferUnderrunException".to_owned())
    }
}

/// The header's date at midnight UTC, none for a month or day out of range. dBase counts the
/// year from 1900, and FoxBASE and others write its last two digits, so a year byte from 80 on
/// counts from 1900 (1980, the year dBase came out, to 2155) and one below from 2000. Tika takes
/// a byte that would put the date after the current year as counted from 1900 and any other
/// from 2000, so the output changes over time, and rolls fields out of range over (#118).
fn modified(date: [Option<u8>; 3]) -> Option<i64> {
    let [Some(year), Some(month), Some(day)] = date.map(|part| part.map(i64::from)) else {
        return None;
    };
    let year = if year >= 80 { 1900 + year } else { 2000 + year };
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if civil::is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days_in_month)
        .contains(&day)
        .then(|| civil::days_from_civil(year, month, day) * SECONDS_PER_DAY * 1000)
}

/// `DBFFileHeader.parse`.
fn header(stream: &mut Stream<'_>) -> Result<Header, String> {
    let first = stream.read();
    let (format, kind) = first.and_then(version).ok_or_else(|| {
        format!(
            "Unrecognized first byte in DBFFile: {}",
            first.map_or(-1, i32::from)
        )
    })?;
    let content_type = if kind.is_empty() {
        format!("application/x-dbf; format={format}")
    } else {
        format!("application/x-dbf; format={format}; type={kind}")
    };
    let modified = modified([(); 3].map(|()| stream.read()));
    stream.number::<4>()?;
    let header_length = i16::from_le_bytes(stream.number()?);
    let record_length = i16::from_le_bytes(stream.number()?);
    let reserved = stream.take(20)?;
    let driver = reserved[29 - 12];
    let code_page = code_page(driver).map(|name| {
        Charset::for_name(name)
            .unwrap_or_else(|| panic!("the JDK has the charset {name} of code page mark {driver}"))
    });
    let mut columns = Vec::new();
    let mut accounted = 0;
    loop {
        let field = stream.take(32)?;
        let mut name = [0u8; 11];
        name[..10].copy_from_slice(&field[..10]);
        let code = field[11];
        let kind = Kind::of(code).ok_or_else(|| {
            format!(
                "Unrecognized column type for column: {}. I regret I don't recognize: {}",
                java::trim(&java::ascii(&name)),
                char::from(code)
            )
        })?;
        let length = usize::from(field[16]);
        accounted += length;
        columns.push(Column { name, kind, length });
        if accounted as i64 >= i64::from(record_length) - 1 {
            break;
        }
    }
    if stream.read() != Some(END_OF_HEADER) {
        return Err("Expected new line at end of header".to_owned());
    }
    let read = 32 + columns.len() as i64 * 32 + 1;
    let extra = i64::from(header_length) - read;
    let extra = usize::try_from(extra).map_err(|_| {
        format!("IllegalArgumentException: Skip count must be non-negative, actual: {extra}")
    })?;
    stream.take(extra)?;
    Ok(Header {
        content_type,
        modified,
        code_page,
        columns,
    })
}

/// `DBFReader.nextRecord`: the next record, read from `stream` past the header.
fn next_record<'a>(stream: &mut Stream<'a>, columns: &[Column]) -> Next<'a> {
    let deleted = match stream.read() {
        Some(mark @ (b' ' | DELETED)) => mark == DELETED,
        None | Some(26) => return Next::End,
        Some(other) => {
            return Next::Thrown(format!(
                "Expecting space or asterisk at beginning of record, not:{other}"
            ));
        }
    };
    let cells: Vec<&[u8]> = columns
        .iter()
        .map(|column| stream.some(column.length))
        .collect();
    if cells.iter().all(|cell| cell.is_empty()) {
        return Next::End;
    }
    Next::Record(Record { deleted, cells })
}

/// The code page the header names, or, as `getCharset` gives it, the charset detected from the
/// character data of the `first` records, ISO-8859-1 where there is too little of it; and the
/// charset it is decoded as.
fn charsets(header: &Header, first: &[Record<'_>]) -> (Charset, Charset) {
    if let Some(code_page) = header.code_page {
        return (code_page, code_page);
    }
    let mut data = Vec::new();
    for record in first.iter().take(ROWS_FOR_DETECTION) {
        for (cell, column) in record.cells.iter().zip(&header.columns) {
            if column.kind == Kind::Character {
                data.extend_from_slice(cell);
                if data.len() > MAX_BYTES_FOR_DETECTION {
                    break;
                }
            }
        }
    }
    if data.len() <= MIN_BYTES_FOR_DETECTION {
        let latin1 = Charset::named("ISO-8859-1");
        return (latin1, latin1);
    }
    let detection = charset::detect(&data, None);
    (detection.charset, detection.decode_as)
}

/// `DBFCell.getFormattedDate`: `MM/dd/yyyy`, or nothing for a date that is not numbers.
fn date(cell: &[u8]) -> String {
    if cell.len() < 8 {
        return String::new();
    }
    let part = |range: std::ops::Range<usize>| java::ascii(&cell[range]);
    let (year, month, day) = (part(0..4), part(4..6), part(6..8));
    if [&year, &month, &day]
        .iter()
        .any(|part| java::parse_int(part).is_none())
    {
        return String::new();
    }
    format!("{month}/{day}/{year}")
}

/// The Julian day number of 1970-01-01.
const JULIAN_DAY_OF_EPOCH: i64 = 2_440_588;

/// A Visual FoxPro date and time, `yyyy-MM-ddTHH:mm:ss` on the proleptic Gregorian calendar, or
/// nothing for one that is blank or out of range.
fn date_time(cell: &[u8]) -> String {
    let Some((day, time)) = cell.get(..8).map(|bytes| bytes.split_at(4)) else {
        return String::new();
    };
    let day = i64::from(i32::from_le_bytes(day.try_into().expect("four bytes")));
    let millis = i64::from(i32::from_le_bytes(time.try_into().expect("four bytes")));
    if day <= 0 || !(0..SECONDS_PER_DAY * 1000).contains(&millis) {
        return String::new();
    }
    let (year, month, day) = civil::civil_from_days(day - JULIAN_DAY_OF_EPOCH);
    let seconds = millis / 1000;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn decode(charset: Charset, bytes: &[u8]) -> Result<String, Failure> {
    charset.decode(bytes).ok_or_else(|| {
        Failure::Malformed(format!(
            "the table is detected as {}, which the native decoders do not decode",
            charset.name()
        ))
    })
}

/// Writes the table: its first records, read ahead to detect the charset from, which Tika's
/// reader fails on without writing the table where it throws among them, then each record as
/// it is read.
fn write(header: &Header, mut stream: Stream<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let metadata = xhtml.metadata_mut();
    metadata.set("Content-Type", header.content_type.as_str());
    if let Some(modified) = header.modified {
        metadata.set("dcterms:modified", format_instant(modified));
    }
    let mut first = Vec::with_capacity(ROWS_FOR_DETECTION + 1);
    let mut ended = false;
    while first.len() <= ROWS_FOR_DETECTION {
        match next_record(&mut stream, &header.columns) {
            Next::Record(record) => first.push(record),
            Next::End => {
                ended = true;
                break;
            }
            Next::Thrown(thrown) => return Err(Failure::Malformed(thrown)),
        }
    }
    let (detected, decode_as) = charsets(header, &first);
    xhtml
        .metadata_mut()
        .set("Content-Encoding", detected.name());
    xhtml.start_element("table")?;
    xhtml.start_element("thead")?;
    for column in &header.columns {
        xhtml.start_element("th")?;
        xhtml.characters(java::trim(&decode(decode_as, &column.name)?))?;
        xhtml.end_element("th")?;
    }
    xhtml.end_element("thead")?;
    xhtml.start_element("tbody")?;
    for record in &first {
        write_record(record, &header.columns, decode_as, xhtml)?;
    }
    drop(first);
    while !ended {
        match next_record(&mut stream, &header.columns) {
            Next::Record(record) => write_record(&record, &header.columns, decode_as, xhtml)?,
            Next::End => ended = true,
            Next::Thrown(thrown) => return Err(Failure::Malformed(thrown)),
        }
    }
    xhtml.end_element("tbody")?;
    xhtml.end_element("table")?;
    Ok(())
}

/// Writes `record` as a row, unless it is deleted, which Tika writes as a row too (#118).
fn write_record(
    record: &Record<'_>,
    columns: &[Column],
    decode_as: Charset,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    if record.deleted {
        return Ok(());
    }
    xhtml.start_element("tr")?;
    for (cell, column) in record.cells.iter().zip(columns) {
        let text = match column.kind {
            Kind::Character => java::trim(&decode(decode_as, cell)?).to_owned(),
            Kind::Date => date(cell),
            Kind::Other => java::trim(&java::ascii(cell)).to_owned(),
            Kind::DateTime => date_time(cell),
        };
        xhtml.start_element("td")?;
        xhtml.characters(&text)?;
        xhtml.end_element("td")?;
    }
    xhtml.end_element("tr")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_rewriting,
        conformance_corpus, documents, ComparedMetadata, Scratch,
    };

    /// A table of the `columns`, each a name, type and length, and the `records`, each a deletion
    /// mark and its cells' bytes.
    fn table(version: u8, columns: &[(&str, u8, u8)], records: &[(u8, Vec<&[u8]>)]) -> Vec<u8> {
        let record_length: usize = 1 + columns
            .iter()
            .map(|&(_, _, length)| usize::from(length))
            .sum::<usize>();
        let header_length = 32 + 32 * columns.len() + 1;
        let mut file = vec![version, 124, 3, 9];
        file.extend_from_slice(&u32::try_from(records.len()).unwrap().to_le_bytes());
        file.extend_from_slice(&u16::try_from(header_length).unwrap().to_le_bytes());
        file.extend_from_slice(&u16::try_from(record_length).unwrap().to_le_bytes());
        file.extend_from_slice(&[0; 20]);
        for &(name, kind, length) in columns {
            let mut field = [0u8; 32];
            field[..name.len()].copy_from_slice(name.as_bytes());
            field[11] = kind;
            field[16] = length;
            file.extend_from_slice(&field);
        }
        file.push(13);
        for (mark, cells) in records {
            file.push(*mark);
            for (cell, &(_, _, length)) in cells.iter().zip(columns) {
                let mut padded = cell.to_vec();
                padded.resize(usize::from(length), b' ');
                file.extend_from_slice(&padded);
            }
        }
        file
    }

    const COLUMNS: [(&str, u8, u8); 4] = [
        ("NAVN", b'C', 24),
        ("BELOEB", b'N', 8),
        ("DATO", b'D', 8),
        ("AKTIV", b'L', 1),
    ];

    fn record(
        name: &'static [u8],
        amount: &'static [u8],
        date: &'static [u8],
    ) -> (u8, Vec<&'static [u8]>) {
        (b' ', vec![name, amount, date, b"T"])
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_dbase_tables_as_tikas_path_does() {
        let scratch = Scratch::new("dbf");
        // Without a deleted record, which Tika writes as a row (#118).
        let mut records = vec![
            record(b"S\xf8ren S\xf8rensen", b"1250.50", b"20240309"),
            (
                b' ',
                vec![b"Slettet \xc6bleh\xf8j", b"0", b"2024AB09", b"F"],
            ),
            record(b"\xc5se \xd8stergaard", b"-3.25", b"1999"),
            record(b"M\xe6rsk", b"12", b"+0010101"),
        ];
        let customers = table(0x03, &COLUMNS, &records);
        let mut ended = customers.clone();
        ended.extend_from_slice(&[26, b' ', b'x']);
        let short = table(0x30, &COLUMNS, &[record(b"Ib", b"1", b"20000101")]);
        for index in 0..10 {
            records.push(record(
                b"R\xe6kke",
                if index % 2 == 0 { b"1" } else { b"2" },
                b"20240101",
            ));
        }
        let many = table(0x8B, &COLUMNS, &records);
        let mut early = table(0x03, &COLUMNS, &records[..2]);
        early.extend_from_slice(b"X");
        let read = [
            scratch.write("kunder.dbf", &customers),
            scratch.write("afsluttet.dbf", &ended),
            scratch.write("kort.dbf", &short),
            scratch.write("mange.dbf", &many),
            scratch.write("afkortet.dbf", &customers[..customers.len() - 30]),
        ];
        let failing = [
            scratch.write("hoved.dbf", &customers[..40]),
            scratch.write("tidlig.dbf", &early),
            scratch.write("type.dbf", &table(0x03, &[("X", b'Q', 4)], &[])),
        ];
        let timed = scratch.write(
            "tid.dbf",
            &table(
                0x30,
                &[("NAVN", b'C', 10), ("TID", b'T', 8)],
                &[
                    (
                        b' ',
                        vec![&b"Morgen"[..], &date_time_cell(2_460_380, 30_645_000)],
                    ),
                    (b' ', vec![&b"Midnat"[..], &date_time_cell(2_299_161, 0)]),
                ],
            ),
        );
        let (parsed, left) = documents(&DbfParser, &[scratch.directory()]);
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(parsed.len(), read.len() + failing.len() + 1, "{parsed:?}");
        assert_same_as_tika_rewriting(&[timed.as_path()], DbfParser.tika_name(), as_tika_times);
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            DbfParser.tika_name(),
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    /// A Visual FoxPro date and time cell of the Julian day number `day` and the milliseconds
    /// since midnight `millis`.
    fn date_time_cell(day: i32, millis: i32) -> Vec<u8> {
        [day.to_le_bytes(), millis.to_le_bytes()].concat()
    }

    /// The native text with each date and time of a cell, from 1582-10-15 on, as Tika's path
    /// writes it: the day's midnight in UTC, without its time, written in the JVM's time zone and
    /// marked as UTC (#118).
    #[cfg(feature = "tika")]
    fn as_tika_times(text: &mut String, _: &mut ComparedMetadata, _: &ComparedMetadata) {
        let zone = crate::java::time::TimeZone::java_default().unwrap();
        let time = regex::Regex::new(r"(\d{4})-(\d{2})-(\d{2})T\d{2}:\d{2}:\d{2}(Z?)").unwrap();
        *text = time
            .replace_all(text, |found: &regex::Captures| {
                if &found[4] == "Z" {
                    return found[0].to_owned();
                }
                let [year, month, day] = [1, 2, 3].map(|group| found[group].parse().unwrap());
                let midnight = civil::days_from_civil(year, month, day) * SECONDS_PER_DAY * 1000;
                format_instant(midnight + zone.offset_at_utc(midnight))
            })
            .into_owned();
    }

    #[test]
    fn writes_date_and_time_cells_as_written() {
        assert_eq!(
            date_time(&date_time_cell(2_460_380, 30_645_999)),
            "2024-03-10T08:30:45"
        );
        assert_eq!(
            date_time(&date_time_cell(2_299_161, 0)),
            "1582-10-15T00:00:00"
        );
        assert_eq!(date_time(&date_time_cell(0, 0)), "");
        assert_eq!(date_time(&date_time_cell(2_460_380, 86_400_000)), "");
        assert_eq!(date_time(b"        "), "");
        assert_eq!(date_time(&[1, 2, 3]), "");
    }

    /// The records the body at `index` of [`a_table_too_large_to_read_whole`] holds.
    const RECORDS_PER_BODY: u64 = 1000;

    /// The text of the row of record `number`.
    fn large_row(number: u64) -> String {
        format!("Række {number}\t{}\t03/09/2024\tT", number % 1000)
    }

    /// A table too large to read whole is read a record at a time, each row written as it is
    /// read.
    #[test]
    fn a_table_too_large_to_read_whole() {
        let mut head = table(0x03, &COLUMNS, &[]);
        head[29] = 0x03;
        let record_length = 42;
        let body = |index: u64| {
            let first = index * RECORDS_PER_BODY;
            let mut records = Vec::with_capacity(record_length * RECORDS_PER_BODY as usize);
            for number in first..first + RECORDS_PER_BODY {
                records.push(b' ');
                records.extend_from_slice(b"R\xe6kke ");
                records.extend_from_slice(format!("{number:<18}").as_bytes());
                records.extend_from_slice(format!("{:<8}", number % 1000).as_bytes());
                records.extend_from_slice(b"20240309T");
            }
            records
        };
        let count =
            extractous_core::large::bodies_past_limit(record_length * RECORDS_PER_BODY as usize);
        let file = extractous_core::large::LargeFile::new("long.dbf", &head, count, body, b"");
        let mut rows = 0;
        let metadata = file.extract_lines(DbfParser.name(), |line| {
            if line.starts_with("Række") {
                assert_eq!(line, large_row(rows));
                rows += 1;
            }
        });
        assert_eq!(rows, count * RECORDS_PER_BODY);
        assert_eq!(metadata["Content-Encoding"], ["windows-1252"]);
    }

    #[test]
    fn decodes_text_in_the_code_page_the_header_names() {
        let columns = [("NAVN", b'C', 30)];
        let records = [
            (b' ', vec![&b"S\x9brensen fra \x8frhus"[..]]),
            (b' ', vec![&b"\x92bler og \x9dstersvamp"[..]]),
        ];
        let mut file = table(0x03, &columns, &records);
        file[29] = 0x66;
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&file)
            .unwrap();
        assert_eq!(metadata["Content-Encoding"], ["IBM865"]);
        assert!(text.contains("Sørensen fra Århus"), "{text}");
        assert!(text.contains("Æbler og Østersvamp"), "{text}");
        assert_eq!(code_page(0), None);
        for driver in 0..=u8::MAX {
            if let Some(name) = code_page(driver) {
                assert!(Charset::for_name(name).is_some(), "{name}");
            }
        }
    }

    #[test]
    fn leaves_out_deleted_records_and_dates_from_the_current_year() {
        // Tika writes deleted records, and counts a header's year from 2000 unless that puts
        // it after the current year, so 79 is 1979 until 2079 (#118).
        let records = [
            record(b"Bevaret", b"1", b"20240309"),
            (b'*', vec![b"Slettet", b"2", b"20240309", b"F"]),
        ];
        let mut file = table(0x03, &COLUMNS, &records);
        file[1] = 79;
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(&file)
            .unwrap();
        assert!(xml.contains("Bevaret"), "{xml}");
        assert!(!xml.contains("Slettet"), "{xml}");
        assert_eq!(metadata["dcterms:modified"], ["2079-03-09T00:00:00Z"]);
        assert_eq!(
            modified([Some(80), Some(1), Some(2)]).map(format_instant),
            Some("1980-01-02T00:00:00Z".to_owned())
        );
        assert_eq!(
            modified([Some(16), Some(5), Some(24)]),
            modified([Some(116), Some(5), Some(24)])
        );
        assert_eq!(modified([Some(126), Some(2), Some(29)]), None);
        assert_eq!(modified([Some(124), Some(13), Some(1)]), None);
        assert_eq!(modified([Some(0), Some(0), Some(0)]), None);
        assert_eq!(modified([Some(124), Some(2), None]), None);
        assert_eq!(
            modified([Some(124), Some(2), Some(29)]).map(format_instant),
            Some("2024-02-29T00:00:00Z".to_owned())
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_dbase_tables_as_tikas_path_does() {
        let (parsed, deferred) = documents(&DbfParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 3, "{parsed:?}");
        let (failing, read): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| path.ends_with("testOCTET_header.dbase3"));
        assert_same_as_tika(&read, DbfParser.tika_name());
        assert_fails_as_tika(&failing);
    }
}
