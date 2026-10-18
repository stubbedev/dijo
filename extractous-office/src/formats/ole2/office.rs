//! What Tika's `OfficeParser` does before it reads a compound document of a known type: POI
//! opens the file, its container detection names the type, and macros, when asked for, are
//! extracted, which no port does.

use crate::config::ParserConfigs;
use crate::formats::{Difference, Unported};
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{Ole2, Unreadable};

/// A compound document opened as POI opens a file, and where POI, opening it from a stream as
/// Tika's path does, reads it differently, if it does.
pub struct Opened<'a> {
    pub file: Ole2<'a>,
    pub difference: Option<Difference>,
}

/// The compound document `input`, opened as POI opens it, or why Tika's path should read it:
/// macros extracted when asked to, and a file POI cannot open. A file POI opens otherwise from a
/// stream is read differently from Tika's path.
pub fn open_file<'a>(input: &'a [u8], configs: &ParserConfigs) -> Result<Opened<'a>, Unported> {
    if configs.office.extracts_macros() {
        return Err(Unported(
            "macros are extracted, which this port does not do".to_owned(),
        ));
    }
    let file = Ole2::open(input).map_err(|Unreadable(reason)| {
        Unported(format!("POI cannot open the compound document: {reason}"))
    })?;
    let difference = Ole2::open_stream(input).err().map(|Unreadable(reason)| {
        Difference(format!(
            "POI reads the compound document from a stream differently: {reason}"
        ))
    });
    Ok(Opened { file, difference })
}

/// [`open_file`] of a compound document `media_type`, the type of a `kind` of document; one its
/// container detection names otherwise is left to Tika's path.
pub fn open<'a>(
    input: &'a [u8],
    configs: &ParserConfigs,
    media_type: &str,
    kind: &str,
) -> Result<Opened<'a>, Unported> {
    open_as(input, configs, &[media_type], kind).map(|(opened, _)| opened)
}

/// [`open`] of a compound document of any of `media_types`, the types of a `kind` of document:
/// the document, and which of them its container detection names it.
pub fn open_as<'a, 't>(
    input: &'a [u8],
    configs: &ParserConfigs,
    media_types: &[&'t str],
    kind: &str,
) -> Result<(Opened<'a>, &'t str), Unported> {
    let opened = open_file(input, configs)?;
    let detected = ole_directory_type(&opened.file, opened.file.root());
    let media_type = media_types
        .iter()
        .find(|&&media_type| detected == MediaType::named(media_type))
        .ok_or_else(|| {
            Unported(format!(
                "the compound document is no {kind} to POI's container detection"
            ))
        })?;
    Ok((opened, media_type))
}
