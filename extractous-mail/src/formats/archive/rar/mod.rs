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

use std::convert::Infallible;

use super::nesting::Scan;
use super::{entry_metadata, record_unreadable, too_large, DEFERS};
use crate::compress::{ArchiveFile, MAX_DECODED};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Failure, Footprint, Opened, Parser, Prepared};
use crate::input::{Input, Pages, Paging};

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

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Reads the headers, and writes each file as it is unpacked, spooled, before the next is
    /// unpacked, the pages of the archive released after each.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
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
        let (names, difference) = names(&headers);
        if DEFERS {
            scan(input, &headers, &names)?;
        }
        let paging = Paging::new(pages);
        paging.release();
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            xhtml.element("div", " ")?;
            extract::each(input, &headers, |index, extracted| {
                let (file, unread) = file(&headers.files[index], &names[index], extracted);
                let metadata = entry_metadata(&file, xhtml)?;
                match unread {
                    None => xhtml.parse_embedded_input_opened(
                        &file.content,
                        &metadata,
                        Wrapping::UnderlyingPackageEntry,
                        Opened::Alone,
                    )?,
                    Some(reason) => record_unreadable(xhtml.metadata_mut(), &file.name, &reason),
                }
                paging.release();
                Ok::<(), Failure>(())
            })
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
pub(super) fn archive_files(data: &[u8]) -> Option<Vec<ArchiveFile<'static>>> {
    let headers = headers::read(data).ok()?;
    let (names, _) = names(&headers);
    let mut files = Vec::new();
    let extracted = extract::each(data, &headers, |index, extracted| {
        files.push(file(&headers.files[index], &names[index], extracted).0);
        Ok::<(), Infallible>(())
    });
    match extracted {
        Ok(()) => Some(files),
        Err(never) => match never {},
    }
}

/// The names of the files of the archive, and the first detail in which the port reads them
/// otherwise than junrar, if any.
fn names(headers: &headers::Headers) -> (Vec<String>, Option<Difference>) {
    let mut names = Vec::with_capacity(headers.files.len());
    let mut difference = None;
    for header in &headers.files {
        match &header.name {
            Name::Decoded(name) => names.push(name.clone()),
            Name::Differs(name, what) => {
                names.push(name.clone());
                difference =
                    difference.or_else(|| Some(Difference::new(format!("an entry has {what}"))));
            }
        }
    }
    (names, difference)
}

/// Why Tika's path should read the archive `input`, which a build with Tika defers to it: its
/// files decode into more than the native path holds in memory to scan them, or what they hold
/// may reach Tika's limits otherwise than the native path's.
fn scan(
    input: &[u8],
    headers: &headers::Headers,
    names: &[String],
) -> Result<(), Decline<'static>> {
    let declared = headers
        .files
        .iter()
        .filter(|header| !header.is_directory() && !header.is_split())
        .fold(0i64, |declared, header| {
            declared.saturating_add(header.full_unpack_size)
        });
    if declared > MAX_DECODED as i64 {
        return Err(too_large("the entries of the RAR archive"));
    }
    let mut files = Vec::new();
    let extracted = extract::each(input, headers, |index, extracted| {
        files.push(file(&headers.files[index], &names[index], extracted).0);
        Ok::<(), Infallible>(())
    });
    if let Err(never) = extracted {
        match never {}
    }
    match Scan::new().files(input.len(), &files) {
        Some(reason) => Err(Decline::Deferred(reason)),
        None => Ok(()),
    }
}

/// The file of `header`, named `name`, as extracting it gave it, `extracted`, and why its data
/// cannot be given, if it cannot.
fn file(
    header: &headers::FileHeader,
    name: &str,
    extracted: Extracted,
) -> (ArchiveFile<'static>, Option<String>) {
    let (content, unread) = match extracted {
        Extracted::Data(content) => (content, None),
        Extracted::Unread(reason) => (Input::Borrowed(&[]), Some(reason)),
    };
    let file = ArchiveFile {
        name: name.to_owned(),
        size: header.full_unpack_size as u64,
        modified_millis: header.modified_millis,
        created_millis: header.created_millis,
        directory: false,
        content,
    };
    (file, unread)
}

#[cfg(test)]
mod tests;
