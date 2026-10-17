use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, deferral, documents,
};

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn extracts_onenote_sections_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&OneNoteParser, &roots);
    assert!(deferred.is_empty(), "{deferred:?}");
    let (failing, same): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| name(path) == "short-header.one");
    assert_eq!(failing.len(), 1);
    assert_eq!(
        same.iter().map(|path| name(path)).collect::<Vec<_>>(),
        ["embedded-word.one", "section.one", "truncated.one"]
    );
    let same: Vec<&Path> = same.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&same, OneNoteParser.tika_name());
    assert_fails_as_tika(&[failing[0].as_path()]);
    let truncated = std::fs::read("../test_files/documents/onenote/truncated.one").unwrap();
    let mut document = Document::default();
    let mut reader = Reader::new(Dif::new(&truncated), &mut document);
    let header = reader.header().unwrap();
    assert_eq!(
        reader.root(header.fcr_file_node_list_root),
        Err(Thrown(
            "java.io.IOException: requesting seek past end of stream".to_owned()
        ))
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&OneNoteParser, &[&conformance_corpus()]);
    let mut deferred_names: Vec<String> = deferred.iter().map(|path| name(path)).collect();
    deferred_names.sort();
    assert_eq!(
        deferred_names,
        [
            "testOneNote2007OrEarlier.one",
            "testOneNoteEmbeddedImage.one",
            "testOneNoteFromOffice365-2.one",
            "testOneNoteFromOffice365.one",
        ]
    );
    for path in &deferred {
        let reason = deferral(&OneNoteParser, path).unwrap();
        assert!(reason.contains("MS-FSSHTTPB"), "{reason}");
    }
    assert_eq!(parsed.len(), 11, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, OneNoteParser.tika_name());
}
