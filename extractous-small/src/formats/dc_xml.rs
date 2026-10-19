//! XML documents without a parser of their own, as Tika's `DcXMLParser` reads them: all their
//! character data in one paragraph, a space before each element, and the Dublin Core elements
//! as metadata. The Dublin Core is read before the paragraph starts, so the head shows it:
//! Tika reads the metadata as it writes the paragraph, after the head (#105).

use crate::config::ParserConfigs;
use crate::formats::element_metadata::ElementHandlers;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;

/// The XML types Tika's registry hands `DcXMLParser`, having no parser of their own.
const MEDIA_TYPES: [&str; 8] = [
    "application/xml",
    "application/rdf+xml",
    "application/onix-message+xml",
    "application/ttml+xml",
    "application/vnd.adobe.xdp+xml",
    "application/vnd.google-earth.kml+xml",
    "application/vnd.ms-package.3dmanufacturing-3dmodel+xml",
    "text/iso19139+xml",
];

/// Tika's `DcXMLParser`.
pub struct DcXmlParser;

impl Parser for DcXmlParser {
    fn name(&self) -> &'static str {
        "extractous::DcXMLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.xml.DcXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &["application/xml"]
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
        Ok(Box::new(move |xhtml| {
            let mut metadata = DublinCoreReader {
                metadata: xhtml.metadata_mut(),
                dublin_core: ElementHandlers::dublin_core(),
            };
            // A malformed document fails the second reading, as far as which it is written.
            let _ = xml::parse_paged(input, pages, "the XML document", &mut metadata);
            xhtml.start_element("p")?;
            let mut handler = Text { xhtml };
            let parsed = xml::parse_paged(input, pages, "the XML document", &mut handler);
            xhtml.end_element("p")?;
            xhtml.end_document()?;
            parsed.map_err(|failure| match failure {
                Failure::Malformed(reason) => {
                    Failure::Malformed(format!("XML parse error: {reason}"))
                }
                other => other,
            })
        }))
    }
}

/// The Dublin Core handlers `DcXMLParser` tees the events to.
struct DublinCoreReader<'m> {
    metadata: &'m mut TikaMetadata,
    dublin_core: ElementHandlers,
}

impl SaxHandler for DublinCoreReader<'_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.dublin_core.start(namespace, local_name);
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.dublin_core.end(namespace, local_name, self.metadata);
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.dublin_core.characters(text);
        Ok(())
    }
}

/// `TextContentHandler`, with a space before each element.
struct Text<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
}

impl SaxHandler for Text<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        _local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.xhtml.characters(" ")?;
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.xhtml.characters(text)?;
        Ok(())
    }
}

#[cfg(test)]
mod large {
    use extractous_core::large::{bodies_past_limit, LargeFile};

    use super::*;

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// A UTF-16 document too large to read whole is decoded a window at a time and its text
    /// written as it is read, in either build, under a memory budget far below its size.
    #[test]
    fn writes_utf16_documents_too_large_to_read_whole_as_they_are_read() {
        let row = |index: u64| utf16(&format!("<r>R\u{e6}kke {index}</r>\n"));
        let count = bodies_past_limit(row(0).len());
        let file = LargeFile::new(
            "long.xml",
            &utf16("\u{feff}<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<doc>\n"),
            count,
            row,
            &utf16("</doc>\n"),
        );
        let mut rows = 0;
        file.extract_lines(DcXmlParser.name(), |line| {
            let line = line.trim();
            if !line.is_empty() {
                assert_eq!(line, format!("R\u{e6}kke {rows}"));
                rows += 1;
            }
        });
        assert_eq!(rows, count);
    }
}
