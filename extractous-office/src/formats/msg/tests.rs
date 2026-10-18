use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity;
#[cfg(feature = "tika")]
use crate::formats::parity::assert_same_as_tika_with;

#[cfg(feature = "tika")]
const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

/// The messages this port leaves to Tika, by file name, with a part of the reason.
#[cfg(feature = "tika")]
const DEFERRED: [(&str, &str); 1] = [("testMSG_chinese.msg", "7-bit strings")];

#[cfg(feature = "tika")]
fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Of `documents`, those this port reads, after checking that it leaves exactly those
/// [`DEFERRED`] names to Tika.
#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

#[cfg(feature = "tika")]
fn unicode(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A fixed-length property of a properties stream: its type, its id and its value.
#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
/// A stored message spools as a message file of its own, its properties stream headed as a
/// message's is (#112), where Tika's path spools it empty (#116).
#[test]
fn spools_a_stored_message_as_a_message_of_its_own() {
    let input = message_with_a_stored_message();
    let file = Ole2::open(&input).unwrap();
    let attachment = file
        .child(file.root(), "__attach_version1.0_#00000000")
        .unwrap();
    let stored = file.child(attachment, "__substg1.0_3701000D").unwrap();
    let data = standalone(&file, stored).unwrap();
    let message = Ole2::open(&data).unwrap();
    let properties = message.child(message.root(), PROPERTIES).unwrap();
    assert_eq!(message.read(properties).unwrap().len(), MESSAGE_HEADER);
    let (text, _) = crate::Extractor::new()
        .extract_bytes_to_string(&data)
        .unwrap();
    assert!(text.contains("The minutes of the meeting"), "{text}");
    let scratch = crate::formats::parity::Scratch::new("msg-stored-spool");
    let path = scratch.write("stored.msg", &input);
    let spool = crate::EmbeddedSpool {
        dir: scratch.directory().to_path_buf(),
        byte_budget: u64::MAX,
    };
    let (mut reader, _) = crate::Extractor::new()
        .extract_file_spooling_embedded(path.to_str().unwrap(), &spool)
        .unwrap();
    std::io::copy(&mut reader, &mut std::io::sink()).unwrap();
    let embedded = reader.embedded().unwrap();
    let [spooled] = &embedded[..] else {
        panic!("one spooled message: {embedded:?}");
    };
    assert_eq!(std::fs::read(&spooled.path).unwrap(), data);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_messages() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]));
    assert_eq!(ported.len(), 11);
    let documents: Vec<&Path> = ported.iter().map(PathBuf::as_path).collect();
    let departures = [RECIPIENT_NAMES, crate::formats::html::DEPARTURES].concat();
    parity::assert_same_as_tika_departing(&documents, TIKA_PARSER, &departures);
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn names_recipients_by_their_display_names() {
    let path = corpus(&[&parity::conformance_corpus()])
        .into_iter()
        .find(|path| path.ends_with("testMSG.msg"))
        .expect("Tika's test message");
    let (_, metadata) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    let get = |name: &str| metadata.get(name).cloned().unwrap_or_default();
    assert_eq!(get("message:to-name"), ["tika-dev@lucene.apache.org"]);
    assert_eq!(get("message:to-display-name"), [""]);
}

/// A message whose 7-bit strings Tika may decode otherwise is read natively, whole and in the
/// charset the message gives, as a build without Tika takes it (#144).
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn reads_natively_a_message_whose_7_bit_strings_tika_may_decode_otherwise() {
    let path = corpus(&[&parity::conformance_corpus()])
        .into_iter()
        .find(|path| path.ends_with("testMSG_chinese.msg"))
        .expect("Tika's test message");
    let (crate::formats::Difference(reason), text) = parity::differing_reading(&MsgParser, &path);
    assert!(reason.contains("7-bit strings"), "{reason}");
    assert!(
        text.contains("Alfresco MSG format testing ( MSG 格式測試 ) 中文測試"),
        "{text}"
    );
}
