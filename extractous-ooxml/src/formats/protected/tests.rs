use std::path::{Path, PathBuf};

use super::*;
use crate::config::Password;
use crate::formats::parity;

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

/// The protected packages of Tika's tests Office's default password opens.
const DEFAULT_PASSWORD: [&str; 2] = ["protect.xlsx", "protectedFile.xlsx"];
/// The protected packages of Tika's tests no password opens.
const UNOPENED: [&str; 1] = ["protected_upper_case.docx"];

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

/// Tika's XHTML of a workbook whose sheets are protected, with single breaks and with its head
/// saying so, as the native parser writes it, where Tika's says `false` before it reads the
/// sheets (#95). The metadata, which the comparison also holds alike, keeps Tika's final value.
fn with_protected_sheets_in_head(xhtml: &str) -> String {
    let meta = format!(
        "<meta name=\"{}\" content=",
        crate::formats::xlsx::excel::PROTECTED_WORKSHEET
    );
    parity::without_doubled_breaks(xhtml).replace(
        &format!("{meta}\"false\" />"),
        &format!("{meta}\"true\" />"),
    )
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

/// Tika's test documents, with the packages Office's default password opens, whose compound
/// documents end inside a block POI reads short from a stream, and the one whose streams are
/// named in upper case, which POI finds ignoring case and no password opens.
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]), &[]);
    assert_eq!(ported.len(), 8);
    let named = |names: &[&str]| -> Vec<PathBuf> {
        ported
            .iter()
            .filter(|path| names.contains(&file_name(path).as_str()))
            .cloned()
            .collect()
    };
    let default_password = named(&DEFAULT_PASSWORD);
    assert_eq!(default_password.len(), DEFAULT_PASSWORD.len());
    for document in &default_password {
        parity::assert_same_xhtml_as_tika_with(
            &[document.as_path()],
            TIKA_PARSER,
            &ParserConfigs::default(),
            with_protected_sheets_in_head,
        );
    }
    let unopened = named(&UNOPENED);
    assert_eq!(unopened.len(), UNOPENED.len());
    for document in &unopened {
        let (native, tika) = parity::errors(document, &ParserConfigs::default());
        assert_eq!(native, tika, "{}", document.display());
        assert!(native.contains("EncryptedDocumentException"), "{native}");
    }
    let encrypted = named(&ENCRYPTED.map(|(name, _)| name));
    assert_eq!(
        encrypted.len() + default_password.len() + unopened.len(),
        ported.len()
    );
    assert_same_as_tika_with_single_breaks(&encrypted);
}
