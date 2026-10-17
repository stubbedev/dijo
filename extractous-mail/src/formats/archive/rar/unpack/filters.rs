//! `Unpack`'s filters: `addVMCode`, which puts a filter on the stack, and `UnpWriteBuf`,
//! which writes the window out, running each filter whose block it reaches over its block.

use super::bits::BitInput;
use super::vm::{self, Filter, Program, Vm};
use super::{
    Thrown, Unpack, MAX3_UNPACK_FILTERS, MAX_WIN_MASK, MAX_WIN_SIZE, NULL, OUT_OF_BOUNDS,
    VM_FIXEDGLOBALSIZE, VM_GLOBALMEMADDR, VM_GLOBALMEMSIZE,
};

const FIXED: usize = VM_FIXEDGLOBALSIZE as usize;

impl Unpack<'_> {
    /// `addVMCode`: a filter of the program list, new or used before, put on the filter stack
    /// for the block it names; `false` where junrar gives up on the data, which may leave the
    /// filter on the stack half made.
    pub(super) fn add_vm_code(&mut self, first_byte: i32, code: &[u8]) -> Result<bool, Thrown> {
        let mut input = Self::vm_input(code);
        let filt_pos = if first_byte & 0x80 != 0 {
            let filt_pos = vm::read_data(&mut input)?;
            if filt_pos == 0 {
                self.init_filters();
                filt_pos
            } else {
                filt_pos.wrapping_sub(1)
            }
        } else {
            self.last_filter
        };
        if filt_pos as u32 > self.filters.len() as u32
            || filt_pos as u32 > self.old_filter_lengths.len() as u32
        {
            return Ok(false);
        }
        self.last_filter = filt_pos;
        let new_filter = filt_pos as usize == self.filters.len();
        let mut stack_filter = Filter::default();
        let parent = if new_filter {
            if filt_pos as usize > MAX3_UNPACK_FILTERS {
                return Ok(false);
            }
            self.filters.push(Filter::default());
            self.old_filter_lengths.push(0);
            self.filters.len() - 1
        } else {
            let parent = filt_pos as usize;
            self.filters[parent].exec_count += 1;
            parent
        };
        stack_filter.parent_filter = parent as i32;
        let mut empty_count = 0;
        for i in 0..self.prg_stack.len() {
            let current = self.prg_stack[i].take();
            let empty = current.is_none();
            self.prg_stack[i - empty_count] = current;
            if empty {
                empty_count += 1;
            }
            if empty_count > 0 {
                self.prg_stack[i] = None;
            }
        }
        if empty_count == 0 {
            if self.prg_stack.len() > MAX3_UNPACK_FILTERS {
                return Ok(false);
            }
            self.prg_stack.push(None);
            empty_count = 1;
        }
        let slot = self.prg_stack.len() - empty_count;
        let kept = self.fill_stack_filter(
            &mut stack_filter,
            parent,
            new_filter,
            first_byte,
            &mut input,
        );
        self.prg_stack[slot] = Some(stack_filter);
        kept
    }

    /// The rest of `addVMCode`, for the filter it has put on the stack: its block, registers,
    /// global data and data block, and the code of a new filter.
    fn fill_stack_filter(
        &mut self,
        stack_filter: &mut Filter,
        parent: usize,
        new_filter: bool,
        first_byte: i32,
        input: &mut BitInput,
    ) -> Result<bool, Thrown> {
        stack_filter.exec_count = self.filters[parent].exec_count;
        let mut block_start = vm::read_data(input)?;
        if first_byte & 0x40 != 0 {
            block_start = block_start.wrapping_add(258);
        }
        stack_filter.block_start = block_start.wrapping_add(self.unp_ptr) & MAX_WIN_MASK;
        stack_filter.block_length = if first_byte & 0x20 != 0 {
            vm::read_data(input)?
        } else {
            self.old_filter_lengths[parent]
        };
        stack_filter.next_window = self.wr_ptr != self.unp_ptr
            && (self.wr_ptr.wrapping_sub(self.unp_ptr) & MAX_WIN_MASK) as u32 <= block_start as u32;
        self.old_filter_lengths[parent] = stack_filter.block_length;
        let prg = &mut stack_filter.prg;
        prg.init_r = [0; 7];
        prg.init_r[3] = VM_GLOBALMEMADDR;
        prg.init_r[4] = stack_filter.block_length;
        prg.init_r[5] = stack_filter.exec_count;
        if first_byte & 0x10 != 0 {
            let init_mask = input.getbits()? >> 9;
            input.addbits(7);
            for i in 0..7 {
                if init_mask & (1 << i) != 0 {
                    prg.init_r[i] = vm::read_data(input)?;
                }
            }
        }
        if new_filter {
            let code_size = vm::read_data(input)?;
            if code_size as u32 >= 0x10000 || code_size == 0 {
                return Ok(false);
            }
            let mut code = Vec::with_capacity(code_size as usize);
            for _ in 0..code_size {
                if input.overflows(3) {
                    return Ok(false);
                }
                code.push((input.getbits()? >> 8) as u8);
                input.addbits(8);
            }
            Vm::prepare(&code, &mut self.filters[parent].prg);
        }
        prg.kind = self.filters[parent].prg.kind;
        if prg.global.len() < FIXED {
            prg.global = vec![0; FIXED];
        }
        for i in 0..7 {
            prg.set_global(i * 4, prg.init_r[i as usize])?;
        }
        prg.set_global(0x1c, stack_filter.block_length)?;
        prg.set_global(0x20, 0)?;
        prg.set_global(0x24, 0)?;
        prg.set_global(0x28, 0)?;
        prg.set_global(0x2c, stack_filter.exec_count)?;
        prg.global[0x30..FIXED].fill(0);
        if first_byte & 8 != 0 {
            if input.overflows(3) {
                return Ok(false);
            }
            let data_size = vm::read_data(input)?;
            if data_size as u32 > (VM_GLOBALMEMSIZE - VM_FIXEDGLOBALSIZE) as u32 {
                return Ok(false);
            }
            let size = data_size + VM_FIXEDGLOBALSIZE;
            if (prg.global.len() as i32) < size {
                prg.resize_global(size)?;
            }
            for i in 0..data_size as usize {
                if input.overflows(3) {
                    return Ok(false);
                }
                let byte = (input.getbits()? >> 8) as u8;
                *prg.global.get_mut(FIXED + i).ok_or(OUT_OF_BOUNDS)? = byte;
                input.addbits(8);
            }
        }
        Ok(true)
    }

    /// `UnpWriteBuf`: the window written out up to the unpacking position, each filter on the
    /// stack whose block is all there run over it and its output written in its place.
    pub(super) fn unp_write_buf(&mut self) -> Result<(), Thrown> {
        let mut written_border = self.wr_ptr;
        let mut write_size = self.unp_ptr.wrapping_sub(written_border) & MAX_WIN_MASK;
        let mut i = 0;
        while i < self.prg_stack.len() {
            let Some(flt) = self.prg_stack[i].as_mut() else {
                i += 1;
                continue;
            };
            if flt.next_window {
                flt.next_window = false;
                i += 1;
                continue;
            }
            let block_start = flt.block_start;
            let block_length = flt.block_length;
            if (block_start.wrapping_sub(written_border) & MAX_WIN_MASK) >= write_size {
                i += 1;
                continue;
            }
            if written_border != block_start {
                self.unp_write_area(written_border, block_start)?;
                written_border = block_start;
                write_size = self.unp_ptr.wrapping_sub(written_border) & MAX_WIN_MASK;
            }
            if block_length as u32 > write_size as u32 {
                for filter in self.prg_stack[i..].iter_mut().flatten() {
                    filter.next_window = false;
                }
                self.wr_ptr = written_border;
                return Ok(());
            }
            let block_end = block_start.wrapping_add(block_length) & MAX_WIN_MASK;
            let window = self.window.as_deref().ok_or(NULL)?;
            if block_start < block_end || block_end == 0 {
                self.vm.set_memory(0, window, block_start, block_length)?;
            } else {
                let first_part_length = MAX_WIN_SIZE - block_start;
                self.vm
                    .set_memory(0, window, block_start, first_part_length)?;
                self.vm
                    .set_memory(first_part_length, window, 0, block_end)?;
            }
            let Some(mut flt) = self.prg_stack[i].take() else {
                return Err(NULL);
            };
            self.run_filter(&mut flt)?;
            let mut filtered = self.filtered_from_memory(&flt.prg)?;
            while let Some(Some(next)) = self.prg_stack.get(i + 1) {
                if next.block_start != block_start
                    || next.block_length != filtered.len() as i32
                    || next.next_window
                {
                    break;
                }
                self.vm.set_memory(0, &filtered, 0, filtered.len() as i32)?;
                i += 1;
                let Some(mut next) = self.prg_stack[i].take() else {
                    return Err(NULL);
                };
                self.run_filter(&mut next)?;
                filtered = self.filtered_from_memory(&next.prg)?;
            }
            self.input.io.write(&filtered, 0, filtered.len() as i32)?;
            self.written_file_size += filtered.len() as i64;
            written_border = block_end;
            write_size = self.unp_ptr.wrapping_sub(written_border) & MAX_WIN_MASK;
            i += 1;
        }
        self.unp_write_area(written_border, self.unp_ptr)?;
        self.wr_ptr = self.unp_ptr;
        Ok(())
    }

    /// A filter of the stack run, the global data past the fixed part passed from its parent
    /// in the program list and back.
    fn run_filter(&mut self, filter: &mut Filter) -> Result<(), Thrown> {
        let parent = usize::try_from(filter.parent_filter)
            .ok()
            .filter(|&parent| parent < self.filters.len())
            .ok_or(OUT_OF_BOUNDS)?;
        let parent_global = &self.filters[parent].prg.global;
        if parent_global.len() > FIXED {
            filter.prg.global.resize(parent_global.len(), 0);
            filter.prg.global[FIXED..].copy_from_slice(&parent_global[FIXED..]);
        }
        self.execute_code(&mut filter.prg)?;
        let parent_global = &mut self.filters[parent].prg.global;
        if filter.prg.global.len() > FIXED {
            if parent_global.len() < filter.prg.global.len() {
                parent_global.resize(filter.prg.global.len(), 0);
            }
            let length = filter.prg.global.len();
            parent_global[FIXED..length].copy_from_slice(&filter.prg.global[FIXED..]);
        } else {
            parent_global.clear();
        }
        Ok(())
    }

    /// `ExecuteCode`: the program run, told how much of the entry is written.
    fn execute_code(&mut self, prg: &mut Program) -> Result<(), Thrown> {
        if prg.global.is_empty() {
            return Ok(());
        }
        prg.init_r[6] = self.written_file_size as i32;
        prg.set_global(0x24, self.written_file_size as i32)?;
        prg.set_global(0x28, (self.written_file_size as u64 >> 32) as i32)?;
        self.vm.execute(prg)
    }

    /// The output of a filter run over a block, from the memory, where unrar takes it from
    /// for each filter of a chain; junrar takes a chained filter's from its global data (#122).
    fn filtered_from_memory(&self, prg: &Program) -> Result<Vec<u8>, Thrown> {
        let size = usize::try_from(prg.filtered_data_size).map_err(|_| NEGATIVE_SIZE)?;
        let offset = usize::try_from(prg.filtered_data_offset).map_err(|_| OUT_OF_BOUNDS)?;
        self.vm
            .mem
            .get(offset..offset + size)
            .map(<[u8]>::to_vec)
            .ok_or(OUT_OF_BOUNDS)
    }

    /// `UnpWriteArea`: the window from `start` to `end` written out, around its end.
    fn unp_write_area(&mut self, start: i32, end: i32) -> Result<(), Thrown> {
        if end < start {
            self.unp_write_data(start, start.wrapping_neg() & MAX_WIN_MASK)?;
            self.unp_write_data(0, end)
        } else {
            self.unp_write_data(start, end - start)
        }
    }

    /// `UnpWriteData`: `size` bytes of the window from `offset` on, as many of them as the
    /// entry has left.
    fn unp_write_data(&mut self, offset: i32, size: i32) -> Result<(), Thrown> {
        if self.written_file_size >= self.dest_unp_size {
            return Ok(());
        }
        let left = self.dest_unp_size - self.written_file_size;
        let write_size = if i64::from(size) > left {
            left as i32
        } else {
            size
        };
        let window = self.window.as_deref().ok_or(NULL)?;
        self.input.io.write(window, offset, write_size)?;
        self.written_file_size += i64::from(size);
        Ok(())
    }
}

const NEGATIVE_SIZE: Thrown = Thrown("a NegativeArraySizeException");
