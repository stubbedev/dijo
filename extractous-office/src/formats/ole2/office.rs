//! What Tika's `OfficeParser` does before it reads a compound document of a known type: POI
//! opens the file, its container detection names the type, and macros, when asked for, are
//! extracted, which no port does.

use crate::config::ParserConfigs;
use crate::formats::Unported;
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{Ole2, Unreadable};

/// The compound document `input`, opened as POI opens it, or why Tika's path should read it:
/// macros extracted when asked to, a file POI opens otherwise from a stream, and one its
/// container detection names other than `media_type`, the type of a `kind` of document.
pub fn open<'a>(
    input: &'a [u8],
    configs: &ParserConfigs,
    media_type: &str,
    kind: &str,
) -> Result<Ole2<'a>, Unported> {
    open_as(input, configs, &[media_type], kind).map(|(file, _)| file)
}

/// [`open`] of a compound document of any of `media_types`, the types of a `kind` of document:
/// the file, and which of them its container detection names it.
pub fn open_as<'a, 't>(
    input: &'a [u8],
    configs: &ParserConfigs,
    media_types: &[&'t str],
    kind: &str,
) -> Result<(Ole2<'a>, &'t str), Unported> {
    if configs.office.extracts_macros() {
        return Err(Unported(
            "macros are extracted, which this port does not do".to_owned(),
        ));
    }
    let file = Ole2::open(input).map_err(|Unreadable(reason)| {
        Unported(format!("POI cannot open the compound document: {reason}"))
    })?;
    if let Err(Unreadable(reason)) = Ole2::open_stream(input) {
        return Err(Unported(format!(
            "POI reads the compound document from a stream differently: {reason}"
        )));
    }
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
