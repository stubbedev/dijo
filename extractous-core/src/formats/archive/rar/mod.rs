//! Tika's `RarParser`, which reads RAR 1.5 to 4 archives with junrar 8.1.1 and fails on RAR 5:
//! each file but a directory, in archive order, handed to the embedded document extractor
//! with the metadata `handleEntryMetadata` gives it. The parser hands the extractor the
//! content handler it was given rather than its `XHTMLContentHandler`, so the `package-entry`
//! divs and the entries' content are written without the line breaks the XHTML adds.
//!
//! The headers are ported from junrar's `Archive` ([`headers`]), its extraction from
//! `getInputStream` on ([`extract`]) and its unpackers, PPMd decoder and filters ([`unpack`]),
//! so an entry's data is junrar's, checked against its CRC. What junrar reads that the port
//! does not, RAR 5 and 1.4 archives, self-extractors and entries continued in another volume,
//! and every entry the unpacker fails on, is left to Tika.

mod extract;
mod headers;
mod unpack;

use headers::{Name, Unopened};

use super::entry_metadata;
use super::nesting::Scan;
use crate::compress::{ArchiveFile, MAX_DECODED};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Deferral, Opened, Parser, Prepared};
use crate::java::time::TimeZone;

const RAR: &str = "application/x-rar-compressed";

/// Tika's `RarParser`.
pub(crate) struct RarParser;

impl Parser for RarParser {
    fn name(&self) -> &'static str {
        "extractous::RarParser"
    }

    #[cfg(test)]
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
    ) -> Result<Prepared<'a>, Declined> {
        if metadata
            .get("Content-Type")
            .is_some_and(|media_type| media_type.contains("version=5"))
        {
            return Err(Declined::Fails(Error::ParseError(
                "Tika does not yet support rar version 5.".to_owned(),
            )));
        }
        let zone = TimeZone::java_default().map_err(|_| {
            "the JVM's default time zone, which entry times are read in, is unknown".to_owned()
        })?;
        let headers = headers::read(input, zone).map_err(|unopened| match unopened {
            Unopened::Fails {
                encrypted: true,
                reason,
            } => Declined::Fails(Error::Encrypted(reason)),
            Unopened::Fails { reason, .. } => Declined::Fails(Error::ParseError(reason)),
            Unopened::Unported(what) => {
                Declined::Deferred(format!("junrar reads {what}, which the port does not"))
            }
        })?;
        let files = files(input, &headers)?;
        if let Some(reason) = Scan::new().files(input.len(), &files) {
            return Err(Declined::Deferred(reason));
        }
        Ok(Box::new(move |xhtml| {
            xhtml.element("div", " ")?;
            for file in &files {
                let metadata = entry_metadata(file, xhtml)?;
                xhtml.parse_embedded_opened(
                    &file.content,
                    &metadata,
                    Wrapping::UnderlyingPackageEntry,
                    Opened::Alone,
                )?;
            }
            Ok(())
        }))
    }
}

/// Whether `data` starts with the marker Tika detects a RAR archive by.
pub(super) fn is_rar(data: &[u8]) -> bool {
    data.starts_with(b"Rar!")
}

/// The files of the RAR archive `data` but its directories, with their data, if the port
/// reads it.
pub(super) fn archive_files(data: &[u8]) -> Option<Vec<ArchiveFile>> {
    let zone = TimeZone::java_default().ok()?;
    let headers = headers::read(data, zone).ok()?;
    files(data, &headers).ok()
}

/// The files of the archive but its directories, with the data their streams give.
fn files(input: &[u8], headers: &headers::Headers) -> Result<Vec<ArchiveFile>, Deferral> {
    let mut declared = 0i64;
    let mut names = Vec::with_capacity(headers.files.len());
    for header in &headers.files {
        match &header.name {
            Name::Decoded(name) => names.push(name),
            Name::Unported(what) => return Err(format!("an entry has {what}")),
        }
        if header.full_unpack_size > 0 && header.is_split() {
            return Err("an entry continued from or in another volume".to_owned());
        }
        if !header.is_directory() {
            declared = declared.saturating_add(header.full_unpack_size);
        }
    }
    if declared > MAX_DECODED as i64 {
        return Err(format!(
            "its entries declare more than the {MAX_DECODED} bytes the port decodes"
        ));
    }
    Ok(extract::streams(input, headers)?
        .into_iter()
        .map(|(index, content)| {
            let header = &headers.files[index];
            ArchiveFile {
                name: names[index].clone(),
                size: header.full_unpack_size as u64,
                modified_millis: Some(header.modified_millis),
                created_millis: header.created_millis,
                directory: false,
                content,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests;
