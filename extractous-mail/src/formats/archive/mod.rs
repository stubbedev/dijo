//! Archives and compressed files as Tika 4.1's package module reads them, each entry handed to
//! the embedded document extractor: ZIP archives with `ZipParser` ([`zip`]), compressed files
//! with `CompressorParser` ([`compressor`]), TAR, ar, cpio and ARJ archives with `PackageParser`
//! ([`package`]), 7z archives with `SevenZParser` ([`seven_z`]) and RAR archives with
//! `RarParser` ([`rar`]).

mod compressor;
#[cfg(all(test, not(feature = "tika")))]
pub(crate) mod large;
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

use crate::compress::{ArchiveFile, Limited, Undecoded};
use crate::formats::embedded::{record_stream_exception, Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure};
use crate::java::time::format_instant;
use crate::Error;

/// For the parity tests: why the native path extracts the first segment of a split ZIP archive
/// where Tika's path fails on it: Tika's stream fails the document at the entry the segment cuts
/// short, the native parser records that entry as unreadable and keeps the entries before it.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
const SPLIT_SEGMENT: &str = "Tika fails the document at the entry the segment cuts short";

/// For the parity tests: the first segment of a split ZIP archive among Tika's test documents,
/// by file name, with why it is not compared ([`SPLIT_SEGMENT`]).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const TIKAS_SPLIT_SEGMENT: (&str, &str) = ("test-documents-spanned.z01", SPLIT_SEGMENT);

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

/// Whether there is a Tika path to leave a document to.
const DEFERS: bool = cfg!(feature = "tika");

/// Why the native readers do not read `what`, for the reason `undecoded`, given to Tika's
/// parser `tika_name`: a stream beyond the memory limit fails at once, as Tika's path fails it.
/// Data that fails to decode fails as the reader's `IOException` fails Tika's parser, where
/// there is no Tika's path to fail with the reader's own message; what the native readers do
/// not read is left to Tika's path.
fn undecoded(what: &str, undecoded: Undecoded, tika_name: &str) -> Decline<'static> {
    match undecoded {
        Undecoded::TooLarge => too_large(what),
        Undecoded::MemoryLimit(_) => {
            Decline::Fails(Error::LimitExceeded(format!("{what}: {undecoded}")))
        }
        Undecoded::Unspooled(_) => Decline::Fails(Error::IoError(format!("{what}: {undecoded}"))),
        Undecoded::Unported(_) => Decline::Deferred(format!("{what}: {undecoded}")),
        Undecoded::Fails(_) | Undecoded::Encrypted | Undecoded::BadPassword if DEFERS => {
            Decline::Deferred(format!(
                "{what}: {undecoded}, which Tika's path fails with its reader's own exception"
            ))
        }
        Undecoded::Fails(_) | Undecoded::Encrypted | Undecoded::BadPassword => Decline::Fails(
            Thrown::Io(Exception::new(
                "java.io.IOException",
                format!("{what}: {undecoded}"),
            ))
            .into_error(tika_name),
        ),
    }
}

/// A document holding `what`, which decodes into more than
/// [`MAX_DECODED`](crate::compress::MAX_DECODED) bytes, where the native parser writes nothing
/// of it: Tika's path streams it, and takes the document where there is one. Without it, the
/// document exceeds what the native path decodes.
fn too_large(what: &str) -> Decline<'static> {
    if DEFERS {
        Decline::Deferred(format!(
            "{what}: {}, which Tika's path streams",
            Undecoded::TooLarge
        ))
    } else {
        Decline::Fails(past_the_limit(what))
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

/// What the native parser writes of `what`, as `limited` holds it: all of it, or, in a build
/// without Tika, what it decodes into up to [`MAX_DECODED`](crate::compress::MAX_DECODED)
/// bytes, and where the document then stops. Tika's path streams it whole, and takes the
/// document where there is one.
fn within_limit<T>(
    what: String,
    limited: Limited<T>,
) -> Result<(T, Option<Cut>), Decline<'static>> {
    match limited {
        Limited::Whole(decoded) => Ok((decoded, None)),
        Limited::Cut(_) if DEFERS => Err(too_large(&what)),
        Limited::Cut(decoded) => Ok((decoded, Some(Cut(what)))),
    }
}

/// Hands `file`, which is no directory, to the embedded document extractor, with its metadata.
fn write_file(file: &ArchiveFile, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let metadata = entry_metadata(file, xhtml)?;
    xhtml.parse_embedded_input(&file.content, &metadata, Wrapping::PackageEntry)
}

#[cfg(test)]
mod tests;
