use super::wps4::tests::Mn0;
use super::wps8::tests::{contents, utf16};
use super::{DATABASE, MEDIA_TYPE};
use crate::ole2::tests::compound;

const DOCUMENTS: &str = "../test_files/documents";
/// The sentence of Tika's test documents of Works 4, 2000 and 7.
const SENTENCE: &str = "This is an example document made with Microsoft Works Word Processor 7.0";

fn extract(input: &[u8]) -> (String, crate::extracted::Metadata) {
    crate::Extractor::new()
        .extract_bytes_to_string(input)
        .unwrap()
}

/// Tika's test documents of Works 4 (`MN0`), Works 2000 (`CHNKINK`) and Works 7 (`CHNKWKS`),
/// which Tika reads no text of, are read for their text.
#[test]
fn reads_the_text_of_tikas_works_documents() {
    for name in ["works4.wps", "works2000.wps", "works7.wps"] {
        let input = std::fs::read(format!("{DOCUMENTS}/{name}")).unwrap();
        let (text, metadata) = extract(&input);
        assert_eq!(text.trim(), SENTENCE, "{name}");
        assert_eq!(metadata["Content-Type"], [MEDIA_TYPE], "{name}");
        assert_eq!(
            metadata["tk:parsed-by"].last().unwrap(),
            "dijo::WorksParser"
        );
    }
}

#[test]
fn writes_the_stories_of_a_works_4_document_in_their_order() {
    let mn0 = Mn0 {
        header: b"Kapitel 1\r\n",
        footer: b"Side \x02\r\n",
        body: b"Havnen i \xc5lborg\x06\r\n",
        notes: &[b"\x06 Fjorden\r\n"],
        fonts: &[],
        runs: &[],
    }
    .bytes();
    let (text, _) = extract(&compound(&["MatOST/", "MN0"], &mn0));
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines,
        ["Kapitel 1", "Havnen i \u{c5}lborg", " Fjorden", "Side "]
    );
}

#[test]
fn reads_a_works_2000_document_by_its_contents() {
    let text = "Havnen i \u{c5}lborg\r";
    let mut stream = contents(&[(*b"TEXT", *b"TEXT", 0, utf16(text))]);
    stream[..8].copy_from_slice(b"CHNKINK ");
    let (extracted, metadata) = extract(&compound(&["CONTENTS", "SPELLING"], &stream));
    assert_eq!(extracted.trim(), "Havnen i \u{c5}lborg");
    assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
}

/// A database, whose `MN0` stream starts as one does, is read for its summary information only.
#[test]
fn reads_a_database_for_its_summary_information_only() {
    let mut mn0 = DATABASE.to_vec();
    mn0.resize(0x200, 0);
    let (text, metadata) = extract(&compound(&["MatOST/", "MN0"], &mn0));
    assert_eq!(text.trim(), "");
    assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
}

#[test]
fn fails_a_malformed_document_with_what_is_wrong() {
    let mut mn0 = vec![6, 0xFE];
    mn0.resize(0x80, 0);
    let error = crate::Extractor::new()
        .extract_bytes_to_string(&compound(&["MatOST/", "MN0"], &mn0))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("MN0: the stream is 128 bytes long"),
        "{error}"
    );
}
