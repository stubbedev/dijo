use std::path::{Path, PathBuf};

use super::format::{self, Detected};
use super::pages::rc4;
use super::*;
use crate::formats::parity::{
    assert_same_as_tika, conformance_corpus, deferral, documents, spools_as_tika, Scratch,
};

const FIXTURES: &str = "../test_files/documents/access";

fn names(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

#[test]
fn extracts_access_databases_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&JackcessParser, &roots);
    assert_eq!(names(&deferred), ["tika-password.accdb"]);
    let reason = deferral(&JackcessParser, &deferred[0]).unwrap();
    assert!(reason.contains("an encrypted database"), "{reason}");
    assert_eq!(
        names(&parsed),
        [
            "ace-calculated.accdb",
            "ace-complex.accdb",
            "ace-extended-dates.accdb",
            "ace-ole.accdb",
            "ace-rich-text.accdb",
            "jet3-ole.mdb",
            "jet3-overflow.mdb",
            "jet3-queries.mdb",
            "jet4-turkish.mdb",
            "jet4-unicode-compression.mdb",
        ]
    );
    let parsed: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&parsed, JackcessParser.tika_name());
}

#[test]
fn spools_embedded_objects_as_tikas_path_does() {
    for name in ["ace-ole.accdb", "jet3-ole.mdb"] {
        let path = Path::new(FIXTURES).join(name);
        for budget in [u64::MAX, 3000] {
            let spooled = spools_as_tika(&path, budget);
            assert!(matches!(spooled, Ok(1..)), "{name}: {spooled:?}");
        }
    }
}

/// A Jet database encoded as Access encodes it once it is given an encoding key: each page
/// after the first RC4 under the key XORed with the page's number.
fn encoded(mut file: Vec<u8>, key: [u8; 4]) -> Vec<u8> {
    let Ok(Detected::Format(format)) = format::detect(&file) else {
        panic!("a Jet database");
    };
    for (number, page) in file.chunks_mut(format.page_size).enumerate().skip(1) {
        let mut page_key = (number as u32).to_le_bytes();
        for (byte, key) in page_key.iter_mut().zip(key) {
            *byte ^= key;
        }
        rc4(&page_key, page);
    }
    let start = format.offset_encoding_key;
    let mask = &format.header_mask[start - format.offset_masked_header..];
    for (i, byte) in key.into_iter().enumerate() {
        file[start + i] = byte ^ mask[i];
    }
    file
}

#[test]
fn reads_encoded_jet_databases_as_tikas_path_does() {
    let scratch = Scratch::new("access-encoded");
    let jet3 = scratch.write(
        "encoded-jet3.mdb",
        &encoded(fixture("jet3-queries.mdb"), [0x12, 0x34, 0x56, 0x78]),
    );
    let jet4 = scratch.write(
        "encoded-jet4.mdb",
        &encoded(fixture("jet4-turkish.mdb"), [0xFE, 0xDC, 0xBA, 0x98]),
    );
    for path in [&jet3, &jet4] {
        assert_eq!(deferral(&JackcessParser, path), None);
    }
    assert_same_as_tika(&[&jet3, &jet4], JackcessParser.tika_name());
}

#[test]
fn leaves_databases_jackcess_throws_reading_to_tika() {
    let scratch = Scratch::new("access-unreadable");
    let original = fixture("jet4-turkish.mdb");
    let truncated = scratch.write("truncated.mdb", &original[..3 * 4096]);
    assert_eq!(
        deferral(&JackcessParser, &truncated).as_deref(),
        Some(
            "an Access database Jackcess throws reading: java.lang.IllegalStateException: \
             invalid page number 6"
        )
    );
}

/// `bytes` with each `from` replaced by `to`, of the same length.
fn replaced(mut bytes: Vec<u8>, from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut start = 0;
    while let Some(at) = bytes[start..]
        .windows(from.len())
        .position(|window| window == from)
    {
        bytes[start + at..start + at + to.len()].copy_from_slice(to);
        start += at + to.len();
    }
    bytes
}

#[test]
fn leaves_jet3_names_the_index_may_not_find_to_tika() {
    let scratch = Scratch::new("access-jet3-names");
    let renamed = scratch.write(
        "renamed.mdb",
        &replaced(fixture("jet3-queries.mdb"), b"Table2", b"T\xe4ble2"),
    );
    assert_eq!(
        deferral(&JackcessParser, &renamed).as_deref(),
        Some(
            "a table name Jackcess may not find through the catalog's index, as it decodes it \
             otherwise than Access encoded it, which the port does not read"
        )
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&JackcessParser, &[&conformance_corpus()]);
    assert_eq!(names(&deferred), ["testAccess2_encrypted.accdb"]);
    assert_eq!(
        names(&parsed),
        [
            "testACCESS.mdb",
            "testAccess2.accdb",
            "testAccess2_2000.mdb",
            "testAccess2_2002-2003.mdb",
            "testAccess_V1997.mdb",
        ]
    );
    let parsed: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&parsed, JackcessParser.tika_name());
}
