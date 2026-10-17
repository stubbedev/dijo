use std::path::{Path, PathBuf};

use super::*;
use crate::formats::ooxml::testing::{package_with, relationship};
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};

const SEQUENCE_TYPE: &str = "application/vnd.ms-package.xps-fixeddocumentsequence+xml";
const DOCUMENT_TYPE: &str = "application/vnd.ms-package.xps-fixeddocument+xml";
const PAGE_TYPE: &str = "application/vnd.ms-package.xps-fixedpage+xml";
const XPS_NAMESPACE: &str = "http://schemas.microsoft.com/xps/2005/06";

fn paths(files: &[PathBuf]) -> Vec<&Path> {
    files.iter().map(PathBuf::as_path).collect()
}

/// An XPS package of one document of `pages`, each the content of a fixed page, which lists
/// `missing` pages more that the package lacks.
fn xps(pages: &[&str], missing: usize) -> Vec<u8> {
    let sequence = format!(
        "<FixedDocumentSequence xmlns=\"{XPS_NAMESPACE}\">\
         <DocumentReference Source=\"/Documents/1/FixedDocument.fdoc\"/></FixedDocumentSequence>"
    );
    let references: String = (1..=pages.len() + missing)
        .map(|page| format!("<PageContent Source=\"Pages/{page}.fpage\"/>"))
        .collect();
    let document = format!("<FixedDocument xmlns=\"{XPS_NAMESPACE}\">{references}</FixedDocument>");
    let pages: Vec<(String, String)> = pages
        .iter()
        .enumerate()
        .map(|(index, content)| {
            (
                format!("Documents/1/Pages/{}.fpage", index + 1),
                format!(
                    "<FixedPage xmlns=\"{XPS_NAMESPACE}\" Width=\"816\" Height=\"1056\">{content}</FixedPage>"
                ),
            )
        })
        .collect();
    let mut parts: Vec<(&str, &str, &[u8])> = vec![
        ("FixedDocSeq.fdseq", SEQUENCE_TYPE, sequence.as_bytes()),
        (
            "Documents/1/FixedDocument.fdoc",
            DOCUMENT_TYPE,
            document.as_bytes(),
        ),
    ];
    for (name, page) in &pages {
        parts.push((name, PAGE_TYPE, page.as_bytes()));
    }
    package_with(
        &relationship("rId1", XPS_DOCUMENT, "/FixedDocSeq.fdseq"),
        &parts,
    )
}

fn glyphs(text: &str, x: &str, y: &str, indices: &str) -> String {
    format!(
        "<Glyphs OriginX=\"{x}\" OriginY=\"{y}\" FontRenderingEmSize=\"12\" \
         UnicodeString=\"{text}\" Indices=\"{indices}\"/>"
    )
}

#[test]
fn extracts_xps_documents_as_tikas_path_does() {
    let scratch = Scratch::new("xps");
    let first = [
        glyphs("Hello", "10", "20", ""),
        glyphs("world", "60", "21", ""),
        glyphs("Next line", "10", "60", ";,50;,50;,250;,50"),
        "<Canvas Clip=\"M 0,0 L 10,10\">".to_owned(),
        glyphs("In a canvas", "10", "90", ""),
        glyphs("\u{5e9}\u{5dc}\u{5d5}\u{5dd}", "300", "90", ""),
        "</Canvas>".to_owned(),
        "<Path FixedPage.NavigateUri=\"https://example.com/\" Data=\"M 0,0\"/>".to_owned(),
        glyphs("Above", "10", "5", ""),
    ]
    .concat();
    let second = [
        glyphs("Right to left", "200", "20", "").replace("/>", " BidiLevel=\"1\"/>"),
        glyphs("Second page", "10", "40", ""),
        glyphs("No origin", "", "", ""),
    ]
    .concat();
    let read = [
        scratch.write("pages.xps", &xps(&[&first, &second], 0)),
        scratch.write("empty.xps", &xps(&[], 0)),
    ];
    let failing = [
        scratch.write(
            "bad-size.xps",
            &xps(&[&glyphs("x", "1", "1", "").replace("12", "big")], 0),
        ),
        scratch.write("missing-page.xps", &xps(&[&first], 1)),
    ];
    let (parsed, deferred) = documents(&XpsParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
    assert_same_as_tika(&paths(&read), XpsParser.tika_name());
    assert_fails_as_tika(&paths(&failing));
}

fn xml(input: &[u8]) -> String {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(input)
        .unwrap()
        .0
}

/// What Tika slips on, read as it should be (#127): a visual brush ends with its element, so
/// the runs after it share the canvas of those before; and each document sequence is read, not
/// the first again.
#[test]
fn reads_what_tika_slips_on() {
    let page = [
        glyphs("Before", "10", "20", ""),
        "<Path Data=\"M 0,0\"><Path.Fill><VisualBrush Transform=\"1,0,0,1,0,0\">\
         </VisualBrush></Path.Fill></Path>"
            .to_owned(),
        glyphs("After", "10", "40", ""),
    ]
    .concat();
    let text = xml(&xps(&[&page], 0));
    assert!(
        text.contains("<div class=\"canvas\"><p>Before</p>\n<p>After</p>\n</div>"),
        "{text}"
    );
    let sequence = |document: &str| {
        format!(
            "<FixedDocumentSequence xmlns=\"{XPS_NAMESPACE}\">\
             <DocumentReference Source=\"/{document}/FixedDocument.fdoc\"/></FixedDocumentSequence>"
        )
    };
    let document = format!(
        "<FixedDocument xmlns=\"{XPS_NAMESPACE}\"><PageContent Source=\"1.fpage\"/></FixedDocument>"
    );
    let page = |text: &str| {
        format!(
            "<FixedPage xmlns=\"{XPS_NAMESPACE}\" Width=\"816\" Height=\"1056\">{}</FixedPage>",
            glyphs(text, "10", "20", "")
        )
    };
    let (first, second) = (sequence("One"), sequence("Two"));
    let (first_page, second_page) = (page("First"), page("Second"));
    let input = package_with(
        &[
            relationship("rId1", XPS_DOCUMENT, "/One.fdseq"),
            relationship("rId2", XPS_DOCUMENT, "/Two.fdseq"),
        ]
        .concat(),
        &[
            ("One.fdseq", SEQUENCE_TYPE, first.as_bytes()),
            ("Two.fdseq", SEQUENCE_TYPE, second.as_bytes()),
            ("One/FixedDocument.fdoc", DOCUMENT_TYPE, document.as_bytes()),
            ("Two/FixedDocument.fdoc", DOCUMENT_TYPE, document.as_bytes()),
            ("One/1.fpage", PAGE_TYPE, first_page.as_bytes()),
            ("Two/1.fpage", PAGE_TYPE, second_page.as_bytes()),
        ],
    );
    let text = xml(&input);
    let (first, second) = (text.find("<p>First</p>"), text.find("<p>Second</p>"));
    assert!(first.is_some() && first < second, "{text}");
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_xps_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&XpsParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 6, "{parsed:?}");
    assert_same_as_tika(&paths(&parsed), XpsParser.tika_name());
}
