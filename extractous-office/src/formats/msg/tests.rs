use std::path::{Path, PathBuf};

use super::*;
use crate::formats::testing;

fn corpus(roots: &[&Path]) -> Vec<PathBuf> {
    let (mut documents, shortfalls) = testing::documents(&MsgParser, roots);
    documents.extend(shortfalls);
    documents.sort();
    documents
}

fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

fn unicode(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn names_recipients_by_their_display_names() {
    let path = corpus(&[&testing::conformance_corpus()])
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

/// A message whose attachment is too large to read whole is read in place, the attachment
/// handed on where it lies and the attachments after it written, in memory that does not grow
/// with it.
#[test]
fn extracts_messages_with_attachments_too_large_to_read_whole_in_bounded_memory() {
    use crate::input::MAX_READ_WHOLE;
    use crate::ole2::{write, Node, Storage};
    use extractous_core::large::LargeFile;
    use std::fmt::Write as _;

    let units = |name: &str| name.encode_utf16().collect::<Vec<u16>>();
    let unicode = |text: &str| {
        text.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>()
    };
    let document = |name: &str, data: Vec<u8>| (units(name), Node::Owned(data));
    let attachment = |name: &str, data: Vec<u8>| {
        Node::Storage(Storage {
            class_id: [0; 16],
            entries: vec![
                document("__substg1.0_3707001F", unicode(name)),
                document("__substg1.0_370E001F", unicode("text/plain")),
                document("__substg1.0_37010102", data),
                document("__properties_version1.0", vec![0; 8]),
            ],
        })
    };
    let mut log = String::new();
    let mut lines = 0u64;
    while log.len() as u64 <= MAX_READ_WHOLE {
        writeln!(log, "Linje {lines} i logbogen").unwrap();
        lines += 1;
    }
    let message = write(&Storage {
        class_id: [0; 16],
        entries: vec![
            document("__substg1.0_0037001F", unicode("Logbogen")),
            document("__substg1.0_1000001F", unicode("Her er logbogen")),
            document("__substg1.0_001A001F", unicode("IPM.Note")),
            document("__properties_version1.0", vec![0; 32]),
            (
                units("__attach_version1.0_#00000000"),
                attachment("logbog.txt", log.into_bytes()),
            ),
            (
                units("__attach_version1.0_#00000001"),
                attachment("slut.txt", b"Slut paa logbogen\n".to_vec()),
            ),
        ],
    })
    .unwrap();
    let file = LargeFile::new("large.msg", &message, 0, |_| Vec::new(), b"");
    drop(message);
    let last = format!("Linje {} i logbogen", lines - 1);
    let (mut first_seen, mut last_seen, mut end_seen) = (false, false, false);
    let metadata = file.extract_lines("extractous::OfficeParser", |line| match line.trim() {
        "Linje 0 i logbogen" => first_seen = true,
        "Slut paa logbogen" => end_seen = last_seen,
        line if line == last => last_seen = first_seen,
        _ => {}
    });
    assert!(first_seen && last_seen && end_seen);
    assert_eq!(metadata["dc:title"], ["Logbogen"]);
}

/// A message of a subject and an entry whose name POI cannot read as a chunk's.
fn message_of_a_bad_chunk_name() -> crate::ole2::Storage<'static> {
    use crate::ole2::{Node, Storage};
    Storage {
        class_id: [0; 16],
        entries: vec![
            (
                units("__substg1.0_0037001F"),
                Node::Owned(unicode("Minutes")),
            ),
            (units("__substg1.0_003700"), Node::Owned(vec![1])),
            (units("__properties_version1.0"), Node::Owned(vec![0; 32])),
        ],
    }
}

/// A message POI throws on fails as Tika fails on it.
#[test]
fn fails_a_message_poi_throws_on_with_tikas_wording() {
    let data = crate::ole2::write(&message_of_a_bad_chunk_name()).unwrap();
    let error = crate::Extractor::new()
        .extract_bytes_to_string(&data)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Parse error occurred : org.apache.tika.exception.TikaException: Unexpected \
         RuntimeException from org.apache.tika.parser.microsoft.OfficeParser <- caused by \
         java.lang.IllegalArgumentException: Invalid chunk name __substg1.0_003700"
    );
}

/// A stored message POI throws on is read as far as its summary information, its failure
/// written for the message storing it to record, as Tika's embedded document extractor
/// records it, rather than leaving the message storing it unread.
#[test]
fn reads_a_stored_message_poi_throws_on_as_its_failure() {
    use crate::ole2::{write, Node, Storage};
    let data = write(&Storage {
        class_id: [0; 16],
        entries: vec![(
            units("__attach_version1.0_#00000000"),
            Node::Storage(Storage {
                class_id: [0; 16],
                entries: vec![(
                    units("__substg1.0_3701000D"),
                    Node::Storage(message_of_a_bad_chunk_name()),
                )],
            }),
        )],
    })
    .unwrap();
    let file = Ole2::open(&data).unwrap();
    let attachment = file
        .child(file.root(), "__attach_version1.0_#00000000")
        .unwrap();
    let stored = file.child(attachment, "__substg1.0_3701000D").unwrap();
    let message = EmbeddedMessage::read(&file, stored, &ParserConfigs::default()).unwrap();
    let Err(thrown) = &message.extracted else {
        panic!("a message POI throws on");
    };
    assert_eq!(
        thrown.to_string(),
        "java.lang.IllegalArgumentException: Invalid chunk name __substg1.0_003700"
    );
}

/// A message submission id stored as a directory, which POI records without reading it, gives
/// no submission id and no date.
#[test]
fn reads_a_submission_id_poi_leaves_unread() {
    use crate::ole2::{write, Node, Storage};
    let mut message = message_of_a_bad_chunk_name();
    message
        .entries
        .retain(|(name, _)| *name != units("__substg1.0_003700"));
    message.entries.push((
        units("__substg1.0_0047000D"),
        Node::Storage(Storage {
            class_id: [0; 16],
            entries: Vec::new(),
        }),
    ));
    let data = write(&message).unwrap();
    let file = Ole2::open(&data).unwrap();
    let read = chunks::Message::read(&file, file.root(), false).unwrap();
    let submissions: Vec<_> = read
        .main
        .chunks
        .iter()
        .filter_map(|chunk| match &chunk.value {
            chunks::Value::Submission(submission) => Some(submission.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        submissions,
        [chunks::Submission {
            id: None,
            date: None
        }]
    );
}

/// A stream that cannot be read is left out of the message, which is read without it, noting what
/// POI reads otherwise (#186) in a warning. A directory of a multi-valued property is never one of
/// its values, as its name, ending in `000D`, holds no decimal index: POI takes it for the
/// property's length, and so does the port, leaving nothing out.
#[test]
fn leaves_out_unreadable_streams() {
    use crate::ole2::{write, Node, Storage};
    const SENDER: &str = "__substg1.0_0C1A001F";
    const KEYWORD: &str = "__substg1.0_3A54101F-0000000D";
    let mut message = message_of_a_bad_chunk_name();
    message
        .entries
        .retain(|(name, _)| *name != units("__substg1.0_003700"));
    message
        .entries
        .push((units(SENDER), Node::Owned(unicode("Ann"))));
    message.entries.push((
        units(KEYWORD),
        Node::Storage(Storage {
            class_id: [0; 16],
            entries: Vec::new(),
        }),
    ));
    let mut data = write(&message).unwrap();
    let name = unicode(SENDER);
    let entry = data
        .windows(name.len())
        .position(|window| window == name)
        .unwrap();
    data[entry + 120..entry + 124].copy_from_slice(&4000u32.to_le_bytes());

    let file = Ole2::open(&data).unwrap();
    let read = chunks::Message::read(&file, file.root(), false).unwrap();
    assert!(read.main.all(0x0C1A).next().is_none());
    assert!(read.main.all(0x3A54).next().is_none());
    let left_out = read.left_out().unwrap();
    assert!(
        left_out.contains(&format!("the stream {SENDER}, which cannot be read")),
        "{left_out}"
    );
    assert!(!left_out.contains(KEYWORD), "{left_out}");
    let (_, difference) = extract(&file, read, &ParserConfigs::default()).unwrap();
    assert!(difference.is_some_and(|Difference(reason)| reason.contains(SENDER)));

    {
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&data)
            .unwrap();
        assert_eq!(metadata["dc:title"], ["Minutes"]);
        let warnings = &metadata[crate::formats::embedded::EXCEPTION_WARNING];
        assert!(
            warnings.iter().any(|warning| warning.contains(SENDER)),
            "{warnings:?}"
        );
    }
}
