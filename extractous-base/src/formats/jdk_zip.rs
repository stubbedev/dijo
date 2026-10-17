//! ZIP archives as the JDK reads them for our Java parsers: `java.util.zip.ZipFile`, by the
//! central directory, and `ZipInputStream`, front to back by the local headers.
//!
//! The port opens an archive with the port of commons-compress's `ZipFile`, failing where the
//! JDK refuses to open it (encrypted entries, methods other than stored and deflated, names
//! that are not UTF-8), and reads local headers with the CRC and size checks `ZipInputStream`
//! makes. What the JDK might read differently (duplicate names, names commons-compress takes
//! from elsewhere, ZIP64 sizes, data descriptors, damaged data) is left to Tika's path.

use std::collections::HashSet;
use std::io::Read;

use crate::compress::zip::file::{ZipEntry, ZipFile};
use crate::compress::zip::{u16_at, u32_at, Content, DEFLATED, LFH_SIG, STORED};
use crate::compress::{crc32, MAX_DECODED};

const LOCAL_HEADER_BYTES: usize = 30;
const ENCRYPTED: u16 = 0x0001;
const DATA_DESCRIPTOR: u16 = 0x0008;
const ZIP64_SIZE: i64 = 0xFFFF_FFFF;

/// Why a port stops reading: Tika's path is left to read the document, or the Java parser
/// fails on it, for the reason given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unread(&'static str),
    Failed(String),
}

pub fn failed(reason: impl Into<String>) -> Refusal {
    Refusal::Failed(reason.into())
}

/// `new ZipFile(file)`: the archive, failing where the JDK refuses to open it.
pub fn open(input: &[u8]) -> Result<ZipFile<'_>, Refusal> {
    let zip = ZipFile::open(input).map_err(|_| failed("The document is not a ZIP archive"))?;
    let mut names = HashSet::new();
    for entry in zip.entries() {
        if entry.uses_encryption() {
            return Err(failed("invalid CEN header (encrypted entry)"));
        }
        if entry.method != STORED && entry.method != DEFLATED {
            return Err(failed(format!(
                "invalid CEN header (bad compression method: {})",
                entry.method
            )));
        }
        if std::str::from_utf8(&entry.raw_name).is_err() {
            return Err(failed("invalid CEN header (bad entry name or comment)"));
        }
        if entry.name.as_bytes() != entry.raw_name {
            return Err(Refusal::Unread("an entry name commons-compress changes"));
        }
        if entry.size >= ZIP64_SIZE || entry.compressed_size >= ZIP64_SIZE {
            return Err(Refusal::Unread("an entry with ZIP64 sizes"));
        }
        if !names.insert(entry.name.as_str()) {
            return Err(Refusal::Unread("several entries of one name"));
        }
    }
    Ok(zip)
}

/// `zip.getInputStream(entry)` read to its end: all of it, or its first [`MAX_DECODED`] bytes
/// and more of an entry that inflates past them.
pub fn read<'a>(zip: &ZipFile<'a>, entry: &ZipEntry) -> Result<Content<'a>, Refusal> {
    match zip.read(entry) {
        Some(content @ (Content::Complete(_) | Content::Oversized(_))) => Ok(content),
        Some(Content::Truncated(_)) | None => Err(Refusal::Unread("a damaged entry")),
    }
}

/// `zip.getInputStream(entry)` read whole, an entry that inflates past [`MAX_DECODED`] bytes
/// left to Tika's path.
pub fn read_whole(zip: &ZipFile<'_>, entry: &ZipEntry) -> Result<Vec<u8>, Refusal> {
    match read(zip, entry)? {
        Content::Complete(bytes) => Ok(bytes.into_owned()),
        _ => Err(Refusal::Unread("an entry larger than the port reads")),
    }
}

/// `zip.getEntry(name)`: the entry of that name, or else the directory of it, which the port
/// leaves to Tika's path.
pub fn entry<'z>(zip: &'z ZipFile<'_>, name: &str) -> Result<Option<&'z ZipEntry>, Refusal> {
    if let Some(entry) = zip.entry(name) {
        return Ok(Some(entry));
    }
    if zip.entry(&format!("{name}/")).is_some() {
        return Err(Refusal::Unread("a directory read as a file"));
    }
    Ok(None)
}

/// The entries `ZipInputStream` reads from `data`, with their content, front to back until a
/// header that is not a local file header.
pub fn local_entries(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, Refusal> {
    let mut entries = Vec::new();
    let mut at = 0;
    while let Some(header) = data.get(at..at + LOCAL_HEADER_BYTES) {
        if header[..4] != LFH_SIG {
            break;
        }
        let flags = u16_at(header, 6);
        let method = u16_at(header, 8);
        let crc = u32_at(header, 14);
        let compressed_size = u32_at(header, 18);
        let size = u32_at(header, 22);
        let name_length = usize::from(u16_at(header, 26));
        let extra_length = usize::from(u16_at(header, 28));
        if flags & (ENCRYPTED | DATA_DESCRIPTOR) != 0
            || compressed_size == ZIP64_SIZE
            || size == ZIP64_SIZE
        {
            return Err(Refusal::Unread(
                "a ZIP entry encrypted, with a data descriptor or with ZIP64 sizes",
            ));
        }
        let unread = || Refusal::Unread("a ZIP entry ZipInputStream may read otherwise");
        let name_at = at + LOCAL_HEADER_BYTES;
        let name = data
            .get(name_at..name_at + name_length)
            .and_then(|name| std::str::from_utf8(name).ok())
            .ok_or_else(unread)?;
        let data_at = name_at + name_length + extra_length;
        let compressed_size = usize::try_from(compressed_size).map_err(|_| unread())?;
        let raw = data
            .get(data_at..data_at + compressed_size)
            .ok_or_else(unread)?;
        let content = match method {
            STORED => raw.to_vec(),
            DEFLATED => inflate_exactly(raw).ok_or_else(unread)?,
            _ => return Err(unread()),
        };
        if i64::try_from(content.len()) != Ok(size) || i64::from(crc32(&content)) != crc {
            return Err(unread());
        }
        entries.push((name.to_owned(), content));
        at = data_at + compressed_size;
    }
    Ok(entries)
}

/// Raw deflate data that inflates to its end, within [`MAX_DECODED`] bytes, using every byte
/// of it.
fn inflate_exactly(raw: &[u8]) -> Option<Vec<u8>> {
    let mut inflater = flate2::bufread::DeflateDecoder::new(raw);
    let mut content = Vec::new();
    inflater
        .by_ref()
        .take(MAX_DECODED as u64 + 1)
        .read_to_end(&mut content)
        .ok()?;
    (inflater.total_in() == raw.len() as u64 && content.len() <= MAX_DECODED).then_some(content)
}
