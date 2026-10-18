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

use std::collections::BTreeMap;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Parser, Prepared};
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
        let cells = read(input).map_err(|reason| {
            Decline::Fails(Error::ParseError(format!(
                "Cannot read the Quattro Pro spreadsheet: {reason}"
            )))
        })?;
        Ok(Box::new(move |xhtml| Ok(write(&cells, xhtml)?)))
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

/// The text of each cell that holds a value, in the order of their addresses, or why the
/// records do not read.
fn read(input: &[u8]) -> Result<BTreeMap<Address, String>, String> {
    let mut records = Records { input, at: 0 };
    let (kind, body) = records.next()?.ok_or("the file is empty")?;
    let version = match (kind, body) {
        (BOF, [low, high, ..]) => u16::from_le_bytes([*low, *high]),
        _ => return Err(format!("the file begins with a record of type {kind:#06x}")),
    };
    let release = Release::of(version)
        .ok_or_else(|| format!("version {version:#06x} is no Quattro Pro 1 to 6 version"))?;
    let charset = release.charset();
    let decode = |bytes: &[u8]| {
        charset
            .decode(bytes)
            .expect("the code pages of Quattro Pro's systems decode")
    };
    let mut cells = BTreeMap::new();
    while let Some((kind, body)) = records.next()? {
        if kind == EOF {
            break;
        }
        if !matches!(kind, INTEGER | NUMBER | LABEL | FORMULA | STRING) {
            continue;
        }
        let at = records.at;
        let short = || format!("the cell record of type {kind:#06x} ending at byte {at} is cut");
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
        match text {
            Some(text) => cells.insert(address, text),
            None => cells.remove(&address),
        };
    }
    Ok(cells)
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

/// Each page as a table of its rows.
fn write(cells: &BTreeMap<Address, String>, xhtml: &mut Xhtml) -> Result<(), Stop> {
    let mut cells = cells.iter().peekable();
    while let Some(&(&first, _)) = cells.peek() {
        xhtml.start_element_with("div", &[("class", "page")])?;
        xhtml.start_element("table")?;
        xhtml.start_element("tbody")?;
        while let Some(&(&row, _)) = cells.peek() {
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
                cell(xhtml, text)?;
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
