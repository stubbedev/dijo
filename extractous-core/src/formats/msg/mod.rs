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
use crate::formats::ole2::{office, Summaries};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared, Unported};
use crate::ole2::{write, EntryId, EntryKind, Node, Ole2, Unreadable};
use properties::{EMBEDDED_MESSAGE_HEADER, MESSAGE_HEADER, PROPERTIES};

const MEDIA_TYPE: &str = "application/vnd.ms-outlook";

pub(crate) struct MsgParser;

impl Parser for MsgParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Defers messages POI or Tika throws on and those it leaves to Tika's parsers. Tika writes
    /// the named properties and times of a message in the order of hash maps keyed by identity,
    /// which its head shows; the native head lists them sorted by name (#64).
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let (summaries, extracted) = read(input, configs).map_err(|Unported(reason)| reason)?;
        Ok(Box::new(move |xhtml| {
            summaries.write(xhtml.metadata_mut());
            xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
            extracted.write(xhtml)
        }))
    }
}

/// The summary information and what Tika's extractor makes of the message in a compound
/// document.
fn read(input: &[u8], configs: &ParserConfigs) -> Result<(Summaries, Extracted), Unported> {
    let file = office::open(input, configs, MEDIA_TYPE, "message")?;
    let root = file.root();
    let summaries = Summaries::read(&file, root)?;
    let message = Message::read(&file, root, false)
        .map_err(|thrown| Unported(format!("POI throws reading the message: {}", thrown.0)))?;
    let extracted = extract(&file, message, configs)?;
    Ok((summaries, extracted))
}

/// The most messages this port reads stored one in another, where Java's stack would end first.
const MAX_STORED_MESSAGES: usize = 16;

/// A message stored in another document as Tika's `OfficeParser` reads it from the storage
/// handed on as an open container: the storage's summary information, then the message, its
/// properties read as those of an embedded message; and the message as a document of its own.
pub(crate) struct EmbeddedMessage {
    summaries: Summaries,
    extracted: Extracted,
    data: Vec<u8>,
}

impl EmbeddedMessage {
    /// Reads the message in `directory`, a storage of `file` below its root.
    pub(crate) fn read(
        file: &Ole2<'_>,
        directory: EntryId,
        configs: &ParserConfigs,
    ) -> Result<EmbeddedMessage, Unported> {
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
        let extracted = extract(file, message, configs)?;
        let data = standalone(file, directory).map_err(|Unreadable(reason)| {
            Unported(format!(
                "a stored message POI cannot read, whose failure Tika records: {reason}"
            ))
        })?;
        Ok(EmbeddedMessage {
            summaries,
            extracted,
            data,
        })
    }

    /// The message as a message file of its own, which an extraction spools.
    pub(crate) fn data(&self) -> &[u8] {
        &self.data
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
fn standalone(file: &Ole2<'_>, directory: EntryId) -> Result<Vec<u8>, Unreadable> {
    let mut storage = file.storage(directory)?;
    let properties: Vec<u16> = PROPERTIES.encode_utf16().collect();
    for (name, node) in &mut storage.entries {
        if *name != properties {
            continue;
        }
        if let Node::Document(data) = node {
            if data.len() >= EMBEDDED_MESSAGE_HEADER {
                let reserved = [0; MESSAGE_HEADER - EMBEDDED_MESSAGE_HEADER];
                data.splice(EMBEDDED_MESSAGE_HEADER..EMBEDDED_MESSAGE_HEADER, reserved);
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
    write(&storage)
}

#[cfg(test)]
pub(crate) mod tests;
