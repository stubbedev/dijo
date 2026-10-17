//! java-libpst's tables: `PSTTable`, the heap on a node; `PSTTableBC`, a property context; and
//! `PSTTable7C`, a table context, read as java-libpst reads them.

use std::collections::HashMap;

use super::file::{le, le_checked, Format, PstFile, Read, SubNodes, Thrown};
use super::node::NodeStream;
use crate::java;

const SIGNATURE: u8 = 0xEC;
const TABLE_CONTEXT: u8 = 0x7C;
const PROPERTY_CONTEXT: u8 = 0xBC;
const TREE_HEADER: i32 = 0xB5;
const HEADER: i32 = 0x20;
const BLOCK_SIZE: i64 = 8176;
/// `PidTagLtpRowId`, the column every row of a table context gets.
pub(super) const ROW_ID: i32 = 0x67F2;
const UNICODE: i32 = 0x1F;
const STRING8: i32 = 0x1E;

/// `PSTTableItem`: a property of a property context or a cell of a table context.
#[derive(Debug, Clone, Default)]
pub(super) struct Item {
    pub(super) value_type: i32,
    pub(super) reference: i32,
    pub(super) data: Vec<u8>,
    pub(super) external: bool,
}

impl Item {
    /// `getStringValue`.
    pub(super) fn string_value(&self) -> String {
        match self.value_type {
            UNICODE if self.external => "External string reference!".to_owned(),
            UNICODE => java::trim(&java::utf16le(&self.data)).to_owned(),
            STRING8 => java::trim(&java::utf8(&self.data)).to_owned(),
            _ => {
                let mut text = String::new();
                let mut hex = String::new();
                for &byte in &self.data {
                    let character = char::from(byte);
                    if character.is_alphabetic() || character.is_ascii_digit() {
                        text.push(character);
                        text.push(' ');
                    } else {
                        text.push_str(". ");
                    }
                    let digits = format!("{byte:x}");
                    hex.push_str(&digits);
                    hex.push(' ');
                    if digits.len() > 1 {
                        text.push(' ');
                    }
                }
                text.push_str("\n\t");
                text.push_str(&hex);
                text
            }
        }
    }
}

/// The table's kind, by `bClientSig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Table,
    Properties,
}

/// `NodeInfo`: an allocation of the heap, or the whole of a sub-node.
struct Allocation<'f> {
    sub_node: Option<NodeStream<'f>>,
    start: usize,
    end: usize,
}

impl<'f> Allocation<'f> {
    fn stream<'a>(&'a self, heap: &'a Heap<'_, 'f>) -> &'a NodeStream<'f> {
        self.sub_node.as_ref().unwrap_or(&heap.stream)
    }

    fn len(&self) -> usize {
        self.end - self.start
    }

    fn read_all(&self, heap: &Heap<'_, 'f>) -> Read<Vec<u8>> {
        self.stream(heap).read(self.start, self.len())
    }

    fn read_long(&self, heap: &Heap<'_, 'f>, offset: usize, length: usize) -> Read<i64> {
        self.stream(heap).read_long(self.start + offset, length)
    }
}

/// `PSTTable`: the heap on a node and its tree's header.
struct Heap<'p, 'f> {
    file: &'p PstFile<'f>,
    stream: NodeStream<'f>,
    blocks: Vec<i64>,
    sub_nodes: SubNodes,
    kind: Kind,
    user_root: i32,
    key_size: usize,
    value_size: usize,
    root: i32,
}

/// A position java-libpst computes as an `int`, which a stream cannot seek to when negative.
fn position(value: i64) -> Read<usize> {
    usize::try_from(value).map_err(|_| Thrown::out_of_bounds("a negative position of a heap"))
}

impl<'p, 'f> Heap<'p, 'f> {
    fn read(file: &'p PstFile<'f>, stream: NodeStream<'f>, sub_nodes: SubNodes) -> Read<Self> {
        let blocks = stream.block_offsets();
        let head = stream.read(0, 4)?;
        if head[2] != SIGNATURE {
            return Err(Thrown::pst("Unable to parse table, bad table type..."));
        }
        let kind = match head[3] {
            TABLE_CONTEXT => Kind::Table,
            PROPERTY_CONTEXT => Kind::Properties,
            other => {
                return Err(Thrown::pst(format!(
                    "Unable to parse table, bad table type.  Unknown identifier: 0x{:x}",
                    i64::from(other as i8)
                )))
            }
        };
        let user_root = stream.read_long(4, 4)? as i32;
        let mut heap = Self {
            file,
            stream,
            blocks,
            sub_nodes,
            kind,
            user_root,
            key_size: 0,
            value_size: 0,
            root: 0,
        };
        let header = heap
            .allocation(HEADER)?
            .ok_or_else(|| Thrown::null("the tree header"))?;
        let stream = header.stream(&heap);
        let first = stream.byte(header.start)? & 0xFF;
        if first != TREE_HEADER {
            stream.read(header.start, 1024)?;
            return Err(Thrown::pst(format!(
                "Unable to parse table, can't find BTHHEADER header information: {first}"
            )));
        }
        let key_size = (stream.byte(header.start + 1)? & 0xFF) as usize;
        let value_size = (stream.byte(header.start + 2)? & 0xFF) as usize;
        stream.byte(header.start + 3)?;
        let root = header.read_long(&heap, 4, 4)? as i32;
        heap.key_size = key_size;
        heap.value_size = value_size;
        heap.root = root;
        Ok(heap)
    }

    /// `getNodeInfo`: the allocation or sub-node an HNID names, none for a node id that is not a
    /// sub-node of the table's.
    fn allocation(&self, hnid: i32) -> Read<Option<Allocation<'f>>> {
        if hnid == 0 {
            return Ok(Some(Allocation {
                sub_node: None,
                start: 0,
                end: 0,
            }));
        }
        if let Some(sub_node) = self.sub_nodes.get(hnid) {
            let stream = NodeStream::from_sub_node(self.file, sub_node).map_err(|thrown| {
                if thrown.kind == super::file::Kind::Io {
                    Thrown::pst(format!("IOException reading subNode: 0x{hnid:08X}"))
                } else {
                    thrown
                }
            })?;
            let end = stream.len();
            return Ok(Some(Allocation {
                sub_node: Some(stream),
                start: 0,
                end,
            }));
        }
        if hnid & 0x1F != 0 {
            return Ok(None);
        }
        let which = (hnid as u32 >> 16) as usize;
        if which > self.blocks.len() {
            return Err(Thrown::pst(format!(
                "getNodeInfo: block doesn't exist! hnid = 0x{hnid:08X}\n\
                 getNodeInfo: block doesn't exist! whichBlock = 0x{which:08X}\n\n{}",
                self.blocks.len()
            )));
        }
        let index = i64::from((hnid & 0xFFFF) >> 5);
        let block_offset = if which > 0 {
            i64::from(self.blocks[which - 1] as i32)
        } else {
            0
        };
        let mut map = self.stream.read_long(position(block_offset)?, 2)? + block_offset;
        let allocations = self.stream.read_long(position(map)?, 2)?;
        if index > allocations {
            return Err(Thrown::pst(format!(
                "getNodeInfo: node index doesn't exist! nid = 0x{hnid:08X}\n"
            )));
        }
        map += 2 * index + 2;
        let start = self.stream.read_long(position(map)?, 2)? + block_offset;
        let end = self.stream.read_long(position(map + 2)?, 2)? + block_offset;
        if start > end {
            return Err(Thrown::pst(format!(
                "Invalid NodeInfo parameters: start {start} is greater than end {end}"
            )));
        }
        Ok(Some(Allocation {
            sub_node: None,
            start: position(start)?,
            end: position(end)?,
        }))
    }

    /// The size of a key and its value in the tree, by which a division counts its entries.
    fn entry_size(&self) -> Read<usize> {
        match self.key_size + self.value_size {
            0 => Err(Thrown::runtime(
                "java.lang.ArithmeticException",
                "/ by zero",
            )),
            size => Ok(size),
        }
    }
}

/// `PSTTableBC`: a property context, its properties by id.
#[derive(Debug, Default)]
pub(super) struct PropertyContext {
    pub(super) items: HashMap<i32, Item>,
}

impl PropertyContext {
    pub(super) fn read(file: &PstFile<'_>, stream: NodeStream<'_>) -> Read<Self> {
        let heap = Heap::read(file, stream, SubNodes::default())?;
        if heap.kind != Kind::Properties {
            return Err(Thrown::pst(
                "unable to create PSTTableBC, table does not appear to be a bc!",
            ));
        }
        let keys = heap
            .allocation(heap.root)?
            .ok_or_else(|| Thrown::null("the property tree"))?
            .read_all(&heap)?;
        let count = keys.len() / heap.entry_size()?;
        let mut items = HashMap::new();
        let mut offset = 0;
        for _ in 0..count {
            let what = "a property";
            let entry_type = le_checked(&keys, offset, offset + 2, what)? as i32;
            let value_type = le_checked(&keys, offset + 2, offset + 4, what)? as i32;
            let mut reference = le_checked(&keys, offset + 4, offset + 8, what)? as i32;
            let mut item = Item {
                value_type,
                reference,
                data: Vec::new(),
                external: true,
            };
            match value_type {
                0x0002 => reference &= 0xFFFF,
                0x0003 | 0x000A | 0x0001 | 0x0004 => {}
                0x000B => reference &= 0xFF,
                _ => {
                    if let Some(allocation) = heap.allocation(reference)? {
                        item.data = allocation.read_all(&heap)?;
                        item.external = false;
                    }
                }
            }
            item.reference = reference;
            offset += 8;
            items.insert(entry_type, item);
        }
        Ok(Self { items })
    }
}

/// `ColumnDescriptor`.
#[derive(Debug, Clone, Copy)]
struct Column {
    value_type: i32,
    id: i32,
    offset: usize,
    size: i32,
    bit: usize,
}

/// A row of a table context: its cells by property id.
pub(super) type Row = HashMap<i32, Item>;

/// `PSTTable7C`: a table context.
pub(super) struct TableContext<'p, 'f> {
    heap: Heap<'p, 'f>,
    rows: Allocation<'f>,
    columns: Vec<Column>,
    column_count: usize,
    first_column: usize,
    bitmap_offset: i64,
    row_size: i64,
    row_count: usize,
}

impl<'p, 'f> TableContext<'p, 'f> {
    /// `new PSTTable7C(in, subNodeDescriptorItems, entityToExtract)`: `entity` names the column
    /// the rows are read from on, or none.
    pub(super) fn read(
        file: &'p PstFile<'f>,
        stream: NodeStream<'f>,
        sub_nodes: SubNodes,
        entity: Option<i32>,
    ) -> Read<Self> {
        let heap = Heap::read(file, stream, sub_nodes)?;
        if heap.kind != Kind::Table {
            return Err(Thrown::pst(
                "unable to create PSTTable7C, table does not appear to be a 7c!",
            ));
        }
        let header = heap
            .allocation(heap.user_root)?
            .ok_or_else(|| Thrown::null("the table header"))?;
        let fields = header.stream(&heap).read(header.start, 18)?;
        let mut column_count = le(&fields, 1, 2) as usize;
        let bitmap_offset = le(&fields, 6, 8);
        let row_size = le(&fields, 8, 10);
        let hnid_rows = le(&fields, 14, 18) as i32;
        let mut columns = Vec::with_capacity(column_count);
        let mut first_column = 0;
        let stream = header.stream(&heap);
        for index in 0..column_count {
            let at = 22 + 8 * index;
            let column = Column {
                value_type: (header.read_long(&heap, at, 2)? & 0xFFFF) as i32,
                id: (header.read_long(&heap, at + 2, 2)? & 0xFFFF) as i32,
                offset: (header.read_long(&heap, at + 4, 2)? & 0xFFFF) as usize,
                size: stream.byte(header.start + at + 6)? & 0xFF,
                bit: (stream.byte(header.start + at + 7)? & 0xFF) as usize,
            };
            if Some(column.id) == entity {
                first_column = index;
            }
            columns.push(column);
        }
        let overridden = entity.is_some() && columns.iter().any(|column| Some(column.id) == entity);
        if overridden {
            column_count = first_column + 1;
        } else {
            first_column = 0;
        }
        heap.allocation(heap.root)?
            .ok_or_else(|| Thrown::null("the row index"))?;
        heap.entry_size()?;
        let rows = heap
            .allocation(hnid_rows)?
            .ok_or_else(|| Thrown::null("the rows"))?;
        if row_size == 0 {
            return Err(Thrown::runtime(
                "java.lang.ArithmeticException",
                "/ by zero",
            ));
        }
        let length = rows.len() as i64;
        let per_block = BLOCK_SIZE / row_size;
        let row_count =
            ((length / BLOCK_SIZE) * per_block + (length % BLOCK_SIZE) / row_size).max(0) as usize;
        Ok(Self {
            heap,
            rows,
            columns,
            column_count,
            first_column,
            bitmap_offset,
            row_size,
            row_count,
        })
    }

    /// `getRowCount`.
    pub(super) fn row_count(&self) -> usize {
        self.row_count
    }

    /// `getItems()`: every row.
    pub(super) fn all_rows(&self) -> Read<Vec<Row>> {
        self.rows(0, self.row_count)
    }

    /// `getItems(start, count)`: the rows from `start`, at most `count` of them.
    pub(super) fn rows(&self, start: usize, count: usize) -> Read<Vec<Row>> {
        let per_block = BLOCK_SIZE / self.row_size;
        let padding = BLOCK_SIZE - per_block * self.row_size;
        let start = start as i64;
        let count = (count as i64).min(self.row_count as i64 - start);
        let ansi = self.heap.file.format() == Format::Ansi;
        let stream = self.rows.stream(&self.heap);
        let mut at = if per_block == 0 {
            return Err(Thrown::runtime(
                "java.lang.ArithmeticException",
                "/ by zero",
            ));
        } else {
            (start / per_block) * BLOCK_SIZE + (start % per_block) * self.row_size
        };
        let mut rows = Vec::new();
        for _ in 0..count {
            if ansi {
                if at >= BLOCK_SIZE {
                    at += 4 * (at / BLOCK_SIZE);
                }
                if self.rows.start as i64 + at + self.bitmap_offset > stream.len() as i64 {
                    continue;
                }
            } else if at % BLOCK_SIZE > BLOCK_SIZE - self.row_size {
                at += padding;
                if at + self.row_size > self.rows.len() as i64 {
                    continue;
                }
            }
            rows.push(self.row(position(at)?)?);
            at += self.row_size;
        }
        Ok(rows)
    }

    /// The row at `at` in the rows.
    fn row(&self, at: usize) -> Read<Row> {
        let stream = self.rows.stream(&self.heap);
        let base = self.rows.start + at;
        let bitmap = stream.read(
            position(base as i64 + self.bitmap_offset)?,
            self.column_count.div_ceil(8),
        )?;
        let id = stream.read_long(base, 4)? as i32;
        let mut row = HashMap::new();
        row.insert(
            ROW_ID,
            Item {
                value_type: 3,
                reference: id,
                data: Vec::new(),
                external: true,
            },
        );
        for column in &self.columns[self.first_column..self.column_count] {
            let byte = column.bit / 8;
            if byte >= bitmap.len() || bitmap[byte] & (1 << (column.bit % 8)) == 0 {
                continue;
            }
            let mut item = Item {
                value_type: column.value_type,
                ..Item::default()
            };
            let cell = base + column.offset;
            match column.size {
                1 => {
                    item.reference = (stream.read_long(cell, 1)? & 0xFF) as i32;
                    item.external = true;
                }
                2 => {
                    item.reference = (stream.read_long(cell, 2)? & 0xFFFF) as i32;
                    item.external = true;
                }
                8 => item.data = stream.read(cell, 8)?,
                _ => {
                    item.reference = stream.read_long(cell, 4)? as i32;
                    if matches!(column.value_type, 0x0003 | 0x0004 | 0x000A)
                        || item.reference & 0x1F != 0
                    {
                        item.external = true;
                    } else if item.reference != 0 {
                        let allocation = self
                            .heap
                            .allocation(item.reference)?
                            .ok_or_else(|| Thrown::null("a cell's allocation"))?;
                        item.data = allocation.read_all(&self.heap)?;
                    }
                }
            }
            row.insert(column.id, item);
        }
        Ok(row)
    }
}
