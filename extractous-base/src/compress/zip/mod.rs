//! ZIP archives as commons-compress reads them: by the central directory with `ZipFile`
//! ([`file`]), or front to back by the local headers with `ZipArchiveInputStream` ([`stream`]).

pub mod extra;
pub mod file;
pub mod salvage;
pub mod stream;

use std::borrow::Cow;
use std::io::Read;

use flate2::{Decompress, FlushDecompress, Status};

#[cfg(doc)]
use super::{decode_limit, MAX_DECODED};
use super::{Decoding, Held, Limited, Undecoded, CHUNK};
use crate::input::Input;

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

/// An entry's data as it is read: all of it, what was read before the data failed, or what an
/// entry that decodes past the limit decodes into up to it, which is not read on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded<D> {
    Complete(D),
    Truncated(D),
    Oversized(D),
}

/// An entry's content in memory, up to [`MAX_DECODED`] bytes: a stored entry's the archive's own
/// bytes, an inflated entry's its own.
pub type Content<'a> = Decoded<Cow<'a, [u8]>>;

/// An entry's content as a document embedded in the archive, up to [`decode_limit`] bytes: a
/// stored entry's the archive's own bytes, an inflated entry's spooled.
pub type Spooled<'a> = Decoded<Input<'a>>;

impl<D> Decoded<D> {
    pub fn map<E>(self, f: impl FnOnce(D) -> E) -> Decoded<E> {
        match self {
            Decoded::Complete(data) => Decoded::Complete(f(data)),
            Decoded::Truncated(data) => Decoded::Truncated(f(data)),
            Decoded::Oversized(data) => Decoded::Oversized(f(data)),
        }
    }

    pub fn bytes(&self) -> &[u8]
    where
        D: AsRef<[u8]>,
    {
        match self {
            Decoded::Complete(bytes) | Decoded::Truncated(bytes) | Decoded::Oversized(bytes) => {
                bytes.as_ref()
            }
        }
    }
}

/// What `decoding` holds of an entry's data, which it decoded to its end where `complete`.
fn decoded<'a, W: Held>(
    decoding: Decoding<W>,
    complete: bool,
) -> Result<Decoded<W::Bytes<'a>>, Undecoded> {
    Ok(match decoding.finish()? {
        Limited::Cut(data) => Decoded::Oversized(data),
        Limited::Whole(data) if complete => Decoded::Complete(data),
        Limited::Whole(data) => Decoded::Truncated(data),
    })
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

/// Raw deflate data inflated into `decoding` as `InflaterInputStream` inflates it: to the end of
/// the deflate stream, or as far as it goes before the input runs out or turns out corrupt, or
/// past the limit of `decoding`.
pub(crate) fn inflate_into<'a, W: Held>(
    raw: &[u8],
    mut decoding: Decoding<W>,
) -> Result<Decoded<W::Bytes<'a>>, Undecoded> {
    let mut inflater = Decompress::new(false);
    let mut chunk = vec![0; CHUNK];
    loop {
        let (before_in, before_out) = (inflater.total_in(), inflater.total_out());
        let consumed = usize::try_from(before_in).expect("the input fits in memory");
        let input: &[u8] = match consumed.cmp(&raw.len()) {
            std::cmp::Ordering::Less => &raw[consumed..],
            std::cmp::Ordering::Equal => &[0],
            std::cmp::Ordering::Greater => return decoded(decoding, false),
        };
        let status = inflater.decompress(input, &mut chunk, FlushDecompress::None);
        let produced = usize::try_from(inflater.total_out() - before_out)
            .expect("at most a chunk is produced");
        decoding.write(&chunk[..produced])?;
        match status {
            _ if decoding.is_cut() => return decoded(decoding, false),
            Ok(Status::StreamEnd) => return decoded(decoding, true),
            Err(_) => return decoded(decoding, false),
            Ok(_) if inflater.total_in() == before_in && produced == 0 => {
                return decoded(decoding, false)
            }
            Ok(_) => {}
        }
    }
}

/// Whether commons-compress decompresses data compressed with `method`, neither stored nor
/// deflated: `ZipUtil.supportsMethodOf`'s other methods.
pub(crate) fn decompresses(method: u16) -> bool {
    [UNSHRINKING, IMPLODING, DEFLATE64, BZIP2, XZ].contains(&method) || ZSTD.contains(&method)
}

/// The name of the `ZipMethod` of `code`, as `ZipMethod.getMethodByCode` finds it.
pub(crate) fn method_name(code: u16) -> Option<&'static str> {
    Some(match code {
        STORED => "STORED",
        UNSHRINKING => "UNSHRINKING",
        2 => "EXPANDING_LEVEL_1",
        3 => "EXPANDING_LEVEL_2",
        4 => "EXPANDING_LEVEL_3",
        5 => "EXPANDING_LEVEL_4",
        IMPLODING => "IMPLODING",
        7 => "TOKENIZATION",
        DEFLATED => "DEFLATED",
        DEFLATE64 => "ENHANCED_DEFLATED",
        10 => "PKWARE_IMPLODING",
        BZIP2 => "BZIP2",
        14 => "LZMA",
        20 => "ZSTD_DEPRECATED",
        93 => "ZSTD",
        XZ => "XZ",
        96 => "JPEG",
        97 => "WAVPACK",
        98 => "PPMD",
        99 => "AES_ENCRYPTED",
        _ => return None,
    })
}

/// The data `raw` of an entry compressed with `method` and the general purpose `flags`, `size`
/// bytes once decoded, decoded into `decoding` as commons-compress's `ZipFile.getInputStream`
/// decodes it, without checking its CRC: all of it, what was decoded before the data failed, or
/// past the limit of `decoding`. `None` for a method commons-compress does not decompress, and
/// for stored and deflated data, which the readers take from the archive as far as it goes.
pub(crate) fn decode_into<'a, W: Held>(
    method: u16,
    flags: u16,
    raw: &[u8],
    size: u64,
    mut decoding: Decoding<W>,
) -> Option<Result<Decoded<W::Bytes<'a>>, Undecoded>> {
    if ZSTD.contains(&method) {
        return Some(match super::zstd::decode_into(raw, &mut decoding) {
            Ok(()) => decoded(decoding, true),
            Err(Undecoded::Unspooled(reason)) => Err(Undecoded::Unspooled(reason)),
            Err(_) => decoded(decoding, false),
        });
    }
    decompresses(method).then(|| decode_with_zip_crate(method, flags, raw, size, decoding))
}

/// Data of a method other than Zstandard, as the `zip` crate decodes it, read from a local
/// header written for it, the only way the crate takes data of the legacy methods.
fn decode_with_zip_crate<'a, W: Held>(
    method: u16,
    flags: u16,
    raw: &[u8],
    size: u64,
    mut decoding: Decoding<W>,
) -> Result<Decoded<W::Bytes<'a>>, Undecoded> {
    const DATA_DESCRIPTOR: u16 = 0x0008;
    const IMPLODE_OPTIONS: u16 = 0x0006;
    const DOS_EPOCH_DATE: u16 = 0x0021;
    let mut header = Vec::with_capacity(30);
    header.extend_from_slice(&LFH_SIG);
    for field in [
        10,
        flags & IMPLODE_OPTIONS | DATA_DESCRIPTOR,
        method,
        0,
        DOS_EPOCH_DATE,
    ] {
        header.extend_from_slice(&field.to_le_bytes());
    }
    header.extend_from_slice(&[0; 16]);
    let options = zip::read::ZipReadOptions::new()
        .ignore_crc32(true)
        .override_compressed_size(raw.len() as u64)
        .override_uncompressed_size(size);
    let mut input = header.as_slice().chain(raw);
    let read = match zip::read::read_zipfile_from_stream_with_options(&mut input, options) {
        Ok(Some(entry)) => match decoding.read_from(entry) {
            Ok(()) => true,
            Err(Undecoded::Unspooled(reason)) => return Err(Undecoded::Unspooled(reason)),
            Err(_) => false,
        },
        Ok(None) | Err(_) => false,
    };
    let complete = read && decoding.written() == size;
    decoded(decoding, complete)
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
        let decoded = decode_into(IMPLODING, entry.flags, raw, size, Decoding::in_memory());
        let Some(Ok(Content::Truncated(bytes))) = decoded else {
            panic!("data cut short is decoded whole");
        };
        assert!(bytes.len() < 1078);
    }
}
