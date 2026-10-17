//! OpenPGP-encrypted data, as our Java `OpenPgpParser` reports it: encrypted, whose decryption
//! needs the recipient's private key. Tika has no OpenPGP parser of its own.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};

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

    fn prepare<'a>(
        &self,
        _input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        Err(Declined::Fails(Error::Encrypted(
            "OpenPGP-encrypted data; decrypting it needs the recipient's private key".to_owned(),
        )))
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_fails_as_tika, conformance_corpus, documents};

    #[test]
    fn fails_as_encrypted_where_tikas_path_does() {
        let (parsed, deferred) = documents(&OpenPgpParser, &[Path::new("../test_files/documents")]);
        assert!(deferred.is_empty());
        assert_eq!(parsed.len(), 2);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
        let error = crate::Extractor::new()
            .extract_file_to_string(paths[0].to_str().unwrap())
            .unwrap_err();
        assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn fails_on_tikas_test_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&OpenPgpParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty());
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
    }
}
