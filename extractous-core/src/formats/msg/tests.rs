use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{self, assert_same_as_tika_with};

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

/// The messages this port leaves to Tika, by file name, with a part of the reason.
const DEFERRED: [(&str, &str); 1] = [("testMSG_chinese.msg", "partial block")];

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

fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

fn unicode(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A fixed-length property of a properties stream: its type, its id and its value.
fn property(kind: u16, id: u16, value: &[u8]) -> Vec<u8> {
    let mut bytes = kind.to_le_bytes().to_vec();
    bytes.extend(id.to_le_bytes());
    bytes.extend([6, 0, 0, 0]);
    let mut padded = value.to_vec();
    padded.resize(8, 0);
    bytes.extend(padded);
    bytes
}

/// A message from Ann with a text body, which carries a message from Bob stored in its
/// attachment's storage, its properties stream headed as an embedded message's is.
fn message_with_a_stored_message() -> Vec<u8> {
    use crate::ole2::{write, Node, Storage};
    let document = |name: &str, data: Vec<u8>| (units(name), Node::Document(data));
    let stored = Storage {
        class_id: [0; 16],
        entries: vec![
            document("__substg1.0_0037001F", unicode("Minutes")),
            document("__substg1.0_0C1A001F", unicode("Bob")),
            document(
                "__substg1.0_1000001F",
                unicode("The minutes of the meeting"),
            ),
            document("__substg1.0_001A001F", unicode("IPM.Note")),
            document("__properties_version1.0", vec![0; 24]),
        ],
    };
    let mut attachment_properties = vec![0; 8];
    attachment_properties.extend(property(0x0003, 0x3705, &5i32.to_le_bytes()));
    let attachment = Storage {
        class_id: [0; 16],
        entries: vec![
            document("__substg1.0_3001001F", unicode("Minutes")),
            document("__properties_version1.0", attachment_properties),
            (units("__substg1.0_3701000D"), Node::Storage(stored)),
        ],
    };
    write(&Storage {
        class_id: [0; 16],
        entries: vec![
            document("__substg1.0_0037001F", unicode("Forwarded minutes")),
            document("__substg1.0_0C1A001F", unicode("Ann")),
            document("__substg1.0_1000001F", unicode("Here are the minutes")),
            document("__substg1.0_001A001F", unicode("IPM.Note")),
            document("__properties_version1.0", vec![0; 32]),
            (
                units("__attach_version1.0_#00000000"),
                Node::Storage(attachment),
            ),
        ],
    })
    .unwrap()
}

#[test]
fn reads_a_stored_message_as_tikas_path_reads_it() {
    let scratch = parity::Scratch::new("msg-stored-message");
    let path = scratch.write("stored.msg", &message_with_a_stored_message());
    assert_eq!(parity::deferral(&MsgParser, &path), None);
    assert_same_as_tika_with(&[&path], TIKA_PARSER, &ParserConfigs::default());
    let (text, _) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    assert!(text.contains("The minutes of the meeting"), "{text}");
    for budget in [u64::MAX, 100] {
        let spooled = parity::spools_as_tika(&path, budget);
        assert_eq!(spooled, Ok(1));
    }
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_messages() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]));
    assert_eq!(ported.len(), 5);
    let documents: Vec<&Path> = ported.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_with(&documents, TIKA_PARSER, &ParserConfigs::default());
}
