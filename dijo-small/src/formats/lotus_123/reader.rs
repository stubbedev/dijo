//! The record stream of a 1-2-3 file and the cells, sheet names and formats read from it.
//!
//! Every record is a 16-bit type and length, little-endian, and its body. The first is the BOF
//! record, whose version tells Release 1 and 2 worksheets (`.wks`, `.wk1`), whose cell records
//! give a format byte, a 16-bit column and row, from the workbooks of Release 3 and later
//! (`.wk3`, `.wk4`, `.123`), whose cell records give a 16-bit row, a sheet and a column, and
//! whose formats are kept apart from the cells, row by row. The cells end at the first EOF
//! record: what follows it, as the format block of a `.wk4` file or the document information of a
//! `.123` file, holds no cells.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use super::format::Style;
use super::lmbcs;
use super::number::{self, Number};
use crate::formats::xhtml::within_budget;
use crate::formats::Failure;

const BOF: u16 = 0x00;
const EOF: u16 = 0x01;

/// The record types of Release 1 and 2.
mod release2 {
    pub(super) const WINDOW1: u16 = 0x07;
    pub(super) const INTEGER: u16 = 0x0D;
    pub(super) const NUMBER: u16 = 0x0E;
    pub(super) const LABEL: u16 = 0x0F;
    pub(super) const FORMULA: u16 = 0x10;
    pub(super) const STRING: u16 = 0x33;
    pub(super) const PASSWORD: u16 = 0x4B;
}

/// The record types of Release 3 and later.
mod release3 {
    pub(super) const PASSWORD: u16 = 0x02;
    pub(super) const FORMAT: u16 = 0x13;
    pub(super) const ERROR: u16 = 0x14;
    pub(super) const NOT_AVAILABLE: u16 = 0x15;
    pub(super) const LABEL: u16 = 0x16;
    pub(super) const NUMBER: u16 = 0x17;
    pub(super) const SMALL_NUMBER: u16 = 0x18;
    pub(super) const FORMULA: u16 = 0x19;
    pub(super) const FORMULA_STRING: u16 = 0x1A;
    pub(super) const EXTENDED: u16 = 0x1B;
    pub(super) const NAMED_SHEET: u16 = 0x23;
    pub(super) const PACKED_NUMBER: u16 = 0x25;
    pub(super) const DOUBLE_NUMBER: u16 = 0x27;
    pub(super) const DOUBLE_FORMULA: u16 = 0x28;
}

/// The subtype of an extended record, and of a named-sheet record, that names a sheet.
const SHEET_NAME: u16 = 14000;
/// The number of columns, and of sheets, of a workbook.
const MAX_COLUMNS: u16 = 256;
/// What begins a label: how it is aligned, or that it repeats or does not print.
const LABEL_PREFIXES: [u8; 5] = *b"'\"^\\|";

/// A record of the stream.
pub(super) struct Record<'a> {
    pub(super) kind: u16,
    /// Where its header starts.
    pub(super) offset: usize,
    pub(super) body: &'a [u8],
}

impl Record<'_> {
    /// `length` bytes of the body from `at`, failing on a body too short for them.
    fn bytes<const N: usize>(&self, at: usize) -> Result<[u8; N], String> {
        self.body
            .get(at..at + N)
            .map(|bytes| bytes.try_into().expect("N bytes"))
            .ok_or_else(|| {
                format!(
                    "the record of type {:#06x} at {} has {} bytes, too few for its fields",
                    self.kind,
                    self.offset,
                    self.body.len()
                )
            })
    }

    fn u8(&self, at: usize) -> Result<u8, String> {
        Ok(self.bytes::<1>(at)?[0])
    }

    fn u16(&self, at: usize) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.bytes(at)?))
    }

    /// The text of the body from `at`, to its first NUL.
    fn text(&self, at: usize, group: u8) -> Result<String, String> {
        let bytes = self.body.get(at..).ok_or_else(|| {
            format!(
                "the record of type {:#06x} at {} ends before its text",
                self.kind, self.offset
            )
        })?;
        Ok(lmbcs::decode(bytes, group))
    }

    /// The text of a label from `at`, without the prefix it begins with.
    fn label(&self, at: usize, group: u8) -> Result<String, String> {
        let prefixed = self
            .body
            .get(at)
            .is_some_and(|b| LABEL_PREFIXES.contains(b));
        self.text(at + usize::from(prefixed), group)
    }
}

/// The records of `input`, from its first, failing on one that runs past its end.
pub(super) fn records(input: &[u8]) -> impl Iterator<Item = Result<Record<'_>, String>> {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at == input.len() {
            return None;
        }
        let offset = at;
        let Some(&[kind_low, kind_high, length_low, length_high]) = input.get(at..at + 4) else {
            at = input.len();
            return Some(Err(format!(
                "the header of the record at {offset} runs past the end of the file"
            )));
        };
        let length = usize::from(u16::from_le_bytes([length_low, length_high]));
        let Some(body) = input.get(at + 4..at + 4 + length) else {
            at = input.len();
            return Some(Err(format!(
                "the record at {offset} of {length} bytes runs past the end of the file"
            )));
        };
        at += 4 + length;
        Some(Ok(Record {
            kind: u16::from_le_bytes([kind_low, kind_high]),
            offset,
            body,
        }))
    })
}

/// What a file's BOF record makes of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Generation {
    /// A worksheet of 1-2-3 Release 1 or 2, or of Symphony, its text in LMBCS group 1.
    Release2,
    /// A workbook of 1-2-3 Release 3 or later, its text in LMBCS of this default group.
    Release3 { group: u8 },
}

impl Generation {
    /// The generation the file's first record, its BOF, names.
    fn of(first: &Record<'_>) -> Result<Self, String> {
        if first.kind != BOF {
            return Err(format!(
                "the file begins with a record of type {:#06x}, not a BOF record",
                first.kind
            ));
        }
        match first.u16(0)? {
            0x0404..=0x0406 => Ok(Generation::Release2),
            0x1000..=0x1005 => Ok(Generation::Release3 {
                group: first.u8(16)?,
            }),
            0x8007 => Err("a 1-2-3 format file, which holds no cells".to_owned()),
            version => Err(format!(
                "a BOF record of version {version:#06x}, of no 1-2-3 worksheet"
            )),
        }
    }

    fn password(self) -> u16 {
        match self {
            Generation::Release2 => release2::PASSWORD,
            Generation::Release3 { .. } => release3::PASSWORD,
        }
    }
}

/// What a check of the records before the cells are read finds.
pub(super) enum Checked {
    Readable(Generation),
    /// The file is protected by a password, its records encrypted after this one.
    Encrypted,
}

/// The generation of `input`, read from its BOF record, and whether a password protects it,
/// failing on a file that is no 1-2-3 file or whose records run past its end before its EOF.
pub(super) fn check(input: &[u8]) -> Result<Checked, String> {
    let mut records = records(input);
    let first = records
        .next()
        .ok_or_else(|| "the file is empty".to_owned())??;
    let generation = Generation::of(&first)?;
    for record in records {
        let record = record?;
        if record.kind == generation.password() {
            return Ok(Checked::Encrypted);
        }
        if record.kind == EOF {
            return Ok(Checked::Readable(generation));
        }
    }
    Err("the file ends before its EOF record".to_owned())
}

/// What a cell holds.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    Number(f64),
    Text(String),
    NotAvailable,
    Error,
}

/// A cell: what it holds, and the format its record gives, which only Release 2's do.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Cell {
    pub(super) value: Value,
    pub(super) style: Option<Style>,
}

/// A sheet: its name, if a record names it, and its cells by row and column.
#[derive(Debug, Default)]
pub(super) struct Sheet {
    pub(super) name: Option<String>,
    pub(super) cells: BTreeMap<(u16, u8), Cell>,
}

/// The format of the columns from `first` to `last` of a row.
#[derive(Debug, Clone, Copy)]
struct Run {
    first: u8,
    last: u8,
    style: Style,
}

/// The formats of a workbook's cells beyond those their records give.
#[derive(Debug)]
enum Formats {
    /// The worksheet's default, of the `WINDOW1` record.
    Release2 { default: Option<Style> },
    /// The formats of the rows, by sheet and row, and the default of each sheet.
    Release3 {
        rows: HashMap<(u8, u16), Rc<[Run]>>,
        defaults: HashMap<u8, Style>,
    },
}

/// What is read of a file: its sheets by number, and their formats.
#[derive(Debug)]
pub(super) struct Workbook {
    pub(super) generation: Generation,
    pub(super) sheets: BTreeMap<u8, Sheet>,
    formats: Formats,
}

impl Workbook {
    /// The format the cell at `row` and `column` of sheet `sheet` is shown in, if any but the
    /// default names one.
    pub(super) fn style(&self, sheet: u8, (row, column): (u16, u8), cell: &Cell) -> Option<Style> {
        let given = |style: &Style| !style.is_default();
        match &self.formats {
            Formats::Release2 { default } => cell.style.filter(given).or(default.filter(given)),
            Formats::Release3 { rows, defaults } => rows
                .get(&(sheet, row))
                .and_then(|runs| {
                    runs.iter()
                        .find(|run| run.first <= column && column <= run.last)
                })
                .map(|run| run.style)
                .filter(given)
                .or(defaults.get(&sheet).copied().filter(given)),
        }
    }

    fn put(&mut self, sheet: u8, place: (u16, u8), cell: Cell) -> Result<(), Failure> {
        within_budget()?;
        self.sheets
            .entry(sheet)
            .or_default()
            .cells
            .insert(place, cell);
        Ok(())
    }

    /// Puts `value` in the cell at `place`, keeping the format the cell has.
    fn put_value(&mut self, sheet: u8, place: (u16, u8), value: Value) -> Result<(), Failure> {
        within_budget()?;
        let cells = &mut self.sheets.entry(sheet).or_default().cells;
        match cells.get_mut(&place) {
            Some(cell) => cell.value = value,
            None => {
                cells.insert(place, Cell { value, style: None });
            }
        }
        Ok(())
    }
}

/// The value of a number, `None` for a blank or a string that a later record gives.
fn value_of(number: Number) -> Option<Value> {
    match number {
        Number::Value(value) => Some(Value::Number(value)),
        Number::NotAvailable => Some(Value::NotAvailable),
        Number::Error => Some(Value::Error),
        Number::String | Number::Blank => None,
    }
}

/// Reads the cells, sheet names and formats of `input`, a file [`check`] found readable as of
/// `generation`, up to its first EOF record.
pub(super) fn read(input: &[u8], generation: Generation) -> Result<Workbook, Failure> {
    let mut workbook = Workbook {
        generation,
        sheets: BTreeMap::new(),
        formats: match generation {
            Generation::Release2 => Formats::Release2 { default: None },
            Generation::Release3 { .. } => Formats::Release3 {
                rows: HashMap::new(),
                defaults: HashMap::new(),
            },
        },
    };
    for record in records(input).skip(1) {
        let record = record.map_err(Failure::Malformed)?;
        if record.kind == EOF {
            return Ok(workbook);
        }
        let read = match generation {
            Generation::Release2 => read_release2(&mut workbook, &record),
            Generation::Release3 { group } => read_release3(&mut workbook, &record, group),
        };
        read.map_err(|reason| match reason {
            Unreadable::Malformed(reason) => Failure::Malformed(reason),
            Unreadable::Failed(failure) => failure,
        })?;
    }
    Err(Failure::Malformed(
        "the file ends before its EOF record".to_owned(),
    ))
}

/// Why a record is not read.
enum Unreadable {
    Malformed(String),
    Failed(Failure),
}

impl From<String> for Unreadable {
    fn from(reason: String) -> Self {
        Unreadable::Malformed(reason)
    }
}

impl From<Failure> for Unreadable {
    fn from(failure: Failure) -> Self {
        Unreadable::Failed(failure)
    }
}

/// The format byte and place of a cell record of Release 2, failing on a column past the last.
fn release2_cell(record: &Record<'_>) -> Result<(Style, (u16, u8)), String> {
    let column = record.u16(1)?;
    let column = u8::try_from(column).map_err(|_| {
        format!(
            "the cell record at {} is in column {column}, past the last of {MAX_COLUMNS}",
            record.offset
        )
    })?;
    Ok((Style::code(record.u8(0)?), (record.u16(3)?, column)))
}

fn read_release2(workbook: &mut Workbook, record: &Record<'_>) -> Result<(), Unreadable> {
    let group = lmbcs::LATIN;
    let number = match record.kind {
        release2::WINDOW1 => {
            workbook.formats = Formats::Release2 {
                default: Some(Style::code(record.u8(4)?)),
            };
            return Ok(());
        }
        release2::LABEL => {
            let (style, place) = release2_cell(record)?;
            let value = Value::Text(record.label(5, group)?);
            let style = Some(style);
            return Ok(workbook.put(0, place, Cell { value, style })?);
        }
        release2::STRING => {
            let (_, place) = release2_cell(record)?;
            let value = Value::Text(record.text(5, group)?);
            return Ok(workbook.put_value(0, place, value)?);
        }
        release2::INTEGER => Number::Value(f64::from(i16::from_le_bytes(record.bytes(5)?))),
        release2::NUMBER | release2::FORMULA => number::double(record.bytes(5)?),
        _ => return Ok(()),
    };
    let (style, place) = release2_cell(record)?;
    if let Some(value) = value_of(number) {
        let style = Some(style);
        workbook.put(0, place, Cell { value, style })?;
    }
    Ok(())
}

/// The sheet and place of a cell record of Release 3 and later.
fn release3_cell(record: &Record<'_>) -> Result<(u8, (u16, u8)), String> {
    Ok((record.u8(2)?, (record.u16(0)?, record.u8(3)?)))
}

fn read_release3(
    workbook: &mut Workbook,
    record: &Record<'_>,
    group: u8,
) -> Result<(), Unreadable> {
    let value = match record.kind {
        release3::FORMAT => return Ok(read_format(workbook, record)?),
        release3::EXTENDED if record.u16(0)? == SHEET_NAME => {
            let sheet = sheet_number(record, record.u16(2)?)?;
            name_sheet(workbook, sheet, record.text(4, group)?);
            return Ok(());
        }
        release3::NAMED_SHEET if record.u16(0)? == SHEET_NAME => {
            let sheet = record.u8(2)?;
            name_sheet(workbook, sheet, record.text(4, group)?);
            return Ok(());
        }
        release3::LABEL => Some(Value::Text(record.label(4, group)?)),
        release3::FORMULA_STRING => Some(Value::Text(record.text(4, group)?)),
        release3::ERROR => Some(Value::Error),
        release3::NOT_AVAILABLE => Some(Value::NotAvailable),
        release3::NUMBER | release3::FORMULA => value_of(number::extended(record.bytes(4)?)),
        release3::SMALL_NUMBER => Some(Value::Number(number::small(i16::from_le_bytes(
            record.bytes(4)?,
        )))),
        release3::PACKED_NUMBER => Some(Value::Number(number::packed(u32::from_le_bytes(
            record.bytes(4)?,
        )))),
        release3::DOUBLE_NUMBER | release3::DOUBLE_FORMULA => {
            value_of(number::double(record.bytes(4)?))
        }
        _ => return Ok(()),
    };
    let (sheet, place) = release3_cell(record)?;
    if let Some(value) = value {
        workbook.put_value(sheet, place, value)?;
    }
    Ok(())
}

/// `number`, a sheet's number a record gives, failing on one past the last sheet.
fn sheet_number(record: &Record<'_>, number: u16) -> Result<u8, String> {
    u8::try_from(number).map_err(|_| {
        format!(
            "the record at {} names sheet {number}, past the last of {MAX_COLUMNS}",
            record.offset
        )
    })
}

fn name_sheet(workbook: &mut Workbook, sheet: u8, name: String) {
    workbook.sheets.entry(sheet).or_default().name = Some(name);
}

/// The format of a cell in a `FORMAT` record from `at`: its format byte, attributes and a byte
/// whose high bit says that a count of more columns that take it follows. Returns the style, the
/// number of columns that take it, and where the next begins.
fn cell_format(record: &Record<'_>, at: usize) -> Result<(Style, u16, usize), String> {
    let [code, attributes, _, repeat] = record.bytes::<4>(at)?;
    let style = Style {
        code,
        parentheses: attributes & 0x02 != 0,
    };
    if repeat & 0x80 == 0 {
        return Ok((style, 1, at + 4));
    }
    Ok((style, 1 + u16::from(record.u8(at + 4)?), at + 5))
}

/// A `FORMAT` record: the formats of a row's cells, column after column, the default of a sheet,
/// or the formats of a row that repeats an earlier row's.
fn read_format(workbook: &mut Workbook, record: &Record<'_>) -> Result<(), String> {
    let Formats::Release3 { rows, defaults } = &mut workbook.formats else {
        unreachable!("a workbook of Release 3 has the formats of Release 3");
    };
    let sheet = record.u8(0)?;
    let row = record.u16(2)?;
    match record.u8(1)? {
        0 => {
            let mut runs = Vec::new();
            let (mut column, mut at) = (0u16, 4);
            while at < record.body.len() {
                let (style, count, next) = cell_format(record, at)?;
                let first = u8::try_from(column).map_err(|_| {
                    format!(
                        "the FORMAT record at {} formats more than {MAX_COLUMNS} columns",
                        record.offset
                    )
                })?;
                let last = u8::try_from((column + count - 1).min(MAX_COLUMNS - 1))
                    .expect("a column below 256");
                runs.push(Run { first, last, style });
                column += count;
                at = next;
            }
            rows.insert((sheet, row), runs.into());
        }
        1 => {
            defaults.insert(sheet, cell_format(record, 4)?.0);
        }
        2 => {
            let original = (record.u8(4)?, record.u16(6)?);
            match rows.get(&original).cloned() {
                Some(runs) => rows.insert((sheet, row), runs),
                None => rows.remove(&(sheet, row)),
            };
        }
        _ => {}
    }
    Ok(())
}
