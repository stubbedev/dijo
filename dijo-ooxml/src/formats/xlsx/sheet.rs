//! A worksheet as Tika reads it: `TikaSheetXMLHandler`, POI's event-based sheet reader adapted to
//! Tika's style, string and comment shims, behind `XSSFSheetInterestingPartsCapturer`, which notes
//! hidden rows and columns, writing rows and cells as `SheetTextAsHTML` does. The capturer's
//! note of protection is read ahead, by [`super::handlers::Protection`].

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::VecDeque;

use super::cell::{reference_column, CellAddress};
use super::parts::{Comment, Comments, SharedStrings, Styles};
use super::SPREADSHEETML_NS;
use crate::formats::data_format::{DataFormatter, FormatError};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{attribute_named, Attribute, SaxHandler, StartTag};
use crate::formats::Failure;
use crate::java::{parse_double, parse_int, trim};
use crate::Error;

/// What a cell holds, by its `t` attribute and whether it has a formula.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataType {
    Boolean,
    Error,
    Formula,
    InlineString,
    SharedString,
    Number,
}

/// Where a check for comments on empty cells happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommentCheck {
    Cell,
    EndOfRow,
    EndOfSheetData,
}

/// The settings of `OfficeParserConfig` a sheet is written under.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SheetSettings {
    pub(crate) headers_and_footers: bool,
    pub(crate) missing_rows: bool,
}

/// What `XSSFSheetInterestingPartsCapturer` noted about a sheet.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Captured {
    pub(crate) hidden_row: bool,
    pub(crate) hidden_column: bool,
}

/// `SheetTextAsHTML`: rows and cells as table rows and cells, the headers and footers kept for
/// after the table.
pub(crate) struct SheetOutput {
    settings: SheetSettings,
    pub(crate) headers: Vec<String>,
    pub(crate) footers: Vec<String>,
    last_seen_row: i32,
    last_seen_column: i32,
    row_open: bool,
}

impl SheetOutput {
    pub(crate) fn new(settings: SheetSettings) -> Self {
        Self {
            settings,
            headers: Vec::new(),
            footers: Vec::new(),
            last_seen_row: -1,
            last_seen_column: -1,
            row_open: false,
        }
    }

    /// `startRow`: with missing rows asked for, an empty row for each row missing between the
    /// last row written and this one. Tika never updates its `lastSeenRow`, so each row is
    /// preceded by an empty row for every row before it (#93).
    pub(crate) fn start_row(&mut self, row: i32, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let missing = row.saturating_sub(self.last_seen_row.saturating_add(1));
        self.last_seen_row = self.last_seen_row.max(row);
        if self.settings.missing_rows {
            for _ in 0..missing.max(0) {
                xhtml.start_element("tr")?;
                xhtml.start_element("td")?;
                xhtml.end_element("td")?;
                xhtml.end_element("tr")?;
            }
        }
        xhtml.start_element("tr")?;
        self.row_open = true;
        self.last_seen_column = -1;
        Ok(())
    }

    /// `endRow`.
    pub(crate) fn end_row(&mut self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        xhtml.end_element("tr")?;
        self.row_open = false;
        Ok(())
    }

    /// `closeAnyPending`: ends the row a sheet whose XML failed left open. A cell is written
    /// whole, so none is left open.
    pub(crate) fn close_any_pending(&mut self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        if self.row_open {
            self.end_row(xhtml)?;
        }
        Ok(())
    }

    pub(crate) fn cell(
        &mut self,
        reference: Option<&str>,
        value: Option<&str>,
        comment: Option<&Comment>,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let column = match reference {
            None => self.last_seen_column.wrapping_add(1),
            Some(reference) => reference_column(reference)?,
        };
        let mut gap = self.last_seen_column.wrapping_add(1);
        while gap < column {
            xhtml.start_element("td")?;
            xhtml.end_element("td")?;
            gap += 1;
        }
        self.last_seen_column = column;
        xhtml.start_element("td")?;
        if let Some(value) = value {
            xhtml.characters(value)?;
        }
        if let Some(comment) = comment {
            xhtml.start_element("br")?;
            xhtml.end_element("br")?;
            xhtml.characters(&comment.author)?;
            xhtml.characters(": ")?;
            xhtml.characters(&comment.text)?;
        }
        xhtml.end_element("td")?;
        Ok(())
    }

    pub(crate) fn header_footer(&mut self, text: String, header: bool) {
        if !self.settings.headers_and_footers {
            return;
        }
        if header {
            self.headers.push(text);
        } else {
            self.footers.push(text);
        }
    }
}

/// `TikaSheetXMLHandler` behind `XSSFSheetInterestingPartsCapturer`, writing to `xhtml`.
pub(crate) struct SheetReader<'r, 'x, 'h> {
    pub(crate) captured: Captured,
    styles: Option<&'r Styles>,
    comments: Option<&'r Comments>,
    strings: Option<&'r SharedStrings>,
    formatter: &'r mut DataFormatter,
    output: &'r mut SheetOutput,
    xhtml: &'x mut Xhtml<'h>,
    value_open: bool,
    inline_open: bool,
    header_footer_open: bool,
    data_type: DataType,
    format_index: i16,
    format_string: Option<&'r str>,
    row: i32,
    next_row: i32,
    has_reference: bool,
    reference: String,
    value: String,
    header_footer: String,
    comment_cells: VecDeque<CellAddress>,
}

const HEADERS: [&str; 3] = ["oddHeader", "evenHeader", "firstHeader"];
const FOOTERS: [&str; 3] = ["oddFooter", "evenFooter", "firstFooter"];

/// A `RuntimeException` of the sheet's handler, which fails Tika's parse of the workbook.
fn runtime(reason: String) -> Failure {
    Failure::Extraction(Error::ParseError(reason))
}

impl<'r, 'x, 'h> SheetReader<'r, 'x, 'h> {
    pub(crate) fn new(
        styles: Option<&'r Styles>,
        comments: Option<&'r Comments>,
        strings: Option<&'r SharedStrings>,
        formatter: &'r mut DataFormatter,
        output: &'r mut SheetOutput,
        xhtml: &'x mut Xhtml<'h>,
    ) -> Self {
        Self {
            captured: Captured::default(),
            styles,
            comments,
            strings,
            formatter,
            output,
            xhtml,
            value_open: false,
            inline_open: false,
            header_footer_open: false,
            data_type: DataType::Number,
            format_index: 0,
            format_string: None,
            row: 0,
            next_row: 0,
            has_reference: false,
            reference: String::new(),
            value: String::new(),
            header_footer: String::new(),
            comment_cells: comments
                .map(|comments| comments.cells().collect())
                .unwrap_or_default(),
        }
    }

    /// The cell's `r` attribute.
    fn reference(&self) -> Option<&str> {
        self.has_reference.then_some(self.reference.as_str())
    }

    fn is_text_tag(&self, name: &str) -> bool {
        name == "v" || name == "inlineStr" || (name == "t" && self.inline_open)
    }

    fn start(&mut self, local_name: &str, attributes: &[Attribute]) -> Result<(), Failure> {
        let value = |name: &str| attribute_named(attributes, name);
        if self.is_text_tag(local_name) {
            self.value_open = true;
            if !self.inline_open {
                self.value.clear();
            }
        } else if local_name == "is" {
            self.inline_open = true;
        } else if local_name == "f" {
            if self.data_type == DataType::Number {
                self.data_type = DataType::Formula;
            }
        } else if HEADERS.contains(&local_name) || FOOTERS.contains(&local_name) {
            self.header_footer_open = true;
            self.header_footer.clear();
        } else if local_name == "row" {
            self.row = match value("r") {
                Some(row) => parse_int(trim(row))
                    .ok_or_else(|| runtime(format!("The row number {row:?} is not an int")))?
                    .wrapping_sub(1),
                None => self.next_row,
            };
            self.output.start_row(self.row, self.xhtml)?;
        } else if local_name == "c" {
            self.format_index = -1;
            self.format_string = None;
            self.reference.clear();
            self.has_reference = match value("r") {
                Some(reference) => {
                    self.reference.push_str(reference);
                    true
                }
                None => false,
            };
            self.data_type = match value("t") {
                Some("b") => DataType::Boolean,
                Some("e") => DataType::Error,
                Some("inlineStr") => DataType::InlineString,
                Some("s") => DataType::SharedString,
                Some("str") => DataType::Formula,
                _ => {
                    self.resolve_format(value("s"))?;
                    DataType::Number
                }
            };
        }
        Ok(())
    }

    /// The format of a number cell's style, the first style's for a cell without one.
    fn resolve_format(&mut self, style: Option<&str>) -> Result<(), Failure> {
        let Some(styles) = self.styles else {
            return Ok(());
        };
        let style = match style {
            Some(style) => parse_int(trim(style))
                .ok_or_else(|| runtime(format!("The cell style {style:?} is not an int")))?,
            None if styles.len() > 0 => 0,
            None => -1,
        };
        if style >= 0 {
            self.format_index = styles.format_index(style);
            self.format_string = styles.format_string(style);
        }
        Ok(())
    }

    fn end(&mut self, local_name: &str) -> Result<(), Failure> {
        if self.is_text_tag(local_name) {
            self.value_open = false;
            if !self.inline_open {
                self.output_cell()?;
                self.value.clear();
            }
        } else if local_name == "is" {
            self.inline_open = false;
            self.output_cell()?;
            self.value.clear();
        } else if local_name == "row" {
            self.check_empty_cell_comments(CommentCheck::EndOfRow)?;
            self.output.end_row(self.xhtml)?;
            self.next_row = self.row.wrapping_add(1);
        } else if local_name == "sheetData" {
            self.check_empty_cell_comments(CommentCheck::EndOfSheetData)?;
        } else if HEADERS.contains(&local_name) {
            self.header_footer_open = false;
            self.output
                .header_footer(std::mem::take(&mut self.header_footer), true);
        } else if FOOTERS.contains(&local_name) {
            self.header_footer_open = false;
            self.output
                .header_footer(std::mem::take(&mut self.header_footer), false);
        }
        Ok(())
    }

    /// The number `value` holds in the cell's format, or the value itself where it is not a
    /// number or the format throws, as Tika's catch writes it.
    fn formatted<'v>(&mut self, value: &'v str) -> Result<Cow<'v, str>, Failure> {
        let Some(format) = self.format_string else {
            return Ok(Cow::Borrowed(value));
        };
        let Some(number) = parse_double(trim(value)) else {
            return Ok(Cow::Borrowed(value));
        };
        match self
            .formatter
            .format_raw(number, i32::from(self.format_index), format)
        {
            Ok(text) => Ok(Cow::Owned(text)),
            Err(FormatError::Thrown(_)) => Ok(Cow::Borrowed(value)),
            Err(FormatError::Unported(what)) => Err(runtime(format!(
                "the cell {} holds {what}, which this port does not write",
                self.reference().unwrap_or("without a reference")
            ))),
        }
    }

    /// Writes the cell its value has been read for, the value set aside while it is written.
    fn output_cell(&mut self) -> Result<(), Failure> {
        let value = std::mem::take(&mut self.value);
        let written = self.write_cell(&value);
        self.value = value;
        written
    }

    fn write_cell(&mut self, value: &str) -> Result<(), Failure> {
        let text = match self.data_type {
            DataType::Boolean => {
                let first = value
                    .chars()
                    .next()
                    .ok_or_else(|| runtime("A boolean cell has an empty value".to_owned()))?;
                Some(Cow::Borrowed(if first == '0' { "FALSE" } else { "TRUE" }))
            }
            DataType::Error => Some(Cow::Owned(format!("ERROR:{value}"))),
            DataType::Formula => Some(self.formatted(value)?),
            DataType::InlineString => Some(Cow::Borrowed(value)),
            DataType::SharedString => self.shared_string(value)?.map(Cow::Borrowed),
            DataType::Number => {
                if value.is_empty() {
                    Some(Cow::Borrowed(""))
                } else {
                    Some(self.formatted(value)?)
                }
            }
        };
        self.check_empty_cell_comments(CommentCheck::Cell)?;
        let comment = match self.comments {
            Some(comments) => {
                let reference = self.reference().ok_or_else(|| {
                    runtime("A cell without a reference is in a sheet with comments".to_owned())
                })?;
                comments.find(CellAddress::parse(reference)?)
            }
            None => None,
        };
        self.output.cell(
            self.has_reference.then_some(self.reference.as_str()),
            text.as_deref(),
            comment,
            self.xhtml,
        )
    }

    /// The shared string the index `value` names, `None` for an index that is not an int.
    fn shared_string(&self, value: &str) -> Result<Option<&'r str>, Failure> {
        let index = trim(value);
        let Some(strings) = self.strings.filter(|_| !index.is_empty()) else {
            return Ok(None);
        };
        let Some(position) = parse_int(index) else {
            return Ok(None);
        };
        usize::try_from(position)
            .ok()
            .and_then(|position| strings.strings.get(position))
            .map(|string| Some(string.as_str()))
            .ok_or_else(|| runtime(format!("The shared string {position} does not exist")))
    }

    /// `checkForEmptyCellComments`: writes the comments of cells the sheet has no cell for,
    /// as their place in the sheet passes.
    fn check_empty_cell_comments(&mut self, check: CommentCheck) -> Result<(), Failure> {
        if self.comments.is_none() || self.comment_cells.is_empty() {
            return Ok(());
        }
        if check == CommentCheck::EndOfSheetData {
            while let Some(cell) = self.comment_cells.pop_front() {
                self.output_empty_cell_comment(cell)?;
            }
            return Ok(());
        }
        let Some(reference) = self.reference() else {
            if check == CommentCheck::EndOfRow {
                while let Some(&cell) = self.comment_cells.front() {
                    if cell.row != self.row {
                        return Ok(());
                    }
                    self.comment_cells.pop_front();
                    self.output_empty_cell_comment(cell)?;
                }
                return Ok(());
            }
            return Err(runtime(format!(
                "Cell ref should be null only if there are only empty cells in the row; rowNum: {}",
                self.row
            )));
        };
        let address = CellAddress::parse(reference)?;
        loop {
            let Some(&peek) = self.comment_cells.front() else {
                return Ok(());
            };
            if check == CommentCheck::Cell && address == peek {
                self.comment_cells.pop_front();
                return Ok(());
            }
            let comparison = peek.compare(&address);
            let up_to_row = peek.row <= self.row;
            let passed =
                (comparison == Ordering::Greater && check == CommentCheck::EndOfRow && up_to_row)
                    || (comparison == Ordering::Less && check == CommentCheck::Cell && up_to_row);
            if !passed {
                return Ok(());
            }
            self.comment_cells.pop_front();
            self.output_empty_cell_comment(peek)?;
            if self.comment_cells.is_empty() {
                return Ok(());
            }
        }
    }

    fn output_empty_cell_comment(&mut self, cell: CellAddress) -> Result<(), Failure> {
        let comment = self.comments.and_then(|comments| comments.find(cell));
        let reference = cell.format();
        self.output
            .cell(Some(&reference), None, comment, self.xhtml)
    }
}

impl SaxHandler for SheetReader<'_, '_, '_> {
    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        // Tika recognises hidden rows and columns by local name in any namespace (#95).
        if tag.namespace != SPREADSHEETML_NS {
            return Ok(());
        }
        let hidden = || {
            attribute_named(tag.attributes, "hidden")
                .is_some_and(|value| value == "true" || value == "1")
        };
        match tag.local_name {
            "row" if !self.captured.hidden_row && hidden() => self.captured.hidden_row = true,
            "col" if !self.captured.hidden_column && hidden() => {
                self.captured.hidden_column = true;
            }
            _ => {}
        }
        self.start(tag.local_name, tag.attributes)
    }

    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.start_tag(&StartTag {
            namespace,
            local_name,
            qualified_name: local_name,
            attributes,
            declarations: &[],
        })
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace != SPREADSHEETML_NS {
            return Ok(());
        }
        self.end(local_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.value_open {
            self.value.push_str(text);
        }
        if self.header_footer_open {
            self.header_footer.push_str(text);
        }
        Ok(())
    }
}
