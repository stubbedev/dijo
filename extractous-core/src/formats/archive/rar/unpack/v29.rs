//! `Unpack.unpack29`: RAR 2.9 to 4 data, LZ blocks of Huffman codes or PPMd blocks, with
//! filters over blocks of the output.

use super::bits::BitInput;
use super::{
    at, BlockType, Thrown, Unpack, BC, DC, DISTANCES, HUFF_TABLE_SIZE, LDC, LOW_DIST_REP_COUNT,
    L_BITS, L_DECODE, MAX_WIN_MASK, MAX_WIN_SIZE, NC, OUT_OF_BOUNDS, RC, SD_BITS, SD_DECODE,
};

impl Unpack<'_> {
    pub(super) fn unpack29(&mut self, solid: bool) -> Result<(), Thrown> {
        let (d_decode, d_bits) = DISTANCES;
        self.unp_init_data(solid);
        if !self.input.read_buf()? {
            return Ok(());
        }
        if (!solid || !self.tables_read) && !self.read_tables()? {
            return Ok(());
        }
        if self.ppm_error {
            return Ok(());
        }
        loop {
            self.unp_ptr &= MAX_WIN_MASK;
            self.first_win_done |= self.prev_ptr > self.unp_ptr;
            self.prev_ptr = self.unp_ptr;
            if self.input.bits.in_addr > self.input.read_border && !self.input.read_buf()? {
                break;
            }
            if (self.wr_ptr.wrapping_sub(self.unp_ptr) & MAX_WIN_MASK) < 260
                && self.wr_ptr != self.unp_ptr
            {
                self.unp_write_buf()?;
                if self.written_file_size > self.dest_unp_size {
                    return Ok(());
                }
            }
            if self.unp_block_type == BlockType::Ppm {
                let ch = self.ppm.decode_char(&mut self.input)?;
                if ch == -1 {
                    self.ppm_error = true;
                    break;
                }
                if ch == self.ppm_esc_char {
                    match self.safe_ppm_decode_char()? {
                        0 => {
                            if !self.read_tables()? {
                                break;
                            }
                            continue;
                        }
                        2 | -1 => break,
                        3 => {
                            if !self.read_vm_code_ppm()? {
                                break;
                            }
                            continue;
                        }
                        4 => {
                            let Some((distance, length)) = self.ppm_match()? else {
                                break;
                            };
                            self.copy_string(length + 32, distance + 2)?;
                            continue;
                        }
                        5 => {
                            let length = self.safe_ppm_decode_char()?;
                            if length == -1 {
                                break;
                            }
                            self.copy_string(length + 4, 1)?;
                            continue;
                        }
                        _ => {}
                    }
                }
                self.put(ch)?;
                continue;
            }
            let number = self.ld.decode(&mut self.input.bits)?;
            if number < 256 {
                self.put(number)?;
            } else if number >= 271 {
                let mut length = at(&L_DECODE, number - 271)? + 3;
                let bits = at(&L_BITS, number - 271)?;
                if bits > 0 {
                    length += (self.getbits()? as u32 >> (16 - bits)) as i32;
                    self.addbits(bits);
                }
                let dist_number = self.dd.decode(&mut self.input.bits)?;
                let mut distance = at(&d_decode, dist_number)? + 1;
                let bits = at(&d_bits, dist_number)?;
                if bits > 0 {
                    if dist_number > 9 {
                        if bits > 4 {
                            distance += ((self.getbits()? as u32 >> (20 - bits)) as i32) << 4;
                            self.addbits(bits - 4);
                        }
                        if self.low_dist_rep_count > 0 {
                            self.low_dist_rep_count -= 1;
                            distance += self.prev_low_dist;
                        } else {
                            let low_dist = self.ldd.decode(&mut self.input.bits)?;
                            if low_dist == 16 {
                                self.low_dist_rep_count = LOW_DIST_REP_COUNT - 1;
                                distance += self.prev_low_dist;
                            } else {
                                distance += low_dist;
                                self.prev_low_dist = low_dist;
                            }
                        }
                    } else {
                        distance += (self.getbits()? as u32 >> (16 - bits)) as i32;
                        self.addbits(bits);
                    }
                }
                if distance >= 0x2000 {
                    length += 1;
                    if distance >= 0x40000 {
                        length += 1;
                    }
                }
                self.insert_old_dist(distance);
                self.insert_last_match(length, distance);
                self.copy_string(length, distance)?;
            } else if number == 256 {
                if !self.read_end_of_block()? {
                    break;
                }
            } else if number == 257 {
                if !self.read_vm_code()? {
                    break;
                }
            } else if number == 258 {
                if self.last_length != 0 {
                    self.copy_string(self.last_length, self.last_dist)?;
                }
            } else if number < 263 {
                let dist_num = (number - 259) as usize;
                let distance = self.old_dist[dist_num];
                self.old_dist.copy_within(0..dist_num, 1);
                self.old_dist[0] = distance;
                let length_number = self.rd.decode(&mut self.input.bits)?;
                let mut length = at(&L_DECODE, length_number)? + 2;
                let bits = at(&L_BITS, length_number)?;
                if bits > 0 {
                    length += (self.getbits()? as u32 >> (16 - bits)) as i32;
                    self.addbits(bits);
                }
                self.insert_last_match(length, distance);
                self.copy_string(length, distance)?;
            } else {
                let mut distance = at(&SD_DECODE, number - 263)? + 1;
                let bits = at(&SD_BITS, number - 263)?;
                if bits > 0 {
                    distance += (self.getbits()? as u32 >> (16 - bits)) as i32;
                    self.addbits(bits);
                }
                self.insert_old_dist(distance);
                self.insert_last_match(2, distance);
                self.copy_string(2, distance)?;
            }
        }
        self.unp_write_buf()
    }

    /// A literal byte into the window.
    fn put(&mut self, byte: i32) -> Result<(), Thrown> {
        let at = self.unp_ptr as usize;
        self.window()?[at] = byte as u8;
        self.unp_ptr += 1;
        Ok(())
    }

    /// The distance and length of a PPMd block's long match, or `None` where the model breaks.
    fn ppm_match(&mut self) -> Result<Option<(i32, i32)>, Thrown> {
        let (mut distance, mut length) = (0, 0);
        for i in 0..4 {
            let ch = self.safe_ppm_decode_char()?;
            if ch == -1 {
                return Ok(None);
            }
            if i == 3 {
                length = ch & 0xff;
            } else {
                distance = (distance << 8) + (ch & 0xff);
            }
        }
        Ok(Some((distance, length)))
    }

    fn safe_ppm_decode_char(&mut self) -> Result<i32, Thrown> {
        let value = self.ppm.decode_char(&mut self.input)?;
        if value == -1 {
            self.ppm_error = true;
        }
        Ok(value)
    }

    fn insert_old_dist(&mut self, distance: i32) {
        self.old_dist.copy_within(0..3, 1);
        self.old_dist[0] = distance;
    }

    fn insert_last_match(&mut self, length: i32, distance: i32) {
        self.last_dist = distance;
        self.last_length = length;
    }

    /// `copyString`: `length` bytes from `distance` back, by a fast copy where neither end is
    /// near the window's end, which fails as Java's does past it.
    fn copy_string(&mut self, length: i32, distance: i32) -> Result<(), Thrown> {
        let first_win_done = self.first_win_done;
        let mut ptr = self.unp_ptr;
        let mut dest_ptr = ptr.wrapping_sub(distance);
        let mut length = length;
        let window = self.window()?;
        if (0..MAX_WIN_SIZE - 260).contains(&dest_ptr) && ptr < MAX_WIN_SIZE - 260 {
            let (from, to) = (dest_ptr as usize, ptr as usize);
            if distance == 1 {
                let count = usize::try_from(length).map_err(|_| OUT_OF_BOUNDS)?;
                let byte = window[from];
                window
                    .get_mut(to..to + count)
                    .ok_or(OUT_OF_BOUNDS)?
                    .fill(byte);
                ptr += length;
            } else if dest_ptr + length <= ptr {
                let count = usize::try_from(length).map_err(|_| OUT_OF_BOUNDS)?;
                if to + count > window.len() {
                    return Err(OUT_OF_BOUNDS);
                }
                window.copy_within(from..from + count, to);
                ptr += length;
            } else {
                loop {
                    let byte = *window.get(dest_ptr as usize).ok_or(OUT_OF_BOUNDS)?;
                    *window.get_mut(ptr as usize).ok_or(OUT_OF_BOUNDS)? = byte;
                    ptr += 1;
                    dest_ptr += 1;
                    length -= 1;
                    if length <= 0 {
                        break;
                    }
                }
            }
        } else if distance > ptr && (!first_win_done || distance > MAX_WIN_SIZE) {
            while length != 0 {
                length = length.wrapping_sub(1);
                window[ptr as usize] = 0;
                ptr = (ptr + 1) & MAX_WIN_MASK;
            }
        } else {
            while length != 0 {
                length = length.wrapping_sub(1);
                window[ptr as usize] = window[(dest_ptr & MAX_WIN_MASK) as usize];
                dest_ptr = dest_ptr.wrapping_add(1);
                ptr = (ptr + 1) & MAX_WIN_MASK;
            }
        }
        self.unp_ptr = ptr;
        Ok(())
    }

    fn read_end_of_block(&mut self) -> Result<bool, Thrown> {
        let bit_field = self.getbits()?;
        let (new_table, new_file) = if bit_field & 0x8000 != 0 {
            self.addbits(1);
            (true, false)
        } else {
            self.addbits(2);
            (bit_field & 0x4000 != 0, true)
        };
        self.tables_read = !new_table;
        Ok(!(new_file || new_table && !self.read_tables()?))
    }

    pub(super) fn read_tables(&mut self) -> Result<bool, Thrown> {
        let mut bit_length = [0u8; BC];
        let mut table = [0u8; HUFF_TABLE_SIZE];
        if self.input.bits.in_addr > self.input.read_top - 25 && !self.input.read_buf()? {
            return Ok(false);
        }
        self.addbits((8 - self.input.bits.in_bit) & 7);
        let bit_field = self.getbits()?;
        if bit_field & 0x8000 != 0 {
            self.unp_block_type = BlockType::Ppm;
            return self
                .ppm
                .decode_init(&mut self.input, &mut self.ppm_esc_char);
        }
        self.unp_block_type = BlockType::Lz;
        self.prev_low_dist = 0;
        self.low_dist_rep_count = 0;
        if bit_field & 0x4000 == 0 {
            self.unp_old_table = [0; HUFF_TABLE_SIZE];
        }
        self.addbits(2);
        let mut i = 0;
        while i < BC {
            let length = (self.getbits()? >> 12) & 0xff;
            self.addbits(4);
            if length == 15 {
                let mut zero_count = (self.getbits()? >> 12) & 0xff;
                self.addbits(4);
                if zero_count == 0 {
                    bit_length[i] = 15;
                } else {
                    zero_count += 2;
                    while zero_count > 0 && i < BC {
                        zero_count -= 1;
                        bit_length[i] = 0;
                        i += 1;
                    }
                    i -= 1;
                }
            } else {
                bit_length[i] = length as u8;
            }
            i += 1;
        }
        self.bd.make(&bit_length, BC)?;
        let mut i = 0;
        while i < HUFF_TABLE_SIZE {
            if self.input.bits.in_addr > self.input.read_top - 5 && !self.input.read_buf()? {
                return Ok(false);
            }
            let number = self.bd.decode(&mut self.input.bits)?;
            if number < 16 {
                table[i] = ((number + i32::from(self.unp_old_table[i])) & 0xf) as u8;
                i += 1;
            } else if number < 18 {
                let mut n = self.repeat_count(number == 16)?;
                while n > 0 && i < HUFF_TABLE_SIZE {
                    n -= 1;
                    table[i] = *table.get(i.wrapping_sub(1)).ok_or(OUT_OF_BOUNDS)?;
                    i += 1;
                }
            } else {
                let mut n = self.repeat_count(number == 18)?;
                while n > 0 && i < HUFF_TABLE_SIZE {
                    n -= 1;
                    table[i] = 0;
                    i += 1;
                }
            }
        }
        self.tables_read = true;
        if self.input.bits.in_addr > self.input.read_top {
            return Ok(false);
        }
        self.ld.make(&table, NC)?;
        self.dd.make(&table[NC..], DC)?;
        self.ldd.make(&table[NC + DC..], LDC)?;
        self.rd.make(&table[NC + DC + LDC..], RC)?;
        self.unp_old_table = table;
        Ok(true)
    }

    /// How many times a table length repeats: 3 to 10 in three bits, or 11 to 138 in seven.
    fn repeat_count(&mut self, short: bool) -> Result<i32, Thrown> {
        let field = self.getbits()?;
        Ok(if short {
            self.addbits(3);
            (field >> 13) + 3
        } else {
            self.addbits(7);
            (field >> 9) + 11
        })
    }

    fn read_vm_code(&mut self) -> Result<bool, Thrown> {
        let first_byte = self.getbits()? >> 8;
        self.addbits(8);
        let mut length = (first_byte & 7) + 1;
        if length == 7 {
            length = (self.getbits()? >> 8) + 7;
            self.addbits(8);
        } else if length == 8 {
            length = self.getbits()?;
            self.addbits(16);
        }
        let mut code = Vec::with_capacity(length as usize);
        for i in 0..length {
            if self.input.bits.in_addr >= self.input.read_top - 1
                && !self.input.read_buf()?
                && i < length - 1
            {
                return Ok(false);
            }
            code.push((self.getbits()? >> 8) as u8);
            self.addbits(8);
        }
        self.add_vm_code(first_byte, &code)
    }

    fn read_vm_code_ppm(&mut self) -> Result<bool, Thrown> {
        let first_byte = self.safe_ppm_decode_char()?;
        if first_byte == -1 {
            return Ok(false);
        }
        let mut length = (first_byte & 7) + 1;
        if length == 7 {
            let b1 = self.safe_ppm_decode_char()?;
            if b1 == -1 {
                return Ok(false);
            }
            length = b1 + 7;
        } else if length == 8 {
            let b1 = self.safe_ppm_decode_char()?;
            if b1 == -1 {
                return Ok(false);
            }
            let b2 = self.safe_ppm_decode_char()?;
            if b2 == -1 {
                return Ok(false);
            }
            length = b1 * 256 + b2;
        }
        let mut code = Vec::with_capacity(length as usize);
        for _ in 0..length {
            let ch = self.safe_ppm_decode_char()?;
            if ch == -1 {
                return Ok(false);
            }
            code.push(ch as u8);
        }
        self.add_vm_code(first_byte, &code)
    }

    /// A filter's code in a bit input of its own, as much of it as the buffer holds.
    pub(super) fn vm_input(code: &[u8]) -> BitInput {
        let mut input = BitInput::new();
        let copied = code.len().min(input.buf.len());
        input.buf[..copied].copy_from_slice(&code[..copied]);
        input
    }
}
