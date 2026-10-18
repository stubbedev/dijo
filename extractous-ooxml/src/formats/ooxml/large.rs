//! Packages too large to read whole, holding parts too large to decode into memory, or
//! encrypted around packages of either kind, extracted under a memory budget far below their
//! size.

use std::io::{self, Write};

use extractous_core::large::{LargeFile, BUDGET};
use extractous_core::Extractor;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::extractor::kind;
use super::testing::{content_types, relationship, relationships};
use super::RELATIONSHIPS_CONTENT_TYPE;
use crate::formats::crypt::{testing::encrypted_package, DEFAULT_PASSWORD};
use crate::input::MAX_READ_WHOLE;
use crate::ole2::{self, Node, Storage};

/// The size of a large part: past what a spool holds in memory, so that it decodes to a file.
const LARGE: u64 = MAX_READ_WHOLE + (16 << 20);
/// The text after the large content, which only a document read to its end writes.
const END: &str = "Slutningen af dokumentet";
const OOXML_PARSER: &str = "extractous::OOXMLParser";
const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";
const DOCUMENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

/// What writes a part's content.
type Content<'c> = Box<dyn FnOnce(&mut dyn Write) -> io::Result<()> + 'c>;

/// A part: its name, its content type and what writes its content.
struct Part<'c> {
    name: &'c str,
    content_type: &'c str,
    content: Content<'c>,
}

impl<'c> Part<'c> {
    fn new(name: &'c str, content_type: &'c str, content: Content<'c>) -> Self {
        Self {
            name,
            content_type,
            content,
        }
    }

    /// A part of `text`.
    fn text(name: &'c str, content_type: &'c str, text: String) -> Self {
        Self::new(
            name,
            content_type,
            Box::new(move |out| out.write_all(text.as_bytes())),
        )
    }

    /// The relationships part of the part `source`, of `relationships`.
    fn relationships(name: &'c str, relationships: &str) -> Self {
        Self::text(
            name,
            RELATIONSHIPS_CONTENT_TYPE,
            super::testing::relationships(relationships),
        )
    }
}

/// Writes to `out` a package of `parts`, whose own relationship leads to `main` as its office
/// document, each part compressed with `method`, as a stream.
fn write_package(
    out: &mut dyn Write,
    method: CompressionMethod,
    main_relationship: (&str, &str),
    parts: Vec<Part>,
) -> io::Result<()> {
    let options = SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(Some(1))
        .large_file(true);
    let types = content_types(parts.iter().map(|part| (part.name, part.content_type)));
    let (kind, main) = main_relationship;
    let mut zip = ZipWriter::new_stream(out);
    zip.start_file("[Content_Types].xml", options)
        .map_err(io::Error::other)?;
    zip.write_all(types.as_bytes())?;
    zip.start_file("_rels/.rels", options)
        .map_err(io::Error::other)?;
    zip.write_all(relationships(&relationship("rId1", kind, main)).as_bytes())?;
    for part in parts {
        zip.start_file(part.name, options)
            .map_err(io::Error::other)?;
        (part.content)(&mut zip)?;
    }
    zip.finish().map_err(io::Error::other)?.flush()
}

/// Writes to `out` `head`, the units `unit` makes of their indexes up to `length` bytes in
/// all, then `tail`.
fn repeated(
    out: &mut dyn Write,
    head: &str,
    length: u64,
    mut unit: impl FnMut(u64) -> String,
    tail: &str,
) -> io::Result<()> {
    out.write_all(head.as_bytes())?;
    let mut written = 0;
    for index in 0.. {
        if written >= length {
            break;
        }
        let unit = unit(index);
        out.write_all(unit.as_bytes())?;
        written += unit.len() as u64;
    }
    out.write_all(tail.as_bytes())
}

/// A Word paragraph of `text`.
fn paragraph(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

/// The main part of a Word document of paragraphs to `length` bytes, then one of [`END`].
fn word_document<'c>(length: u64) -> Part<'c> {
    Part::new(
        "word/document.xml",
        DOCUMENT_TYPE,
        Box::new(move |out| {
            repeated(
                out,
                &format!("<w:document {W}><w:body>"),
                length,
                |index| paragraph(&format!("Linje {index} af et meget langt dokument")),
                &format!("{}</w:body></w:document>", paragraph(END)),
            )
        }),
    )
}

/// Writes to `out` a Word document whose main part holds `length` bytes of paragraphs.
fn write_docx(out: &mut dyn Write, method: CompressionMethod, length: u64) -> io::Result<()> {
    write_package(
        out,
        method,
        (kind::OFFICE_DOCUMENT, "word/document.xml"),
        vec![word_document(length)],
    )
}

#[test]
fn reads_a_word_document_too_large_to_read_whole_in_place() {
    let file = LargeFile::written("large.docx", |out| {
        write_docx(out, CompressionMethod::Stored, LARGE)
    });
    let (text, _) = file.extract(OOXML_PARSER);
    assert!(
        text.contains("Linje 0 af et meget langt dokument"),
        "{text}"
    );
    assert!(text.contains(END), "{text}");
}

#[test]
fn spools_a_deflated_word_document_part_past_the_memory_limit() {
    let file = LargeFile::written("deflated.docx", |out| {
        write_docx(out, CompressionMethod::Deflated, LARGE)
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, OOXML_PARSER);
    assert!(text.contains(END), "{text}");
}

#[test]
fn reads_a_worksheet_past_the_memory_limit_a_row_at_a_time() {
    const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const WORKSHEET: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
    let file = LargeFile::written("large.xlsx", |out| {
        let workbook = format!(
            "<workbook xmlns=\"{MAIN}\" xmlns:r=\"http://schemas.openxmlformats.org/\
             officeDocument/2006/relationships\"><sheets><sheet name=\"Ark1\" sheetId=\"1\" \
             r:id=\"rId1\"/></sheets></workbook>"
        );
        let sheet = Part::new(
            "xl/worksheets/sheet1.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
            Box::new(|out| {
                let row = |number: u64, text: &str| {
                    format!(
                        "<row r=\"{number}\"><c r=\"A{number}\" t=\"inlineStr\"><is><t>{text}\
                         </t></is></c><c r=\"B{number}\"><v>{number}</v></c></row>"
                    )
                };
                let mut rows = 0;
                repeated(
                    out,
                    &format!("<worksheet xmlns=\"{MAIN}\"><sheetData>"),
                    LARGE,
                    |index| {
                        rows = index + 1;
                        row(
                            rows,
                            &format!("Række {rows} i et meget stort regneark med tekst"),
                        )
                    },
                    "",
                )?;
                out.write_all(row(rows + 1, END).as_bytes())?;
                out.write_all(b"</sheetData></worksheet>")
            }),
        );
        write_package(
            out,
            CompressionMethod::Deflated,
            (kind::OFFICE_DOCUMENT, "xl/workbook.xml"),
            vec![
                Part::text(
                    "xl/workbook.xml",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
                    workbook,
                ),
                Part::relationships(
                    "xl/_rels/workbook.xml.rels",
                    &relationship("rId1", WORKSHEET, "worksheets/sheet1.xml"),
                ),
                sheet,
            ],
        )
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, OOXML_PARSER);
    assert!(text.contains("Række 1 i et meget stort regneark"), "{text}");
    assert!(text.contains(END), "{text}");
}

#[test]
fn reads_a_slide_past_the_memory_limit_as_it_writes_it() {
    const NAMESPACES: &str =
        "xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
    const SLIDE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
    let pml = |kind: &str| {
        format!("application/vnd.openxmlformats-officedocument.presentationml.{kind}+xml")
    };
    let (presentation_type, slide_type) = (pml("presentation.main"), pml("slide"));
    let file = LargeFile::written("large.pptx", |out| {
        let text = |text: &str| format!("<a:p><a:r><a:t>{text}</a:t></a:r></a:p>");
        let slide = Part::new(
            "ppt/slides/slide1.xml",
            &slide_type,
            Box::new(move |out| {
                repeated(
                    out,
                    &format!(
                        "<p:sld {NAMESPACES}><p:cSld><p:spTree><p:sp><p:nvSpPr>\
                         <p:cNvPr id=\"2\" name=\"Tekst\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
                         <p:txBody>"
                    ),
                    LARGE,
                    |index| text(&format!("Punkt {index} på et meget langt dias")),
                    &format!(
                        "{}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>",
                        text(END)
                    ),
                )
            }),
        );
        write_package(
            out,
            CompressionMethod::Deflated,
            (kind::OFFICE_DOCUMENT, "ppt/presentation.xml"),
            vec![
                Part::text(
                    "ppt/presentation.xml",
                    &presentation_type,
                    format!("<p:presentation {NAMESPACES}/>"),
                ),
                Part::relationships(
                    "ppt/_rels/presentation.xml.rels",
                    &relationship("rId1", SLIDE, "slides/slide1.xml"),
                ),
                slide,
            ],
        )
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, OOXML_PARSER);
    assert!(text.contains("Punkt 0 på et meget langt dias"), "{text}");
    assert!(text.contains(END), "{text}");
}

#[test]
fn reads_an_xps_document_too_large_to_read_whole_a_page_at_a_time() {
    const XPS: &str = "http://schemas.microsoft.com/xps/2005/06";
    const PAGE_TYPE: &str = "application/vnd.ms-package.xps-fixedpage+xml";
    const PAGE: u64 = 256 << 10;
    let pages = LARGE / PAGE + 1;
    let names: Vec<String> = (1..=pages)
        .map(|page| format!("Documents/1/Pages/{page}.fpage"))
        .collect();
    let file = LargeFile::written("large.xps", |out| {
        let references: String = (1..=pages)
            .map(|page| format!("<PageContent Source=\"Pages/{page}.fpage\"/>"))
            .collect();
        let mut parts = vec![
            Part::text(
                "FixedDocSeq.fdseq",
                "application/vnd.ms-package.xps-fixeddocumentsequence+xml",
                format!(
                    "<FixedDocumentSequence xmlns=\"{XPS}\"><DocumentReference \
                     Source=\"/Documents/1/FixedDocument.fdoc\"/></FixedDocumentSequence>"
                ),
            ),
            Part::text(
                "Documents/1/FixedDocument.fdoc",
                "application/vnd.ms-package.xps-fixeddocument+xml",
                format!("<FixedDocument xmlns=\"{XPS}\">{references}</FixedDocument>"),
            ),
        ];
        for (page, name) in (1..=pages).zip(&names) {
            let last = page == pages;
            parts.push(Part::new(
                name,
                PAGE_TYPE,
                Box::new(move |out| {
                    let glyphs = |text: &str, y: u64| {
                        format!(
                            "<Glyphs OriginX=\"10\" OriginY=\"{y}\" FontRenderingEmSize=\"12\" \
                             UnicodeString=\"{text}\"/>"
                        )
                    };
                    repeated(
                        out,
                        &format!("<FixedPage xmlns=\"{XPS}\" Width=\"816\" Height=\"1056\">"),
                        PAGE,
                        |line| glyphs(&format!("Side {page}, linje {line}"), line * 14 + 14),
                        &format!(
                            "{}</FixedPage>",
                            if last { glyphs(END, 1) } else { String::new() }
                        ),
                    )
                }),
            ));
        }
        write_package(
            out,
            CompressionMethod::Stored,
            (
                "http://schemas.microsoft.com/xps/2005/06/fixedrepresentation",
                "/FixedDocSeq.fdseq",
            ),
            parts,
        )
    });
    let (text, _) = file.extract(OOXML_PARSER);
    assert!(text.contains("Side 1, linje 0"), "{text}");
    assert!(text.contains(END), "{text}");
}

#[test]
fn decrypts_a_protected_package_too_large_to_read_whole_to_a_spool() {
    let file = LargeFile::written("protected.docx", |out| {
        let mut package = Vec::new();
        write_docx(&mut package, CompressionMethod::Stored, LARGE)?;
        let (info, stream) = encrypted_package(DEFAULT_PASSWORD, &package);
        drop(package);
        let document = |name: &str, content| (name.encode_utf16().collect(), Node::Owned(content));
        let compound = ole2::write(&Storage {
            class_id: [0; 16],
            entries: vec![
                document("EncryptionInfo", info),
                document("EncryptedPackage", stream),
            ],
        })
        .map_err(|e| io::Error::other(format!("{e:?}")))?;
        out.write_all(&compound)
    });
    let (text, metadata) = file.extract("extractous::OfficeParser");
    assert!(
        text.contains("Linje 0 af et meget langt dokument"),
        "{text}"
    );
    assert!(text.contains(END), "{text}");
    assert!(metadata["tk:parsed-by"]
        .iter()
        .any(|parser| parser == OOXML_PARSER));
}

#[test]
fn decodes_the_binary_parts_of_a_word_2006_document_too_large_to_read_whole() {
    const PACKAGE: &str = "http://schemas.microsoft.com/office/2006/xmlPackage";
    let file = LargeFile::written("large.xml", |out| {
        let base64 = "RXQgdmVkaMOmZnRldCBkb2t1bWVudA==";
        repeated(
            out,
            &format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
                 <?mso-application progid=\"Word.Document\"?>\n\
                 <pkg:package xmlns:pkg=\"{PACKAGE}\">\
                 <pkg:part pkg:name=\"/media/attached.txt\" pkg:contentType=\"text/plain\">\
                 <pkg:binaryData>{base64}</pkg:binaryData></pkg:part>\
                 <pkg:part pkg:name=\"/word/document.xml\" pkg:contentType=\"{DOCUMENT_TYPE}\">\
                 <pkg:xmlData><w:document {W}><w:body>"
            ),
            LARGE,
            |index| paragraph(&format!("Linje {index} af et meget langt dokument")),
            &format!(
                "{}</w:body></w:document></pkg:xmlData></pkg:part></pkg:package>",
                paragraph(END)
            ),
        )
    });
    let (text, _) = file.extract("extractous::Word2006MLParser");
    assert!(text.contains("Et vedhæftet dokument"), "{text}");
    assert!(text.contains(END), "{text}");
}
