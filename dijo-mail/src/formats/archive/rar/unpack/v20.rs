//! `Unpack20`: RAR 2 data, LZ with Huffman codes or audio deltas over up to four channels.

use super::huffman::Decode;
use super::{
    at, AudioVariables, Thrown, Unpack, BC20, DC20, L_BITS, L_DECODE, MAX_WIN_MASK, MAX_WIN_SIZE,
    MC20, NC20, NULL, OUT_OF_BOUNDS, RC20, SD_BITS, SD_DECODE,
};

/// `Unpack20.DDecode` and `DBits`: RAR 2's distance slots.
const D_DECODE: [i32; 48] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608,
    262144, 327680, 393216, 458752, 524288, 589824, 655360, 720896, 786432, 851968, 917504, 983040,
];
const D_BITS: [i32; 48] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
];

/// The table of audio channel `channel`, which junrar makes only once it has started an
/// entry that does not continue a solid stream.
fn channel_decode(md: &[Option<Decode>; 4], channel: i32) -> Result<&Decode, Thrown> {
    md.get(channel as usize)
        .ok_or(OUT_OF_BOUNDS)?
        .as_ref()
        .ok_or(NULL)
}

impl Unpack<'_> {
    pub(super) fn unpack20(&mut self, solid: bool) -> Result<(), Thrown> {
        self.unp_init_data(solid);
        if !self.input.read_buf()? {
            return Ok(());
        }
        if !solid && !self.read_tables20()? {
            return Ok(());
        }
        self.dest_unp_size -= 1;
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
            if self.unp_audio_block != 0 {
                let audio_number =
                    channel_decode(&self.md, self.unp_cur_channel)?.decode(&mut self.input.bits)?;
                if audio_number == 256 {
                    if !self.read_tables20()? {
                        break;
                    }
                    continue;
                }
                let byte = self.decode_audio(audio_number)?;
                let ptr = self.unp_ptr as usize;
                self.window()?[ptr] = byte;
                self.unp_ptr += 1;
                self.unp_cur_channel += 1;
                if self.unp_cur_channel == self.unp_channels {
                    self.unp_cur_channel = 0;
                }
                self.dest_unp_size -= 1;
                continue;
            }
            let number = self.ld.decode(&mut self.input.bits)?;
            if number < 256 {
                let ptr = self.unp_ptr as usize;
                self.window()?[ptr] = number as u8;
                self.unp_ptr += 1;
                self.dest_unp_size -= 1;
            } else if number > 269 {
                let mut length = at(&L_DECODE, number - 270)? + 3;
                let bits = at(&L_BITS, number - 270)?;
                if bits > 0 {
                    length += (self.getbits()? as u32 >> (16 - bits)) as i32;
                    self.addbits(bits);
                }
                let dist_number = self.dd.decode(&mut self.input.bits)?;
                let mut distance = at(&D_DECODE, dist_number)? + 1;
                let bits = at(&D_BITS, dist_number)?;
                if bits > 0 {
                    distance += (self.getbits()? as u32 >> (16 - bits)) as i32;
                    self.addbits(bits);
                }
                if distance >= 0x2000 {
                    length += 1;
                    if distance >= 0x40000 {
                        length += 1;
                    }
                }
                self.copy_string20(length, distance)?;
            } else if number == 269 {
                if !self.read_tables20()? {
                    break;
                }
            } else if number == 256 {
                self.copy_string20(self.last_length, self.last_dist)?;
            } else if number < 261 {
                let distance =
                    self.old_dist[(self.old_dist_ptr.wrapping_sub(number - 256) & 3) as usize];
                let length_number = self.rd.decode(&mut self.input.bits)?;
                let mut length = at(&L_DECODE, length_number)? + 2;
                let bits = at(&L_BITS, length_number)?;
                if bits > 0 {
                    length += (self.getbits()? as u32 >> (16 - bits)) as i32;
                    self.addbits(bits);
                }
                if distance >= 0x101 {
                    length += 1;
                    if distance >= 0x2000 {
                        length += 1;
                        if distance >= 0x40000 {
                            length += 1;
                        }
                    }
                }
                self.copy_string20(length, distance)?;
            } else {
                let mut distance = at(&SD_DECODE, number - 261)? + 1;
                let bits = at(&SD_BITS, number - 261)?;
                if bits > 0 {
                    distance += (self.getbits()? as u32 >> (16 - bits)) as i32;
                    self.addbits(bits);
                }
                self.copy_string20(2, distance)?;
            }
        }
        self.read_last_tables()?;
        self.old_unp_write_buf()
    }

    fn copy_string20(&mut self, length: i32, distance: i32) -> Result<(), Thrown> {
        self.last_dist = distance;
        self.old_dist[(self.old_dist_ptr & 3) as usize] = distance;
        self.old_dist_ptr = self.old_dist_ptr.wrapping_add(1);
        self.last_length = length;
        self.dest_unp_size -= i64::from(length);
        let first_win_done = self.first_win_done;
        let mut ptr = self.unp_ptr;
        let mut dest_ptr = ptr.wrapping_sub(distance);
        let mut length = length;
        let window = self.window()?;
        if (0..MAX_WIN_SIZE - 300).contains(&dest_ptr) && ptr < MAX_WIN_SIZE - 300 {
            if dest_ptr + length <= ptr {
                let count = usize::try_from(length).map_err(|_| OUT_OF_BOUNDS)?;
                let (from, to) = (dest_ptr as usize, ptr as usize);
                if to + count > window.len() {
                    return Err(OUT_OF_BOUNDS);
                }
                window.copy_within(from..from + count, to);
                ptr += length;
            } else {
                for _ in 0..2 {
                    window[ptr as usize] = window[dest_ptr as usize];
                    ptr += 1;
                    dest_ptr += 1;
                }
                while length > 2 {
                    length -= 1;
                    let byte = *window.get(dest_ptr as usize).ok_or(OUT_OF_BOUNDS)?;
                    *window.get_mut(ptr as usize).ok_or(OUT_OF_BOUNDS)? = byte;
                    ptr += 1;
                    dest_ptr += 1;
                }
            }
        } else if distance > ptr && (!first_win_done || distance > MAX_WIN_SIZE) {
            while length != 0 {
                length = length.wrapping_sub(1);
                *window.get_mut(ptr as usize).ok_or(OUT_OF_BOUNDS)? = 0;
                ptr = (ptr + 1) & MAX_WIN_MASK;
            }
        } else {
            while length != 0 {
                length = length.wrapping_sub(1);
                let byte = window[(dest_ptr & MAX_WIN_MASK) as usize];
                *window.get_mut(ptr as usize).ok_or(OUT_OF_BOUNDS)? = byte;
                dest_ptr = dest_ptr.wrapping_add(1);
                ptr = (ptr + 1) & MAX_WIN_MASK;
            }
        }
        self.unp_ptr = ptr;
        Ok(())
    }

    fn read_tables20(&mut self) -> Result<bool, Thrown> {
        let mut bit_length = [0u8; BC20];
        let mut table = [0u8; MC20 * 4];
        if self.input.bits.in_addr > self.input.read_top - 25 && !self.input.read_buf()? {
            return Ok(false);
        }
        let bit_field = self.getbits()?;
        self.unp_audio_block = bit_field & 0x8000;
        if bit_field & 0x4000 == 0 {
            self.unp_old_table20 = [0; MC20 * 4];
        }
        self.addbits(2);
        let table_size = if self.unp_audio_block != 0 {
            self.unp_channels = ((bit_field >> 12) & 3) + 1;
            if self.unp_cur_channel >= self.unp_channels {
                self.unp_cur_channel = 0;
            }
            self.addbits(2);
            MC20 * self.unp_channels as usize
        } else {
            NC20 + DC20 + RC20
        };
        for length in &mut bit_length {
            *length = (self.getbits()? >> 12) as u8;
            self.addbits(4);
        }
        self.bd.make(&bit_length, BC20)?;
        let mut i = 0;
        while i < table_size {
            if self.input.bits.in_addr > self.input.read_top - 5 && !self.input.read_buf()? {
                return Ok(false);
            }
            let number = self.bd.decode(&mut self.input.bits)?;
            if number < 16 {
                table[i] = ((number + i32::from(self.unp_old_table20[i])) & 0xf) as u8;
                i += 1;
            } else if number == 16 {
                let mut n = (self.getbits()? >> 14) + 3;
                self.addbits(2);
                while n > 0 && i < table_size {
                    n -= 1;
                    table[i] = *table.get(i.wrapping_sub(1)).ok_or(OUT_OF_BOUNDS)?;
                    i += 1;
                }
            } else {
                let mut n = if number == 17 {
                    let n = (self.getbits()? >> 13) + 3;
                    self.addbits(3);
                    n
                } else {
                    let n = (self.getbits()? >> 9) + 11;
                    self.addbits(7);
                    n
                };
                while n > 0 && i < table_size {
                    n -= 1;
                    table[i] = 0;
                    i += 1;
                }
            }
        }
        if self.input.bits.in_addr > self.input.read_top {
            return Ok(true);
        }
        if self.unp_audio_block != 0 {
            for channel in 0..self.unp_channels as usize {
                self.md[channel]
                    .as_mut()
                    .ok_or(NULL)?
                    .make(&table[channel * MC20..], MC20)?;
            }
        } else {
            self.ld.make(&table, NC20)?;
            self.dd.make(&table[NC20..], DC20)?;
            self.rd.make(&table[NC20 + DC20..], RC20)?;
        }
        self.unp_old_table20 = table;
        Ok(true)
    }

    pub(super) fn unp_init_data20(&mut self, solid: bool) {
        if !solid {
            self.unp_channel_delta = 0;
            self.unp_cur_channel = 0;
            self.unp_channels = 1;
            self.aud_v = [Some(AudioVariables::default()); 4];
            self.unp_old_table20 = [0; MC20 * 4];
            for decode in &mut self.md {
                match decode {
                    Some(decode) => decode.reset(),
                    None => *decode = Some(Decode::new(MC20)),
                }
            }
        }
    }

    fn read_last_tables(&mut self) -> Result<(), Thrown> {
        if self.input.read_top < self.input.bits.in_addr + 5 {
            return Ok(());
        }
        if self.unp_audio_block != 0 {
            if channel_decode(&self.md, self.unp_cur_channel)?.decode(&mut self.input.bits)? == 256
            {
                self.read_tables20()?;
            }
        } else if self.ld.decode(&mut self.input.bits)? == 269 {
            self.read_tables20()?;
        }
        Ok(())
    }

    fn decode_audio(&mut self, delta: i32) -> Result<u8, Thrown> {
        let channel_delta = self.unp_channel_delta;
        let v = self
            .aud_v
            .get_mut(self.unp_cur_channel as usize)
            .ok_or(OUT_OF_BOUNDS)?
            .as_mut()
            .ok_or(NULL)?;
        v.byte_count = v.byte_count.wrapping_add(1);
        v.d4 = v.d3;
        v.d3 = v.d2;
        v.d2 = v.last_delta.wrapping_sub(v.d1);
        v.d1 = v.last_delta;
        let mut pch = v
            .last_char
            .wrapping_mul(8)
            .wrapping_add(v.k1.wrapping_mul(v.d1))
            .wrapping_add(v.k2.wrapping_mul(v.d2))
            .wrapping_add(v.k3.wrapping_mul(v.d3))
            .wrapping_add(v.k4.wrapping_mul(v.d4))
            .wrapping_add(v.k5.wrapping_mul(channel_delta));
        pch = (pch as u32 >> 3) as i32 & 0xff;
        let ch = pch.wrapping_sub(delta);
        let d = i32::from(delta as i8) << 3;
        for (dif, value) in v.dif.iter_mut().zip([
            d,
            d.wrapping_sub(v.d1),
            d.wrapping_add(v.d1),
            d.wrapping_sub(v.d2),
            d.wrapping_add(v.d2),
            d.wrapping_sub(v.d3),
            d.wrapping_add(v.d3),
            d.wrapping_sub(v.d4),
            d.wrapping_add(v.d4),
            d.wrapping_sub(channel_delta),
            d.wrapping_add(channel_delta),
        ]) {
            *dif = dif.wrapping_add(value.wrapping_abs());
        }
        v.last_delta = i32::from(ch.wrapping_sub(v.last_char) as i8);
        self.unp_channel_delta = v.last_delta;
        v.last_char = ch;
        if v.byte_count & 0x1f == 0 {
            let num_min_dif = super::least_dif(&mut v.dif);
            match num_min_dif {
                1 if v.k1 >= -16 => v.k1 -= 1,
                2 if v.k1 < 16 => v.k1 += 1,
                3 if v.k2 >= -16 => v.k2 -= 1,
                4 if v.k2 < 16 => v.k2 += 1,
                5 if v.k3 >= -16 => v.k3 -= 1,
                6 if v.k3 < 16 => v.k3 += 1,
                7 if v.k4 >= -16 => v.k4 -= 1,
                8 if v.k4 < 16 => v.k4 += 1,
                9 if v.k5 >= -16 => v.k5 -= 1,
                10 if v.k5 < 16 => v.k5 += 1,
                _ => {}
            }
        }
        Ok(ch as u8)
    }
}
