//! LZFSE streams, as Apple's reference decoder, lzfse 1.0 (`lzfse_decode_base.c`,
//! `lzfse_fse.c`), decodes them, whole and into memory: a sequence of blocks up to the
//! end-of-stream block (`bvx$`), each uncompressed (`bvx-`), coded with LZVN (`bvxn`, [`lzvn`])
//! or coded with LZFSE, its header uncompressed (`bvx1`) or compressed (`bvx2`). An LZFSE block
//! holds its literals and its literal lengths, match lengths and match distances as streams of
//! finite-state entropy codes, each read backwards from its end; a distance of 0 repeats the one
//! before. Matches reach back into all the stream has decoded into, the blocks before included.
//!
//! Where the reference decoder lets a literal run on into the stale bytes of its literal buffer,
//! this one fails the stream.

use super::lzvn::{self, copy_match};

const L_SYMBOLS: usize = 20;
const M_SYMBOLS: usize = 20;
const D_SYMBOLS: usize = 64;
const LITERAL_SYMBOLS: usize = 256;
const L_STATES: usize = 64;
const M_STATES: usize = 64;
const D_STATES: usize = 256;
const LITERAL_STATES: usize = 1024;
const MATCHES_PER_BLOCK: u32 = 10_000;
const LITERALS_PER_BLOCK: u32 = 4 * MATCHES_PER_BLOCK;
/// The bytes of a `bvx1` header, the C structure with its padding.
const V1_HEADER: usize = 772;
/// The bytes of a `bvx2` header before its frequency tables.
const V2_FIXED: usize = 32;

const L_EXTRA_BITS: [u8; L_SYMBOLS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 5, 8];
const L_BASE: [i32; L_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 20, 28, 60,
];
const M_EXTRA_BITS: [u8; M_SYMBOLS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 5, 8, 11];
const M_BASE: [i32; M_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 24, 56, 312,
];
const D_EXTRA_BITS: [u8; D_SYMBOLS] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7,
    8, 8, 8, 8, 9, 9, 9, 9, 10, 10, 10, 10, 11, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14,
    14, 14, 15, 15, 15, 15,
];
const D_BASE: [i32; D_SYMBOLS] = [
    0, 1, 2, 3, 4, 6, 8, 10, 12, 16, 20, 24, 28, 36, 44, 52, 60, 76, 92, 108, 124, 156, 188, 220,
    252, 316, 380, 444, 508, 636, 764, 892, 1020, 1276, 1532, 1788, 2044, 2556, 3068, 3580, 4092,
    5116, 6140, 7164, 8188, 10236, 12284, 14332, 16380, 20476, 24572, 28668, 32764, 40956, 49148,
    57340, 65532, 81916, 98300, 114684, 131068, 163836, 196604, 229372,
];

/// The bits of a `bvx2` header's frequency code, by its low five bits: how many it takes.
const FREQ_BITS: [u8; 32] = [
    2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14, 2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2,
    14,
];
/// The frequency a code of at most five bits stands for, by its low five bits.
const FREQ_VALUES: [u16; 32] = [
    0, 2, 1, 4, 0, 3, 1, 0, 0, 2, 1, 5, 0, 3, 1, 0, 0, 2, 1, 6, 0, 3, 1, 0, 0, 2, 1, 7, 0, 3, 1, 0,
];

/// Decodes the LZFSE stream `stream` into `out`, which it empties first, failing where the
/// stream is malformed or decodes into more than `limit` bytes.
pub(super) fn decode(stream: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<(), String> {
    out.clear();
    let mut at = 0;
    loop {
        let magic = stream.get(at..at + 4).ok_or_else(|| {
            format!("the LZFSE stream ends at byte {at}, before its end-of-stream block")
        })?;
        let header = move |length: usize| {
            stream.get(at..at + length).ok_or_else(|| {
                format!("the LZFSE block header at byte {at} runs past the stream's end")
            })
        };
        let word = |bytes: &[u8], offset: usize| {
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
        };
        at = match magic {
            b"bvx$" => return Ok(()),
            b"bvx-" => {
                let raw = word(header(8)?, 4) as usize;
                fits(out, raw, limit, at)?;
                let data = stream.get(at + 8..at + 8 + raw).ok_or_else(|| {
                    format!("the uncompressed LZFSE block at byte {at} runs past the stream's end")
                })?;
                out.extend_from_slice(data);
                at + 8 + raw
            }
            b"bvxn" => {
                let fields = header(12)?;
                let (raw, payload) = (word(fields, 4) as usize, word(fields, 8) as usize);
                fits(out, raw, limit, at)?;
                let block = stream.get(at + 12..at + 12 + payload).ok_or_else(|| {
                    format!("the LZVN block at byte {at} runs past the stream's end")
                })?;
                lzvn::decode(block, raw, out)
                    .map_err(|reason| format!("the block at byte {at}: {reason}"))?;
                at + 12 + payload
            }
            b"bvx1" | b"bvx2" => {
                let (block, size) = if magic == b"bvx1" {
                    (Block::v1(header(V1_HEADER)?), V1_HEADER)
                } else {
                    let fixed = header(V2_FIXED)?;
                    let size = word(fixed, 24) as usize;
                    (Block::v2(header(size.max(V2_FIXED))?, size), size)
                };
                let block = block
                    .and_then(Block::checked)
                    .map_err(|reason| format!("the LZFSE block at byte {at}: {reason}"))?;
                let end = at + size + block.literal_payload + block.lmd_payload;
                if end > stream.len() {
                    return Err(format!(
                        "the LZFSE block at byte {at} runs past the stream's end"
                    ));
                }
                block
                    .decode(&stream[..end], at + size, out, limit)
                    .map_err(|reason| format!("the LZFSE block at byte {at}: {reason}"))?;
                end
            }
            _ => {
                return Err(format!(
                    "the LZFSE stream has no block at byte {at}: its magic is {magic:02x?}"
                ))
            }
        };
    }
}

/// Whether `raw` bytes more fit in `out` within `limit` bytes, for the block at `at`.
fn fits(out: &[u8], raw: usize, limit: usize, at: usize) -> Result<(), String> {
    if out.len().saturating_add(raw) > limit {
        return Err(format!(
            "the LZFSE block at byte {at} decodes past the {limit} bytes expected"
        ));
    }
    Ok(())
}

/// The header of an LZFSE block, as `lzfse_compressed_block_header_v1` holds it.
struct Block {
    literals: u32,
    matches: u32,
    literal_payload: usize,
    lmd_payload: usize,
    literal_bits: i32,
    literal_states: [u16; 4],
    lmd_bits: i32,
    l_state: u16,
    m_state: u16,
    d_state: u16,
    /// The frequencies of the L, M, D and literal symbols, in that order.
    freqs: [u16; L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LITERAL_SYMBOLS],
}

impl Block {
    /// The `bvx1` header `header`.
    fn v1(header: &[u8]) -> Result<Block, String> {
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("four"));
        let half = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
        let mut freqs = [0; L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LITERAL_SYMBOLS];
        for (index, freq) in freqs.iter_mut().enumerate() {
            *freq = half(50 + 2 * index);
        }
        Ok(Block {
            literals: word(12),
            matches: word(16),
            literal_payload: word(20) as usize,
            lmd_payload: word(24) as usize,
            literal_bits: word(28) as i32,
            literal_states: [half(32), half(34), half(36), half(38)],
            lmd_bits: word(40) as i32,
            l_state: half(44),
            m_state: half(46),
            d_state: half(48),
            freqs,
        })
    }

    /// The `bvx2` header `header`, `size` bytes long, its fields unpacked and its frequency
    /// tables decoded as `lzfse_decode_v1` decodes them.
    fn v2(header: &[u8], size: usize) -> Result<Block, String> {
        if size < V2_FIXED {
            return Err(format!(
                "its header is {size} bytes long, shorter than its fixed fields"
            ));
        }
        let packed = |index: usize| {
            u64::from_le_bytes(
                header[8 + 8 * index..16 + 8 * index]
                    .try_into()
                    .expect("eight"),
            )
        };
        let (v0, v1, v2) = (packed(0), packed(1), packed(2));
        let field =
            |value: u64, offset: u32, bits: u32| (value >> offset & ((1 << bits) - 1)) as u32;
        let mut freqs = [0; L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LITERAL_SYMBOLS];
        if size > V2_FIXED {
            let mut accum: u32 = 0;
            let mut accum_bits = 0;
            let mut at = V2_FIXED;
            for freq in freqs.iter_mut() {
                while at < size && accum_bits + 8 <= 32 {
                    accum |= u32::from(header[at]) << accum_bits;
                    accum_bits += 8;
                    at += 1;
                }
                let code = (accum & 31) as usize;
                let bits = u32::from(FREQ_BITS[code]);
                *freq = match bits {
                    8 => 8 + (accum >> 4 & 0xF) as u16,
                    14 => 24 + (accum >> 4 & 0x3FF) as u16,
                    _ => FREQ_VALUES[code],
                };
                if bits > accum_bits {
                    return Err("its frequency tables run past its header".into());
                }
                accum >>= bits;
                accum_bits -= bits;
            }
            if accum_bits >= 8 || at != size {
                return Err("its frequency tables end before its header does".into());
            }
        }
        Ok(Block {
            literals: field(v0, 0, 20),
            literal_payload: field(v0, 20, 20) as usize,
            matches: field(v0, 40, 20),
            literal_bits: field(v0, 60, 3) as i32 - 7,
            literal_states: [0, 10, 20, 30].map(|offset| field(v1, offset, 10) as u16),
            lmd_payload: field(v1, 40, 20) as usize,
            lmd_bits: field(v1, 60, 3) as i32 - 7,
            l_state: field(v2, 32, 10) as u16,
            m_state: field(v2, 42, 10) as u16,
            d_state: field(v2, 52, 10) as u16,
            freqs,
        })
    }

    /// The block, if its fields are within the bounds `lzfse_check_block_header_v1` sets.
    fn checked(self) -> Result<Block, String> {
        let (l, rest) = self.freqs.split_at(L_SYMBOLS);
        let (m, rest) = rest.split_at(M_SYMBOLS);
        let (d, literal) = rest.split_at(D_SYMBOLS);
        let within = |freqs: &[u16], states: usize| {
            freqs.iter().map(|&freq| usize::from(freq)).sum::<usize>() <= states
        };
        if self.literals > LITERALS_PER_BLOCK
            || self.matches > MATCHES_PER_BLOCK
            || self
                .literal_states
                .iter()
                .any(|&state| usize::from(state) >= LITERAL_STATES)
            || usize::from(self.l_state) >= L_STATES
            || usize::from(self.m_state) >= M_STATES
            || usize::from(self.d_state) >= D_STATES
            || !within(l, L_STATES)
            || !within(m, M_STATES)
            || !within(d, D_STATES)
            || !within(literal, LITERAL_STATES)
        {
            return Err("its header's counts, states or frequencies are out of bounds".into());
        }
        Ok(self)
    }

    /// Decodes the block, whose payload starts at `payload` of `stream`, which ends with it,
    /// into `out`, up to `limit` bytes.
    fn decode(
        &self,
        stream: &[u8],
        payload: usize,
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<(), String> {
        let (l, rest) = self.freqs.split_at(L_SYMBOLS);
        let (m, rest) = rest.split_at(M_SYMBOLS);
        let (d, literal) = rest.split_at(D_SYMBOLS);
        let literal_table = symbol_table::<LITERAL_STATES>(literal);
        let mut literals = vec![0u8; (self.literals as usize).next_multiple_of(4)];
        let mut at = payload + self.literal_payload;
        let mut bits = Bits::new(stream, &mut at, 0, self.literal_bits)?;
        let mut states = self.literal_states;
        for quad in literals.as_chunks_mut::<4>().0 {
            bits.flush(stream, &mut at, 0)?;
            for (literal, state) in quad.iter_mut().zip(&mut states) {
                *literal = literal_table[usize::from(*state)].decode(state, &mut bits);
            }
        }

        let l_table = value_table::<L_STATES>(l, &L_EXTRA_BITS, &L_BASE);
        let m_table = value_table::<M_STATES>(m, &M_EXTRA_BITS, &M_BASE);
        let d_table = value_table::<D_STATES>(d, &D_EXTRA_BITS, &D_BASE);
        let lmd = payload + self.literal_payload;
        let mut at = lmd + self.lmd_payload;
        let mut bits = Bits::new(stream, &mut at, lmd, self.lmd_bits)?;
        let (mut l_state, mut m_state, mut d_state) = (self.l_state, self.m_state, self.d_state);
        let mut distance: i64 = -1;
        let mut next_literal = 0;
        for _ in 0..self.matches {
            bits.flush(stream, &mut at, 0)?;
            let literal = l_table[usize::from(l_state)].decode(&mut l_state, &mut bits) as usize;
            let matched = m_table[usize::from(m_state)].decode(&mut m_state, &mut bits) as usize;
            let new = d_table[usize::from(d_state)].decode(&mut d_state, &mut bits);
            if new != 0 {
                distance = i64::from(new);
            }
            let run = literals
                .get(next_literal..next_literal + literal)
                .ok_or("a literal runs past the block's literals")?;
            if out.len() + literal + matched > limit {
                return Err(format!("it decodes past the {limit} bytes expected"));
            }
            out.extend_from_slice(run);
            next_literal += literal;
            let distance = usize::try_from(distance)
                .map_err(|_| "its first match repeats a distance before any is given")?;
            copy_match(out, distance, matched)?;
        }
        Ok(())
    }
}

/// An input stream of finite-state entropy codes, read backwards from its end, its bits in a
/// 64-bit accumulator, as `fse_in_stream64` reads them.
struct Bits {
    accum: u64,
    count: u32,
}

impl Bits {
    /// The stream ending at `*at` of `stream`, its last byte holding `last_bits` (from -7 to 0)
    /// bits fewer than eight, and moves `*at` back past the bytes it reads; it may read no
    /// further back than `start`.
    fn new(stream: &[u8], at: &mut usize, start: usize, last_bits: i32) -> Result<Bits, String> {
        if !(-7..=0).contains(&last_bits) {
            return Err(format!(
                "its bit stream ends in a byte of {last_bits} bits fewer than eight"
            ));
        }
        let (bytes, count) = if last_bits == 0 {
            (7, 56)
        } else {
            (8, (64 + last_bits) as u32)
        };
        if *at < start + bytes {
            return Err("its bit stream starts before its payload".into());
        }
        *at -= bytes;
        let mut word = [0; 8];
        word[..bytes].copy_from_slice(&stream[*at..*at + bytes]);
        let accum = u64::from_le_bytes(word);
        if accum >> count != 0 {
            return Err("its bit stream does not start as an encoder ends one".into());
        }
        Ok(Bits { accum, count })
    }

    /// Refills the accumulator to between 56 and 63 bits from the bytes before `*at`, reading no
    /// further back than `start`.
    fn flush(&mut self, stream: &[u8], at: &mut usize, start: usize) -> Result<(), String> {
        let bits = (63 - self.count) & !7;
        if bits == 0 {
            return Ok(());
        }
        let bytes = (bits / 8) as usize;
        if *at < start + bytes {
            return Err("its bit stream runs past its start".into());
        }
        *at -= bytes;
        let mut word = [0; 8];
        word[..bytes].copy_from_slice(&stream[*at..*at + bytes]);
        self.accum = self.accum << bits | u64::from_le_bytes(word);
        self.count += bits;
        Ok(())
    }

    /// The next `n` bits, at most the accumulator's.
    fn pull(&mut self, n: u32) -> u64 {
        self.count -= n;
        let pulled = self.accum >> self.count;
        self.accum &= (1u64 << self.count) - 1;
        pulled
    }
}

/// A state of a decoder of symbols: the bits it reads, the symbol it stands for and the base of
/// the next state.
#[derive(Clone, Copy, Default)]
struct SymbolEntry {
    bits: u8,
    symbol: u8,
    delta: u16,
}

impl SymbolEntry {
    fn decode(self, state: &mut u16, bits: &mut Bits) -> u8 {
        *state = self.delta + bits.pull(u32::from(self.bits)) as u16;
        self.symbol
    }
}

/// A state of a decoder of values: the bits it reads, state and extra bits together, its extra
/// bits, the base of the next state and the value's base.
#[derive(Clone, Copy, Default)]
struct ValueEntry {
    total_bits: u8,
    value_bits: u8,
    delta: u16,
    base: i32,
}

impl ValueEntry {
    fn decode(self, state: &mut u16, bits: &mut Bits) -> i32 {
        let pulled = bits.pull(u32::from(self.total_bits));
        *state = self.delta + (pulled >> self.value_bits) as u16;
        self.base + (pulled & ((1 << self.value_bits) - 1)) as i32
    }
}

/// The states of a symbol of frequency `freq` among `STATES`: for each, the bits it reads and
/// the base of the next state, as `fse_init_decoder_table` lays them out.
fn spread<const STATES: usize>(freq: u16) -> impl Iterator<Item = (u8, u16)> {
    let freq = u32::from(freq);
    let shift = freq.leading_zeros() - (STATES as u32).leading_zeros();
    let low = ((2 * STATES as u32) >> shift) - freq;
    (0..freq).map(move |j| {
        if j < low {
            (shift as u8, (((freq + j) << shift) - STATES as u32) as u16)
        } else {
            ((shift - 1) as u8, ((j - low) << (shift - 1)) as u16)
        }
    })
}

/// The decoder of symbols of frequencies `freqs`, whose sum is at most `STATES`.
fn symbol_table<const STATES: usize>(freqs: &[u16]) -> Box<[SymbolEntry; STATES]> {
    let mut table = Box::new([SymbolEntry::default(); STATES]);
    let mut slots = table.iter_mut();
    for (symbol, &freq) in freqs.iter().enumerate().filter(|(_, &freq)| freq > 0) {
        for ((bits, delta), slot) in spread::<STATES>(freq).zip(&mut slots) {
            *slot = SymbolEntry {
                bits,
                symbol: symbol as u8,
                delta,
            };
        }
    }
    table
}

/// The decoder of values of frequencies `freqs`, whose sum is at most `STATES`, each symbol
/// standing for its base plus the extra bits it reads.
fn value_table<const STATES: usize>(
    freqs: &[u16],
    extra_bits: &[u8],
    bases: &[i32],
) -> [ValueEntry; STATES] {
    let mut table = [ValueEntry::default(); STATES];
    let mut slots = table.iter_mut();
    for (symbol, &freq) in freqs.iter().enumerate().filter(|(_, &freq)| freq > 0) {
        for ((bits, delta), slot) in spread::<STATES>(freq).zip(&mut slots) {
            *slot = ValueEntry {
                total_bits: bits + extra_bits[symbol],
                value_bits: extra_bits[symbol],
                delta,
                base: bases[symbol],
            };
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("tests/fixtures/apple_disk_image/{name}.lzfse")).unwrap()
    }

    fn decoded(stream: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = vec![1, 2, 3];
        decode(stream, &mut out, 1 << 20).map(|()| out)
    }

    /// The streams `lzfse.py` writes with the reference encoder, each its length and CRC-32.
    #[test]
    fn decodes_what_the_reference_encoder_writes() {
        for (name, length, crc) in [
            ("lzvn", 2_000, 0x9f58_3dcb),
            ("raw", 64, 0x57c1_7ad7),
            ("blocks", 200_000, 0xd0ab_15c8),
            ("sectors", 32_768, 0xf282_caf4),
            ("v1", 200_000, 0xd0ab_15c8),
        ] {
            let out = decoded(&fixture(name)).unwrap_or_else(|reason| panic!("{name}: {reason}"));
            assert_eq!(
                (out.len(), crate::compress::crc32(&out)),
                (length, crc),
                "{name}"
            );
        }
    }

    #[test]
    fn fails_streams_cut_short_corrupt_or_past_the_limit() {
        let blocks = fixture("blocks");
        for cut in [3, 40, 1000, blocks.len() - 4] {
            assert!(decoded(&blocks[..cut]).is_err(), "cut at {cut}");
        }
        let mut corrupt = blocks.clone();
        corrupt[0] = b'X';
        assert!(decoded(&corrupt)
            .unwrap_err()
            .contains("has no block at byte 0"));
        let mut header = blocks.clone();
        header[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decoded(&header).is_err());
        let mut out = Vec::new();
        assert!(decode(&blocks, &mut out, 199_999)
            .unwrap_err()
            .contains("past the 199999 bytes"));
        let lzvn = fixture("lzvn");
        assert!(decode(&lzvn, &mut out, 1_999)
            .unwrap_err()
            .contains("past the 1999 bytes"));
    }

    /// Garbled payloads decode into garbage or fail, within the limit, never past it.
    #[test]
    fn keeps_garbled_payloads_within_the_limit() {
        let v1 = fixture("v1");
        for start in (V1_HEADER..v1.len() - 4).step_by(997) {
            let mut garbled = v1.clone();
            for byte in &mut garbled[start..start + 3] {
                *byte ^= 0x5A;
            }
            let mut out = Vec::new();
            if decode(&garbled, &mut out, 200_000).is_ok() {
                assert!(out.len() <= 200_000, "garbled at {start}");
            }
        }
    }

    #[test]
    fn refuses_frequencies_beyond_the_states() {
        let mut header = fixture("v1")[..V1_HEADER].to_vec();
        header[50..52].copy_from_slice(&65u16.to_le_bytes());
        let block = Block::v1(&header).unwrap();
        assert!(block.checked().is_err());
    }
}
