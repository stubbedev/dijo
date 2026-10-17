//! `Unpack15`: RAR 1.5 data, coded with adaptive tables of fixed Huffman-like codes, and the
//! buffer handling and window copies `Unpack20` shares.

use super::{at, Thrown, Unpack, MAX_WIN_MASK, MAX_WIN_SIZE, NULL, OUT_OF_BOUNDS};

const STARTL1: i32 = 2;
const DEC_L1: [i32; 11] = [
    0x8000, 0xa000, 0xc000, 0xd000, 0xe000, 0xea00, 0xee00, 0xf000, 0xf200, 0xf200, 0xffff,
];
const POS_L1: [i32; 13] = [0, 0, 0, 2, 3, 5, 7, 11, 16, 20, 24, 32, 32];
const STARTL2: i32 = 3;
const DEC_L2: [i32; 10] = [
    0xa000, 0xc000, 0xd000, 0xe000, 0xea00, 0xee00, 0xf000, 0xf200, 0xf240, 0xffff,
];
const POS_L2: [i32; 13] = [0, 0, 0, 0, 5, 7, 9, 13, 18, 22, 26, 34, 36];
const STARTHF0: i32 = 4;
const DEC_HF0: [i32; 9] = [
    0x8000, 0xc000, 0xe000, 0xf200, 0xf200, 0xf200, 0xf200, 0xf200, 0xffff,
];
const POS_HF0: [i32; 13] = [0, 0, 0, 0, 0, 8, 16, 24, 33, 33, 33, 33, 33];
const STARTHF1: i32 = 5;
const DEC_HF1: [i32; 8] = [
    0x2000, 0xc000, 0xe000, 0xf000, 0xf200, 0xf200, 0xf7e0, 0xffff,
];
const POS_HF1: [i32; 13] = [0, 0, 0, 0, 0, 0, 4, 44, 60, 76, 80, 80, 127];
const STARTHF2: i32 = 5;
const DEC_HF2: [i32; 8] = [
    0x1000, 0x2400, 0x8000, 0xc000, 0xfa00, 0xffff, 0xffff, 0xffff,
];
const POS_HF2: [i32; 13] = [0, 0, 0, 0, 0, 0, 2, 7, 53, 117, 233, 0, 0];
const STARTHF3: i32 = 6;
const DEC_HF3: [i32; 7] = [0x800, 0x2400, 0xee00, 0xfe80, 0xffff, 0xffff, 0xffff];
const POS_HF3: [i32; 13] = [0, 0, 0, 0, 0, 0, 0, 2, 16, 218, 251, 0, 0];
const STARTHF4: i32 = 8;
const DEC_HF4: [i32; 6] = [0xff00, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff];
const POS_HF4: [i32; 13] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0];
const SHORT_LEN1: [i32; 16] = [1, 3, 4, 4, 5, 6, 7, 8, 8, 4, 4, 5, 6, 6, 4, 0];
const SHORT_XOR1: [i32; 15] = [
    0, 0xa0, 0xd0, 0xe0, 0xf0, 0xf8, 0xfc, 0xfe, 0xff, 0xc0, 0x80, 0x90, 0x98, 0x9c, 0xb0,
];
const SHORT_LEN2: [i32; 16] = [2, 3, 3, 3, 4, 4, 5, 6, 6, 4, 4, 5, 6, 6, 4, 0];
const SHORT_XOR2: [i32; 15] = [
    0, 0x40, 0x60, 0xa0, 0xd0, 0xe0, 0xf0, 0xf8, 0xfc, 0xc0, 0x80, 0x90, 0x98, 0x9c, 0xb0,
];

/// A table of `decodeNum`: where its codes of each length start, and their first positions.
struct Table {
    start: i32,
    decode: &'static [i32],
    positions: &'static [i32; 13],
}

const L1: Table = Table {
    start: STARTL1,
    decode: &DEC_L1,
    positions: &POS_L1,
};
const L2: Table = Table {
    start: STARTL2,
    decode: &DEC_L2,
    positions: &POS_L2,
};
const HF0: Table = Table {
    start: STARTHF0,
    decode: &DEC_HF0,
    positions: &POS_HF0,
};
const HF1: Table = Table {
    start: STARTHF1,
    decode: &DEC_HF1,
    positions: &POS_HF1,
};
const HF2: Table = Table {
    start: STARTHF2,
    decode: &DEC_HF2,
    positions: &POS_HF2,
};
const HF3: Table = Table {
    start: STARTHF3,
    decode: &DEC_HF3,
    positions: &POS_HF3,
};
const HF4: Table = Table {
    start: STARTHF4,
    decode: &DEC_HF4,
    positions: &POS_HF4,
};

/// The adaptive state of `Unpack15`.
pub(super) struct State {
    ch_set: [i32; 256],
    ch_set_a: [i32; 256],
    ch_set_b: [i32; 256],
    ch_set_c: [i32; 256],
    n_to_pl: [i32; 256],
    n_to_pl_b: [i32; 256],
    n_to_pl_c: [i32; 256],
    flag_buf: i32,
    avr_plc: i32,
    avr_plc_b: i32,
    avr_ln1: i32,
    avr_ln2: i32,
    avr_ln3: i32,
    buf60: i32,
    num_huf: i32,
    st_mode: i32,
    l_count: i32,
    flags_cnt: i32,
    nhfb: i32,
    nlzb: i32,
    max_dist3: i32,
}

impl Default for State {
    fn default() -> Self {
        State {
            ch_set: [0; 256],
            ch_set_a: [0; 256],
            ch_set_b: [0; 256],
            ch_set_c: [0; 256],
            n_to_pl: [0; 256],
            n_to_pl_b: [0; 256],
            n_to_pl_c: [0; 256],
            flag_buf: 0,
            avr_plc: 0,
            avr_plc_b: 0,
            avr_ln1: 0,
            avr_ln2: 0,
            avr_ln3: 0,
            buf60: 0,
            num_huf: 0,
            st_mode: 0,
            l_count: 0,
            flags_cnt: 0,
            nhfb: 0,
            nlzb: 0,
            max_dist3: 0,
        }
    }
}

/// `corrHuff`: the places of a character set reset to their groups of 32.
fn corr_huff(char_set: &mut [i32; 256], num_to_place: &mut [i32; 256]) {
    for (group, chunk) in char_set.chunks_mut(32).enumerate() {
        for value in chunk {
            *value = (*value & !0xff) | (7 - group as i32);
        }
    }
    *num_to_place = [0; 256];
    for (i, place) in num_to_place.iter_mut().take(7).enumerate() {
        *place = (7 - i as i32) * 32;
    }
}

/// One step of the move-to-front lists: the entry at `place` moved up, the lists rebuilt
/// whenever an entry's count reaches `limit`; the entry's new value.
fn promote(
    char_set: &mut [i32; 256],
    num_to_place: &mut [i32; 256],
    place: usize,
    overflowed: impl Fn(i32) -> bool,
) -> Result<i32, Thrown> {
    let (value, new_place) = loop {
        let mut value = char_set[place];
        let slot = (value & 0xff) as usize;
        let new_place = num_to_place[slot];
        num_to_place[slot] += 1;
        value += 1;
        if overflowed(value) {
            corr_huff(char_set, num_to_place);
        } else {
            break (value, new_place);
        }
    };
    let new_place = usize::try_from(new_place)
        .ok()
        .filter(|&new_place| new_place < 256)
        .ok_or(OUT_OF_BOUNDS)?;
    char_set[place] = char_set[new_place];
    char_set[new_place] = value;
    Ok(value)
}

impl Unpack<'_> {
    pub(super) fn unpack15(&mut self, solid: bool) -> Result<(), Thrown> {
        self.unp_init_data(solid);
        self.old_unp_init_data(solid);
        self.input.read_buf()?;
        if solid {
            self.unp_ptr = self.wr_ptr;
        } else {
            self.init_huff();
            self.unp_ptr = 0;
        }
        self.dest_unp_size -= 1;
        if self.dest_unp_size >= 0 {
            self.get_flags_buf()?;
            self.v15.flags_cnt = 8;
        }
        while self.dest_unp_size >= 0 {
            self.unp_ptr &= MAX_WIN_MASK;
            self.first_win_done |= self.prev_ptr > self.unp_ptr;
            self.prev_ptr = self.unp_ptr;
            if self.input.bits.in_addr > self.input.read_top - 30 && !self.input.read_buf()? {
                break;
            }
            if (self.wr_ptr.wrapping_sub(self.unp_ptr) & MAX_WIN_MASK) < 270
                && self.wr_ptr != self.unp_ptr
            {
                self.old_unp_write_buf()?;
            }
            if self.v15.st_mode != 0 {
                self.huff_decode()?;
                continue;
            }
            self.v15.flags_cnt -= 1;
            if self.v15.flags_cnt < 0 {
                self.get_flags_buf()?;
                self.v15.flags_cnt = 7;
            }
            if self.v15.flag_buf & 0x80 != 0 {
                self.v15.flag_buf <<= 1;
                if self.v15.nlzb > self.v15.nhfb {
                    self.long_lz()?;
                } else {
                    self.huff_decode()?;
                }
            } else {
                self.v15.flag_buf <<= 1;
                self.v15.flags_cnt -= 1;
                if self.v15.flags_cnt < 0 {
                    self.get_flags_buf()?;
                    self.v15.flags_cnt = 7;
                }
                if self.v15.flag_buf & 0x80 != 0 {
                    self.v15.flag_buf <<= 1;
                    if self.v15.nlzb > self.v15.nhfb {
                        self.huff_decode()?;
                    } else {
                        self.long_lz()?;
                    }
                } else {
                    self.v15.flag_buf <<= 1;
                    self.short_lz()?;
                }
            }
        }
        self.old_unp_write_buf()
    }

    fn decode_num(&mut self, num: i32, table: &Table) -> Result<i32, Thrown> {
        let num = num & 0xfff0;
        let mut start = table.start;
        let mut i = 0;
        while at(table.decode, i)? <= num {
            start += 1;
            i += 1;
        }
        self.addbits(start);
        let base = if i != 0 { at(table.decode, i - 1)? } else { 0 };
        Ok(((num - base) as u32 >> (16 - start)) as i32 + at(table.positions, start)?)
    }

    fn short_len(&self, second: bool, pos: i32) -> Result<i32, Thrown> {
        match (second, pos) {
            (false, 1) | (true, 3) => Ok(self.v15.buf60 + 3),
            (false, _) => at(&SHORT_LEN1, pos),
            (true, _) => at(&SHORT_LEN2, pos),
        }
    }

    fn short_lz(&mut self) -> Result<(), Thrown> {
        self.v15.num_huf = 0;
        let mut bit_field = self.getbits()?;
        if self.v15.l_count == 2 {
            self.addbits(1);
            if bit_field >= 0x8000 {
                return self.old_copy_string(self.last_dist, self.last_length);
            }
            bit_field <<= 1;
            self.v15.l_count = 0;
        }
        bit_field = (bit_field as u32 >> 8) as i32;
        let second = self.v15.avr_ln1 >= 37;
        let xors: &[i32] = if second { &SHORT_XOR2 } else { &SHORT_XOR1 };
        let mut length = 0;
        loop {
            let mask = !(0xff_u32 >> self.short_len(second, length)?) as i32;
            if (bit_field ^ at(xors, length)?) & mask == 0 {
                break;
            }
            length += 1;
        }
        let bits = self.short_len(second, length)?;
        self.addbits(bits);
        if length >= 9 {
            if length == 9 {
                self.v15.l_count += 1;
                return self.old_copy_string(self.last_dist, self.last_length);
            }
            if length == 14 {
                self.v15.l_count = 0;
                let field = self.getbits()?;
                let length = self.decode_num(field, &L2)? + 5;
                let distance = (self.getbits()? >> 1) | 0x8000;
                self.addbits(15);
                self.last_length = length;
                self.last_dist = distance;
                return self.old_copy_string(distance, length);
            }
            self.v15.l_count = 0;
            let save_length = length;
            let distance = self.old_dist[(self.old_dist_ptr.wrapping_sub(length - 9) & 3) as usize];
            let field = self.getbits()?;
            let mut length = self.decode_num(field, &L1)? + 2;
            if length == 0x101 && save_length == 10 {
                self.v15.buf60 ^= 1;
                return Ok(());
            }
            if distance > 256 {
                length += 1;
            }
            if distance >= self.v15.max_dist3 {
                length += 1;
            }
            self.push_old_dist(distance)?;
            self.last_length = length;
            self.last_dist = distance;
            return self.old_copy_string(distance, length);
        }
        self.v15.l_count = 0;
        self.v15.avr_ln1 += length;
        self.v15.avr_ln1 -= self.v15.avr_ln1 >> 4;
        let field = self.getbits()?;
        let mut distance_place = (self.decode_num(field, &HF2)? & 0xff) as usize;
        let mut distance = self.v15.ch_set_a[distance_place];
        if distance_place != 0 {
            distance_place -= 1;
            let last_distance = self.v15.ch_set_a[distance_place];
            self.v15.ch_set_a[distance_place + 1] = last_distance;
            self.v15.ch_set_a[distance_place] = distance;
        }
        length += 2;
        distance += 1;
        self.push_old_dist(distance)?;
        self.last_length = length;
        self.last_dist = distance;
        self.old_copy_string(distance, length)
    }

    /// `oldDist[oldDistPtr++] = distance; oldDistPtr &= 3`, failing as Java does where
    /// `Unpack20` left the pointer past the array.
    fn push_old_dist(&mut self, distance: i32) -> Result<(), Thrown> {
        let slot = usize::try_from(self.old_dist_ptr)
            .ok()
            .and_then(|at| self.old_dist.get_mut(at))
            .ok_or(OUT_OF_BOUNDS)?;
        *slot = distance;
        self.old_dist_ptr = (self.old_dist_ptr + 1) & 3;
        Ok(())
    }

    fn long_lz(&mut self) -> Result<(), Thrown> {
        self.v15.num_huf = 0;
        self.v15.nlzb += 16;
        if self.v15.nlzb > 0xff {
            self.v15.nlzb = 0x90;
            self.v15.nhfb >>= 1;
        }
        let old_avr2 = self.v15.avr_ln2;
        let bit_field = self.getbits()?;
        let mut length = if self.v15.avr_ln2 >= 122 {
            self.decode_num(bit_field, &L2)?
        } else if self.v15.avr_ln2 >= 64 {
            self.decode_num(bit_field, &L1)?
        } else if bit_field < 0x100 {
            self.addbits(16);
            bit_field
        } else {
            let mut length = 0;
            while (bit_field << length) & 0x8000 == 0 {
                length += 1;
            }
            self.addbits(length + 1);
            length
        };
        self.v15.avr_ln2 += length;
        self.v15.avr_ln2 -= self.v15.avr_ln2 >> 5;
        let bit_field = self.getbits()?;
        let distance_place = if self.v15.avr_plc_b > 0x28ff {
            self.decode_num(bit_field, &HF2)?
        } else if self.v15.avr_plc_b > 0x6ff {
            self.decode_num(bit_field, &HF1)?
        } else {
            self.decode_num(bit_field, &HF0)?
        };
        self.v15.avr_plc_b += distance_place;
        self.v15.avr_plc_b -= self.v15.avr_plc_b >> 8;
        let place = (distance_place & 0xff) as usize;
        let mut distance = promote(
            &mut self.v15.ch_set_b,
            &mut self.v15.n_to_pl_b,
            place,
            |value| value & 0xff == 0,
        )?;
        distance = ((distance & 0xff00) | (self.getbits()? >> 8)) >> 1;
        self.addbits(7);
        let old_avr3 = self.v15.avr_ln3;
        if length != 1 && length != 4 {
            if length == 0 && distance <= self.v15.max_dist3 {
                self.v15.avr_ln3 += 1;
                self.v15.avr_ln3 -= self.v15.avr_ln3 >> 8;
            } else if self.v15.avr_ln3 > 0 {
                self.v15.avr_ln3 -= 1;
            }
        }
        length += 3;
        if distance >= self.v15.max_dist3 {
            length += 1;
        }
        if distance <= 256 {
            length += 8;
        }
        self.v15.max_dist3 = if old_avr3 > 0xb0 || self.v15.avr_plc >= 0x2a00 && old_avr2 < 0x40 {
            0x7f00
        } else {
            0x2001
        };
        self.push_old_dist(distance)?;
        self.last_length = length;
        self.last_dist = distance;
        self.old_copy_string(distance, length)
    }

    fn huff_decode(&mut self) -> Result<(), Thrown> {
        let mut bit_field = self.getbits()?;
        let table = match self.v15.avr_plc {
            plc if plc > 0x75ff => &HF4,
            plc if plc > 0x5dff => &HF3,
            plc if plc > 0x35ff => &HF2,
            plc if plc > 0x0dff => &HF1,
            _ => &HF0,
        };
        let mut byte_place = self.decode_num(bit_field, table)? & 0xff;
        if self.v15.st_mode != 0 {
            if byte_place == 0 && bit_field > 0xfff {
                byte_place = 0x100;
            }
            byte_place -= 1;
            if byte_place == -1 {
                bit_field = self.getbits()?;
                self.addbits(1);
                if bit_field & 0x8000 != 0 {
                    self.v15.num_huf = 0;
                    self.v15.st_mode = 0;
                    return Ok(());
                }
                let length = if bit_field & 0x4000 != 0 { 4 } else { 3 };
                self.addbits(1);
                let field = self.getbits()?;
                let distance = self.decode_num(field, &HF2)?;
                let distance = (distance << 5) | (self.getbits()? >> 11);
                self.addbits(5);
                return self.old_copy_string(distance, length);
            }
        } else {
            let num_huf = self.v15.num_huf;
            self.v15.num_huf += 1;
            if num_huf >= 16 && self.v15.flags_cnt == 0 {
                self.v15.st_mode = 1;
            }
        }
        self.v15.avr_plc += byte_place;
        self.v15.avr_plc -= self.v15.avr_plc >> 8;
        self.v15.nhfb += 16;
        if self.v15.nhfb > 0xff {
            self.v15.nhfb = 0x90;
            self.v15.nlzb >>= 1;
        }
        let place = byte_place as usize;
        let byte = (self.v15.ch_set[place] as u32 >> 8) as u8;
        let ptr = self.unp_ptr as usize;
        self.window()?[ptr] = byte;
        self.unp_ptr += 1;
        self.dest_unp_size -= 1;
        promote(
            &mut self.v15.ch_set,
            &mut self.v15.n_to_pl,
            place,
            |value| value & 0xff > 0xa1,
        )?;
        Ok(())
    }

    fn get_flags_buf(&mut self) -> Result<(), Thrown> {
        let field = self.getbits()?;
        let flags_place = self.decode_num(field, &HF2)?;
        if flags_place >= 256 {
            return Ok(());
        }
        let place = usize::try_from(flags_place).map_err(|_| OUT_OF_BOUNDS)?;
        let state = &mut self.v15;
        let flags = promote(&mut state.ch_set_c, &mut state.n_to_pl_c, place, |value| {
            value & 0xff == 0
        })?;
        state.flag_buf = ((flags - 1) as u32 >> 8) as i32;
        Ok(())
    }

    fn old_unp_init_data(&mut self, solid: bool) {
        let state = &mut self.v15;
        if !solid {
            state.avr_plc_b = 0;
            state.avr_ln1 = 0;
            state.avr_ln2 = 0;
            state.avr_ln3 = 0;
            state.num_huf = 0;
            state.buf60 = 0;
            state.avr_plc = 0x3500;
            state.max_dist3 = 0x2001;
            state.nhfb = 0x80;
            state.nlzb = 0x80;
        }
        state.flags_cnt = 0;
        state.flag_buf = 0;
        state.st_mode = 0;
        state.l_count = 0;
        self.input.read_top = 0;
    }

    fn init_huff(&mut self) {
        let state = &mut self.v15;
        for i in 0..256 {
            let reversed = (!(i as i32) + 1) & 0xff;
            state.ch_set[i] = (i as i32) << 8;
            state.ch_set_b[i] = (i as i32) << 8;
            state.ch_set_a[i] = i as i32;
            state.ch_set_c[i] = reversed << 8;
        }
        state.n_to_pl = [0; 256];
        state.n_to_pl_b = [0; 256];
        state.n_to_pl_c = [0; 256];
        corr_huff(&mut state.ch_set_b, &mut state.n_to_pl_b);
    }

    /// `oldCopyString`: `length` bytes from `distance` back, of the old unpackers.
    pub(super) fn old_copy_string(&mut self, distance: i32, length: i32) -> Result<(), Thrown> {
        self.dest_unp_size -= i64::from(length);
        let first_win_done = self.first_win_done;
        let mut ptr = self.unp_ptr;
        let mut length = length;
        let window = self.window()?;
        if (!first_win_done && distance > ptr) || distance > MAX_WIN_SIZE {
            while length != 0 {
                length = length.wrapping_sub(1);
                *window.get_mut(ptr as usize).ok_or(OUT_OF_BOUNDS)? = 0;
                ptr = (ptr + 1) & MAX_WIN_MASK;
            }
            self.unp_ptr = ptr;
            return Ok(());
        }
        let mut dest_ptr = ptr.wrapping_sub(distance) & MAX_WIN_MASK;
        if distance == 1 {
            let count = usize::try_from(length).map_err(|_| OUT_OF_BOUNDS)?;
            let byte = window[dest_ptr as usize];
            let to = usize::try_from(ptr).map_err(|_| OUT_OF_BOUNDS)?;
            window
                .get_mut(to..to + count)
                .ok_or(OUT_OF_BOUNDS)?
                .fill(byte);
            ptr += length;
        } else if dest_ptr + length <= ptr {
            let count = usize::try_from(length).map_err(|_| OUT_OF_BOUNDS)?;
            let (from, to) = (dest_ptr as usize, ptr as usize);
            if to + count > window.len() {
                return Err(OUT_OF_BOUNDS);
            }
            window.copy_within(from..from + count, to);
            ptr += length;
        } else {
            while length != 0 {
                length = length.wrapping_sub(1);
                let byte = window[dest_ptr as usize];
                *window.get_mut(ptr as usize).ok_or(OUT_OF_BOUNDS)? = byte;
                ptr = (ptr + 1) & MAX_WIN_MASK;
                dest_ptr = (dest_ptr + 1) & MAX_WIN_MASK;
            }
        }
        self.unp_ptr = ptr;
        Ok(())
    }

    /// `oldUnpWriteBuf`: the window from the last written position to the unpacking
    /// position written out, all of it.
    pub(super) fn old_unp_write_buf(&mut self) -> Result<(), Thrown> {
        let window = self.window.as_deref().ok_or(NULL)?;
        let (wr_ptr, unp_ptr) = (self.wr_ptr, self.unp_ptr);
        if unp_ptr < wr_ptr {
            self.input
                .io
                .write(window, wr_ptr, wr_ptr.wrapping_neg() & MAX_WIN_MASK)?;
            self.input.io.write(window, 0, unp_ptr)?;
        } else {
            self.input.io.write(window, wr_ptr, unp_ptr - wr_ptr)?;
        }
        self.wr_ptr = unp_ptr;
        Ok(())
    }
}
