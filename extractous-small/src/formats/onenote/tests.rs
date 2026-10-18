use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_normalising,
    assert_same_as_tika_rewriting, conformance_corpus, deferral, documents, ComparedMetadata,
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
    // truncated.one falls back to the legacy dump, which now writes the run "Monday, August
    // 08, 2016" Tika's misplaced whitespace count leaves out (#122):
    // `dumps_the_runs_of_a_section_it_cannot_walk` checks its text.
    let same: Vec<&Path> = same
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| name(path) != "truncated.one")
        .collect();
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
    assert!(deferred.is_empty(), "{deferred:?}");
    let packaged: Vec<String> = parsed
        .iter()
        .filter(|path| {
            let input = std::fs::read(path).unwrap();
            let mut document = Document::default();
            Reader::new(Dif::new(&input), &mut document)
                .header()
                .is_ok_and(|header| header.alternative_packaging)
        })
        .map(|path| name(path))
        .collect();
    assert_eq!(packaged.len(), 4, "{packaged:?}");
    for path in parsed.iter().filter(|path| packaged.contains(&name(path))) {
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(path.to_str().unwrap())
            .unwrap();
        assert!(
            xml.contains("<div class=\"page\">"),
            "{}: {xml}",
            name(path)
        );
        let warnings = metadata
            .get("tk:exception:warn")
            .cloned()
            .unwrap_or_default();
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("legacy text dump")),
            "{}: {warnings:?}",
            name(path)
        );
    }
    assert_eq!(parsed.len(), 15, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_rewriting(
        &paths,
        OneNoteParser.tika_name(),
        as_tika_names_packaged_objects,
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn falls_back_on_damaged_packages_as_tikas_path_does() {
    let (parsed, _) = documents(&OneNoteParser, &[&conformance_corpus()]);
    let path = parsed
        .iter()
        .find(|path| name(path) == "testOneNoteFromOffice365.one")
        .expect("Tika's packaged OneNote section");
    let input = std::fs::read(path).unwrap();
    let scratch = crate::formats::parity::Scratch::new("onenote-damaged-package");
    let mut damaged = Vec::new();
    for share in [3, 5, 9] {
        let cut = input.len() * share / 10;
        damaged.push(scratch.write(&format!("cut-{share}.one"), &input[..cut]));
    }
    for at in [0x60, 0x100, input.len() / 2] {
        let mut flipped = input.clone();
        flipped[at] ^= 0xFF;
        damaged.push(scratch.write(&format!("flipped-{at}.one"), &flipped));
    }
    let compared: Vec<&Path> = damaged
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| deferral(&OneNoteParser, path).is_none())
        .collect();
    assert_eq!(compared.len(), damaged.len(), "{damaged:?}");
    assert_same_as_tika_rewriting(
        &compared,
        OneNoteParser.tika_name(),
        as_tika_names_packaged_objects,
    );
}

/// The run of truncated.one's legacy dump that Tika's dump leaves out, counting whitespace by
/// the characters' indexes (#122), taken out of a text.
fn without_the_run_tika_misjudges(text: &mut String) {
    *text = text.replacen("Monday, August 08, 2016\n", "", 1);
}

fn unchanged(_: &mut String, _: &mut ComparedMetadata, _: &ComparedMetadata) {}

#[test]
fn dumps_the_runs_of_a_section_it_cannot_walk() {
    let path = Path::new("../test_files/documents/onenote/truncated.one");
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    assert_eq!(
        text.trim_start().lines().next(),
        Some("Monday, August 08, 2016")
    );
    assert!(metadata["tk:exception:warn"]
        .iter()
        .any(|warning| warning.contains("legacy text dump")));
    assert_same_as_tika_normalising(
        &[path],
        OneNoteParser.tika_name(),
        unchanged,
        without_the_run_tika_misjudges,
    );
}

#[test]
fn reads_the_header_fields_where_they_are() {
    let input = std::fs::read("../test_files/documents/onenote/section.one").unwrap();
    let mut document = Document::default();
    let header = Reader::new(Dif::new(&input), &mut document)
        .header()
        .unwrap();
    let int_at = |at: usize| i64::from(u32::from_le_bytes(input[at..at + 4].try_into().unwrap()));
    let long_at = |at: usize| i64::from_le_bytes(input[at..at + 8].try_into().unwrap());
    assert_eq!(header.cb_legacy_expected_file_length, int_at(100));
    assert_eq!(header.cb_expected_file_length, long_at(196));
    let mut metadata = TikaMetadata::new();
    set_found_metadata(&mut metadata, walker::Found::default());
    assert_eq!(metadata.get("onenote:creationTimestamp"), None);
    assert_eq!(metadata.get("dcterms:modified"), None);
}
