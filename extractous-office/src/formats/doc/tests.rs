#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::config::OfficeParserConfig;
#[cfg(feature = "tika")]
use crate::formats::parity::{self, assert_same_as_tika_with, assert_same_as_tika_with_passwords};

#[cfg(feature = "tika")]
const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";
#[cfg(feature = "tika")]
const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];

/// The documents this port leaves to Tika, by file name, with a part of the reason.
#[cfg(feature = "tika")]
const DEFERRED: [(&str, &str); 0] = [];

/// The encrypted documents, by file name, with the password that opens them.
#[cfg(feature = "tika")]
const ENCRYPTED: [(&str, &str); 2] = [
    ("testWORD_protected_passtika.doc", "tika"),
    ("tika-password.doc", "tika"),
];

/// Tika's test documents this port reads otherwise than Tika does where Tika slips (#111, #139),
/// each with what the native XHTML holds where Tika's does not, or the comment persons in order.
#[cfg(feature = "tika")]
const DEPARTURES: [(&str, Departure); 4] = [
    // A hyperlink whose text holds a page reference field ends with its own end mark.
    (
        "testControlCharacters.doc",
        Departure::Holds(
            "<a href=\"_Toc220727528\"><u>1.</u>\t<u>Introduzione</u><b>\t</b></a></p>",
        ),
    ),
    // A hyperlink field whose code holds another field is paired.
    (
        "testWORD_2006ml.doc",
        Departure::Holds("<p class=\"tOC_1\">3<a href=\"_Toc467647605\"><u>Heading1</u>\t</a></p>"),
    ),
    // The revision authors in the order the document lists them.
    (
        "testWORD_features.doc",
        Departure::Persons(&["Unknown", "Unknown Author", "Kyle Reese"]),
    ),
    // The text of the picture of the embedded document, where POI fails on its font (#142).
    (
        "testLargeOLEDoc.doc",
        Departure::Holds(
            "<p><img src=\"embedded:image-1.emf\" alt=\"image-1.emf\" /><p>This is an embedded \
             file.This is an embedded file.",
        ),
    ),
];

/// What a departure's native extraction holds.
#[cfg(feature = "tika")]
enum Departure {
    Holds(&'static str),
    Persons(&'static [&'static str]),
}

/// Of `documents`, those Tika's path extracts alike, after checking that each departure holds
/// what it should.
#[cfg(feature = "tika")]
fn without_departures(documents: &[PathBuf]) -> Vec<PathBuf> {
    let mut alike = Vec::new();
    for document in documents {
        let name = file_name(document);
        let Some((_, departure)) = DEPARTURES.iter().find(|(departure, _)| *departure == name)
        else {
            alike.push(document.clone());
            continue;
        };
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(document.to_str().unwrap())
            .unwrap();
        match departure {
            Departure::Holds(part) => assert!(xml.contains(part), "{name}: no {part:?} in\n{xml}"),
            Departure::Persons(persons) => assert_eq!(
                metadata["msoffice:comment-person-display-name"], *persons,
                "{name}"
            ),
        }
    }
    alike
}

#[cfg(feature = "tika")]
/// The paragraphs of the text boxes anchored in the main text of the Word 97 document at
/// `path`, which the native parser writes before the paragraph of their anchor where Tika
/// writes them after the main text (#111).
fn anchored_textboxes(path: &Path) -> Vec<String> {
    let input = std::fs::read(path).unwrap();
    let configs = ParserConfigs::default();
    let file = office::open(&input, &configs, MEDIA_TYPE, "Word document").unwrap();
    let Ok(Word::Modern(document)) = Document::read(&file, file.root(), &configs) else {
        return Vec::new();
    };
    let anchored = document.anchored_textboxes().unwrap();
    let (start, end) = document.story(fib::Story::Textbox);
    let story = document.range(start, end).unwrap();
    let mut paragraphs = Vec::new();
    for index in 0..document.paragraph_count(story).unwrap() {
        let paragraph = document.paragraph(story, index).unwrap();
        let span = paragraph.span;
        if anchored
            .iter()
            .any(|(_, textbox)| span.start >= textbox.start && span.start < textbox.end)
        {
            let text = extractor::strip_fields(document.text_of(span).unwrap()).unwrap();
            let text = String::from_utf16_lossy(&extractor::clean(&text));
            let text = text.trim();
            if !text.is_empty() {
                paragraphs.push(text.to_owned());
            }
        }
    }
    paragraphs
}

#[cfg(feature = "tika")]
/// Of `documents`, those whose text boxes are anchored in the main text, after checking that
/// the native text holds each of their paragraphs, compared with Tika's path without the text
/// boxes: the setting that leaves them out leaves out what the native parser moves, and nothing
/// else (#111).
fn with_anchored_textboxes(documents: &[PathBuf]) -> Vec<PathBuf> {
    let mut anchored = Vec::new();
    for document in documents {
        let paragraphs = anchored_textboxes(document);
        if paragraphs.is_empty() {
            continue;
        }
        let (text, _) = crate::Extractor::new()
            .extract_file_to_string(document.to_str().unwrap())
            .unwrap();
        for paragraph in &paragraphs {
            assert!(
                text.contains(paragraph.as_str()),
                "{paragraph:?} in\n{text}"
            );
        }
        anchored.push(document.clone());
    }
    anchored
}

#[cfg(feature = "tika")]
/// The settings documents with anchored text boxes are compared under.
fn without_textboxes() -> ParserConfigs {
    ParserConfigs {
        office: OfficeParserConfig::new().set_include_shape_based_content(false),
        ..ParserConfigs::default()
    }
}

#[cfg(feature = "tika")]
/// Compares `documents` with Tika's path, those with anchored text boxes without the text
/// boxes, and checks that `textboxes` of them have such text boxes.
fn assert_same_as_tika(documents: &[PathBuf], textboxes: usize) {
    let anchored = with_anchored_textboxes(documents);
    assert_eq!(anchored.len(), textboxes, "{anchored:?}");
    let others: Vec<PathBuf> = documents
        .iter()
        .filter(|document| !anchored.contains(document))
        .cloned()
        .collect();
    assert_same_as_tika_with_passwords(&others, TIKA_PARSER, &ENCRYPTED);
    let anchored: Vec<&Path> = anchored.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_with(&anchored, TIKA_PARSER, &without_textboxes());
}

#[cfg(feature = "tika")]
/// A text box is written before the paragraph its anchor is in (#111).
#[test]
fn writes_a_text_box_where_it_is_anchored() {
    let path = Path::new("../quality/corpus/notes.doc");
    assert_eq!(
        anchored_textboxes(path),
        ["Sidebar: the tower was painted white in 1921."]
    );
    let (text, _) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    let at = |part: &str| {
        text.find(part)
            .unwrap_or_else(|| panic!("{part:?} in {text}"))
    };
    assert!(at("The Keeper's Notes") < at("Sidebar:"), "{text}");
    assert!(at("Sidebar:") < at("The keeper wrote"), "{text}");
}

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
        match (parity::deferral(&DocParser, document), expected) {
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
    let (mut documents, deferred) = parity::documents(&DocParser, roots);
    documents.extend(deferred);
    documents.sort();
    documents
}

#[test]
fn decrypts_an_encrypted_document_with_its_password_only() {
    let input = std::fs::read("../test_files/documents/encrypted/tika-password.doc").unwrap();
    let configs = |password: &str| {
        ParserConfigs::default().with_password(Some(&crate::config::Password::new(password)))
    };
    let (_, extracted) = read(&input, &configs("tika")).unwrap();
    let text: String = extracted
        .out
        .iter()
        .filter_map(|out| match out {
            extractor::Out::Text(text) => Some(String::from_utf16_lossy(text)),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("This is an encrypted Word 2007 File."),
        "{text}"
    );
    for configs in [ParserConfigs::default(), configs("wrong")] {
        assert!(matches!(
            read(&input, &configs),
            Err(Decline::Fails(crate::Error::Encrypted(_)))
        ));
    }
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "runs Tika on every Word document of the repository"]
fn extracts_what_tikas_path_extracts_from_the_repositorys_documents() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let ported = ported(&corpus(&roots));
    assert_eq!(ported.len(), 5);
    assert_same_as_tika(&ported, 1);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let ported = ported(&corpus(&[&parity::conformance_corpus()]));
    assert_eq!(ported.len(), 42);
    let alike = without_departures(&ported);
    assert_eq!(ported.len() - alike.len(), DEPARTURES.len());
    assert_same_as_tika(&alike, 2);
}
