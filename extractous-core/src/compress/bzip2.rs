//! bzip2 files as commons-compress's `BZip2CompressorInputStream` reads them without
//! `decompressConcatenated`: the first stream only, its block and stream CRCs checked, whatever
//! follows it ignored. libbzip2's decoder, which commons-compress ports, does the decoding.

use super::{throws, within_limit, Undecoded};

/// The content of the first bzip2 stream of `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Vec<u8>, Undecoded> {
    let mut decoder = ::bzip2::Decompress::new(false);
    let mut out = Vec::with_capacity(data.len().saturating_mul(4).min(super::MAX_DECODED));
    loop {
        let consumed = usize::try_from(decoder.total_in()).expect("the input fits in memory");
        let produced = out.len();
        out.reserve(64 * 1024);
        let status = decoder
            .decompress_vec(&data[consumed..], &mut out)
            .map_err(|e| throws(format!("the bzip2 data is corrupt: {e}")))?;
        within_limit(&out)?;
        if status == ::bzip2::Status::StreamEnd {
            return Ok(out);
        }
        let taken = usize::try_from(decoder.total_in()).expect("the input fits in memory");
        if taken == consumed && out.len() == produced {
            return Err(throws("the input ends inside the bzip2 stream"));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    pub(crate) fn bzip2(content: &[u8]) -> Vec<u8> {
        let mut encoder =
            ::bzip2::write::BzEncoder::new(Vec::new(), ::bzip2::Compression::default());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn decodes_the_first_stream() {
        let mut data = bzip2(b"Hello World\n");
        data.extend_from_slice(&bzip2(b"second"));
        assert_eq!(decode(&data).unwrap(), b"Hello World\n");
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
