//! Tika's `PackageParser` over the TAR, ar and cpio archives commons-compress's
//! `ArchiveStreamFactory` recognizes: each file but a directory handed to the embedded document
//! extractor with the metadata `handleEntryMetadata` gives it, in archive order.

use super::nesting::Scan;
use super::write_files;
use crate::compress::archiver::Archiver;
use crate::compress::{ar, cpio, tar, ArchiveFile};
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};
use crate::mime::MediaType;

const GTAR: &str = "application/x-gtar";

/// The format `ArchiveStreamFactory` recognizes in `input` and the files its archive input
/// stream gives, or why the native readers do not read them.
fn read(input: &[u8]) -> Result<(Archiver, Vec<ArchiveFile>), String> {
    let archiver = Archiver::detect(input)
        .ok_or_else(|| "commons-compress recognizes no archive in it".to_owned())?;
    let files = match archiver {
        Archiver::Tar => tar::files(input),
        Archiver::Ar => ar::files(input),
        Archiver::Cpio => cpio::files(input),
        other => {
            return Err(format!(
                "commons-compress reads it as {other:?}, which PackageParser's port does not"
            ))
        }
    };
    files
        .map(|files| (archiver, files))
        .map_err(|undecoded| format!("the {archiver:?} archive: {undecoded}"))
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

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let (archiver, files) = read(input)?;
        if let Some(reason) = Scan::new().files(input.len(), &files) {
            return Err(Declined::Deferred(reason));
        }
        Ok(Box::new(move |xhtml| {
            update_media_type(archiver, xhtml.metadata_mut());
            let written = write_files(&files, xhtml);
            xhtml.end_document()?;
            written
        }))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::cpio::tests::cpio;
    use crate::compress::tar_header::tests::tar;
    use crate::compress::zip::file::tests::zip;

    fn defers(input: &[u8]) -> Option<String> {
        PackageParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
    }

    #[test]
    fn defers_what_the_native_readers_do_not_read_as_commons_compress_does() {
        assert_eq!(defers(&tar(&[(b"a.txt", b'0', b"text")])), None);
        assert_eq!(defers(&cpio(&[("a.txt", b"text")])), None);
        let data = tar(&[(b"a.txt", b'0', b"text")]);
        let cut = defers(&data[..515]).unwrap();
        assert!(cut.contains("Tar archive"), "{cut}");
        let zip = defers(&zip(&[("a", b"text")])).unwrap();
        assert!(zip.contains("Zip"), "{zip}");
        let none = defers(b"plain text").unwrap();
        assert!(none.contains("no archive"), "{none}");
        let old_cpio = defers(b"070707000000000000000000000000000000000000000000").unwrap();
        assert!(old_cpio.contains("old ASCII"), "{old_cpio}");
    }
}
