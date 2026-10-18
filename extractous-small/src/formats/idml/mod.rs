//! InDesign Markup Language packages (IDML), as Tika's `IDMLParser` reads them: the package's
//! `mimetype` as its content type, the XMP of `META-INF/metadata.xml`, and the parts of its
//! master spreads, spreads and stories, read in the archive's order by its
//! `ContentAndMetadataExtractor`, each `Content` element a paragraph and the spreads' page counts
//! summed.
//!
//! `IDMLParser` keeps its page counts in fields of its one instance and never resets them, so
//! Tika's path adds each document's counts to those of the documents it parsed before. The native
//! path counts each document's own pages.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::jdk_zip::{self, Refusal};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{xmp, Decline, Failure, Parser, Prepared};
use crate::java;

/// `META_NAME`, the package's XMP, which Tika reads before the other entries.
const METADATA: &str = "META-INF/metadata.xml";
pub(crate) const SPREAD_PAGE_COUNT: &str = "idml:spread-page-count";
pub(crate) const MASTER_SPREAD_PAGE_COUNT: &str = "idml:master-spread-page-count";
pub(crate) const PAGE_COUNT: &str = "meta:page-count";

/// Tika's `IDMLParser`.
pub struct IdmlParser;

impl Parser for IdmlParser {
    fn name(&self) -> &'static str {
        "extractous::IDMLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.indesign.IDMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/vnd.adobe.indesign-idml-package"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let entries = entries(input).map_err(|refusal| match refusal {
            Refusal::Unread(reason) => Decline::Deferred(reason.to_owned()),
            Refusal::Failed(reason) => Decline::Fails(Error::ParseError(reason)),
        })?;
        Ok(Box::new(move |xhtml| write(&entries, xhtml)))
    }
}

/// What `handleZipEntry` does with an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    /// The `mimetype`, as text.
    MediaType(String),
    /// The package's XMP.
    Metadata(Vec<u8>),
    /// A master spread, a spread or a story, read for its content and page count.
    Part { kind: Kind, xml: Vec<u8> },
}

/// Which page count a part's spreads add to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    MasterSpread,
    Spread,
    Story,
}

/// The entries `handleZipFile` hands `handleZipEntry`, `META-INF/metadata.xml` first, read; a
/// package Tika's path reads otherwise left to it.
fn entries(input: &[u8]) -> Result<Vec<Entry>, Refusal> {
    let zip = jdk_zip::open(input)?;
    let metadata = jdk_zip::entry(&zip, METADATA)?;
    let mut entries = Vec::new();
    for entry in metadata
        .into_iter()
        .chain(zip.entries().iter().filter(|entry| entry.name != METADATA))
    {
        let name = entry.name.as_str();
        let read = || jdk_zip::read_whole(&zip, entry);
        let part = |kind| -> Result<Entry, Refusal> {
            let xml = read()?;
            if xml::has_document_type(&xml) {
                return Err(Refusal::Unread(
                    "a part declaring a document type, under which Xerces may report \
                     whitespace as ignorable, which the content handler drops",
                ));
            }
            Ok(Entry::Part { kind, xml })
        };
        entries.push(if name == "mimetype" {
            Entry::MediaType(java::utf8(&read()?))
        } else if name == METADATA {
            let xml = read()?;
            xmp::extract(&xml, &mut TikaMetadata::new(), xmp::as_is)
                .map_err(|_| Refusal::Unread("XMP Tika fails to map, whose failure it records"))?;
            Entry::Metadata(xml)
        } else if name.contains("MasterSpreads") {
            part(Kind::MasterSpread)?
        } else if name.contains("Spreads/Spread") {
            part(Kind::Spread)?
        } else if name.contains("Stories") {
            part(Kind::Story)?
        } else {
            continue;
        });
    }
    Ok(entries)
}

/// `IDMLParser.parse` once the archive is open.
fn write(entries: &[Entry], xhtml: &mut Xhtml) -> Result<(), Failure> {
    let (mut pages, mut master_pages) = (0i32, 0i32);
    for entry in entries {
        match entry {
            Entry::MediaType(media_type) => {
                xhtml
                    .metadata_mut()
                    .set("Content-Type", media_type.as_str());
            }
            Entry::Metadata(xml) => {
                xmp::extract(xml, xhtml.metadata_mut(), xmp::as_is)
                    .map_err(|unported| Failure::Malformed(unported.0))?;
            }
            Entry::Part { kind, xml } => {
                let mut handler = ContentAndMetadata {
                    xhtml: &mut *xhtml,
                    in_content: false,
                    spread: false,
                    pages: 0,
                };
                xml::parse_sax(xml, "the IDML part", &mut handler)?;
                let spread = handler.spread.then_some(handler.pages);
                let count = || {
                    spread.ok_or_else(|| {
                        Failure::Malformed(
                            "NumberFormatException: the spread names no page count".to_owned(),
                        )
                    })
                };
                match kind {
                    Kind::MasterSpread => master_pages = master_pages.wrapping_add(count()?),
                    Kind::Spread => pages = pages.wrapping_add(count()?),
                    Kind::Story => {}
                }
            }
        }
    }
    let metadata = xhtml.metadata_mut();
    metadata.set(SPREAD_PAGE_COUNT, pages.to_string());
    metadata.set(MASTER_SPREAD_PAGE_COUNT, master_pages.to_string());
    metadata.set(PAGE_COUNT, pages.wrapping_add(master_pages).to_string());
    Ok(())
}

/// `ContentAndMetadataHandler`: each `Content` element a paragraph of its text, and the pages
/// of the spreads summed.
struct ContentAndMetadata<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    in_content: bool,
    spread: bool,
    pages: i32,
}

impl SaxHandler for ContentAndMetadata<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if matches!(local_name, "Spread" | "MasterSpread") {
            self.spread = true;
            if let Some(count) = attribute_named(attributes, "PageCount")
                .and_then(|value| java::parse_int(java::trim(value)))
            {
                self.pages = self.pages.wrapping_add(count);
            }
        }
        if local_name == "Content" {
            self.in_content = true;
            self.xhtml.start_element("p")?;
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name == "Content" {
            self.in_content = false;
            self.xhtml.end_element("p")?;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_content {
            self.xhtml.characters_chunk(text)?;
        }
        Ok(())
    }
}

/// The page counts `IDMLParser` keeps adding to across the documents Tika's path parses, where
/// the native parser counts each document's own (#124).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const ACCUMULATED_PAGE_COUNTS: crate::formats::parity::Departures = &[
    crate::formats::parity::Departure::Values(SPREAD_PAGE_COUNT, "#124"),
    crate::formats::parity::Departure::Values(MASTER_SPREAD_PAGE_COUNT, "#124"),
    crate::formats::parity::Departure::Values(PAGE_COUNT, "#124"),
];

#[cfg(all(test, feature = "tika"))]
mod tests;
