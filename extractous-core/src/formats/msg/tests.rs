use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{self, assert_same_as_tika_with};

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

/// The messages this port leaves to Tika, by file name, with a part of the reason.
const DEFERRED: [(&str, &str); 7] = [
    ("testMSG_Appointment.msg", "identity hash codes"),
    ("testMSG_StickyNote.msg", "identity hash codes"),
    ("testMSG_Task.msg", "identity hash codes"),
    ("testMSG_att_doc.msg", "identity hash codes"),
    ("testMSG_forwarded.msg", "identity hash codes"),
    ("testMSG_att_msg.msg", "embedded Outlook message"),
    ("testMSG_chinese.msg", "partial block"),
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
        match (parity::deferral(&MsgParser, document), expected) {
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
    let (mut documents, deferred) = parity::documents(&MsgParser, roots);
    documents.extend(deferred);
    documents.sort();
    documents
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_messages() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]));
    assert_eq!(ported.len(), 5);
    let documents: Vec<&Path> = ported.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_with(&documents, TIKA_PARSER, &ParserConfigs::default());
}
