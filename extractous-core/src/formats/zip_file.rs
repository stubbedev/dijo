//! A ZIP archive as Tika opens one with commons-compress's `ZipFile`: its entries in the order of
//! the central directory, named in UTF-8, the first of a name found by that name. Where the zip
//! crate reads an archive differently, as it does with several entries of one name, a parser can
//! tell and leave the document to Tika.

use std::io::{Cursor, Read};

use zip::{CompressionMethod, ZipArchive};

use crate::formats::Failure;

const CENTRAL_DIRECTORY_SIGNATURE: &[u8] = b"PK\x01\x02";
const CENTRAL_DIRECTORY_HEADER_LENGTH: usize = 46;

/// An entry of the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZipEntry {
    index: usize,
    /// The name as commons-compress decodes it, in UTF-8.
    pub(crate) name: String,
    /// The name as stored.
    pub(crate) raw_name: Vec<u8>,
}

/// An opened archive.
pub(crate) struct ZipFile<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    entries: Vec<ZipEntry>,
}

impl<'a> ZipFile<'a> {
    /// Opens `input`, an archive `what` names in errors.
    pub(crate) fn open(input: &'a [u8], what: &str) -> Result<Self, Failure> {
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
        Ok(Self { zip, entries })
    }

    /// The entries in the order of the central directory, as `ZipFile.getEntries` lists them.
    pub(crate) fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// The first entry named `name`, as `ZipFile.getEntry` finds it.
    pub(crate) fn named(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// The uncompressed content of `entry`.
    pub(crate) fn read(&mut self, entry: &ZipEntry) -> Result<Vec<u8>, Failure> {
        let unreadable = |e: &dyn std::fmt::Display| {
            Failure::Malformed(format!("{} cannot be read: {e}", entry.name))
        };
        let mut file = self.zip.by_index(entry.index).map_err(|e| unreadable(&e))?;
        if !matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(unreadable(&format!(
                "it is compressed with {:?}",
                file.compression()
            )));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| unreadable(&e))?;
        Ok(bytes)
    }

    /// Whether every entry is stored or deflated and not encrypted, so that the zip crate reads
    /// what `ZipFile.canReadEntryData` lets commons-compress read.
    pub(crate) fn reads_every_entry(&self) -> bool {
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
    pub(crate) fn has_every_record(&self, input: &[u8]) -> bool {
        let Ok(mut position) = usize::try_from(self.zip.central_directory_start()) else {
            return false;
        };
        let mut records = 0;
        while input.get(position..position + CENTRAL_DIRECTORY_SIGNATURE.len())
            == Some(CENTRAL_DIRECTORY_SIGNATURE)
        {
            let Some(header) = input.get(position..position + CENTRAL_DIRECTORY_HEADER_LENGTH)
            else {
                return false;
            };
            let length = |at: usize| usize::from(u16::from_le_bytes([header[at], header[at + 1]]));
            position += CENTRAL_DIRECTORY_HEADER_LENGTH + length(28) + length(30) + length(32);
            records += 1;
        }
        records == self.entries.len()
    }
}
