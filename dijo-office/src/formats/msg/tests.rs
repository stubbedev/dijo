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
    use dijo_core::large::LargeFile;
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
    let metadata = file.extract_lines("dijo::OfficeParser", |line| match line.trim() {
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

/// A stream whose chain ends before its size is read as far as it goes, where POI leaves it out,
/// noting the difference (#186) in a warning. A directory named as a value of a multi-valued
/// property, which is none, is left out with a warning, where POI records a chunk of no value.
#[test]
fn reads_streams_as_far_as_their_chains_go() {
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
    assert_eq!(
        read.main.string(0x0C1A).map(|text| text.value.as_str()),
        Some("Ann")
    );
    assert!(read.main.all(0x3A54).next().is_none());
    let read_otherwise = read.read_otherwise().unwrap();
    for what in [
        format!("the stream {SENDER}, whose chain ends before its size"),
        format!("the directory {KEYWORD}"),
    ] {
        assert!(read_otherwise.contains(&what), "{read_otherwise}");
    }
    let (_, difference) = extract(&file, read, &ParserConfigs::default()).unwrap();
    assert!(difference.is_some_and(|Difference(reason)| reason.contains(SENDER)));

    let (_, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&data)
        .unwrap();
    assert_eq!(metadata["dc:title"], ["Minutes"]);
    assert_eq!(metadata["dc:creator"], ["Ann"]);
    let warnings = &metadata[crate::formats::embedded::EXCEPTION_WARNING];
    assert!(
        warnings.iter().any(|warning| warning.contains(SENDER)),
        "{warnings:?}"
    );
}

/// A stream whose chain is whole is read with nothing noted.
#[test]
fn notes_nothing_of_whole_streams() {
    let mut message = message_of_a_bad_chunk_name();
    message
        .entries
        .retain(|(name, _)| *name != units("__substg1.0_003700"));
    let data = crate::ole2::write(&message).unwrap();
    let file = Ole2::open(&data).unwrap();
    let read = chunks::Message::read(&file, file.root(), false).unwrap();
    assert_eq!(read.read_otherwise(), None);
}

/// A message of the multi-valued named properties `PidLidCategories`, of the public strings,
/// holding `categories` in streams of their own, and `PidLidNonSendToTrackStatus`, of the
/// common properties, holding `statuses` in one stream.
fn message_of_categories(categories: &[&str], statuses: &[i32]) -> Vec<u8> {
    use crate::ole2::{write, Node, Storage};
    let mut entries = vec![(
        units("__substg1.0_0037001F"),
        Node::Owned(unicode("Minutes")),
    )];
    let mut properties = vec![0; 32];
    for (kind, id) in [(0x101Fu16, 0x8000u16), (0x1003, 0x8001)] {
        properties.extend(kind.to_le_bytes());
        properties.extend(id.to_le_bytes());
        properties.extend([6, 0, 0, 0]);
        properties.extend([0; 8]);
    }
    entries.push((units("__properties_version1.0"), Node::Owned(properties)));
    let mut lengths = Vec::new();
    for (index, category) in categories.iter().enumerate() {
        let value = unicode(&format!("{category}\0"));
        lengths.extend((value.len() as u32).to_le_bytes());
        entries.push((
            units(&format!("__substg1.0_8000101F-{index:08X}")),
            Node::Owned(value),
        ));
    }
    entries.push((units("__substg1.0_8000101F"), Node::Owned(lengths)));
    let values: Vec<u8> = statuses.iter().flat_map(|n| n.to_le_bytes()).collect();
    entries.push((units("__substg1.0_80011003"), Node::Owned(values)));
    let common = vec![
        0x08, 0x20, 0x06, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46,
    ];
    let mut named = vec![
        (units("__substg1.0_00020102"), Node::Owned(common)),
        (units("__substg1.0_00040102"), Node::Owned(Vec::new())),
    ];
    let mut entry_stream = Vec::new();
    for (property_index, (long_id, guid_index)) in
        [(0x9000u32, 2u16), (0x8543, 3)].into_iter().enumerate()
    {
        let mut entry = long_id.to_le_bytes().to_vec();
        entry.extend((guid_index << 1).to_le_bytes());
        entry.extend((property_index as u16).to_le_bytes());
        let stream = 0x1000 + (long_id ^ (u32::from(guid_index) << 1)) % 0x1F;
        named.push((
            units(&format!("__substg1.0_{stream:04X}0102")),
            Node::Owned(entry.clone()),
        ));
        entry_stream.extend(entry);
    }
    named.push((units("__substg1.0_00030102"), Node::Owned(entry_stream)));
    entries.push((
        units("__nameid_version1.0"),
        Node::Storage(Storage {
            class_id: [0; 16],
            entries: named,
        }),
    ));
    write(&Storage {
        class_id: [0; 16],
        entries,
    })
    .unwrap()
}

/// The values of multi-valued named properties are written, each of its own: strings in the
/// order of their indexes, read as the hexadecimal numbers they are, past the tenth, and
/// integers kept in one stream (#186). Tika writes none of them.
#[test]
fn writes_each_value_of_multi_valued_named_properties() {
    let categories: Vec<String> = (0..12).map(|n| format!("Kategori {n}")).collect();
    let categories: Vec<&str> = categories.iter().map(String::as_str).collect();
    let data = message_of_categories(&categories, &[3, 0, 7]);
    let (_, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&data)
        .unwrap();
    assert_eq!(metadata["mapi:property:PidLidCategories"], categories);
    assert_eq!(
        metadata["mapi:property:PidLidNonSendToTrackStatus"],
        ["3", "0", "7"]
    );
}

/// A message of no values of its multi-valued named properties writes none of them.
#[test]
fn writes_no_multi_valued_named_property_without_values() {
    let data = message_of_categories(&[], &[]);
    let (_, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&data)
        .unwrap();
    assert!(!metadata.contains_key("mapi:property:PidLidCategories"));
    assert!(!metadata.contains_key("mapi:property:PidLidNonSendToTrackStatus"));
}
