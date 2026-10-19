//! zlib streams, which flate2 decodes, read as `DeflateCompressorInputStream` reads them: the
//! header, the deflate data and the Adler-32 checksum, whatever follows ignored.

use super::{spool_limited, Decoding, Limited, Undecoded};
use crate::input::Input;

/// The content of the zlib stream `data`, or why it is not decoded natively.
pub fn decode(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    spool_limited(flate2::bufread::ZlibDecoder::new(data), data.len(), None)
}

/// The content of the zlib stream `data` starts with, decoded into memory, up to
/// [`MAX_DECODED`](super::MAX_DECODED) bytes, past which it fails, and how many bytes of `data`
/// the stream takes.
pub fn decode_prefix(data: &[u8]) -> Result<(Vec<u8>, usize), Undecoded> {
    let mut decoder = flate2::bufread::ZlibDecoder::new(data);
    let mut decoding = Decoding::in_memory();
    decoding.read_from(&mut decoder)?;
    let content = decoding.finish()?.whole()?.into_owned();
    let taken = usize::try_from(decoder.total_in()).expect("a stream takes at most its input");
    Ok((content, taken))
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::io::Write;

    #[cfg(test)]
    use super::*;

    pub fn zlib(content: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn decodes_to_the_checksum() {
        let mut data = zlib(b"Test d'indexation de Txt");
        data.extend_from_slice(b"after");
        assert_eq!(
            decode(&data).and_then(Limited::whole).unwrap(),
            b"Test d'indexation de Txt"
        );
        assert!(decode(&data[..data.len() - 6]).is_err());
        let mut corrupt = data.clone();
        let checksum = corrupt.len() - 6;
        corrupt[checksum] ^= 1;
        assert!(decode(&corrupt).is_err());
        assert_eq!(
            decode_prefix(&data).unwrap(),
            (b"Test d'indexation de Txt".to_vec(), data.len() - 5)
        );
    }
}
