//! Rich text, an item of CD records. A record starts with its signature byte and its length,
//! which counts its header: one byte more for a byte length, 0xFF and a 16-bit length, or 0x00
//! and a 32-bit one; the next record starts on an even byte. A paragraph record starts a
//! paragraph, and a text record, after its font, holds a run of the paragraph's text; a table is
//! a table record, a cell record of each cell, whose first byte is its row, and a table end
//! record, with the paragraphs of the cells between them. The other records, of formatting,
//! pictures, links, fields and the like, write nothing.

use super::note::write_lines;
use super::{span, u16_at, u32_at, Problem};
use crate::formats::xhtml::Xhtml;
use crate::formats::Failure;

const PARAGRAPH: u8 = 0x81;
const TEXT: u8 = 0x85;
const BIDI_TEXT: u8 = 0xE4;
const TABLE_BEGIN: u8 = 0xA3;
const TABLE_CELL: u8 = 0xA4;
const TABLE_END: u8 = 0xA5;
const NESTED_TABLE_BEGIN: u8 = 0xCF;
const NESTED_TABLE_CELL: u8 = 0xD0;
const NESTED_TABLE_END: u8 = 0xD1;

const WORD_LENGTH: u8 = 0xFF;
const LONG_LENGTH: u8 = 0x00;
const FONT_ID_SIZE: usize = 4;

/// A CD record: its signature and what follows its header.
struct Record<'a> {
    signature: u8,
    body: &'a [u8],
}

/// The record at `at` of `value`, and its length.
fn record(value: &[u8], at: usize) -> Result<(Record<'_>, usize), Problem> {
    let what = "a rich-text record";
    let head = span(value, at, 2, what)?;
    let (header, length) = match head[1] {
        WORD_LENGTH => (4, usize::from(u16_at(value, at + 2, what)?)),
        LONG_LENGTH => (6, u32_at(value, at + 2, what)? as usize),
        length => (2, usize::from(length)),
    };
    if length < header {
        return Err(Problem::Malformed(format!(
            "a rich-text record of {length} bytes is shorter than its header"
        )));
    }
    let record = span(value, at, length, what)?;
    Ok((
        Record {
            signature: head[0],
            body: &record[header..],
        },
        length,
    ))
}

/// The records of `value`, up to the first that is not well-formed.
fn records(value: &[u8]) -> impl Iterator<Item = Result<Record<'_>, Problem>> {
    let mut at = 0;
    let mut failed = false;
    std::iter::from_fn(move || {
        if failed || at >= value.len() {
            return None;
        }
        Some(match record(value, at) {
            Ok((record, length)) => {
                at += length + length % 2;
                Ok(record)
            }
            Err(problem) => {
                failed = true;
                Err(problem)
            }
        })
    })
}

/// A table being written: the row of its last cell, and whether a cell is open.
struct Table {
    row: Option<u8>,
    cell: bool,
}

/// Writes the rich text `value`: its paragraphs and tables.
pub(super) fn write(value: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut paragraph = false;
    let mut tables: Vec<Table> = Vec::new();
    for record in records(value) {
        let record = record?;
        match record.signature {
            PARAGRAPH => end_paragraph(&mut paragraph, xhtml)?,
            TEXT | BIDI_TEXT => {
                let text = record.body.get(FONT_ID_SIZE..).ok_or_else(|| {
                    Problem::Malformed("a text record shorter than its font".to_owned())
                })?;
                if text.is_empty() {
                    continue;
                }
                if !paragraph {
                    xhtml.start_element("p")?;
                    paragraph = true;
                }
                write_lines(text, xhtml)?;
            }
            TABLE_BEGIN | NESTED_TABLE_BEGIN => {
                end_paragraph(&mut paragraph, xhtml)?;
                xhtml.start_element("table")?;
                tables.push(Table {
                    row: None,
                    cell: false,
                });
            }
            TABLE_CELL | NESTED_TABLE_CELL => {
                end_paragraph(&mut paragraph, xhtml)?;
                let row = *record.body.first().ok_or_else(|| {
                    Problem::Malformed("a table cell record without its row".to_owned())
                })?;
                let table = tables.last_mut().ok_or_else(|| {
                    Problem::Malformed("a table cell record outside a table".to_owned())
                })?;
                if table.cell {
                    xhtml.end_element("td")?;
                }
                if table.row != Some(row) {
                    if table.row.is_some() {
                        xhtml.end_element("tr")?;
                    }
                    xhtml.start_element("tr")?;
                    table.row = Some(row);
                }
                xhtml.start_element("td")?;
                table.cell = true;
            }
            TABLE_END | NESTED_TABLE_END => {
                end_paragraph(&mut paragraph, xhtml)?;
                let table = tables.pop().ok_or_else(|| {
                    Problem::Malformed("a table end record outside a table".to_owned())
                })?;
                end_table(table, xhtml)?;
            }
            _ => {}
        }
    }
    end_paragraph(&mut paragraph, xhtml)?;
    while let Some(table) = tables.pop() {
        end_table(table, xhtml)?;
    }
    Ok(())
}

fn end_paragraph(paragraph: &mut bool, xhtml: &mut Xhtml) -> Result<(), Failure> {
    if std::mem::take(paragraph) {
        xhtml.end_element("p")?;
    }
    Ok(())
}

fn end_table(table: Table, xhtml: &mut Xhtml) -> Result<(), Failure> {
    if table.cell {
        xhtml.end_element("td")?;
    }
    if table.row.is_some() {
        xhtml.end_element("tr")?;
    }
    xhtml.end_element("table")?;
    Ok(())
}
