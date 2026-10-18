//! LZ4 frames, which lz4_flex decodes, read as `FramedLZ4CompressorInputStream` reads them
//! without `decompressConcatenated`: the skippable frames before the first frame passed over,
//! and that frame alone decoded, whatever follows it ignored.

use super::{spool_limited, throws, Limited, Undecoded};
use crate::input::Input;

const MAGIC: [u8; 4] = [0x04, 0x22, 0x4D, 0x18];
const BLOCK_CHECKSUM: u8 = 0x10;
const CONTENT_SIZE: u8 = 0x08;
const CONTENT_CHECKSUM: u8 = 0x04;
const UNCOMPRESSED: u32 = 0x8000_0000;

/// The content of the first LZ4 frame of `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    let frame = first_frame(data)?;
    spool_limited(
        lz4_flex::frame::FrameDecoder::new(frame),
        data.len(),
        declared_size(frame),
    )
}

/// The content size the descriptor of `frame` declares, if it declares one.
fn declared_size(frame: &[u8]) -> Option<u64> {
    if frame.get(4)? & CONTENT_SIZE == 0 {
        return None;
    }
    Some(u64::from_le_bytes(
        frame.get(6..14)?.try_into().expect("eight bytes"),
    ))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, Undecoded> {
    data.get(at..at + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .ok_or_else(|| throws("the input ends inside the LZ4 frame"))
}

/// The bytes of the first frame, past the skippable frames before it: its descriptor, its
/// blocks and their checksums up to the end mark, and the content checksum.
fn first_frame(data: &[u8]) -> Result<&[u8], Undecoded> {
    let mut at = 0usize;
    loop {
        let signature = data
            .get(at..at + 4)
            .ok_or_else(|| throws("Not a LZ4 frame stream"))?;
        if signature[0] & 0x50 == 0x50 && signature[1..] == [0x2A, 0x4D, 0x18] {
            at = at
                .checked_add(8 + u32_at(data, at + 4)? as usize)
                .ok_or_else(|| throws("a skippable frame past the input"))?;
            continue;
        }
        if signature != MAGIC {
            return Err(throws("Not a LZ4 frame stream"));
        }
        break;
    }
    let start = at;
    let flags = *data
        .get(at + 4)
        .ok_or_else(|| throws("the input ends inside the LZ4 frame"))?;
    at += 7 + if flags & CONTENT_SIZE != 0 { 8 } else { 0 };
    loop {
        let length = u32_at(data, at)?;
        at += 4;
        if length == 0 {
            if flags & CONTENT_CHECKSUM != 0 {
                at += 4;
            }
            break;
        }
        at += (length & !UNCOMPRESSED) as usize;
        if flags & BLOCK_CHECKSUM != 0 {
            at += 4;
        }
    }
    data.get(start..at)
        .ok_or_else(|| throws("the input ends inside the LZ4 frame"))
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::io::Write;

    #[cfg(test)]
    use super::*;

    /// An LZ4 frame of `content`.
    pub fn lz4(content: &[u8]) -> Vec<u8> {
        let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn decodes_the_first_frame_only() {
        let content = b"0123456789abcdefghij0123456789abcdefghij";
        let mut data = vec![0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3];
        data.extend_from_slice(&lz4(content));
        data.extend_from_slice(&lz4(b"second frame"));
        data.extend_from_slice(b"garbage");
        assert_eq!(decode(&data).and_then(Limited::whole).unwrap(), content);
        let one = lz4(content);
        assert!(decode(&one[..one.len() - 2]).is_err());
        let mut corrupt = one.clone();
        corrupt[5] ^= 1;
        assert!(decode(&corrupt).is_err());
        assert!(decode(b"\x02\x21\x4C\x18rest").is_err());
    }
}
