//! bzip2 files, which the bzip2 crate decodes, read as `BZip2CompressorInputStream` reads them
//! without `decompressConcatenated`: the first stream only, its block and stream CRCs checked,
//! whatever follows it ignored.

use super::{spool_limited, Limited, Undecoded};
use crate::input::Input;

/// The content of the first bzip2 stream of `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    spool_limited(::bzip2::bufread::BzDecoder::new(data), data.len(), None)
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::io::Write;

    #[cfg(test)]
    use super::*;

    pub fn bzip2(content: &[u8]) -> Vec<u8> {
        let mut encoder =
            ::bzip2::write::BzEncoder::new(Vec::new(), ::bzip2::Compression::default());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn decodes_the_first_stream() {
        let mut data = bzip2(b"Hello World\n");
        data.extend_from_slice(&bzip2(b"second"));
        assert_eq!(
            decode(&data).and_then(Limited::whole).unwrap(),
            b"Hello World\n"
        );
    }

    #[test]
    fn fails_where_commons_compress_fails() {
        let data = bzip2(b"Hello World\n");
        assert!(decode(&data[..data.len() - 2]).is_err());
        assert!(decode(b"BZh0").is_err());
        let mut corrupt = data.clone();
        corrupt[12] ^= 0x10;
        assert!(decode(&corrupt).is_err());
    }
}
