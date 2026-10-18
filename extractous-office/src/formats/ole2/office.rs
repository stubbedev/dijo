//! What Tika's `OfficeParser` does before it reads a compound document of a known type: POI
//! opens the file, its container detection names the type, and macros, when asked for, are
//! extracted, which no port does.

use crate::config::ParserConfigs;
use crate::formats::poi::{Thrown, Unread};
use crate::formats::Unported;
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{Declared, Ole2, Unreadable};

/// `POIFSContainerDetector.MAX_UNBUDGETED_COPY`: the most bytes POI's stream loader may copy when
/// no memory budget accounts for them.
const MAX_UNBUDGETED_COPY: i64 = 16 * 1024 * 1024;
/// `POIFSContainerDetector.MIN_DECLARED_BAT_BLOCKS`: the block allocation table blocks any
/// document may declare, whatever its length.
const MIN_DECLARED_BAT_BLOCKS: i64 = 4;
/// `POIFSContainerDetector.MAX_DECLARED_AMPLIFICATION`: past this multiple of its length, a
/// header's declared size does not describe the document.
const MAX_DECLARED_AMPLIFICATION: i64 = 16;

/// The compound document `input`, opened as Tika's path opens it, or why it is not read as Tika's
/// path reads it: macros extracted when asked to, which Tika's path reads, and a file POI cannot
/// open, which fails Tika's parse.
pub fn open_file<'a>(input: &'a [u8], configs: &ParserConfigs) -> Result<Ole2<'a>, Unread> {
    if configs.office.extracts_macros() {
        return Err(
            Unported("macros are extracted, which this port does not do".to_owned()).into(),
        );
    }
    opened_as_detected(input).map_err(|Unreadable(reason)| {
        Thrown(format!(
            "POIFSFileSystem cannot open the compound document: {reason}"
        ))
        .into()
    })
}

/// `POIFSContainerDetector.getTopLevelNames` on a document in memory, which opens the container
/// the parser then reads: from a stream, which POI copies into memory, where the header declares
/// an honest size within the copy's cap and the stream opens, and as a file otherwise.
fn opened_as_detected(input: &[u8]) -> Result<Ole2<'_>, Unreadable> {
    let streamed = honest_declared_size(input)
        .filter(|&declared| declared <= MAX_UNBUDGETED_COPY)
        .and_then(|_| Ole2::open_stream(input).ok());
    match streamed {
        Some(file) => Ok(file),
        None => Ole2::open(input),
    }
}

/// `POIFSContainerDetector.honestDeclaredSize`: the bytes POI's stream loader allocates for the
/// document, as its header declares them, unless the header is missing or declares an
/// implausible multiple of the document's length.
fn honest_declared_size(input: &[u8]) -> Option<i64> {
    let Declared { block_size, size } = Declared::read(input)?;
    let sector = block_size as i64;
    let bat_span = sector * (sector / 4);
    let ceiling = (sector + MIN_DECLARED_BAT_BLOCKS * bat_span)
        .max(input.len() as i64 * MAX_DECLARED_AMPLIFICATION);
    (size <= ceiling).then_some(size)
}

/// [`open_file`] of a compound document `media_type`, the type of a `kind` of document; one its
/// container detection names otherwise is left to Tika's path.
pub fn open<'a>(
    input: &'a [u8],
    configs: &ParserConfigs,
    media_type: &str,
    kind: &str,
) -> Result<Ole2<'a>, Unread> {
    open_as(input, configs, &[media_type], kind).map(|(file, _)| file)
}

/// [`open`] of a compound document of any of `media_types`, the types of a `kind` of document:
/// the document, and which of them its container detection names it.
pub fn open_as<'a, 't>(
    input: &'a [u8],
    configs: &ParserConfigs,
    media_types: &[&'t str],
    kind: &str,
) -> Result<(Ole2<'a>, &'t str), Unread> {
    let file = open_file(input, configs)?;
    let detected = ole_directory_type(&file, file.root());
    let media_type = media_types
        .iter()
        .find(|&&media_type| detected == MediaType::named(media_type))
        .ok_or_else(|| {
            Unported(format!(
                "the compound document is no {kind} to POI's container detection"
            ))
        })?;
    Ok((file, media_type))
}
