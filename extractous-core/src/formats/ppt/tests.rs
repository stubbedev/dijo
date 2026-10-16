use std::path::{Path, PathBuf};

use super::*;
use crate::config::Password;
use crate::formats::parity::{self, assert_fails_as_tika, assert_same_as_tika_with};

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";
const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];

/// The presentations this port leaves to Tika, by file name, with a part of the reason.
const DEFERRED: [(&str, &str); 1] = [("testPPT_skipBadCompressedObject.ppt", "does not inflate")];

/// The encrypted presentations, by file name, with the password that opens them.
const ENCRYPTED: [(&str, &str); 2] = [
    ("testPPT_protected_passtika.ppt", "tika"),
    ("tika-password.ppt", "tika"),
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

/// Asserts that the encrypted presentations among `documents` fail as Tika's path fails on them
/// without a password and with a wrong one, and that the right one opens them as it opens them;
/// and that the rest extract as Tika's path extracts them.
fn assert_same_as_tika_with_passwords(documents: &[PathBuf]) {
    let (encrypted, plain): (Vec<&Path>, Vec<&Path>) = documents
        .iter()
        .map(PathBuf::as_path)
        .partition(|document| password_of(document).is_some());
    assert_same_as_tika_with(&plain, TIKA_PARSER, &ParserConfigs::default());
    assert_fails_as_tika(&encrypted);
    let wrong = ParserConfigs::default().with_password(Some(&Password::new("wrong")));
    for document in encrypted {
        let password = Password::new(password_of(document).expect("an encrypted presentation"));
        let configs = ParserConfigs::default().with_password(Some(&password));
        assert_same_as_tika_with(&[document], TIKA_PARSER, &configs);
        let input = std::fs::read(document).unwrap();
        for configs in [ParserConfigs::default(), wrong.clone()] {
            let native = match read(&input, &configs) {
                Err(Declined::Fails(error)) => format!("{error:?}"),
                other => panic!("{document:?} does not fail: {:?}", other.map(|_| ())),
            };
            let tika = format!("{:?}", parity::tika_error(document, &configs));
            assert_eq!(native, tika);
        }
    }
}

fn password_of(document: &Path) -> Option<&'static str> {
    let name = file_name(document);
    ENCRYPTED
        .iter()
        .find(|(encrypted, _)| *encrypted == name)
        .map(|(_, password)| *password)
}

#[test]
fn defers_macros_when_they_are_extracted() {
    let configs = ParserConfigs {
        office: crate::config::OfficeParserConfig::new().set_extract_macros(true),
        ..ParserConfigs::default()
    };
    assert!(
        matches!(read(b"", &configs), Err(Declined::Deferred(reason)) if reason.contains("macros"))
    );
}

#[test]
fn defers_what_is_no_presentation() {
    let compound = crate::ole2::tests::compound(&["WordDocument"], b"");
    assert!(matches!(
        read(&compound, &ParserConfigs::default()),
        Err(Declined::Deferred(reason)) if reason.contains("no presentation")
    ));
}

#[test]
fn opens_the_repositorys_encrypted_presentation_as_tikas_path_does() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let ported = ported(&corpus(&roots));
    assert_eq!(ported.len(), 1, "{ported:?}");
    assert_same_as_tika_with_passwords(&ported);
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]));
    assert_eq!(ported.len(), 21);
    assert_same_as_tika_with_passwords(&ported);
}
