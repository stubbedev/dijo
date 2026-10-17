//! Tika's `ZipSalvager.salvageCopy`: an archive `ZipFile` cannot open read front to back by its
//! local headers, as `ZipArchiveInputStream` reads it, and each entry it can read copied, by
//! its name alone, into a new archive, the last one as far as its data goes, which `ZipFile`
//! then opens.
//!
//! The new archive stores its entries rather than deflating them, as commons-compress's
//! `ZipArchiveOutputStream` does, and gives them no time, where that stream gives each the time
//! of the copy: what differs is only in the entries' own metadata, which no extraction shows.

use super::stream::{self, ZipStream};
use super::Content;
use crate::compress::crc32;

/// An entry copied into the salvaged archive: its name and data.
struct Copied {
    name: String,
    data: Vec<u8>,
}

/// Why an archive is not salvaged as Tika salvages it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unsalvaged {
    /// `salvageCopy` throws: Tika reads the archive entry by entry instead.
    Thrown,
    /// What the port does not read as commons-compress does.
    Unported(String),
}

/// What reading the archive front to back with a stream that does or does not allow stored
/// entries with a data descriptor ends in.
enum Pass {
    Copied(Vec<Copied>),
    /// A stored entry with a data descriptor, which the stream does not allow.
    DataDescriptor,
}

/// The salvaged archive of `data`.
pub(crate) fn salvage(data: &[u8]) -> Result<Vec<u8>, Unsalvaged> {
    let copied = match copy(data, false)? {
        Pass::Copied(copied) => copied,
        Pass::DataDescriptor => match copy(data, true)? {
            Pass::Copied(copied) => copied,
            Pass::DataDescriptor => return Err(Unsalvaged::Thrown),
        },
    };
    Ok(archive(&copied))
}

/// `processZAE` over a `ZipArchiveInputStream` that reads names as UTF-8, without their
/// Unicode extra fields: the entries copied, up to the first whose data ends early, or to where
/// the stream fails, which `salvageCopy` swallows, but for a data descriptor, which it retries
/// for, and an unsupported feature of the first entry, which it throws on.
fn copy(data: &[u8], allow_stored_data_descriptor: bool) -> Result<Pass, Unsalvaged> {
    let mut zip = ZipStream::new(data, allow_stored_data_descriptor).without_unicode_extra_fields();
    let mut copied = Vec::new();
    let mut first = true;
    loop {
        let next = zip.next_entry();
        let entry = match next {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(stream::Failure::DataDescriptor) => return Ok(Pass::DataDescriptor),
            Err(stream::Failure::Method(method)) => return Err(unported(method)),
            Err(stream::Failure::Unsupported) if first => return Err(Unsalvaged::Thrown),
            Err(stream::Failure::Unsupported | stream::Failure::Io) => break,
        };
        first = false;
        if entry.name.ends_with('/') {
            continue;
        }
        match zip.content() {
            Ok(Content::Complete(data)) => copied.push(Copied {
                name: entry.name,
                data,
            }),
            Ok(Content::Truncated(data)) => {
                copied.push(Copied {
                    name: entry.name,
                    data,
                });
                break;
            }
            Ok(Content::Oversized(_)) => {
                return Err(Unsalvaged::Unported(format!(
                    "the entry {} inflates past what the port holds",
                    entry.name
                )))
            }
            Err(stream::Failure::Unsupported | stream::Failure::DataDescriptor) => {}
            Err(stream::Failure::Method(method)) => return Err(unported(method)),
            Err(stream::Failure::Io) => {
                return Err(Unsalvaged::Unported(format!(
                    "the entry {} the stream cannot read, which commons-compress copies as far \
                     as it reads",
                    entry.name
                )))
            }
        }
    }
    Ok(Pass::Copied(copied))
}

fn unported(method: u16) -> Unsalvaged {
    Unsalvaged::Unported(format!(
        "an entry compressed with method {method}, which the port does not decompress"
    ))
}

/// The bytes of an archive storing `entries`, their names in UTF-8, flagged so, as made on a
/// FAT file system.
fn archive(entries: &[Copied]) -> Vec<u8> {
    const VERSION: u16 = 20;
    const UTF8_FLAG: u16 = 0x0800;
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let offset = out.len() as u32;
        let name = entry.name.as_bytes();
        let crc = crc32(&entry.data);
        let size = entry.data.len() as u32;
        let mut common = Vec::with_capacity(26);
        common.extend_from_slice(&VERSION.to_le_bytes());
        common.extend_from_slice(&UTF8_FLAG.to_le_bytes());
        common.extend_from_slice(&super::STORED.to_le_bytes());
        common.extend_from_slice(&[0; 4]);
        common.extend_from_slice(&crc.to_le_bytes());
        common.extend_from_slice(&size.to_le_bytes());
        common.extend_from_slice(&size.to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&super::LFH_SIG);
        out.extend_from_slice(&common);
        out.extend_from_slice(name);
        out.extend_from_slice(&entry.data);
        central.extend_from_slice(&super::CFH_SIG);
        central.extend_from_slice(&VERSION.to_le_bytes());
        central.extend_from_slice(&common);
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let central_offset = out.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::zip::file::tests::zip;
    use crate::compress::zip::file::ZipFile;

    #[test]
    fn copies_what_the_stream_reads_into_an_archive_zip_file_opens() {
        let data = zip(&[("a.txt", b"hej"), ("d/", b""), ("b.txt", b"med dig")]);
        let cut = &data[..data.len() - 30];
        assert!(ZipFile::open(cut).is_err());
        let salvaged = salvage(cut).unwrap();
        let zip = ZipFile::open(&salvaged).unwrap();
        let names: Vec<&str> = zip
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["a.txt", "b.txt"]);
        let read = |index: usize| zip.read(&zip.entries()[index]).unwrap();
        assert_eq!(read(0), Content::Complete(b"hej".to_vec()));
        assert_eq!(read(1), Content::Complete(b"med dig".to_vec()));
    }
}
