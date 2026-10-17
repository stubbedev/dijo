//! A ZIP archive as Tika opens one with commons-compress's `ZipFile`: its entries in the order of
//! the central directory, named in UTF-8, the first of a name found by that name. Where the zip
//! crate reads an archive differently, as it does with several entries of one name, a parser can
//! tell and leave the document to Tika.

use std::io::{self, Cursor};

use zip::{CompressionMethod, ZipArchive};

use crate::formats::Failure;
use crate::input::{Input, Pages, Paging, Spool};

const CENTRAL_DIRECTORY_SIGNATURE: &[u8] = b"PK\x01\x02";
const CENTRAL_DIRECTORY_HEADER_LENGTH: usize = 46;

/// An entry of the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    index: usize,
    /// The name as commons-compress decodes it, in UTF-8.
    pub name: String,
    /// The name as stored.
    pub raw_name: Vec<u8>,
}

/// An opened archive.
pub struct ZipFile<'a> {
    input: &'a [u8],
    pages: Pages<'a>,
    zip: ZipArchive<Cursor<&'a [u8]>>,
    entries: Vec<ZipEntry>,
}

impl<'a> ZipFile<'a> {
    /// Opens `input`, an archive `what` names in errors.
    pub fn open(input: &'a [u8], pages: Pages<'a>, what: &str) -> Result<Self, Failure> {
        let zip = ZipArchive::new(Cursor::new(input))
            .map_err(|e| Failure::Malformed(format!("The {what} archive cannot be read: {e}")))?;
        let entries = (0..zip.len())
            .map(|index| {
                let entry = zip.by_index_data(index).map_err(|e| {
                    Failure::Malformed(format!(
                        "Entry {index} of the {what} archive cannot be read: {e}"
                    ))
                })?;
                let raw_name = entry.name_raw().to_vec();
                Ok(ZipEntry {
                    index,
                    name: String::from_utf8_lossy(&raw_name).into_owned(),
                    raw_name,
                })
            })
            .collect::<Result<_, Failure>>()?;
        Ok(Self {
            input,
            pages,
            zip,
            entries,
        })
    }

    /// The entries in the order of the central directory, as `ZipFile.getEntries` lists them.
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// The first entry named `name`, as `ZipFile.getEntry` finds it.
    pub fn named(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// The uncompressed content of `entry`: the archive's own bytes where it is stored.
    pub fn read(&mut self, entry: &ZipEntry) -> Result<Input<'a>, Failure> {
        let unreadable = |e: &dyn std::fmt::Display| {
            Failure::Malformed(format!("{} cannot be read: {e}", entry.name))
        };
        let compression = self
            .zip
            .by_index_raw(entry.index)
            .map_err(|e| unreadable(&e))?
            .compression();
        if !matches!(
            compression,
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(unreadable(&format!(
                "it is compressed with {compression:?}"
            )));
        }
        read_entry(&mut self.zip, self.input, self.pages, entry.index).map_err(|e| unreadable(&e))
    }

    /// Whether every entry is stored or deflated and not encrypted, so that the zip crate reads
    /// what `ZipFile.canReadEntryData` lets commons-compress read.
    pub fn reads_every_entry(&self) -> bool {
        self.entries.iter().all(|entry| {
            self.zip.by_index_data(entry.index).is_ok_and(|data| {
                !data.encrypted()
                    && matches!(
                        data.compression(),
                        CompressionMethod::Stored | CompressionMethod::Deflated
                    )
            })
        })
    }

    /// Whether every record of the central directory is an entry: the zip crate keeps one of
    /// several entries of the same name, where commons-compress keeps them all.
    pub fn has_every_record(&self, input: &[u8]) -> bool {
        central_directory_records(input, self.zip.central_directory_start())
            == Some(self.entries.len())
    }
}

/// The uncompressed content of the entry `index` of `archive`, which reads `input`, whose pages
/// are `pages`: the archive's own bytes where the entry is stored, checked against its CRC as the zip crate
/// checks what it reads, so that reading a large archive copies none of its stored data; an
/// inflated entry spooled as a stream is, to a file of its own past what is read whole.
pub fn read_entry<'a>(
    archive: &mut ZipArchive<Cursor<&'a [u8]>>,
    input: &'a [u8],
    pages: Pages<'a>,
    index: usize,
) -> Result<Input<'a>, String> {
    let raw = archive.by_index_raw(index).map_err(|e| e.to_string())?;
    if raw.compression() == CompressionMethod::Stored && !raw.encrypted() {
        let start = raw
            .data_start()
            .and_then(|start| usize::try_from(start).ok());
        let length = usize::try_from(raw.compressed_size()).ok();
        let stored = start
            .zip(length)
            .and_then(|(start, length)| input.get(start..start.checked_add(length)?))
            .ok_or("its data lies outside the archive")?;
        let paging = Paging::new(pages);
        let mut crc = flate2::Crc::new();
        paging.scan(stored, |window| crc.update(window));
        if crc.sum() != raw.crc32() {
            return Err("Invalid checksum".to_owned());
        }
        return Ok(Input::Within(stored, pages));
    }
    drop(raw);
    let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
    let mut spool = Spool::new(Some(file.size()));
    io::copy(&mut file, &mut spool).map_err(|e| e.to_string())?;
    spool.finish().map_err(|e| e.to_string())
}

/// How many records the central directory starting at `start` in `input` holds, or `None` where
/// one is cut short. The zip crate lists one entry for several records of one name, which this
/// count tells apart.
pub fn central_directory_records(input: &[u8], start: u64) -> Option<usize> {
    let mut position = usize::try_from(start).ok()?;
    let mut records = 0;
    while input.get(position..position + CENTRAL_DIRECTORY_SIGNATURE.len())
        == Some(CENTRAL_DIRECTORY_SIGNATURE)
    {
        let header = input.get(position..position + CENTRAL_DIRECTORY_HEADER_LENGTH)?;
        let length = |at: usize| usize::from(u16::from_le_bytes([header[at], header[at + 1]]));
        position += CENTRAL_DIRECTORY_HEADER_LENGTH + length(28) + length(30) + length(32);
        records += 1;
    }
    Some(records)
}
