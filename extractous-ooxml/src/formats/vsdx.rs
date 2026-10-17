//! Visio drawings, stencils and templates in Office Open XML (VSDX, VSDM, VSSX, VSSM, VSTX,
//! VSTM), as Tika's `OOXMLParser` reads them with its `VSDXExtractorDecorator`: the package's
//! metadata and properties, then the text of each shape, trimmed, a paragraph each, in a `page`
//! div for each page, and the thumbnail.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor::{self, Ignore};
use crate::formats::ooxml::{self, Package, Properties, Unported};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;

/// `VISIO_DOCUMENT_REL`.
const DOCUMENT_RELATIONSHIP: &str =
    "http://schemas.microsoft.com/visio/2010/relationships/document";
/// `VISIO_PAGES_REL`.
const PAGES_RELATIONSHIP: &str = "http://schemas.microsoft.com/visio/2010/relationships/pages";
/// `VISIO_PAGE_REL`.
const PAGE_RELATIONSHIP: &str = "http://schemas.microsoft.com/visio/2010/relationships/page";
/// `VisioPageHandler.VISIO_NS`.
const VISIO_NAMESPACE: &str = "http://schemas.microsoft.com/office/visio/2012/main";

/// Tika's `OOXMLParser` of Visio packages.
pub struct VsdxParser;

impl Parser for VsdxParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.ms-visio.drawing",
            "application/vnd.ms-visio.drawing.macroenabled.12",
            "application/vnd.ms-visio.stencil",
            "application/vnd.ms-visio.stencil.macroenabled.12",
            "application/vnd.ms-visio.template",
            "application/vnd.ms-visio.template.macroenabled.12",
        ]
    }

    /// Defers packages POI does not open as this port does and pages declaring a document type.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let drawing = Drawing::open(input).map_err(|Unported(reason)| reason)?;
        Ok(Box::new(move |xhtml| drawing.write(xhtml)))
    }
}

/// A Visio package read as far as Tika reads it before it writes the drawing.
struct Drawing<'a> {
    package: Package<'a>,
    properties: Properties,
    pages: Vec<usize>,
}

impl<'a> Drawing<'a> {
    fn open(input: &'a [u8]) -> Result<Self, Unported> {
        let package = Package::open(input, crate::input::Pages::IN_MEMORY)?;
        let pages = page_parts(&package);
        for &page in &pages {
            ooxml::read_quietly(&package, page, Ignore)?;
        }
        let properties = Properties::read(&package)?;
        Ok(Self {
            package,
            properties,
            pages,
        })
    }

    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        extractor::write_package_metadata(&self.package, xhtml.metadata_mut());
        self.properties.write(xhtml.metadata_mut());
        for &page in &self.pages {
            xhtml.start_element_with("div", &[("class", "page")])?;
            let bytes = self.package.read(page)?;
            let mut handler = PageHandler {
                xhtml: &mut *xhtml,
                text: None,
            };
            xml::parse_part(&bytes, self.package.name(page), &mut handler)?;
            xhtml.end_element("div")?;
        }
        extractor::write_thumbnails(&self.package, xhtml)?;
        Ok(())
    }
}

/// `getPageParts`: the pages the pages part of the package's document lists.
fn page_parts(package: &Package) -> Vec<usize> {
    let Some(document) = Package::relationships_of(&package.relationships, DOCUMENT_RELATIONSHIP)
        .next()
        .and_then(|relationship| package.package_part(relationship))
    else {
        return Vec::new();
    };
    let relationships = &package.parts[document].relationships;
    let Some(pages) = Package::first_related(relationships, PAGES_RELATIONSHIP) else {
        return Vec::new();
    };
    Package::related(&package.parts[pages].relationships, PAGE_RELATIONSHIP)
}

/// `VisioPageHandler`: the text of each `Text` element, trimmed, as a paragraph unless empty.
struct PageHandler<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    /// The text of the `Text` element being read.
    text: Option<String>,
}

impl SaxHandler for PageHandler<'_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "Text" && namespace == VISIO_NAMESPACE {
            self.text = Some(String::new());
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name != "Text" || namespace != VISIO_NAMESPACE {
            return Ok(());
        }
        let text = self.text.take().unwrap_or_default();
        let text = java::trim(&text);
        if !text.is_empty() {
            self.xhtml.start_element("p")?;
            self.xhtml.characters(text)?;
            self.xhtml.end_element("p")?;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if let Some(collected) = &mut self.text {
            collected.push_str(text);
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::ooxml::testing::{package_with, relationship, relationships};
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    const RELATIONSHIPS: &str = "application/vnd.openxmlformats-package.relationships+xml";

    fn paths(files: &[PathBuf]) -> Vec<&Path> {
        files.iter().map(PathBuf::as_path).collect()
    }

    /// A drawing of `pages`, each the XML of a page part.
    fn drawing(pages: &[&str]) -> Vec<u8> {
        let document = format!(
            "<VisioDocument xmlns=\"{VISIO_NAMESPACE}\" \
             xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"/>"
        );
        let document_rels =
            relationships(&relationship("rId1", PAGES_RELATIONSHIP, "pages/pages.xml"));
        let pages_rels = relationships(
            &(1..=pages.len())
                .map(|page| {
                    relationship(
                        &format!("rId{page}"),
                        PAGE_RELATIONSHIP,
                        &format!("page{page}.xml"),
                    )
                })
                .collect::<String>(),
        );
        let names: Vec<String> = (1..=pages.len())
            .map(|page| format!("visio/pages/page{page}.xml"))
            .collect();
        let mut parts: Vec<(&str, &str, &[u8])> = vec![
            (
                "visio/document.xml",
                "application/vnd.ms-visio.drawing.main+xml",
                document.as_bytes(),
            ),
            (
                "visio/_rels/document.xml.rels",
                RELATIONSHIPS,
                document_rels.as_bytes(),
            ),
            (
                "visio/pages/pages.xml",
                "application/vnd.ms-visio.pages+xml",
                b"<Pages/>",
            ),
            (
                "visio/pages/_rels/pages.xml.rels",
                RELATIONSHIPS,
                pages_rels.as_bytes(),
            ),
        ];
        for (name, page) in names.iter().zip(pages) {
            parts.push((name, "application/vnd.ms-visio.page+xml", page.as_bytes()));
        }
        package_with(
            &relationship("rId1", DOCUMENT_RELATIONSHIP, "visio/document.xml"),
            &parts,
        )
    }

    fn page(texts: &[&str]) -> String {
        let shapes: String = texts
            .iter()
            .map(|text| format!("<Shape><Text>{text}</Text></Shape>"))
            .collect();
        format!(
            "<PageContents xmlns=\"{VISIO_NAMESPACE}\"><Shapes>{shapes}</Shapes></PageContents>"
        )
    }

    #[test]
    fn extracts_drawings_as_tikas_path_does() {
        let scratch = Scratch::new("vsdx");
        let first = page(&["  Pump  ", "Valve <cp IX=\"0\"/>A", "   "]);
        let second = page(&["Tank\n&amp; pipe"]);
        let read = [
            scratch.write("plant.vsdx", &drawing(&[&first, &second])),
            scratch.write("blank.vsdx", &drawing(&[])),
        ];
        let failing = [scratch.write("broken.vsdx", &drawing(&["<PageContents><Text>open"]))];
        let (parsed, deferred) = documents(&VsdxParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 3, "{parsed:?}");
        assert_same_as_tika(&paths(&read), VsdxParser.tika_name());
        assert_fails_as_tika(&paths(&failing));
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_drawings_as_tikas_path_does() {
        let (parsed, deferred) = documents(&VsdxParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 8, "{parsed:?}");
        assert_same_as_tika(&paths(&parsed), VsdxParser.tika_name());
    }
}
