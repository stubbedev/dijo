//! ZIP archives as the JDK reads them for our Java parsers: `java.util.zip.ZipFile`, by the
//! central directory, and `ZipInputStream`, front to back by the local headers.
//!
//! The port opens an archive with the port of commons-compress's `ZipFile`, failing where the
//! JDK refuses to open it (encrypted entries, methods other than stored and deflated, names
//! that are not UTF-8), and reads local headers with the CRC and size checks `ZipInputStream`
//! makes. An entry is read as a document of its own ([`Archive::spool`]): a stored one in place,
//! a deflated one decoded to a spool, which keeps as much in memory as a file read whole and the
//! rest in a mapped temporary file. What the JDK might read differently (duplicate names, names commons-compress takes
//! from elsewhere, ZIP64 sizes, data descriptors, damaged data) is left to Tika's path.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::ops::Range;

#[cfg(doc)]
use crate::compress::decode_limit;
use crate::compress::zip::file::{ZipEntry, ZipFile};
use crate::compress::zip::{u16_at, u32_at, Content, Decoded, DEFLATED, LFH_SIG, STORED};
use crate::compress::MAX_DECODED;
use crate::errors::Error;
use crate::formats::{Decline, Failure};
use crate::input::{Input, Pages, Paging, Spool};

const LOCAL_HEADER_BYTES: usize = 30;
const ENCRYPTED: u16 = 0x0001;
const DATA_DESCRIPTOR: u16 = 0x0008;
const ZIP64_SIZE: i64 = 0xFFFF_FFFF;

/// Why a port stops reading: Tika's path is left to read the document, or the Java parser
/// fails on it, for the reason given, or spooling an entry fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unread(&'static str),
    Failed(String),
    /// Spooling what an entry decodes to a temporary file fails, as this says.
    Unspooled(String),
}

impl From<Refusal> for Decline<'_> {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unread(why) => Decline::Deferred(why.to_owned()),
            Refusal::Failed(reason) => Decline::Fails(Error::ParseError(reason)),
            Refusal::Unspooled(reason) => Decline::Fails(Error::IoError(reason)),
        }
    }
}

/// The failure of a writing that reads again an entry its check read.
impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unread(why) => {
                Failure::Malformed(format!("an entry its check read is not read again: {why}"))
            }
            Refusal::Failed(reason) => Failure::Malformed(reason),
            Refusal::Unspooled(reason) => Failure::Extraction(Error::IoError(reason)),
        }
    }
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
/// of an entry that inflates past them.
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

/// An archive opened as [`open`] opens it, from bytes whose pages are `pages`, whose entries are
/// read as documents of their own, the pages of their compressed data released as it is
/// decoded.
pub struct Archive<'a> {
    zip: ZipFile<'a>,
    pages: Pages<'a>,
    paging: Paging<'a>,
}

/// An entry an [`Archive`] read to its end ahead of its writing: kept where it spooled to a
/// file, which holds no memory, else read again as it is written.
pub struct Checked {
    entry: ZipEntry,
    kept: Option<Input<'static>>,
}

impl<'a> Archive<'a> {
    /// `new ZipFile(file)` of `input`, whose pages are `pages`.
    pub fn open(input: &'a [u8], pages: Pages<'a>) -> Result<Self, Refusal> {
        Ok(Self {
            zip: open(input)?,
            pages,
            paging: Paging::new(pages),
        })
    }

    pub fn zip(&self) -> &ZipFile<'a> {
        &self.zip
    }

    /// `zip.getInputStream(entry)` read to its end as a document of its own: a stored entry in
    /// place, whose reading releases the archive's pages, a deflated one decoded to a spool, up
    /// to [`decode_limit`] bytes of its compressed data, past which it is left to Tika's path,
    /// which streams it.
    pub fn spool(&self, entry: &ZipEntry) -> Result<Input<'a>, Refusal> {
        match self.decode(entry)? {
            Decoded::Complete(input) => Ok(input),
            Decoded::Oversized(_) => Err(Refusal::Unread("an entry larger than the port reads")),
            Decoded::Truncated(_) => Err(Refusal::Unread("a damaged entry")),
        }
    }

    /// The data of `entry` as [`Archive::spool`] reads it, whether read whole, cut short, or
    /// cut at the limit.
    pub fn decode(&self, entry: &ZipEntry) -> Result<Decoded<Input<'a>>, Refusal> {
        let spooled = self.zip.spool(entry, self.pages);
        if entry.method != STORED {
            self.paging
                .read(usize::try_from(entry.compressed_size).unwrap_or(usize::MAX));
        }
        match spooled {
            Some(Ok(decoded)) => Ok(decoded),
            None => Err(Refusal::Unread("a damaged entry")),
            Some(Err(undecoded)) => Err(Refusal::Unspooled(format!(
                "reading the entry {} fails: {undecoded}",
                entry.name
            ))),
        }
    }

    /// [`Archive::spool`] of `entry` ahead of its writing, which [`Archive::read`] reads.
    pub fn check(&self, entry: &ZipEntry) -> Result<Checked, Refusal> {
        Ok(Self::keep(entry, self.spool(entry)?))
    }

    /// `entry` read ahead into `data`, as [`Archive::spool`] reads it, for [`Archive::read`] to
    /// read: the data kept where it spooled to a file, else let go of.
    pub fn keep(entry: &ZipEntry, data: Input<'_>) -> Checked {
        let kept = match data {
            Input::Mapped(map) => Some(Input::Mapped(map)),
            _ => None,
        };
        Checked {
            entry: entry.clone(),
            kept,
        }
    }

    /// The data of the entry `checked` read ahead: what it kept, or read again.
    pub fn read(&self, checked: &Checked) -> Result<Input<'a>, Refusal> {
        match &checked.kept {
            Some(kept) => Ok(kept.clone()),
            None => self.spool(&checked.entry),
        }
    }
}

impl Checked {
    pub fn entry(&self) -> &ZipEntry {
        &self.entry
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

/// An entry `ZipInputStream` reads from an archive front to back: its name, and where its data
/// lies in the archive, with how it is compressed and what its local header says of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEntry {
    pub name: String,
    method: u16,
    raw: Range<usize>,
    size: i64,
    crc: i64,
}

fn unread_locally() -> Refusal {
    Refusal::Unread("a ZIP entry ZipInputStream may read otherwise")
}

/// The entries `ZipInputStream` reads from `data`, front to back until a header that is not a
/// local file header, by their local headers.
pub fn local_entries(data: &[u8]) -> Result<Vec<LocalEntry>, Refusal> {
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
        let name_at = at + LOCAL_HEADER_BYTES;
        let name = data
            .get(name_at..name_at + name_length)
            .and_then(|name| std::str::from_utf8(name).ok())
            .ok_or_else(unread_locally)?;
        let data_at = name_at + name_length + extra_length;
        let compressed_size = usize::try_from(compressed_size).map_err(|_| unread_locally())?;
        let raw = data_at..data_at + compressed_size;
        if data.get(raw.clone()).is_none() || ![STORED, DEFLATED].contains(&method) {
            return Err(unread_locally());
        }
        at = raw.end;
        entries.push(LocalEntry {
            name: name.to_owned(),
            method,
            raw,
            size,
            crc,
        });
    }
    Ok(entries)
}

/// The content `ZipInputStream` reads of `entry` of `data`, whose pages are `pages`, checked
/// against the size and CRC of its local header: stored data in place, deflated data that
/// inflates to its end, within [`MAX_DECODED`] bytes, using every byte of it, spooled.
pub fn local_content<'a>(
    data: &'a [u8],
    pages: Pages<'a>,
    entry: &LocalEntry,
) -> Result<Input<'a>, Refusal> {
    let raw = &data[entry.raw.clone()];
    let mut crc = flate2::Crc::new();
    let content = match entry.method {
        STORED => {
            crc.update(raw);
            Input::Within(raw, pages)
        }
        _ => {
            let mut inflater = flate2::bufread::DeflateDecoder::new(raw);
            let mut spool = Spool::new(u64::try_from(entry.size).ok());
            let mut chunk = vec![0; 64 << 10];
            let mut length = 0;
            loop {
                let read = inflater.read(&mut chunk).map_err(|_| unread_locally())?;
                if read == 0 {
                    break;
                }
                length += read;
                if length > MAX_DECODED {
                    return Err(unread_locally());
                }
                crc.update(&chunk[..read]);
                spool
                    .write_all(&chunk[..read])
                    .map_err(|error| unspooled(entry, &error))?;
            }
            if inflater.total_in() != raw.len() as u64 {
                return Err(unread_locally());
            }
            spool.finish().map_err(|error| unspooled(entry, &error))?
        }
    };
    if i64::try_from(content.len()) != Ok(entry.size) || i64::from(crc.sum()) != entry.crc {
        return Err(unread_locally());
    }
    Ok(content)
}

fn unspooled(entry: &LocalEntry, error: &std::io::Error) -> Refusal {
    Refusal::Unspooled(format!("reading the entry {} fails: {error}", entry.name))
}
