use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

fn paths(files: &[PathBuf]) -> Vec<&Path> {
    files.iter().map(PathBuf::as_path).collect()
}

fn part(name: &str, content_type: &str, content: &str) -> String {
    format!("<pkg:part pkg:name=\"{name}\" pkg:contentType=\"{content_type}\">{content}</pkg:part>")
}

fn xml_part(name: &str, content_type: &str, xml: &str) -> String {
    part(
        name,
        content_type,
        &format!("<pkg:xmlData>{xml}</pkg:xmlData>"),
    )
}

/// A Word 2006 XML document of `parts`.
fn flat(parts: &[String]) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <?mso-application progid=\"Word.Document\"?>\n\
         <pkg:package xmlns:pkg=\"{PACKAGE_NAMESPACE}\">{}</pkg:package>",
        parts.concat()
    )
    .into_bytes()
}

fn word(root: &str, body: &str) -> String {
    format!("<w:{root} {W}>{body}</w:{root}>")
}

fn paragraph(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

/// A document with each kind of part: text with a note, a comment, a table, a styled and
/// numbered paragraph, a header, notes and comments, styles, an attachment, and properties.
fn sample() -> Vec<u8> {
    let relationships = xml_part(
        "/_rels/.rels",
        RELATIONSHIPS_CONTENT_TYPE,
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/\
         2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>",
    );
    let body = format!(
        "<w:body>{}<w:p><w:r><w:t>Noted</w:t></w:r><w:r><w:footnoteReference w:id=\"1\"/></w:r>\
         <w:r><w:t>said</w:t></w:r><w:r><w:commentReference w:id=\"0\"/></w:r></w:p>\
         <w:tbl><w:tr><w:tc>{}</w:tc><w:tc>{}</w:tc></w:tr></w:tbl>\
         <w:p><w:pPr><w:pStyle w:val=\"Heading1\"/><w:numPr><w:ilvl w:val=\"0\"/>\
         <w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:rPr><w:b/></w:rPr>\
         <w:t>Bold heading</w:t></w:r></w:p></w:body>",
        paragraph("Første afsnit på dansk"),
        paragraph("a1"),
        paragraph("b1"),
    );
    let document = xml_part(
        "/word/document.xml",
        MAIN_DOCUMENT_TYPES[0],
        &word("document", &body),
    );
    let header = xml_part(
        "/word/header1.xml",
        TEXT_PART_TYPES[2],
        &word("hdr", &paragraph("The header")),
    );
    let footnotes = xml_part(
        "/word/footnotes.xml",
        TEXT_PART_TYPES[0],
        &word(
            "footnotes",
            &format!(
                "<w:footnote w:id=\"1\">{}</w:footnote>",
                paragraph("A note")
            ),
        ),
    );
    let styles = xml_part(
        "/word/styles.xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
        &word("styles", "<w:style w:styleId=\"Heading1\"/>"),
    );
    let attachment = part(
        "/word/embeddings/note.txt",
        "text/plain",
        "<pkg:binaryData>SGVsbG8sIGVt\nYmVkZGVkIHdv\ncmxkIQ==</pkg:binaryData>",
    );
    let comments = xml_part(
        "/word/comments.xml",
        TEXT_PART_TYPES[4],
        &word(
            "comments",
            &format!(
                "<w:comment w:id=\"0\">{}</w:comment>",
                paragraph("A comment")
            ),
        ),
    );
    let core = xml_part(
        "/docProps/core.xml",
        CORE_PROPERTIES_CONTENT_TYPE,
        "<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/\
         metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:dcterms=\"http://purl.org/dc/terms/\"><dc:title>A title</dc:title>\
         <dc:subject>A subject</dc:subject><dc:creator>Anna</dc:creator>\
         <cp:lastModifiedBy>Bo</cp:lastModifiedBy><cp:revision>3</cp:revision>\
         <dcterms:created>2016-11-16T13:02:00Z</dcterms:created>\
         <cp:keywords>ignored</cp:keywords></cp:coreProperties>",
    );
    let app = xml_part(
        "/docProps/app.xml",
        EXTENDED_PROPERTIES_CONTENT_TYPE,
        "<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/\
         extended-properties\"><Template>Normal.dotm</Template><Pages>2</Pages>\
         <Words>12</Words><Application>Microsoft Office Word</Application>\
         <Company>Stubbe</Company></Properties>",
    );
    flat(&[
        relationships,
        document,
        header,
        footnotes,
        styles,
        attachment,
        comments,
        core,
        app,
    ])
}

#[test]
fn extracts_flat_word_documents_as_tikas_path_does() {
    let scratch = Scratch::new("word2006ml");
    let read = [
        scratch.write("sample.xml", &sample()),
        scratch.write("empty.xml", &flat(&[])),
    ];
    let mut broken = sample();
    broken.truncate(broken.len() / 2);
    let failing = [scratch.write("broken.xml", &broken)];
    let (parsed, deferred) = documents(&Word2006MlParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 3, "{parsed:?}");
    assert_same_as_tika(&paths(&read), Word2006MlParser.tika_name());
    assert_fails_as_tika(&paths(&failing));
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_flat_word_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&Word2006MlParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    assert_same_as_tika(&paths(&parsed), Word2006MlParser.tika_name());
}
