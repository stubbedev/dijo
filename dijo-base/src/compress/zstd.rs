//! Zstandard files, which ruzstd decodes: every frame in turn, skippable frames passed over,
//! as `zstd -d` and zstd-jni's `ZstdInputStream` read them.

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::StreamingDecoder;

use super::Held;

use super::{throws, Decoding, Limited, Undecoded};
use crate::input::Input;

/// The content of the Zstandard file `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    let mut decoding = Decoding::spooled(data.len(), None);
    decode_into(data, &mut decoding)?;
    decoding.finish()
}

/// The first bytes of a Zstandard frame.
const FRAME_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Whether `data` starts as a Zstandard frame or a skippable frame does.
fn starts_frame(data: &[u8]) -> bool {
    data.starts_with(&FRAME_MAGIC)
        || (data.len() >= 4 && data[0] & 0xF0 == 0x50 && data[1..4] == [0x2A, 0x4D, 0x18])
}

/// What may follow the frames decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rest {
    /// Nothing: the data is frames to its end.
    Nothing,
    /// Anything that does not start as a frame does, where the frames end.
    Anything,
}

/// Decodes the Zstandard file `data` into `decoding`, up to its limit.
pub(crate) fn decode_into(
    data: &[u8],
    decoding: &mut Decoding<impl Held>,
) -> Result<(), Undecoded> {
    frames_into(data, decoding, Rest::Nothing).map(|_| ())
}

/// Decodes into `decoding`, up to its limit, the Zstandard frames at the start of `data`, as
/// long as what follows starts as a frame does, and returns how many bytes they take; none
/// where the decoding is cut at its limit.
pub(crate) fn decode_frames_into(
    data: &[u8],
    decoding: &mut Decoding<impl Held>,
) -> Result<Option<usize>, Undecoded> {
    frames_into(data, decoding, Rest::Anything)
}

/// Decodes the frames at the start of `data` into `decoding`, every frame in turn, skippable
/// frames passed over, up to what `rest` lets follow them: how many bytes they take, none
/// where the decoding is cut at its limit.
fn frames_into(
    data: &[u8],
    decoding: &mut Decoding<impl Held>,
    rest: Rest,
) -> Result<Option<usize>, Undecoded> {
    let mut input = data;
    while !input.is_empty() && (rest == Rest::Nothing || starts_frame(input)) {
        match StreamingDecoder::new(&mut input) {
            Ok(mut frame) => {
                decoding.read_from(&mut frame)?;
                if decoding.is_cut() {
                    return Ok(None);
                }
            }
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                input = input
                    .get(length as usize..)
                    .ok_or_else(|| throws("a skippable frame runs past the input"))?;
            }
            Err(e) => return Err(throws(e.to_string())),
        }
    }
    Ok(Some(data.len() - input.len()))
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    #[cfg(test)]
    use super::*;

    /// A Zstandard frame of `content`, stored in raw blocks.
    pub fn zstd(content: &[u8]) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(content, ruzstd::encoding::CompressionLevel::Uncompressed)
    }

    #[test]
    fn decodes_every_frame() {
        let mut data = zstd(b"first ");
        data.extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18, 2, 0, 0, 0, 9, 9]);
        data.extend_from_slice(&zstd(b"second"));
        assert_eq!(
            decode(&data).and_then(Limited::whole).unwrap(),
            *b"first second"
        );
        assert!(decode(&data[..5]).is_err());
    }

    #[test]
    fn finds_where_the_frames_end() {
        let mut data = zstd(b"first ");
        data.extend_from_slice(&zstd(b"second"));
        let frames = data.len();
        data.extend_from_slice(b"PK\x07\x08 after the frames");
        let mut decoding = Decoding::in_memory();
        assert_eq!(decode_frames_into(&data, &mut decoding), Ok(Some(frames)));
        assert_eq!(
            *decoding.finish().and_then(Limited::whole).unwrap(),
            *b"first second"
        );
    }
}
