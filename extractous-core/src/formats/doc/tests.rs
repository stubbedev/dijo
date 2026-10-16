use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{self, assert_same_as_tika_with_passwords};

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";
const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];

/// The documents this port leaves to Tika, by file name, with a part of the reason.
const DEFERRED: [(&str, &str); 0] = [];

/// The encrypted documents, by file name, with the password that opens them.
const ENCRYPTED: [(&str, &str); 2] = [
    ("testWORD_protected_passtika.doc", "tika"),
    ("tika-password.doc", "tika"),
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
        match (parity::deferral(&DocParser, document), expected) {
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
    let (mut documents, deferred) = parity::documents(&DocParser, roots);
    documents.extend(deferred);
    documents.sort();
    documents
}

#[test]
fn decrypts_an_encrypted_document_with_its_password_only() {
    let input = std::fs::read("../test_files/documents/encrypted/tika-password.doc").unwrap();
    let configs = |password: &str| {
        ParserConfigs::default().with_password(Some(&crate::config::Password::new(password)))
    };
    let (_, extracted) = read(&input, &configs("tika")).unwrap();
    let text: String = extracted
        .out
        .iter()
        .filter_map(|out| match out {
            extractor::Out::Text(text) => Some(String::from_utf16_lossy(text)),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("This is an encrypted Word 2007 File."),
        "{text}"
    );
    for configs in [ParserConfigs::default(), configs("wrong")] {
        assert!(matches!(
            read(&input, &configs),
            Err(Declined::Fails(crate::Error::Encrypted(_)))
        ));
    }
}

#[test]
#[ignore = "runs Tika on every Word document of the repository"]
fn extracts_what_tikas_path_extracts_from_the_repositorys_documents() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let ported = ported(&corpus(&roots));
    assert_eq!(ported.len(), 5);
    assert_same_as_tika_with_passwords(&ported, TIKA_PARSER, &ENCRYPTED);
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]));
    assert_eq!(ported.len(), 42);
    assert_same_as_tika_with_passwords(&ported, TIKA_PARSER, &ENCRYPTED);
}
