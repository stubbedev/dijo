//! Tika's `SevenZParser`: the files of a 7z archive but its directories, in the order the
//! archive lists them, handed to the embedded document extractor with the metadata
//! `handleEntryMetadata` gives them, their creation and modification times among it.

use super::nesting::Scan;
use super::write_files;
use crate::compress::{seven_z, Undecoded};
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{crypt, Declined, Parser, Prepared, PASSWORD_USED};

const SEVEN_Z: &str = "application/x-7z-compressed";
/// What commons-compress's `SevenZFile` throws on encrypted data it was given no password for,
/// reading the archive from a channel, which it knows no name of.
const PASSWORD_REQUIRED: &str = "org.apache.commons.compress.PasswordRequiredException: Cannot \
                                 read encrypted content from unknown archive without a password.";

/// Tika's `SevenZParser`.
pub struct SevenZParser;

impl Parser for SevenZParser {
    fn name(&self) -> &'static str {
        "extractous::SevenZParser"
    }
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
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let password = configs.password.as_ref().map(|password| password.expose());
        let files = seven_z::files(input, password).map_err(|undecoded| match undecoded {
            Undecoded::Encrypted => Declined::Fails(crypt::encrypted(Some(PASSWORD_REQUIRED))),
            undecoded => Declined::Deferred(format!("the 7z archive: {undecoded}")),
        })?;
        if let Some(reason) = Scan::new().files(input.len(), &files) {
            return Err(Declined::Deferred(reason));
        }
        Ok(Box::new(move |xhtml| {
            if password.is_some() {
                xhtml.metadata_mut().set(PASSWORD_USED, "true");
            }
            xhtml.metadata_mut().set("Content-Type", SEVEN_Z);
            let written = write_files(&files, xhtml);
            xhtml.end_document()?;
            written
        }))
    }
}
