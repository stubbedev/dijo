use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

#[test]
fn extracts_what_tika_parses_with_empty_parser_as_tikas_path_does() {
    let scratch = Scratch::new("empty");
    let binary = scratch.write("tilfældig.bin", &[0x00, 0x9F, 0x3B, 0xFE, 0x10, 0x00, 0x7F]);
    let hwp3 = scratch.write(
        "gammel.hwp",
        b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05",
    );
    let key = scratch.write(
        "server.key",
        b"-----BEGIN PRIVATE KEY-----\nMIIBVAIBADANBgkqhkiG9w0BAQEFAASCAT4wggE6AgEAAkEA\n\
          -----END PRIVATE KEY-----\n",
    );
    let (parsed, deferred) = documents(&EmptyParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed, [hwp3.clone(), key.clone(), binary.clone()]);
    assert_same_as_tika(
        &[binary.as_path(), hwp3.as_path(), key.as_path()],
        EmptyParser.tika_name(),
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
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
