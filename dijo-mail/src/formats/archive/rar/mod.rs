//! Tika's `RarParser`, which reads RAR 1.4 to 4 archives with junrar 8.1.2, at the start of
//! the file or after a self-extractor's code, and fails on RAR 5:
//! each file but a directory, in archive order, handed to the embedded document extractor
//! with the metadata `handleEntryMetadata` gives it. The parser hands the extractor the
//! content handler it was given rather than its `XHTMLContentHandler`, so the `package-entry`
//! divs and the entries' content are written without the line breaks the XHTML adds.
//!
//! The headers are ported from junrar's `Archive` ([`headers`]), its extraction from
//! `getInputStream` on ([`extract`]) and its unpackers, PPMd decoder and filters ([`unpack`]),
//! so an entry's data is junrar's, checked against its CRC, but where junrar slips (#122). An
//! entry whose extraction fails, as one whose data does not match its CRC, is compressed for an
//! unknown RAR version, continues in another volume or fails to unpack does, is parsed empty and
//! its failure recorded, as `RarParser`'s `EntryStream` records it; one the port does not give,
//! past the most it decodes, is recorded as unread.

mod extract;
mod headers;
mod unpack;

use extract::Extracted;
use headers::{Name, Unopened};

use super::{entry_metadata, record_unreadable};
use crate::compress::{ArchiveFile, FileTime};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{record_stream_exception, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Failure, Footprint, Opened, Parser, Prepared};
use crate::input::{Input, Pages, Paging};

const RAR: &str = "application/x-rar-compressed";

/// Tika's `RarParser`.
pub struct RarParser;

impl Parser for RarParser {
    fn name(&self) -> &'static str {
        "dijo::RarParser"
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
                let header = &headers.files[index];
                let (file, read) = file(header, &names[index], extracted);
                let metadata = entry_metadata(&file, xhtml)?;
                match read {
                    Read::Unread(reason) => {
                        record_unreadable(xhtml.metadata_mut(), &file.name, &reason);
                    }
                    read => {
                        xhtml.parse_embedded_input_opened(
                            &file.content,
                            &metadata,
                            Wrapping::UnderlyingPackageEntry,
                            Opened::Alone,
                        )?;
                        if read == Read::Failed {
                            record_stream_exception(
                                xhtml.metadata_mut(),
                                &format!("java.io.IOException: RAR entry {}", header.file_name),
                            );
                        }
                    }
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

/// How Tika's parser reads a file's data.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Read {
    /// Whole.
    Whole,
    /// Empty, and its stream's failure is recorded, as `EntryStream` records it.
    Failed,
    /// Not at all, as the port does not give it, for this reason, which is recorded.
    Unread(String),
}

/// The file of `header`, named `name`, as extracting it gave it, `extracted`, and how its data
/// is read.
fn file(
    header: &headers::FileHeader,
    name: &str,
    extracted: Extracted,
) -> (ArchiveFile<'static>, Read) {
    let (content, read) = match extracted {
        Extracted::Data(content) => (content, Read::Whole),
        Extracted::Failed => (Input::Borrowed(&[]), Read::Failed),
        Extracted::Unread(reason) => (Input::Borrowed(&[]), Read::Unread(reason)),
    };
    let file = ArchiveFile {
        name: name.to_owned(),
        size: header.full_unpack_size,
        modified: header.modified_millis.map(FileTime::Local),
        created: header.created_millis.map(FileTime::Local),
        accessed: header.accessed_millis.map(FileTime::Local),
        directory: false,
        content,
    };
    (file, read)
}

#[cfg(test)]
mod tests;
