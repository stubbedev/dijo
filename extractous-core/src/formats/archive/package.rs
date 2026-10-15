//! Tika's `PackageParser` over the TAR, ar and cpio archives commons-compress's
//! `ArchiveStreamFactory` recognizes: each entry but a directory handed to the embedded document
//! extractor with the metadata `handleEntryMetadata` gives it, in archive order.

use super::nesting::Scan;
use super::write_entry_div;
use crate::compress::ar::ArStream;
use crate::compress::archiver::Archiver;
use crate::compress::cpio::CpioStream;
use crate::compress::tar::{TarStream, Unread};
use crate::config::ParserConfigs;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::java_time::format_instant;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};
use crate::mime::MediaType;

const GTAR: &str = "application/x-gtar";

/// An entry as `PackageParser` hands it on.
struct PackageEntry<'a> {
    name: String,
    modified_millis: i64,
    size: i64,
    directory: bool,
    /// The data, as reading it to its end gives it: none for a directory, which is not read.
    content: &'a [u8],
}

/// The archive input stream `ArchiveStreamFactory` opens for a format the port reads.
enum Archive<'a> {
    Tar(TarStream<'a>),
    Ar(ArStream<'a>),
    Cpio(CpioStream<'a>),
}

impl<'a> Archive<'a> {
    /// The stream `createArchiveInputStream` opens for `input`, or why the port does not read it.
    fn open(input: &'a [u8]) -> Result<(Archiver, Archive<'a>), String> {
        match Archiver::detect(input) {
            Some(Archiver::Tar) => Ok((Archiver::Tar, Archive::Tar(TarStream::new(input)))),
            Some(Archiver::Ar) => Ok((Archiver::Ar, Archive::Ar(ArStream::new(input)))),
            Some(Archiver::Cpio) => Ok((Archiver::Cpio, Archive::Cpio(CpioStream::new(input)))),
            Some(other) => Err(format!(
                "commons-compress reads it as {other:?}, which the port does not"
            )),
            None => Err("commons-compress recognizes no archive in it".to_owned()),
        }
    }

    /// `getNextEntry`: `None` at the end of the archive, or why the port does not read on.
    fn next(&mut self) -> Result<Option<PackageEntry<'a>>, String> {
        match self {
            Archive::Tar(stream) => {
                let Some(entry) = stream.next_entry().map_err(|unread| match unread {
                    Unread::Throws(reason) => format!("commons-compress throws: {reason}"),
                    Unread::Sparse => "an entry is a sparse file".to_owned(),
                    Unread::NonAsciiNumber => {
                        "a PAX number holds characters beyond ASCII".to_owned()
                    }
                })?
                else {
                    return Ok(None);
                };
                if entry.header.sparse {
                    return Err("an entry is a sparse file".to_owned());
                }
                let directory = entry.header.is_directory();
                let content = if directory {
                    &[][..]
                } else {
                    stream
                        .content(&entry)
                        .map_err(|_| "Truncated TAR archive".to_owned())?
                };
                Ok(Some(PackageEntry {
                    name: entry.header.name,
                    modified_millis: entry.header.modified_millis,
                    size: entry.header.size,
                    directory,
                    content,
                }))
            }
            Archive::Ar(stream) => {
                let Some(entry) = stream
                    .next_entry()
                    .map_err(|e| format!("commons-compress throws: {}", e.0))?
                else {
                    return Ok(None);
                };
                let content = stream.content(&entry);
                Ok(Some(PackageEntry {
                    name: entry.name,
                    modified_millis: entry.modified_millis,
                    size: entry.length,
                    directory: false,
                    content,
                }))
            }
            Archive::Cpio(stream) => {
                let Some(entry) = stream
                    .next_entry()
                    .map_err(|e| format!("commons-compress throws: {}", e.0))?
                else {
                    return Ok(None);
                };
                let directory = entry.is_directory();
                let content = if directory {
                    &[][..]
                } else {
                    stream
                        .content(&entry)
                        .map_err(|e| format!("commons-compress throws: {}", e.0))?
                };
                Ok(Some(PackageEntry {
                    name: entry.name,
                    modified_millis: entry.modified_millis,
                    size: entry.size,
                    directory,
                    content,
                }))
            }
        }
    }
}

/// Tika's `PackageParser`.
pub(crate) struct PackageParser;

impl Parser for PackageParser {
    fn name(&self) -> &'static str {
        "extractous::PackageParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/x-tar",
            GTAR,
            "application/x-archive",
            "application/x-cpio",
        ]
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        let mut archive = match Archive::open(input) {
            Ok((_, archive)) => archive,
            Err(reason) => return Some(reason),
        };
        let mut scan = Scan::new();
        loop {
            match archive.next() {
                Ok(Some(entry)) if entry.directory => {}
                Ok(Some(entry)) => {
                    if let Some(reason) = scan.entry(entry.content) {
                        return Some(reason);
                    }
                }
                Ok(None) => return scan.document(input.len()),
                Err(reason) => return Some(reason),
            }
        }
    }

    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let (archiver, mut archive) = Archive::open(input).map_err(Failure::Malformed)?;
        update_media_type(archiver, xhtml.metadata_mut());
        let written = write_entries(&mut archive, xhtml);
        xhtml.end_document()?;
        written
    }
}

/// The loop over the entries, which the document ends after whatever happens.
fn write_entries(archive: &mut Archive, xhtml: &mut Xhtml) -> Result<(), Failure> {
    while let Some(entry) = archive.next().map_err(Failure::Malformed)? {
        if entry.directory {
            continue;
        }
        let metadata = entry_metadata(&entry, xhtml)?;
        xhtml.parse_embedded(entry.content, &metadata, Wrapping::PackageEntry)?;
    }
    Ok(())
}

/// `updateMediaType`: the format's type, unless the archive was detected as a GNU TAR archive.
fn update_media_type(archiver: Archiver, metadata: &mut TikaMetadata) {
    let gtar = metadata
        .get("Content-Type")
        .and_then(MediaType::parse)
        .is_some_and(|incoming| incoming.as_str() == GTAR);
    if !gtar {
        metadata.set("Content-Type", archiver.media_type());
    }
}

/// `handleEntryMetadata`: the modification date, length and name, with the entry's div written
/// for a named one.
fn entry_metadata(entry: &PackageEntry, xhtml: &mut Xhtml) -> Result<TikaMetadata, Failure> {
    let mut metadata = TikaMetadata::new();
    metadata.set("dcterms:modified", format_instant(entry.modified_millis));
    metadata.set("Content-Length", entry.size.to_string());
    if !entry.name.is_empty() {
        let name = entry.name.replace('\\', "/");
        metadata.set(RESOURCE_NAME, name.clone());
        metadata.set(INTERNAL_PATH, name.clone());
        write_entry_div(xhtml, &name)?;
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::tar::tests::tar;
    use crate::compress::zip::file::tests::zip;

    fn defers(input: &[u8]) -> Option<String> {
        PackageParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
    }

    #[test]
    fn defers_what_the_port_does_not_read_as_commons_compress_does() {
        assert_eq!(defers(&tar(&[(b"a.txt", b'0', b"text")])), None);
        let sparse = defers(&tar(&[(b"sparse", b'S', b"")])).unwrap();
        assert!(sparse.contains("sparse"), "{sparse}");
        let data = tar(&[(b"a.txt", b'0', b"text")]);
        let cut = defers(&data[..515]).unwrap();
        assert!(cut.contains("Truncated"), "{cut}");
        let zip = defers(&zip(&[("a", b"text")])).unwrap();
        assert!(zip.contains("Zip"), "{zip}");
        let none = defers(b"plain text").unwrap();
        assert!(none.contains("no archive"), "{none}");
    }
}
