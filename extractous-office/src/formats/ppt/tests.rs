#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity;
#[cfg(feature = "tika")]
use crate::formats::parity::assert_same_as_tika_with_passwords;

#[cfg(feature = "tika")]
const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";
#[cfg(feature = "tika")]
const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];

/// The presentations this port leaves to Tika, by file name, with a part of the reason.
#[cfg(feature = "tika")]
const DEFERRED: [(&str, &str); 0] = [];

/// The encrypted presentations, by file name, with the password that opens them.
#[cfg(feature = "tika")]
const ENCRYPTED: [(&str, &str); 2] = [
    ("testPPT_protected_passtika.ppt", "tika"),
    ("tika-password.ppt", "tika"),
];

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
        match (parity::deferral(&PptParser, document), expected) {
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

#[cfg(feature = "tika")]
fn corpus(roots: &[&Path]) -> Vec<PathBuf> {
    let (mut documents, deferred) = parity::documents(&PptParser, roots);
    documents.extend(deferred);
    documents.sort();
    documents
}

#[test]
fn defers_what_is_no_presentation() {
    let compound = crate::ole2::tests::compound(&["WordDocument"], b"");
    assert!(matches!(
        plan(&compound, &ParserConfigs::default()),
        Err(Decline::Deferred(reason)) if reason.contains("no presentation")
    ));
}

#[cfg(feature = "tika")]
#[test]
fn opens_the_repositorys_encrypted_presentation_as_tikas_path_does() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let ported = ported(&corpus(&roots));
    assert_eq!(ported.len(), 1, "{ported:?}");
    assert_same_as_tika_with_passwords(&ported, TIKA_PARSER, &ENCRYPTED);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let (visio_nuls, ported): (Vec<PathBuf>, Vec<PathBuf>) =
        ported(&corpus(&[&parity::conformance_corpus()]))
            .into_iter()
            .partition(|path| file_name(path) == VISIO_NULS);
    assert_eq!(ported.len(), 21);
    assert_same_as_tika_with_passwords(&ported, TIKA_PARSER, &ENCRYPTED);
    assert_eq!(visio_nuls.len(), 1);
    parity::assert_same_xhtml_as_tika_with(
        &[visio_nuls[0].as_path()],
        TIKA_PARSER,
        &ParserConfigs::default(),
        without_visio_nuls,
    );
}

/// The presentation of Tika's tests with an object whose data does not inflate, which both paths
/// record as a warning, and an embedded Visio 5 drawing whose texts end in a NUL. The native
/// parser ends each text at its NUL where HDGF keeps it, which Tika's path writes as a U+FFFD
/// (#129).
#[cfg(feature = "tika")]
const VISIO_NULS: &str = "testPPT_skipBadCompressedObject.ppt";

/// Tika's XHTML without the U+FFFD HDGF leaves at the end of a Visio 5 text (#129).
#[cfg(feature = "tika")]
fn without_visio_nuls(xhtml: &str) -> String {
    xhtml.replace("\u{FFFD}</p>", "</p>")
}

/// A presentation too large to read whole, its size in an embedded object, is read in place:
/// its records built from the document stream where they lie and the object handed on from
/// there, in memory that does not grow with it.
#[test]
fn extracts_presentations_too_large_to_read_whole_in_bounded_memory() {
    use crate::input::MAX_READ_WHOLE;
    use crate::ole2::{write, Node, Storage};
    use extractous_core::large::LargeFile;
    use records::kind;
    use records::tests::record;
    use std::fmt::Write as _;

    let mut log = String::new();
    let mut lines = 0u64;
    while log.len() as u64 <= MAX_READ_WHOLE {
        writeln!(log, "Linje {lines} i logbogen").unwrap();
        lines += 1;
    }
    let mut stream = record(
        0x000F,
        kind::DOCUMENT,
        &record(0x0001, kind::DOCUMENT_ATOM, &[0; 40]),
    );
    let object_at = stream.len() as u32;
    stream.extend(record(0, kind::EX_OLE_OBJ_STG, log.as_bytes()));
    drop(log);
    let pointers_at = stream.len() as u32;
    let mut pointers = ((2u32 << 20) | 1).to_le_bytes().to_vec();
    pointers.extend(0u32.to_le_bytes());
    pointers.extend(object_at.to_le_bytes());
    stream.extend(record(0, kind::PERSIST_PTR_INCREMENTAL_BLOCK, &pointers));
    let edit_at = stream.len() as u32;
    let mut edit = vec![0; 32];
    edit[12..16].copy_from_slice(&pointers_at.to_le_bytes());
    edit[16..20].copy_from_slice(&1u32.to_le_bytes());
    edit[28..32].copy_from_slice(&(-1i32).to_le_bytes());
    stream.extend(record(0, kind::USER_EDIT_ATOM, &edit));
    let mut current_user = vec![0; 28];
    current_user[16..20].copy_from_slice(&edit_at.to_le_bytes());
    let units = |name: &str| name.encode_utf16().collect::<Vec<u16>>();
    let presentation = write(&Storage {
        class_id: [0; 16],
        entries: vec![
            (units("Current User"), Node::Owned(current_user)),
            (units("PowerPoint Document"), Node::Owned(stream)),
        ],
    })
    .unwrap();
    let file = LargeFile::new("large.ppt", &presentation, 0, |_| Vec::new(), b"");
    drop(presentation);
    let last = format!("Linje {} i logbogen", lines - 1);
    let (mut first_seen, mut last_seen) = (false, false);
    let metadata = file.extract_lines("extractous::OfficeParser", |line| match line.trim() {
        "Linje 0 i logbogen" => first_seen = true,
        line if line == last => last_seen = first_seen,
        _ => {}
    });
    assert!(first_seen && last_seen);
    assert_eq!(metadata["Content-Type"], ["application/vnd.ms-powerpoint"]);
}
