//! ZIP archives as commons-compress reads them: by the central directory with `ZipFile`
//! ([`file`]), or front to back by the local headers with `ZipArchiveInputStream` ([`stream`]).

pub mod extra;
pub mod file;
pub mod salvage;
pub mod stream;

use std::borrow::Cow;
use std::io::Read;

use flate2::{Decompress, FlushDecompress, Status};

use super::MAX_DECODED;

pub(crate) const LFH_SIG: [u8; 4] = *b"PK\x03\x04";
pub(crate) const CFH_SIG: [u8; 4] = *b"PK\x01\x02";
pub(crate) const STORED: u16 = 0;
pub(crate) const UNSHRINKING: u16 = 1;
pub(crate) const IMPLODING: u16 = 6;
pub(crate) const DEFLATED: u16 = 8;
pub(crate) const DEFLATE64: u16 = 9;
pub(crate) const BZIP2: u16 = 12;
pub(crate) const ZSTD: [u16; 2] = [93, 20];
pub(crate) const XZ: u16 = 95;

/// Why `ZipFile` cannot open an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupt;

/// An entry's content: all of it, what was read before the data failed, or the first
/// [`MAX_DECODED`] bytes of an entry that inflates past them, which is not read on.
/// A stored entry's is the archive's own bytes; an inflated entry's is its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content<'a> {
    Complete(Cow<'a, [u8]>),
    Truncated(Cow<'a, [u8]>),
    Oversized(Cow<'a, [u8]>),
}

impl Content<'_> {
    pub fn bytes(&self) -> &[u8] {
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
            Ok(_) if out.len() > MAX_DECODED => return oversized(out),
            Ok(Status::StreamEnd) => return Content::Complete(Cow::Owned(out)),
            Err(_) => return Content::Truncated(Cow::Owned(out)),
            Ok(_) if inflater.total_in() == before_in && out.len() == before_out => {
                return Content::Truncated(Cow::Owned(out))
            }
            Ok(_) => {}
        }
    }
}

/// The content of an entry that decodes past [`MAX_DECODED`] bytes, of which `decoded` holds
/// more.
fn oversized(mut decoded: Vec<u8>) -> Content<'static> {
    decoded.truncate(MAX_DECODED);
    Content::Oversized(Cow::Owned(decoded))
}

/// Whether commons-compress decompresses data compressed with `method`, neither stored nor
/// deflated: `ZipUtil.supportsMethodOf`'s other methods.
pub(crate) fn decompresses(method: u16) -> bool {
    [UNSHRINKING, IMPLODING, DEFLATE64, BZIP2, XZ].contains(&method) || ZSTD.contains(&method)
}

/// The data `raw` of an entry compressed with `method` and the general purpose `flags`, `size`
/// bytes once decoded, as commons-compress's `ZipFile.getInputStream` decodes it, without
/// checking its CRC: all of it, what was decoded before the data failed, or past
/// [`MAX_DECODED`] bytes. `None` for a method commons-compress does not decompress, and for
/// stored and deflated data, which the readers take from the archive as far as it goes.
pub(crate) fn decode(method: u16, flags: u16, raw: &[u8], size: u64) -> Option<Content<'static>> {
    if ZSTD.contains(&method) {
        return Some(match super::zstd::decode(raw) {
            Ok(super::Limited::Whole(content)) => Content::Complete(Cow::Owned(content)),
            Ok(super::Limited::Cut(content)) => Content::Oversized(Cow::Owned(content)),
            Err(_) => Content::Truncated(Cow::Owned(Vec::new())),
        });
    }
    decompresses(method).then(|| decode_with_zip_crate(method, flags, raw, size))
}

/// Data of a method other than Zstandard, as the `zip` crate decodes it, read from a local
/// header written for it, the only way the crate takes data of the legacy methods.
fn decode_with_zip_crate(method: u16, flags: u16, raw: &[u8], size: u64) -> Content<'static> {
    const DATA_DESCRIPTOR: u16 = 0x0008;
    const IMPLODE_OPTIONS: u16 = 0x0006;
    const DOS_EPOCH_DATE: u16 = 0x0021;
    let mut stream = Vec::with_capacity(30 + raw.len());
    stream.extend_from_slice(&LFH_SIG);
    for field in [
        10,
        flags & IMPLODE_OPTIONS | DATA_DESCRIPTOR,
        method,
        0,
        DOS_EPOCH_DATE,
    ] {
        stream.extend_from_slice(&field.to_le_bytes());
    }
    stream.extend_from_slice(&[0; 16]);
    stream.extend_from_slice(raw);
    let options = zip::read::ZipReadOptions::new()
        .ignore_crc32(true)
        .override_compressed_size(raw.len() as u64)
        .override_uncompressed_size(size);
    let mut out = Vec::new();
    let mut input = stream.as_slice();
    let read = match zip::read::read_zipfile_from_stream_with_options(&mut input, options) {
        Ok(Some(entry)) => entry
            .take(MAX_DECODED as u64 + 1)
            .read_to_end(&mut out)
            .is_ok(),
        Ok(None) | Err(_) => false,
    };
    if out.len() > MAX_DECODED {
        oversized(out)
    } else if read && out.len() as u64 == size {
        Content::Complete(Cow::Owned(out))
    } else {
        Content::Truncated(Cow::Owned(out))
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

    /// Two entries of Tika's `moby.zip`, one imploded and one shrunk.
    const LEGACY: &[u8] = include_bytes!("../../../tests/fixtures/zip/legacy.zip");

    fn assert_decoded(entry: &file::ZipEntry, content: Content) {
        let Content::Complete(bytes) = content else {
            panic!("{} is not decoded whole: {content:?}", entry.name);
        };
        assert_eq!(bytes.len() as i64, entry.size, "{}", entry.name);
        assert_eq!(
            i64::from(super::super::crc32(&bytes)),
            entry.crc,
            "{}",
            entry.name
        );
    }

    #[test]
    fn decodes_imploded_and_shrunk_entries() {
        let zip = file::ZipFile::open(LEGACY).unwrap();
        let methods: Vec<u16> = zip.entries().iter().map(|entry| entry.method).collect();
        assert_eq!(methods, [IMPLODING, UNSHRINKING]);
        for entry in zip.entries() {
            assert_decoded(entry, zip.read(entry).unwrap());
        }
        let mut stream = stream::ZipStream::new(LEGACY, false);
        let mut read = 0;
        while let Some(entry) = stream.next_entry().unwrap() {
            assert_decoded(&entry, stream.content().unwrap());
            read += 1;
        }
        assert_eq!(read, 2);
    }

    #[test]
    fn keeps_what_it_decoded_of_data_cut_short() {
        let zip = file::ZipFile::open(LEGACY).unwrap();
        let entry = &zip.entries()[0];
        let start = usize::try_from(entry.data_offset).unwrap();
        let raw = &LEGACY[start..start + 300];
        let size = u64::try_from(entry.size).unwrap();
        let Some(Content::Truncated(bytes)) = decode(IMPLODING, entry.flags, raw, size) else {
            panic!("data cut short is decoded whole");
        };
        assert!(bytes.len() < 1078);
    }
}
