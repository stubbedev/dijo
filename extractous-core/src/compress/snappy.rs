//! Framed Snappy streams as commons-compress's `FramedSnappyCompressorInputStream` reads them in
//! its standard dialect: the stream identifier, compressed and uncompressed chunks checked
//! against their masked CRC-32C, padding and skippable chunks skipped. A compressed chunk is
//! read only as far as its Snappy data goes, and what follows that is read as the next chunk.

use super::{throws, within_limit, Undecoded};

const STREAM_IDENTIFIER: [u8; 10] = *b"\xFF\x06\x00\x00sNaPpY";
const COMPRESSED: u8 = 0x00;
const UNCOMPRESSED: u8 = 0x01;
const PADDING: u8 = 0xFE;
const STREAM_IDENTIFIER_TYPE: u8 = 0xFF;
const MASK_OFFSET: u32 = 0xA282_EAD8;
/// The most a chunk decodes into without `SnappyCompressorInputStream`'s window sliding, past
/// which which back-references it accepts depends on how its reader reads.
const MAX_CHUNK: i32 = 65536;

/// The data being read, a read past its end failing as commons-compress does.
struct Input<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Input<'a> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], Undecoded> {
        let end = self
            .at
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| throws("Premature end of stream"))?;
        let taken = &self.data[self.at..end];
        self.at = end;
        Ok(taken)
    }

    /// `ByteUtils.fromLittleEndian` of `length` bytes.
    fn little_endian(&mut self, length: usize) -> Result<u32, Undecoded> {
        let bytes = self.take(length)?;
        Ok(bytes
            .iter()
            .rev()
            .fold(0u32, |value, &b| value << 8 | u32::from(b)))
    }
}

/// `PureJavaCrc32C`: the CRC-32C of `bytes`.
fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                crc >> 1 ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `unmask`: a chunk's stored checksum as the CRC-32C it masks.
fn unmask(masked: u32) -> u32 {
    masked.wrapping_sub(MASK_OFFSET).rotate_right(17)
}

/// The content of the framed Snappy stream `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Vec<u8>, Undecoded> {
    let mut input = Input { data, at: 0 };
    read_stream_identifier(&mut input)?;
    let mut out = Vec::new();
    let mut chunk_start = 0;
    let mut expected: Option<u32> = None;
    loop {
        if let Some(expected) = expected.take() {
            if crc32c(&out[chunk_start..]) != expected {
                return Err(throws("Checksum verification failed"));
            }
        }
        chunk_start = out.len();
        let Some(kind) = input.byte() else {
            return Ok(out);
        };
        match kind {
            STREAM_IDENTIFIER_TYPE => {
                input.at -= 1;
                read_stream_identifier(&mut input)?;
            }
            PADDING | 0x80..=0xFD => {
                let size = input.little_endian(3)?;
                input.take(size as usize)?;
            }
            0x02..=0x7F => return Err(throws(format!("Unskippable chunk with type {kind}"))),
            UNCOMPRESSED => {
                let size = input.little_endian(3)? as i32 - 4;
                if size < 0 {
                    return Err(throws("Found illegal chunk with negative size"));
                }
                expected = Some(unmask(input.little_endian(4)?));
                let available = (input.data.len() - input.at).min(size as usize);
                out.extend_from_slice(input.take(available)?);
            }
            COMPRESSED => {
                let size = i64::from(input.little_endian(3)?) - 4;
                if size < 0 {
                    return Err(throws("Found illegal chunk with negative size"));
                }
                expected = Some(unmask(input.little_endian(4)?));
                let end = input.data.len().min(input.at + size as usize);
                let mut chunk = Input {
                    data: &input.data[..end],
                    at: input.at,
                };
                decode_chunk(&mut chunk, &mut out)?;
                input.at = chunk.at;
            }
        }
        within_limit(&out)?;
    }
}

fn read_stream_identifier(input: &mut Input) -> Result<(), Undecoded> {
    let available = (input.data.len() - input.at).min(STREAM_IDENTIFIER.len());
    if input.take(available)? != STREAM_IDENTIFIER {
        return Err(throws("Not a framed Snappy stream"));
    }
    Ok(())
}

/// `SnappyCompressorInputStream` over one chunk: the uncompressed length, then literals and
/// back-references until they make that length.
fn decode_chunk(input: &mut Input, out: &mut Vec<u8>) -> Result<(), Undecoded> {
    let premature = || throws("Premature end of stream");
    let mut size: i32 = 0;
    let mut index: u32 = 0;
    loop {
        let b = input.byte().ok_or_else(premature)?;
        size |= i32::from(b & 0x7F).wrapping_shl(index * 7);
        index += 1;
        if b & 0x80 == 0 {
            break;
        }
    }
    if !(0..=MAX_CHUNK).contains(&size) {
        return Err(throws(format!("a chunk of {size} bytes")));
    }
    let start = out.len();
    let mut remaining = size;
    while remaining != 0 {
        let b = input.byte().ok_or_else(premature)?;
        let (offset, length) = match b & 0x03 {
            0x00 => {
                let length = match b >> 2 {
                    60 => i64::from(input.byte().ok_or_else(premature)?),
                    61 => i64::from(input.little_endian(2)?),
                    62 => i64::from(input.little_endian(3)?),
                    63 => i64::from(input.little_endian(4)? as i32),
                    short => i64::from(short),
                } + 1;
                let length = i32::try_from(length)
                    .ok()
                    .filter(|length| *length >= 0)
                    .ok_or_else(|| throws("Illegal block with a negative literal size found"))?;
                remaining = remaining.wrapping_sub(length);
                out.extend_from_slice(input.take(length as usize)?);
                continue;
            }
            0x01 => {
                let length = 4 + i32::from(b >> 2 & 0x07);
                let low = input.byte().ok_or_else(premature)?;
                (usize::from(b & 0xE0) << 3 | usize::from(low), length)
            }
            0x02 => (input.little_endian(2)? as usize, i32::from(b >> 2) + 1),
            _ => (
                (input.little_endian(4)? & 0x7FFF_FFFF) as usize,
                i32::from(b >> 2) + 1,
            ),
        };
        remaining = remaining.wrapping_sub(length);
        copy_back(out, start, offset, length as usize)?;
        within_limit(out)?;
    }
    Ok(())
}

/// `startBackReference` and its copy: `length` bytes from `offset` back, which must lie within
/// what the chunk has decoded since `start`.
fn copy_back(
    out: &mut Vec<u8>,
    start: usize,
    offset: usize,
    length: usize,
) -> Result<(), Undecoded> {
    if offset == 0 || offset > out.len() - start {
        return Err(throws("Illegal block with bad offset found"));
    }
    for _ in 0..length {
        out.push(out[out.len() - offset]);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn mask(crc: u32) -> u32 {
        crc.rotate_left(17).wrapping_add(MASK_OFFSET)
    }

    /// A framed stream of `content` in one uncompressed and one compressed chunk, the second
    /// all literals but for a back-reference repeating the first eight bytes.
    pub(crate) fn snappy(content: &[u8]) -> Vec<u8> {
        let mut out = STREAM_IDENTIFIER.to_vec();
        let (first, second) = content.split_at(content.len() / 2);
        out.push(UNCOMPRESSED);
        out.extend_from_slice(&((first.len() + 4) as u32).to_le_bytes()[..3]);
        out.extend_from_slice(&mask(crc32c(first)).to_le_bytes());
        out.extend_from_slice(first);
        let mut chunk = vec![(second.len() + 8) as u8];
        chunk.push(((second.len() - 1) as u8) << 2);
        chunk.extend_from_slice(second);
        chunk.push(0x01 | (4 << 2));
        chunk.push(second.len() as u8);
        let mut decoded = second.to_vec();
        decoded.extend_from_slice(&second[..8]);
        out.push(COMPRESSED);
        out.extend_from_slice(&((chunk.len() + 4) as u32).to_le_bytes()[..3]);
        out.extend_from_slice(&mask(crc32c(&decoded)).to_le_bytes());
        out.extend_from_slice(&chunk);
        out
    }

    #[test]
    fn decodes_chunks_checking_their_sums() {
        let content = b"Lorem ipsum dolor sit amet, consectetur adipiscing";
        let data = snappy(content);
        let decoded = decode(&data).unwrap();
        assert_eq!(&decoded[..content.len()], content);
        assert_eq!(
            &decoded[content.len()..],
            &content[content.len() / 2..][..8]
        );
        let mut corrupt = data.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        assert!(decode(&corrupt).is_err());
        assert!(decode(&data[..data.len() - 3]).is_err());
        assert!(decode(b"\xFF\x06\x00\x00sNaPpX").is_err());
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }
}
