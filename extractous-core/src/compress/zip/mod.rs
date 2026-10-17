//! ZIP archives as commons-compress reads them: by the central directory with `ZipFile`
//! ([`file`]), or front to back by the local headers with `ZipArchiveInputStream` ([`stream`]).

pub(crate) mod extra;
pub(crate) mod file;
pub(crate) mod salvage;
pub(crate) mod stream;

use std::borrow::Cow;

use flate2::{Decompress, FlushDecompress, Status};

use super::MAX_DECODED;

pub(crate) const LFH_SIG: [u8; 4] = *b"PK\x03\x04";
pub(crate) const CFH_SIG: [u8; 4] = *b"PK\x01\x02";
pub(crate) const STORED: u16 = 0;
pub(crate) const DEFLATED: u16 = 8;

/// Why `ZipFile` cannot open an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Corrupt;

/// An entry's content: all of it, what was read before the data failed, or the first
/// [`MAX_DECODED`] bytes and more of an entry that inflates past them, which is not read on.
/// A stored entry's is the archive's own bytes; an inflated entry's is its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content<'a> {
    Complete(Cow<'a, [u8]>),
    Truncated(Cow<'a, [u8]>),
    Oversized(Cow<'a, [u8]>),
}

impl Content<'_> {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Content::Complete(bytes) | Content::Truncated(bytes) | Content::Oversized(bytes) => {
                bytes
            }
        }
    }
}

/// Bytes decoded as UTF-8, a malformed sequence replaced by `?`, as commons-compress decodes
/// entry names and comments.
pub(crate) fn decode_name(bytes: &[u8]) -> String {
    let mut name = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        name.push_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            name.push('?');
        }
    }
    name
}

pub(crate) fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

pub(crate) fn u32_at(data: &[u8], offset: usize) -> i64 {
    i64::from(u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

pub(crate) fn u64_at(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(data[offset..offset + 8].try_into().expect("eight bytes"))
}

/// Raw deflate data inflated as `InflaterInputStream` inflates it: to the end of the deflate
/// stream, or as far as it goes before the input runs out or turns out corrupt, or past
/// [`MAX_DECODED`] bytes.
pub(crate) fn inflate(raw: &[u8]) -> Content<'static> {
    let mut inflater = Decompress::new(false);
    let mut out = Vec::new();
    loop {
        let (before_in, before_out) = (inflater.total_in(), out.len());
        out.reserve(64 * 1024);
        let consumed = usize::try_from(before_in).expect("the input fits in memory");
        let input: &[u8] = match consumed.cmp(&raw.len()) {
            std::cmp::Ordering::Less => &raw[consumed..],
            std::cmp::Ordering::Equal => &[0],
            std::cmp::Ordering::Greater => return Content::Truncated(Cow::Owned(out)),
        };
        match inflater.decompress_vec(input, &mut out, FlushDecompress::None) {
            Ok(_) if out.len() > MAX_DECODED => return Content::Oversized(Cow::Owned(out)),
            Ok(Status::StreamEnd) => return Content::Complete(Cow::Owned(out)),
            Err(_) => return Content::Truncated(Cow::Owned(out)),
            Ok(_) if inflater.total_in() == before_in && out.len() == before_out => {
                return Content::Truncated(Cow::Owned(out))
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_names_as_commons_compress_does() {
        assert_eq!(decode_name(b"caf\xC3\xA9"), "caf\u{E9}");
        assert_eq!(decode_name(b"caf\xE9"), "caf?");
    }
}
