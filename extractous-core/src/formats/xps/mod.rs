//! XPS documents, as Tika's `OOXMLParser` reads them with its `XPSExtractorDecorator`: the
//! package's signature and properties, then each fixed page of each document the document
//! sequence references, in a `page` div, its text laid out by Tika's `XPSPageContentHandler`,
//! then the images the pages are filled with, parsed as embedded documents, and the thumbnail.

mod page;

use crate::config::ParserConfigs;
use crate::formats::embedded::{ResourceType, Wrapping};
use crate::formats::metadata::office::ORIGINAL_RESOURCE_NAME;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor;
use crate::formats::ooxml::{Package, Properties, Unported};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Opened, Parser, Prepared};
use crate::input::{Input, Pages};

use page::{Block, Images, PageHandler};

/// `XPS_DOCUMENT`.
const XPS_DOCUMENT: &str = "http://schemas.microsoft.com/xps/2005/06/fixedrepresentation";
/// `OPEN_XPS_DOCUMENT`.
const OPEN_XPS_DOCUMENT: &str = "http://schemas.openxps.org/oxps/v1.0/fixedrepresentation";

/// Tika's `OOXMLParser` of XPS documents.
pub(crate) struct XpsParser;

impl Parser for XpsParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/vnd.ms-xpsdocument"]
    }

    /// Defers packages POI does not open as this port does.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let package = Package::open(input, Pages::IN_MEMORY).map_err(|Unported(reason)| reason)?;
        let properties = Properties::read(&package).map_err(|Unported(reason)| reason)?;
        let document = read_document(&package);
        Ok(Box::new(move |xhtml| {
            extractor::write_signature(&package, xhtml.metadata_mut());
            properties.write(xhtml.metadata_mut());
            write(&package, document, xhtml)?;
            extractor::write_thumbnails(&package, xhtml)?;
            Ok(())
        }))
    }
}

/// The document as `buildXHTML` reads it before writing anything of it: its pages, and the
/// failure that ends the parse after them, if one does.
struct Document {
    pages: Vec<Vec<Block>>,
    images: Images,
    failure: Option<Failure>,
}

/// Reads the pages of every document the package's document sequences reference, as far as
/// Tika's parse gets.
fn read_document(package: &Package) -> Document {
    let mut document = Document {
        pages: Vec::new(),
        images: Images::new(),
        failure: None,
    };
    if let Err(failure) = read_sequences(package, &mut document) {
        document.failure = Some(failure);
    }
    document
}

/// `buildXHTML`'s reading of the document sequences: for each relationship to one, the
/// package's first, as POI's `getPart` finds the part of a relationship.
fn read_sequences(package: &Package, document: &mut Document) -> Result<(), Failure> {
    let mut sequences: Vec<_> =
        Package::relationships_of(&package.relationships, XPS_DOCUMENT).collect();
    if sequences.is_empty() {
        sequences = Package::relationships_of(&package.relationships, OPEN_XPS_DOCUMENT).collect();
    }
    for relationship in sequences {
        let part = package.package_part(relationship).ok_or_else(|| {
            Failure::Malformed(format!(
                "NullPointerException: the document sequence {} is no part",
                relationship.target
            ))
        })?;
        let bytes = package.read(part)?;
        let mut references = SourcesOf::new("DocumentReference", false);
        xml::parse_part(&bytes, package.name(part), &mut references)?;
        for reference in references.sources {
            read_fixed_document(package, &reference, document)?;
        }
    }
    Ok(())
}

/// `handleDocumentRef`: the pages of the fixed document at `reference`.
fn read_fixed_document(
    package: &Package,
    reference: &str,
    document: &mut Document,
) -> Result<(), Failure> {
    let relative_root = match reference.rfind('/') {
        Some(at) if at > 0 => &reference[..at],
        _ => "",
    };
    let path = reference.strip_prefix('/').unwrap_or(reference);
    let bytes = zip_entry(package, path)
        .map_err(|_| Failure::Malformed(format!("IOException trying to read: {reference}")))?;
    let mut pages = SourcesOf::new("PageContent", true);
    xml::parse_part(&bytes, path, &mut pages)?;
    for source in pages.sources {
        let joined = if source.starts_with('/') {
            source
        } else {
            format!("{relative_root}/{source}")
        };
        let page_path = joined.strip_prefix('/').unwrap_or(&joined);
        let bytes = zip_entry(package, page_path)?;
        let mut handler = PageHandler::default();
        xml::parse_part(&bytes, page_path, &mut handler)?;
        let (blocks, images) = handler.finish()?;
        document.pages.push(blocks);
        page::merge_images(&mut document.images, images);
    }
    Ok(())
}

/// `getZipStream`: the archive's entry at `path`, a leading slash dropped from a longer path.
fn zip_entry<'a>(package: &Package<'a>, path: &str) -> Result<Input<'a>, Failure> {
    let target = if path.len() > 1 {
        path.strip_prefix('/').unwrap_or(path)
    } else {
        path
    };
    package
        .read_entry_named(target)
        .ok_or_else(|| Failure::Malformed(format!("Couldn't find required zip entry: {path}")))?
}

/// The `Source` attributes of the elements named `element`: each of them, or the first of each
/// element where `first_only`.
struct SourcesOf {
    element: &'static str,
    first_only: bool,
    sources: Vec<String>,
}

impl SourcesOf {
    fn new(element: &'static str, first_only: bool) -> Self {
        Self {
            element,
            first_only,
            sources: Vec::new(),
        }
    }
}

impl SaxHandler for SourcesOf {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name != self.element {
            return Ok(());
        }
        for attribute in attributes {
            if attribute.local_name == "Source" {
                self.sources.push(attribute.value.to_string());
                if self.first_only {
                    break;
                }
            }
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// Writes the pages read, then, unless reading them failed, parses the images.
fn write(package: &Package, document: Document, xhtml: &mut Xhtml) -> Result<(), Failure> {
    for blocks in document.pages {
        xhtml.start_element_with("div", &[("class", "page")])?;
        for block in blocks {
            write_block(block, xhtml)?;
        }
        xhtml.end_element("div")?;
    }
    if let Some(failure) = document.failure {
        return Err(failure);
    }
    for (source, image) in document.images {
        let mut metadata = TikaMetadata::new();
        if let Some(location) = image.original_location {
            metadata.set(ORIGINAL_RESOURCE_NAME, location);
        }
        ResourceType::Inline.set(&mut metadata);
        let Ok(data) = zip_entry(package, &source) else {
            continue;
        };
        xhtml.parse_embedded_opened(&data, &metadata, Wrapping::PackageEntry, Opened::Alone)?;
    }
    Ok(())
}

fn write_block(block: Block, xhtml: &mut Xhtml) -> Result<(), Failure> {
    match block {
        Block::Canvas(rows) => {
            xhtml.start_element_with("div", &[("class", "canvas")])?;
            for row in rows {
                xhtml.start_element("p")?;
                for (space, text) in row {
                    if space {
                        xhtml.ignorable_whitespace(" ")?;
                    }
                    if !text.is_empty() {
                        xhtml.characters_utf16(&text)?;
                    }
                }
                xhtml.end_element("p")?;
            }
            xhtml.end_element("div")?;
        }
        Block::Links(urls) => {
            xhtml.start_element_with("div", &[("class", "urls")])?;
            for url in urls {
                xhtml.start_element_with("a", &[("href", &url)])?;
                xhtml.characters(&url)?;
                xhtml.end_element("a")?;
            }
            xhtml.end_element("div")?;
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "tika"))]
mod tests;
