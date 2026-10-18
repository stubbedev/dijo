//! The spreadsheets of Quattro Pro 1 to 6, for DOS (`.wq1`, `.wq2`) and for Windows (`.wb1`,
//! `.wb2`): records in the manner of Lotus 1-2-3's WK1, each a type, a length and a body, the
//! cells among them addressed by page, row and column.
//!
//! Tika reads none of them: their types specialize `application/x-tika-msoffice`, so its
//! composite parser hands them to `OfficeParser`, which fails them as no compound documents
//! (#158). Each page is written as a table of its rows, each row with a cell for each column up
//! to its last: labels and the text results of formulas as text, numbers and the values of other
//! formulas as numbers, without their display formats. An error or a value that is not
//! available, which these files hold as a NaN, is an empty cell. Text is in the code page of the
//! system the file was written on, which the file does not name: IBM437 for DOS, windows-1252
//! for Windows.
//!
//! The cells are written in the order of their addresses, as the records give them where they
//! come in that order, else by an index of where each cell's last record starts, read again.

use std::collections::BTreeMap;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::java;
use crate::ole2;
use crate::text::charset::Charset;

const BOF: u16 = 0x0000;
const EOF: u16 = 0x0001;
const INTEGER: u16 = 0x000D;
const NUMBER: u16 = 0x000E;
const LABEL: u16 = 0x000F;
const FORMULA: u16 = 0x0010;
const STRING: u16 = 0x0033;

/// The spreadsheets of Quattro Pro 1 to 6, which Tika's path hands to `OfficeParser`.
pub struct OldQuattroProParser;

impl Parser for OldQuattroProParser {
    fn name(&self) -> &'static str {
        "extractous::OldQuattroProParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/x-quattro-pro",
            "application/x-quattro-pro; version=1-4",
            "application/x-quattro-pro; version=5",
            "application/x-quattro-pro; version=1+5",
            "application/x-quattro-pro; version=6",
        ]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Reads a spreadsheet's cells, and defers one that is a compound document after all, which
    /// `OfficeParser` opens.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        if ole2::is_ole(input) {
            return Err(Decline::Deferred(
                "a compound document, which OfficeParser opens".to_owned(),
            ));
        }
        let layout = Layout::of(input).map_err(|reason| {
            Decline::Fails(Error::ParseError(format!(
                "Cannot read the Quattro Pro spreadsheet: {reason}"
            )))
        })?;
        Ok(Box::new(move |xhtml| {
            let mut unread = None;
            let cells = layout
                .cells(input)
                .map_err(Failure::Malformed)?
                .map_while(|cell| cell.map_err(|reason| unread = Some(reason)).ok());
            write(cells, xhtml)?;
            unread.map_or(Ok(()), |reason| Err(Failure::Malformed(reason)))
        }))
    }
}

/// The releases of Quattro Pro whose records differ, by the version their beginning names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    /// Quattro Pro 1 to 4 for DOS (`.wq1`): a cell's format, then its column and row, as WK1
    /// has them, and text after its length.
    Dos,
    /// Quattro Pro 5 for DOS (`.wq2`): a cell's column, page, row and attributes, and text
    /// after its length.
    Dos5,
    /// Quattro Pro 1 and 5 for Windows (`.wb1`, `.wb2`): a cell's column, page, row and
    /// attributes, and text ended by a zero byte.
    Windows,
}

impl Release {
    fn of(version: u16) -> Option<Self> {
        match version {
            0x5120 => Some(Self::Dos),
            0x5121 => Some(Self::Dos5),
            0x1001 | 0x1002 => Some(Self::Windows),
            _ => None,
        }
    }

    fn charset(self) -> Charset {
        Charset::named(match self {
            Self::Dos | Self::Dos5 => "IBM437",
            Self::Windows => "windows-1252",
        })
    }

    /// The address of the cell a record's `body` holds, and what follows it.
    fn address(self, body: &[u8]) -> Option<(Address, &[u8])> {
        match self {
            Self::Dos => {
                let (head, rest) = body.split_first_chunk::<5>()?;
                let address = Address {
                    page: 0,
                    row: u16::from_le_bytes([head[3], head[4]]),
                    column: u16::from_le_bytes([head[1], head[2]]),
                };
                Some((address, rest))
            }
            Self::Dos5 | Self::Windows => {
                let (head, rest) = body.split_first_chunk::<6>()?;
                let address = Address {
                    page: head[1],
                    row: u16::from_le_bytes([head[2], head[3]]),
                    column: u16::from(head[0]),
                };
                Some((address, rest))
            }
        }
    }

    /// The bytes of a text that starts `data`: after its length, or up to a zero byte.
    fn text(self, data: &[u8]) -> Option<&[u8]> {
        match self {
            Self::Dos | Self::Dos5 => {
                let (&length, rest) = data.split_first()?;
                rest.get(..usize::from(length))
            }
            Self::Windows => Some(
                data.iter()
                    .position(|&byte| byte == 0)
                    .map_or(data, |end| &data[..end]),
            ),
        }
    }
}

/// Where a cell is, in the order the spreadsheet is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Address {
    page: u8,
    row: u16,
    column: u16,
}

/// The release a spreadsheet's beginning names, and its records after it, or why they do not
/// read.
fn release(input: &[u8]) -> Result<(Release, Records<'_>), String> {
    let mut records = Records { input, at: 0 };
    let (kind, body) = records.next()?.ok_or("the file is empty")?;
    let version = match (kind, body) {
        (BOF, [low, high, ..]) => u16::from_le_bytes([*low, *high]),
        _ => return Err(format!("the file begins with a record of type {kind:#06x}")),
    };
    let release = Release::of(version)
        .ok_or_else(|| format!("version {version:#06x} is no Quattro Pro 1 to 6 version"))?;
    Ok((release, records))
}

/// A cell record: where it is, where the record starts, and its text, none for a value that is
/// an error or not available, which takes the place of the cell's earlier value.
type Cell = (Address, usize, Option<String>);

/// A cell that holds a value, as it is written: where it is and its text, or why its record
/// does not read.
type Valued = Result<(Address, String), String>;

/// The cell records of a spreadsheet in the order they come, up to its end, each read whole.
struct Cells<'a> {
    release: Release,
    records: Records<'a>,
    ended: bool,
}

impl<'a> Cells<'a> {
    fn new(input: &'a [u8]) -> Result<Self, String> {
        let (release, records) = release(input)?;
        Ok(Self {
            release,
            records,
            ended: false,
        })
    }

    /// The cell of the record at `start` of `input`, which a reading of it found there.
    fn at(&self, start: usize) -> Result<(Address, String), String> {
        let mut records = Records {
            input: self.records.input,
            at: start,
        };
        let again = || format!("the cell record at byte {start} reads otherwise again");
        let (kind, body) = records.next()?.ok_or_else(again)?;
        match cell_of(self.release, kind, body, records.at)? {
            Some((address, Some(text))) => Ok((address, text)),
            _ => Err(again()),
        }
    }
}

impl Iterator for Cells<'_> {
    type Item = Result<Cell, String>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.ended {
            let start = self.records.at;
            let (kind, body) = match self.records.next() {
                Ok(Some((EOF, _)) | None) => {
                    self.ended = true;
                    return None;
                }
                Ok(Some(record)) => record,
                Err(cut) => {
                    self.ended = true;
                    return Some(Err(cut));
                }
            };
            match cell_of(self.release, kind, body, self.records.at) {
                Ok(Some((address, text))) => return Some(Ok((address, start, text))),
                Ok(None) => {}
                Err(cut) => return Some(Err(cut)),
            }
        }
        None
    }
}

/// The address and text of the cell a record of `kind` with `body`, ending at byte `end`,
/// holds, none for a record of another kind, or why it is cut.
fn cell_of(
    release: Release,
    kind: u16,
    body: &[u8],
    end: usize,
) -> Result<Option<(Address, Option<String>)>, String> {
    if !matches!(kind, INTEGER | NUMBER | LABEL | FORMULA | STRING) {
        return Ok(None);
    }
    let decode = |bytes: &[u8]| {
        release
            .charset()
            .decode(bytes)
            .expect("the code pages of Quattro Pro's systems decode")
    };
    let short = || format!("the cell record of type {kind:#06x} ending at byte {end} is cut");
    let (address, data) = release.address(body).ok_or_else(short)?;
    let text = match kind {
        INTEGER => {
            let value = data.first_chunk::<2>().ok_or_else(short)?;
            Some(i16::from_le_bytes(*value).to_string())
        }
        NUMBER | FORMULA => {
            let value = data.first_chunk::<8>().ok_or_else(short)?;
            number(f64::from_le_bytes(*value))
        }
        LABEL => {
            let text = data
                .split_first()
                .and_then(|(_, text)| release.text(text))
                .ok_or_else(short)?;
            Some(decode(text))
        }
        _ => Some(decode(release.text(data).ok_or_else(short)?)),
    };
    Ok(Some((address, text)))
}

/// Where the cells of a spreadsheet are, once its records read: in the order of their
/// addresses, a later record of a cell's address following the earlier, or out of it, each
/// cell that holds a value then indexed by its address, with where its last record starts.
enum Layout {
    InOrder,
    Indexed(BTreeMap<Address, usize>),
}

impl Layout {
    /// The layout of the cells of `input`, or why its records do not read.
    fn of(input: &[u8]) -> Result<Self, String> {
        let mut last = None;
        for cell in Cells::new(input)? {
            let (address, _, _) = cell?;
            if last.is_some_and(|last| address < last) {
                let mut index = BTreeMap::new();
                for cell in Cells::new(input)? {
                    match cell? {
                        (address, start, Some(_)) => index.insert(address, start),
                        (address, _, None) => index.remove(&address),
                    };
                }
                return Ok(Self::Indexed(index));
            }
            last = Some(address);
        }
        Ok(Self::InOrder)
    }

    /// The cells of `input` that hold a value, in the order of their addresses, read again.
    fn cells<'a>(
        &'a self,
        input: &'a [u8],
    ) -> Result<Box<dyn Iterator<Item = Valued> + 'a>, String> {
        let cells = Cells::new(input)?;
        Ok(match self {
            Self::InOrder => Box::new(InOrder {
                cells,
                pending: None,
            }),
            Self::Indexed(index) => Box::new(index.values().map(move |&start| cells.at(start))),
        })
    }
}

/// The cells of records in the order of their addresses, each the last record of its address,
/// those without a value left out.
struct InOrder<'a> {
    cells: Cells<'a>,
    pending: Option<(Address, Option<String>)>,
}

impl Iterator for InOrder<'_> {
    type Item = Valued;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.cells.next() {
                Some(Ok((address, _, text))) => {
                    let earlier = self.pending.replace((address, text));
                    if let Some((at, Some(text))) = earlier.filter(|(at, _)| *at != address) {
                        return Some(Ok((at, text)));
                    }
                }
                Some(Err(cut)) => return Some(Err(cut)),
                None => {
                    if let (at, Some(text)) = self.pending.take()? {
                        return Some(Ok((at, text)));
                    }
                }
            }
        }
    }
}

/// The text of a number: a whole number below 10^15 without a fraction, others as Java's
/// `Double.toString` writes them, and none for an error or a value that is not available.
fn number(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    Some(if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        java::dtoa::java_double_to_string(value)
    })
}

/// The records of a file: each a type and a length, then a body that long.
struct Records<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> Records<'a> {
    /// The next record's type and body, none at the end of the file, or why it is cut.
    fn next(&mut self) -> Result<Option<(u16, &'a [u8])>, String> {
        let rest = &self.input[self.at..];
        if rest.is_empty() {
            return Ok(None);
        }
        let cut = || format!("the record at byte {} is cut", self.at);
        let (head, rest) = rest.split_first_chunk::<4>().ok_or_else(cut)?;
        let kind = u16::from_le_bytes([head[0], head[1]]);
        let length = usize::from(u16::from_le_bytes([head[2], head[3]]));
        let body = rest.get(..length).ok_or_else(cut)?;
        self.at += 4 + length;
        Ok(Some((kind, body)))
    }
}

/// Each page as a table of its rows, of `cells` in the order of their addresses.
fn write(cells: impl Iterator<Item = (Address, String)>, xhtml: &mut Xhtml) -> Result<(), Stop> {
    let mut cells = cells.peekable();
    while let Some(&(first, _)) = cells.peek() {
        xhtml.start_element_with("div", &[("class", "page")])?;
        xhtml.start_element("table")?;
        xhtml.start_element("tbody")?;
        while let Some(&(row, _)) = cells.peek() {
            if row.page != first.page {
                break;
            }
            xhtml.start_element("tr")?;
            let mut column = 0;
            while let Some((address, text)) =
                cells.next_if(|(address, _)| address.page == row.page && address.row == row.row)
            {
                for _ in column..address.column {
                    cell(xhtml, "")?;
                }
                cell(xhtml, &text)?;
                column = address.column + 1;
            }
            xhtml.end_element("tr")?;
        }
        xhtml.end_element("tbody")?;
        xhtml.end_element("table")?;
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// A cell of a row, empty or holding `text`.
fn cell(xhtml: &mut Xhtml, text: &str) -> Result<(), Stop> {
    xhtml.start_element("td")?;
    xhtml.characters(text)?;
    xhtml.end_element("td")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_numbers_by_their_value() {
        assert_eq!(number(30.0).as_deref(), Some("30"));
        assert_eq!(number(-2.5).as_deref(), Some("-2.5"));
        assert_eq!(number(682.33).as_deref(), Some("682.33"));
        assert_eq!(number(1.5e-15).as_deref(), Some("1.5E-15"));
        assert_eq!(number(1e20).as_deref(), Some("1.0E20"));
        assert_eq!(number(f64::NAN), None);
    }
}
