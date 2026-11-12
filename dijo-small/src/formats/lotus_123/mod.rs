//! Lotus 1-2-3 spreadsheets, which Tika does not read, read for their cells as the spreadsheet
//! parsers write them: each sheet a `<div class="page">`, headed by its name, of a table of its
//! rows, a cell a `<td>` at its column. Labels are written without their alignment prefix,
//! numbers in the format 1-2-3 shows them in (see [`format`]), and formulas by the result the file
//! keeps of them, which is not computed again; `NA` and `ERR` are written as 1-2-3 shows them.
//!
//! The records ([`reader`]) are those LibreOffice's and libwps' Lotus imports read: the
//! worksheets of 1-2-3 Release 1 and 2 (`.wks`, `.wk1`), one sheet of no name in their formats;
//! and the workbooks of Release 3, 4 and 5 (`.wk3`, `.wk4`) and of 1-2-3 97 and Millennium
//! (`.123`), their sheets named by their records or else by their letters, as 1-2-3 names them.
//! The cells of Release 3 to 5 are written in the formats of their rows; those of 1-2-3 97 and
//! later, whose formats are kept in style databases this parser does not read, in General. Text
//! is decoded from the Lotus Multi-Byte Character Set ([`lmbcs`]): Release 1 and 2 keep it in
//! its group 1, code page 850, as Gnumeric reads them, and later releases in the group their
//! BOF record names. A file protected by a password fails as encrypted.

mod format;
mod lmbcs;
mod number;
mod reader;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::crypt::encrypted_document;
use crate::formats::data_format::{DataFormatter, FormatError};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::java::trim;
use format::{Shown, Style};
use reader::{Cell, Checked, Generation, Value, Workbook};

const MEDIA_TYPE: &str = "application/vnd.lotus-1-2-3";
/// The index the formatter is given a format by: Excel's first of a workbook's own formats.
const CUSTOM_FORMAT: i32 = 164;

pub struct Lotus123Parser;

impl Parser for Lotus123Parser {
    fn name(&self) -> &'static str {
        "dijo::Lotus123Parser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::Lotus123Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// The release subtypes Tika's detection names, as `version=2`, have no parser of their own.
    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails a file that is no 1-2-3 file, and one a password protects, as encrypted, before
    /// anything is written; the cells are read as the document is written.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let generation = match reader::check(input) {
            Ok(Checked::Readable(generation)) => generation,
            Ok(Checked::Encrypted) => {
                return Err(Decline::Fails(encrypted_document(
                    "a Lotus 1-2-3 file protected by a password",
                )))
            }
            Err(reason) => {
                return Err(Decline::Fails(Error::ParseError(format!(
                    "Lotus 1-2-3 file: {reason}"
                ))))
            }
        };
        Ok(Box::new(move |xhtml| {
            let workbook = reader::read(input, generation)?;
            write(&workbook, xhtml)
        }))
    }
}

/// Writes the sheets of `workbook` in their order, each its rows of cells.
fn write(workbook: &Workbook, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut formatter = DataFormatter::new(false);
    let mut row = Vec::new();
    for (&number, sheet) in &workbook.sheets {
        xhtml.start_element_with("div", &[("class", "page")])?;
        if let Generation::Release3 { .. } = workbook.generation {
            match &sheet.name {
                Some(name) => xhtml.element("h1", name)?,
                None => xhtml.element("h1", &letters(number))?,
            }
        }
        xhtml.start_element("table")?;
        xhtml.start_element("tbody")?;
        let mut current = None;
        for (&place, cell) in &sheet.cells {
            if current != Some(place.0) {
                write_row(&row, xhtml)?;
                row.clear();
                current = Some(place.0);
            }
            let style = workbook.style(number, place, cell);
            if let Some(text) = text(cell, style, &mut formatter)? {
                row.push((place.1, text));
            }
        }
        write_row(&row, xhtml)?;
        row.clear();
        xhtml.end_element("tbody")?;
        xhtml.end_element("table")?;
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// Writes a row of the texts of its cells by column, an empty cell in each column between them;
/// nothing of a row of no text.
fn write_row(texts: &[(u8, String)], xhtml: &mut Xhtml) -> Result<(), Failure> {
    if texts.is_empty() {
        return Ok(());
    }
    xhtml.start_element("tr")?;
    let mut next = 0;
    for (column, text) in texts {
        for _ in next..*column {
            xhtml.start_element("td")?;
            xhtml.end_element("td")?;
        }
        xhtml.element("td", text)?;
        next = column + 1;
    }
    xhtml.end_element("tr")?;
    Ok(())
}

/// The text of `cell`, a number shown in `style`, or General without one; `None` for a cell that
/// shows nothing.
fn text(
    cell: &Cell,
    style: Option<Style>,
    formatter: &mut DataFormatter,
) -> Result<Option<String>, Failure> {
    let value = match &cell.value {
        Value::Text(text) => {
            let text = trim(text);
            return Ok((!text.is_empty()).then(|| text.to_owned()));
        }
        Value::NotAvailable => return Ok(Some("NA".to_owned())),
        Value::Error => return Ok(Some("ERR".to_owned())),
        Value::Number(value) => *value,
    };
    let shown = style.map_or_else(Shown::general, Style::shown);
    let Some(format) = shown.excel_format(value) else {
        return Ok(None);
    };
    let text = formatter
        .format_raw(value, CUSTOM_FORMAT, format)
        .map_err(|error| {
            Failure::Malformed(format!(
                "the number {value} in the format {format}: {}",
                match error {
                    FormatError::Thrown(reason) => reason,
                    FormatError::Unported(what) => what.to_owned(),
                }
            ))
        })?;
    Ok(Some(shown.enclose(text)))
}

/// The letters 1-2-3 names sheet `number` by: `A` to `Z`, then `AA` to `IV`.
fn letters(number: u8) -> String {
    let letter = |index: u8| char::from(b'A' + index);
    match number.checked_sub(26) {
        None => letter(number).to_string(),
        Some(beyond) => [letter(beyond / 26), letter(beyond % 26)].iter().collect(),
    }
}

#[cfg(test)]
mod tests;
