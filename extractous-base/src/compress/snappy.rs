//! Framed Snappy streams, which the snap crate decodes, read as
//! `FramedSnappyCompressorInputStream` reads them: the stream identifier, compressed and
//! uncompressed chunks checked against their masked CRC-32C, padding and skippable chunks
//! skipped.

use super::{spool_limited, Limited, Undecoded};
use crate::input::Input;

/// The content of the framed Snappy stream `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    spool_limited(snap::read::FrameDecoder::new(data), data.len())
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::io::Write;

    #[cfg(test)]
    use super::*;

    /// A framed Snappy stream of `content`.
    pub fn snappy(content: &[u8]) -> Vec<u8> {
        let mut encoder = snap::write::FrameEncoder::new(Vec::new());
        encoder.write_all(content).unwrap();
        encoder.into_inner().unwrap()
    }

    #[test]
    fn decodes_chunks_checking_their_sums() {
        let content = b"Lorem ipsum dolor sit amet, Lorem ipsum dolor sit amet, consectetur";
        let data = snappy(content);
        assert_eq!(decode(&data).and_then(Limited::whole).unwrap(), content);
        let mut corrupt = data.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        assert!(decode(&corrupt).is_err());
        assert!(decode(&data[..data.len() - 3]).is_err());
        assert!(decode(b"\xFF\x06\x00\x00sNaPpX").is_err());
    }
}
