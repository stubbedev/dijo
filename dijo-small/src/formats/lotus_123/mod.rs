//! Lotus 1-2-3 spreadsheets, which Tika does not read, read for their cells as the xls parser
//! writes a workbook's ([`SheetTable`]): each sheet a `<div class="page">`, headed by its name,
//! of a table of its rows, a cell a `<td>` at its column. Labels are written without their
//! alignment prefix, numbers in the format 1-2-3 shows them in (see [`format`]), and formulas by
//! the result the file keeps of them, which is not computed again; `NA` and `ERR` are written as
//! 1-2-3 shows them. A cell's comment follows what the cell shows, after a line break.
//!
//! The records ([`reader`]) are those LibreOffice's, libwps' and Gnumeric's Lotus imports read:
//! the worksheets of 1-2-3 Release 1 and 2 (`.wks`, `.wk1`), one sheet of no name in their
//! formats; and the workbooks of Release 3, 4 and 5 (`.wk3`, `.wk4`) and of 1-2-3 97 and
//! Millennium (`.123`), their sheets named by their records or else by their letters, as 1-2-3
//! names them. The cells of Release 3 to 5 are written in the formats of their rows, those of
//! 1-2-3 97 and later in those of their formats database ([`database`]). Range names are not
//! written, as no spreadsheet parser writes a workbook's names, and neither is the document
//! information after the EOF record of a `.123` file, whose records no reference describes. Text
//! is decoded from the Lotus Multi-Byte Character Set ([`lmbcs`]): Release 1 and 2 keep it in
//! its group 1, code page 850, as Gnumeric reads them, and later releases in the group their
//! BOF record names. A file protected by a password fails as encrypted.

mod database;
mod format;
mod lmbcs;
mod number;
mod reader;

use std::borrow::Cow;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::crypt::encrypted_document;
use crate::formats::data_format::{DataFormatter, FormatError};
use crate::formats::metadata::TikaMetadata;
use crate::formats::sheet_table::SheetTable;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::java::trim;
use format::{Shown, Style};
use reader::{Checked, Generation, Value, Workbook};

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
    for (&number, sheet) in &workbook.sheets {
        let name = match workbook.generation {
            Generation::Release2 => None,
            Generation::Release3 { .. } => Some(
                sheet
                    .name
                    .as_deref()
                    .map_or_else(|| Cow::Owned(letters(number)), Cow::Borrowed),
            ),
        };
        let mut table = SheetTable::start(name.as_deref(), false, xhtml)?;
        for (&place, cell) in &sheet.cells {
            let style = workbook.style(number, place, cell);
            let shown = match &cell.value {
                Some(value) => text(value, style, &mut formatter)?,
                None => None,
            };
            if shown.is_none() && cell.comment.is_none() {
                continue;
            }
            table.enter((i32::from(place.0), i32::from(place.1)), xhtml)?;
            if let Some(shown) = &shown {
                xhtml.characters(shown)?;
            }
            if let Some(comment) = &cell.comment {
                if shown.is_some() {
                    xhtml.start_element("br")?;
                    xhtml.end_element("br")?;
                }
                xhtml.characters(comment)?;
            }
        }
        table.end(xhtml)?;
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// The text of `value`, a number shown in `style`, or General without one; `None` for a value
/// that shows nothing.
fn text(
    value: &Value,
    style: Option<Style>,
    formatter: &mut DataFormatter,
) -> Result<Option<String>, Failure> {
    let value = match value {
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
