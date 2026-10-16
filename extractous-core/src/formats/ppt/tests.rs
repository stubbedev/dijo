use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{self, assert_same_as_tika_with};

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";
const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];

/// The presentations this port leaves to Tika, by file name, with a part of the reason.
const DEFERRED: [(&str, &str); 3] = [
    ("testPPT_protected_passtika.ppt", "encrypted"),
    ("tika-password.ppt", "encrypted"),
    ("testPPT_skipBadCompressedObject.ppt", "does not inflate"),
];

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Of `documents`, those this port reads, after checking that it leaves exactly those
/// [`DEFERRED`] names to Tika.
fn ported(documents: &[PathBuf]) -> Vec<PathBuf> {
    let mut ported = Vec::new();
    let mut unexpected = Vec::new();
    for document in documents {
        let name = file_name(document);
        let expected = DEFERRED.iter().find(|(deferred, _)| *deferred == name);
        match (parity::deferral(&PptParser, document), expected) {
            (None, None) => ported.push(document.clone()),
            (Some(reason), Some((_, why))) if reason.contains(why) => {}
            (reason, expected) => {
                unexpected.push(format!("{name}: {reason:?}, expected {expected:?}"))
            }
        }
    }
    assert!(unexpected.is_empty(), "{}", unexpected.join("\n"));
    ported
}

fn corpus(roots: &[&Path]) -> Vec<PathBuf> {
    let (mut documents, deferred) = parity::documents(&PptParser, roots);
    documents.extend(deferred);
    documents.sort();
    documents
}

#[test]
fn defers_macros_when_they_are_extracted() {
    let config = OfficeParserConfig::new().set_extract_macros(true);
    assert!(matches!(read(b"", &config), Err(Unported(reason)) if reason.contains("macros")));
}

#[test]
fn defers_what_is_no_presentation() {
    let compound = crate::ole2::tests::compound(&["WordDocument"], b"");
    let config = OfficeParserConfig::default();
    assert!(matches!(
        read(&compound, &config),
        Err(Unported(reason)) if reason.contains("no presentation")
    ));
}

#[test]
fn leaves_the_repositorys_encrypted_presentation_to_tika() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let ported = ported(&corpus(&roots));
    assert!(ported.is_empty(), "{ported:?}");
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]));
    assert_eq!(ported.len(), 20);
    let documents: Vec<&Path> = ported.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_with(&documents, TIKA_PARSER, &ParserConfigs::default());
}
