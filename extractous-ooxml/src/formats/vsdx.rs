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
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
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

    /// The pages are checked one at a time, then written one at a time as they are read.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Leaves unread packages POI does not open as this port does and pages declaring a document
    /// type.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let package = Package::open(input, pages)?;
        let drawing = Drawing::open(package).map_err(|Unported(reason)| reason)?;
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
    fn open(package: Package<'a>) -> Result<Self, Unported> {
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
