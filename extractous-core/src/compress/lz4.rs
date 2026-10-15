//! LZ4 frames as commons-compress's `FramedLZ4CompressorInputStream` reads them without
//! `decompressConcatenated`: skippable frames before the first, the frame descriptor and its
//! checksum, blocks compressed or stored with their checksums, and the content checksum at the
//! end mark. A compressed block is read only as far as its sequences go, the last of which ends
//! with literals; what follows that is read as the block checksum or the next block.

use super::{throws, within_limit, Undecoded};

const MAGIC: [u8; 4] = [0x04, 0x22, 0x4D, 0x18];
const VERSION_MASK: u8 = 0xC0;
const SUPPORTED_VERSION: u8 = 0x40;
const BLOCK_INDEPENDENCE: u8 = 0x20;
const BLOCK_CHECKSUM: u8 = 0x10;
const CONTENT_SIZE: u8 = 0x08;
const CONTENT_CHECKSUM: u8 = 0x04;
const UNCOMPRESSED: u32 = 0x8000_0000;
/// `BlockLZ4CompressorInputStream.WINDOW_SIZE`: the dependency buffer of linked blocks, which
/// starts all zeros.
const WINDOW: usize = 1 << 16;

const PRIME1: u32 = 0x9E37_79B1;
const PRIME2: u32 = 0x85EB_CA77;
const PRIME3: u32 = 0xC2B2_AE3D;
const PRIME4: u32 = 0x27D4_EB2F;
const PRIME5: u32 = 0x1656_67B1;

/// `XXHash32` with seed 0.
fn xxhash32(data: &[u8]) -> u32 {
    let lane = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().expect("four bytes"));
    let round = |value: u32, input: u32| {
        value
            .wrapping_add(input.wrapping_mul(PRIME2))
            .rotate_left(13)
            .wrapping_mul(PRIME1)
    };
    let mut rest = data;
    let mut hash = if data.len() >= 16 {
        let mut v = [
            PRIME1.wrapping_add(PRIME2),
            PRIME2,
            0,
            0u32.wrapping_sub(PRIME1),
        ];
        while rest.len() >= 16 {
            for (i, value) in v.iter_mut().enumerate() {
                *value = round(*value, lane(&rest[i * 4..i * 4 + 4]));
            }
            rest = &rest[16..];
        }
        v[0].rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18))
    } else {
        PRIME5
    };
    hash = hash.wrapping_add(data.len() as u32);
    while rest.len() >= 4 {
        hash = hash
            .wrapping_add(lane(&rest[..4]).wrapping_mul(PRIME3))
            .rotate_left(17)
            .wrapping_mul(PRIME4);
        rest = &rest[4..];
    }
    for &byte in rest {
        hash = hash
            .wrapping_add(u32::from(byte).wrapping_mul(PRIME5))
            .rotate_left(11)
            .wrapping_mul(PRIME1);
    }
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(PRIME2);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(PRIME3);
    hash ^ hash >> 16
}

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

    fn take(&mut self, length: usize, what: &str) -> Result<&'a [u8], Undecoded> {
        let end = self
            .at
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| throws(format!("Premature end of stream while reading {what}")))?;
        let taken = &self.data[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u32_le(&mut self, what: &str) -> Result<u32, Undecoded> {
        let bytes = self.take(4, what)?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
    }
}

/// The content of the LZ4 frame stream `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Vec<u8>, Undecoded> {
    let mut input = Input { data, at: 0 };
    let mut signature = input
        .take(4, "the signature")
        .map_err(|_| throws("Not a LZ4 frame stream"))?;
    while signature[0] & 0x50 == 0x50 && signature[1..] == [0x2A, 0x4D, 0x18] {
        let length = input.u32_le("a skippable frame's size")?;
        input.take(length as usize, "a skippable frame")?;
        signature = input
            .take(4, "the signature")
            .map_err(|_| throws("Not a LZ4 frame stream"))?;
    }
    if signature != MAGIC {
        return Err(throws("Not a LZ4 frame stream"));
    }
    let descriptor_start = input.at;
    let flags = input
        .byte()
        .ok_or_else(|| throws("Premature end of stream while reading frame flags"))?;
    if flags & VERSION_MASK != SUPPORTED_VERSION {
        return Err(throws(format!("Unsupported version {}", flags >> 6)));
    }
    input
        .byte()
        .ok_or_else(|| throws("Premature end of stream while reading frame BD byte"))?;
    if flags & CONTENT_SIZE != 0 {
        input.take(8, "content size")?;
    }
    let header_hash = xxhash32(&input.data[descriptor_start..input.at]) >> 8 & 0xFF;
    let stored = input
        .byte()
        .ok_or_else(|| throws("Premature end of stream while reading frame header checksum"))?;
    if u32::from(stored) != header_hash {
        return Err(throws("Frame header checksum mismatch"));
    }
    let linked = flags & BLOCK_INDEPENDENCE == 0;
    let history = if linked { WINDOW } else { 0 };
    let mut out = vec![0u8; history];
    let mut last_block: Option<&[u8]> = None;
    loop {
        if let Some(block) = last_block.take() {
            if flags & BLOCK_CHECKSUM != 0 && input.u32_le("block checksum")? != xxhash32(block) {
                return Err(throws("block checksum mismatch."));
            }
        }
        let length = input.u32_le("a block size")?;
        let size = (length & !UNCOMPRESSED) as usize;
        if size == 0 {
            if flags & CONTENT_CHECKSUM != 0
                && input.u32_le("content checksum")? != xxhash32(&out[history..])
            {
                return Err(throws("content checksum mismatch."));
            }
            out.drain(..history);
            return Ok(out);
        }
        let end = input.data.len().min(input.at.saturating_add(size));
        let start = input.at;
        if length & UNCOMPRESSED != 0 {
            out.extend_from_slice(&input.data[start..end]);
            input.at = end;
        } else {
            let mut block = Input {
                data: &input.data[..end],
                at: start,
            };
            let block_start = if linked { 0 } else { out.len() };
            decode_block(&mut block, &mut out, block_start)?;
            input.at = block.at;
        }
        last_block = Some(&input.data[start..input.at]);
        within_limit(&out)?;
    }
}

/// `readSizeBytes`: the bytes added to a length of 15, up to one below 255.
fn extension(block: &mut Input) -> Result<usize, Undecoded> {
    let mut length = 0usize;
    loop {
        let byte = block
            .byte()
            .ok_or_else(|| throws("Premature end of stream while parsing length"))?;
        length += usize::from(byte);
        if byte != 255 {
            return Ok(length);
        }
    }
}

/// `BlockLZ4CompressorInputStream`: sequences of literals and back-references until the input
/// ends where a back-reference's offset would start after a sequence without one. A
/// back-reference reaches at most back to `available_from`.
fn decode_block(
    block: &mut Input,
    out: &mut Vec<u8>,
    available_from: usize,
) -> Result<(), Undecoded> {
    loop {
        let token = block
            .byte()
            .ok_or_else(|| throws("Premature end of stream while looking for next block"))?;
        let mut literals = usize::from(token >> 4);
        if literals == 15 {
            literals += extension(block)?;
        }
        out.extend_from_slice(block.take(literals, "literal")?);
        let match_nibble = usize::from(token & 0x0F);
        let Some(low) = block.byte() else {
            if match_nibble == 0 {
                return Ok(());
            }
            return Err(throws("Premature end of data"));
        };
        let Some(high) = block.byte() else {
            if match_nibble == 0 {
                return Ok(());
            }
            return Err(throws("Premature end of data"));
        };
        let offset = usize::from(u16::from_le_bytes([low, high]));
        let mut length = match_nibble;
        if length == 15 {
            length += extension(block)?;
        }
        length += 4;
        if offset == 0 || offset > out.len() - available_from {
            return Err(throws("Illegal block with bad offset found"));
        }
        for _ in 0..length {
            out.push(out[out.len() - offset]);
        }
        within_limit(out)?;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A frame of independent blocks with block and content checksums: `content` stored in one
    /// block, then compressed in another as literals and a back-reference of its first four
    /// bytes.
    pub(crate) fn lz4(content: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        let flags = SUPPORTED_VERSION | BLOCK_INDEPENDENCE | BLOCK_CHECKSUM | CONTENT_CHECKSUM;
        out.extend_from_slice(&[flags, 0x40]);
        out.push((xxhash32(&[flags, 0x40]) >> 8 & 0xFF) as u8);
        out.extend_from_slice(&(content.len() as u32 | UNCOMPRESSED).to_le_bytes());
        out.extend_from_slice(content);
        out.extend_from_slice(&xxhash32(content).to_le_bytes());
        let literals = content.len().min(14);
        let mut block = vec![(literals as u8) << 4];
        block.extend_from_slice(&content[..literals]);
        block.extend_from_slice(&(literals as u16).to_le_bytes());
        block.push(0);
        let mut decoded = content.to_vec();
        decoded.extend_from_slice(&content[..literals]);
        decoded.extend_from_slice(&content[..4]);
        out.extend_from_slice(&(block.len() as u32).to_le_bytes());
        out.extend_from_slice(&block);
        out.extend_from_slice(&xxhash32(&block).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&xxhash32(&decoded).to_le_bytes());
        out
    }

    #[test]
    fn hashes_as_xxhash32_does() {
        assert_eq!(xxhash32(b""), 0x02CC_5D05);
        assert_eq!(xxhash32(b"abc"), 0x32D1_53FF);
        assert_eq!(
            xxhash32(b"Nobody inspects the spammish repetition"),
            0xE229_3B2F
        );
    }

    #[test]
    fn decodes_blocks_checking_their_sums() {
        let content = b"0123456789abcdefghij";
        let data = lz4(content);
        let decoded = decode(&data).unwrap();
        assert_eq!(&decoded[..content.len()], content);
        assert_eq!(decoded.len(), content.len() + 14 + 4);
        let mut corrupt = data.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        assert!(decode(&corrupt).is_err());
        assert!(decode(&data[..data.len() - 6]).is_err());
        let mut bad_header = data.clone();
        bad_header[6] ^= 1;
        assert!(decode(&bad_header).is_err());
    }
}
