//! Tika's `ZipSalvager.salvageCopy`: an archive `ZipFile` cannot open read front to back by its
//! local headers, as `ZipArchiveInputStream` reads it, and each entry it can read copied, by
//! its name alone, into a new archive, the last one as far as its data goes, which `ZipFile`
//! then opens.
//!
//! The new archive stores its entries rather than deflating them, as commons-compress's
//! `ZipArchiveOutputStream` does, and gives them no time, where that stream gives each the time
//! of the copy: what differs is only in the entries' own metadata, which no extraction shows.

use std::io::{self, Write};

use super::stream::{self, ZipStream};
use super::Decoded;
use crate::compress::crc32;
use crate::input::{Input, Pages, Spool};

/// Why an archive is not salvaged as Tika salvages it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsalvaged {
    /// `salvageCopy` throws: Tika reads the archive entry by entry instead.
    Thrown,
    /// What the port does not read as commons-compress does.
    Unported(String),
    /// Spooling an entry or the salvaged archive fails, for this reason.
    Unspooled(String),
}

/// What reading the archive front to back with a stream that does or does not allow stored
/// entries with a data descriptor ends in.
enum Pass {
    Copied(Input<'static>),
    /// A stored entry with a data descriptor, which the stream does not allow.
    DataDescriptor,
}

/// The salvaged archive of `data`, spooled: in memory as far as a file is read whole, mapped
/// from a temporary file past that.
pub fn salvage(data: &[u8]) -> Result<Input<'static>, Unsalvaged> {
    match copy(data, false)? {
        Pass::Copied(salvaged) => Ok(salvaged),
        Pass::DataDescriptor => match copy(data, true)? {
            Pass::Copied(salvaged) => Ok(salvaged),
            Pass::DataDescriptor => Err(Unsalvaged::Thrown),
        },
    }
}

/// `processZAE` over a `ZipArchiveInputStream` that reads names as UTF-8, without their
/// Unicode extra fields: the entries copied, each as it is read, up to the first whose data
/// ends early, or to where the stream fails, which `salvageCopy` swallows, but for a data
/// descriptor, which it retries for, and an unsupported feature of the first entry, which it
/// throws on.
fn copy(data: &[u8], allow_stored_data_descriptor: bool) -> Result<Pass, Unsalvaged> {
    let mut zip = ZipStream::new(data, allow_stored_data_descriptor).without_unicode_extra_fields();
    let mut archive = Archive::new(data.len());
    let mut first = true;
    loop {
        let next = zip.next_entry();
        let entry = match next {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(stream::Failure::DataDescriptor) => return Ok(Pass::DataDescriptor),
            Err(stream::Failure::Unsupported) if first => return Err(Unsalvaged::Thrown),
            Err(stream::Failure::Unsupported | stream::Failure::Io) => break,
        };
        first = false;
        if entry.name.ends_with('/') {
            continue;
        }
        match zip.spool(Pages::IN_MEMORY) {
            Ok(Ok(Decoded::Complete(content))) => archive.add(&entry.name, &content)?,
            Ok(Ok(Decoded::Truncated(content))) => {
                archive.add(&entry.name, &content)?;
                break;
            }
            Ok(Ok(Decoded::Oversized(_))) => {
                return Err(Unsalvaged::Unported(format!(
                    "the entry {} inflates past what the port holds",
                    entry.name
                )))
            }
            Ok(Err(undecoded)) => {
                return Err(Unsalvaged::Unspooled(format!(
                    "the entry {}: {undecoded}",
                    entry.name
                )))
            }
            Err(stream::Failure::Unsupported | stream::Failure::DataDescriptor) => {}
            Err(stream::Failure::Io) => {
                return Err(Unsalvaged::Unported(format!(
                    "the entry {} the stream cannot read, which commons-compress copies as far \
                     as it reads",
                    entry.name
                )))
            }
        }
    }
    archive.finish().map(Pass::Copied)
}

/// A salvaged archive too large for the fields of an archive without ZIP64 records, which
/// commons-compress adds as it needs them and the port does not write.
fn needs_zip64(what: &str) -> Unsalvaged {
    Unsalvaged::Unported(format!(
        "a salvaged archive whose {what} needs ZIP64 records, which the port does not write"
    ))
}

fn unspooled(error: io::Error) -> Unsalvaged {
    Unsalvaged::Unspooled(format!("spooling the salvaged archive fails: {error}"))
}

/// An archive storing entries, their names in UTF-8, flagged so, as made on a FAT file system,
/// as it is written: each entry's local header and data to a spool as the entry is added, the
/// central directory held until the archive ends.
struct Archive {
    out: Spool,
    written: u64,
    central: Vec<u8>,
    count: u16,
}

impl Archive {
    const VERSION: u16 = 20;
    const UTF8_FLAG: u16 = 0x0800;

    /// An archive salvaged from one of `damaged` bytes, which it is about as large as: it
    /// stores the entries the damaged one holds, as far as it holds them.
    fn new(damaged: usize) -> Self {
        Self {
            out: Spool::new(Some(damaged as u64)),
            written: 0,
            central: Vec::new(),
            count: 0,
        }
    }

    /// Stores `data` as the entry `name`.
    fn add(&mut self, name: &str, data: &[u8]) -> Result<(), Unsalvaged> {
        let offset = u32::try_from(self.written).map_err(|_| needs_zip64("size"))?;
        let size = u32::try_from(data.len()).map_err(|_| needs_zip64("entry size"))?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| needs_zip64("entry count"))?;
        let name = name.as_bytes();
        let name_length = u16::try_from(name.len()).map_err(|_| needs_zip64("entry name"))?;
        let mut common = Vec::with_capacity(26);
        common.extend_from_slice(&Self::VERSION.to_le_bytes());
        common.extend_from_slice(&Self::UTF8_FLAG.to_le_bytes());
        common.extend_from_slice(&super::STORED.to_le_bytes());
        common.extend_from_slice(&[0; 4]);
        common.extend_from_slice(&crc32(data).to_le_bytes());
        common.extend_from_slice(&size.to_le_bytes());
        common.extend_from_slice(&size.to_le_bytes());
        common.extend_from_slice(&name_length.to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());
        self.write(&[&super::LFH_SIG, &common, name, data])?;
        self.central.extend_from_slice(&super::CFH_SIG);
        self.central.extend_from_slice(&Self::VERSION.to_le_bytes());
        self.central.extend_from_slice(&common);
        self.central.extend_from_slice(&[0; 10]);
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name);
        Ok(())
    }

    /// The archive, once its central directory and the end of it are written.
    fn finish(mut self) -> Result<Input<'static>, Unsalvaged> {
        let central_offset = u32::try_from(self.written).map_err(|_| needs_zip64("size"))?;
        let central = std::mem::take(&mut self.central);
        let central_length =
            u32::try_from(central.len()).map_err(|_| needs_zip64("central directory"))?;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(b"PK\x05\x06");
        end.extend_from_slice(&[0; 4]);
        end.extend_from_slice(&self.count.to_le_bytes());
        end.extend_from_slice(&self.count.to_le_bytes());
        end.extend_from_slice(&central_length.to_le_bytes());
        end.extend_from_slice(&central_offset.to_le_bytes());
        end.extend_from_slice(&[0; 2]);
        self.write(&[&central, &end])?;
        self.out.finish().map_err(unspooled)
    }

    fn write(&mut self, pieces: &[&[u8]]) -> Result<(), Unsalvaged> {
        for piece in pieces {
            self.out.write_all(piece).map_err(unspooled)?;
            self.written += piece.len() as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::zip::file::tests::zip;
    use crate::compress::zip::file::ZipFile;
    use crate::compress::zip::Content;

    #[test]
    fn copies_what_the_stream_reads_into_an_archive_zip_file_opens() {
        let data = zip(&[("a.txt", b"hej"), ("d/", b""), ("b.txt", b"med dig")]);
        let cut = &data[..data.len() - 30];
        assert!(ZipFile::open(cut).is_err());
        let salvaged = salvage(cut).unwrap();
        assert!(!salvaged.is_mapped());
        let zip = ZipFile::open(&salvaged).unwrap();
        let names: Vec<&str> = zip
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["a.txt", "b.txt"]);
        let read = |index: usize| zip.read(&zip.entries()[index]).unwrap();
        assert_eq!(read(0), Content::Complete(b"hej"[..].into()));
        assert_eq!(read(1), Content::Complete(b"med dig"[..].into()));
    }
}
