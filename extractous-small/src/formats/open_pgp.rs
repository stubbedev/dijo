//! OpenPGP-encrypted data, as our Java `OpenPgpParser` reports it: encrypted, whose decryption
//! needs the recipient's private key. Tika has no OpenPGP parser of its own.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Footprint, Parser, Prepared};

/// Our Java `OpenPgpParser`.
pub struct OpenPgpParser;

impl Parser for OpenPgpParser {
    fn name(&self) -> &'static str {
        "extractous::OpenPgpParser"
    }
    fn tika_name(&self) -> &'static str {
        "ai.yobix.OpenPgpParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/pgp-encrypted"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        _input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Err(Decline::Fails(Error::Encrypted(
            "OpenPGP-encrypted data; decrypting it needs the recipient's private key".to_owned(),
        )))
    }
}

#[cfg(test)]
mod tests {

    use extractous_core::large::LargeFile;

    use super::*;

    /// Encrypted data too large to read whole fails as encrypted without being read.
    #[test]
    fn fails_files_too_large_to_read_whole_as_encrypted() {
        let file = LargeFile::padded(
            "long.asc",
            b"-----BEGIN PGP MESSAGE-----

",
        );
        let error = crate::Extractor::new()
            .extract_file_to_string(file.path())
            .unwrap_err();
        assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
    }
}
