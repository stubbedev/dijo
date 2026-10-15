//! zlib streams as commons-compress's `DeflateCompressorInputStream` reads them: the zlib
//! header, the deflate data and the Adler-32 checksum through `InflaterInputStream`, whatever
//! follows ignored.

use super::{inflate_whole, throws, Undecoded};

/// The `FDICT` flag of the header's second byte: the data needs a preset dictionary.
const FDICT: u8 = 0x20;

/// The content of the zlib stream `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Vec<u8>, Undecoded> {
    if data.get(1).is_some_and(|flags| flags & FDICT != 0) {
        return Err(throws(
            "the stream needs a preset dictionary, without which InflaterInputStream ends it",
        ));
    }
    inflate_whole(data, true).map(|(content, _)| content)
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
