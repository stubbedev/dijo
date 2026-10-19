use super::*;
use crate::formats::ooxml::testing::{self, relationship};

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

fn shortfall(input: &[u8]) -> Option<String> {
    PptxParser.shortfall(input, &TikaMetadata::new(), &ParserConfigs::default())
}

fn office(config: OfficeParserConfig) -> ParserConfigs {
    ParserConfigs {
        office: config,
        ..ParserConfigs::default()
    }
}

#[test]
fn falls_short_on_ole_objects_tika_fails_to_read() {
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
    assert!(shortfall(&built.bytes())
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
/// notes by their own relationships, a comment's own text, comment authors in the part's order,
/// placeholders of the handout master, the div of each kind of part, and slides past the ninth in
/// order.
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
                "<p:cmAuthorLst {NAMESPACES}><p:cmAuthor id=\"10\" name=\"Zed\"/>\
                 <p:cmAuthor id=\"0\" name=\"Ann\"/></p:cmAuthorLst>"
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
    assert_eq!(shortfall(&input), None);
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
    let persons: Vec<usize> = ["Zed", "Ann"]
        .iter()
        .map(|name| {
            let meta = format!(
                "<meta name=\"{}\" content=\"{name}\"",
                name::COMMENT_PERSONS
            );
            xml.find(&meta).unwrap()
        })
        .collect();
    assert!(persons[0] < persons[1], "{xml}");
    for div in [
        "<div class=\"slide-master-content\"><p><a href=\"https://layout.example/\">",
        "<div class=\"slide-notes\"><p><a href=\"https://notes.example/\">",
        "<div class=\"slide-master\"><p>Master text</p>",
        "<div class=\"slide-handout-master\"><p>Handout text</p>",
    ] {
        assert!(xml.contains(div), "{div} in {xml}");
    }
    let order: Vec<usize> = (2..=11)
        .map(|number| xml.find(&format!("Slide {number}<")).unwrap())
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{xml}");
}

#[test]
fn differs_on_macros_that_do_not_open_only_when_they_are_extracted() {
    let mut built = Built::new(&[("rId1", kind::VBA_PROJECT, "vbaProject.bin")]);
    built.binary(
        "ppt/vbaProject.bin",
        "application/vnd.ms-office.vbaProject",
        b"x",
    );
    let input = built.bytes();
    assert_eq!(shortfall(&input), None);
    let macros = office(OfficeParserConfig::new().set_extract_macros(true));
    assert!(PptxParser
        .shortfall(&input, &TikaMetadata::new(), &macros)
        .is_some_and(|reason| reason.contains("Tika fails the package")));
}

/// The modules of a presentation's VBA project are extracted when asked for, after its slides
/// (#186).
#[test]
fn extracts_macros_when_asked_to() {
    const SOURCE: &str = "Attribute VB_Name = \"Modul1\"\r\nSub Lysbillede()\r\nEnd Sub\r\n";
    let mut built = Built::new(&[("rId1", kind::VBA_PROJECT, "vbaProject.bin")]);
    built.binary(
        "ppt/vbaProject.bin",
        "application/vnd.ms-office.vbaProject",
        &dijo_office::formats::ole2::vba::testing::project(&[("Modul1", SOURCE)]),
    );
    let input = built.bytes();
    let macros = office(OfficeParserConfig::new().set_extract_macros(true));
    assert_eq!(
        PptxParser.shortfall(&input, &TikaMetadata::new(), &macros),
        None
    );
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .set_office_config(OfficeParserConfig::new().set_extract_macros(true))
        .extract_bytes_to_string(&input)
        .unwrap();
    assert!(xml.contains("Sub Lysbillede()"), "{xml}");
    assert!(!self::xml(&input).contains("Sub Lysbillede()"));
}
