//! Tika's `TikaHSSFListener` behind POI's `FormatTrackingHSSFListener`: the records of a
//! workbook into a page per sheet with its cells in a table, the text found outside the cells
//! after it, and what the workbook's metadata records of its sheets, rows, columns and
//! comments.
//!
//! The records are listened to twice. The first time plans the workbook, as far as deciding
//! whether it is read and how each sheet is written, and holds none of its text; the second
//! writes it. Tika's listener holds a sheet's cells until the sheet ends, to write them in
//! order; a sheet whose cells come in order is written as they come, the hyperlinks found later
//! in the sheet applied as the plan found them, and only a sheet whose cells do not is held.
//!
//! Unlike Tika's listener (#107), it writes a formula's boolean or error result as such, where
//! Tika writes 0; a linked cell once, in the table, where Tika writes it again outside it; a
//! sheet with an embedded chart as one page, where Tika ends the page at the chart and takes
//! the cells after it for text outside any sheet; headers and footers without their codes; and
//! every number with one formatter, in the workbook's date system.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};

use super::records::{bof, sid, Cached, CellRef, Record, SharedStringReader, Special};
use super::stream::RecordStream;
use crate::formats::data_format::header_footer;
use crate::formats::data_format::{
    builtin_format, default_number_format, error_text, DataFormatter, FormatError,
};
use crate::formats::poi::{Thrown, INDEX_OUT_OF_BOUNDS};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Unported;
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

/// Where the listener writes the XHTML of the sheets.
pub(super) trait Out {
    fn start(&mut self, name: &'static str) -> Result<(), Stop>;
    fn start_class(&mut self, name: &'static str, class: &'static str) -> Result<(), Stop>;
    fn start_link(&mut self, address: &str) -> Result<(), Stop>;
    fn end(&mut self, name: &'static str) -> Result<(), Stop>;
    fn text(&mut self, text: &str) -> Result<(), Stop>;
    /// `XHTMLContentHandler.element`: the element with its text, unless that is empty.
    fn element(&mut self, name: &'static str, text: &str) -> Result<(), Stop>;
}

impl Out for Xhtml<'_> {
    fn start(&mut self, name: &'static str) -> Result<(), Stop> {
        self.start_element(name)
    }

    fn start_class(&mut self, name: &'static str, class: &'static str) -> Result<(), Stop> {
        self.start_element_with(name, &[("class", class)])
    }

    fn start_link(&mut self, address: &str) -> Result<(), Stop> {
        self.start_element_with("a", &[("href", address)])
    }

    fn end(&mut self, name: &'static str) -> Result<(), Stop> {
        self.end_element(name)
    }

    fn text(&mut self, text: &str) -> Result<(), Stop> {
        self.characters(text)
    }

    fn element(&mut self, name: &'static str, text: &str) -> Result<(), Stop> {
        Xhtml::element(self, name, text)
    }
}

/// How a sheet is written, as planning the workbook found.
#[derive(Debug, Clone, Default)]
pub(super) struct SheetPlan {
    /// Whether the sheet ends, so that Tika's listener writes it.
    ends: bool,
    /// Whether its cells come in the order they are written, each after the last.
    ordered: bool,
    /// The name its page is headed with, if any.
    name: Option<String>,
    /// The hyperlinks of an ordered sheet, by the cell they lead from: how many cells were in
    /// the table when each came, and its address, in their order.
    links: HashMap<(i32, i32), Vec<(u64, String)>>,
}

impl SheetPlan {
    pub(super) fn is_streamed(&self) -> bool {
        self.ends && self.ordered
    }
}

/// What the listener does with the cells of the sheets.
pub(super) enum Pass<'p> {
    /// Plans the workbook: reads it as far as deciding whether it is read, and finds how each
    /// sheet is written, holding none of the text.
    Plan,
    /// Writes the sheets as the plans say.
    Write(&'p [SheetPlan]),
}

/// What the listener found of the workbook beyond its text.
#[derive(Debug, Default)]
pub(super) struct Found {
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
    /// How each sheet is written, in the order the sheets start.
    pub(super) plans: Vec<SheetPlan>,
}

/// The cells of the sheet being read.
enum Sheet {
    /// Planned, by its plan among the workbook's: where its cells are placed, to find whether
    /// they come in order.
    Planned {
        last: Option<(i32, i32)>,
        placed: u64,
        plan: usize,
    },
    /// Written as its cells come.
    Streamed {
        table: Table,
        placed: u64,
        plan: usize,
    },
    /// Held until it ends, as Tika's listener holds every sheet.
    Held(BTreeMap<(i32, i32), Cell>),
}

/// A sheet's table being written: the row and column of the cell last written.
struct Table {
    row: i32,
    column: i32,
}

impl Table {
    /// Starts the page of a sheet, headed with its name, and its table.
    fn start(name: Option<&str>, out: &mut dyn Out) -> Result<Table, Stop> {
        out.start_class("div", "page")?;
        if let Some(name) = name {
            out.element("h1", name)?;
        }
        for name in ["table", "tbody", "tr", "td"] {
            out.start(name)?;
        }
        Ok(Table { row: 0, column: 0 })
    }

    /// Writes the cell at `place`, which comes after the cells written before it.
    fn cell(
        &mut self,
        (row, column): (i32, i32),
        cell: &Cell,
        missing_rows: bool,
        out: &mut dyn Out,
    ) -> Result<(), Stop> {
        if self.row != row {
            loop {
                out.end("td")?;
                out.end("tr")?;
                out.start("tr")?;
                out.start("td")?;
                self.row += 1;
                if !(missing_rows && self.row < row) {
                    break;
                }
            }
            self.row = row;
            self.column = 0;
        }
        while self.column < column {
            out.end("td")?;
            out.start("td")?;
            self.column += 1;
        }
        render(cell, out)
    }

    fn end(out: &mut dyn Out) -> Result<(), Stop> {
        for name in ["td", "tr", "tbody", "table"] {
            out.end(name)?;
        }
        Ok(())
    }
}

/// The listener, as it goes through the records.
pub(super) struct Listener<'a, 'p> {
    settings: Settings,
    pass: Pass<'p>,
    custom_formats: HashMap<i32, String>,
    extended_formats: Vec<i16>,
    formatter: DataFormatter,
    shared_strings: Option<SharedStringReader<'a>>,
    string_formula: Option<CellRef>,
    previous_sid: u16,
    sheet_names: Vec<String>,
    sheet_index: i16,
    sheet: Option<Sheet>,
    /// How many sheets have started.
    sheets: usize,
    /// How many charts embedded in the sheet are open.
    charts: usize,
    /// The text outside the cells since the last sheet ended, which only a write holds.
    extra: Vec<Cell>,
    in_drawing: bool,
    /// The exception a record raised, after which Tika's listener reads no more.
    stored: Option<Thrown>,
    found: Found,
}

/// Why the listener stopped on a record.
pub(super) enum Interrupted {
    /// POI or Tika threw.
    Thrown(Thrown),
    /// The record holds what the port does not write.
    Unported(Unported),
    /// The output took no more.
    Stopped(Stop),
}

impl From<Stop> for Interrupted {
    fn from(stop: Stop) -> Self {
        Interrupted::Stopped(stop)
    }
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

impl<'a, 'p> Listener<'a, 'p> {
    pub(super) fn new(settings: Settings, pass: Pass<'p>) -> Self {
        Listener {
            settings,
            pass,
            custom_formats: HashMap::new(),
            extended_formats: Vec::new(),
            formatter: DataFormatter::new(false),
            shared_strings: None,
            string_formula: None,
            previous_sid: 0,
            sheet_names: Vec::new(),
            sheet_index: 0,
            sheet: None,
            sheets: 0,
            charts: 0,
            extra: Vec::new(),
            in_drawing: false,
            stored: None,
            found: Found::default(),
        }
    }

    fn plans(&self) -> bool {
        matches!(self.pass, Pass::Plan)
    }

    /// `FormatTrackingHSSFListener.processRecord`, then `TikaHSSFListener.processRecord` for a
    /// record handed to them, read from `stream`, writing to `out`. A failure Tika's listener
    /// stores is kept until [`Listener::finish`]; one it does not catch fails at once.
    pub(super) fn process(
        &mut self,
        record_sid: u16,
        record: &Record,
        stream: &RecordStream<'a>,
        out: &mut dyn Out,
    ) -> Result<(), Interrupted> {
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
        match self.process_record(record_sid, record, stream, out) {
            Err(Uncaught::Stored(thrown)) => {
                self.stored = Some(thrown);
                Ok(())
            }
            Err(Uncaught::Thrown(thrown)) => Err(Interrupted::Thrown(thrown)),
            Err(Uncaught::Unported(unported)) => Err(Interrupted::Unported(unported)),
            Err(Uncaught::Stopped(stop)) => Err(Interrupted::Stopped(stop)),
            Ok(()) => Ok(()),
        }
    }

    /// A drawing group record handed to the listener.
    pub(super) fn drawing_group(&mut self, index: usize) {
        if self.stored.is_some() {
            return;
        }
        self.in_drawing = false;
        self.found.drawing_groups.push(index);
        self.previous_sid = sid::DRAWING_GROUP;
        self.string_formula = None;
    }

    fn process_record(
        &mut self,
        record_sid: u16,
        record: &Record,
        stream: &RecordStream<'a>,
        out: &mut dyn Out,
    ) -> Result<(), Uncaught> {
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
                        self.new_sheet(out)?;
                    }
                }
                bof::WORKSHEET => self.new_sheet(out)?,
                _ => {}
            },
            Record::Eof if self.charts > 0 => self.charts -= 1,
            Record::Eof => {
                if self.sheet.is_some() {
                    self.process_sheet(out)?;
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
                    self.found.hidden_sheets.push(name.clone());
                }
                if *very_hidden {
                    self.found.very_hidden_sheets.push(name.clone());
                }
                self.sheet_names.push(name.clone());
            }
            Record::Sst(strings) => {
                self.shared_strings = Some(SharedStringReader::new(strings.clone(), stream));
            }
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
                    self.add_text_cell(Some(*cell), &text, out)?;
                }
                // Tika formats a boolean, an error or an empty result as the number 0.
                Cached::Special(Special {
                    kind: Special::BOOLEAN,
                    value,
                }) => {
                    let text = if *value == 1 { "TRUE" } else { "FALSE" };
                    self.add_text_cell(Some(*cell), text, out)?;
                }
                Cached::Special(Special {
                    kind: Special::ERROR,
                    value,
                }) => {
                    if let Some(text) = error_text(*value) {
                        self.add_text_cell(Some(*cell), text, out)?;
                    }
                }
                Cached::Special(_) => {}
            },
            Record::String(text) => {
                if self.previous_sid == sid::FORMULA {
                    self.add_text_cell(self.string_formula, text, out)?;
                }
            }
            Record::Label { cell, text } => self.add_text_cell(Some(*cell), text, out)?,
            Record::LabelSst { cell, index } => {
                let text = self.shared_string(*index)?;
                self.add_text_cell(Some(*cell), &text, out)?;
            }
            Record::Number { cell, value } => {
                let text = self.format_number(*cell, *value)?;
                self.add_text_cell(Some(*cell), &text, out)?;
            }
            Record::Hyperlink {
                first_row,
                first_column,
                address: Some(address),
            } => self.link((*first_row, *first_column), address),
            Record::TextObject(text) => {
                if self.settings.shape_based_content {
                    self.add_text_cell(None, text, out)?;
                }
            }
            Record::SeriesText(text) => self.add_text_cell(None, text, out)?,
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
                    self.add_text_cell(None, &header_footer::extract(text), out)?;
                }
            }
            Record::Protect(true) => self.found.protected_sheet = true,
            Record::ColumnInfo { hidden: true } => self.found.hidden_column = true,
            Record::Note { author } => {
                if !is_blank(author) {
                    let units = author.encode_utf16().collect();
                    self.found.comment_authors.insert(units, author.clone());
                }
            }
            Record::Row { zero_height: true } => self.found.hidden_row = true,
            _ => {}
        }
        self.previous_sid = record_sid;
        if !keeps_string_formula {
            self.string_formula = None;
        }
        Ok(())
    }

    /// The text of the shared string at `index`: when planning, only whether the table holds
    /// it.
    fn shared_string(&mut self, index: i32) -> Result<String, Uncaught> {
        let Some(strings) = &mut self.shared_strings else {
            return Err(Uncaught::Stored(Thrown::tika(
                "sstRecord should have been initialized before a ref to the shared string table",
            )));
        };
        let index = usize::try_from(index)
            .ok()
            .filter(|&index| index < strings.len())
            .ok_or_else(|| Uncaught::Thrown(index_out_of_bounds(index, strings.len())))?;
        let string = strings.get(index).map_err(Uncaught::Thrown)?;
        Ok(if self.settings.phonetic_runs {
            let phonetic = string
                .phonetic
                .as_deref()
                .filter(|phonetic| !is_blank(phonetic))
                .unwrap_or("");
            format!("{} {phonetic}", string.text)
        } else {
            string.text
        })
    }

    /// A hyperlink from the cell at `place` of the sheet, which links that cell where it is
    /// already in the table. Tika writes it again, linked, outside the table.
    fn link(&mut self, place: (i32, i32), address: &str) {
        match &mut self.sheet {
            Some(Sheet::Planned { placed, plan, .. }) => {
                let plan = &mut self.found.plans[*plan];
                if plan.ordered {
                    plan.links
                        .entry(place)
                        .or_default()
                        .push((*placed, address.to_owned()));
                }
            }
            Some(Sheet::Held(cells)) => {
                if let Some(cell) = cells.get_mut(&place) {
                    let text = std::mem::replace(cell, Cell::Text(String::new()));
                    *cell = Cell::Linked(Box::new(text), address.to_owned());
                }
            }
            Some(Sheet::Streamed { .. }) | None => {}
        }
    }

    /// The end of the records: the stored failure, else what the listener found, and, of a
    /// write, the text left outside any sheet.
    pub(super) fn finish(self) -> Result<(Found, Vec<Cell>), Thrown> {
        if let Some(thrown) = self.stored {
            return Err(thrown);
        }
        Ok((self.found, self.extra))
    }

    fn append_drawing(&mut self, data: &[u8]) {
        if !self.plans() {
            return;
        }
        self.found
            .sheet_drawings
            .entry(i32::from(self.sheet_index as u16))
            .or_default()
            .extend_from_slice(data);
    }

    fn new_sheet(&mut self, out: &mut dyn Out) -> Result<(), Uncaught> {
        self.sheet_index = self.sheet_index.wrapping_add(1);
        self.charts = 0;
        let started = self.sheets;
        self.sheets += 1;
        self.sheet = Some(match self.pass {
            Pass::Plan => {
                self.found.plans.push(SheetPlan {
                    ordered: true,
                    ..SheetPlan::default()
                });
                Sheet::Planned {
                    last: None,
                    placed: 0,
                    plan: started,
                }
            }
            Pass::Write(plans) if plans[started].is_streamed() => Sheet::Streamed {
                table: Table::start(plans[started].name.as_deref(), out)?,
                placed: 0,
                plan: started,
            },
            Pass::Write(_) => Sheet::Held(BTreeMap::new()),
        });
        Ok(())
    }

    /// Adds a cell to the sheet's table, or, without a place or with a taken one, or in a chart
    /// embedded in the sheet, to the text outside the cells.
    fn add_cell(
        &mut self,
        cell_ref: Option<CellRef>,
        cell: Cell,
        out: &mut dyn Out,
    ) -> Result<(), Stop> {
        let place = cell_ref
            .filter(|_| self.charts == 0)
            .map(|cell_ref| (cell_ref.row, i32::from(cell_ref.column)));
        let (Some(sheet), Some(place)) = (&mut self.sheet, place) else {
            if !self.plans() {
                self.extra.push(cell);
            }
            return Ok(());
        };
        match sheet {
            Sheet::Planned { last, placed, plan } => {
                if last.is_some_and(|last| place <= last) {
                    self.found.plans[*plan].ordered = false;
                }
                *last = Some(place);
                *placed += 1;
            }
            Sheet::Streamed {
                table,
                placed,
                plan,
            } => {
                let Pass::Write(plans) = self.pass else {
                    unreachable!("a sheet is streamed as it is written");
                };
                let mut cell = cell;
                for (before, address) in plans[*plan].links.get(&place).into_iter().flatten() {
                    if *before > *placed {
                        cell = Cell::Linked(Box::new(cell), address.clone());
                    }
                }
                *placed += 1;
                table.cell(place, &cell, self.settings.missing_rows, out)?;
            }
            Sheet::Held(cells) => match cells.entry(place) {
                Entry::Occupied(_) => self.extra.push(cell),
                Entry::Vacant(vacant) => {
                    vacant.insert(cell);
                }
            },
        }
        Ok(())
    }

    fn add_text_cell(
        &mut self,
        cell_ref: Option<CellRef>,
        text: &str,
        out: &mut dyn Out,
    ) -> Result<(), Stop> {
        let text = trim(text);
        if text.is_empty() {
            return Ok(());
        }
        self.add_cell(cell_ref, Cell::Text(text.to_owned()), out)
    }

    /// The name the page of the sheet is headed with, if any, failing on a sheet before the
    /// first.
    fn sheet_name(&self) -> Result<Option<String>, Uncaught> {
        if i32::from(self.sheet_index) >= self.sheet_names.len() as i32 {
            return Ok(None);
        }
        usize::try_from(self.sheet_index)
            .ok()
            .and_then(|index| self.sheet_names.get(index))
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                Uncaught::Thrown(index_out_of_bounds(
                    self.sheet_index,
                    self.sheet_names.len(),
                ))
            })
    }

    /// `processSheet`: the page, then the text found outside the cells, each in a div.
    fn process_sheet(&mut self, out: &mut dyn Out) -> Result<(), Uncaught> {
        let name = self.sheet_name()?;
        match self.sheet.take().expect("a sheet being read") {
            Sheet::Planned { plan, .. } => {
                let plan = &mut self.found.plans[plan];
                plan.ends = true;
                plan.name = name;
                return Ok(());
            }
            Sheet::Streamed { .. } => Table::end(out)?,
            Sheet::Held(cells) => {
                let mut table = Table::start(name.as_deref(), out)?;
                for (&place, cell) in &cells {
                    table.cell(place, cell, self.settings.missing_rows, out)?;
                }
                Table::end(out)?;
            }
        }
        write_outside(&std::mem::take(&mut self.extra), out)?;
        out.end("div")?;
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
        formatted.map_err(|error| match error {
            FormatError::Thrown(reason) => {
                Uncaught::Thrown(Thrown::new(format!("the cell format throws: {reason}")))
            }
            FormatError::Unported(what) => Uncaught::Unported(Unported(format!(
                "a cell holds {what}, which this port does not write"
            ))),
        })
    }

    /// `getFormatIndex`: failing on a style the workbook does not have.
    fn format_index(&self, cell: CellRef) -> Result<i32, Uncaught> {
        usize::try_from(cell.xf)
            .ok()
            .and_then(|xf| self.extended_formats.get(xf))
            .map(|&index| i32::from(index))
            .ok_or_else(|| {
                Uncaught::Thrown(index_out_of_bounds(cell.xf, self.extended_formats.len()))
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

/// `ArrayList.get(index)` of a list of `length` elements, past its end.
fn index_out_of_bounds(index: impl std::fmt::Display, length: usize) -> Thrown {
    Thrown::runtime(
        INDEX_OUT_OF_BOUNDS,
        format!("Index {index} out of bounds for length {length}"),
    )
}

/// A failure of a record: one Tika's listener stores, one it lets through, what the port does
/// not write, or the output taking no more.
enum Uncaught {
    Stored(Thrown),
    Thrown(Thrown),
    Unported(Unported),
    Stopped(Stop),
}

impl From<Stop> for Uncaught {
    fn from(stop: Stop) -> Self {
        Uncaught::Stopped(stop)
    }
}

/// `processExtraText`.
pub(super) fn write_outside(cells: &[Cell], out: &mut dyn Out) -> Result<(), Stop> {
    for cell in cells {
        out.start_class("div", "outside")?;
        render(cell, out)?;
        out.end("div")?;
    }
    Ok(())
}

/// `Cell.render`.
fn render(cell: &Cell, out: &mut dyn Out) -> Result<(), Stop> {
    match cell {
        Cell::Text(text) => out.text(text),
        Cell::Linked(cell, address) => {
            out.start_link(address)?;
            render(cell, out)?;
            out.end("a")
        }
    }
}

/// An output that takes nothing, which planning writes to.
pub(super) struct Nowhere;

impl Out for Nowhere {
    fn start(&mut self, _: &'static str) -> Result<(), Stop> {
        Ok(())
    }

    fn start_class(&mut self, _: &'static str, _: &'static str) -> Result<(), Stop> {
        Ok(())
    }

    fn start_link(&mut self, _: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn end(&mut self, _: &'static str) -> Result<(), Stop> {
        Ok(())
    }

    fn text(&mut self, _: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn element(&mut self, _: &'static str, _: &str) -> Result<(), Stop> {
        Ok(())
    }
}
