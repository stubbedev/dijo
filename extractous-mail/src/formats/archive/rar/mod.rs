//! Tika's `RarParser`, which reads RAR 1.4 to 4 archives with junrar 8.1.1, at the start of
//! the file or after a self-extractor's code, and fails on RAR 5:
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
//! failure where Tika's path gives it empty.

mod extract;
mod headers;
mod unpack;

use extract::Extracted;
use headers::{Name, Unopened};

use super::{entry_metadata, record_unreadable};
use crate::compress::ArchiveFile;
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
            Unopened {
                encrypted: true,
                reason,
            } => Decline::Fails(Error::Encrypted(reason)),
            Unopened { reason, .. } => Decline::Fails(Error::ParseError(reason)),
        })?;
        let (names, difference) = names(&headers);
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
        size: header.full_unpack_size,
        modified_millis: header.modified_millis,
        created_millis: header.created_millis,
        directory: false,
        content,
    };
    (file, unread)
}

#[cfg(test)]
mod tests;
