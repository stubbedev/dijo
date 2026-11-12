//! The PKWARE Data Compression Library's `explode`, binary mode, as LibreOffice's
//! `Decompression` reads the `WordProData` stream of a small Word Pro file: a literal byte, or a
//! length and a distance back into what is decoded, each coded by a fixed Huffman tree, up to
//! the code of length 519 that ends the stream.

use super::Malformed;

/// The codes of the lengths 0 to 15 (`Tree1String`) and of the distances' high bits 0 to 63
/// (`Tree2String`), most significant bit first.
const LENGTH_CODES: [&str; 16] = [
    "101", "11", "100", "011", "0101", "0100", "0011", "00101", "00100", "00011", "00010",
    "000011", "000010", "000001", "0000001", "0000000",
];
const DISTANCE_CODES: [&str; 64] = [
    "11", "1011", "1010", "10011", "10010", "10001", "10000", "011111", "011110", "011101",
    "011100", "011011", "011010", "011001", "011000", "010111", "010110", "010101", "010100",
    "010011", "010010", "010001", "0100001", "0100000", "0011111", "0011110", "0011101", "0011100",
    "0011011", "0011010", "0011001", "0011000", "0010111", "0010110", "0010101", "0010100",
    "0010011", "0010010", "0010001", "0010000", "0001111", "0001110", "0001101", "0001100",
    "0001011", "0001010", "0001001", "0001000", "00001111", "00001110", "00001101", "00001100",
    "00001011", "00001010", "00001001", "00001000", "00000111", "00000110", "00000101", "00000100",
    "00000011", "00000010", "00000001", "00000000",
];
/// The longest code of either tree.
const LONGEST_CODE: u32 = 8;
/// The length that ends the stream.
const END: usize = 519;
/// The lengths of 8 and more take extra bits above these bases (`m_iArrayOfM`).
const LENGTH_BASES: [usize; 9] = [7, 8, 10, 14, 22, 38, 70, 134, 262];

/// The bytes `compressed` decodes into, at most `limit` of them, and how many bytes of
/// `compressed` the stream took up to its end code.
pub(super) fn explode(compressed: &[u8], limit: usize) -> Result<(Vec<u8>, usize), Malformed> {
    let mut bits = Bits {
        bytes: compressed,
        at: 0,
        held: 0,
        count: 0,
    };
    let mode = bits.read(8)?;
    if mode != 0 {
        return Err(Malformed(format!(
            "the compressed stream is of mode {mode}, not binary"
        )));
    }
    let dictionary = bits.read(8)?;
    if !(4..=6).contains(&dictionary) {
        return Err(Malformed(format!(
            "the compressed stream's dictionary size {dictionary} is not 4, 5 or 6"
        )));
    }
    let mut output = Vec::new();
    loop {
        if bits.read(1)? == 0 {
            let literal = bits.read(8)? as u8;
            push(&mut output, limit, 1)?;
            output.push(literal);
            continue;
        }
        let code = bits.decode(&LENGTH_CODES, "length")?;
        let length = if code <= 7 {
            code + 2
        } else {
            let extra = (code - 7) as u32;
            bits.read(extra)? as usize + 2 + LENGTH_BASES[code - 7]
        };
        if length == END {
            return Ok((output, bits.at));
        }
        let high = bits.decode(&DISTANCE_CODES, "distance")?;
        let low_bits = if length == 2 { 2 } else { dictionary };
        let distance = ((high << low_bits) | bits.read(low_bits)? as usize) + 1;
        if distance > output.len() {
            return Err(Malformed(format!(
                "the compressed stream copies from {distance} bytes back, {} bytes into its output",
                output.len()
            )));
        }
        push(&mut output, limit, length)?;
        let start = output.len() - distance;
        for at in start..start + length {
            output.push(output[at]);
        }
    }
}

/// Makes room for `more` bytes in `output`, unless that takes it past `limit`.
fn push(output: &mut Vec<u8>, limit: usize, more: usize) -> Result<(), Malformed> {
    if output.len() + more > limit {
        return Err(Malformed(format!(
            "the compressed stream decodes into more than {limit} bytes"
        )));
    }
    output.reserve(more);
    Ok(())
}

/// The bits of a stream, least significant first.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
    held: u32,
    count: u32,
}

impl Bits<'_> {
    /// The next `count` bits, at most 16.
    fn read(&mut self, count: u32) -> Result<u32, Malformed> {
        while self.count < count {
            let byte = *self.bytes.get(self.at).ok_or_else(|| {
                Malformed("the compressed stream ends before its end code".into())
            })?;
            self.held |= u32::from(byte) << self.count;
            self.at += 1;
            self.count += 8;
        }
        let value = self.held & ((1 << count) - 1);
        self.held >>= count;
        self.count -= count;
        Ok(value)
    }

    /// The index of the code of `codes` the next bits spell, read a bit at a time.
    fn decode(&mut self, codes: &[&str], what: &str) -> Result<usize, Malformed> {
        let mut code = String::with_capacity(LONGEST_CODE as usize);
        for _ in 0..LONGEST_CODE {
            code.push(if self.read(1)? == 1 { '1' } else { '0' });
            if let Some(index) = codes.iter().position(|&known| known == code) {
                return Ok(index);
            }
        }
        Err(Malformed(format!(
            "the compressed stream holds no {what} code at {code}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test vector of zlib's `blast.c`, which ends in the end code.
    #[test]
    fn explodes_the_blast_test_vector() {
        let compressed = [0x00, 0x04, 0x82, 0x24, 0x25, 0x8f, 0x80, 0x7f];
        let (output, used) = explode(&compressed, 100).unwrap();
        assert_eq!(output, b"AIAIAIAIAIAIA");
        assert_eq!(used, compressed.len());
    }

    #[test]
    fn stops_at_the_limit() {
        let compressed = [0x00, 0x04, 0x82, 0x24, 0x25, 0x8f, 0x80, 0x7f];
        let error = explode(&compressed, 5).unwrap_err();
        assert!(error.0.contains("more than 5 bytes"), "{error:?}");
    }

    #[test]
    fn fails_a_stream_without_its_end_code() {
        let error = explode(&[0x00, 0x04, 0x82, 0x24], 100).unwrap_err();
        assert!(error.0.contains("before its end code"), "{error:?}");
    }
}
