//! Archives and compressed files as Tika 4.1's package module reads them, each entry handed to
//! the embedded document extractor: ZIP archives with `ZipParser` ([`zip`]), compressed files
//! with `CompressorParser` ([`compressor`]), TAR, ar and cpio archives with `PackageParser`
//! ([`package`]) and 7z archives with `SevenZParser` ([`seven_z`]).

mod compressor;
mod nesting;
mod package;
mod seven_z;
mod zip;

pub(crate) use compressor::CompressorParser;
pub(crate) use package::PackageParser;
pub(crate) use seven_z::SevenZParser;
pub(crate) use zip::ZipParser;

use crate::compress::ArchiveFile;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;
use crate::java::time::format_instant;

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
