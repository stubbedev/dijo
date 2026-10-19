//! Archives and compressed files as Tika 4.1's package module reads them, each entry handed to
//! the embedded document extractor: ZIP archives with `ZipParser` ([`zip`]), compressed files
//! with `CompressorParser` ([`compressor`]), TAR, ar, cpio and ARJ archives with `PackageParser`
//! ([`package`]), 7z archives with `SevenZParser` ([`seven_z`]) and RAR archives with
//! `RarParser` ([`rar`]).

mod compressor;
#[cfg(test)]
pub(crate) mod large;
mod package;
mod rar;
mod seven_z;
mod zip;

pub use compressor::CompressorParser;
pub use package::PackageParser;
pub use rar::RarParser;
pub use seven_z::SevenZParser;
pub use zip::ZipParser;

use crate::compress::{ArchiveFile, Limited, Undecoded};
use crate::formats::embedded::{record_stream_exception, Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure};
use crate::java::time::format_instant;
use crate::Error;

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

/// Why the native readers do not read `what`, for the reason `undecoded`, given to Tika's
/// parser `tika_name`: a stream beyond the memory limit fails at once, as Tika fails it,
/// and data that fails to decode fails as the reader's `IOException` fails Tika's parser.
fn undecoded(what: &str, undecoded: Undecoded, tika_name: &str) -> Decline<'static> {
    match undecoded {
        Undecoded::TooLarge => Decline::Fails(past_the_limit(what)),
        Undecoded::MemoryLimit(_) => {
            Decline::Fails(Error::LimitExceeded(format!("{what}: {undecoded}")))
        }
        Undecoded::Unspooled(_) => Decline::Fails(Error::IoError(format!("{what}: {undecoded}"))),
        Undecoded::Unported(_) => Decline::Unread(format!("{what}: {undecoded}")),
        Undecoded::Fails(_) | Undecoded::Encrypted | Undecoded::BadPassword => Decline::Fails(
            Thrown::Io(Exception::new(
                "java.io.IOException",
                format!("{what}: {undecoded}"),
            ))
            .into_error(tika_name),
        ),
    }
}

/// The error of a document holding `what`, which decodes into more than
/// [`MAX_DECODED`](crate::compress::MAX_DECODED) bytes, the most the native path decodes.
fn past_the_limit(what: &str) -> Error {
    Error::LimitExceeded(format!(
        "{what}: {}, the most the native parser decodes",
        Undecoded::TooLarge
    ))
}

/// Where the native parser stops writing a stream or archive cut at
/// [`MAX_DECODED`](crate::compress::MAX_DECODED) bytes: after `what` it decodes into up to
/// there.
#[derive(Debug)]
struct Cut(String);

impl Cut {
    /// How a document ends once it has written what it decoded: as it would, or, where it was
    /// `cut`, failing, as does every document it is embedded in, with the text written so far.
    fn end(cut: Option<Cut>) -> Result<(), Failure> {
        cut.map_or(Ok(()), |Cut(what)| {
            Err(Failure::Embedded(past_the_limit(&what)))
        })
    }
}

/// What the native parser writes of `what`, as `limited` holds it: all of it, or what it
/// decodes into up to [`MAX_DECODED`](crate::compress::MAX_DECODED) bytes, and where the
/// document then stops.
fn within_limit<T>(what: String, limited: Limited<T>) -> (T, Option<Cut>) {
    match limited {
        Limited::Whole(decoded) => (decoded, None),
        Limited::Cut(decoded) => (decoded, Some(Cut(what))),
    }
}

/// Hands `file`, which is no directory, to the embedded document extractor, with its metadata.
fn write_file(file: &ArchiveFile, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let metadata = entry_metadata(file, xhtml)?;
    xhtml.parse_embedded_input(&file.content, &metadata, Wrapping::PackageEntry)
}

#[cfg(test)]
mod tests;
