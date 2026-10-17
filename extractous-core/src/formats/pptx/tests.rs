use std::path::{Path, PathBuf};

use super::*;
use crate::formats::ooxml::testing::{self, relationship};
use crate::formats::parity::{
    assert_same_as_tika, assert_same_as_tika_with, conformance_corpus, documents, Scratch,
};
use crate::mime::compound_document;

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.ooxml.OOXMLParser";

const DOCUMENTS: [&str; 3] = [
    "../test_files/documents/simple.pptx",
    "../test_files/documents/ooxml/issue4.pptx",
    "../test_files/documents/ooxml/smartart.pptx",
];

/// The documents this port reads otherwise than Tika does where Tika slips (#102), each with
/// what the native XHTML holds in order where Tika's does not.
const DEPARTURES: [(&str, &[&str]); 7] = [
    // A layout's picture is named by the layout's own relationships.
    (
        "science-exploration-1p.pptx",
        &["<img src=\"embedded:image2.png\" alt=\"\" />"],
    ),
    (
        "test_embedded_zip.pptx",
        &["<img src=\"embedded:image1.png\" alt=\"IBM100_PPT_BannerWhite\" />"],
    ),
    // The handout master's placeholders are skipped.
    (
        "testPPT.potm",
        &["<p>Investment</p>\n<p />\n</div>\n<div class=\"embedded\""],
    ),
    (
        "testPPT_autodate.pptx",
        &["<p>2011-12-19 10:20:04 AM</p>\n<div class=\"embedded\""],
    ),
    // A link to an anchor is a link.
    ("testPPT_various.pptx", &["<a href=\"#_ftn1\">[1]</a>"]),
    // Slides and embedded documents in the order of their relationships' numbers.
    (
        "testPPT_various2.pptx",
        &[
            "<div class=\"slide-content\"><p>This is some content</p>",
            "<p>This slide is hidden</p>",
        ],
    ),
    (
        "testWPSAttachment.pptx",
        &["<h1>PDF.pdf</h1>", "<h1>Workbook1.xls</h1>"],
    ),
];

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Of `documents`, those Tika's path extracts alike, after checking that each departure holds
/// what it should.
fn without_departures<'p>(documents: &[&'p Path]) -> Vec<&'p Path> {
    let mut alike = Vec::new();
    for &document in documents {
        let name = file_name(document);
        let Some((_, expected)) = DEPARTURES.iter().find(|(departure, _)| *departure == name)
        else {
            alike.push(document);
            continue;
        };
        let xml = xml(&std::fs::read(document).unwrap());
        let mut from = 0;
        for part in *expected {
            let at = xml[from..].find(part).map(|at| from + at);
            assert!(at.is_some(), "{name}: no {part:?} after {from} in\n{xml}");
            from = at.unwrap() + part.len();
        }
    }
    alike
}

const NAMESPACES: &str = "xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" \
     xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
     xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
const PRESENTATION_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const RELATIONSHIPS_TYPE: &str = "application/vnd.openxmlformats-package.relationships+xml";

/// A presentation being built: its parts, each a name, a content type and the content.
#[derive(Default)]
struct Built {
    parts: Vec<(String, String, Vec<u8>)>,
}

impl Built {
    /// A presentation whose main part's relationships are `relationships`, each an id, a kind
    /// and a target.
    fn new(relationships: &[(&str, &str, &str)]) -> Self {
        let mut built = Self::default();
        built.part(
            "ppt/presentation.xml",
            PRESENTATION_TYPE,
            &format!("<p:presentation {NAMESPACES}/>"),
        );
        built.relationships("ppt/presentation.xml", relationships);
        built
    }

    fn part(&mut self, name: &str, content_type: &str, content: &str) -> &mut Self {
        self.binary(name, content_type, content.as_bytes())
    }

    fn binary(&mut self, name: &str, content_type: &str, content: &[u8]) -> &mut Self {
        self.parts
            .push((name.to_owned(), content_type.to_owned(), content.to_vec()));
        self
    }

    /// The relationships of the part `source`.
    fn relationships(&mut self, source: &str, relationships: &[(&str, &str, &str)]) -> &mut Self {
        let (directory, file) = source.rsplit_once('/').unwrap();
        let listed: String = relationships
            .iter()
            .map(|(id, kind, target)| {
                if target.starts_with("http") {
                    format!(
                        "<Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{target}\" \
                         TargetMode=\"External\"/>"
                    )
                } else {
                    relationship(id, kind, target)
                }
            })
            .collect();
        self.part(
            &format!("{directory}/_rels/{file}.rels"),
            RELATIONSHIPS_TYPE,
            &testing::relationships(&listed),
        )
    }

    fn bytes(&self) -> Vec<u8> {
        let parts: Vec<(&str, &str, &[u8])> = self
            .parts
            .iter()
            .map(|(name, content_type, content)| {
                (name.as_str(), content_type.as_str(), content.as_slice())
            })
            .collect();
        testing::package("ppt/presentation.xml", &parts)
    }
}

fn pml(kind: &str) -> String {
    format!("application/vnd.openxmlformats-officedocument.presentationml.{kind}+xml")
}

/// A shape of text, a placeholder of `placeholder` when there is one.
fn shape(placeholder: Option<&str>, text: &str) -> String {
    let placeholder = placeholder.map_or(String::new(), |kind| format!("<p:ph type=\"{kind}\"/>"));
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Shape\"/><p:cNvSpPr/><p:nvPr>{placeholder}\
         </p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"
    )
}

/// A slide-like part of `root` holding `content` in its shape tree, after `attributes`.
fn sheet(root: &str, attributes: &str, content: &str, after: &str) -> String {
    format!(
        "<p:{root} {NAMESPACES}{attributes}><p:cSld><p:spTree>{content}</p:spTree></p:cSld>\
         {after}</p:{root}>"
    )
}

fn native_record(body: &[u8]) -> Vec<u8> {
    [&i32::try_from(body.len()).unwrap().to_le_bytes()[..], body].concat()
}

/// A parsed `Ole10Native` record of a file labelled `label`.
fn parsed_native(label: &str, data: &[u8]) -> Vec<u8> {
    let mut body = vec![2, 0];
    body.extend_from_slice(label.as_bytes());
    body.extend_from_slice(b"\0C:\\f\0\0\0\0\0");
    body.extend_from_slice(&1i32.to_le_bytes());
    body.push(0);
    body.extend_from_slice(&i32::try_from(data.len()).unwrap().to_le_bytes());
    body.extend_from_slice(data);
    body.extend_from_slice(&[0, 0]);
    native_record(&body)
}

/// A presentation of two slides exercising what the extractor writes: a hidden, animated slide
/// with a title, a link and a table, its layout and master with placeholders, notes and
/// comments; and a slide embedding OLE objects of each kind.
fn rich_presentation() -> Vec<u8> {
    let mut built = Built::new(&[
        ("rId1", kind::SLIDE, "slides/slide1.xml"),
        ("rId2", kind::SLIDE, "slides/slide2.xml"),
        ("rId3", kind::SLIDE_MASTER, "slideMasters/slideMaster1.xml"),
        (
            "rId4",
            kind::HANDOUT_MASTER,
            "handoutMasters/handoutMaster1.xml",
        ),
        ("rId5", kind::COMMENT_AUTHORS, "commentAuthors.xml"),
    ]);
    let table = "<p:graphicFrame><a:graphic><a:graphicData><a:tbl><a:tr><a:tc><a:txBody><a:p>\
                 <a:r><a:t>Cell</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl>\
                 </a:graphicData></a:graphic></p:graphicFrame>";
    let link = "<p:sp><p:txBody><a:p><a:r><a:rPr><a:hlinkClick r:id=\"rId9\"/></a:rPr>\
                <a:t>Link</a:t></a:r></a:p></p:txBody></p:sp>";
    built
        .part(
            "ppt/slides/slide1.xml",
            &pml("slide"),
            &sheet(
                "sld",
                " show=\"0\"",
                &format!("{}{link}{table}", shape(Some("title"), "Title")),
                "<p:timing><p:tnLst><p:par/></p:tnLst></p:timing>",
            ),
        )
        .relationships(
            "ppt/slides/slide1.xml",
            &[
                (
                    "rId1",
                    kind::SLIDE_LAYOUT,
                    "../slideLayouts/slideLayout1.xml",
                ),
                ("rId2", kind::NOTES, "../notesSlides/notesSlide1.xml"),
                ("rId3", kind::COMMENTS, "../comments/comment1.xml"),
                (
                    "rId9",
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
                    "https://example.com/",
                ),
            ],
        )
        .part(
            "ppt/slideLayouts/slideLayout1.xml",
            &pml("slideLayout"),
            &sheet(
                "sldLayout",
                "",
                &format!(
                    "{}{}",
                    shape(Some("body"), "Layout placeholder"),
                    shape(None, "Layout text")
                ),
                "",
            ),
        )
        .part(
            "ppt/slideMasters/slideMaster1.xml",
            &pml("slideMaster"),
            &sheet(
                "sldMaster",
                "",
                &format!(
                    "{}{}",
                    shape(Some("title"), "Master placeholder"),
                    shape(None, "Master text")
                ),
                "",
            ),
        )
        .part(
            "ppt/handoutMasters/handoutMaster1.xml",
            &pml("handoutMaster"),
            &sheet("handoutMaster", "", &shape(None, "Handout text"), ""),
        )
        .part(
            "ppt/notesSlides/notesSlide1.xml",
            &pml("notesSlide"),
            &sheet("notes", "", &shape(Some("body"), "Speaker notes"), ""),
        )
        .part(
            "ppt/commentAuthors.xml",
            &pml("commentAuthors"),
            &format!(
                "<p:cmAuthorLst {NAMESPACES}><p:cmAuthor id=\"0\" name=\"Anna Berg\" \
                 initials=\"AB\"/><p:cmAuthor id=\"1\" name=\" \" initials=\"X\"/>\
                 <p:cmAuthor id=\"2\" name=\"Bo\"/></p:cmAuthorLst>"
            ),
        )
        .part(
            "ppt/comments/comment1.xml",
            &pml("comments"),
            &format!(
                "<p:cmLst {NAMESPACES}><p:cm authorId=\"0\"><p:pos x=\"1\" y=\"1\"/>\
                 <p:text>Nice slide</p:text></p:cm><p:cm authorId=\"1\"><p:text>Blank</p:text>\
                 </p:cm><p:cm authorId=\"2\"><p:text>No initials</p:text></p:cm>\
                 <p:cm authorId=\"7\"><p:text>Unknown</p:text></p:cm></p:cmLst>"
            ),
        );
    let objects: [(&str, Vec<u8>); 6] = [
        (
            "oleObject1.bin",
            compound_document(&["\u{1}Ole", "Package"], b"Text in a package stream"),
        ),
        (
            "oleObject2.bin",
            compound_document(
                &["\u{1}Ole10Native"],
                &parsed_native("note.txt", b"Text in an OLE 1.0 object"),
            ),
        ),
        (
            "oleObject3.bin",
            compound_document(
                &["\u{1}Ole10Native"],
                &native_record(b"\x07\0unparsed text"),
            ),
        ),
        ("oleObject4.bin", compound_document(&["CONTENTS"], b"x")),
        ("oleObject5.bin", vec![0; 100]),
        (
            "oleObject6.bin",
            compound_document(
                &["\u{1}Ole10Native"],
                &native_record(
                    &[
                        &b"\x02\0a\0b\0\0\0\0\0"[..],
                        &0i32.to_le_bytes(),
                        &9i32.to_le_bytes(),
                        b"x",
                    ]
                    .concat(),
                ),
            ),
        ),
    ];
    let mut slide_relationships = Vec::new();
    let ids: Vec<String> = (1..=objects.len()).map(|i| format!("rId{i}")).collect();
    let targets: Vec<String> = objects
        .iter()
        .map(|(name, _)| format!("../embeddings/{name}"))
        .collect();
    for ((id, target), (name, content)) in ids.iter().zip(&targets).zip(&objects) {
        slide_relationships.push((id.as_str(), kind::OLE_OBJECT, target.as_str()));
        built.binary(
            &format!("ppt/embeddings/{name}"),
            extractor::OLE_OBJECT_TYPE,
            content,
        );
    }
    slide_relationships.push(("rId9", kind::OLE_OBJECT, "https://example.com/linked.xlsx"));
    built
        .part(
            "ppt/slides/slide2.xml",
            &pml("slide"),
            &sheet("sld", "", &shape(None, "Objects"), ""),
        )
        .relationships("ppt/slides/slide2.xml", &slide_relationships);
    built.bytes()
}

fn defers(input: &[u8]) -> Option<String> {
    PptxParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
}

fn office(config: OfficeParserConfig) -> ParserConfigs {
    ParserConfigs {
        office: config,
        ..ParserConfigs::default()
    }
}

#[test]
fn extracts_what_tikas_path_extracts() {
    let documents: Vec<&Path> = DOCUMENTS.iter().map(Path::new).collect();
    assert_same_as_tika(&documents, TIKA_PARSER);
    let departures = [Path::new(
        "../test_files/documents/science-exploration-1p.pptx",
    )];
    assert!(without_departures(&departures).is_empty());
}

#[test]
fn extracts_what_tikas_path_extracts_from_built_presentations() {
    let input = rich_presentation();
    assert_eq!(defers(&input), None);
    let scratch = Scratch::new("pptx-built");
    let document = scratch.write("rich.pptx", &input);
    assert_same_as_tika(&[document.as_path()], TIKA_PARSER);
    for configs in [
        office(OfficeParserConfig::new().set_include_slide_notes(false)),
        office(OfficeParserConfig::new().set_include_slide_master_content(false)),
    ] {
        assert_same_as_tika_with(&[document.as_path()], TIKA_PARSER, &configs);
    }
}

#[test]
fn defers_ole_objects_tika_fails_to_read() {
    let mut built = Built::new(&[("rId1", kind::SLIDE, "slides/slide1.xml")]);
    built
        .part(
            "ppt/slides/slide1.xml",
            &pml("slide"),
            &sheet("sld", "", "", ""),
        )
        .relationships(
            "ppt/slides/slide1.xml",
            &[("rId1", kind::OLE_OBJECT, "../embeddings/oleObject1.bin")],
        )
        .binary(
            "ppt/embeddings/oleObject1.bin",
            extractor::OLE_OBJECT_TYPE,
            &[0; 2048],
        );
    assert!(defers(&built.bytes())
        .is_some_and(|reason| reason.contains("Tika fails to read the OLE object")));
}

const HYPERLINK: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

/// A shape of text linked by the relationship `id`.
fn linked(id: &str, text: &str) -> String {
    format!(
        "<p:sp><p:txBody><a:p><a:r><a:rPr><a:hlinkClick r:id=\"{id}\"/></a:rPr><a:t>{text}</a:t>\
         </a:r></a:p></p:txBody></p:sp>"
    )
}

fn xml(input: &[u8]) -> String {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(input)
        .unwrap()
        .0
}

/// What Tika slips on, read as it should be (#102): a link to an anchor, links of a layout and
/// notes by their own relationships, a comment's own text, placeholders of the handout master,
/// and slides past the ninth in order.
#[test]
fn reads_what_tika_slips_on() {
    let mut slides = vec![
        (
            "rId100",
            kind::SLIDE_MASTER,
            "slideMasters/slideMaster1.xml".to_owned(),
        ),
        (
            "rId101",
            kind::HANDOUT_MASTER,
            "handoutMasters/handoutMaster1.xml".to_owned(),
        ),
        (
            "rId102",
            kind::COMMENT_AUTHORS,
            "commentAuthors.xml".to_owned(),
        ),
    ];
    let ids: Vec<String> = (1..=11).map(|number| format!("rId{number}")).collect();
    for (number, id) in (1..=11).zip(&ids) {
        slides.push((id, kind::SLIDE, format!("slides/slide{number}.xml")));
    }
    let listed: Vec<(&str, &str, &str)> = slides
        .iter()
        .map(|(id, kind, target)| (*id, *kind, target.as_str()))
        .collect();
    let mut built = Built::new(&listed);
    for number in 2..=11 {
        let name = format!("ppt/slides/slide{number}.xml");
        built.part(
            &name,
            &pml("slide"),
            &sheet("sld", "", &shape(None, &format!("Slide {number}")), ""),
        );
    }
    built
        .part(
            "ppt/slides/slide1.xml",
            &pml("slide"),
            &sheet("sld", "", &linked("rId1", "To the note"), ""),
        )
        .relationships(
            "ppt/slides/slide1.xml",
            &[
                ("rId1", HYPERLINK, "#_ftn1"),
                (
                    "rId2",
                    kind::SLIDE_LAYOUT,
                    "../slideLayouts/slideLayout1.xml",
                ),
                ("rId3", kind::NOTES, "../notesSlides/notesSlide1.xml"),
                ("rId4", kind::COMMENTS, "../comments/comment1.xml"),
                ("rId5", HYPERLINK, "https://slide.example/"),
            ],
        )
        .part(
            "ppt/slideLayouts/slideLayout1.xml",
            &pml("slideLayout"),
            &sheet("sldLayout", "", &linked("rId5", "Layout link"), ""),
        )
        .relationships(
            "ppt/slideLayouts/slideLayout1.xml",
            &[("rId5", HYPERLINK, "https://layout.example/")],
        )
        .part(
            "ppt/notesSlides/notesSlide1.xml",
            &pml("notesSlide"),
            &sheet("notes", "", &linked("rId5", "Notes link"), ""),
        )
        .relationships(
            "ppt/notesSlides/notesSlide1.xml",
            &[("rId5", HYPERLINK, "https://notes.example/")],
        )
        .part(
            "ppt/slideMasters/slideMaster1.xml",
            &pml("slideMaster"),
            &sheet("sldMaster", "", &shape(None, "Master text"), ""),
        )
        .part(
            "ppt/handoutMasters/handoutMaster1.xml",
            &pml("handoutMaster"),
            &sheet(
                "handoutMaster",
                "",
                &format!(
                    "{}{}",
                    shape(Some("hdr"), "Handout header"),
                    shape(None, "Handout text")
                ),
                "",
            ),
        )
        .part(
            "ppt/commentAuthors.xml",
            &pml("commentAuthors"),
            &format!(
                "<p:cmAuthorLst {NAMESPACES}><p:cmAuthor id=\"0\" name=\"Ann\"/></p:cmAuthorLst>"
            ),
        )
        .part(
            "ppt/comments/comment1.xml",
            &pml("comments"),
            &format!(
                "<p:cmLst {NAMESPACES}>Stray<p:cm authorId=\"0\"><p:pos x=\"1\" y=\"1\"/>\
                 <p:text>First</p:text></p:cm>More<p:cm authorId=\"0\"><p:text>Second</p:text>\
                 </p:cm></p:cmLst>"
            ),
        );
    let input = built.bytes();
    assert_eq!(defers(&input), None);
    let xml = xml(&input);
    assert!(xml.contains("<a href=\"#_ftn1\">To the note</a>"), "{xml}");
    assert!(
        xml.contains("<a href=\"https://layout.example/\">Layout link</a>"),
        "{xml}"
    );
    assert!(
        xml.contains("<a href=\"https://notes.example/\">Notes link</a>"),
        "{xml}"
    );
    assert!(xml.contains("<b>Ann</b>First</p>"), "{xml}");
    assert!(xml.contains("<b>Ann</b>Second</p>"), "{xml}");
    assert!(!xml.contains("Stray") && !xml.contains("More"), "{xml}");
    assert!(
        xml.contains("Handout text") && !xml.contains("Handout header"),
        "{xml}"
    );
    let order: Vec<usize> = (2..=11)
        .map(|number| xml.find(&format!("Slide {number}<")).unwrap())
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{xml}");
}

#[test]
fn defers_macros_only_when_they_are_extracted() {
    let mut built = Built::new(&[("rId1", kind::VBA_PROJECT, "vbaProject.bin")]);
    built.binary(
        "ppt/vbaProject.bin",
        "application/vnd.ms-office.vbaProject",
        b"x",
    );
    let input = built.bytes();
    assert_eq!(defers(&input), None);
    let macros = office(OfficeParserConfig::new().set_extract_macros(true));
    assert!(PptxParser
        .defers(&input, &TikaMetadata::new(), &macros)
        .is_some_and(|reason| reason.contains("vbaProject.bin")));
}

#[test]
#[ignore]
fn extracts_what_tikas_path_extracts_from_tikas_documents() {
    let corpus = conformance_corpus();
    let (parsed, deferred) = documents(&PptxParser, &[&corpus]);
    let names: Vec<String> = deferred
        .iter()
        .map(|file| file.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.is_empty(), "{names:?}");
    let parsed: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    let alike = without_departures(&parsed);
    assert_eq!(parsed.len() - alike.len(), DEPARTURES.len() - 1);
    assert_same_as_tika(&alike, TIKA_PARSER);
}
