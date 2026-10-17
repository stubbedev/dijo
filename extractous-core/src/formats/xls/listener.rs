//! Tika's `TikaHSSFListener` behind POI's `FormatTrackingHSSFListener`: the records of a
//! workbook into a page per sheet with its cells in a table, the text found outside the cells
//! after it, and what the workbook's metadata records of its sheets, rows, columns and
//! comments.
//!
//! Unlike Tika's listener (#107), it writes a formula's boolean or error result as such, where
//! Tika writes 0; a linked cell once, in the table, where Tika writes it again outside it; a
//! sheet with an embedded chart as one page, where Tika ends the page at the chart and takes
//! the cells after it for text outside any sheet; headers and footers without their codes; and
//! every number with one formatter, in the workbook's date system.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};

use super::records::{bof, error_text, sid, Cached, CellRef, Record, SharedString, Special};
use crate::formats::data_format::{
    builtin_format, default_number_format, DataFormatter, FormatError,
};
use crate::formats::poi::{Read, Thrown};
use crate::formats::xlsx::header_footer;
use crate::java::is_blank;
use crate::java::trim;

/// The first format index of a workbook's own formats.
const FIRST_CUSTOM_FORMAT: i32 = 50;

/// What of the workbook Tika's Office parser settings have the listener write.
#[derive(Debug, Clone, Copy)]
pub(super) struct Settings {
    pub(super) headers_and_footers: bool,
    pub(super) shape_based_content: bool,
    pub(super) phonetic_runs: bool,
    pub(super) missing_rows: bool,
}

/// A cell's content: text, or a cell a hyperlink leads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Cell {
    Text(String),
    Linked(Box<Cell>, String),
}

/// The XHTML the listener writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Out {
    Start(&'static str),
    StartClass(&'static str, &'static str),
    StartLink(String),
    End(&'static str),
    Text(String),
    /// `XHTMLContentHandler.element`: the element with its text, unless that is empty.
    Element(&'static str, String),
}

/// A sheet as `processSheet` writes it: a page with its name and its cells in a table, then
/// the text found outside the cells since the sheet before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Page {
    /// From the page's start to the table's end.
    pub(super) table: Vec<Out>,
    pub(super) outside: Vec<Cell>,
}

/// What the listener read of the workbook.
#[derive(Debug, Default)]
pub(super) struct Extracted {
    pub(super) pages: Vec<Page>,
    /// The text found outside the cells after the last sheet.
    pub(super) outside: Vec<Cell>,
    pub(super) hidden_sheets: Vec<String>,
    pub(super) very_hidden_sheets: Vec<String>,
    /// The authors of comments, by their UTF-16 units, as a `TreeSet` orders them.
    pub(super) comment_authors: BTreeMap<Vec<u16>, String>,
    pub(super) hidden_column: bool,
    pub(super) hidden_row: bool,
    pub(super) protected_sheet: bool,
    /// The drawing data of each sheet, by its index.
    pub(super) sheet_drawings: BTreeMap<i32, Vec<u8>>,
    /// The drawing groups, by their index among the workbook's.
    pub(super) drawing_groups: Vec<usize>,
}

/// The listener, as it goes through the records.
pub(super) struct Listener {
    settings: Settings,
    custom_formats: HashMap<i32, String>,
    extended_formats: Vec<i16>,
    formatter: DataFormatter,
    shared_strings: Option<Vec<SharedString>>,
    string_formula: Option<CellRef>,
    previous_sid: u16,
    sheet_names: Vec<String>,
    sheet_index: i16,
    sheet: Option<BTreeMap<(i32, i32), Cell>>,
    /// How many charts embedded in the sheet are open.
    charts: usize,
    extra: Vec<Cell>,
    in_drawing: bool,
    /// The exception a record raised, after which Tika's listener reads no more.
    stored: Option<Thrown>,
    pub(super) read: Extracted,
}

/// Whether Tika's `HSSFRequest` hands records of this type to the listener.
pub(super) fn is_listened(record_sid: u16, settings: &Settings) -> bool {
    const COUNTRY: u16 = 0x008C;
    matches!(
        record_sid,
        sid::BOF
            | sid::EOF
            | sid::DATE_WINDOW_1904
            | COUNTRY
            | sid::BOUND_SHEET
            | sid::SST
            | sid::FORMULA
            | sid::LABEL
            | sid::LABEL_SST
            | sid::NUMBER
            | sid::RK
            | sid::STRING
            | sid::HYPERLINK
            | sid::TEXT_OBJECT
            | sid::SERIES_TEXT
            | sid::FORMAT
            | sid::EXTENDED_FORMAT
            | sid::DRAWING_GROUP
            | sid::DRAWING
            | sid::CONTINUE
            | sid::PROTECT
            | sid::COLUMN_INFO
            | sid::ROW
            | sid::NOTE
    ) || (settings.headers_and_footers && matches!(record_sid, sid::HEADER | sid::FOOTER))
}

impl Listener {
    pub(super) fn new(settings: Settings) -> Self {
        Listener {
            settings,
            custom_formats: HashMap::new(),
            extended_formats: Vec::new(),
            formatter: DataFormatter::new(false),
            shared_strings: None,
            string_formula: None,
            previous_sid: 0,
            sheet_names: Vec::new(),
            sheet_index: 0,
            sheet: None,
            charts: 0,
            extra: Vec::new(),
            in_drawing: false,
            stored: None,
            read: Extracted::default(),
        }
    }

    /// `FormatTrackingHSSFListener.processRecord`, then `TikaHSSFListener.processRecord` for a
    /// record handed to them. A failure Tika's listener stores is kept until
    /// [`Listener::finish`]; one it does not catch fails at once.
    pub(super) fn process(&mut self, record_sid: u16, record: &Record) -> Read<()> {
        match record {
            Record::Format { index, format } => {
                self.custom_formats.insert(*index, format.clone());
            }
            Record::ExtendedFormat { format_index } => self.extended_formats.push(*format_index),
            _ => {}
        }
        if self.stored.is_some() {
            return Ok(());
        }
        match self.process_record(record_sid, record) {
            Err(Uncaught::Stored(thrown)) => {
                self.stored = Some(thrown);
                Ok(())
            }
            Err(Uncaught::Thrown(thrown)) => Err(thrown),
            Ok(()) => Ok(()),
        }
    }

    /// A drawing group record handed to the listener.
    pub(super) fn drawing_group(&mut self, index: usize) {
        if self.stored.is_some() {
            return;
        }
        self.in_drawing = false;
        self.read.drawing_groups.push(index);
        self.previous_sid = sid::DRAWING_GROUP;
        self.string_formula = None;
    }

    fn process_record(&mut self, record_sid: u16, record: &Record) -> Result<(), Uncaught> {
        let was_in_drawing = std::mem::replace(&mut self.in_drawing, false);
        let mut keeps_string_formula = false;
        match record {
            Record::Bof { kind } => match *kind {
                bof::WORKBOOK => self.sheet_index = -1,
                bof::CHART => {
                    if self.sheet.is_some() {
                        // A chart embedded in the sheet, which Tika takes for the end of the
                        // sheet's page and the start of another under the same name, or, after
                        // another chart, for a sheet of its own.
                        self.charts += 1;
                    } else if self.previous_sid == sid::EOF {
                        self.new_sheet();
                    }
                }
                bof::WORKSHEET => self.new_sheet(),
                _ => {}
            },
            Record::Eof if self.charts > 0 => self.charts -= 1,
            Record::Eof => {
                if self.sheet.is_some() {
                    self.process_sheet()?;
                }
                self.sheet = None;
            }
            Record::DateWindow1904(date_1904) => self.formatter.set_date_1904(*date_1904),
            Record::BoundSheet {
                name,
                hidden,
                very_hidden,
            } => {
                if *hidden {
                    self.read.hidden_sheets.push(name.clone());
                }
                if *very_hidden {
                    self.read.very_hidden_sheets.push(name.clone());
                }
                self.sheet_names.push(name.clone());
            }
            Record::Sst(strings) => self.shared_strings = Some(strings.clone()),
            Record::Formula { cell, cached } => match cached {
                Cached::Special(Special {
                    kind: Special::STRING,
                    ..
                }) => {
                    self.string_formula = Some(*cell);
                    keeps_string_formula = true;
                }
                Cached::Number(value) => {
                    let text = self.format_number(*cell, *value)?;
                    self.add_text_cell(Some(*cell), &text);
                }
                // Tika formats a boolean, an error or an empty result as the number 0.
                Cached::Special(Special {
                    kind: Special::BOOLEAN,
                    value,
                }) => {
                    let text = if *value == 1 { "TRUE" } else { "FALSE" };
                    self.add_text_cell(Some(*cell), text);
                }
                Cached::Special(Special {
                    kind: Special::ERROR,
                    value,
                }) => {
                    if let Some(text) = error_text(*value) {
                        self.add_text_cell(Some(*cell), text);
                    }
                }
                Cached::Special(_) => {}
            },
            Record::String(text) => {
                if self.previous_sid == sid::FORMULA {
                    self.add_text_cell(self.string_formula, text);
                }
            }
            Record::Label { cell, text } => self.add_text_cell(Some(*cell), text),
            Record::LabelSst { cell, index } => {
                let Some(strings) = &self.shared_strings else {
                    return Err(Uncaught::Stored(Thrown(
                        "sstRecord should have been initialized before a ref to the shared \
                         string table"
                            .to_owned(),
                    )));
                };
                let string = usize::try_from(*index)
                    .ok()
                    .and_then(|index| strings.get(index))
                    .ok_or_else(|| {
                        Uncaught::Thrown(Thrown(format!(
                            "the shared string {index} is beyond the table"
                        )))
                    })?;
                let text = if self.settings.phonetic_runs {
                    let phonetic = string
                        .phonetic
                        .as_deref()
                        .filter(|phonetic| !is_blank(phonetic))
                        .unwrap_or("");
                    format!("{} {phonetic}", string.text)
                } else {
                    string.text.clone()
                };
                self.add_text_cell(Some(*cell), &text);
            }
            Record::Number { cell, value } => {
                let text = self.format_number(*cell, *value)?;
                self.add_text_cell(Some(*cell), &text);
            }
            Record::Hyperlink {
                first_row,
                first_column,
                address,
            } => {
                // The cell is linked where it is. Tika writes it again, linked, outside the
                // table.
                let linked = self
                    .sheet
                    .as_mut()
                    .and_then(|sheet| sheet.get_mut(&(*first_row, *first_column)));
                if let (Some(cell), Some(address)) = (linked, address) {
                    let text = std::mem::replace(cell, Cell::Text(String::new()));
                    *cell = Cell::Linked(Box::new(text), address.clone());
                }
            }
            Record::TextObject(text) => {
                if self.settings.shape_based_content {
                    self.add_text_cell(None, text);
                }
            }
            Record::SeriesText(text) => self.add_text_cell(None, text),
            Record::Drawing(data) => {
                if self.sheet_index >= 0 {
                    self.append_drawing(data);
                    self.in_drawing = true;
                }
            }
            Record::Continue(data) => {
                if was_in_drawing && self.sheet_index >= 0 {
                    self.append_drawing(data);
                    self.in_drawing = true;
                }
            }
            Record::Header(text) | Record::Footer(text) => {
                // Tika writes them with their codes.
                if self.settings.headers_and_footers {
                    self.add_text_cell(None, &header_footer::extract(text));
                }
            }
            Record::Protect(true) => self.read.protected_sheet = true,
            Record::ColumnInfo { hidden: true } => self.read.hidden_column = true,
            Record::Note { author } => {
                if !is_blank(author) {
                    let units = author.encode_utf16().collect();
                    self.read.comment_authors.insert(units, author.clone());
                }
            }
            Record::Row { zero_height: true } => self.read.hidden_row = true,
            _ => {}
        }
        self.previous_sid = record_sid;
        if !keeps_string_formula {
            self.string_formula = None;
        }
        Ok(())
    }

    /// The end of the records: the stored failure, else what the listener read, with the text
    /// left outside any sheet.
    pub(super) fn finish(mut self) -> Read<Extracted> {
        if let Some(thrown) = self.stored {
            return Err(thrown);
        }
        self.read.outside = std::mem::take(&mut self.extra);
        Ok(self.read)
    }

    fn append_drawing(&mut self, data: &[u8]) {
        self.read
            .sheet_drawings
            .entry(i32::from(self.sheet_index as u16))
            .or_default()
            .extend_from_slice(data);
    }

    fn new_sheet(&mut self) {
        self.sheet_index = self.sheet_index.wrapping_add(1);
        self.sheet = Some(BTreeMap::new());
        self.charts = 0;
    }

    /// Adds a cell to the sheet's table, or, without a place or with a taken one, or in a chart
    /// embedded in the sheet, to the text outside the cells.
    fn add_cell(&mut self, cell_ref: Option<CellRef>, cell: Cell) {
        let cell_ref = cell_ref.filter(|_| self.charts == 0);
        match (&mut self.sheet, cell_ref) {
            (Some(sheet), Some(cell_ref)) => {
                match sheet.entry((cell_ref.row, i32::from(cell_ref.column))) {
                    Entry::Occupied(_) => self.extra.push(cell),
                    Entry::Vacant(place) => {
                        place.insert(cell);
                    }
                }
            }
            _ => self.extra.push(cell),
        }
    }

    fn add_text_cell(&mut self, cell_ref: Option<CellRef>, text: &str) {
        let text = trim(text);
        if !text.is_empty() {
            self.add_cell(cell_ref, Cell::Text(text.to_owned()));
        }
    }

    fn process_sheet(&mut self) -> Result<(), Uncaught> {
        let mut table = Vec::new();
        let out = &mut table;
        out.push(Out::StartClass("div", "page"));
        if i32::from(self.sheet_index) < self.sheet_names.len() as i32 {
            let name = usize::try_from(self.sheet_index)
                .ok()
                .and_then(|index| self.sheet_names.get(index))
                .ok_or_else(|| {
                    Uncaught::Thrown(Thrown(format!(
                        "the sheet index {} is negative",
                        self.sheet_index
                    )))
                })?;
            out.push(Out::Element("h1", name.clone()));
        }
        out.push(Out::Start("table"));
        out.push(Out::Start("tbody"));
        out.push(Out::Start("tr"));
        out.push(Out::Start("td"));
        let mut current_row = 0;
        let mut current_column = 0;
        let sheet = self.sheet.as_ref().expect("a sheet being read");
        for (&(row, column), cell) in sheet {
            if current_row != row {
                loop {
                    out.push(Out::End("td"));
                    out.push(Out::End("tr"));
                    out.push(Out::Start("tr"));
                    out.push(Out::Start("td"));
                    current_row += 1;
                    if !(self.settings.missing_rows && current_row < row) {
                        break;
                    }
                }
                current_row = row;
                current_column = 0;
            }
            while current_column < column {
                out.push(Out::End("td"));
                out.push(Out::Start("td"));
                current_column += 1;
            }
            render(cell, out);
        }
        out.push(Out::End("td"));
        out.push(Out::End("tr"));
        out.push(Out::End("tbody"));
        out.push(Out::End("table"));
        let outside = std::mem::take(&mut self.extra);
        self.read.pages.push(Page { table, outside });
        Ok(())
    }

    /// `TikaFormatTrackingHSSFListener.formatNumberDateCell`, with one formatter. Tika formats
    /// dates and the general format with its formatter and any other format with POI's, each
    /// caching formats of its own.
    fn format_number(&mut self, cell: CellRef, value: f64) -> Result<String, Uncaught> {
        let index = self.format_index(cell)?;
        let format = self.format_string(index);
        let formatted = match format {
            Some(format) => self.formatter.format_raw(value, index, &format),
            None => Ok(default_number_format(value)),
        };
        formatted.map_err(|error| {
            Uncaught::Thrown(Thrown(match error {
                FormatError::Thrown(reason) => format!("the cell format throws: {reason}"),
                FormatError::Unported(what) => {
                    format!("a cell holds {what}, which this port does not write")
                }
            }))
        })
    }

    /// `getFormatIndex`: failing on a style the workbook does not have.
    fn format_index(&self, cell: CellRef) -> Result<i32, Uncaught> {
        usize::try_from(cell.xf)
            .ok()
            .and_then(|xf| self.extended_formats.get(xf))
            .map(|&index| i32::from(index))
            .ok_or_else(|| {
                Uncaught::Thrown(Thrown(format!(
                    "the cell style {} is beyond the workbook's",
                    cell.xf
                )))
            })
    }

    /// `getFormatString(formatIndex)`.
    fn format_string(&self, index: i32) -> Option<String> {
        if index >= FIRST_CUSTOM_FORMAT {
            self.custom_formats.get(&index).cloned()
        } else {
            builtin_format(i32::from(index as i16)).map(str::to_owned)
        }
    }
}

/// A failure of a record: one Tika's listener stores, or one it lets through.
enum Uncaught {
    Stored(Thrown),
    Thrown(Thrown),
}

/// `Cell.render`.
pub(super) fn render(cell: &Cell, out: &mut Vec<Out>) {
    match cell {
        Cell::Text(text) => out.push(Out::Text(text.clone())),
        Cell::Linked(cell, address) => {
            out.push(Out::StartLink(address.clone()));
            render(cell, out);
            out.push(Out::End("a"));
        }
    }
}
