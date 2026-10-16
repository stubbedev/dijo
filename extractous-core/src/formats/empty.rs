//! Tika's `EmptyParser`, which `CompositeParser` falls back to for a document of a type none of
//! its parsers reads: an empty document, with no metadata of its own.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};

/// Tika's `EmptyParser`.
pub(crate) struct EmptyParser;

impl Parser for EmptyParser {
    fn name(&self) -> &'static str {
        "extractous::EmptyParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.EmptyParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn prepare<'a>(
        &self,
        _input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        Ok(Box::new(|_| Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

    #[test]
    fn extracts_documents_of_types_tika_has_no_parser_for_as_tikas_path_does() {
        let scratch = Scratch::new("empty");
        let key = scratch.write(
            "server.key",
            b"-----BEGIN PRIVATE KEY-----\nMIIBVAIBADANBgkqhkiG9w0BAQEFAASCAT4wggE6AgEAAkEA\n\
              -----END PRIVATE KEY-----\n",
        );
        let unknown = scratch.write("unknown.bin", &[0xfe, 0x00, 0x13, 0x37, 0x00, 0x01]);
        let (parsed, deferred) = documents(&EmptyParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed, [key.clone(), unknown.clone()]);
        assert_same_as_tika(&[key.as_path(), unknown.as_path()], EmptyParser.tika_name());
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_documents_of_types_tika_has_no_parser_for_as_tikas_path_does() {
        let roots = [
            PathBuf::from("../test_files/documents"),
            PathBuf::from("../quality/corpus"),
            conformance_corpus(),
        ];
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        let (parsed, deferred) = documents(&EmptyParser, &roots);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert!(parsed.len() > 50, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, EmptyParser.tika_name());
    }
}
