//! dBase tables, as Tika's `DBFParser` reads them: the header's version as the type and its date
//! as the modification date, the columns' names as a table head, every record but a deleted one
//! as a row, and the text decoded in the charset detected from the first ten records' character
//! data.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
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
    /// A Visual FoxPro date and time.
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
    columns: Vec<Column>,
}

/// The table as Tika reads it: the header, then each record's cells, up to the exception that
/// ends it where one does.
#[derive(Debug)]
struct Table<'a> {
    header: Header,
    rows: Vec<Vec<&'a [u8]>>,
    /// Whether each record, by its mark, is deleted; a mark may follow the last row.
    deleted: Vec<bool>,
    /// The exception reading the records ends with, and how many of them were read before it.
    thrown: Option<(usize, String)>,
}

/// Tika's `DBFParser`.
pub(crate) struct DbfParser;

impl Parser for DbfParser {
    fn name(&self) -> &'static str {
        "extractous::DBFParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.dbf.DBFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-dbf"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let table = table(input).map_err(|thrown| Declined::Fails(Error::ParseError(thrown)))?;
        if table
            .header
            .columns
            .iter()
            .any(|column| column.kind == Kind::DateTime)
        {
            return Err(Declined::Deferred(
                "a date and time column, which Tika writes from a lenient calendar in the JVM's \
                 time zone"
                    .to_owned(),
            ));
        }
        Ok(Box::new(move |xhtml| write(&table, xhtml)))
    }
}

/// The input as Tika's stream reads it.
struct Stream<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Stream<'a> {
    fn read(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.at).copied()?;
        self.at += 1;
        Some(byte)
    }

    /// `IOUtils.readFully` and `skipFully`.
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let taken = self
            .bytes
            .get(self.at..self.at.saturating_add(count))
            .ok_or_else(|| "EOFException: the table ends early".to_owned())?;
        self.at += count;
        Ok(taken)
    }

    /// `IOUtils.read`: as many bytes as are left, up to `count`.
    fn some(&mut self, count: usize) -> &'a [u8] {
        let end = self.bytes.len().min(self.at + count);
        let read = &self.bytes[self.at..end];
        self.at = end;
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
    stream.take(20)?;
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
        columns,
    })
}

/// `DBFReader.open` and `next` until there are no more records.
fn table(input: &[u8]) -> Result<Table<'_>, String> {
    let mut stream = Stream {
        bytes: input,
        at: 0,
    };
    let header = header(&mut stream)?;
    let mut rows = Vec::new();
    let mut deleted = Vec::new();
    let thrown = loop {
        match stream.read() {
            Some(mark @ (b' ' | DELETED)) => deleted.push(mark == DELETED),
            None | Some(26) => break None,
            Some(other) => {
                break Some((
                    rows.len(),
                    format!("Expecting space or asterisk at beginning of record, not:{other}"),
                ));
            }
        }
        let cells: Vec<&[u8]> = header
            .columns
            .iter()
            .map(|column| stream.some(column.length))
            .collect();
        if cells.iter().all(|cell| cell.is_empty()) {
            break None;
        }
        rows.push(cells);
    };
    Ok(Table {
        header,
        rows,
        deleted,
        thrown,
    })
}

/// `getCharset`: the charset detected from the character data of the first rows, ISO-8859-1
/// where there is too little of it, and the charset it is decoded as.
fn charsets(table: &Table<'_>) -> (Charset, Charset) {
    let mut data = Vec::new();
    for row in table.rows.iter().take(ROWS_FOR_DETECTION) {
        for (cell, column) in row.iter().zip(&table.header.columns) {
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

fn decode(charset: Charset, bytes: &[u8]) -> Result<String, Failure> {
    charset.decode(bytes).ok_or_else(|| {
        Failure::Malformed(format!(
            "the table is detected as {}, which the native decoders do not decode",
            charset.name()
        ))
    })
}

fn write(table: &Table<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let metadata = xhtml.metadata_mut();
    metadata.set("Content-Type", table.header.content_type.as_str());
    if let Some(modified) = table.header.modified {
        metadata.set("dcterms:modified", format_instant(modified));
    }
    if let Some((read, thrown)) = &table.thrown {
        if *read <= ROWS_FOR_DETECTION {
            return Err(Failure::Malformed(thrown.clone()));
        }
    }
    let (detected, decode_as) = charsets(table);
    xhtml
        .metadata_mut()
        .set("Content-Encoding", detected.name());
    xhtml.start_element("table")?;
    xhtml.start_element("thead")?;
    for column in &table.header.columns {
        xhtml.start_element("th")?;
        xhtml.characters(java::trim(&decode(decode_as, &column.name)?))?;
        xhtml.end_element("th")?;
    }
    xhtml.end_element("thead")?;
    xhtml.start_element("tbody")?;
    // Tika writes deleted records as rows too (#118).
    for (row, _) in table
        .rows
        .iter()
        .zip(&table.deleted)
        .filter(|(_, &deleted)| !deleted)
    {
        xhtml.start_element("tr")?;
        for (cell, column) in row.iter().zip(&table.header.columns) {
            let text = match column.kind {
                Kind::Character => java::trim(&decode(decode_as, cell)?).to_owned(),
                Kind::Date => date(cell),
                Kind::Other => java::trim(&java::ascii(cell)).to_owned(),
                Kind::DateTime => {
                    unreachable!("a table with a date and time column is left to Tika")
                }
            };
            xhtml.start_element("td")?;
            xhtml.characters(&text)?;
            xhtml.end_element("td")?;
        }
        xhtml.end_element("tr")?;
    }
    if let Some((_, thrown)) = &table.thrown {
        return Err(Failure::Malformed(thrown.clone()));
    }
    xhtml.end_element("tbody")?;
    xhtml.end_element("table")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
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
        let deferred = scratch.write(
            "tid.dbf",
            &table(0x30, &[("TID", b'T', 8)], &[(b' ', vec![&[0; 8][..]])]),
        );
        let (parsed, left) = documents(&DbfParser, &[scratch.directory()]);
        assert_eq!(left, [deferred]);
        assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            DbfParser.tika_name(),
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
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
