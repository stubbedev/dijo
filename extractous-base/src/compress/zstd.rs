//! Zstandard files, which ruzstd decodes: every frame in turn, skippable frames passed over,
//! as `zstd -d` and zstd-jni's `ZstdInputStream` read them. Tika's path does not decode them,
//! since its native image lacks zstd-jni.

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::StreamingDecoder;

use super::{limit, read_limited, throws, Limited, Undecoded, MAX_DECODED};

/// The content of the Zstandard file `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Limited<Vec<u8>>, Undecoded> {
    let mut input = data;
    let mut out = Vec::new();
    while !input.is_empty() {
        match StreamingDecoder::new(&mut input) {
            Ok(mut frame) => match read_limited(&mut frame)? {
                Limited::Whole(content) => out.extend_from_slice(&content),
                Limited::Cut(content) => {
                    out.extend_from_slice(&content);
                    out.truncate(MAX_DECODED);
                    return Ok(Limited::Cut(out));
                }
            },
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
        if out.len() > MAX_DECODED {
            return Ok(limit(out));
        }
    }
    Ok(Limited::Whole(out))
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
            b"first second"
        );
        assert!(decode(&data[..5]).is_err());
    }
}
