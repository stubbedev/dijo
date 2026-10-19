//! The compression formats commons-compress's `CompressorStreamFactory` decodes: those its
//! `detect` recognizes by their first bytes, and Brotli, which has no signature and is decoded
//! only as named.

use super::{brotli, bzip2, gzip, lz4, lzw, pack200, snappy, xz, zlib, zstd, Limited, Undecoded};
use crate::input::Input;

/// A compression format `CompressorStreamFactory` decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compressor {
    Bzip2,
    Gzip,
    Pack200,
    SnappyFramed,
    Z,
    Zlib,
    Xz,
    Lzma,
    Lz4Framed,
    Zstd,
    Brotli,
}

/// What a compressed stream decodes into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The original file name a gzip header holds.
    pub file_name: Option<String>,
    pub content: Input<'static>,
}

const ALL: [Compressor; 11] = [
    Compressor::Bzip2,
    Compressor::Gzip,
    Compressor::Pack200,
    Compressor::SnappyFramed,
    Compressor::Z,
    Compressor::Zlib,
    Compressor::Xz,
    Compressor::Lzma,
    Compressor::Lz4Framed,
    Compressor::Zstd,
    Compressor::Brotli,
];

impl Compressor {
    /// `CompressorStreamFactory.detect` over the first bytes.
    pub fn detect(prefix: &[u8]) -> Option<Compressor> {
        let length = prefix.len().min(12);
        let mut signature = [0u8; 12];
        signature[..length].copy_from_slice(&prefix[..length]);
        let s = &signature;
        let found = if length >= 3 && s.starts_with(b"BZh") {
            Compressor::Bzip2
        } else if length >= 2 && s.starts_with(&[0x1F, 0x8B]) {
            Compressor::Gzip
        } else if length >= 4 && s.starts_with(&[0xCA, 0xFE, 0xD0, 0x0D]) {
            Compressor::Pack200
        } else if length >= 10 && s.starts_with(b"\xFF\x06\x00\x00sNaPpY") {
            Compressor::SnappyFramed
        } else if length > 3 && s.starts_with(&[0x1F, 0x9D]) {
            Compressor::Z
        } else if length > 3 && s[0] == 0x78 && matches!(s[1], 0x01 | 0x5E | 0x9C | 0xDA) {
            Compressor::Zlib
        } else if length >= 6 && s.starts_with(b"\xFD7zXZ\x00") {
            Compressor::Xz
        } else if length >= 3 && s.starts_with(&[0x5D, 0, 0]) {
            Compressor::Lzma
        } else if length >= 4 && s.starts_with(&[0x04, 0x22, 0x4D, 0x18]) {
            Compressor::Lz4Framed
        } else if length >= 4
            && (s.starts_with(&[0x28, 0xB5, 0x2F, 0xFD])
                || (s[0] & 0xF0 == 0x50 && s[1..4] == [0x2A, 0x4D, 0x18]))
        {
            Compressor::Zstd
        } else {
            return None;
        };
        Some(found)
    }

    /// The media type Tika gives the format.
    pub const fn media_type(self) -> &'static str {
        match self {
            Compressor::Bzip2 => "application/x-bzip2",
            Compressor::Gzip => "application/gzip",
            Compressor::Pack200 => "application/x-java-pack200",
            Compressor::SnappyFramed => "application/x-snappy",
            Compressor::Z => "application/x-compress",
            Compressor::Zlib => "application/zlib",
            Compressor::Xz => "application/x-xz",
            Compressor::Lzma => "application/x-lzma",
            Compressor::Lz4Framed => "application/x-lz4",
            Compressor::Zstd => "application/zstd",
            Compressor::Brotli => "application/x-brotli",
        }
    }

    /// The format of exactly the media type `media_type`.
    pub fn named(media_type: &str) -> Option<Compressor> {
        ALL.into_iter()
            .find(|compressor| compressor.media_type() == media_type)
    }

    /// The stream `data` decoded whole to a spool, up to [`decode_limit`](super::decode_limit)
    /// bytes, as commons-compress decodes it without `decompressConcatenated`, or why it is not
    /// decoded natively.
    pub fn decode(self, data: &[u8]) -> Result<Limited<Decoded>, Undecoded> {
        let content = |content: Limited<Input<'static>>| {
            content.map(|content| Decoded {
                file_name: None,
                content,
            })
        };
        match self {
            Compressor::Gzip => gzip::decode(data).map(|member| {
                member.map(|member| Decoded {
                    file_name: member.file_name,
                    content: member.content,
                })
            }),
            Compressor::Bzip2 => bzip2::decode(data).map(content),
            Compressor::Zlib => zlib::decode(data).map(content),
            Compressor::Z => lzw::decode(data).map(content),
            Compressor::SnappyFramed => snappy::decode(data).map(content),
            Compressor::Lz4Framed => lz4::decode(data).map(content),
            Compressor::Xz => xz::decode_xz(data).map(content),
            Compressor::Lzma => xz::decode_lzma(data).map(content),
            Compressor::Zstd => zstd::decode(data).map(content),
            Compressor::Brotli => brotli::decode(data).map(content),
            Compressor::Pack200 => pack200::decode(data).map(content),
        }
    }
}
