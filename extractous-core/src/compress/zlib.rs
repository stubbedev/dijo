//! zlib streams, which flate2 decodes, read as `DeflateCompressorInputStream` reads them: the
//! header, the deflate data and the Adler-32 checksum, whatever follows ignored.

use super::{read_whole, Undecoded};

/// The content of the zlib stream `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Vec<u8>, Undecoded> {
    read_whole(flate2::bufread::ZlibDecoder::new(data))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    pub(crate) fn zlib(content: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn decodes_to_the_checksum() {
        let mut data = zlib(b"Test d'indexation de Txt");
        data.extend_from_slice(b"after");
        assert_eq!(decode(&data).unwrap(), b"Test d'indexation de Txt");
        assert!(decode(&data[..data.len() - 6]).is_err());
        let mut corrupt = data.clone();
        let checksum = corrupt.len() - 6;
        corrupt[checksum] ^= 1;
        assert!(decode(&corrupt).is_err());
    }
}
