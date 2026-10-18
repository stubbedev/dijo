//! Tika's `PackageParser` over TAR, ar and cpio archives: each file but a directory handed to the
//! embedded document extractor with the metadata `handleEntryMetadata` gives it, in archive
//! order. The archive is read as the format its detected type names, and keeps that type; Tika
//! reads it as the format `ArchiveStreamFactory` recognizes in its bytes and sets that format's
//! type, but for GNU TAR (#101).

use super::nesting::Scan;
use super::{undecoded, within_limit, write_files, Cut};
use crate::compress::archiver::Archiver;
use crate::compress::{ar, cpio, tar, ArchiveFile};
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Parser, Prepared};
use crate::mime::MediaType;

const GTAR: &str = "application/x-gtar";

/// The format `content_type` names.
fn named(content_type: &str) -> Option<Archiver> {
    match MediaType::parse(content_type)?.base_type().as_str() {
        "application/x-tar" | GTAR => Some(Archiver::Tar),
        "application/x-archive" => Some(Archiver::Ar),
        "application/x-cpio" => Some(Archiver::Cpio),
        _ => None,
    }
}

/// The format `content_type` names, else the one `ArchiveStreamFactory` recognizes in `input`,
/// and the files its reader gives, or why the native readers do not read them.
fn read(
    input: &[u8],
    content_type: Option<&str>,
) -> Result<(Vec<ArchiveFile>, Option<Cut>), Decline<'static>> {
    let archiver = content_type
        .and_then(named)
        .or_else(|| Archiver::detect(input))
        .ok_or_else(|| "commons-compress recognizes no archive in it".to_owned())?;
    let files = match archiver {
        Archiver::Tar => tar::files(input),
        Archiver::Ar => ar::files(input),
        Archiver::Cpio => cpio::files(input),
        other => {
            return Err(Decline::Deferred(format!(
                "commons-compress reads it as {other:?}, which PackageParser's port does not"
            )))
        }
    };
    let what = format!("the {archiver:?} archive");
    match files {
        Ok(files) => within_limit(what, files),
        Err(reason) => Err(undecoded(&what, reason)),
    }
}

/// Tika's `PackageParser`.
pub struct PackageParser;

impl Parser for PackageParser {
    fn name(&self) -> &'static str {
        "extractous::PackageParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.PackageParser"
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
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'static>> {
        let (files, cut) = read(input, metadata.get("Content-Type"))?;
        if let Some(reason) = Scan::new().files(input.len(), &files) {
            return Err(Decline::Deferred(reason));
        }
        Ok(Box::new(move |xhtml| {
            let written = write_files(&files, xhtml).and_then(|()| Cut::end(cut));
            xhtml.end_document()?;
            written
        }))
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
        assert!(old_cpio.contains("reading it fails"), "{old_cpio}");
    }
}
