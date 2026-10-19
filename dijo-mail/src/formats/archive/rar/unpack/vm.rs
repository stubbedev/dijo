//! junrar's `RarVM`, which runs none of a RAR 3 filter's code: it recognizes the standard
//! filters by the length and CRC-32 of their code and runs its own version of them, and
//! writes nothing for any other filter's block.

use super::bits::BitInput;
use super::{Thrown, OUT_OF_BOUNDS};

pub(super) const VM_MEMSIZE: i32 = 0x40000;
pub(super) const VM_GLOBALMEMADDR: i32 = 0x3C000;
pub(super) const VM_GLOBALMEMSIZE: i32 = 0x2000;
pub(super) const VM_FIXEDGLOBALSIZE: i32 = 64;
const MAX3_UNPACK_CHANNELS: u32 = 1024;
/// `0x80000000` as Java widens the `int` to a `long`.
const SIGN: i64 = -0x8000_0000;

/// `VMStandardFilters`: the filters junrar runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Standard {
    #[default]
    None,
    E8,
    E8E9,
    Itanium,
    Delta,
    Rgb,
    Audio,
}

/// `IsStandardFilter`'s list: each filter's code length and CRC-32.
const SIGNATURES: [(usize, u32, Standard); 6] = [
    (53, 0xad57_6887, Standard::E8),
    (57, 0x3cd7_e57e, Standard::E8E9),
    (120, 0x3769_893f, Standard::Itanium),
    (29, 0x0e06_077d, Standard::Delta),
    (149, 0x1c2c_5dc8, Standard::Rgb),
    (216, 0xbc85_e701, Standard::Audio),
];

/// `VMPreparedProgram`: a filter's kind, initial registers and global data, and where its output
/// is.
#[derive(Debug, Clone, Default)]
pub(super) struct Program {
    pub(super) kind: Standard,
    pub(super) init_r: [i32; 7],
    pub(super) global: Vec<u8>,
    pub(super) filtered_data_offset: i32,
    pub(super) filtered_data_size: i32,
}

impl Program {
    /// `setLowEndianValue` of the global data, failing as `Vector.set` does past its size.
    pub(super) fn set_global(&mut self, offset: i32, value: i32) -> Result<(), Thrown> {
        for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
            let slot = usize::try_from(offset)
                .ok()
                .and_then(|offset| self.global.get_mut(offset + i))
                .ok_or(OUT_OF_BOUNDS)?;
            *slot = byte;
        }
        Ok(())
    }

    /// `Vector.setSize`: grown with zeros, or cut.
    pub(super) fn resize_global(&mut self, size: i32) -> Result<(), Thrown> {
        let size = usize::try_from(size).map_err(|_| OUT_OF_BOUNDS)?;
        self.global.resize(size, 0);
        Ok(())
    }
}

/// `UnpackFilter`: a filter of the program list, or an instance of one on the filter stack.
#[derive(Debug, Clone, Default)]
pub(super) struct Filter {
    pub(super) block_start: i32,
    pub(super) block_length: i32,
    pub(super) exec_count: i32,
    pub(super) next_window: bool,
    pub(super) parent_filter: i32,
    pub(super) prg: Program,
}

/// `RarVM`'s memory, which persists from filter to filter and entry to entry.
pub(super) struct Vm {
    pub(super) mem: Box<[u8]>,
}

impl Vm {
    pub(super) fn new() -> Self {
        Vm {
            mem: vec![0; VM_MEMSIZE as usize + 4].into_boxed_slice(),
        }
    }

    /// `prepare`: the program's kind, if its code's checksum holds and it is a standard filter.
    pub(super) fn prepare(code: &[u8], prg: &mut Program) {
        let Some((&first, rest)) = code.split_first() else {
            return;
        };
        if rest.iter().fold(0u8, |sum, &byte| sum ^ byte) != first {
            return;
        }
        let crc = crate::compress::crc32(code);
        if let Some(&(_, _, kind)) = SIGNATURES
            .iter()
            .find(|&&(length, signature, _)| signature == crc && length == code.len())
        {
            prg.kind = kind;
        }
    }

    /// `setMemory`: `data_size` bytes of `data` from `offset` on at `pos`, as many as fit.
    pub(super) fn set_memory(
        &mut self,
        pos: i32,
        data: &[u8],
        offset: i32,
        data_size: i32,
    ) -> Result<(), Thrown> {
        if pos >= VM_MEMSIZE {
            return Ok(());
        }
        let available = data.len() as i64 - i64::from(offset);
        let count = available
            .min(i64::from(data_size))
            .min(i64::from(VM_MEMSIZE - pos) + 1);
        if count <= 0 {
            return Ok(());
        }
        let pos = usize::try_from(pos).map_err(|_| OUT_OF_BOUNDS)?;
        let offset = usize::try_from(offset).map_err(|_| OUT_OF_BOUNDS)?;
        let count = count as usize;
        self.mem[pos..pos + count].copy_from_slice(&data[offset..offset + count]);
        Ok(())
    }

    /// `execute`: the program's standard filter run over the memory, and where its output is.
    pub(super) fn execute(&mut self, prg: &mut Program) -> Result<(), Thrown> {
        if prg.kind == Standard::None {
            prg.filtered_data_size = 0;
            prg.filtered_data_offset = 0;
            return Ok(());
        }
        let r = prg.init_r;
        match prg.kind {
            Standard::None => {}
            Standard::E8 => self.e8(&r, 0xe8)?,
            Standard::E8E9 => self.e8(&r, 0xe9)?,
            Standard::Itanium => self.itanium(&r)?,
            Standard::Delta => self.delta(&r)?,
            Standard::Rgb => self.rgb(&r)?,
            Standard::Audio => self.audio(&r)?,
        }
        let block_size = r[4];
        prg.filtered_data_size = block_size;
        prg.filtered_data_offset = match prg.kind {
            Standard::Delta | Standard::Rgb | Standard::Audio
                if 2 * i64::from(block_size) < i64::from(VM_MEMSIZE) =>
            {
                block_size
            }
            _ => 0,
        };
        Ok(())
    }

    fn int_at(&self, pos: i32) -> Result<i32, Thrown> {
        let pos = usize::try_from(pos).map_err(|_| OUT_OF_BOUNDS)?;
        self.mem
            .get(pos..pos + 4)
            .map(|bytes| i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .ok_or(OUT_OF_BOUNDS)
    }

    fn set_int(&mut self, pos: i32, value: i32) -> Result<(), Thrown> {
        let pos = usize::try_from(pos).map_err(|_| OUT_OF_BOUNDS)?;
        self.mem
            .get_mut(pos..pos + 4)
            .ok_or(OUT_OF_BOUNDS)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn byte(&self, pos: i32) -> Result<u8, Thrown> {
        usize::try_from(pos)
            .ok()
            .and_then(|pos| self.mem.get(pos))
            .copied()
            .ok_or(OUT_OF_BOUNDS)
    }

    fn set_byte(&mut self, pos: i32, value: u8) -> Result<(), Thrown> {
        let slot = usize::try_from(pos)
            .ok()
            .and_then(|pos| self.mem.get_mut(pos))
            .ok_or(OUT_OF_BOUNDS)?;
        *slot = value;
        Ok(())
    }

    fn e8(&mut self, r: &[i32; 7], second: u8) -> Result<(), Thrown> {
        const FILE_SIZE: i32 = 0x100_0000;
        let data_size = r[4];
        let file_offset = i64::from(r[6]);
        if data_size >= VM_GLOBALMEMADDR {
            return Ok(());
        }
        let mut cur_pos = 0i32;
        while cur_pos < data_size.wrapping_sub(4) {
            let cur_byte = self.byte(cur_pos)?;
            cur_pos += 1;
            if cur_byte == 0xe8 || cur_byte == second {
                let offset = i64::from(cur_pos) + file_offset;
                let addr = i64::from(self.int_at(cur_pos)?);
                if addr & SIGN != 0 {
                    if (addr + offset) & SIGN == 0 {
                        self.set_int(cur_pos, (addr as i32).wrapping_add(FILE_SIZE))?;
                    }
                } else if (addr - i64::from(FILE_SIZE)) & SIGN != 0 {
                    self.set_int(cur_pos, (addr - offset) as i32)?;
                }
                cur_pos += 4;
            }
        }
        Ok(())
    }

    fn itanium(&mut self, r: &[i32; 7]) -> Result<(), Thrown> {
        const MASKS: [u8; 16] = [4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0];
        let data_size = r[4];
        if data_size >= VM_GLOBALMEMADDR {
            return Ok(());
        }
        let mut file_offset = (i64::from(r[6]) as u64 >> 4) as i64;
        let mut cur_pos = 0i32;
        while cur_pos < data_size.wrapping_sub(21) {
            let kind = i32::from(self.byte(cur_pos)? & 0x1f) - 0x10;
            if kind >= 0 {
                let cmd_mask = MASKS[kind as usize];
                for i in 0..=2 {
                    if cmd_mask & (1 << i) != 0 {
                        let start_pos = i * 41 + 5;
                        if self.itanium_bits(cur_pos, start_pos + 37, 4)? == 5 {
                            let offset = self.itanium_bits(cur_pos, start_pos + 13, 20)?;
                            let moved = (i64::from(offset) - file_offset) as i32 & 0xfffff;
                            self.set_itanium_bits(cur_pos, moved, start_pos + 13, 20)?;
                        }
                    }
                }
            }
            cur_pos += 16;
            file_offset += 1;
        }
        Ok(())
    }

    fn itanium_bits(&self, cur_pos: i32, bit_pos: i32, bit_count: u32) -> Result<i32, Thrown> {
        let field = self.int_at(cur_pos + bit_pos / 8)? as u32 >> (bit_pos & 7);
        Ok((field & (u32::MAX >> (32 - bit_count))) as i32)
    }

    fn set_itanium_bits(
        &mut self,
        cur_pos: i32,
        bit_field: i32,
        bit_pos: i32,
        bit_count: u32,
    ) -> Result<(), Thrown> {
        let at = cur_pos + bit_pos / 8;
        let in_bit = bit_pos & 7;
        let mut and_mask = !((u32::MAX >> (32 - bit_count)) << in_bit);
        let mut bit_field = (bit_field as u32) << in_bit;
        for i in 0..4 {
            let byte = self.byte(at + i)?;
            self.set_byte(at + i, (byte & and_mask as u8) | bit_field as u8)?;
            and_mask = (and_mask >> 8) | 0xff00_0000;
            bit_field >>= 8;
        }
        Ok(())
    }

    fn delta(&mut self, r: &[i32; 7]) -> Result<(), Thrown> {
        let data_size = r[4];
        let channels = r[0];
        let border = data_size.wrapping_mul(2);
        self.set_int(VM_GLOBALMEMADDR + 0x20, data_size)?;
        if data_size as u32 >= (VM_GLOBALMEMADDR / 2) as u32
            || channels as u32 > MAX3_UNPACK_CHANNELS
        {
            return Ok(());
        }
        let mut src_pos = 0i32;
        for cur_channel in 0..channels {
            let mut prev_byte = 0u8;
            let mut dest_pos = data_size + cur_channel;
            while dest_pos < border {
                prev_byte = prev_byte.wrapping_sub(self.byte(src_pos)?);
                src_pos += 1;
                self.set_byte(dest_pos, prev_byte)?;
                dest_pos += channels;
            }
        }
        Ok(())
    }

    fn rgb(&mut self, r: &[i32; 7]) -> Result<(), Thrown> {
        let data_size = r[4];
        let width = r[0].wrapping_sub(3);
        let pos_r = r[1];
        let dest = data_size;
        self.set_int(VM_GLOBALMEMADDR + 0x20, data_size)?;
        if data_size >= VM_GLOBALMEMADDR / 2 || pos_r < 0 {
            return Ok(());
        }
        let mut src_pos = 0i32;
        for cur_channel in 0..3 {
            let mut prev_byte = 0i64;
            let mut i = cur_channel;
            while i < data_size {
                let upper_pos = i.wrapping_sub(width);
                let predicted = if upper_pos >= 3 {
                    let upper_data_pos = dest.wrapping_add(upper_pos);
                    let upper_byte = i64::from(self.byte(upper_data_pos)?);
                    let upper_left_byte = i64::from(self.byte(upper_data_pos.wrapping_sub(3))?);
                    let predicted = prev_byte + upper_byte - upper_left_byte;
                    let pa = ((predicted - prev_byte) as i32).wrapping_abs();
                    let pb = ((predicted - upper_byte) as i32).wrapping_abs();
                    let pc = ((predicted - upper_left_byte) as i32).wrapping_abs();
                    if pa <= pb && pa <= pc {
                        prev_byte
                    } else if pb <= pc {
                        upper_byte
                    } else {
                        upper_left_byte
                    }
                } else {
                    prev_byte
                };
                prev_byte = (predicted - i64::from(self.byte(src_pos)?)) & 0xff;
                src_pos += 1;
                self.set_byte(dest + i, prev_byte as u8)?;
                i += 3;
            }
        }
        let mut i = pos_r;
        while i < data_size.wrapping_sub(2) {
            let at = dest.wrapping_add(i);
            let green = self.byte(at.wrapping_add(1))?;
            let red = self.byte(at)?;
            self.set_byte(at, red.wrapping_add(green))?;
            let blue = self.byte(at.wrapping_add(2))?;
            self.set_byte(at.wrapping_add(2), blue.wrapping_add(green))?;
            i = i.wrapping_add(3);
        }
        Ok(())
    }

    fn audio(&mut self, r: &[i32; 7]) -> Result<(), Thrown> {
        let data_size = r[4];
        let channels = r[0];
        let dest = data_size;
        self.set_int(VM_GLOBALMEMADDR + 0x20, data_size)?;
        if data_size >= VM_GLOBALMEMADDR / 2 {
            return Ok(());
        }
        let mut src_pos = 0i32;
        for cur_channel in 0..channels.min(data_size.max(0)) {
            let mut prev_byte = 0i64;
            let mut prev_delta = 0i64;
            let mut dif = [0i64; 7];
            let (mut d1, mut d2) = (0i32, 0i32);
            let (mut k1, mut k2, mut k3) = (0i32, 0i32, 0i32);
            let mut i = cur_channel;
            let mut byte_count = 0i32;
            while i < data_size {
                let d3 = d2;
                d2 = (prev_delta as i32).wrapping_sub(d1);
                d1 = prev_delta as i32;
                let mut predicted = 8 * prev_byte
                    + i64::from(k1.wrapping_mul(d1))
                    + i64::from(k2.wrapping_mul(d2))
                    + i64::from(k3.wrapping_mul(d3));
                predicted = (predicted as u64 >> 3) as i64 & 0xff;
                let cur_byte = i64::from(self.byte(src_pos)?);
                src_pos += 1;
                predicted = (predicted - cur_byte) & 0xffff_ffff;
                self.set_byte(dest.wrapping_add(i), predicted as u8)?;
                prev_delta = i64::from((predicted - prev_byte) as i8);
                prev_byte = predicted;
                let d = i32::from(cur_byte as u8 as i8) << 3;
                for (slot, value) in dif.iter_mut().zip([
                    d,
                    d.wrapping_sub(d1),
                    d.wrapping_add(d1),
                    d.wrapping_sub(d2),
                    d.wrapping_add(d2),
                    d.wrapping_sub(d3),
                    d.wrapping_add(d3),
                ]) {
                    *slot += i64::from(value.wrapping_abs());
                }
                if byte_count & 0x1f == 0 {
                    match super::least_dif(&mut dif) {
                        1 if k1 >= -16 => k1 -= 1,
                        2 if k1 < 16 => k1 += 1,
                        3 if k2 >= -16 => k2 -= 1,
                        4 if k2 < 16 => k2 += 1,
                        5 if k3 >= -16 => k3 -= 1,
                        6 if k3 < 16 => k3 += 1,
                        _ => {}
                    }
                }
                i = i.wrapping_add(channels);
                byte_count = byte_count.wrapping_add(1);
            }
        }
        Ok(())
    }
}

/// `RarVM.ReadData`: a number of 4, 8, 16 or 32 bits from a filter's code.
pub(super) fn read_data(input: &mut BitInput) -> Result<i32, Thrown> {
    let data = input.getbits()?;
    match data & 0xc000 {
        0 => {
            input.addbits(6);
            Ok((data >> 10) & 0xf)
        }
        0x4000 => {
            if data & 0x3c00 == 0 {
                input.addbits(14);
                Ok(0xffff_ff00_u32 as i32 | ((data >> 2) & 0xff))
            } else {
                input.addbits(10);
                Ok((data >> 6) & 0xff)
            }
        }
        0x8000 => {
            input.addbits(2);
            let data = input.getbits()?;
            input.addbits(16);
            Ok(data)
        }
        _ => {
            input.addbits(2);
            let high = input.getbits()? << 16;
            input.addbits(16);
            let low = input.getbits()?;
            input.addbits(16);
            Ok(high | low)
        }
    }
}
