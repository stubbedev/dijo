//! OpenDocument files, packaged and flat, and EPUB books too large to read whole, extracted under
//! a memory budget far below their size: a part or chapter whose declared size is past what a
//! spool holds in memory decoded straight to a file, and base64 binary data, whose size nothing
//! declares, decoded to a spool.

use std::io::{self, Write};

use extractous_core::large::{
    noise, package, write_base64, Binary, LargeFile, BUDGET, END, LARGE_PART, SPOOLING_BUDGET,
};
use extractous_core::{Extractor, Metadata};

use crate::input::MAX_READ_WHOLE;

const TITLE: &str = "A large document";
const OFFICE_NAMESPACES: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/""#;

/// Asserts that `file` is too large to read whole, that `text` reaches [`END`], and that
/// `metadata` has the title.
fn assert_read_to_its_end(file: &LargeFile, text: &str, metadata: &Metadata) {
    assert!(std::fs::metadata(file.path()).unwrap().len() > MAX_READ_WHOLE);
    assert!(text.contains(END), "{text}");
    assert_eq!(metadata["dc:title"], [TITLE]);
}

#[test]
fn reads_a_package_too_large_to_read_whole_spooling_its_content() {
    let file = LargeFile::written("large.odt", |out| {
        package(
            out,
            vec![
                (
                    "mimetype",
                    Box::new(|out| out.write_all(b"application/vnd.oasis.opendocument.text")),
                ),
                (
                    "META-INF/manifest.xml",
                    Box::new(|out| {
                        out.write_all(
                            br#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/></manifest:manifest>"#,
                        )
                    }),
                ),
                (
                    "meta.xml",
                    Box::new(|out| {
                        write!(
                            out,
                            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta {OFFICE_NAMESPACES}><office:meta><dc:title>{TITLE}</dc:title></office:meta></office:document-meta>"#
                        )
                    }),
                ),
                (
                    "content.xml",
                    Box::new(|out| {
                        write!(
                            out,
                            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {OFFICE_NAMESPACES}><office:body><office:text><text:tracked-changes><text:changed-region text:id="c"><text:deletion>"#
                        )?;
                        noise(out, "text:p", LARGE_PART)?;
                        write!(
                            out,
                            "</text:deletion></text:changed-region></text:tracked-changes>\
                             <text:p>{END}</text:p></office:text></office:body>\
                             </office:document-content>"
                        )
                    }),
                ),
            ],
        )
    });
    let (text, metadata) =
        file.extract_with(&Extractor::new(), BUDGET, "extractous::OpenDocumentParser");
    assert_read_to_its_end(&file, &text, &metadata);
}

#[test]
fn reads_a_flat_document_too_large_to_read_whole_spooling_its_binary_data() {
    let file = LargeFile::written("large.fodt", |out| {
        write!(
            out,
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document {OFFICE_NAMESPACES} office:version="1.2" office:mimetype="application/vnd.oasis.opendocument.text"><office:meta><dc:title>{TITLE}</dc:title></office:meta><office:body><office:text><text:p><draw:frame><draw:image><office:binary-data>"#
        )?;
        write_base64(out, Binary::new(MAX_READ_WHOLE + (16 << 20)), b"\n")?;
        write!(
            out,
            "</office:binary-data></draw:image></draw:frame></text:p><text:p>{END}</text:p>\
             </office:text></office:body></office:document>"
        )
    });
    let (text, metadata) = file.extract_with(
        &Extractor::new(),
        SPOOLING_BUDGET,
        "extractous::FlatOpenDocumentParser",
    );
    assert_read_to_its_end(&file, &text, &metadata);
}

#[test]
fn reads_a_book_too_large_to_read_whole_a_chapter_at_a_time() {
    let chapter = |out: &mut dyn Write, head: &dyn Fn(&mut dyn Write) -> io::Result<()>, body| {
        write!(
            out,
            r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title>"#
        )?;
        head(out)?;
        write!(out, "</head><body><p>{body}</p></body></html>")
    };
    let file = LargeFile::written("large.epub", |out| {
        package(
            out,
            vec![
                (
                    "mimetype",
                    Box::new(|out| out.write_all(b"application/epub+zip")),
                ),
                (
                    "META-INF/container.xml",
                    Box::new(|out| {
                        out.write_all(
                            br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
                        )
                    }),
                ),
                (
                    "OEBPS/content.opf",
                    Box::new(|out| {
                        write!(
                            out,
                            r#"<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{TITLE}</dc:title><dc:identifier id="id">large</dc:identifier></metadata><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="one"/><itemref idref="two"/></spine></package>"#
                        )
                    }),
                ),
                (
                    "OEBPS/one.xhtml",
                    Box::new(move |out| {
                        chapter(out, &|out| noise(out, "style", LARGE_PART), "one")
                    }),
                ),
                (
                    "OEBPS/two.xhtml",
                    Box::new(move |out| chapter(out, &|_| Ok(()), END)),
                ),
            ],
        )
    });
    let (text, metadata) = file.extract_with(&Extractor::new(), BUDGET, "extractous::EpubParser");
    assert_read_to_its_end(&file, &text, &metadata);
}
