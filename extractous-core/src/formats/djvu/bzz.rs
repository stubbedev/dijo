//! BZZ, the general-purpose compression of DjVu files: blocks of a Burrows-Wheeler transform
//! whose move-to-front ranks are coded with the Z′ coder, as the DjVu v3 specification describes
//! them (Appendix 4); our Java `BzzDecoder`.

use super::zp::ZpDecoder;
use super::DjvuError;

/// The largest block the format allows.
const MAX_BLOCK_BYTES: usize = 4096 * 1024;
const CONTEXTS: usize = 260;

/// The bytes `compressed` holds, failing when they would exceed `max_bytes`.
pub(super) fn decode(compressed: &[u8], max_bytes: usize) -> Result<Vec<u8>, DjvuError> {
    let mut decoder = Decoder {
        zp: ZpDecoder::new(compressed),
        contexts: [0; CONTEXTS],
    };
    let mut out = Vec::new();
    while let Some(block) = decoder.block()? {
        if out.len() + block.len() > max_bytes {
            return Err(DjvuError::Malformed(format!(
                "BZZ data decompresses to more than {max_bytes} bytes"
            )));
        }
        out.extend_from_slice(&block);
    }
    Ok(out)
}

struct Decoder<'a> {
    zp: ZpDecoder<'a>,
    contexts: [u8; CONTEXTS],
}

impl Decoder<'_> {
    fn decode(&mut self, context: usize) -> u32 {
        self.zp.decode(&mut self.contexts, context)
    }

    fn decode_raw(&mut self, bits: u32) -> u32 {
        let mut n = 1;
        while n < 1 << bits {
            n = (n << 1) | self.zp.decode_pass_through();
        }
        n - (1 << bits)
    }

    fn decode_bin(&mut self, offset: usize, bits: u32) -> usize {
        let mut n = 1usize;
        while n < 1 << bits {
            n = (n << 1) | self.decode(offset + n - 1) as usize;
        }
        n - (1 << bits)
    }

    /// The bytes of the next block, or none at the end of the stream (13.2.1.2).
    fn block(&mut self) -> Result<Option<Vec<u8>>, DjvuError> {
        let block_size = self.decode_raw(24) as usize;
        if block_size == 0 {
            return Ok(None);
        }
        if block_size > MAX_BLOCK_BYTES {
            return Err(DjvuError::Malformed(format!(
                "A BZZ block of {block_size} bytes exceeds the {MAX_BLOCK_BYTES} the format allows"
            )));
        }
        let mut shift = 0;
        if self.zp.decode_pass_through() == 1 {
            shift = if self.zp.decode_pass_through() == 1 {
                2
            } else {
                1
            };
        }
        let mut mtf: [u8; 256] = std::array::from_fn(|i| i as u8);
        let mut frequencies = [0i32; 4];
        let mut data = vec![0u8; block_size];
        let mut marker = None;
        let mut add: i32 = 4;
        let mut rank: usize = 3;
        for (i, slot) in data.iter_mut().enumerate() {
            let context = rank.min(2);
            rank = if self.decode(context) == 1 {
                0
            } else if self.decode(context + 3) == 1 {
                1
            } else if self.decode(6) == 1 {
                2 + self.decode_bin(7, 1)
            } else if self.decode(8) == 1 {
                4 + self.decode_bin(9, 2)
            } else if self.decode(12) == 1 {
                8 + self.decode_bin(13, 3)
            } else if self.decode(20) == 1 {
                16 + self.decode_bin(21, 4)
            } else if self.decode(36) == 1 {
                32 + self.decode_bin(37, 5)
            } else if self.decode(68) == 1 {
                64 + self.decode_bin(69, 6)
            } else if self.decode(132) == 1 {
                128 + self.decode_bin(133, 7)
            } else {
                *slot = 0;
                marker = Some(i);
                rank = 256;
                continue;
            };
            let symbol = mtf[rank];
            *slot = symbol;
            add = add.wrapping_add(add >> shift);
            if add > 0x1000_0000 {
                add >>= 24;
                for frequency in &mut frequencies {
                    *frequency >>= 24;
                }
            }
            let frequency = if rank < 4 {
                add.wrapping_add(frequencies[rank])
            } else {
                add
            };
            let mut k = rank;
            while k > 3 {
                mtf[k] = mtf[k - 1];
                k -= 1;
            }
            while k > 0 && frequency >= frequencies[k - 1] {
                mtf[k] = mtf[k - 1];
                frequencies[k] = frequencies[k - 1];
                k -= 1;
            }
            mtf[k] = symbol;
            frequencies[k] = frequency;
        }
        let marker = marker
            .filter(|&marker| marker > 0 && marker < block_size)
            .ok_or_else(|| {
                DjvuError::Malformed("A BZZ block has no valid end-of-block marker".to_owned())
            })?;
        inverse_burrows_wheeler(&data, marker).map(Some)
    }
}

/// Follows the transform's thread backwards (13.2.1.3).
fn inverse_burrows_wheeler(data: &[u8], marker: usize) -> Result<Vec<u8>, DjvuError> {
    let block_size = data.len();
    let mut count = [0usize; 256];
    let mut ranks = vec![0usize; block_size];
    for (i, &symbol) in data.iter().enumerate() {
        if i != marker {
            ranks[i] = count[usize::from(symbol)];
            count[usize::from(symbol)] += 1;
        }
    }
    let mut last = 1;
    for occurrences in &mut count {
        let seen = *occurrences;
        *occurrences = last;
        last += seen;
    }
    if last != block_size {
        return Err(DjvuError::Malformed(
            "A BZZ block does not add up to its size".to_owned(),
        ));
    }
    let mut decoded = vec![0u8; block_size - 1];
    let mut k = 0;
    for position in (1..block_size).rev() {
        let symbol = data[k];
        decoded[position - 1] = symbol;
        k = count[usize::from(symbol)] + ranks[k];
        if k >= block_size {
            return Err(DjvuError::Malformed(
                "A BZZ block's transform leaves the block".to_owned(),
            ));
        }
    }
    if k != marker {
        return Err(DjvuError::Malformed(
            "A BZZ block's transform does not lead back to its marker".to_owned(),
        ));
    }
    Ok(decoded)
}
