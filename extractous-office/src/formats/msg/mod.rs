//! Outlook messages (MSG) as Tika's `OfficeParser` reads them with its `OutlookExtractor` on
//! POI's HSMF: the summary information, then the message's chunks read by POI's
//! `POIFSChunkParser` into its sender, recipients, subject, dates, transport headers and named
//! properties as metadata, its body from its HTML, its compressed RTF, which may encapsulate
//! HTML, or its text, parsed with the native HTML and RTF parsers, and its attachments as
//! embedded documents.
//!
//! A message stored in another document, which Tika's `OfficeParser` reads from its storage
//! with POI reading its properties as those of an embedded message, is read alike: see
//! [`EmbeddedMessage`].
//!
//! A message POI or Tika throws on is left to Tika, as is one whose 7-bit strings read
//! otherwise in a charset Tika may guess for them, one whose body the native parsers leave to
//! Tika, and macros extracted when asked to.

mod chunks;
pub(super) mod extractor;
mod named;
mod properties;
mod property_table;
pub(super) mod types;

use chunks::Message;
use extractor::{extract, Extracted};

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::Summaries;
use crate::formats::poi::Unread;
use crate::formats::xhtml::Xhtml;
use crate::formats::{
    unless_different, Decline, Difference, Failure, Footprint, Parser, Prepared, Unported,
};
use crate::ole2::{EntryId, EntryKind, Node, Ole2, Storage, Unreadable};
use properties::{EMBEDDED_MESSAGE_HEADER, MESSAGE_HEADER, PROPERTIES};

const MEDIA_TYPE: &str = "application/vnd.ms-outlook";

pub struct MsgParser;

impl Parser for MsgParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the message's streams in place. What it holds is what POI and Tika hold: the
    /// strings and properties decoded, and the body, which the HTML and RTF parsers read whole.
    /// The attachments are handed on one at a time as they are written, from where they lie,
    /// copied together only where their blocks are scattered, and a storage written as a
    /// compound document of its own, each spooled to a file past what is read whole.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Defers messages POI or Tika throws on and those it leaves to Tika's parsers. Tika writes
    /// the named properties and times of a message in the order of hash maps keyed by identity,
    /// which its head shows; the native head lists them sorted by name (#64).
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (summaries, extracted, difference) = read(input, configs)?;
        unless_different(
            Box::new(move |xhtml| {
                summaries.write(xhtml.metadata_mut());
                xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
                extracted.write(xhtml)
            }),
            difference,
        )
    }
}

/// The summary information and what Tika's extractor makes of the message in a compound
/// document, and where the native parsers read it differently from Tika's, if they do.
fn read<'a>(
    input: &'a [u8],
    configs: &ParserConfigs,
) -> Result<(Summaries, Extracted<'a>, Option<Difference>), Unread> {
    let file = office::open(input, configs, MEDIA_TYPE, "message")?;
    let root = file.root();
    let summaries = Summaries::read(&file, root)?;
    let message = Message::read(&file, root, false)
        .map_err(|thrown| Unported(format!("POI throws reading the message: {}", thrown.0)))?;
    let (extracted, read_otherwise) = extract(&file, message, configs)?;
    Ok((summaries, extracted, read_otherwise))
}

/// The most messages this port reads stored one in another, where Java's stack would end first.
const MAX_STORED_MESSAGES: usize = 16;

/// A message stored in another document as Tika's `OfficeParser` reads it from the storage
/// handed on as an open container: the storage's summary information, then the message, its
/// properties read as those of an embedded message; and the message as a document of its own,
/// its streams in place.
pub(crate) struct EmbeddedMessage<'a> {
    summaries: Summaries,
    extracted: Extracted<'a>,
    standalone: Storage<'a>,
}

impl<'a> EmbeddedMessage<'a> {
    /// Reads the message in `directory`, a storage of `file` below its root.
    pub(crate) fn read(
        file: &Ole2<'a>,
        directory: EntryId,
        configs: &ParserConfigs,
    ) -> Result<EmbeddedMessage<'a>, Unported> {
        if file.depth(directory) > MAX_STORED_MESSAGES {
            return Err(Unported(
                "messages stored in messages deeper than this port reads them".to_owned(),
            ));
        }
        let summaries = Summaries::read(file, directory)?;
        let message = Message::read(file, directory, true).map_err(|thrown| {
            Unported(format!(
                "POI throws reading a stored message, whose failure Tika records: {}",
                thrown.0
            ))
        })?;
        let (extracted, difference) = extract(file, message, configs)?;
        if let Some(Difference(reason)) = difference {
            return Err(Unported(format!(
                "a stored message this port reads differently from Tika: {reason}"
            )));
        }
        let standalone = standalone(file, directory).map_err(|Unreadable(reason)| {
            Unported(format!(
                "a stored message POI cannot read, whose failure Tika records: {reason}"
            ))
        })?;
        Ok(EmbeddedMessage {
            summaries,
            extracted,
            standalone,
        })
    }

    /// The message as a message file of its own, which an extraction spools.
    pub(crate) fn standalone(&self) -> Node<'a> {
        Node::Storage(self.standalone.clone())
    }

    /// The same, owning the streams it read in place.
    pub(crate) fn into_owned(self) -> EmbeddedMessage<'static> {
        EmbeddedMessage {
            summaries: self.summaries,
            extracted: self.extracted.into_owned(),
            standalone: self.standalone.into_owned(),
        }
    }

    /// Writes the message as `OfficeParser` does: its summary information and type as metadata,
    /// then what its `OutlookExtractor` writes.
    pub(crate) fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        self.summaries.write(xhtml.metadata_mut());
        xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
        self.extracted.write(xhtml)
    }
}

/// The message stored in `directory` of `file` as a message file of its own: its properties
/// stream with the header of a message, and the named properties of the message it is stored
/// in, which a stored message shares (#112). Tika's path spools it empty (#116).
fn standalone<'a>(file: &Ole2<'a>, directory: EntryId) -> Result<Storage<'a>, Unreadable> {
    let mut storage = file.storage(directory)?;
    let properties: Vec<u16> = PROPERTIES.encode_utf16().collect();
    for (name, node) in &mut storage.entries {
        if *name != properties {
            continue;
        }
        if let Node::Document(content) = node {
            if content.len() >= EMBEDDED_MESSAGE_HEADER {
                let mut data = content.to_vec();
                let reserved = [0; MESSAGE_HEADER - EMBEDDED_MESSAGE_HEADER];
                data.splice(EMBEDDED_MESSAGE_HEADER..EMBEDDED_MESSAGE_HEADER, reserved);
                *node = Node::Owned(data);
            }
        }
    }
    let name_id: Vec<u16> = chunks::NAME_ID.encode_utf16().collect();
    let shares_names = !storage.entries.iter().any(|(name, _)| *name == name_id);
    let names = file
        .child(file.root(), chunks::NAME_ID)
        .filter(|&names| file.kind(names) == EntryKind::Directory);
    if let (true, Some(names)) = (shares_names, names) {
        storage
            .entries
            .push((name_id, Node::Storage(file.storage(names)?)));
    }
    storage.check_writable()?;
    Ok(storage)
}

/// The recipients' names, which POI swaps (#112).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const RECIPIENT_NAMES: crate::formats::parity::Departures = &[
    crate::formats::parity::Departure::Values("message:to-name", "#112"),
    crate::formats::parity::Departure::Values("message:to-display-name", "#112"),
    crate::formats::parity::Departure::Values("message:cc-name", "#112"),
    crate::formats::parity::Departure::Values("message:cc-display-name", "#112"),
    crate::formats::parity::Departure::Values("message:bcc-name", "#112"),
    crate::formats::parity::Departure::Values("message:bcc-display-name", "#112"),
];

#[cfg(test)]
mod tests;
