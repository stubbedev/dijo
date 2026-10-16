use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{self, assert_same_as_tika_with_passwords};

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

/// The protected packages of Tika's tests this port leaves to Tika, with a part of the reason.
const DEFERRED_CONFORMANCE: [(&str, &str); 3] = [
    ("protect.xlsx", "partial block"),
    ("protectedFile.xlsx", "partial block"),
    ("protected_upper_case.docx", "no stream EncryptionInfo"),
];

/// The protected packages, by file name, with the password that opens them.
const ENCRYPTED: [(&str, &str); 6] = [
    ("password.docx", "extractous"),
    ("protected_normal_case.docx", "password"),
    ("testEXCEL_protected_passtika.xlsx", "tika"),
    ("testEXCEL_protected_passtika_2.xlsx", "tika"),
    ("testPPT_protected_passtika.pptx", "tika"),
    ("testWORD_protected_passtika.docx", "tika"),
];

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Of `documents`, those this port reads, after checking that it leaves exactly those
/// `deferred` names to Tika.
fn ported(documents: &[PathBuf], deferred: &[(&str, &str)]) -> Vec<PathBuf> {
    let mut ported = Vec::new();
    let mut unexpected = Vec::new();
    for document in documents {
        let name = file_name(document);
        let expected = deferred.iter().find(|(deferred, _)| *deferred == name);
        match (parity::deferral(&ProtectedParser, document), expected) {
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
    let (mut documents, deferred) = parity::documents(&ProtectedParser, roots);
    documents.extend(deferred);
    documents.sort();
    documents
}

#[test]
fn opens_the_repositorys_protected_package_as_tikas_path_does() {
    let ported = ported(&corpus(&[Path::new("../test_files/documents")]), &[]);
    assert_eq!(ported.len(), 1, "{ported:?}");
    assert_same_as_tika_with_passwords(&ported, TIKA_PARSER, &ENCRYPTED);
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let ported = ported(
        &corpus(&[&parity::conformance_corpus()]),
        &DEFERRED_CONFORMANCE,
    );
    assert_eq!(ported.len(), 5);
    assert_same_as_tika_with_passwords(&ported, TIKA_PARSER, &ENCRYPTED);
}
