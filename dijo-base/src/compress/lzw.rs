//! Unix compress (`.Z`) files as commons-compress's `ZCompressorInputStream` reads them, with
//! the memory limit Tika's `CompressorParser` sets: LZW codes of growing width read from a
//! little-endian bit stream, realigned to a multiple of eight codes whenever the width changes,
//! the stream ending at the first code the input cannot fill.

use super::{throws, Decoding, Limited, Undecoded, CHUNK, MEMORY_LIMIT_KIB};
use crate::input::Input;

const MAGIC: [u8; 2] = [0x1F, 0x9D];
const BLOCK_MODE: u8 = 0x80;
const MAX_CODE_SIZE_MASK: u8 = 0x1F;
const DEFAULT_CODE_SIZE: u32 = 9;
const UNUSED_PREFIX: i32 = -1;

/// `BitInputStream` in little-endian order: bytes taken one at a time into a cache of bits.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    cached: u64,
    cached_size: u32,
}

impl Bits<'_> {
    /// `readBits`: the next `count` bits, or `None` once the input cannot fill them.
    fn read(&mut self, count: u32) -> Option<u32> {
        while self.cached_size < count {
            let &byte = self.data.get(self.at)?;
            self.at += 1;
            self.cached |= u64::from(byte) << self.cached_size;
            self.cached_size += 8;
        }
        let bits = self.cached & ((1 << count) - 1);
        self.cached >>= count;
        self.cached_size -= count;
        Some(u32::try_from(bits).expect("at most 31 bits"))
    }

    fn clear_cache(&mut self) {
        self.cached = 0;
        self.cached_size = 0;
    }
}

/// The state of `LZWInputStream` and `ZCompressorInputStream`.
struct Decoder<'a> {
    bits: Bits<'a>,
    block_mode: bool,
    max_code_size: u32,
    code_size: u32,
    total_codes_read: u64,
    previous_code: i32,
    previous_first: u8,
    table_size: usize,
    prefixes: Vec<i32>,
    characters: Vec<u8>,
}

impl Decoder<'_> {
    fn clear_code(&self) -> Option<u32> {
        self.block_mode.then_some(1 << (DEFAULT_CODE_SIZE - 1))
    }

    fn read_code(&mut self) -> Option<u32> {
        let code = self.bits.read(self.code_size);
        if code.is_some() {
            self.total_codes_read += 1;
        }
        code
    }

    /// `reAlignReading`: the codes up to the next multiple of eight read and dropped, then the
    /// bits left of the last byte.
    fn realign(&mut self) {
        for _ in 0..(8 - self.total_codes_read % 8) % 8 {
            self.read_code();
        }
        self.bits.clear_cache();
    }

    fn clear_entries(&mut self) {
        self.table_size = 256 + usize::from(self.block_mode);
    }

    /// `addEntry`, then the wider codes once the table fills the current width.
    fn add_entry(&mut self, prefix: i32, character: u8) {
        let max_table_size = 1usize << self.code_size;
        if self.table_size < max_table_size {
            if self.table_size == self.prefixes.len() {
                self.prefixes.push(prefix);
                self.characters.push(character);
            } else {
                self.prefixes[self.table_size] = prefix;
                self.characters[self.table_size] = character;
            }
            self.table_size += 1;
        }
        if self.table_size == max_table_size && self.code_size < self.max_code_size {
            self.realign();
            self.code_size += 1;
        }
    }

    /// `decompressNextSymbol` with `expandCodeToOutputStack`: the next code's bytes appended to
    /// `out`, `false` at the end of the stream.
    fn next_symbol(&mut self, out: &mut Vec<u8>) -> Result<bool, Undecoded> {
        let Some(code) = self.read_code() else {
            return Ok(false);
        };
        if Some(code) == self.clear_code() {
            self.clear_entries();
            self.realign();
            self.code_size = DEFAULT_CODE_SIZE;
            self.previous_code = UNUSED_PREFIX;
            return Ok(true);
        }
        let code = usize::try_from(code).expect("a code fits in usize");
        let mut added_unfinished = false;
        if code == self.table_size {
            if self.previous_code == UNUSED_PREFIX {
                return Err(throws(
                    "The first code can't be a reference to its preceding code",
                ));
            }
            self.add_entry(self.previous_code, self.previous_first);
            added_unfinished = true;
        } else if code > self.table_size {
            return Err(throws(format!(
                "Invalid {} bit code 0x{code:x}",
                self.code_size
            )));
        }
        let start = out.len();
        let mut entry = i32::try_from(code).expect("a code fits in an int");
        while entry >= 0 {
            let index = usize::try_from(entry).expect("a table index");
            out.push(self.characters[index]);
            entry = self.prefixes[index];
        }
        out[start..].reverse();
        let first = out[start];
        if self.previous_code != UNUSED_PREFIX && !added_unfinished {
            self.add_entry(self.previous_code, first);
        }
        self.previous_code = i32::try_from(code).expect("a code fits in an int");
        self.previous_first = first;
        Ok(true)
    }
}

/// The content of the `.Z` file `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    let (Some(magic), Some(&flags)) = (data.get(..2), data.get(2)) else {
        return Err(throws("Input is not in .Z format"));
    };
    if magic != MAGIC {
        return Err(throws("Input is not in .Z format"));
    }
    let max_code_size = u32::from(flags & MAX_CODE_SIZE_MASK);
    if max_code_size < DEFAULT_CODE_SIZE {
        return Err(throws(format!(
            "maxCodeSize {max_code_size} is out of bounds."
        )));
    }
    let memory_kib = ((1u64 << max_code_size) * 6) >> 10;
    if max_code_size == 31 || memory_kib > u64::from(MEMORY_LIMIT_KIB) {
        return Err(Undecoded::MemoryLimit(format!(
            "{memory_kib} KiB of memory would be needed, beyond the limit of {MEMORY_LIMIT_KIB} KiB"
        )));
    }
    let mut decoder = Decoder {
        bits: Bits {
            data,
            at: 3,
            cached: 0,
            cached_size: 0,
        },
        block_mode: flags & BLOCK_MODE != 0,
        max_code_size,
        code_size: DEFAULT_CODE_SIZE,
        total_codes_read: 0,
        previous_code: UNUSED_PREFIX,
        previous_first: 0,
        table_size: 0,
        prefixes: (0..=256)
            .map(|i| if i < 256 { UNUSED_PREFIX } else { 0 })
            .collect(),
        characters: (0..=256u16).map(|i| i as u8).collect(),
    };
    decoder.clear_entries();
    let mut decoding = Decoding::spooled(data.len(), None);
    let mut out = Vec::with_capacity(2 * CHUNK);
    while decoder.next_symbol(&mut out)? {
        if out.len() >= CHUNK {
            decoding.write(&out)?;
            if decoding.is_cut() {
                return decoding.finish();
            }
            out.clear();
        }
    }
    decoding.write(&out)?;
    decoding.finish()
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::collections::HashMap;

    #[cfg(test)]
    use super::*;

    /// `content` compressed as `compress` does while every code fits in nine bits.
    pub fn encode(content: &[u8]) -> Vec<u8> {
        let mut table: HashMap<Vec<u8>, u16> =
            (0..=255u8).map(|b| (vec![b], u16::from(b))).collect();
        let mut next = 257;
        let mut codes = Vec::new();
        let mut current: Vec<u8> = Vec::new();
        for &byte in content {
            let mut extended = current.clone();
            extended.push(byte);
            if table.contains_key(&extended) {
                current = extended;
                continue;
            }
            codes.push(table[&current]);
            table.insert(extended, next);
            next += 1;
            assert!(next < 511, "the content needs codes wider than nine bits");
            current = vec![byte];
        }
        if !current.is_empty() {
            codes.push(table[&current]);
        }
        compressed(&codes)
    }

    /// A `.Z` file of 9-bit codes in block mode, which `compress` writes for short inputs.
    fn compressed(codes: &[u16]) -> Vec<u8> {
        let mut data = vec![0x1F, 0x9D, 0x90];
        let (mut cache, mut size) = (0u32, 0);
        for &code in codes {
            cache |= u32::from(code) << size;
            size += 9;
            while size >= 8 {
                data.push(cache as u8);
                cache >>= 8;
                size -= 8;
            }
        }
        if size > 0 {
            data.push(cache as u8);
        }
        data
    }

    #[test]
    fn decodes_codes_and_their_repeats() {
        let data = compressed(&[b'a'.into(), b'b'.into(), 257, 259]);
        assert_eq!(decode(&data).and_then(Limited::whole).unwrap(), b"abababa");
    }

    #[test]
    fn decodes_what_compress_encodes() {
        let text = b"TOBEORNOTTOBEORTOBEORNOT#".repeat(4);
        assert_eq!(
            decode(&encode(&text)).and_then(Limited::whole).unwrap(),
            text
        );
    }

    #[test]
    fn fails_where_commons_compress_fails() {
        assert!(decode(&compressed(&[300])).is_err());
        assert!(matches!(
            decode(&[0x1F, 0x9D, 0x99]),
            Err(Undecoded::MemoryLimit(_))
        ));
        assert!(decode(&[0x1F, 0x9D, 0x88]).is_err());
        assert!(decode(b"\x1F\x8B\x90").is_err());
    }
}
