//! InDesign Markup Language packages (IDML), as Tika's `IDMLParser` reads them: the package's
//! `mimetype` as its content type, the XMP of `META-INF/metadata.xml`, and the parts of its
//! master spreads, spreads and stories, read in the archive's order by its
//! `ContentAndMetadataExtractor`, each `Content` element a paragraph and the spreads' page counts
//! summed. Each part is spooled as it is read, ahead of the writing to tell whether the port
//! reads it, and again as it is written, one at a time.
//!
//! `IDMLParser` keeps its page counts in fields of its one instance and never resets them, so
//! Tika's path adds each document's counts to those of the documents it parsed before. The native
//! path counts each document's own pages.

use crate::config::ParserConfigs;
use crate::formats::jdk_zip::{self, Archive, Checked, Refusal};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{xmp, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
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

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let archive = Archive::open(input, pages)?;
        let entries = entries(&archive)?;
        Ok(Box::new(move |xhtml| write(&archive, entries, xhtml)))
    }
}

/// What `handleZipEntry` does with an entry.
enum Entry {
    /// The `mimetype`, as text.
    MediaType(String),
    /// The package's XMP, read ahead.
    Metadata(Checked),
    /// A master spread, a spread or a story, read ahead for its content and page count.
    Part { kind: Kind, xml: Checked },
}

/// Which page count a part's spreads add to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    MasterSpread,
    Spread,
    Story,
}

/// The entries `handleZipFile` hands `handleZipEntry`, `META-INF/metadata.xml` first, read
/// ahead; a package Tika's path reads otherwise left to it.
fn entries(archive: &Archive<'_>) -> Result<Vec<Entry>, Refusal> {
    let zip = archive.zip();
    let metadata = jdk_zip::entry(zip, METADATA)?;
    let mut entries = Vec::new();
    for entry in metadata
        .into_iter()
        .chain(zip.entries().iter().filter(|entry| entry.name != METADATA))
    {
        let name = entry.name.as_str();
        let part = |kind| -> Result<Entry, Refusal> {
            let xml = archive.spool(entry)?;
            if xml::has_document_type(&xml) {
                return Err(Refusal::Unread(
                    "a part declaring a document type, under which Xerces may report \
                     whitespace as ignorable, which the content handler drops",
                ));
            }
            Ok(Entry::Part {
                kind,
                xml: Archive::keep(entry, xml),
            })
        };
        entries.push(if name == "mimetype" {
            Entry::MediaType(java::utf8(&archive.spool(entry)?))
        } else if name == METADATA {
            let xml = archive.spool(entry)?;
            xmp::extract(&xml, &mut TikaMetadata::new(), xmp::as_is)
                .map_err(|_| Refusal::Unread("XMP Tika fails to map, whose failure it records"))?;
            Entry::Metadata(Archive::keep(entry, xml))
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

/// `IDMLParser.parse` once the archive is open, each entry of `archive` read again as it is
/// written.
fn write(archive: &Archive<'_>, entries: Vec<Entry>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let (mut pages, mut master_pages) = (0i32, 0i32);
    for entry in entries {
        match entry {
            Entry::MediaType(media_type) => {
                xhtml
                    .metadata_mut()
                    .set("Content-Type", media_type.as_str());
            }
            Entry::Metadata(xml) => {
                xmp::extract(&archive.take(xml)?, xhtml.metadata_mut(), xmp::as_is)
                    .map_err(|unported| Failure::Malformed(unported.0))?;
            }
            Entry::Part { kind, xml } => {
                let mut handler = ContentAndMetadata {
                    xhtml: &mut *xhtml,
                    in_content: false,
                    spread: false,
                    pages: 0,
                };
                xml::parse_part(&archive.take(xml)?, "the IDML part", &mut handler)?;
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

#[cfg(test)]
mod large_tests {
    use extractous_core::large::{LargeFile, SPOOLING_BUDGET};

    use crate::formats::large::{bytes, noise, package, END, LARGE};

    const NAMESPACE: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";

    /// A package whose first story decodes past what a spool holds in memory, with noise outside
    /// its `Content` elements, which is not written, and whose second story ends the document:
    /// each spools as it is read, to a file past the memory, under a budget far below them.
    #[test]
    fn reads_a_package_too_large_to_read_whole_a_part_at_a_time() {
        let last = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><idPkg:Story xmlns:idPkg="{NAMESPACE}"><Story><Content>{END}</Content></Story></idPkg:Story>"#
        );
        let file = LargeFile::written("large.idml", |out| {
            package(
                out,
                vec![
                    (
                        "mimetype",
                        bytes(b"application/vnd.adobe.indesign-idml-package"),
                    ),
                    (
                        "Stories/Story_u1.xml",
                        Box::new(|out| {
                            write!(
                                out,
                                r#"<?xml version="1.0" encoding="UTF-8"?><idPkg:Story xmlns:idPkg="{NAMESPACE}"><Story>"#
                            )?;
                            noise(out, "Noise", LARGE)?;
                            out.write_all(b"</Story></idPkg:Story>")
                        }),
                    ),
                    ("Stories/Story_u2.xml", bytes(last.as_bytes())),
                ],
            )
        });
        let (text, metadata) = file.extract_with(
            &crate::Extractor::new(),
            SPOOLING_BUDGET,
            "extractous::IDMLParser",
        );
        assert!(text.contains(END), "{text}");
        assert_eq!(metadata[super::PAGE_COUNT], ["0"]);
    }
}
