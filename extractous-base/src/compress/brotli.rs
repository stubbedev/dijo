//! Brotli streams, which brotli-decompressor decodes as `BrotliCompressorInputStream`, over
//! Google's own Java decoder, decodes them for Tika's `CompressorParser`.

use super::{spool_limited, Limited, Undecoded};
use crate::input::Input;

/// The content of the Brotli stream `data`, or why it is not decoded natively.
pub(crate) fn decode(data: &[u8]) -> Result<Limited<Input<'static>>, Undecoded> {
    spool_limited(
        brotli_decompressor::Decompressor::new(data, 64 * 1024),
        data.len(),
    )
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::io::Write;

    #[cfg(test)]
    use super::*;

    /// `content` compressed as `brotli` does by default.
    pub fn brotli(content: &[u8]) -> Vec<u8> {
        let mut writer = ::brotli::CompressorWriter::new(Vec::new(), 4096, 11, 22);
        writer.write_all(content).unwrap();
        writer.into_inner()
    }

    #[test]
    fn decodes_what_it_is_given_whole() {
        let text = b"Brotli text, compressed. ".repeat(40);
        assert_eq!(
            decode(&brotli(&text)).and_then(Limited::whole).unwrap(),
            text
        );
    }

    #[test]
    fn fails_on_streams_cut_short() {
        let data = brotli(&b"Brotli text, compressed. ".repeat(40));
        assert!(matches!(
            decode(&data[..data.len() / 2]),
            Err(Undecoded::Fails(_))
        ));
    }
}
