use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use super::*;
use crate::formats::embedded::ParseContext;
use crate::formats::ooxml::extractor::OLE_OBJECT_TYPE;
use crate::formats::ooxml::testing::relationship;
use crate::formats::testing;
use crate::formats::{Chain, Output, StringOut};

const TRUNCATED: &str = "../test_files/documents/truncated.docx";

fn office(config: OfficeParserConfig) -> ParserConfigs {
    ParserConfigs {
        office: config,
        ..ParserConfigs::default()
    }
}

const MAIN: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
                 xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";

/// A Word package of `parts`, each a name, a content type and the content.
fn package(parts: &[(&str, &str, &[u8])]) -> Vec<u8> {
    ooxml::testing::package("word/document.xml", parts)
}

/// A package with a main document holding `body` and relationships `relationships`, besides
/// the further `parts`.
fn document(body: &str, relationships: &str, parts: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let document = format!("<w:document {W}><w:body>{body}</w:body></w:document>");
    let rels = ooxml::testing::relationships(relationships);
    let mut all: Vec<(&str, &str, &[u8])> = vec![
        ("word/document.xml", MAIN, document.as_bytes()),
        (
            "word/_rels/document.xml.rels",
            "application/vnd.openxmlformats-package.relationships+xml",
            rels.as_bytes(),
        ),
    ];
    all.extend_from_slice(parts);
    package(&all)
}

fn defers(input: &[u8]) -> Option<String> {
    DocxParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
}

fn text_of(input: &[u8], configs: &ParserConfigs) -> String {
    testing::install_routes();
    let mut out = StringOut::default();
    // Tika's string chain, which leaves the blocks the parser writes where it writes them (#63).
    let chain = Chain::TikaString {
        output: Output::Xml,
        max_length: 100_000,
    };
    let prepared = DocxParser
        .prepare(input, &TikaMetadata::new(), configs)
        .unwrap();
    let (_, _, result) = chain.write_to(&mut out, |handler| {
        Xhtml::write(
            handler,
            TikaMetadata::new(),
            &ParseContext::new(configs),
            prepared,
        )
    });
    result.unwrap();
    out.0
}

#[test]
fn reads_a_plain_document() {
    let input = document("<w:p><w:r><w:t>Hej verden</w:t></w:r></w:p>", "", &[]);
    assert_eq!(defers(&input), None);
    assert!(text_of(&input, &ParserConfigs::default()).contains("<p>Hej verden</p>"));
}

#[test]
fn defers_ole_objects_tika_opens_only() {
    let ole = |size: usize| {
        let content = vec![0u8; size];
        document(
            "<w:p/>",
            &relationship("rId9", kind::OLE_OBJECT, "embeddings/oleObject1.bin"),
            &[("word/embeddings/oleObject1.bin", OLE_OBJECT_TYPE, &content)],
        )
    };
    assert_eq!(defers(&ole(1535)), None);
    assert!(defers(&ole(1536)).is_some());
}

#[test]
fn defers_macros_only_when_they_are_extracted() {
    let input = document(
        "<w:p/>",
        &relationship("rId9", kind::VBA_PROJECT, "vbaProject.bin"),
        &[(
            "word/vbaProject.bin",
            "application/vnd.ms-office.vbaProject",
            b"x",
        )],
    );
    assert_eq!(defers(&input), None);
    let macros = office(OfficeParserConfig::new().set_extract_macros(true));
    assert!(DocxParser
        .defers(&input, &TikaMetadata::new(), &macros)
        .is_some());
}

/// A document whose main part's XML fails after a paragraph, with a header of `header`.
fn failing_main_part(header: &[u8]) -> Vec<u8> {
    document(
        "<w:p><w:r><w:t>kept</w:t></w:r></w:p><w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>lost</w:p>",
        &relationship("rId9", kind::HEADER, "header1.xml"),
        &[(
            "word/header1.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
            header,
        )],
    )
}

#[test]
fn records_parts_whose_xml_fails_and_goes_on() {
    let input = failing_main_part(b"<w:hdr><w:p><w:r><w:t>head</w:t></w:p>");
    assert_eq!(defers(&input), None);
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(&input)
        .unwrap();
    assert!(xml.contains("<p>kept</p>"), "{xml}");
    assert!(xml.contains("</body>"), "{xml}");
    let warnings = &metadata["tk:exception:warn"];
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings
        .iter()
        .all(|warning| warning.starts_with("org.xml.sax.SAXParseException: /word/")));
    assert!(
        !metadata.contains_key(name::HAS_HIDDEN_TEXT),
        "{metadata:?}"
    );
}

#[test]
fn defers_parts_declaring_a_document_type() {
    let header = document(
        "<w:p/>",
        &relationship("rId9", kind::HEADER, "header1.xml"),
        &[(
            "word/header1.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
            b"<!DOCTYPE w:hdr><w:hdr><w:p/></w:hdr>",
        )],
    );
    assert!(defers(&header).is_some_and(|reason| reason.contains("header1.xml declares")));
    let quietly_read = document(
        "<w:p/>",
        &relationship("rId9", kind::SETTINGS, "settings.xml"),
        &[(
            "word/settings.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
            b"<!DOCTYPE n><n/>",
        )],
    );
    assert!(defers(&quietly_read).is_some_and(|reason| reason.contains("document type")));
}

#[test]
fn defers_packages_poi_refuses_or_reads_otherwise() {
    assert!(defers(b"PK\x03\x04 not really").is_some());
    let no_types = {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap().into_inner()
    };
    assert!(defers(&no_types).is_some_and(|reason| reason.contains("[Content_Types].xml")));
    let odd_name = document("<w:p/>", "", &[("word/media/a b.png", "image/png", b"x")]);
    assert!(defers(&odd_name).is_some_and(|reason| reason.contains("a b.png")));
    let absolute = document(
        "<w:p/>",
        &relationship("rId9", kind::STYLES, "http://example.com/styles.xml"),
        &[],
    );
    assert!(defers(&absolute).is_some_and(|reason| reason.contains("does not resolve")));
    let not_word = package(&[(
        "word/document.xml",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
        b"<p/>",
    )]);
    assert!(defers(&not_word).is_some_and(|reason| reason.contains("main part")));
}

/// `input` without its central directory, as an archive cut short there is.
fn without_central_directory(input: &[u8]) -> Vec<u8> {
    let start = input
        .windows(4)
        .position(|window| window == b"PK\x01\x02")
        .unwrap();
    input[..start].to_vec()
}

#[test]
fn reads_packages_without_a_central_directory_by_their_local_headers() {
    let input = document("<w:p><w:r><w:t>Hej verden</w:t></w:r></w:p>", "", &[]);
    let cut = without_central_directory(&input);
    assert_eq!(defers(&cut), None);
    let defaults = ParserConfigs::default();
    assert_eq!(text_of(&cut, &defaults), text_of(&input, &defaults));
}

#[test]
fn fails_packages_cut_short_in_an_entry_as_poi_does() {
    let input = std::fs::read(TRUNCATED).unwrap();
    let configs = ParserConfigs::default();
    let declined = DocxParser
        .prepare(&input, &TikaMetadata::new(), &configs)
        .err();
    assert!(
        matches!(
            &declined,
            Some(Decline::Fails(crate::Error::ParseError(reason)))
                if reason.starts_with("Error opening OOXML file: Truncated ZIP file")
                    && reason.contains("word/styles.xml")
        ),
        "{declined:?}"
    );
}

#[test]
fn defers_archives_listing_a_name_twice() {
    let mut input = document(
        "<w:p/>",
        "",
        &[
            ("word/a.xml", "application/xml", b"<a/>"),
            ("word/b.xml", "application/xml", b"<b/>"),
        ],
    );
    let renamed: Vec<usize> = input
        .windows(10)
        .enumerate()
        .filter(|(index, window)| *window == b"word/b.xml" && input[index - 1] != b'/')
        .map(|(index, _)| index)
        .collect();
    assert_eq!(renamed.len(), 2);
    for index in renamed {
        input[index + 5] = b'a';
    }
    assert!(defers(&input).is_some_and(|reason| reason.contains("same name")));
}

#[test]
fn defers_core_properties_poi_refuses() {
    let core = |content: &str| {
        let content = content.to_owned();
        package(&[
            (
                "word/document.xml",
                MAIN,
                format!("<w:document {W}><w:body/></w:document>").as_bytes(),
            ),
            (
                "docProps/core.xml",
                "application/vnd.openxmlformats-package.core-properties+xml",
                content.as_bytes(),
            ),
        ])
    };
    let properties = |inner: &str| {
        format!(
            "<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
             xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">{inner}</cp:coreProperties>"
        )
    };
    assert_eq!(
        defers(&core(&properties(
            "<dcterms:created xsi:type=\"dcterms:W3CDTF\">2020-01-02T03:04:05Z</dcterms:created>"
        ))),
        None
    );
    assert!(
        defers(&core(&properties("<dcterms:title>x</dcterms:title>")))
            .is_some_and(|reason| reason.contains("M4.3"))
    );
    assert!(defers(&core(&properties(
        "<dcterms:created xsi:type=\"dcterms:W3CDTF\">yesterday</dcterms:created>"
    )))
    .is_some_and(|reason| reason.contains("yesterday")));
}

#[test]
fn writes_notes_where_they_are_referenced_and_comments_after_their_paragraph() {
    let footnotes = format!(
        "<w:footnotes {W}><w:footnote w:id=\"1\"><w:p><w:r><w:t>Note</w:t></w:r></w:p></w:footnote></w:footnotes>"
    );
    let comments = format!(
        "<w:comments {W}><w:comment w:id=\"0\"><w:p><w:r><w:t>Said</w:t></w:r></w:p></w:comment>\
         <w:comment w:id=\"7\"><w:p><w:r><w:t>Loose</w:t></w:r></w:p></w:comment></w:comments>"
    );
    let input = document(
        "<w:p><w:r><w:t>A</w:t></w:r><w:r><w:footnoteReference w:id=\"1\"/></w:r>\
         <w:r><w:commentReference w:id=\"0\"/></w:r><w:r><w:t>B</w:t></w:r></w:p>\
         <w:p><w:r><w:footnoteReference w:id=\"5\"/></w:r></w:p>",
        &format!(
            "{}{}",
            relationship("rId1", kind::FOOTNOTES, "footnotes.xml"),
            relationship("rId2", kind::COMMENTS, "comments.xml")
        ),
        &[
            (
                "word/footnotes.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
                footnotes.as_bytes(),
            ),
            (
                "word/comments.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
                comments.as_bytes(),
            ),
        ],
    );
    assert_eq!(defers(&input), None);
    let text = text_of(&input, &ParserConfigs::default());
    assert!(
        text.contains(
            "<p>A<div class=\"footnote\"><p>Note</p>\n</div>\nB</p>\n<div class=\"comment\"><p>Said</p>\n</div>"
        ),
        "{text}"
    );
    assert!(text.contains("<p>[5]</p>"), "{text}");
    assert!(
        text.contains("<div class=\"comment\"><p>Loose</p>"),
        "{text}"
    );
}

#[test]
fn includes_deleted_and_moved_text_on_request() {
    let input = document(
        "<w:p><w:r><w:t>kept </w:t></w:r><w:del w:author=\"A\"><w:r><w:delText>gone</w:delText></w:r></w:del>\
         <w:moveFrom w:author=\"B\"><w:r><w:t>moved</w:t></w:r></w:moveFrom></w:p>",
        "",
        &[],
    );
    let default = text_of(&input, &ParserConfigs::default());
    assert!(default.contains("<p>kept </p>"), "{default}");
    let deleted = office(OfficeParserConfig::new().set_include_deleted_content(true));
    assert!(text_of(&input, &deleted).contains("<p>kept gone</p>"));
    let moved = office(OfficeParserConfig::new().set_include_move_from_content(true));
    assert!(text_of(&input, &moved).contains("<p>kept moved</p>"));
}
