use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};
use crate::isolate::isolate;
use crate::mime;
use crate::tika::{self, EmbeddedOutcome};

/// The metadata key naming the type whose parser parses a document, whatever it is detected as.
const PARSER_OVERRIDE: &str = "tk:content-type-parser-override";
const TYPES: &str = "src/formats/empty/types.rs";

/// The declared types Tika's path parses with `EmptyParser`, asked of it type by type: a
/// document of one byte parsed with each type's parser.
fn tikas_empty_types() -> Vec<String> {
    let isolate = isolate().unwrap();
    let configs = ParserConfigs::default();
    let mut empty: Vec<String> = mime::declared_types()
        .map(ToString::to_string)
        .filter(|declared| {
            let parsed = tika::parse_embedded(
                isolate,
                b"x",
                &[(PARSER_OVERRIDE, declared.as_str())],
                &configs,
            )
            .unwrap_or_else(|error| panic!("Tika's path fails to parse as {declared}: {error}"));
            let EmbeddedOutcome::Parsed { metadata, .. } = parsed.outcome else {
                return false;
            };
            metadata.get("tk:parsed-by").map(Vec::as_slice)
                == Some(&[EmptyParser.tika_name().to_owned()][..])
        })
        .collect();
    empty.sort();
    empty
}

#[test]
fn takes_the_types_tika_parses_with_empty_parser() {
    let empty = tikas_empty_types();
    if std::env::var_os("EMPTY_PARSER_TYPES_BLESS").is_some() {
        let mut table = String::from(
            "//! The types Tika's path parses with `EmptyParser`, as `just empty-parser-types` \
             writes them.\n\n#[rustfmt::skip]\n",
        );
        table.push_str(&format!(
            "pub(super) static EMPTY: [&str; {}] = [\n",
            empty.len()
        ));
        for media_type in &empty {
            table.push_str(&format!("    {media_type:?},\n"));
        }
        table.push_str("];\n");
        std::fs::write(TYPES, table).unwrap();
        return;
    }
    assert_eq!(types::EMPTY.to_vec(), empty);
}

#[test]
fn extracts_what_tika_parses_with_empty_parser_as_tikas_path_does() {
    let scratch = Scratch::new("empty");
    let binary = scratch.write("tilfældig.bin", &[0x00, 0x9F, 0x3B, 0xFE, 0x10, 0x00, 0x7F]);
    let hwp3 = scratch.write(
        "gammel.hwp",
        b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05",
    );
    let (parsed, deferred) = documents(&EmptyParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed, [hwp3.clone(), binary.clone()]);
    assert_same_as_tika(&[binary.as_path(), hwp3.as_path()], EmptyParser.tika_name());
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&EmptyParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, EmptyParser.tika_name());
}
