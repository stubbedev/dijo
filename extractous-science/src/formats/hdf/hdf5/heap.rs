//! HDF5's heaps: the local heap of a group's names, the global heap of values of variable
//! length, and the fractal heap of a group's links and an object's attributes in dense storage,
//! read in place.

use super::File;
use crate::formats::bytes::{malformed, text_to_nul, uint, unported, Order, Read, Unread};
use crate::formats::hdf::hdf5::datatype::HeapReference;

/// The data segment of a local heap.
pub(super) fn local_heap<'a>(file: &File<'a>, address: u64) -> Read<&'a [u8]> {
    let mut header = file.cursor_from(address, "a local heap")?;
    if header.take(4)? != b"HEAP" {
        return Err(header.malformed("no local heap signature"));
    }
    header.skip(4)?;
    let size = header.uint(file.lengths)?;
    header.uint(file.lengths)?;
    let data = header.uint(file.offsets)?;
    file.slice(data, size, "a local heap's data")
}

/// The NUL-terminated name at `offset` of a local heap's data `segment`.
pub(super) fn heap_name(segment: &[u8], offset: u64) -> Read<String> {
    let start = usize::try_from(offset)
        .ok()
        .filter(|&start| start < segment.len())
        .ok_or_else(|| {
            malformed(format!(
                "a name at offset {offset} of a local heap of {} bytes",
                segment.len()
            ))
        })?;
    Ok(text_to_nul(&segment[start..]))
}

/// The object a reference of variable length names in a global heap collection.
pub(super) fn global_object(file: &File, reference: HeapReference) -> Read<Vec<u8>> {
    if reference.collection == 0 || file.undefined(reference.collection) {
        return Ok(Vec::new());
    }
    let mut collection = file.cursor_from(reference.collection, "a global heap collection")?;
    if collection.take(4)? != b"GCOL" {
        return Err(collection.malformed("no global heap signature"));
    }
    collection.skip(4)?;
    let size = collection.size(file.lengths, "a global heap collection's size")?;
    let mut objects = file.cursor(
        reference.collection,
        size as u64,
        "a global heap collection",
    )?;
    objects.seek(8 + file.lengths)?;
    while objects.remaining() >= 8 + file.lengths {
        let index = objects.u16()?;
        if index == 0 {
            break;
        }
        objects.skip(6)?;
        let length = objects.size(file.lengths, "a global heap object's size")?;
        let data = objects.take(length)?;
        if u32::from(index) == reference.index {
            let wanted = usize::try_from(reference.length)
                .ok()
                .filter(|&wanted| wanted <= data.len())
                .unwrap_or(data.len());
            return Ok(data[..wanted].to_vec());
        }
        objects.align(8)?;
    }
    Err(objects.malformed(format!("no object {} in the collection", reference.index)))
}

/// A fractal heap, as its header describes it.
pub(super) struct FractalHeap {
    address: u64,
    heap_id_length: usize,
    table_width: u64,
    start_block_size: u64,
    max_direct_rows: u32,
    offset_size: usize,
    length_size: usize,
    root: u64,
    root_rows: u32,
}

impl FractalHeap {
    pub(super) fn read(file: &File, address: u64) -> Read<Self> {
        let mut header = file.cursor_from(address, "a fractal heap header")?;
        if header.take(4)? != b"FRHP" {
            return Err(header.malformed("no fractal heap signature"));
        }
        header.u8()?;
        let heap_id_length = usize::from(header.u16()?);
        let filters = header.u16()?;
        if filters != 0 {
            return Err(unported("a fractal heap whose blocks are filtered"));
        }
        header.u8()?;
        let max_managed = header.u32()?;
        header.skip(file.lengths)?;
        header.skip(file.offsets)?;
        header.skip(file.lengths)?;
        header.skip(file.offsets)?;
        for _ in 0..8 {
            header.skip(file.lengths)?;
        }
        let table_width = u64::from(header.u16()?);
        let start_block_size = header.uint(file.lengths)?;
        let max_direct_size = header.uint(file.lengths)?;
        let max_heap_bits = header.u16()?;
        header.u16()?;
        let root = header.uint(file.offsets)?;
        let root_rows = u32::from(header.u16()?);
        if table_width == 0
            || !start_block_size.is_power_of_two()
            || !max_direct_size.is_power_of_two()
            || max_direct_size < start_block_size
        {
            return Err(header.malformed(format!(
                "a doubling table {table_width} wide of blocks of {start_block_size} to \
                 {max_direct_size} bytes"
            )));
        }
        let max_direct_rows =
            max_direct_size.trailing_zeros() - start_block_size.trailing_zeros() + 2;
        let direct_offset_size = max_direct_size.trailing_zeros().div_ceil(8) as usize;
        Ok(Self {
            address,
            heap_id_length,
            table_width,
            start_block_size,
            max_direct_rows,
            offset_size: usize::from(max_heap_bits).div_ceil(8),
            length_size: direct_offset_size.min(encoded_size(u64::from(max_managed))),
            root,
            root_rows,
        })
    }

    pub(super) fn heap_id_length(&self) -> usize {
        self.heap_id_length
    }

    /// The object a heap ID names.
    pub(super) fn object<'a>(&self, file: &File<'a>, id: &'a [u8]) -> Read<&'a [u8]> {
        let Some((&flags, rest)) = id.split_first() else {
            return Err(malformed("an empty fractal heap ID"));
        };
        match (flags >> 4) & 0x03 {
            0 => {
                let offset = uint(
                    rest.get(..self.offset_size).ok_or_else(|| short_id(id))?,
                    Order::Little,
                );
                let length = uint(
                    rest.get(self.offset_size..self.offset_size + self.length_size)
                        .ok_or_else(|| short_id(id))?,
                    Order::Little,
                );
                self.managed(file, offset, length)
            }
            1 => Err(unported("a huge object of a fractal heap")),
            2 => {
                let length = usize::from(flags & 0x0F) + 1;
                rest.get(..length).ok_or_else(|| short_id(id))
            }
            other => Err(malformed(format!(
                "a fractal heap ID of unknown type {other}"
            ))),
        }
    }

    /// The managed object of `length` bytes at `offset` of the heap's address space.
    fn managed<'a>(&self, file: &File<'a>, offset: u64, length: u64) -> Read<&'a [u8]> {
        if self.root_rows == 0 {
            return self.in_direct(file, self.root, 0, self.start_block_size, offset, length);
        }
        let mut block = self.root;
        let mut rows = self.root_rows;
        let mut block_offset = 0u64;
        loop {
            let mut entries = file.cursor_from(block, "a fractal heap indirect block")?;
            if entries.take(4)? != b"FHIB" {
                return Err(entries.malformed("no indirect block signature"));
            }
            entries.u8()?;
            entries.skip(file.offsets)?;
            entries.skip(self.offset_size)?;
            let mut start = block_offset;
            let mut found = None;
            'rows: for row in 0..rows {
                let size = self
                    .row_block_size(row)
                    .ok_or_else(|| entries.malformed(format!("a doubling table of {rows} rows")))?;
                for _ in 0..self.table_width {
                    let child = entries.uint(file.offsets)?;
                    let end = start
                        .checked_add(size)
                        .ok_or_else(|| entries.malformed("blocks past any heap's size"))?;
                    if offset < end {
                        found = Some((row, child, start, size));
                        break 'rows;
                    }
                    start = end;
                }
            }
            let Some((row, child, start, size)) = found else {
                return Err(entries.malformed(format!(
                    "no block of the fractal heap at {} holds offset {offset}",
                    self.address
                )));
            };
            if file.undefined(child) {
                return Err(entries.malformed(format!(
                    "offset {offset} of the fractal heap at {} lies in no allocated block",
                    self.address
                )));
            }
            if row < self.max_direct_rows {
                return self.in_direct(file, child, start, size, offset, length);
            }
            block = child;
            block_offset = start;
            rows = self
                .start_block_size
                .checked_mul(self.table_width)
                .and_then(|first| size.trailing_zeros().checked_sub(first.trailing_zeros()))
                .map(|rows| rows + 1)
                .ok_or_else(|| entries.malformed(format!("an indirect block of {size} bytes")))?;
        }
    }

    fn in_direct<'a>(
        &self,
        file: &File<'a>,
        block: u64,
        block_offset: u64,
        block_size: u64,
        offset: u64,
        length: u64,
    ) -> Read<&'a [u8]> {
        let within = offset - block_offset;
        if within
            .checked_add(length)
            .is_none_or(|end| end > block_size)
        {
            return Err(malformed(format!(
                "an object of {length} bytes at offset {offset} of the fractal heap at {} \
                 runs past its block",
                self.address
            )));
        }
        let signature = file.slice(block, 4, "a fractal heap direct block")?;
        if signature != b"FHDB" {
            return Err(malformed(format!(
                "no direct block signature at {block} of the fractal heap at {}",
                self.address
            )));
        }
        file.slice(block + within, length, "a fractal heap object")
    }

    /// The size of the blocks of a row of the doubling table, where it is one a file holds.
    fn row_block_size(&self, row: u32) -> Option<u64> {
        let doublings = row.saturating_sub(1);
        self.start_block_size.checked_shl(doublings).filter(|size| {
            size.trailing_zeros() == self.start_block_size.trailing_zeros() + doublings
        })
    }
}

fn short_id(id: &[u8]) -> Unread {
    malformed(format!("a fractal heap ID of {} bytes cut short", id.len()))
}

/// `H5VM_limit_enc_size`: the bytes that encode numbers up to `limit`.
pub(super) fn encoded_size(limit: u64) -> usize {
    (limit.checked_ilog2().unwrap_or(0) / 8 + 1) as usize
}
