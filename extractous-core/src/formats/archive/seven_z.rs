//! Tika's `SevenZParser`: the files of a 7z archive but its directories, in the order the
//! archive lists them, handed to the embedded document extractor with the metadata
//! `handleEntryMetadata` gives them, their creation and modification times among it.

use super::nesting::Scan;
use super::write_files;
use crate::compress::seven_z;
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};

const SEVEN_Z: &str = "application/x-7z-compressed";

/// Tika's `SevenZParser`.
pub(crate) struct SevenZParser;

impl Parser for SevenZParser {
    fn name(&self) -> &'static str {
        "extractous::SevenZParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.SevenZParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[SEVEN_Z]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let files =
            seven_z::files(input).map_err(|undecoded| format!("the 7z archive: {undecoded}"))?;
        if let Some(reason) = Scan::new().files(input.len(), &files) {
            return Err(Declined::Deferred(reason));
        }
        Ok(Box::new(move |xhtml| {
            xhtml.metadata_mut().set("Content-Type", SEVEN_Z);
            let written = write_files(&files, xhtml);
            xhtml.end_document()?;
            written
        }))
    }
}
