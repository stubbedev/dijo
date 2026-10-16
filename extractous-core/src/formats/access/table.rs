//! Jackcess's `TableImpl` as it reads: a table definition over its chain of pages, with its
//! columns, indexes and owned pages, and its rows as a `TableScanCursor` visits them, each
//! followed through overflow rows to its data.

use std::borrow::Cow;
use std::collections::HashSet;

use super::buffer::{Buffer, Thrown};
use super::column::{Column, Complex, DataType, Definition, Value};
use super::format::Format;
use super::pages::{Pages, PAGE_TYPE_DATA, PAGE_TYPE_TABLE_DEF};
use super::properties::PropertyMaps;
use super::usage_map::UsageMap;

const OFFSET_MASK: i16 = 0x1FFF;
const DELETED_ROW_MASK: i16 = 0x8000u16 as i16;
const OVERFLOW_ROW_MASK: i16 = 0x4000;
const TYPE_USER: u8 = 0x4E;
const PRIMARY_KEY_INDEX_TYPE: u8 = 1;
const MAX_INDEX_COLUMNS: usize = 10;
const COLUMN_UNUSED: i16 = -1;
const MAX_BYTE: i64 = 256;
const VALIDATION_RULE: &str = "ValidationRule";
const VALIDATION_TEXT: &str = "ValidationText";
const RESULT_TYPE: &str = "ResultType";

/// `findRowStart`: where a row of a data page starts, without its flags.
pub(super) fn row_start(page: &Buffer<'_>, row: i32, format: &Format) -> Result<i16, Thrown> {
    let offset = format.offset_row_start as i64 + 2 * i64::from(row);
    Ok(page.i16_at(offset)? & OFFSET_MASK)
}

/// `findRowEnd`: where a row of a data page ends, the page's end for the first.
pub(super) fn row_end(page: &Buffer<'_>, row: i32, format: &Format) -> Result<i16, Thrown> {
    if row == 0 {
        return Ok(format.page_size as i16);
    }
    let offset = format.offset_row_start as i64 + 2 * (i64::from(row) - 1);
    Ok(page.i16_at(offset)? & OFFSET_MASK)
}

/// An index's columns, and whether Jackcess can look entries up in it: an `IndexData`.
#[derive(Debug)]
struct IndexData {
    /// The indexed columns, as positions in the table's columns.
    columns: Vec<usize>,
    valid: bool,
}

/// A logical index (`IndexImpl`): its number, the index data it uses and its type.
#[derive(Debug)]
struct Index {
    number: i32,
    data: usize,
    index_type: u8,
}

/// What `findIndexForColumns` finds for some columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IndexMatch {
    /// An index over just these columns, and whether Jackcess looks entries up in it.
    Exact {
        valid: bool,
    },
    /// Only an index over these columns and more.
    Partial,
    None,
}

/// What reading a table's definition needs of its database.
pub(super) trait Context {
    /// `getPropertyMaps` of the table whose definition starts on `page`, which a calculated
    /// column's type and a user's table's validation rule are among.
    fn properties(&self, page: i32) -> Result<PropertyMaps, Thrown>;

    /// `ComplexColumnSupport.create`: what the supporting tables of the complex column `column`,
    /// whose id is `complex_id`, of the table on `page` make it.
    fn complex(&self, page: i32, column: &Column, complex_id: i32) -> Result<Complex, Thrown>;
}

/// A table definition.
#[derive(Debug)]
pub(super) struct Table {
    pub(super) name: String,
    /// The columns, ordered by their numbers.
    pub(super) columns: Vec<Column>,
    owned_pages: UsageMap,
    index_datas: Vec<IndexData>,
    /// The logical indexes, ordered by their numbers.
    indexes: Vec<Index>,
    /// The table's properties, where they were read: for a user's table, or one with a
    /// calculated column.
    pub(super) properties: Option<PropertyMaps>,
}

impl Table {
    /// `DatabaseImpl.readTable` and the `TableImpl` constructor: the definition starting on
    /// `page`.
    pub(super) fn read(
        pages: &Pages<'_>,
        name: String,
        page: i32,
        context: &dyn Context,
    ) -> Result<Self, Thrown> {
        let format = pages.format;
        let first = pages.page(page)?;
        if first[0] != PAGE_TYPE_TABLE_DEF {
            return Err(Thrown::other(format!(
                "java.io.IOException: Looking for {name} at page {page}, but page type is {}",
                first[0] as i8
            )));
        }
        let definition = complete_definition(pages, first)?;
        let mut buffer = Buffer::new(&definition);
        let at = |offset: usize| offset as i64;
        buffer.i32_at(at(format.offset_num_rows))?;
        buffer.i32_at(at(format.offset_next_auto_number))?;
        if let Some(offset) = format.offset_next_complex_auto_number {
            buffer.i32_at(at(offset))?;
        }
        let table_type = buffer.u8_at(at(format.offset_table_type))?;
        buffer.i16_at(at(format.offset_max_cols))?;
        buffer.i16_at(at(format.offset_num_var_cols))?;
        let column_count = buffer.i16_at(at(format.offset_num_cols))?;
        let logical_index_count = buffer.i32_at(at(format.offset_num_index_slots))?;
        let index_count = buffer.i32_at(at(format.offset_num_indexes))?;
        buffer.set_position(at(format.offset_owned_pages))?;
        let owned_pages = UsageMap::read_at(pages, &mut buffer)?;
        buffer.set_position(at(format.offset_free_space_pages))?;
        UsageMap::read_at(pages, &mut buffer)?;
        for i in 0..i64::from(index_count.max(0)) {
            buffer.i32_at(
                at(format.offset_index_def_block) + i * format.size_index_definition as i64 + 4,
            )?;
        }
        let mut table = Self {
            name,
            columns: Vec::new(),
            owned_pages,
            index_datas: Vec::new(),
            indexes: Vec::new(),
            properties: None,
        };
        let page_context = (context, page);
        table.read_columns(pages, &mut buffer, column_count, index_count, page_context)?;
        table.read_indexes(pages, &mut buffer, index_count, logical_index_count)?;
        while buffer.remaining() >= 2 && read_column_usage_maps(pages, &mut buffer)? {}
        table.check_version_histories()?;
        if table_type == TYPE_USER {
            table.check_validation_rule(page_context)?;
        }
        Ok(table)
    }

    /// `getPropertyMaps`, read once.
    fn properties(
        &mut self,
        (context, page): (&dyn Context, i32),
    ) -> Result<&PropertyMaps, Thrown> {
        if self.properties.is_none() {
            self.properties = Some(context.properties(page)?);
        }
        Ok(self.properties.as_ref().expect("just read"))
    }

    /// `VersionHistoryColumnInfoImpl.postTableLoadInit`: the memo each version history names
    /// is a column of the table.
    fn check_version_histories(&self) -> Result<(), Thrown> {
        for column in &self.columns {
            let Some(Complex::VersionHistory { value_column }) = &column.complex else {
                continue;
            };
            let Some(value_column) = value_column else {
                return Err(Thrown::other(
                    "java.lang.NullPointerException: a version history without a memo",
                ));
            };
            if !self
                .columns
                .iter()
                .any(|other| crate::java::equals_ignore_case(&other.name, value_column))
            {
                return Err(Thrown::other(format!(
                    "java.lang.IllegalArgumentException: Column with name {value_column} does \
                     not exist in this table"
                )));
            }
        }
        Ok(())
    }

    /// `reloadRowValidator`'s reading: the validation rule and, where there is one, its text,
    /// each cast to a string.
    fn check_validation_rule(&mut self, context: (&dyn Context, i32)) -> Result<(), Thrown> {
        let properties = self.properties(context)?.default_map();
        let text = |name: &str| match properties.value(name) {
            None => Ok(None),
            Some(Value::Text(text)) => Ok(Some(text.clone())),
            Some(_) => Err(Thrown::other(format!(
                "java.lang.ClassCastException: a {name} that is no String"
            ))),
        };
        if text(VALIDATION_RULE)?.is_some_and(|rule| !crate::java::trim(&rule).is_empty()) {
            text(VALIDATION_TEXT)?;
        }
        Ok(())
    }

    /// `readColumnDefinitions`: the names, then the definitions, ordered by column number.
    fn read_columns(
        &mut self,
        pages: &Pages<'_>,
        buffer: &mut Buffer<'_>,
        column_count: i16,
        index_count: i32,
        context: (&dyn Context, i32),
    ) -> Result<(), Thrown> {
        let format = pages.format;
        let column_offset = format.offset_index_def_block as i64
            + i64::from(index_count) * format.size_index_definition as i64;
        buffer.set_position(
            column_offset + i64::from(column_count) * format.size_column_header as i64,
        )?;
        let mut names = Vec::new();
        for _ in 0..column_count.max(0) {
            names.push(read_name(buffer, format)?);
        }
        for (index, name) in names.into_iter().enumerate() {
            let offset = column_offset + index as i64 * format.size_column_header as i64;
            let definition = Definition::read(buffer, offset, format)?;
            let mut type_byte = definition.type_byte;
            if definition.calculated {
                match self.properties(context)?.column(&name).value(RESULT_TYPE) {
                    None => {}
                    Some(Value::Byte(result_type)) => type_byte = *result_type as u8,
                    Some(_) => {
                        return Err(Thrown::other(
                            "java.lang.ClassCastException: a ResultType that is no Byte",
                        ))
                    }
                }
            }
            let mut column = Column::create(buffer, offset, format, name, &definition, type_byte)?;
            if column.data_type == DataType::ComplexType && !column.is_calculated() {
                let Some(id_offset) = format.offset_column_complex_id else {
                    return Err(Thrown::unported(
                        "a complex column in a database before Access 2007",
                    ));
                };
                let complex_id = buffer.i32_at(offset + id_offset as i64)?;
                let (context, page) = context;
                column.complex = Some(context.complex(page, &column, complex_id)?);
            }
            self.columns.push(column);
        }
        self.columns.sort_by_key(|column| column.number);
        Ok(())
    }

    /// `readIndexDefinitions`: each index's columns and usage map, then the logical indexes
    /// and their names.
    fn read_indexes(
        &mut self,
        pages: &Pages<'_>,
        buffer: &mut Buffer<'_>,
        index_count: i32,
        logical_index_count: i32,
    ) -> Result<(), Thrown> {
        let format = pages.format;
        for _ in 0..index_count.max(0) {
            buffer.forward(format.skip_before_index)?;
            let mut columns = Vec::new();
            let mut valid = true;
            for _ in 0..MAX_INDEX_COLUMNS {
                let number = buffer.i16()?;
                buffer.u8()?;
                if number == COLUMN_UNUSED {
                    continue;
                }
                let Some(position) = self.columns.iter().position(|c| c.number == number) else {
                    return Err(Thrown::other(format!(
                        "java.io.IOException: Could not find column with number {number} for \
                         index"
                    )));
                };
                valid &= is_indexable(&self.columns[position]);
                columns.push(position);
            }
            UsageMap::read_at(pages, buffer)?;
            buffer.i32()?;
            buffer.forward(format.skip_before_index_flags)?;
            buffer.u8()?;
            buffer.forward(format.skip_after_index_flags)?;
            self.index_datas.push(IndexData { columns, valid });
        }
        for _ in 0..logical_index_count.max(0) {
            buffer.forward(format.skip_before_index_slot)?;
            let number = buffer.i32()?;
            let data = buffer.i32()?;
            buffer.take(1 + 4 + 4 + 1 + 1)?;
            let index_type = buffer.u8()?;
            buffer.forward(format.skip_after_index_slot)?;
            let data = usize::try_from(data)
                .ok()
                .filter(|&data| data < self.index_datas.len())
                .ok_or_else(|| {
                    Thrown::other(format!(
                        "java.lang.IndexOutOfBoundsException: Index {data} out of bounds for \
                         length {}",
                        self.index_datas.len()
                    ))
                })?;
            self.indexes.push(Index {
                number,
                data,
                index_type,
            });
        }
        for _ in 0..logical_index_count.max(0) {
            read_name(buffer, format)?;
        }
        self.indexes.sort_by_key(|index| index.number);
        Ok(())
    }

    /// `findIndexForColumns(columns, ANY_MATCH)`: the first index, by number, over just these
    /// columns, named in any case, else whether one over them and more exists.
    pub(super) fn find_index(&self, names: &[&str]) -> IndexMatch {
        let mut partial = false;
        for index in &self.indexes {
            let data = &self.index_datas[index.data];
            if data.columns.len() < names.len() {
                continue;
            }
            let matches = data.columns.iter().zip(names).all(|(&column, name)| {
                crate::java::equals_ignore_case(name, &self.columns[column].name)
            });
            if !matches {
                continue;
            }
            if data.columns.len() == names.len() {
                return IndexMatch::Exact { valid: data.valid };
            }
            partial = true;
        }
        if partial {
            IndexMatch::Partial
        } else {
            IndexMatch::None
        }
    }

    /// `getPrimaryKeyIndex`'s columns: those of the first index, by number, of the primary key's
    /// type.
    pub(super) fn primary_key(&self) -> Option<&[usize]> {
        self.indexes
            .iter()
            .find(|index| index.index_type == PRIMARY_KEY_INDEX_TYPE)
            .map(|index| self.index_datas[index.data].columns.as_slice())
    }

    /// The table's rows, as a forward `TableScanCursor` reads them, with the values of the
    /// columns `wanted` picks.
    pub(super) fn rows<'s>(&'s self, pages: &'s Pages<'s>, wanted: Vec<bool>) -> Rows<'s> {
        Rows {
            table: self,
            pages,
            wanted,
            owned: self.owned_pages.pages().collect::<Vec<_>>().into_iter(),
            page: None,
            row: 0,
            ended: false,
        }
    }
}

/// Whether `IndexData.newColumnDescriptor` keeps an index valid for a column: a text column in
/// a general sort order, or a column of a type the index codes know.
fn is_indexable(column: &Column) -> bool {
    match column.data_type {
        DataType::Text | DataType::Memo => column
            .sort_order
            .is_some_and(|order| order.is_supported_by_indexes()),
        DataType::Ole
        | DataType::BigBinary
        | DataType::UnsupportedFixedLength
        | DataType::UnsupportedVariableLength => false,
        _ => true,
    }
}

/// `loadCompleteTableDefinitionBuffer`: the first page, then each page the chain goes on to
/// without its eight byte header.
fn complete_definition<'a>(
    pages: &Pages<'a>,
    first: Cow<'a, [u8]>,
) -> Result<Cow<'a, [u8]>, Thrown> {
    let format = pages.format;
    let next = |page: &[u8]| -> i32 {
        let start = format.offset_next_table_def_page;
        i32::from_le_bytes(page[start..start + 4].try_into().expect("four bytes"))
    };
    let mut next_page = next(&first);
    if next_page == 0 {
        return Ok(first);
    }
    let mut definition = first.into_owned();
    let mut seen = HashSet::new();
    while next_page != 0 {
        if !seen.insert(next_page) {
            return Err(Thrown::other(
                "a table definition whose pages loop, which Jackcess follows until it runs out \
                 of memory",
            ));
        }
        let page = pages.page(next_page)?;
        next_page = next(&page);
        definition.extend_from_slice(&page[8..]);
    }
    Ok(Cow::Owned(definition))
}

/// `TableImpl.readName`: a name after its length, in the database's charset.
fn read_name(buffer: &mut Buffer<'_>, format: &Format) -> Result<String, Thrown> {
    let length = buffer.unsigned(format.size_name_length)?;
    let bytes = buffer.take(i64::from(length))?;
    Ok(format.charset.decode(bytes))
}

/// `readColumnUsageMaps`: one column's owned and free space maps, `false` at the end of them.
/// An `IllegalStateException` reading them is caught and the maps skipped.
fn read_column_usage_maps(pages: &Pages<'_>, buffer: &mut Buffer<'_>) -> Result<bool, Thrown> {
    if buffer.i16()? == COLUMN_UNUSED {
        return Ok(false);
    }
    let position = buffer.position() as i64;
    let read = UsageMap::read_at(pages, buffer).and_then(|_| UsageMap::read_at(pages, buffer));
    match read {
        Ok(_) => Ok(true),
        Err(Thrown::IllegalState(_)) => {
            buffer.set_position(position + 8)?;
            Ok(true)
        }
        Err(thrown) => Err(thrown),
    }
}

/// The values of a row's columns, `None` for a null or a column not read.
pub(super) type Row = Vec<Option<Value>>;

/// A table's rows, read on demand.
pub(super) struct Rows<'s> {
    table: &'s Table,
    pages: &'s Pages<'s>,
    wanted: Vec<bool>,
    owned: std::vec::IntoIter<i32>,
    /// The page whose rows are being read, and how many it has.
    page: Option<(Cow<'s, [u8]>, i32)>,
    row: i32,
    ended: bool,
}

impl Iterator for Rows<'_> {
    type Item = Result<Row, Thrown>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let result = self.advance();
        if !matches!(result, Ok(Some(_))) {
            self.ended = true;
        }
        result.transpose()
    }
}

impl Rows<'_> {
    /// `findAnotherPosition` and `getRow`: the next row not deleted, read.
    fn advance(&mut self) -> Result<Option<Row>, Thrown> {
        let format = self.pages.format;
        loop {
            let page = match &self.page {
                Some((page, rows)) if self.row < *rows => page,
                _ => {
                    let Some(page_number) = self.owned.next() else {
                        return Ok(None);
                    };
                    if page_number < 0 || !self.table.owned_pages.contains(page_number) {
                        return Ok(None);
                    }
                    let page = self.pages.page(page_number)?;
                    let rows = if page[0] == PAGE_TYPE_DATA {
                        let offset = format.offset_num_rows_on_data_page as i64;
                        i32::from(Buffer::new(&page).i16_at(offset)?)
                    } else {
                        0
                    };
                    self.page = Some((page, rows));
                    self.row = 0;
                    continue;
                }
            };
            let row = self.row;
            self.row += 1;
            let start =
                Buffer::new(page).i16_at(format.offset_row_start as i64 + 2 * i64::from(row))?;
            if start & DELETED_ROW_MASK != 0 {
                continue;
            }
            let page = Cow::Borrowed(&page[..]);
            return read_row(self.table, self.pages, &self.wanted, page, row).map(Some);
        }
    }
}

/// `positionAtRowData` and `getRow`: the row followed through overflow rows to its data, and
/// the values of the columns `wanted` picks.
fn read_row(
    table: &Table,
    pages: &Pages<'_>,
    wanted: &[bool],
    header_page: Cow<'_, [u8]>,
    header_row: i32,
) -> Result<Row, Thrown> {
    let format = pages.format;
    let mut page = header_page;
    let mut row = header_row;
    let mut seen = HashSet::new();
    let (start, end) = loop {
        let buffer = Buffer::new(&page);
        let raw_start = buffer.i16_at(format.offset_row_start as i64 + 2 * i64::from(row))?;
        let end = row_end(&buffer, row, format)?;
        let start = raw_start & OFFSET_MASK;
        if raw_start & OVERFLOW_ROW_MASK == 0 {
            break (start, end);
        }
        if end - start < 4 {
            return Err(Thrown::other(
                "java.io.IOException: invalid overflow row info",
            ));
        }
        let overflow_row = i32::from(buffer.u8_at(i64::from(start))?);
        let overflow_page = buffer.u24_at(i64::from(start) + 1)?;
        if !seen.insert((overflow_page, overflow_row)) {
            return Err(Thrown::other(
                "overflow rows that loop, which Jackcess follows for ever",
            ));
        }
        page = Cow::Owned(pages.page(overflow_page)?.into_owned());
        row = overflow_row;
    };
    let mut buffer = Buffer::new(&page);
    buffer.set_limit(i64::from(end))?;
    buffer.set_position(i64::from(start))?;
    let mut values = Vec::with_capacity(table.columns.len());
    let mut data: Option<RowData<'_>> = None;
    for (column, &wanted) in table.columns.iter().zip(wanted) {
        if !wanted {
            values.push(None);
            continue;
        }
        let data = match &mut data {
            Some(data) => data,
            None => data.insert(RowData::new(buffer, format)?),
        };
        values.push(data.value(column, pages)?);
    }
    Ok(values)
}

/// A row's bytes from its start to its end, with its null mask read.
struct RowData<'p> {
    buffer: Buffer<'p>,
    start: i64,
    column_count: i32,
    null_mask: &'p [u8],
    jump_offsets: Option<Vec<i16>>,
}

impl<'p> RowData<'p> {
    /// `getRowNullMask`: the column count at the row's start and the mask at its end.
    fn new(buffer: Buffer<'p>, format: &Format) -> Result<Self, Thrown> {
        let start = buffer.position() as i64;
        let column_count = buffer.unsigned_at(start, format.size_row_column_count)?;
        let mask_size = (i64::from(column_count) + 7) / 8;
        let mut mask = buffer;
        mask.set_position(buffer.limit() as i64 - mask_size)?;
        let null_mask = mask.take(mask_size)?;
        Ok(Self {
            buffer,
            start,
            column_count,
            null_mask,
            jump_offsets: None,
        })
    }

    /// `NullMask.isNull`: past the row's columns, or its bit clear; a negative column number
    /// indexes the mask as Java's arithmetic does.
    fn is_null(&self, number: i16) -> Result<bool, Thrown> {
        let number = i32::from(number);
        if number >= self.column_count {
            return Ok(true);
        }
        let index = number / 8;
        let byte = usize::try_from(index)
            .ok()
            .and_then(|index| self.null_mask.get(index))
            .ok_or_else(|| {
                Thrown::other(format!(
                    "java.lang.ArrayIndexOutOfBoundsException: Index {index} out of bounds for \
                     length {}",
                    self.null_mask.len()
                ))
            })?;
        let bit = 1i32.wrapping_shl((number % 8) as u32) as u8;
        Ok(byte & bit == 0)
    }

    /// `getRowColumn`.
    fn value(&mut self, column: &Column, pages: &Pages<'_>) -> Result<Option<Value>, Thrown> {
        let format = pages.format;
        let null = self.is_null(column.number)?;
        if column.stores_in_null_mask() {
            return Ok(Some(Value::Boolean(!null)));
        }
        if null {
            return Ok(None);
        }
        let (position, length) = if !column.variable_length {
            (
                self.start
                    + format.offset_column_fixed_data_row_offset as i64
                    + i64::from(column.fixed_offset),
                i64::from(column.fixed_data_size()),
            )
        } else if format.size_row_var_col_offset == 2 {
            let offset_position = (self.buffer.limit() as i64 - self.null_mask.len() as i64 - 4)
                - i64::from(column.var_len_index) * 2;
            let start = self.buffer.i16_at(offset_position)?;
            let end = self.buffer.i16_at(offset_position - 2)?;
            (
                self.start + i64::from(start),
                i64::from(end) - i64::from(start),
            )
        } else {
            let offsets = self.jump_offsets()?;
            let index = usize::try_from(column.var_len_index)
                .ok()
                .filter(|&index| index + 1 < offsets.len())
                .ok_or_else(|| {
                    Thrown::other(format!(
                        "java.lang.ArrayIndexOutOfBoundsException: Index {} out of bounds for \
                         length {}",
                        column.var_len_index,
                        offsets.len()
                    ))
                })?;
            let (start, end) = (offsets[index], offsets[index + 1]);
            (
                self.start + i64::from(start),
                i64::from(end) - i64::from(start),
            )
        };
        let mut data = self.buffer;
        data.set_position(position)?;
        let bytes = data.take(length)?;
        column.read(bytes, pages)
    }

    /// `readJumpTableVarColOffsets`: Jet 3's variable column offsets, a byte each, with the
    /// jump table that adds 256 to those past each jump.
    fn jump_offsets(&mut self) -> Result<&[i16], Thrown> {
        if self.jump_offsets.is_none() {
            let buffer = &self.buffer;
            let mask_size = self.null_mask.len() as i64;
            let row_end = buffer.limit() as i64 - 1;
            let var_columns = i64::from(buffer.u8_at(row_end - mask_size)?);
            let row_length = row_end - self.start + 1;
            let mut jumps = (row_length - 1) / MAX_BYTE;
            let column_offset = row_end - mask_size - jumps - 1;
            if (column_offset - self.start - var_columns) / MAX_BYTE < jumps {
                jumps -= 1;
            }
            let mut used = 0;
            let mut offsets = Vec::with_capacity(var_columns as usize + 1);
            for i in 0..=var_columns {
                while used < jumps && i == i64::from(buffer.u8_at(row_end - mask_size - used - 1)?)
                {
                    used += 1;
                }
                let low = i64::from(buffer.u8_at(column_offset - i)?);
                offsets.push((low + used * MAX_BYTE) as i16);
            }
            self.jump_offsets = Some(offsets);
        }
        Ok(self.jump_offsets.as_deref().expect("just read"))
    }
}
