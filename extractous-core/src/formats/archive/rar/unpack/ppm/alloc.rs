//! junrar's `SubAllocator`: the PPMd model's memory as one byte array of 12-byte units its
//! `int` addresses point into, with free lists of blocks of so many units, and junrar's `Raw`
//! accessors of the numbers in it.

use super::super::{Thrown, OUT_OF_BOUNDS};

const N1: usize = 4;
const N2: usize = 4;
const N3: usize = 4;
const N4: usize = (128 + 3 - N1 - 2 * N2 - 3 * N3) / 4;
pub(super) const N_INDEXES: usize = N1 + N2 + N3 + N4;
/// `UNIT_SIZE`: the larger of a context's and a memory block's 12 bytes.
pub(super) const UNIT_SIZE: i32 = 12;
const FIXED_UNIT_SIZE: i32 = 12;
/// `RarNode.size`: a free list's head, the address of its first block.
const NODE_SIZE: i32 = 4;
/// `RarMemBlock.size`.
const MEM_BLOCK_SIZE: i32 = 12;

pub(super) struct SubAllocator {
    sub_allocator_size: i32,
    indx2_units: [i32; N_INDEXES],
    units2_indx: [i32; 128],
    glue_count: i32,
    heap_start: i32,
    lo_unit: i32,
    hi_unit: i32,
    free_list_pos: i32,
    temp_mem_block_pos: i32,
    pub(super) p_text: i32,
    units_start: i32,
    pub(super) heap_end: i32,
    pub(super) fake_units_start: i32,
    /// The memory, empty while none is allocated, where any access fails as Java's on the
    /// `null` array does.
    heap: Vec<u8>,
}

impl SubAllocator {
    pub(super) fn new() -> Self {
        SubAllocator {
            sub_allocator_size: 0,
            indx2_units: [0; N_INDEXES],
            units2_indx: [0; 128],
            glue_count: 0,
            heap_start: 0,
            lo_unit: 0,
            hi_unit: 0,
            free_list_pos: 0,
            temp_mem_block_pos: 0,
            p_text: 0,
            units_start: 0,
            heap_end: 0,
            fake_units_start: 0,
            heap: Vec::new(),
        }
    }

    /// `GetAllocatedMemory`.
    pub(super) fn allocated(&self) -> i32 {
        self.sub_allocator_size
    }

    /// `stopSubAllocator`.
    pub(super) fn stop(&mut self) {
        if self.sub_allocator_size != 0 {
            self.sub_allocator_size = 0;
            self.heap = Vec::new();
            self.heap_start = 1;
        }
    }

    /// `startSubAllocator`: `megabytes` MiB of memory, kept as it is when that much is
    /// allocated already.
    pub(super) fn start(&mut self, megabytes: i32) {
        let size = megabytes << 20;
        if self.sub_allocator_size == size {
            return;
        }
        self.stop();
        let alloc_size = size / FIXED_UNIT_SIZE * UNIT_SIZE + UNIT_SIZE;
        let real_alloc_size = 1 + alloc_size + NODE_SIZE * N_INDEXES as i32;
        self.temp_mem_block_pos = real_alloc_size;
        self.heap = vec![0; (real_alloc_size + MEM_BLOCK_SIZE) as usize];
        self.heap_start = 1;
        self.heap_end = self.heap_start + alloc_size - UNIT_SIZE;
        self.sub_allocator_size = size;
        self.free_list_pos = self.heap_start + alloc_size;
    }

    /// `initSubAllocator`: every unit free again, the text area empty.
    pub(super) fn init(&mut self) -> Result<(), Thrown> {
        self.fill(self.free_list_pos, NODE_SIZE * N_INDEXES as i32, 0)?;
        self.p_text = self.heap_start;
        let size2 = FIXED_UNIT_SIZE * (self.sub_allocator_size / 8 / FIXED_UNIT_SIZE * 7);
        let real_size2 = size2 / FIXED_UNIT_SIZE * UNIT_SIZE;
        let size1 = self.sub_allocator_size - size2;
        let real_size1 = size1 / FIXED_UNIT_SIZE * UNIT_SIZE + size1 % FIXED_UNIT_SIZE;
        self.lo_unit = self.heap_start + real_size1;
        self.units_start = self.lo_unit;
        self.fake_units_start = self.heap_start + size1;
        self.hi_unit = self.lo_unit + real_size2;
        let mut i = 0;
        let mut k = 0;
        for (count, step) in [(N1, 1), (N2, 2), (N3, 3), (N4, 4)] {
            k += 1;
            for _ in 0..count {
                self.indx2_units[i] = k & 0xff;
                i += 1;
                k += step;
            }
        }
        self.glue_count = 0;
        let mut i = 0;
        for k in 0..128 {
            if self.indx2_units[i] < k as i32 + 1 {
                i += 1;
            }
            self.units2_indx[k] = i as i32 & 0xff;
        }
        Ok(())
    }

    fn u2b(units: i32) -> i32 {
        UNIT_SIZE.wrapping_mul(units)
    }

    fn free_list(&self, index: usize) -> Result<i32, Thrown> {
        if index >= N_INDEXES {
            return Err(OUT_OF_BOUNDS);
        }
        Ok(self.free_list_pos + NODE_SIZE * index as i32)
    }

    fn units_index(&self, units: i32) -> Result<usize, Thrown> {
        usize::try_from(units - 1)
            .ok()
            .and_then(|at| self.units2_indx.get(at))
            .map(|&index| index as usize)
            .ok_or(OUT_OF_BOUNDS)
    }

    fn index_units(&self, index: usize) -> Result<i32, Thrown> {
        self.indx2_units.get(index).copied().ok_or(OUT_OF_BOUNDS)
    }

    fn has_free(&self, index: usize) -> Result<bool, Thrown> {
        Ok(self.i32_at(self.free_list(index)?)? != 0)
    }

    fn insert_node(&mut self, block: i32, index: usize) -> Result<(), Thrown> {
        let head = self.free_list(index)?;
        let first = self.i32_at(head)?;
        self.set_i32(block, first)?;
        self.set_i32(head, block)
    }

    fn remove_node(&mut self, index: usize) -> Result<i32, Thrown> {
        let head = self.free_list(index)?;
        let block = self.i32_at(head)?;
        let next = self.i32_at(block)?;
        self.set_i32(head, next)?;
        Ok(block)
    }

    fn split_block(
        &mut self,
        block: i32,
        old_index: usize,
        new_index: usize,
    ) -> Result<(), Thrown> {
        let mut u_diff = self.index_units(old_index)? - self.index_units(new_index)?;
        let mut p = block + Self::u2b(self.index_units(new_index)?);
        let mut i = self.units_index(u_diff)?;
        if self.index_units(i)? != u_diff {
            i = i.checked_sub(1).ok_or(OUT_OF_BOUNDS)?;
            self.insert_node(p, i)?;
            let units = self.index_units(i)?;
            p += Self::u2b(units);
            u_diff -= units;
        }
        let index = self.units_index(u_diff)?;
        self.insert_node(p, index)
    }

    fn glue_free_blocks(&mut self) -> Result<(), Thrown> {
        let s0 = self.temp_mem_block_pos;
        if self.lo_unit != self.hi_unit {
            self.set_u8(self.lo_unit, 0)?;
        }
        self.set_block_prev(s0, s0)?;
        self.set_block_next(s0, s0)?;
        for i in 0..N_INDEXES {
            while self.has_free(i)? {
                let p = self.remove_node(i)?;
                self.insert_block_after(p, s0)?;
                self.set_u16(p, 0xFFFF)?;
                self.set_u16(p + 2, self.indx2_units[i])?;
            }
        }
        let mut p = self.block_next(s0)?;
        while p != s0 {
            let mut p1 = p + Self::u2b(self.u16_at(p + 2)?);
            while self.u16_at(p1)? == 0xFFFF && self.u16_at(p + 2)? + self.u16_at(p1 + 2)? < 0x10000
            {
                self.remove_block(p1)?;
                let units = self.u16_at(p + 2)? + self.u16_at(p1 + 2)?;
                self.set_u16(p + 2, units)?;
                p1 = p + Self::u2b(self.u16_at(p + 2)?);
            }
            p = self.block_next(p)?;
        }
        p = self.block_next(s0)?;
        while p != s0 {
            self.remove_block(p)?;
            let mut size = self.u16_at(p + 2)?;
            while size > 128 {
                self.insert_node(p, N_INDEXES - 1)?;
                size -= 128;
                p += Self::u2b(128);
            }
            let mut i = self.units_index(size)?;
            if self.index_units(i)? != size {
                i = i.checked_sub(1).ok_or(OUT_OF_BOUNDS)?;
                let k = size - self.index_units(i)?;
                let index = usize::try_from(k - 1).map_err(|_| OUT_OF_BOUNDS)?;
                self.insert_node(p + Self::u2b(size - k), index)?;
            }
            self.insert_node(p, i)?;
            p = self.block_next(s0)?;
        }
        Ok(())
    }

    fn alloc_units_rare(&mut self, index: usize) -> Result<i32, Thrown> {
        if self.glue_count == 0 {
            self.glue_count = 255;
            self.glue_free_blocks()?;
            if self.has_free(index)? {
                return self.remove_node(index);
            }
        }
        let mut i = index;
        loop {
            i += 1;
            if i == N_INDEXES {
                self.glue_count -= 1;
                let bytes = Self::u2b(self.index_units(index)?);
                let fixed = FIXED_UNIT_SIZE * self.index_units(index)?;
                if self.fake_units_start - self.p_text > fixed {
                    self.fake_units_start -= fixed;
                    self.units_start -= bytes;
                    return Ok(self.units_start);
                }
                return Ok(0);
            }
            if self.has_free(i)? {
                break;
            }
        }
        let block = self.remove_node(i)?;
        self.split_block(block, i, index)?;
        Ok(block)
    }

    /// `allocUnits`: a block of `units` units, 0 when none is left.
    pub(super) fn alloc_units(&mut self, units: i32) -> Result<i32, Thrown> {
        let index = self.units_index(units)?;
        if self.has_free(index)? {
            return self.remove_node(index);
        }
        let block = self.lo_unit;
        let bytes = Self::u2b(self.index_units(index)?);
        self.lo_unit += bytes;
        if self.lo_unit <= self.hi_unit {
            return Ok(block);
        }
        self.lo_unit -= bytes;
        self.alloc_units_rare(index)
    }

    /// `allocContext`: a unit for a context, 0 when none is left.
    pub(super) fn alloc_context(&mut self) -> Result<i32, Thrown> {
        if self.hi_unit != self.lo_unit {
            self.hi_unit -= UNIT_SIZE;
            return Ok(self.hi_unit);
        }
        if self.has_free(0)? {
            return self.remove_node(0);
        }
        self.alloc_units_rare(0)
    }

    /// `expandUnits`: the block at `old` of `old_units` units grown by one unit, moved if it
    /// must be; 0 when no memory is left.
    pub(super) fn expand_units(&mut self, old: i32, old_units: i32) -> Result<i32, Thrown> {
        let i0 = self.units_index(old_units)?;
        let i1 = self.units_index(old_units + 1)?;
        if i0 == i1 {
            return Ok(old);
        }
        let block = self.alloc_units(old_units + 1)?;
        if block != 0 {
            self.copy(old, block, Self::u2b(old_units))?;
            self.insert_node(old, i0)?;
        }
        Ok(block)
    }

    /// `shrinkUnits`: the block at `old` of `old_units` units cut to `new_units`.
    pub(super) fn shrink_units(
        &mut self,
        old: i32,
        old_units: i32,
        new_units: i32,
    ) -> Result<i32, Thrown> {
        let i0 = self.units_index(old_units)?;
        let i1 = self.units_index(new_units)?;
        if i0 == i1 {
            return Ok(old);
        }
        if self.has_free(i1)? {
            let block = self.remove_node(i1)?;
            self.copy(old, block, Self::u2b(new_units))?;
            self.insert_node(old, i0)?;
            Ok(block)
        } else {
            self.split_block(old, i0, i1)?;
            Ok(old)
        }
    }

    /// `freeUnits`.
    pub(super) fn free_units(&mut self, block: i32, units: i32) -> Result<(), Thrown> {
        let index = self.units_index(units)?;
        self.insert_node(block, index)
    }

    fn block_next(&self, block: i32) -> Result<i32, Thrown> {
        self.i32_at(block + 4)
    }

    fn set_block_next(&mut self, block: i32, next: i32) -> Result<(), Thrown> {
        self.set_i32(block + 4, next)
    }

    fn block_prev(&self, block: i32) -> Result<i32, Thrown> {
        self.i32_at(block + 8)
    }

    fn set_block_prev(&mut self, block: i32, prev: i32) -> Result<(), Thrown> {
        self.set_i32(block + 8, prev)
    }

    /// `RarMemBlock.insertAt`: `block` linked in after `after`.
    fn insert_block_after(&mut self, block: i32, after: i32) -> Result<(), Thrown> {
        self.set_block_prev(block, after)?;
        let next = self.block_next(after)?;
        self.set_block_next(block, next)?;
        self.set_block_next(after, block)?;
        self.set_block_prev(next, block)
    }

    /// `RarMemBlock.remove`.
    fn remove_block(&mut self, block: i32) -> Result<(), Thrown> {
        let prev = self.block_prev(block)?;
        let next = self.block_next(block)?;
        self.set_block_next(prev, next)?;
        self.set_block_prev(next, prev)
    }

    fn range(&self, at: i32, length: usize) -> Result<std::ops::Range<usize>, Thrown> {
        let at = usize::try_from(at).map_err(|_| OUT_OF_BOUNDS)?;
        let end = at + length;
        if end > self.heap.len() {
            return Err(OUT_OF_BOUNDS);
        }
        Ok(at..end)
    }

    pub(super) fn u8_at(&self, at: i32) -> Result<i32, Thrown> {
        let range = self.range(at, 1)?;
        Ok(i32::from(self.heap[range.start]))
    }

    pub(super) fn set_u8(&mut self, at: i32, value: i32) -> Result<(), Thrown> {
        let range = self.range(at, 1)?;
        self.heap[range.start] = value as u8;
        Ok(())
    }

    /// `Raw.readShortLittleEndian(..) & 0xffff`.
    pub(super) fn u16_at(&self, at: i32) -> Result<i32, Thrown> {
        let range = self.range(at, 2)?;
        Ok(i32::from(u16::from_le_bytes([
            self.heap[range.start],
            self.heap[range.start + 1],
        ])))
    }

    pub(super) fn set_u16(&mut self, at: i32, value: i32) -> Result<(), Thrown> {
        let range = self.range(at, 2)?;
        self.heap[range].copy_from_slice(&(value as u16).to_le_bytes());
        Ok(())
    }

    /// `Raw.incShortLittleEndian`: `delta` added byte by byte, the carry and the high byte of
    /// `delta` only when there are any.
    pub(super) fn inc_u16(&mut self, at: i32, delta: i32) -> Result<(), Thrown> {
        let range = self.range(at, 2)?;
        let low = i32::from(self.heap[range.start]);
        let carry = ((low + (delta & 0xff)) as u32 >> 8) as i32;
        self.heap[range.start] = (low + (delta & 0xff)) as u8;
        if carry > 0 || delta & 0xff00 != 0 {
            let high = i32::from(self.heap[range.start + 1]);
            self.heap[range.start + 1] = (high + ((delta as u32 >> 8) as i32 & 0xff) + carry) as u8;
        }
        Ok(())
    }

    pub(super) fn i32_at(&self, at: i32) -> Result<i32, Thrown> {
        let range = self.range(at, 4)?;
        let bytes = &self.heap[range];
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn set_i32(&mut self, at: i32, value: i32) -> Result<(), Thrown> {
        let range = self.range(at, 4)?;
        self.heap[range].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    /// `System.arraycopy` within the memory.
    pub(super) fn copy(&mut self, from: i32, to: i32, length: i32) -> Result<(), Thrown> {
        let length = usize::try_from(length).map_err(|_| OUT_OF_BOUNDS)?;
        let source = self.range(from, length)?;
        let target = self.range(to, length)?;
        self.heap.copy_within(source, target.start);
        Ok(())
    }

    fn fill(&mut self, at: i32, length: i32, value: u8) -> Result<(), Thrown> {
        let length = usize::try_from(length).map_err(|_| OUT_OF_BOUNDS)?;
        let range = self.range(at, length)?;
        self.heap[range].fill(value);
        Ok(())
    }
}
