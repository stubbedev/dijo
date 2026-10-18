use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika_departing, assert_same_as_tika_normalising,
    assert_same_as_tika_rewriting, conformance_corpus, deferral, documents, ComparedMetadata,
    Departure,
};

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// The paragraphs of the current revision, which the walk writes once each in the order of the
/// document, where Tika's path writes those of every revision in the order of its walk, and
/// misses others (#122).
const CURRENT: Departure = Departure::Paragraphs("#122", &[]);

/// The sections the walk reads, by file name, as they depart from Tika's path (#122): their
/// paragraphs, Tika's but those it writes that only earlier revisions hold, and the metadata
/// the current revision gives otherwise. The authors are those of what it holds, and Tika's
/// path finds some of them in other roles or not at all; the earliest creation time is that of
/// what it holds; and the parsers of the embedded documents come in the order the document
/// holds those.
const WALKED: &[(&str, &[Departure])] = &[
    ("embedded-word.one", &[CURRENT]),
    ("section.one", &[CURRENT]),
    (
        "test-tika-3970-dupetext.one",
        &[
            Departure::Paragraphs("#122", &["This little piggy went to market"]),
            Departure::Values("tk:parsed-by-full-set", "#122"),
        ],
    ),
    (
        "test-tika-4303-Chinese-notes.one",
        &[Departure::Paragraphs("#122", &["zhongwen"])],
    ),
    (
        "testOneNote.one",
        &[Departure::Paragraphs(
            "#122",
            &["Note-ccn-tes", "Note-ccn-test-mmmm"],
        )],
    ),
    (
        "testOneNote1.one",
        &[
            CURRENT,
            Departure::Values("onenote:originalAuthors", "#122"),
        ],
    ),
    (
        "testOneNote2.one",
        &[
            Departure::Paragraphs(
                "#122",
                &[
                    "(Pages are over there)",
                    "1. Take notes anywhere on the page",
                    "2. Get organized",
                    "Add pages inside of each section:",
                    "Add sections for activities like:",
                    "Create your first page",
                    "For more tips, check out 30 second videos",
                    "OneNote: one place for all of your notes",
                    "Section1She",
                    "Section1Sheet",
                    "Section2TextArea1",
                    "Write your name here",
                    "You start with \"My Notebook\" - everything lives in here",
                    "You're in the Quick Notes section - use it for random notes",
                    "neat info about totally killin it bro",
                ],
            ),
            Departure::Values("dc:creator", "#122"),
            Departure::Values("onenote:creationTimestamp", "#122"),
        ],
    ),
    ("testOneNote2016.one", &[CURRENT]),
    (
        "testOneNote3.one",
        &[Departure::Paragraphs(
            "#122",
            &[
                "Quit doing horribl",
                "Quit doing horrible things to me. Dang you. ",
                "Section2H. ",
                "Section3TextArea1",
                "awesome information about sports or some crap like that.",
                "neat info about totally killin i",
            ],
        )],
    ),
    (
        "testOneNote4.one",
        &[Departure::Paragraphs(
            "#122",
            &[
                "Section",
                "Section 3",
                "Section3",
                "Section3 title",
                "Section3Title",
                "This is impo",
                "Title text is here ",
                "Title text is here.",
                "Why would you do that ",
                "awesome information about sports or some",
                "way too much inf",
            ],
        )],
    ),
    ("testOneNoteEmbeddedWordDoc.one", &[CURRENT]),
];

/// The departures of the section at `path`, if the walk reads it.
fn walked(path: &Path) -> Option<&'static [Departure<'static>]> {
    WALKED
        .iter()
        .find(|(file, _)| *file == name(path))
        .map(|(_, departures)| *departures)
}

/// The paragraphs Tika's path writes from earlier revisions of the walked section at `path`.
fn superseded(path: &Path) -> &'static [&'static str] {
    walked(path)
        .unwrap_or_else(|| panic!("{path:?} is walked"))
        .iter()
        .find_map(|departure| match departure {
            Departure::Paragraphs(_, superseded) => Some(*superseded),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{path:?} departs in its paragraphs"))
}

/// Asserts that each section the walk reads extracts as Tika's path does, but for its
/// departures.
fn assert_walked_as_tika(paths: &[&Path]) {
    for path in paths {
        let departures = walked(path).unwrap_or_else(|| panic!("{path:?} is walked"));
        assert_same_as_tika_departing(&[path], OneNoteParser.tika_name(), departures);
    }
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
    assert_walked_as_tika(&same);
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
    let (walked_paths, others): (Vec<&Path>, Vec<&Path>) = parsed
        .iter()
        .map(PathBuf::as_path)
        .partition(|path| walked(path).is_some());
    assert_eq!(walked_paths.len(), WALKED.len() - 2, "{walked_paths:?}");
    assert_walked_as_tika(&walked_paths);
    assert_same_as_tika_rewriting(
        &others,
        OneNoteParser.tika_name(),
        as_tika_names_packaged_objects,
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn leaves_out_only_the_paragraphs_later_revisions_replaced() {
    let (parsed, _) = documents(&OneNoteParser, &[&conformance_corpus()]);
    for path in parsed.iter().filter(|path| walked(path).is_some()) {
        let input = std::fs::read(path).unwrap();
        let mut document = Document::default();
        let mut reader = Reader::new(Dif::new(&input), &mut document);
        let header = reader.header().unwrap();
        reader.root(header.fcr_file_node_list_root).unwrap();
        let (declared, current) = walker::declared_texts(&document, &input);
        for paragraph in superseded(path) {
            assert!(declared.contains(*paragraph), "{path:?}: {paragraph:?}");
            assert!(!current.contains(*paragraph), "{path:?}: {paragraph:?}");
        }
    }
}

#[test]
fn writes_the_current_revision_once_in_document_order() {
    let lines = |file: &str| -> Vec<String> {
        let path = format!("../test_files/documents/onenote/{file}");
        let (text, _) = crate::Extractor::new()
            .extract_file_to_string(&path)
            .unwrap();
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        lines("section.one"),
        [
            "So good",
            "Wednesday, December 11, 2019",
            "5:37 PM",
            "This is one note 2016"
        ]
    );
    let embedded = lines("embedded-word.one");
    assert_eq!(
        embedded[..3],
        [
            "Embedded doc sheet",
            "Wednesday, December 11, 2019",
            "5:35 PM"
        ]
    );
    let document = "Dude this is a super cool embedded doc. Where did you get it?";
    assert_eq!(
        embedded.iter().filter(|line| *line == document).count(),
        1,
        "{embedded:?}"
    );
    assert_eq!(
        embedded.last().map(String::as_str),
        Some("Dude this is a super cool embedded doc.docx")
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

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_damaged_sections_as_tikas_path_does() {
    let (parsed, _) = documents(&OneNoteParser, &[&conformance_corpus()]);
    let path = parsed
        .iter()
        .find(|path| name(path) == "testOneNote4.one")
        .expect("Tika's OneNote section");
    let input = std::fs::read(path).unwrap();
    let scratch = crate::formats::parity::Scratch::new("onenote-damaged-section");
    let mut damaged = Vec::new();
    for share in [3, 5, 9] {
        let cut = input.len() * share / 10;
        damaged.push(scratch.write(&format!("cut-{share}.one"), &input[..cut]));
    }
    let flipped_at = |at: usize| {
        let mut flipped = input.clone();
        flipped[at] ^= 0xFF;
        scratch.write(&format!("flipped-{at}.one"), &flipped)
    };
    for at in [0x400, input.len() / 3, input.len() / 2, input.len() * 2 / 3] {
        damaged.push(flipped_at(at));
    }
    let paths: Vec<&Path> = damaged.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_departing(&paths, OneNoteParser.tika_name(), walked(path).unwrap());
    let current_text = input
        .windows(8)
        .rposition(|window| window == b"poptarts")
        .expect("a paragraph of the current revision");
    let (text, _) = crate::Extractor::new()
        .extract_file_to_string(flipped_at(current_text).to_str().unwrap())
        .unwrap();
    assert!(
        text.contains("way too much information about \u{FFFD}optarts to handle."),
        "{text}"
    );
    assert!(!text.contains("poptarts"), "{text}");
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
