//! Tika's `RarParser`, which reads RAR 1.5 to 4 archives with junrar 8.1.1 and fails on RAR 5:
//! each file but a directory, in archive order, handed to the embedded document extractor
//! with the metadata `handleEntryMetadata` gives it. The parser hands the extractor the
//! content handler it was given rather than its `XHTMLContentHandler`, so the `package-entry`
//! divs and the entries' content are written without the line breaks the XHTML adds.
//!
//! The headers are ported from junrar's `Archive` ([`headers`]), its extraction from
//! `getInputStream` on ([`extract`]) and its unpackers, PPMd decoder and filters ([`unpack`]),
//! so an entry's data is junrar's, checked against its CRC, but where junrar slips (#122). An
//! entry the port cannot give whole, one whose data does not match its CRC, is compressed for
//! an unknown RAR version, continues in another volume or fails to unpack, is recorded as a
//! failure where Tika's path gives it empty. What junrar reads that the port does not, RAR 1.4
//! archives and self-extractors, is left to Tika.

mod extract;
mod headers;
mod unpack;

use extract::Extracted;
use headers::{Name, Unopened};

use super::nesting::Scan;
use super::{entry_metadata, record_unreadable, too_large};
use crate::compress::{ArchiveFile, MAX_DECODED};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Opened, Parser, Prepared};

const RAR: &str = "application/x-rar-compressed";

/// Tika's `RarParser`.
pub struct RarParser;

impl Parser for RarParser {
    fn name(&self) -> &'static str {
        "extractous::RarParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.RarParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[RAR]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        if metadata
            .get("Content-Type")
            .is_some_and(|media_type| media_type.contains("version=5"))
        {
            return Err(Decline::Fails(Error::ParseError(
                "Tika does not yet support rar version 5.".to_owned(),
            )));
        }
        let headers = headers::read(input).map_err(|unopened| match unopened {
            Unopened::Fails {
                encrypted: true,
                reason,
            } => Decline::Fails(Error::Encrypted(reason)),
            Unopened::Fails { reason, .. } => Decline::Fails(Error::ParseError(reason)),
            Unopened::Unported(what) => {
                Decline::Deferred(format!("junrar reads {what}, which the port does not"))
            }
        })?;
        let (entries, difference) = entries(input, &headers)?;
        if let Some(reason) =
            Scan::new().files(input.len(), entries.iter().map(|entry| &entry.file))
        {
            return Err(Decline::Deferred(reason));
        }
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            xhtml.element("div", " ")?;
            for Entry { file, unread } in &entries {
                let metadata = entry_metadata(file, xhtml)?;
                match unread {
                    None => xhtml.parse_embedded_opened(
                        &file.content,
                        &metadata,
                        Wrapping::UnderlyingPackageEntry,
                        Opened::Alone,
                    )?,
                    Some(reason) => record_unreadable(xhtml.metadata_mut(), &file.name, reason),
                }
            }
            Ok(())
        });
        match difference {
            None => Ok(reading),
            Some(difference) => Err(Decline::Differs {
                reading,
                difference,
            }),
        }
    }
}

/// Whether `data` starts with the marker Tika detects a RAR archive by.
pub(super) fn is_rar(data: &[u8]) -> bool {
    data.starts_with(b"Rar!")
}

/// The files of the RAR archive `data` but its directories, with their data, empty for an
/// entry the port cannot give, if the port reads it.
pub(super) fn archive_files(data: &[u8]) -> Option<Vec<ArchiveFile>> {
    let headers = headers::read(data).ok()?;
    let (entries, _) = entries(data, &headers).ok()?;
    Some(entries.into_iter().map(|entry| entry.file).collect())
}

/// A file of the archive, and why its data cannot be given, if it cannot.
struct Entry {
    file: ArchiveFile,
    unread: Option<String>,
}

/// The files of the archive but its directories, and the first detail in which the port reads
/// them otherwise than junrar, if any.
fn entries(
    input: &[u8],
    headers: &headers::Headers,
) -> Result<(Vec<Entry>, Option<Difference>), Decline<'static>> {
    let mut declared = 0i64;
    let mut names = Vec::with_capacity(headers.files.len());
    let mut difference = None;
    for header in &headers.files {
        match &header.name {
            Name::Decoded(name) => names.push(name),
            Name::Differs(name, what) => {
                names.push(name);
                difference =
                    difference.or_else(|| Some(Difference::new(format!("an entry has {what}"))));
            }
        }
        if !header.is_directory() && !header.is_split() {
            declared = declared.saturating_add(header.full_unpack_size);
        }
    }
    if declared > MAX_DECODED as i64 {
        return Err(too_large("the entries of the RAR archive"));
    }
    let entries = extract::entries(input, headers)
        .into_iter()
        .map(|(index, extracted)| {
            let header = &headers.files[index];
            let (content, unread) = match extracted {
                Extracted::Data(content) => (content, None),
                Extracted::Unread(reason) => (Vec::new(), Some(reason)),
            };
            Entry {
                file: ArchiveFile {
                    name: names[index].clone(),
                    size: header.full_unpack_size as u64,
                    modified_millis: header.modified_millis,
                    created_millis: header.created_millis,
                    directory: false,
                    content,
                },
                unread,
            }
        })
        .collect();
    Ok((entries, difference))
}

#[cfg(test)]
mod tests;
