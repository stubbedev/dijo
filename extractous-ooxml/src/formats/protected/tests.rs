use std::path::{Path, PathBuf};

use super::*;
use crate::config::Password;
use crate::formats::parity;

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

/// Asserts that each document, all of them encrypted, fails without its password and with a
/// wrong one as Tika's path fails on it, and extracts with its password the XHTML and metadata
/// Tika's path does, once Tika's doubled line breaks and tabs are single (#119).
fn assert_same_as_tika_with_single_breaks(documents: &[PathBuf]) {
    let wrong = ParserConfigs::default().with_password(Some(&Password::new("wrong")));
    for document in documents {
        let name = file_name(document);
        let (_, password) = ENCRYPTED
            .iter()
            .find(|(encrypted, _)| *encrypted == name)
            .unwrap_or_else(|| panic!("{name} is not encrypted"));
        let configs = ParserConfigs::default().with_password(Some(&Password::new(*password)));
        parity::assert_same_xhtml_as_tika_with(
            &[document.as_path()],
            TIKA_PARSER,
            &configs,
            parity::without_doubled_breaks,
        );
        for configs in [ParserConfigs::default(), wrong.clone()] {
            let (native, tika) = parity::errors(document, &configs);
            assert_eq!(native, tika, "{name}");
        }
    }
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
    assert_same_as_tika_with_single_breaks(&ported);
    let (xml, _) = crate::Extractor::new()
        .set_passwords([Password::new("extractous")])
        .set_xml_output(true)
        .extract_file_to_string(ported[0].to_str().unwrap())
        .unwrap();
    assert!(
        xml.contains("</p>\n<p") && !xml.contains("</p>\n\n"),
        "{xml}"
    );
}

/// The packages whose compound document POI reads short from a stream are read natively, whole,
/// as a build without Tika takes them (#144).
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn reads_natively_the_packages_poi_reads_short_from_a_stream() {
    let documents = corpus(&[&parity::conformance_corpus()]);
    for name in ["protect.xlsx", "protectedFile.xlsx"] {
        let path = documents
            .iter()
            .find(|path| path.ends_with(name))
            .unwrap_or_else(|| panic!("Tika's test document {name}"));
        let (Difference(reason), text) = parity::differing_reading(&ProtectedParser, path);
        assert!(reason.contains("partial block"), "{name}: {reason}");
        assert!(
            text.contains("При запуске пробной версии Office 2007"),
            "{name}: {text}"
        );
    }
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let ported = ported(
        &corpus(&[&parity::conformance_corpus()]),
        &DEFERRED_CONFORMANCE,
    );
    assert_eq!(ported.len(), 5);
    assert_same_as_tika_with_single_breaks(&ported);
}
