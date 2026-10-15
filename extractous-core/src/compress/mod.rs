//! Ports of the parts of Apache commons-compress 1.28 that Tika reads archives and compressed
//! files with, failing and recovering where commons-compress does.
//!
//! A compressed stream is decoded whole, as the native parsers hand its content on, so each
//! decoder answers with all of it or with why commons-compress would not give all of it:
//! [`Undecoded`].

pub(crate) mod ar;
pub(crate) mod archiver;
pub(crate) mod bzip2;
pub(crate) mod compressor;
pub(crate) mod cpio;
pub(crate) mod gzip;
pub(crate) mod lz4;
pub(crate) mod lzw;
pub(crate) mod seven_z;
pub(crate) mod snappy;
pub(crate) mod tar;
pub(crate) mod tar_header;
pub(crate) mod xz;
pub(crate) mod zip;
pub(crate) mod zlib;
pub(crate) mod zstd;

use std::fmt;
use std::io::Read;

/// `CompressorParser`'s `memoryLimitInKb`, which the decoders that size their memory by the
/// stream keep within.
pub(crate) const MEMORY_LIMIT_KIB: u32 = 100_000;

/// The most bytes a stream is decoded into: Tika's path, which streams them, reads more.
pub(crate) const MAX_DECODED: usize = 256 * 1024 * 1024;

/// Why a compressed stream or an archive is not read natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Undecoded {
    /// Reading it fails, for this reason, as reading it fails on Tika's path.
    Fails(String),
    /// It decodes into more than [`MAX_DECODED`] bytes.
    TooLarge,
    /// It holds what the native readers do not read the way Tika's path does.
    Unported(&'static str),
}

impl fmt::Display for Undecoded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Undecoded::Fails(reason) => write!(f, "reading it fails: {reason}"),
            Undecoded::TooLarge => write!(f, "it decodes into more than {MAX_DECODED} bytes"),
            Undecoded::Unported(what) => write!(f, "the native readers do not read {what}"),
        }
    }
}

fn throws(reason: impl Into<String>) -> Undecoded {
    Undecoded::Fails(reason.into())
}

/// A file of an archive, as the archive readers give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveFile {
    pub(crate) name: String,
    pub(crate) size: u64,
    /// The modification time in milliseconds since the epoch, if the archive records one.
    pub(crate) modified_millis: Option<i64>,
    /// The creation time in milliseconds since the epoch, if the archive records one.
    pub(crate) created_millis: Option<i64>,
    pub(crate) directory: bool,
    /// The data, none for a directory.
    pub(crate) content: Vec<u8>,
}

/// Bytes decoded as US-ASCII, as `ArchiveUtils.toAsciiString` decodes them: each byte beyond
/// ASCII replaced by U+FFFD.
fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii() {
                char::from(b)
            } else {
                '\u{FFFD}'
            }
        })
        .collect()
}

/// The CRC-32 of `bytes`.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

/// Fails once `decoded` holds more than [`MAX_DECODED`] bytes.
fn within_limit(decoded: &[u8]) -> Result<(), Undecoded> {
    if decoded.len() > MAX_DECODED {
        Err(Undecoded::TooLarge)
    } else {
        Ok(())
    }
}

/// What `reader`, a crate's decoder, gives up to its end, failing where it fails or past
/// [`MAX_DECODED`] bytes.
fn read_whole(reader: impl Read) -> Result<Vec<u8>, Undecoded> {
    let mut out = Vec::new();
    reader
        .take(MAX_DECODED as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| throws(e.to_string()))?;
    within_limit(&out)?;
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Bits written least significant first, as deflate packs them.
    #[derive(Default)]
    struct Bits {
        out: Vec<u8>,
        cache: u64,
        count: u32,
    }

    impl Bits {
        fn put(&mut self, value: u64, length: u32) {
            self.cache |= value << self.count;
            self.count += length;
            while self.count >= 8 {
                self.out.push(self.cache as u8);
                self.cache >>= 8;
                self.count -= 8;
            }
        }

        /// A Huffman code, which deflate packs most significant bit first.
        fn code(&mut self, code: u64, length: u32) {
            self.put(code.reverse_bits() >> (64 - length), length);
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                self.out.push(self.cache as u8);
            }
            self.out
        }
    }

    /// Raw deflate data of one block with fixed codes: a zero byte, then `copies` copies of the
    /// 258 bytes before, so `1 + 258 * copies` zero bytes in all.
    pub(crate) fn deflated_zeros(copies: usize) -> Vec<u8> {
        let mut bits = Bits::default();
        bits.put(1, 1);
        bits.put(1, 2);
        bits.code(0x30, 8);
        for _ in 0..copies {
            bits.code(0xC5, 8);
            bits.code(0, 5);
        }
        bits.code(0, 7);
        bits.finish()
    }

    #[test]
    fn stops_decoding_past_the_limit() {
        let small = deflated_zeros(3);
        let inflate = |data| read_whole(flate2::bufread::DeflateDecoder::new(data));
        assert_eq!(inflate(&small[..]).unwrap(), vec![0; 775]);
        let bomb = deflated_zeros(MAX_DECODED / 258 + 1);
        assert_eq!(inflate(&bomb[..]), Err(Undecoded::TooLarge));
        assert!(matches!(zip::inflate(&bomb), zip::Content::Oversized(_)));
    }
}
