//! The blocks of a Matroska cluster, `SimpleBlock` and the `Block` of a `BlockGroup`: the track
//! a block belongs to, its time relative to its cluster's, and its frames, laced together or
//! not, then decoded as the track's content encodings say.

use std::borrow::Cow;

use super::ebml::{self, Malformed};
use crate::compress::{Decoding, Limited};
use crate::formats::secure::{MAXIMUM_RATIO, OUTPUT_THRESHOLD};

/// The bits of a block's flags that say how its frames are laced.
const LACING: u8 = 0b0110;
const XIPH_LACING: u8 = 0b0010;
const FIXED_LACING: u8 = 0b0100;
const EBML_LACING: u8 = 0b0110;

/// A block's header: the track it belongs to and its time relative to its cluster's, in the
/// segment's timestamp ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) track: u64,
    pub(super) relative_time: i16,
}

/// The header of the block `bytes`, and where its frames start.
pub(super) fn header(bytes: &[u8]) -> Result<(Header, usize), Malformed> {
    let (track, length) = ebml::unsigned_vint(bytes, 0)?;
    let time = bytes
        .get(length..length + 2)
        .ok_or_else(|| Malformed("a block ends inside its header".to_owned()))?;
    Ok((
        Header {
            track,
            relative_time: i16::from_be_bytes([time[0], time[1]]),
        },
        length + 2,
    ))
}

/// The frames of a block whose flags start at `at` of `bytes`, as its lacing lays them out.
pub(super) fn frames(bytes: &[u8], at: usize) -> Result<Vec<&[u8]>, Malformed> {
    let flags = *bytes
        .get(at)
        .ok_or_else(|| Malformed("a block ends before its flags".to_owned()))?;
    let lacing = flags & LACING;
    if lacing == 0 {
        return Ok(vec![&bytes[at + 1..]]);
    }
    let count = usize::from(
        *bytes
            .get(at + 1)
            .ok_or_else(|| Malformed("a laced block ends before its frame count".to_owned()))?,
    ) + 1;
    let mut at = at + 2;
    let sizes = match lacing {
        XIPH_LACING => xiph_sizes(bytes, &mut at, count)?,
        EBML_LACING => ebml_sizes(bytes, &mut at, count)?,
        FIXED_LACING => {
            let total = bytes.len() - at;
            if !total.is_multiple_of(count) {
                return Err(Malformed(format!(
                    "a block laces {total} bytes into {count} frames of equal size"
                )));
            }
            vec![total / count; count - 1]
        }
        _ => unreachable!("the lacing bits take four values"),
    };
    let mut frames = Vec::with_capacity(count);
    for size in sizes {
        let frame = at
            .checked_add(size)
            .and_then(|end| bytes.get(at..end))
            .ok_or_else(|| {
                Malformed(format!("a laced frame of {size} bytes runs past its block"))
            })?;
        frames.push(frame);
        at += size;
    }
    frames.push(&bytes[at..]);
    Ok(frames)
}

/// The sizes of all but the last of `count` frames, in Xiph lacing: each a run of bytes up to
/// one below 255, summed.
fn xiph_sizes(bytes: &[u8], at: &mut usize, count: usize) -> Result<Vec<usize>, Malformed> {
    let mut sizes = Vec::with_capacity(count - 1);
    for _ in 1..count {
        let mut size = 0usize;
        loop {
            let byte = *bytes
                .get(*at)
                .ok_or_else(|| Malformed("a block ends inside its Xiph lacing".to_owned()))?;
            *at += 1;
            size += usize::from(byte);
            if byte != 255 {
                break;
            }
        }
        sizes.push(size);
    }
    Ok(sizes)
}

/// The sizes of all but the last of `count` frames, in EBML lacing: the first a variable-length
/// integer, each after it a signed difference from the one before.
fn ebml_sizes(bytes: &[u8], at: &mut usize, count: usize) -> Result<Vec<usize>, Malformed> {
    let mut sizes = Vec::with_capacity(count - 1);
    let mut size = 0i64;
    for index in 1..count {
        let difference = if index == 1 {
            let (first, length) = ebml::unsigned_vint(bytes, *at)?;
            *at += length;
            i64::try_from(first)
                .map_err(|_| Malformed(format!("a laced frame declares {first} bytes")))?
        } else {
            let (difference, length) = ebml::signed_vint(bytes, *at)?;
            *at += length;
            difference
        };
        size = size
            .checked_add(difference)
            .filter(|&size| size >= 0)
            .ok_or_else(|| {
                Malformed(format!(
                    "a laced frame's size {size} changes by {difference} to below zero"
                ))
            })?;
        sizes.push(
            usize::try_from(size)
                .map_err(|_| Malformed(format!("a laced frame of {size} bytes")))?,
        );
    }
    Ok(sizes)
}

/// A content encoding of a track's frames, `ContentEncoding`, which a frame is decoded from in
/// turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Encoding {
    /// Compressed with zlib.
    Zlib,
    /// With these bytes, common to every frame, stripped from its start.
    HeaderStripping(Vec<u8>),
}

impl Encoding {
    /// The frame `encoded` decoded.
    fn decode<'a>(&self, encoded: Cow<'a, [u8]>) -> Result<Cow<'a, [u8]>, Malformed> {
        match self {
            Encoding::Zlib => inflate(&encoded).map(Cow::Owned),
            Encoding::HeaderStripping(header) => {
                let mut whole = Vec::with_capacity(header.len() + encoded.len());
                whole.extend_from_slice(header);
                whole.extend_from_slice(&encoded);
                Ok(Cow::Owned(whole))
            }
        }
    }
}

/// The zlib stream `encoded` decoded into memory, up to the output Tika's `SecureContentHandler`
/// allows of so many bytes, past which it takes a document for a zip bomb.
fn inflate(encoded: &[u8]) -> Result<Vec<u8>, Malformed> {
    let limit = OUTPUT_THRESHOLD.max((encoded.len() as u64).saturating_mul(MAXIMUM_RATIO));
    let mut decoding = Decoding::in_memory_up_to(limit);
    decoding
        .read_from(flate2::bufread::ZlibDecoder::new(encoded))
        .and_then(|()| decoding.finish())
        .map_err(|error| Malformed(format!("a zlib-compressed frame: {error}")))
        .and_then(|decoded| match decoded {
            Limited::Whole(decoded) => Ok(decoded.into_owned()),
            Limited::Cut(_) => Err(Malformed(format!(
                "a zlib-compressed frame of {} bytes decodes into more than {limit} bytes, \
                 past which Tika takes a document for a zip bomb",
                encoded.len()
            ))),
        })
}

/// A frame decoded from the `encodings` of its track, in the order they are undone.
pub(super) fn decode<'a>(
    frame: &'a [u8],
    encodings: &[Encoding],
) -> Result<Cow<'a, [u8]>, Malformed> {
    encodings
        .iter()
        .try_fold(Cow::Borrowed(frame), |frame, encoding| {
            encoding.decode(frame)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::zlib::tests::zlib;

    #[test]
    fn reads_a_block_header() {
        assert_eq!(
            header(&[0x82, 0xFF, 0xFE, 0x80]),
            Ok((
                Header {
                    track: 2,
                    relative_time: -2
                },
                3
            ))
        );
        assert!(header(&[0x82, 0x00]).is_err());
    }

    #[test]
    fn splits_the_frames_of_every_lacing() {
        assert_eq!(frames(b"\x00abc", 0).unwrap(), [b"abc"]);
        assert_eq!(
            frames(b"\x02\x02\x01\x02abbccc", 0).unwrap(),
            [&b"a"[..], b"bb", b"ccc"]
        );
        let mut long = vec![0x02, 0x01, 0xFF, 0x01];
        long.extend(vec![b'x'; 256]);
        long.push(b'y');
        let laced = frames(&long, 0).unwrap();
        assert_eq!((laced[0].len(), laced[1]), (256, &b"y"[..]));
        assert_eq!(frames(b"\x04\x01aabb", 0).unwrap(), [&b"aa"[..], b"bb"]);
        assert!(frames(b"\x04\x01aab", 0).is_err());
        assert_eq!(
            frames(b"\x06\x02\x82\xC0abbbbc", 0).unwrap(),
            [&b"ab"[..], b"bbb", b"c"]
        );
        assert!(frames(b"\x02\x01\x09ab", 0).is_err());
    }

    #[test]
    fn decodes_frames_in_turn() {
        let encodings = [
            Encoding::Zlib,
            Encoding::HeaderStripping(b"Every ".to_vec()),
        ];
        assert_eq!(
            decode(&zlib(b"evening"), &encodings).unwrap().as_ref(),
            b"Every evening"
        );
        assert_eq!(decode(b"plain", &[]).unwrap().as_ref(), b"plain");
        assert!(decode(b"not zlib", &[Encoding::Zlib]).is_err());
        let bomb = zlib(&vec![b' '; OUTPUT_THRESHOLD as usize + 1]);
        assert!(decode(&bomb, &[Encoding::Zlib])
            .unwrap_err()
            .0
            .contains("zip bomb"));
    }
}
