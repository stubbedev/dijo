//! Tika's `SevenZParser`: the files of a 7z archive but its directories, in the order the
//! archive lists them, handed to the embedded document extractor with the metadata
//! `handleEntryMetadata` gives them, their creation and modification times among it.

use super::nesting::Scan;
use super::write_files;
use crate::compress::seven_z;
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};

const SEVEN_Z: &str = "application/x-7z-compressed";

/// Tika's `SevenZParser`.
pub(crate) struct SevenZParser;

impl Parser for SevenZParser {
    fn name(&self) -> &'static str {
        "extractous::SevenZParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[SEVEN_Z]
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        match seven_z::files(input) {
            Ok(files) => Scan::new().files(input.len(), &files),
            Err(undecoded) => Some(format!("the 7z archive: {undecoded}")),
        }
    }

    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let files = seven_z::files(input).map_err(|e| Failure::Malformed(e.to_string()))?;
        xhtml.metadata_mut().set("Content-Type", SEVEN_Z);
        let written = write_files(&files, xhtml);
        xhtml.end_document()?;
        written
    }
}
