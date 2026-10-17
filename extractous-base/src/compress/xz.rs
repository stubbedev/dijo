//! xz and LZMA files, which lzma-rust2, a port of XZ for Java, decodes, read as
//! `XZCompressorInputStream` without `decompressConcatenated` and `LZMACompressorInputStream`
//! read them, under the memory limit Tika's `CompressorParser` sets.

use super::{read_whole, throws, Undecoded, MEMORY_LIMIT_KIB};

/// The first stream of the xz file `data`, or why it is not decoded natively.
pub(crate) fn decode_xz(data: &[u8]) -> Result<Vec<u8>, Undecoded> {
    read_whole(lzma_rust2::XzReader::new_mem_limit(
        data,
        false,
        MEMORY_LIMIT_KIB,
    ))
}

/// The content of the `.lzma` file `data`, or why it is not decoded natively.
pub(crate) fn decode_lzma(data: &[u8]) -> Result<Vec<u8>, Undecoded> {
    let reader = lzma_rust2::LzmaReader::new_mem_limit(data, MEMORY_LIMIT_KIB, None)
        .map_err(|e| throws(e.to_string()))?;
    read_whole(reader)
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::io::Write;

    #[cfg(test)]
    use super::*;

    /// An xz file of `content`.
    pub fn xz(content: &[u8]) -> Vec<u8> {
        let mut writer =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(6)).unwrap();
        writer.write_all(content).unwrap();
        writer.finish().unwrap()
    }

    /// A `.lzma` file of `content`, its size in the header.
    pub fn lzma(content: &[u8]) -> Vec<u8> {
        let options = lzma_rust2::LzmaOptions::with_preset(6);
        let mut writer = lzma_rust2::LzmaWriter::new_use_header(
            Vec::new(),
            &options,
            Some(content.len() as u64),
        )
        .unwrap();
        writer.write_all(content).unwrap();
        writer.finish().unwrap()
    }

    #[test]
    fn decodes_the_first_stream() {
        let mut data = xz(b"compressed with xz");
        assert_eq!(decode_xz(&data).unwrap(), b"compressed with xz");
        let length = data.len();
        data[length - 20] ^= 1;
        assert!(decode_xz(&data).is_err());
        assert_eq!(
            decode_lzma(&lzma(b"compressed with lzma")).unwrap(),
            b"compressed with lzma"
        );
    }

    #[test]
    fn refuses_what_needs_more_memory_than_tika_allows() {
        let mut header = vec![0x5D];
        header.extend_from_slice(&(1u32 << 30).to_le_bytes());
        header.extend_from_slice(&[0xFF; 8]);
        header.extend_from_slice(&[0; 16]);
        assert!(decode_lzma(&header).is_err());
    }
}
