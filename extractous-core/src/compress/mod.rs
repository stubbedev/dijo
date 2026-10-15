//! Ports of the parts of Apache commons-compress 1.28 that Tika reads archives and compressed
//! files with, failing and recovering where commons-compress does.
//!
//! A compressed stream is decoded whole, as the native parsers hand its content on, so each
//! decoder answers with all of it or with why commons-compress would not give all of it:
//! [`Undecoded`].

pub(crate) mod ar;
pub(crate) mod archiver;
pub(crate) mod bzip2;
pub(crate) mod cpio;
pub(crate) mod gzip;
pub(crate) mod lzw;
pub(crate) mod tar;
pub(crate) mod zip;
pub(crate) mod zlib;

use std::fmt;

/// The most bytes a stream is decoded into: Tika's path, which streams them, reads more.
pub(crate) const MAX_DECODED: usize = 256 * 1024 * 1024;

/// Why a compressed stream is not decoded natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Undecoded {
    /// commons-compress throws on it, for this reason.
    Throws(String),
    /// It decodes into more than [`MAX_DECODED`] bytes.
    TooLarge,
}

impl fmt::Display for Undecoded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Undecoded::Throws(reason) => write!(f, "commons-compress throws on it: {reason}"),
            Undecoded::TooLarge => write!(f, "it decodes into more than {MAX_DECODED} bytes"),
        }
    }
}

fn throws(reason: impl Into<String>) -> Undecoded {
    Undecoded::Throws(reason.into())
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

/// `String.trim`: without the characters up to the space at either end.
fn java_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c <= ' ')
}

/// `Long.parseLong(text, radix)` of a text of ASCII characters: a sign and at least one digit.
fn parse_long(text: &str, radix: u32) -> Option<i64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    i64::from_str_radix(text, radix).ok()
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

/// Inflates the deflate data at the start of `input` to its end, raw or behind a zlib header
/// as `zlib` says: what it decodes into and how many bytes of `input` it takes, or why it fails
/// to end, as `java.util.zip.Inflater` fails when the input runs out or turns out corrupt.
fn inflate_whole(input: &[u8], zlib: bool) -> Result<(Vec<u8>, usize), Undecoded> {
    let mut inflater = flate2::Decompress::new(zlib);
    let mut out = Vec::with_capacity(input.len().saturating_mul(4).min(MAX_DECODED));
    loop {
        let consumed = usize::try_from(inflater.total_in()).expect("the input fits in memory");
        let produced = out.len();
        out.reserve(64 * 1024);
        let status = inflater
            .decompress_vec(&input[consumed..], &mut out, flate2::FlushDecompress::None)
            .map_err(|e| throws(format!("the compressed data is corrupt: {e}")))?;
        within_limit(&out)?;
        let taken = usize::try_from(inflater.total_in()).expect("the input fits in memory");
        match status {
            flate2::Status::StreamEnd => return Ok((out, taken)),
            _ if taken == consumed && out.len() == produced => {
                return Err(throws("the input ends inside the compressed data"))
            }
            _ => {}
        }
    }
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
        assert_eq!(
            inflate_whole(&small, false).unwrap(),
            (vec![0; 775], small.len())
        );
        let bomb = deflated_zeros(MAX_DECODED / 258 + 1);
        assert_eq!(inflate_whole(&bomb, false), Err(Undecoded::TooLarge));
        assert!(matches!(zip::inflate(&bomb), zip::Content::Oversized(_)));
    }
}
