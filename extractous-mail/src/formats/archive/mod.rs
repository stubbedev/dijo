//! Archives and compressed files as Tika 4.1's package module reads them, each entry handed to
//! the embedded document extractor: ZIP archives with `ZipParser` ([`zip`]), compressed files
//! with `CompressorParser` ([`compressor`]), TAR, ar and cpio archives with `PackageParser`
//! ([`package`]), 7z archives with `SevenZParser` ([`seven_z`]) and RAR archives with
//! `RarParser` ([`rar`]).

mod compressor;
mod nesting;
mod package;
mod rar;
mod seven_z;
mod zip;

pub use compressor::CompressorParser;
pub use package::PackageParser;
pub use rar::RarParser;
pub use seven_z::SevenZParser;
pub use zip::ZipParser;

use crate::compress::ArchiveFile;
use crate::formats::embedded::{record_stream_exception, Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;
use crate::java::time::format_instant;

/// The metadata of the ZIP integrity check.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
const INTEGRITY_CHECK: [&str; 4] = [
    "zip:integrity-check-result",
    "zip:duplicate-entry-names",
    "zip:local-header-only-entries",
    "zip:central-directory-only-entries",
];

/// A native ZIP result as Tika's path writes it, with the integrity check as late as Tika runs
/// it (#99): after the document has ended, so not in its head, and not at all when the parse
/// failed, as at the write limit.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn as_tika_checks_zips(
    text: &mut String,
    metadata: &mut crate::formats::parity::ComparedMetadata,
    tika: &crate::formats::parity::ComparedMetadata,
) {
    for name in INTEGRITY_CHECK {
        let meta = format!(r#"<meta name="{name}" content=""#);
        while let Some(start) = text.find(&meta) {
            let end = start
                + text[start..]
                    .find('\n')
                    .map_or(text.len() - start, |end| end + 1);
            text.replace_range(start..end, "");
        }
    }
    if !tika.iter().any(|(name, _)| name == INTEGRITY_CHECK[0]) {
        metadata.retain(|(name, _)| !INTEGRITY_CHECK.contains(&name.as_str()));
    }
}

/// `ZipParser.writeEntryXhtml` and the div of `handleEntryMetadata`: an empty `div` of class
/// `embedded` identified by the entry's name, if it has one.
fn write_entry_div(xhtml: &mut Xhtml, name: &str) -> Result<(), Stop> {
    if name.is_empty() {
        return Ok(());
    }
    xhtml.start_element_with("div", &[("class", "embedded"), ("id", name)])?;
    xhtml.end_element("div")
}

/// `handleEntryMetadata`: the creation and modification dates, the length and the name, the
/// file's div written for a named one.
fn entry_metadata(file: &ArchiveFile, xhtml: &mut Xhtml) -> Result<TikaMetadata, Stop> {
    let mut metadata = TikaMetadata::new();
    if let Some(created) = file.created_millis {
        metadata.set("dcterms:created", format_instant(created));
    }
    if let Some(modified) = file.modified_millis {
        metadata.set("dcterms:modified", format_instant(modified));
    }
    metadata.set("Content-Length", file.size.to_string());
    if !file.name.is_empty() {
        metadata.set(RESOURCE_NAME, file.name.clone());
        metadata.set(INTERNAL_PATH, file.name.clone());
        write_entry_div(xhtml, &file.name)?;
    }
    Ok(metadata)
}

/// An entry whose data cannot be read whole, for `reason`: recorded as a failure of the archive
/// in the form Tika's `ZipParser` records an unreadable entry in.
fn record_unreadable(metadata: &mut TikaMetadata, name: &str, reason: &str) {
    record_stream_exception(
        metadata,
        &format!(
            "org.apache.tika.exception.TikaException: Can't read archive stream ({name}): {reason}"
        ),
    );
}

/// The loop of `PackageParser` and `SevenZParser` over the files but the directories, each
/// handed to the embedded document extractor.
fn write_files(files: &[ArchiveFile], xhtml: &mut Xhtml) -> Result<(), Failure> {
    for file in files.iter().filter(|file| !file.directory) {
        let metadata = entry_metadata(file, xhtml)?;
        xhtml.parse_embedded(&file.content, &metadata, Wrapping::PackageEntry)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
