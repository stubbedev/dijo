//! Tika's `OneNoteParser` over OneNote 2010 and later sections in the MS-ONESTORE format: the
//! header's numbers as metadata, then the file node lists walked from the root
//! ([`structure`], [`walker`]), the text of their objects written and their file data handed
//! to the embedded document extractor. Where the structure or the walk throws, Tika records the
//! exception and falls back to dumping the file's runs of printable characters ([`legacy`]),
//! after anything the walk wrote. Sections in the alternative packaging of MS-FSSHTTPB, which
//! OneNote writes for files from OneDrive, are read as Tika reads them ([`fsshttpb`]), and
//! dumped as legacy text where that reading throws or finds no content.

mod file;
mod fsshttpb;
mod guid;
mod legacy;
mod structure;
mod walker;

use file::{Dif, Thrown};
use fsshttpb::Walked;
use structure::{Document, Header, Reader};
use walker::{Ended, Found, Walker};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};

const STRUCTURE_FAILED: &str = "OneNote parse failed; falling back to legacy text dump";
const PACKAGE_FAILED: &str = "OneNote FSSHTTPB parse failed; falling back to legacy text dump";
const PACKAGE_EMPTY: &str =
    "OneNote FSSHTTPB parse produced no content; falling back to legacy text dump";

/// Tika's `OneNoteParser`.
pub(crate) struct OneNoteParser;

impl Parser for OneNoteParser {
    fn name(&self) -> &'static str {
        "extractous::OneNoteParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.onenote.OneNoteParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/onenote; format=one"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let mut document = Document::default();
        let mut reader = Reader::new(Dif::new(input), &mut document);
        let header = reader
            .header()
            .map_err(|Thrown(exception)| Declined::Fails(Error::ParseError(exception)))?;
        if !header.is_ms_one_store() {
            if header.alternative_packaging {
                let read = fsshttpb::read(input)?;
                return Ok(Box::new(move |xhtml| {
                    let walked = match &read {
                        Ok(package) => fsshttpb::walk(package, xhtml),
                        Err(thrown) => Walked::Thrown(thrown.clone()),
                    };
                    match walked {
                        Walked::Done { content: true } => Ok(()),
                        Walked::Done { content: false } => {
                            xhtml
                                .metadata_mut()
                                .add(embedded::EXCEPTION_WARNING, PACKAGE_EMPTY);
                            Ok(legacy::dump(input, xhtml)?)
                        }
                        Walked::Thrown(cause) => fall_back(input, PACKAGE_FAILED, cause, xhtml),
                        Walked::Failed(failure) => Err(failure),
                    }
                }));
            }
            return Err(Declined::Fails(Error::ParseError(
                "Invalid OneStore document - could not parse headers".to_owned(),
            )));
        }
        let structure = reader.root(header.fcr_file_node_list_root);
        Ok(Box::new(move |xhtml| {
            set_header_metadata(xhtml.metadata_mut(), &header);
            let cause = match structure {
                Ok(()) => {
                    let mut dif = Dif::new(input);
                    match Walker::new(&document, &mut dif, xhtml).walk() {
                        Ok(found) => {
                            set_found_metadata(xhtml.metadata_mut(), found);
                            return Ok(());
                        }
                        Err(Ended::Thrown(thrown)) => thrown,
                        Err(Ended::Failed(failure)) => return Err(failure),
                    }
                }
                Err(thrown) => thrown,
            };
            fall_back(input, STRUCTURE_FAILED, cause, xhtml)
        }))
    }
}

/// `legacyFallbackDump`: the warning and the exception recorded, then the legacy dump.
fn fall_back(
    input: &[u8],
    warning: &str,
    Thrown(cause): Thrown,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let metadata = xhtml.metadata_mut();
    metadata.add(embedded::EXCEPTION_WARNING, warning);
    embedded::record_exception(metadata, &cause);
    legacy::dump(input, xhtml)?;
    Ok(())
}

fn set_header_metadata(metadata: &mut TikaMetadata, header: &Header) {
    let hex = |value: i64| format!("0x{:x}", value as u64);
    for (name, value) in [
        ("buildNumberCreated", header.build_number_created),
        (
            "buildNumberLastWroteToFile",
            header.build_number_last_wrote_to_file,
        ),
        (
            "buildNumberNewestWritten",
            header.build_number_newest_written,
        ),
        (
            "buildNumberOldestWritten",
            header.build_number_oldest_written,
        ),
        ("cbExpectedFileLength", header.cb_expected_file_length),
        (
            "cbFreeSpaceInFreeChunkList",
            header.cb_free_space_in_free_chunk_list,
        ),
        ("cbLegacyExpectedFileLength", 0),
        (
            "cbLegacyFreeSpaceInFreeChunkList",
            header.cb_legacy_free_space_in_free_chunk_list,
        ),
        ("crcName", header.crc_name),
        ("cTransactionsInLog", header.c_transactions_in_log),
        (
            "ffvLastCodeThatWroteToThisFile",
            header.ffv_last_code_that_wrote_to_this_file,
        ),
        (
            "ffvNewestCodeThatHasWrittenToThisFile",
            header.ffv_newest_code_that_has_written_to_this_file,
        ),
        (
            "ffvOldestCodeThatMayReadThisFile",
            header.ffv_oldest_code_that_may_read_this_file,
        ),
        (
            "ffvOldestCodeThatHasWrittenToThisFile",
            header.ffv_oldest_code_that_has_written_to_this_file,
        ),
        ("grfDebugLogFlags", header.grf_debug_log_flags),
        ("nFileVersionGeneration", header.n_file_version_generation),
        ("rgbPlaceholder", header.rgb_placeholder),
    ] {
        metadata.set(&format!("onenote:{name}"), hex(value));
    }
}

/// The authors, sorted as Java sorts strings, and the times the walk found.
fn set_found_metadata(metadata: &mut TikaMetadata, found: Found) {
    let sorted = |mut values: Vec<String>| {
        values.sort_by(|a, b| crate::java::compare(a, b));
        values
    };
    for (name, values) in [
        ("dc:creator", found.authors),
        ("onenote:mostRecentAuthors", found.most_recent_authors),
        ("onenote:originalAuthors", found.original_authors),
    ] {
        if !values.is_empty() {
            metadata.set_values(name, sorted(values));
        }
    }
    metadata.set(
        "onenote:creationTimestamp",
        found.creation_timestamp.to_string(),
    );
    if let Some(seconds) = found.last_modified_timestamp {
        metadata.set(
            "onenote:lastModifiedTimestamp",
            (seconds * 1000).to_string(),
        );
    }
    if found.last_modified > i64::MIN {
        metadata.set("dcterms:modified", found.last_modified.to_string());
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests;
