//! The topic records of `|TOPIC`, written as they come: a topic header's title as a heading,
//! and the paragraphs of text records and the rows of table records.
//!
//! The first part of a text or table record holds the formatting of its paragraphs and the
//! commands between the strings of its text, which its second part holds NUL-terminated: before
//! each command the next string is written. A command ends a paragraph, breaks a line, moves
//! to the next tab stop, writes a non-breaking space, or sets a font, starts or ends a hotspot,
//! places a picture or an embedded window, which write nothing. A table record is a row of a
//! table, and consecutive table records are the rows of one table: each of its paragraphs
//! starts with the column of the cell it is in.

use super::phrases::{Expanded, Phrases};
use super::reader::Reader;
use super::topic::Link;
use super::Malformed;
use crate::formats::xhtml::Xhtml;
use crate::formats::Failure;
use crate::text::charset::Charset;

const TOPIC_HEADER: u8 = 0x02;
const DISPLAY_30: u8 = 0x01;
const DISPLAY: u8 = 0x20;
const TABLE: u8 = 0x23;

const END: u8 = 0xFF;
const FIELD: u8 = 0x20;
const DTYPE: u8 = 0x21;
const FONT: u8 = 0x80;
const LINE_BREAK: u8 = 0x81;
const END_OF_PARAGRAPH: u8 = 0x82;
const TAB: u8 = 0x83;
const PICTURE_CENTRED: u8 = 0x86;
const PICTURE_RIGHT: u8 = 0x88;
const END_OF_HOTSPOT: u8 = 0x89;
const NON_BREAKING_SPACE: u8 = 0x8B;
const NON_BREAKING_HYPHEN: u8 = 0x8C;
const MACRO: u8 = 0xC8;
const MACRO_WITHOUT_FONT: u8 = 0xCC;
const POPUP_30: u8 = 0xE0;
const JUMP_31: u8 = 0xE3;
const POPUP_WITHOUT_FONT: u8 = 0xE6;
const JUMP_WITHOUT_FONT: u8 = 0xE7;
const EXTERNAL_POPUP: u8 = 0xEA;
const EXTERNAL_POPUP_WITHOUT_FONT: u8 = 0xEB;
const EXTERNAL_JUMP: u8 = 0xEE;
const EXTERNAL_JUMP_WITHOUT_FONT: u8 = 0xEF;
/// The picture type of WinHelp 3.1, which counts the hotspots of a picture before it.
const PICTURE_WITH_HOTSPOTS: u8 = 0x22;
/// The column that ends the paragraphs of a table record.
const END_OF_ROW: i16 = -1;
/// The bytes of a string written at a time at least, so that a long string is not held whole.
const PIECE: usize = 0x10000;

/// The paragraph format's flags of what follows it.
const UNKNOWN_FOLLOWS: u16 = 0x0001;
const SPACINGS_AND_INDENTS: [u16; 6] = [0x0002, 0x0004, 0x0008, 0x0010, 0x0020, 0x0040];
const BORDER_FOLLOWS: u16 = 0x0100;
const TABS_FOLLOW: u16 = 0x0200;
const TAB_TYPE_FOLLOWS: u16 = 0x4000;

/// Writes the topic records of a help file.
pub(super) struct Writer<'w, 'x> {
    xhtml: &'w mut Xhtml<'x>,
    charset: Charset,
    phrases: &'w Phrases<'w>,
    /// The element text is being written into, a heading or a paragraph.
    open: Option<&'static str>,
    /// The row of a table record being written.
    row: Option<Row>,
    /// Whether a table is open, which a record other than a table record ends.
    table: bool,
    piece: Vec<u8>,
}

/// A row being written: its columns, the column of the next cell, and whether a cell is open.
struct Row {
    columns: usize,
    next: usize,
    cell: bool,
}

impl<'w, 'x> Writer<'w, 'x> {
    pub(super) fn new(
        xhtml: &'w mut Xhtml<'x>,
        charset: Charset,
        phrases: &'w Phrases<'w>,
    ) -> Self {
        Self {
            xhtml,
            charset,
            phrases,
            open: None,
            row: None,
            table: false,
            piece: Vec::new(),
        }
    }

    /// Writes the topic record `link`; records of other types write nothing.
    pub(super) fn write(&mut self, link: &Link) -> Result<(), Failure> {
        match link.record_type {
            TOPIC_HEADER => {
                self.end_table()?;
                let mut text = Expanded::new(self.phrases, &link.data2, link.text_length);
                self.write_string(&mut text, "h1")?;
                self.end_block()
            }
            DISPLAY_30 | DISPLAY => {
                self.end_table()?;
                self.write_display(link)
            }
            TABLE => self.write_row(link),
            _ => Ok(()),
        }
    }

    /// Ends what is open after the last record.
    pub(super) fn finish(mut self) -> Result<(), Failure> {
        self.end_block()?;
        self.end_table()
    }

    fn write_display(&mut self, link: &Link) -> Result<(), Failure> {
        let mut data = Reader::new(&link.data1, "a text record");
        data.compressed_i32()?;
        if link.record_type == DISPLAY {
            data.compressed_u16()?;
        }
        if data.is_empty() {
            return Ok(());
        }
        let mut text = Expanded::new(self.phrases, &link.data2, link.text_length);
        paragraph_format(&mut data)?;
        self.write_commands(&mut data, &mut text)
    }

    fn write_row(&mut self, link: &Link) -> Result<(), Failure> {
        let mut data = Reader::new(&link.data1, "a table record");
        data.compressed_i32()?;
        data.compressed_u16()?;
        let columns = usize::from(data.u8()?);
        let table_type = data.u8()?;
        if matches!(table_type, 0 | 2) {
            data.skip(2)?;
        }
        data.skip(4 * columns)?;
        if !std::mem::replace(&mut self.table, true) {
            self.xhtml.start_element("table")?;
        }
        self.xhtml.start_element("tr")?;
        self.row = Some(Row {
            columns,
            next: 0,
            cell: false,
        });
        let mut text = Expanded::new(self.phrases, &link.data2, link.text_length);
        while !data.is_empty() {
            let column = data.i16()?;
            if column == END_OF_ROW {
                break;
            }
            data.skip(3)?;
            self.enter_cell(column)?;
            paragraph_format(&mut data)?;
            self.write_commands(&mut data, &mut text)?;
        }
        self.end_row()
    }

    /// Opens the cell of `column` unless it is open, after empty cells of the columns before
    /// it the row skips.
    fn enter_cell(&mut self, column: i16) -> Result<(), Failure> {
        let column = usize::try_from(column)
            .map_err(|_| Malformed(format!("a table paragraph is in the column {column}")))?;
        let row = self.row.as_mut().expect("a row is being written");
        if row.cell && row.next == column + 1 {
            return Ok(());
        }
        self.end_block()?;
        let row = self.row.as_mut().expect("a row is being written");
        if std::mem::take(&mut row.cell) {
            self.xhtml.end_element("td")?;
        }
        while row.next < column {
            self.xhtml.start_element("td")?;
            self.xhtml.end_element("td")?;
            row.next += 1;
        }
        self.xhtml.start_element("td")?;
        row.cell = true;
        row.next = row.next.max(column + 1);
        Ok(())
    }

    fn end_row(&mut self) -> Result<(), Failure> {
        self.end_block()?;
        let Some(mut row) = self.row.take() else {
            return Ok(());
        };
        if row.cell {
            self.xhtml.end_element("td")?;
        }
        while row.next < row.columns {
            self.xhtml.start_element("td")?;
            self.xhtml.end_element("td")?;
            row.next += 1;
        }
        self.xhtml.end_element("tr")?;
        Ok(())
    }

    fn end_table(&mut self) -> Result<(), Failure> {
        if std::mem::take(&mut self.table) {
            self.xhtml.end_element("table")?;
        }
        Ok(())
    }

    /// Writes the strings of `text` and follows the commands of `data` between them, up to
    /// the command that ends them.
    fn write_commands(
        &mut self,
        data: &mut Reader<'_>,
        text: &mut Expanded<'_>,
    ) -> Result<(), Failure> {
        loop {
            self.write_string(text, "p")?;
            match data.u8()? {
                END => return self.end_block(),
                FIELD => data.skip(4)?,
                DTYPE | FONT => data.skip(2)?,
                LINE_BREAK => {
                    self.begin("p")?;
                    self.xhtml.start_element("br")?;
                    self.xhtml.end_element("br")?;
                }
                END_OF_PARAGRAPH => self.end_block()?,
                TAB => self.characters("\t")?,
                NON_BREAKING_SPACE => self.characters("\u{a0}")?,
                END_OF_HOTSPOT | NON_BREAKING_HYPHEN => {}
                PICTURE_CENTRED..=PICTURE_RIGHT => {
                    let picture_type = data.u8()?;
                    let size = data.compressed_i32()?;
                    if picture_type == PICTURE_WITH_HOTSPOTS {
                        data.compressed_u16()?;
                    }
                    data.skip(usize::try_from(size).map_err(|_| {
                        Malformed(format!("a picture of {size} bytes in a text record"))
                    })?)?;
                }
                MACRO
                | MACRO_WITHOUT_FONT
                | EXTERNAL_POPUP
                | EXTERNAL_POPUP_WITHOUT_FONT
                | EXTERNAL_JUMP
                | EXTERNAL_JUMP_WITHOUT_FONT => {
                    let size = data.i16()?;
                    data.skip(usize::try_from(size).map_err(|_| {
                        Malformed(format!("a hotspot of {size} bytes in a text record"))
                    })?)?;
                }
                POPUP_30..=JUMP_31 | POPUP_WITHOUT_FONT | JUMP_WITHOUT_FONT => data.skip(4)?,
                _ => {}
            }
        }
    }

    /// Writes the next string of `text` into the element `into`, which it opens unless an
    /// element is open, a piece at a time: a piece ends after a byte below 0x80, which ends a
    /// character in every code page a help file's text is in.
    fn write_string(&mut self, text: &mut Expanded<'_>, into: &'static str) -> Result<(), Failure> {
        while let Some(byte) = text.next_byte()? {
            if byte == 0 {
                break;
            }
            self.piece.push(byte);
            if self.piece.len() >= PIECE && byte < 0x80 {
                self.flush(into)?;
            }
        }
        self.flush(into)
    }

    fn flush(&mut self, into: &'static str) -> Result<(), Failure> {
        if self.piece.is_empty() {
            return Ok(());
        }
        let text = self
            .charset
            .decode(&self.piece)
            .expect("the charset of a help file decodes");
        self.piece.clear();
        self.begin(into)?;
        self.xhtml.characters(&text)?;
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.begin("p")?;
        self.xhtml.characters(text)?;
        Ok(())
    }

    /// Opens the element `name` unless an element is open.
    fn begin(&mut self, name: &'static str) -> Result<(), Failure> {
        if self.open.is_none() {
            self.xhtml.start_element(name)?;
            self.open = Some(name);
        }
        Ok(())
    }

    fn end_block(&mut self) -> Result<(), Failure> {
        if let Some(name) = self.open.take() {
            self.xhtml.end_element(name)?;
        }
        Ok(())
    }
}

/// Reads past the paragraph format at the start of the paragraphs of a record: four bytes, the
/// flags of what follows, an unknown long, spacings and indents, a border and tab stops.
fn paragraph_format(data: &mut Reader<'_>) -> Result<(), Malformed> {
    data.skip(4)?;
    let flags = data.u16()?;
    if flags & UNKNOWN_FOLLOWS != 0 {
        data.compressed_i32()?;
    }
    for flag in SPACINGS_AND_INDENTS {
        if flags & flag != 0 {
            data.compressed_i16()?;
        }
    }
    if flags & BORDER_FOLLOWS != 0 {
        data.skip(3)?;
    }
    if flags & TABS_FOLLOW != 0 {
        for _ in 0..data.compressed_i16()?.max(0) {
            if data.compressed_u16()? & TAB_TYPE_FOLLOWS != 0 {
                data.compressed_u16()?;
            }
        }
    }
    Ok(())
}
