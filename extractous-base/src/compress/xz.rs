//! xz and LZMA files, which lzma-rust2, a port of XZ for Java, decodes, read as
//! `XZCompressorInputStream` without `decompressConcatenated` and `LZMACompressorInputStream`
//! read them, under the memory limit Tika's `CompressorParser` sets.

use super::{decoder_failure, spool_limited, Decoding, Held, Limited, Undecoded, MEMORY_LIMIT_KIB};
use crate::input::Input;

/// The first stream of the xz file `data`, or why it is not decoded natively.
pub(crate) fn decode_xz(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    spool_limited(
        lzma_rust2::XzReader::new_mem_limit(data, false, MEMORY_LIMIT_KIB),
        data.len(),
        None,
    )
}

/// Decodes into `decoding`, up to its limit, the xz stream at the start of `data`, read as
/// `XZCompressorInputStream` without `decompressConcatenated` reads it, without a memory limit,
/// and returns how many bytes the stream takes, its index and footer included; none where the
/// decoding is cut at its limit.
pub(crate) fn decode_stream_into(
    data: &[u8],
    decoding: &mut Decoding<impl Held>,
) -> Result<Option<usize>, Undecoded> {
    let mut input = data;
    decoding.read_from(lzma_rust2::XzReader::new(&mut input, false))?;
    Ok((!decoding.is_cut()).then(|| data.len() - input.len()))
}

/// The content of the `.lzma` file `data`, or why it is not decoded natively.
pub(crate) fn decode_lzma(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    let reader = lzma_rust2::LzmaReader::new_mem_limit(data, MEMORY_LIMIT_KIB, None)
        .map_err(decoder_failure)?;
    spool_limited(reader, data.len(), lzma_declared_size(data))
}

/// The uncompressed size the header of the `.lzma` file `data` declares, unless it declares it
/// unknown.
fn lzma_declared_size(data: &[u8]) -> Option<u64> {
    let size = u64::from_le_bytes(data.get(5..13)?.try_into().expect("eight bytes"));
    (size != u64::MAX).then_some(size)
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
    fn finds_where_the_stream_ends() {
        let mut data = xz(b"content");
        let stream = data.len();
        data.extend_from_slice(b"PK\x07\x08 after the stream");
        let mut decoding = Decoding::in_memory();
        assert_eq!(decode_stream_into(&data, &mut decoding), Ok(Some(stream)));
        assert_eq!(
            *decoding.finish().and_then(Limited::whole).unwrap(),
            *b"content"
        );
    }

    #[test]
    fn decodes_the_first_stream() {
        let mut data = xz(b"compressed with xz");
        assert_eq!(
            decode_xz(&data).and_then(Limited::whole).unwrap(),
            b"compressed with xz"
        );
        let length = data.len();
        data[length - 20] ^= 1;
        assert!(decode_xz(&data).is_err());
        assert_eq!(
            decode_lzma(&lzma(b"compressed with lzma"))
                .and_then(Limited::whole)
                .unwrap(),
            b"compressed with lzma"
        );
    }

    #[test]
    fn refuses_what_needs_more_memory_than_tika_allows() {
        let mut header = vec![0x5D];
        header.extend_from_slice(&(1u32 << 30).to_le_bytes());
        header.extend_from_slice(&[0xFF; 8]);
        header.extend_from_slice(&[0; 16]);
        assert!(matches!(
            decode_lzma(&header),
            Err(Undecoded::MemoryLimit(_))
        ));
    }
}
