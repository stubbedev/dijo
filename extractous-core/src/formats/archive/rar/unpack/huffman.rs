//! junrar's Huffman decoding tables (`Decode` and its subclasses) and `Unpack20`'s
//! `makeDecodeTables` and `decodeNumber`, which every unpacker but RAR 1.5's reads codes with.

use super::bits::BitInput;
use super::{Thrown, NC, NC20, OUT_OF_BOUNDS};

/// `Compress.MAX_QUICK_DECODE_BITS`.
const MAX_QUICK_DECODE_BITS: i32 = 9;
const QUICK_SIZE: usize = 1 << MAX_QUICK_DECODE_BITS;

/// A decoding table of an alphabet of as many symbols as its `decode_num` holds.
#[derive(Clone)]
pub(super) struct Decode {
    max_num: i32,
    decode_len: [i32; 16],
    decode_pos: [i32; 16],
    decode_num: Vec<i32>,
    quick_bits: i32,
    quick_len: [i32; QUICK_SIZE],
    quick_num: [i32; QUICK_SIZE],
}

impl Decode {
    /// An empty table for an alphabet of `symbols` symbols.
    pub(super) fn new(symbols: usize) -> Self {
        Decode {
            max_num: 0,
            decode_len: [0; 16],
            decode_pos: [0; 16],
            decode_num: vec![0; symbols],
            quick_bits: 0,
            quick_len: [0; QUICK_SIZE],
            quick_num: [0; QUICK_SIZE],
        }
    }

    /// `reset`: empty again.
    pub(super) fn reset(&mut self) {
        let symbols = self.decode_num.len();
        *self = Decode::new(symbols);
    }

    /// `makeDecodeTables`: the table of the code whose lengths `lengths` holds, `size` of them.
    pub(super) fn make(&mut self, lengths: &[u8], size: usize) -> Result<(), Thrown> {
        let lengths = lengths.get(..size).ok_or(OUT_OF_BOUNDS)?;
        self.max_num = size as i32;
        let mut len_count = [0i32; 16];
        for &length in lengths {
            len_count[usize::from(length & 0xF)] += 1;
        }
        len_count[0] = 0;
        self.decode_num
            .get_mut(..size)
            .ok_or(OUT_OF_BOUNDS)?
            .fill(0);
        self.decode_pos[0] = 0;
        self.decode_len[0] = 0;
        let mut upper_limit = 0i32;
        for i in 1..16 {
            upper_limit += len_count[i];
            self.decode_len[i] = upper_limit << (16 - i);
            upper_limit *= 2;
            self.decode_pos[i] = self.decode_pos[i - 1] + len_count[i - 1];
        }
        let mut next_pos = self.decode_pos;
        for (symbol, &length) in lengths.iter().enumerate() {
            let length = usize::from(length & 0xF);
            if length != 0 {
                self.decode_num[next_pos[length] as usize] = symbol as i32;
                next_pos[length] += 1;
            }
        }
        self.quick_bits = if size == NC || size == NC20 {
            MAX_QUICK_DECODE_BITS
        } else {
            MAX_QUICK_DECODE_BITS - 3
        };
        let mut cur_bit_length = 1usize;
        for code in 0..1usize << self.quick_bits {
            let bit_field = (code as i32) << (16 - self.quick_bits);
            while cur_bit_length < 16 && bit_field >= self.decode_len[cur_bit_length] {
                cur_bit_length += 1;
            }
            self.quick_len[code] = cur_bit_length as i32;
            let dist = ((bit_field - self.decode_len[cur_bit_length - 1]) as u32
                >> (16 - cur_bit_length)) as i32;
            self.quick_num[code] = match self.decode_pos.get(cur_bit_length) {
                Some(&pos) if pos + dist < size as i32 => self.decode_num[(pos + dist) as usize],
                _ => 0,
            };
        }
        Ok(())
    }

    /// `decodeNumber`: the next symbol `bits` holds.
    pub(super) fn decode(&self, bits: &mut BitInput) -> Result<i32, Thrown> {
        let bit_field = bits.getbits()? & 0xfffe;
        let quick_bits = self.quick_bits as usize;
        if bit_field < self.decode_len[quick_bits] {
            let code = (bit_field as u32 >> (16 - quick_bits)) as usize;
            bits.addbits(self.quick_len[code]);
            return Ok(self.quick_num[code]);
        }
        let length = (quick_bits + 1..15)
            .find(|&i| bit_field < self.decode_len[i])
            .unwrap_or(15);
        bits.addbits(length as i32);
        let dist = ((bit_field - self.decode_len[length - 1]) as u32 >> (16 - length)) as i32;
        let mut pos = self.decode_pos[length] + dist;
        if pos >= self.max_num {
            pos = 0;
        }
        usize::try_from(pos)
            .ok()
            .and_then(|pos| self.decode_num.get(pos))
            .copied()
            .ok_or(OUT_OF_BOUNDS)
    }
}
