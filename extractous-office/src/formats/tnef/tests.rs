#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};

const ATTACH_LONG_FILENAME: u16 = 0x3707;
const ATTACH_EXTENSION: u16 = 0x3703;
const CONVERSATION_TOPIC: u16 = 0x0070;
const RTF_COMPRESSED: u16 = 0x1009;
const ASCII: u16 = 0x001E;
const UNICODE: u16 = 0x001F;
const BINARY: u16 = 0x0102;

/// A TNEF attribute at `level`, with its length and a checksum POI does not check.
fn attribute(level: u8, id: u16, kind: u16, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![level];
    bytes.extend(id.to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend((data.len() as i32).to_le_bytes());
    bytes.extend(data);
    let checksum = data.iter().map(|&byte| u32::from(byte)).sum::<u32>() as u16;
    bytes.extend(checksum.to_le_bytes());
    bytes
}

/// MAPI properties of one value each: type, id and value, the value's length written for the
/// types of variable length.
fn mapi(properties: &[(u16, u16, &[u8])]) -> Vec<u8> {
    let mut bytes = (properties.len() as i32).to_le_bytes().to_vec();
    for &(kind, id, value) in properties {
        bytes.extend(kind.to_le_bytes());
        bytes.extend(id.to_le_bytes());
        if [ASCII, UNICODE, BINARY].contains(&kind) {
            bytes.extend(1i32.to_le_bytes());
            bytes.extend((value.len() as i32).to_le_bytes());
        }
        bytes.extend(value);
        bytes.resize(bytes.len().next_multiple_of(4), 0);
    }
    bytes
}

fn unicode(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Uncompressed RTF in the header of compressed RTF.
fn stored_rtf(rtf: &[u8]) -> Vec<u8> {
    let mut bytes = ((rtf.len() + 12) as u32).to_le_bytes().to_vec();
    bytes.extend((rtf.len() as u32).to_le_bytes());
    bytes.extend(b"MELA");
    bytes.extend([0; 4]);
    bytes.extend(rtf);
    bytes
}

fn message(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = SIGNATURE.to_le_bytes().to_vec();
    bytes.extend(0x0001u16.to_le_bytes());
    for part in parts {
        bytes.extend(part);
    }
    bytes
}

fn message_properties(properties: &[(u16, u16, &[u8])]) -> Vec<u8> {
    attribute(
        level::MESSAGE,
        id::MAPI_PROPERTIES,
        0x0006,
        &mapi(properties),
    )
}

/// An attachment: its render data, which starts it, its title, its MAPI properties and its
/// data.
fn attachment(
    title: Option<&str>,
    properties: &[(u16, u16, &[u8])],
    data: Option<&[u8]>,
) -> Vec<u8> {
    let mut bytes = attribute(level::ATTACHMENT, id::ATTACH_RENDER_DATA, 0x0006, &[0; 14]);
    if let Some(title) = title {
        let mut title = title.as_bytes().to_vec();
        title.push(0);
        bytes.extend(attribute(
            level::ATTACHMENT,
            id::ATTACH_TITLE,
            kind::STRING,
            &title,
        ));
    }
    if let Some(data) = data {
        bytes.extend(attribute(level::ATTACHMENT, id::ATTACH_DATA, 0x0006, data));
    }
    bytes.extend(attribute(
        level::ATTACHMENT,
        id::ATTACHMENT,
        0x0006,
        &mapi(properties),
    ));
    bytes
}

fn rich_message() -> Vec<u8> {
    let rtf = stored_rtf(b"{\\rtf1\\ansi Hej fra K\\'f8benhavn.\\par}");
    let subject = unicode("Møde om havnen");
    message(&[
        message_properties(&[
            (UNICODE, CONVERSATION_TOPIC, &subject),
            (BINARY, RTF_COMPRESSED, &rtf),
        ]),
        attribute(level::MESSAGE, 0x8004, kind::STRING, b"Emne\0"),
        attachment(
            Some("NOTER.TXT"),
            &[(
                UNICODE,
                ATTACH_LONG_FILENAME,
                &unicode("noter om havnen.txt"),
            )],
            Some("Bølgerne går højt i Øresund.\n".as_bytes()),
        ),
        attachment(
            Some("liste.csv"),
            &[],
            Some(b"by;indbyggere\nAarhus;290000\n"),
        ),
        attachment(
            None,
            &[(ASCII, ATTACH_EXTENSION, b".txt\0")],
            Some(b"Uden navn\n"),
        ),
        attachment(None, &[], Some(b"Heller ikke\n")),
    ])
}

#[test]
fn reads_messages_as_poi_does() {
    let input = rich_message();
    let message = Message::read(&input).unwrap();
    assert_eq!(message.subject(), Some("Møde om havnen"));
    assert!(message.rtf().unwrap().starts_with(b"{\\rtf1"));
    let mut unnamed = 0;
    let names: Vec<(String, bool)> = message
        .attachments
        .iter()
        .map(|attachment| attachment.name(&mut unnamed))
        .collect();
    assert_eq!(
        names,
        [
            ("noter om havnen.txt".to_owned(), false),
            ("liste.csv".to_owned(), false),
            ("embedded-0.txt".to_owned(), true),
            ("embedded-1".to_owned(), true),
        ]
    );
}

#[test]
fn throws_where_poi_does() {
    let complete = rich_message();
    for (input, reason) in [
        (b"\x78\x9f\x3e\x23\x01\x00".to_vec(), "signature"),
        (complete[..complete.len() - 1].to_vec(), "Buffer"),
        (message(&[vec![3]]), "Unhandled level 3"),
        (
            message(&[attribute(level::MESSAGE, 0x8005, kind::DATE, &[0; 6])]),
            "Invalid date",
        ),
        (
            message(&[message_properties(&[(0x0099, 0x0001, &[])])]),
            "Unknown type",
        ),
        (
            message(&[message_properties(&[(BINARY, RTF_COMPRESSED, b"LZFu")])]),
            "header",
        ),
    ] {
        let thrown = Message::read(&input).unwrap_err();
        assert!(thrown.0.contains(reason), "{thrown:?}");
    }
}

#[cfg(feature = "tika")]
#[test]
fn extracts_tnef_messages_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&TnefParser, &roots);
    assert!(
        parsed.is_empty() && deferred.is_empty(),
        "{parsed:?} {deferred:?}"
    );

    let scratch = Scratch::new("tnef");
    let rich = scratch.write("winmail.dat", &rich_message());
    let bare = scratch.write(
        "bare.dat",
        &message(&[attachment(Some("a.txt"), &[], Some(b"Alene\n"))]),
    );
    let empty = scratch.write("empty.dat", &message(&[]));
    let without_data = scratch.write(
        "without-data.dat",
        &message(&[
            attachment(Some("a.txt"), &[], Some("Først\n".as_bytes())),
            attachment(Some("b.txt"), &[], None),
        ]),
    );
    let truncated = rich_message();
    let truncated = scratch.write("truncated.dat", &truncated[..truncated.len() - 3]);
    let level = scratch.write("level.dat", &message(&[vec![7]]));
    let (parsed, deferred) = documents(&TnefParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 6, "{parsed:?}");
    assert_same_as_tika(
        &[rich.as_path(), bare.as_path(), empty.as_path()],
        TnefParser.tika_name(),
    );
    assert_fails_as_tika(&[without_data.as_path(), truncated.as_path(), level.as_path()]);
}

/// `testWINMAIL.dat` holds `quick.html`, whose white space after `</body>` the native HTML
/// parser puts in the body (#84).
#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_tnef_messages_as_tikas_path_does() {
    let (parsed, deferred) = documents(&TnefParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, TnefParser.tika_name());
}
