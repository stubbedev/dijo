#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{self, assert_same_as_tika_adjusting, Adjustments};

#[cfg(feature = "tika")]
const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.ooxml.OOXMLParser";

/// The workbooks of the corpora in the repository that this port leaves to Tika, with why.
#[cfg(feature = "tika")]
const DEFERRED: [(&str, &str); 0] = [];

/// The workbooks of Tika's own tests that Tika's path fails on: a Strict OOXML workbook, which
/// POI's `XSSFReader` refuses.
#[cfg(feature = "tika")]
const FAILING_CONFORMANCE: [&str; 1] = ["testEXCEL.strict.xlsx"];

#[cfg(feature = "tika")]
fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[cfg(feature = "tika")]
fn deferral(path: &Path, configs: &ParserConfigs) -> Option<String> {
    let input = std::fs::read(path).unwrap();
    XlsxParser.defers(&input, &TikaMetadata::new(), configs)
}

/// Of `documents`, those this port reads under `configs`, after checking that it leaves exactly
/// `deferred` to Tika, by file name and a part of the reason.
#[cfg(feature = "tika")]
fn ported(
    documents: &[PathBuf],
    deferred: &[(&str, &str)],
    configs: &ParserConfigs,
) -> Vec<PathBuf> {
    let mut ported = Vec::new();
    let mut unexpected = Vec::new();
    for document in documents {
        let name = file_name(document);
        let expected = deferred.iter().find(|(deferred, _)| *deferred == name);
        match (deferral(document, configs), expected) {
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

/// The workbooks below `roots`, by Tika's detection.
#[cfg(feature = "tika")]
fn workbooks(roots: &[&Path]) -> Vec<PathBuf> {
    let (mut parsed, deferred) = parity::documents(&XlsxParser, roots);
    parsed.extend(deferred);
    parsed.sort();
    parsed
}

/// How the native parser departs from Tika's path (#93): it writes each link of a sheet once,
/// where Tika writes it again as an outside resource; and its XHTML head says whether a sheet is
/// protected, where Tika's says `false` before it reads the sheets (#95). The div of each chart
/// and diagram is the parity's [`parity::UNWRITTEN_DIVS`].
#[cfg(feature = "tika")]
const DEPARTURES: Adjustments = Adjustments {
    line_breaks: None,
    metadata: &[],
    wrappers: &[],
    repeated: &["<a class=\"external-ref-externalResource\" "],
    final_in_head: &[super::excel::PROTECTED_WORKSHEET],
};

#[cfg(feature = "tika")]
pub(super) fn assert_same_as_tika(documents: &[PathBuf], configs: &ParserConfigs) {
    let documents: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_adjusting(&documents, TIKA_PARSER, configs, DEPARTURES);
}

#[cfg(feature = "tika")]
/// [`assert_same_as_tika`], with each native result rewritten by `rewrite` before it is
/// compared.
pub(super) fn assert_same_as_tika_rewriting(
    documents: &[PathBuf],
    configs: &ParserConfigs,
    rewrite: parity::NativeRewrite,
) {
    let documents: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
    parity::assert_same_as_tika_adjusting_rewriting(
        &documents,
        TIKA_PARSER,
        configs,
        DEPARTURES,
        rewrite,
    );
}

/// The workbook of two million shared strings, which a debug build takes minutes over in the
/// eight extractions each side of a comparison makes, compared on its own.
#[cfg(feature = "tika")]
const LARGE: &str = "many-shared-strings.xlsx";

#[cfg(feature = "tika")]
#[test]
fn extracts_what_tikas_path_extracts() {
    let documents: Vec<PathBuf> = workbooks(&[
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ])
    .into_iter()
    .filter(|document| file_name(document) != LARGE)
    .collect();
    let defaults = ParserConfigs::default();
    assert_same_as_tika(&ported(&documents, &DEFERRED, &defaults), &defaults);
}

#[cfg(feature = "tika")]
#[test]
fn extracts_what_tikas_path_extracts_from_many_shared_strings() {
    let document = Path::new("../test_files/documents").join(LARGE);
    assert_same_as_tika(&[document], &ParserConfigs::default());
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let (failing, documents): (Vec<PathBuf>, Vec<PathBuf>) =
        workbooks(&[&parity::conformance_corpus()])
            .into_iter()
            .partition(|document| FAILING_CONFORMANCE.contains(&file_name(document).as_str()));
    assert_eq!(failing.len(), FAILING_CONFORMANCE.len(), "{failing:?}");
    let defaults = ParserConfigs::default();
    for document in &failing {
        let input = std::fs::read(document).unwrap();
        assert!(matches!(
            XlsxParser.prepare(&input, &TikaMetadata::new(), &defaults),
            Err(Decline::Fails(Error::ParseError(reason))) if reason == STRICT_UNSUPPORTED_BY_TIKA
        ));
    }
    let failing: Vec<&Path> = failing.iter().map(PathBuf::as_path).collect();
    parity::assert_fails_as_tika(&failing);
    assert_same_as_tika(&ported(&documents, &[], &defaults), &defaults);
}

const MAIN_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const RELATIONSHIPS_NS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PACKAGE_RELATIONSHIPS_NS: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";
const STYLES: &str =
    "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
    <numFmts count=\"3\"><numFmt numFmtId=\"164\" formatCode=\"yyyy\\-mm\\-dd\"/>\
    <numFmt numFmtId=\"165\" formatCode=\"#,##0.000;[Red]\\-#,##0.000\"/>\
    <numFmt numFmtId=\"166\" formatCode=\"[h]:mm:ss\"/></numFmts>\
    <cellXfs count=\"8\"><xf numFmtId=\"0\"/><xf numFmtId=\"164\"/><xf numFmtId=\"165\"/>\
    <xf numFmtId=\"166\"/><xf numFmtId=\"9\"/><xf numFmtId=\"13\"/><xf numFmtId=\"22\"/>\
    <xf numFmtId=\"44\"/></cellXfs></styleSheet>";

/// A part of a synthetic workbook: its name, content type and content.
struct Part {
    name: String,
    content_type: String,
    content: Vec<u8>,
}

fn part(name: &str, content_type: &str, content: &str) -> Part {
    Part {
        name: name.to_owned(),
        content_type: content_type.to_owned(),
        content: content.as_bytes().to_vec(),
    }
}

fn kind(name: &str) -> String {
    format!("{RELATIONSHIPS_NS}/{name}")
}

fn relationships(relationships: &[(&str, &str, &str)]) -> String {
    let mut xml = format!("<Relationships xmlns=\"{PACKAGE_RELATIONSHIPS_NS}\">");
    for (id, kind, target) in relationships {
        let mode = if target.contains("://") {
            " TargetMode=\"External\""
        } else {
            ""
        };
        xml.push_str(&format!(
            "<Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{target}\"{mode}/>"
        ));
    }
    xml.push_str("</Relationships>");
    xml
}

/// A sheet of a synthetic workbook: its name, state, `worksheet` content and the relationships
/// of its part.
struct SheetSpec {
    name: &'static str,
    state: &'static str,
    content: String,
    relationships: Vec<(String, String, String)>,
}

fn sheet(name: &'static str, content: String) -> SheetSpec {
    SheetSpec {
        name,
        state: "",
        content,
        relationships: Vec::new(),
    }
}

fn borrowed(relationships: &[(String, String, String)]) -> Vec<(&str, &str, &str)> {
    relationships
        .iter()
        .map(|(id, kind, target)| (id.as_str(), kind.as_str(), target.as_str()))
        .collect()
}

/// A workbook of `sheets` with `strings` shared, the styles of [`STYLES`], the workbook
/// element's further `extra` content and relationships, and further `parts`.
fn workbook(
    sheets: &[SheetSpec],
    strings: &[&str],
    extra: &str,
    workbook_relationships: &[(String, String, String)],
    parts: Vec<Part>,
) -> Vec<u8> {
    let mut all = Vec::new();
    let mut sheet_elements = String::new();
    let mut rels = vec![
        (
            "rIdStyles".to_owned(),
            kind("styles"),
            "styles.xml".to_owned(),
        ),
        (
            "rIdStrings".to_owned(),
            kind("sharedStrings"),
            "sharedStrings.xml".to_owned(),
        ),
    ];
    for (index, spec) in sheets.iter().enumerate() {
        let number = index + 1;
        let state = if spec.state.is_empty() {
            String::new()
        } else {
            format!(" state=\"{}\"", spec.state)
        };
        sheet_elements.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{number}\"{state} r:id=\"rIdSheet{number}\"/>",
            spec.name
        ));
        rels.push((
            format!("rIdSheet{number}"),
            kind("worksheet"),
            format!("worksheets/sheet{number}.xml"),
        ));
        all.push(part(
            &format!("xl/worksheets/sheet{number}.xml"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
            &format!(
                "<worksheet xmlns=\"{MAIN_NS}\" xmlns:r=\"{RELATIONSHIPS_NS}\">{}</worksheet>",
                spec.content
            ),
        ));
        if !spec.relationships.is_empty() {
            all.push(part(
                &format!("xl/worksheets/_rels/sheet{number}.xml.rels"),
                "application/vnd.openxmlformats-package.relationships+xml",
                &relationships(&borrowed(&spec.relationships)),
            ));
        }
    }
    rels.extend(workbook_relationships.iter().cloned());
    all.push(part(
        "xl/workbook.xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        &format!(
            "<workbook xmlns=\"{MAIN_NS}\" xmlns:r=\"{RELATIONSHIPS_NS}\">{extra}<sheets>{sheet_elements}</sheets></workbook>"
        ),
    ));
    all.push(part(
        "xl/_rels/workbook.xml.rels",
        "application/vnd.openxmlformats-package.relationships+xml",
        &relationships(&borrowed(&rels)),
    ));
    all.push(part(
        "xl/styles.xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml",
        STYLES,
    ));
    let mut shared = format!(
        "<sst xmlns=\"{MAIN_NS}\" count=\"{0}\" uniqueCount=\"{0}\">",
        strings.len()
    );
    for string in strings {
        shared.push_str(&format!("<si>{string}</si>"));
    }
    shared.push_str("</sst>");
    all.push(part(
        "xl/sharedStrings.xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml",
        &shared,
    ));
    all.push(part(
        "_rels/.rels",
        "application/vnd.openxmlformats-package.relationships+xml",
        &relationships(&[("rId1", &kind("officeDocument"), "xl/workbook.xml")]),
    ));
    all.extend(parts);
    zip(&all)
}

fn zip(parts: &[Part]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut types = String::from(
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"png\" ContentType=\"image/png\"/>",
    );
    for part in parts {
        if !part.name.ends_with(".rels") && !part.name.ends_with(".png") {
            types.push_str(&format!(
                "<Override PartName=\"/{}\" ContentType=\"{}\"/>",
                part.name, part.content_type
            ));
        }
    }
    types.push_str("</Types>");
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file("[Content_Types].xml", options).unwrap();
    writer.write_all(types.as_bytes()).unwrap();
    for part in parts {
        writer.start_file(part.name.as_str(), options).unwrap();
        writer.write_all(&part.content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[cfg(feature = "tika")]
fn assert_snippets_same_as_tika(snippets: &[(&str, Vec<u8>)], configs: &ParserConfigs) {
    let scratch = parity::Scratch::new("xlsx-snippets");
    let mut paths = Vec::new();
    for (name, bytes) in snippets {
        assert_eq!(
            XlsxParser.defers(bytes, &TikaMetadata::new(), configs),
            None,
            "{name}"
        );
        paths.push(scratch.write(&format!("{name}.xlsx"), bytes));
    }
    assert_same_as_tika(&paths, configs);
}

fn cells(rows: &[&str]) -> String {
    format!("<sheetData>{}</sheetData>", rows.concat())
}

fn rel(id: &str, kind_name: &str, target: &str) -> (String, String, String) {
    (id.to_owned(), kind_name.to_owned(), target.to_owned())
}

/// Workbooks of the features Tika reads, each small, compared with Tika's path.
fn snippets() -> Vec<(&'static str, Vec<u8>)> {
    // No negative number is in a date format: Tika writes it with the date format it made for
    // an earlier cell (#92), which `data_format`'s tests check the port does not.
    let numbers = cells(&[
        "<row r=\"1\"><c r=\"A1\"><v>1234.5678</v></c><c r=\"B1\" s=\"1\"><v>45000.75</v></c>\
         <c r=\"C1\" s=\"2\"><v>-1234.5678</v></c><c r=\"D1\" s=\"3\"><v>1.5</v></c>\
         <c r=\"E1\" s=\"4\"><v>0.256</v></c><c r=\"F1\" s=\"5\"><v>1.25</v></c>\
         <c r=\"G1\" s=\"6\"><v>45000.5</v></c><c r=\"H1\" s=\"7\"><v>-12</v></c></row>",
        "<row r=\"3\"><c r=\"B3\" t=\"b\"><v>1</v></c><c r=\"C3\" t=\"b\"><v>0</v></c>\
         <c r=\"D3\" t=\"e\"><v>#DIV/0!</v></c><c r=\"F3\" t=\"inlineStr\"><is><t>inline</t></is></c>\
         <c r=\"G3\" t=\"s\"><v>0</v></c><c r=\"H3\" t=\"s\"><v>1</v></c></row>",
        "<row r=\"4\"><c r=\"A4\"><f>A1*2</f><v>2469.1356</v></c><c r=\"B4\" t=\"str\"><f>\"x\"&amp;\"y\"</f><v>xy</v></c>\
         <c r=\"C4\" s=\"1\"><f>B1+1</f><v>45001.75</v></c><c r=\"D4\"><v>not a number</v></c>\
         <c><v>7</v></c><c r=\"G4\" s=\"1\"><v>5</v></c></row>",
        "<row><c r=\"A5\" t=\"s\"><v>2</v></c></row>",
    ]);
    let commented = format!(
        "{}<legacyDrawing r:id=\"rIdVml\"/>",
        cells(&[
            "<row r=\"1\"><c r=\"A1\"><v>1</v></c><c r=\"C1\"><v>3</v></c></row>",
            "<row r=\"3\"><c r=\"B3\"><v>2</v></c></row>",
        ])
    );
    let comments = format!(
        "<comments xmlns=\"{MAIN_NS}\"><authors><author>Ann</author><author>Bo</author></authors>\
         <commentList><comment ref=\"C1\" authorId=\"0\"><text><r><t>on a cell</t></r></text></comment>\
         <comment ref=\"B1\" authorId=\"1\"><text><r><t>on</t></r><r><t>an empty cell</t></r></text></comment>\
         <comment ref=\"D2\" authorId=\"0\"><text><t>row two</t></text></comment>\
         <comment ref=\"A9\" authorId=\"5\"><text><t>after the data</t></text></comment></commentList></comments>"
    );
    let layout = format!(
        "<sheetPr><outlinePr/></sheetPr><cols><col min=\"2\" max=\"2\" hidden=\"1\"/></cols>{}\
         <sheetProtection sheet=\"1\"/><hyperlinks><hyperlink ref=\"A1\" r:id=\"rIdLink\"/></hyperlinks>\
         <headerFooter><oddHeader>&amp;LLeft &amp;D&amp;CCentre&amp;R&amp;\"Arial,Bold\"Right</oddHeader>\
         <oddFooter>&amp;CPage &amp;P of &amp;N</oddFooter><firstHeader>First</firstHeader></headerFooter>\
         <drawing r:id=\"rIdDrawing\"/>",
        cells(&[
            "<row r=\"2\" hidden=\"1\"><c r=\"A2\"><v>2</v></c></row>",
            "<row r=\"5\"><c r=\"C5\"><v>5</v></c></row>",
        ])
    );
    let drawing = format!(
        "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{RELATIONSHIPS_NS}\">\
         <xdr:twoCellAnchor><xdr:sp><xdr:nvSpPr><xdr:cNvPr id=\"2\" name=\"Box\"><a:hlinkClick r:id=\"rIdShapeLink\"/></xdr:cNvPr></xdr:nvSpPr>\
         <xdr:txBody><a:p><a:r><a:t>Shape text</a:t></a:r></a:p><a:p><a:r><a:t> second </a:t></a:r></a:p></xdr:txBody>\
         </xdr:sp></xdr:twoCellAnchor></xdr:wsDr>"
    );
    let threaded = "<ThreadedComments xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\">\
         <threadedComment ref=\"A1\" id=\"{1}\" personId=\"{9}\"><text>First thought</text></threadedComment>\
         <threadedComment ref=\"A1\" id=\"{2}\" personId=\"{9}\"><text>Reply</text></threadedComment></ThreadedComments>";
    let persons = "<personList xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\">\
         <person displayName=\"Ann Example\" id=\"{9}\"/><person displayName=\" \" id=\"{8}\"/></personList>";
    let external = format!(
        "<externalLink xmlns=\"{MAIN_NS}\" xmlns:r=\"{RELATIONSHIPS_NS}\"><externalBook r:id=\"rId1\">\
         <sheetNames><sheetName val=\"S\"/></sheetNames></externalBook><ddeLink ddeService=\"Excel\" ddeTopic=\"Book\"/></externalLink>"
    );
    let connections = format!(
        "<connections xmlns=\"{MAIN_NS}\"><connection id=\"1\" name=\"web\"><webPr url=\"http://example.com/data\"/></connection>\
         <connection id=\"2\" name=\"db\"><dbPr connection=\"DSN=db\"/></connection></connections>"
    );
    let rels_type = "application/vnd.openxmlformats-package.relationships+xml";
    vec![
        (
            // The second sheet is protected, past where the length limit cuts the first.
            "late-protection",
            workbook(
                &[
                    sheet(
                        "Long",
                        cells(&[
                            "<row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>A cell of text long \
                             enough that the length limit of a string extraction cuts it short \
                             before the sheet after it is read</t></is></c></row>",
                        ]),
                    ),
                    sheet(
                        "Protected",
                        format!(
                            "{}<sheetProtection sheet=\"1\"/>",
                            cells(&["<row r=\"1\"><c r=\"A1\"><v>1</v></c></row>"])
                        ),
                    ),
                ],
                &[],
                "",
                &[],
                Vec::new(),
            ),
        ),
        (
            "cells",
            workbook(
                &[sheet("Numbers &amp; text", numbers), sheet("Empty", String::new())],
                &[
                    "<t>shared</t>",
                    "<r><t>rich </t></r><r><rPr><b/></rPr><t>text</t></r>",
                    "<t>漢字</t><rPh sb=\"0\" eb=\"2\"><t>かんじ</t></rPh>",
                ],
                "",
                &[],
                Vec::new(),
            ),
        ),
        (
            "comments",
            workbook(
                &[SheetSpec {
                    name: "Commented",
                    state: "",
                    content: commented,
                    relationships: vec![
                        rel("rIdComments", &kind("comments"), "../comments1.xml"),
                        rel("rIdVml", &kind("vmlDrawing"), "../drawings/vmlDrawing1.vml"),
                        rel(
                            "rIdThreaded",
                            "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment",
                            "../threadedComments/threadedComment1.xml",
                        ),
                    ],
                }],
                &[],
                "",
                &[rel(
                    "rIdPersons",
                    "http://schemas.microsoft.com/office/2017/10/relationships/person",
                    "persons/person.xml",
                )],
                vec![
                    part(
                        "xl/comments1.xml",
                        "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml",
                        &comments,
                    ),
                    part(
                        "xl/drawings/vmlDrawing1.vml",
                        "application/vnd.openxmlformats-officedocument.vmlDrawing",
                        "<xml/>",
                    ),
                    part(
                        "xl/threadedComments/threadedComment1.xml",
                        "application/vnd.ms-excel.threadedcomments+xml",
                        threaded,
                    ),
                    part("xl/persons/person.xml", "application/vnd.ms-excel.person+xml", persons),
                ],
            ),
        ),
        (
            "layout",
            workbook(
                &[
                    SheetSpec {
                        name: "Laid out",
                        state: "",
                        content: layout,
                        relationships: vec![
                            rel("rIdLink", &kind("hyperlink"), "https://example.com/a?b=c"),
                            rel("rIdLink2", &kind("hyperlink"), "http://example.com/2"),
                            rel("rIdDrawing", &kind("drawing"), "../drawings/drawing1.xml"),
                        ],
                    },
                    SheetSpec {
                        name: "Hidden",
                        state: "hidden",
                        content: String::new(),
                        relationships: Vec::new(),
                    },
                    SheetSpec {
                        name: "Very hidden",
                        state: "veryHidden",
                        content: String::new(),
                        relationships: Vec::new(),
                    },
                ],
                &[],
                "<fileVersion appName=\"xl\"/><workbookPr codeName=\"ThisWorkbook\"/>",
                &[
                    rel("rIdExternal", &kind("externalLink"), "externalLinks/externalLink1.xml"),
                    rel("rIdExternal2", &kind("externalLink"), "file:///C:/other.xlsx"),
                    rel("rIdConnections", &kind("connections"), "connections.xml"),
                ],
                vec![
                    part(
                        "xl/drawings/drawing1.xml",
                        "application/vnd.openxmlformats-officedocument.drawing+xml",
                        &drawing,
                    ),
                    part(
                        "xl/drawings/_rels/drawing1.xml.rels",
                        rels_type,
                        &relationships(&[(
                            "rIdShapeLink",
                            &kind("hyperlink"),
                            "https://example.com/shape",
                        )]),
                    ),
                    part(
                        "xl/externalLinks/externalLink1.xml",
                        "application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml",
                        &external,
                    ),
                    part(
                        "xl/externalLinks/_rels/externalLink1.xml.rels",
                        rels_type,
                        &relationships(&[(
                            "rId1",
                            &kind("externalLinkPath"),
                            "file:///C:/book.xlsx",
                        )]),
                    ),
                    part(
                        "xl/connections.xml",
                        "application/vnd.openxmlformats-officedocument.spreadsheetml.connections+xml",
                        &connections,
                    ),
                    part(
                        "xl/customData/item1.data",
                        "application/vnd.ms-excel.customDataProperties+xml",
                        "<x/>",
                    ),
                ],
            ),
        ),
    ]
}

/// A PNG of one white pixel.
const PIXEL: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xff, 0xff, 0x3f,
    0x00, 0x05, 0xfe, 0x02, 0xfe, 0xdc, 0xcc, 0x59, 0xe7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Workbooks with a picture and a chart drawn on their sheets.
fn drawing_snippets() -> Vec<(&'static str, Vec<u8>)> {
    let rels_type = "application/vnd.openxmlformats-package.relationships+xml";
    let drawing = format!(
        "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{RELATIONSHIPS_NS}\" \
         xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\">\
         <xdr:twoCellAnchor><xdr:pic><xdr:blipFill><a:blip r:embed=\"rIdImage\"/></xdr:blipFill></xdr:pic></xdr:twoCellAnchor>\
         <xdr:twoCellAnchor><xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id=\"rIdChart\"/></a:graphicData></a:graphic></xdr:graphicFrame></xdr:twoCellAnchor>\
         </xdr:wsDr>"
    );
    let chart = "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><c:chart><c:title><c:tx><c:rich>\
         <a:p><a:r><a:t>Ships by year</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:barChart><c:ser>\
         <c:cat><c:strRef><c:strCache><c:pt idx=\"0\"><c:v>1921</c:v></c:pt></c:strCache></c:strRef></c:cat>\
         <c:val><c:numRef><c:numCache><c:pt idx=\"0\"><c:v>412</c:v></c:pt></c:numCache></c:numRef></c:val>\
         </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>";
    let mut picture = part("xl/media/image1.png", "image/png", "");
    picture.content = PIXEL.to_vec();
    vec![(
        "drawn",
        workbook(
            &[
                SheetSpec {
                    name: "Drawn",
                    state: "",
                    content: format!(
                        "{}<drawing r:id=\"rIdDrawing\"/>",
                        cells(&["<row r=\"1\"><c r=\"A1\"><v>412</v></c></row>"])
                    ),
                    relationships: vec![rel(
                        "rIdDrawing",
                        &kind("drawing"),
                        "../drawings/drawing1.xml",
                    )],
                },
                sheet(
                    "Plain",
                    cells(&["<row r=\"1\"><c r=\"A1\"><v>1</v></c></row>"]),
                ),
            ],
            &[],
            "",
            &[],
            vec![
                part(
                    "xl/drawings/drawing1.xml",
                    "application/vnd.openxmlformats-officedocument.drawing+xml",
                    &drawing,
                ),
                part(
                    "xl/drawings/_rels/drawing1.xml.rels",
                    rels_type,
                    &relationships(&[
                        ("rIdImage", &kind("image"), "../media/image1.png"),
                        ("rIdChart", &kind("chart"), "../charts/chart1.xml"),
                    ]),
                ),
                part(
                    "xl/charts/chart1.xml",
                    "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
                    chart,
                ),
                picture,
            ],
        ),
    )]
}

#[cfg(feature = "tika")]
#[test]
fn extracts_what_tikas_path_extracts_from_snippets() {
    assert_snippets_same_as_tika(&snippets(), &ParserConfigs::default());
}

#[cfg(feature = "tika")]
#[test]
fn extracts_what_tikas_path_extracts_from_drawings() {
    assert_snippets_same_as_tika(&drawing_snippets(), &ParserConfigs::default());
}

#[test]
fn defers_links_inside_the_package() {
    let input = workbook(
        &[SheetSpec {
            name: "Linked",
            state: "",
            content: String::new(),
            relationships: vec![rel("rIdLink", &kind("hyperlink"), "#Sheet2!A1")],
        }],
        &[],
        "",
        &[],
        Vec::new(),
    );
    let reason = XlsxParser
        .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
        .unwrap();
    assert!(reason.contains("links to #Sheet2!A1"), "{reason}");
}

#[cfg(feature = "tika")]
#[test]
fn extracts_what_tikas_path_extracts_from_snippets_with_other_settings() {
    use crate::config::OfficeParserConfig;
    let office = |config: OfficeParserConfig| ParserConfigs {
        office: config,
        ..ParserConfigs::default()
    };
    // Missing rows are not compared: Tika writes an empty row for every row before each row
    // (#93), which `writes_each_missing_row_once` checks the port does not.
    for configs in [
        office(OfficeParserConfig::new().set_include_headers_and_footers(false)),
        office(OfficeParserConfig::new().set_include_shape_based_content(false)),
        office(OfficeParserConfig::new().set_concatenate_phonetic_runs(false)),
    ] {
        assert_snippets_same_as_tika(&snippets(), &configs);
    }
}

#[test]
fn writes_each_missing_row_once() {
    use crate::config::OfficeParserConfig;
    let gaps = workbook(
        &[sheet(
            "Gaps",
            cells(&[
                "<row r=\"1\"><c r=\"A1\"><v>1</v></c></row>",
                "<row r=\"4\"><c r=\"A4\"><v>4</v></c></row>",
                "<row r=\"5\"><c r=\"A5\"><v>5</v></c></row>",
            ]),
        )],
        &[],
        "",
        &[],
        Vec::new(),
    );
    let rows = |missing: bool| {
        let (xml, _) = crate::Extractor::new()
            .set_office_config(OfficeParserConfig::new().set_include_missing_rows(missing))
            .set_xml_output(true)
            .extract_bytes_to_string(&gaps)
            .unwrap();
        xml.matches("<tr>").count()
    };
    assert_eq!(rows(false), 3);
    assert_eq!(rows(true), 5);
}

/// The XHTML and metadata the native parser extracts from `workbook`.
fn extract(workbook: &[u8]) -> (String, crate::Metadata) {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(workbook)
        .unwrap()
}

/// Each link of a sheet is written once, where Tika writes it again as an outside resource, and
/// each chart in a div of its own (#93).
#[test]
fn writes_links_once_and_charts_in_their_divs() {
    let snippets: HashMap<&str, Vec<u8>> =
        snippets().into_iter().chain(drawing_snippets()).collect();
    let (layout, _) = extract(&snippets["layout"]);
    let link = "https://example.com/a?b=c";
    assert_eq!(layout.matches(link).count(), 2, "{layout}");
    assert!(
        layout.contains(&format!("<a href=\"{link}\">{link}</a>")),
        "{layout}"
    );
    let (drawn, _) = extract(&snippets["drawn"]);
    let chart = drawn.find("<div class=\"chart\">").expect("a chart div");
    assert!(drawn[chart..].contains("Ships by year"), "{drawn}");
}

/// A shape's link is looked up among its own drawing's relationships only (#93).
#[test]
fn finds_a_shapes_link_in_its_own_drawing() {
    let rels_type = "application/vnd.openxmlformats-package.relationships+xml";
    let drawing = |text: &str| {
        format!(
            "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
             xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{RELATIONSHIPS_NS}\">\
             <xdr:twoCellAnchor><xdr:sp><xdr:nvSpPr><xdr:cNvPr id=\"2\" name=\"Box\"><a:hlinkClick r:id=\"rIdShapeLink\"/></xdr:cNvPr></xdr:nvSpPr>\
             <xdr:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></xdr:txBody></xdr:sp></xdr:twoCellAnchor></xdr:wsDr>"
        )
    };
    let drawn = |name: &'static str, number: usize| SheetSpec {
        name,
        state: "",
        content: "<sheetData/><drawing r:id=\"rIdDrawing\"/>".to_owned(),
        relationships: vec![rel(
            "rIdDrawing",
            &kind("drawing"),
            &format!("../drawings/drawing{number}.xml"),
        )],
    };
    let drawing_type = "application/vnd.openxmlformats-officedocument.drawing+xml";
    let bytes = workbook(
        &[drawn("One", 1), drawn("Two", 2)],
        &[],
        "",
        &[],
        vec![
            part("xl/drawings/drawing1.xml", drawing_type, &drawing("first")),
            part(
                "xl/drawings/_rels/drawing1.xml.rels",
                rels_type,
                &relationships(&[(
                    "rIdShapeLink",
                    &kind("hyperlink"),
                    "https://example.com/one",
                )]),
            ),
            part("xl/drawings/drawing2.xml", drawing_type, &drawing("second")),
        ],
    );
    let (xml, _) = extract(&bytes);
    let link = "<a href=\"https://example.com/one\">";
    assert_eq!(xml.matches(link).count(), 1, "{xml}");
    assert!(xml.find(link) < xml.find("second"), "{xml}");
}

/// Elements are matched by namespace and Power Query is found by its content type, not by the
/// names of parts (#95).
#[test]
fn matches_elements_by_namespace_and_power_query_by_content_type() {
    let bytes = workbook(
        &[sheet(
            "Plain",
            "<sheetData><row r=\"1\"><c r=\"A1\"><v>1</v></c></row></sheetData>\
             <x:sheetProtection xmlns:x=\"urn:other\" sheet=\"1\"/><x:cols xmlns:x=\"urn:other\">\
             <x:col min=\"1\" max=\"1\" hidden=\"1\"/></x:cols>"
                .to_owned(),
        )],
        &[],
        "<x:sheets xmlns:x=\"urn:other\"><x:sheet name=\"Ghost\" state=\"hidden\" r:id=\"rIdSheet1\"/></x:sheets>",
        &[],
        vec![part("xl/customData/item1.data", "application/binary", "<x/>")],
    );
    let (xml, metadata) = extract(&bytes);
    assert_eq!(xml.matches("<h1>").count(), 1, "{xml}");
    assert_eq!(metadata["msoffice:excel:protected-worksheet"], ["false"]);
    for name in [
        "msoffice:excel:has-hidden-cols",
        "msoffice:excel:has-hidden-sheets",
        "msoffice:excel:has-power-query",
    ] {
        assert!(!metadata.contains_key(name), "{name}: {metadata:?}");
    }
    let layout = snippets()
        .into_iter()
        .find(|(name, _)| *name == "layout")
        .unwrap()
        .1;
    let (xml, metadata) = extract(&layout);
    assert!(
        xml.contains("<meta name=\"msoffice:excel:protected-worksheet\" content=\"true\" />"),
        "{xml}"
    );
    assert_eq!(metadata["msoffice:excel:has-power-query"], ["true"]);
    assert_eq!(metadata["msoffice:excel:protected-worksheet"], ["true"]);
}

/// A workbook whose first sheet's XML fails in its second row, after a hidden row, with a
/// protection the failure keeps Tika from noting, then a sheet that reads.
fn failing_sheet() -> Vec<u8> {
    workbook(
        &[
            sheet(
                "Broken",
                "<sheetData><row r=\"1\" hidden=\"1\"><c r=\"A1\"><v>1</v></c></row>\
                 <row r=\"2\"><c r=\"A2\"><v>2</v></c><c r=\"B2\"><v>3</v></row></sheetData>\
                 <sheetProtection sheet=\"1\"/>"
                    .to_owned(),
            ),
            sheet(
                "Fine",
                "<sheetData><row r=\"1\"><c r=\"A1\"><v>4</v></c></row></sheetData>".to_owned(),
            ),
        ],
        &[],
        "",
        &[],
        Vec::new(),
    )
}

/// A sheet whose XML fails is recorded as Tika's `buildXHTML` records it: a warning, the row
/// left open ended, and nothing noted of the sheet; the workbook goes on (#96).
#[test]
fn records_a_sheet_whose_xml_fails_and_goes_on() {
    let input = failing_sheet();
    assert_eq!(
        XlsxParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default()),
        None
    );
    let (xml, metadata) = extract(&input);
    assert!(xml.contains("<td>1</td></tr>"), "{xml}");
    assert!(xml.contains("<td>2</td>"), "{xml}");
    assert!(xml.contains("<h1>Fine</h1>"), "{xml}");
    assert!(xml.contains("<td>4</td>"), "{xml}");
    assert_eq!(
        xml.matches("<tr>").count(),
        xml.matches("</tr>").count(),
        "{xml}"
    );
    let warnings = &metadata["tk:exception:warn"];
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("org.xml.sax.SAXParseException: /xl/worksheets/sheet1.xml"),
        "{warnings:?}"
    );
    assert_eq!(metadata["msoffice:excel:protected-worksheet"], ["false"]);
    assert!(!metadata.contains_key("msoffice:excel:has-hidden-rows"));
}

#[cfg(feature = "tika")]
#[test]
fn records_a_sheet_whose_xml_fails_as_tikas_path_does() {
    let scratch = parity::Scratch::new("xlsx-failing-sheet");
    let documents = [scratch.write("failing.xlsx", &failing_sheet())];
    assert_same_as_tika_rewriting(
        &documents,
        &ParserConfigs::default(),
        |text, metadata, tika| {
            parity::with_tikas_values(
                text,
                metadata,
                tika,
                crate::formats::embedded::EXCEPTION_WARNING,
            )
        },
    );
}

/// A Strict OOXML workbook, whose main document only a Strict relationship leads to, fails
/// natively as Tika's path fails it (#149).
#[cfg(feature = "tika")]
#[test]
fn fails_a_strict_workbook_as_tikas_path_does() {
    let scratch = parity::Scratch::new("xlsx-strict");
    let strict = crate::formats::ooxml::testing::package_with(
        &crate::formats::ooxml::testing::relationship(
            "rId1",
            kind::STRICT_OFFICE_DOCUMENT,
            "xl/workbook.xml",
        ),
        &[(
            "xl/workbook.xml",
            WORKBOOK_TYPES[0],
            b"<workbook xmlns=\"http://purl.oclc.org/ooxml/spreadsheetml/main\"><sheets/></workbook>",
        )],
    );
    let document = scratch.write("strict.xlsx", &strict);
    assert!(matches!(
        XlsxParser.prepare(&strict, &TikaMetadata::new(), &ParserConfigs::default()),
        Err(Decline::Fails(Error::ParseError(reason))) if reason == STRICT_UNSUPPORTED_BY_TIKA
    ));
    parity::assert_fails_as_tika(&[&document]);
}

/// The message Tika's path fails a Strict OOXML workbook with.
#[cfg(feature = "tika")]
const STRICT_UNSUPPORTED_BY_TIKA: &str = "TIKA-198: Illegal IOException from \
    org.apache.tika.parser.microsoft.ooxml.OOXMLParser <- caused by java.io.IOException: \
    org.apache.poi.ooxml.POIXMLException: Strict OOXML isn't currently supported, please see \
    bug #57699";
