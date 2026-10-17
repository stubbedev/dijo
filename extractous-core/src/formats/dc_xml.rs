//! XML documents without a parser of their own, as Tika's `DcXMLParser` reads them: all their
//! character data in one paragraph, a space before each element, and the Dublin Core elements
//! as metadata. The Dublin Core is read before the paragraph starts, so the head shows it:
//! Tika reads the metadata as it writes the paragraph, after the head (#105).

use crate::config::ParserConfigs;
use crate::formats::element_metadata::ElementHandlers;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};

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
pub(crate) struct DcXmlParser;

impl Parser for DcXmlParser {
    fn name(&self) -> &'static str {
        "extractous::DcXMLParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.xml.DcXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &["application/xml"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        Ok(Box::new(move |xhtml| {
            let mut metadata = DublinCoreReader {
                metadata: xhtml.metadata_mut(),
                dublin_core: ElementHandlers::dublin_core(),
            };
            // A malformed document fails the second reading, as far as which it is written.
            let _ = xml::parse_sax(input, "the XML document", &mut metadata);
            xhtml.start_element("p")?;
            let mut handler = Text { xhtml };
            let parsed = xml::parse_sax(input, "the XML document", &mut handler);
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

/// A native result without the Dublin Core Tika's path has not read: it reads the metadata
/// as it writes the text, so it stops at the length limit before what comes after, where
/// the native parser reads it all first (#105).
#[cfg(all(test, feature = "tika"))]
pub(crate) fn without_unread_dublin_core(
    _text: &mut String,
    metadata: &mut crate::formats::parity::ComparedMetadata,
    tika: &crate::formats::parity::ComparedMetadata,
) {
    let dublin_core = |name: &str| name.starts_with("dc:") || name == "dcterms:created";
    metadata.retain_mut(|(name, values)| {
        if !dublin_core(name) {
            return true;
        }
        let Some((_, read)) = tika.iter().find(|(tika_name, _)| tika_name == name) else {
            return false;
        };
        if values.starts_with(read) {
            values.truncate(read.len());
        }
        true
    });
}

/// A text without its head (#105): the head now shows the Dublin Core, which also titles the
/// document and reorders the other names, the metadata being compared on their own.
#[cfg(all(test, feature = "tika"))]
pub(crate) fn without_head(text: &mut String) {
    if let (Some(start), Some(end)) = (text.find("<head>"), text.find("</head>")) {
        text.replace_range(start..end + "</head>".len(), "");
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::metadata::TikaMetadata;
    use crate::formats::parity::{
        assert_same_as_tika_normalising, conformance_corpus, documents, Scratch,
    };

    fn assert_same_as_tika(documents: &[&Path], tika_name: &str) {
        assert_same_as_tika_normalising(
            documents,
            tika_name,
            without_unread_dublin_core,
            without_head,
        );
    }

    #[test]
    fn writes_dublin_core_in_the_head() {
        let (xml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(
                br#"<?xml version="1.0"?><r xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Titel</dc:title><p>foo<b>bar</b>baz</p>
  <q>indented</q></r>"#,
            )
            .unwrap();
        let head = &xml[..xml.find("<body>").unwrap()];
        assert!(
            head.contains(r#"<meta name="dc:title" content="Titel" />"#),
            "{xml}"
        );
        assert!(head.contains("<title>Titel</title>"), "{xml}");
    }

    /// The metadata `DcXMLParser` reads from `input`, for tests of the metadata alone.
    fn metadata_of(input: &[u8]) -> TikaMetadata {
        struct Metadata {
            metadata: TikaMetadata,
            dublin_core: ElementHandlers,
        }
        impl SaxHandler for Metadata {
            fn start_element(
                &mut self,
                namespace: &str,
                local: &str,
                _: &[Attribute],
            ) -> Result<(), Failure> {
                self.dublin_core.start(namespace, local);
                Ok(())
            }
            fn end_element(&mut self, namespace: &str, local: &str) -> Result<(), Failure> {
                self.dublin_core.end(namespace, local, &mut self.metadata);
                Ok(())
            }
            fn characters(&mut self, text: &str) -> Result<(), Failure> {
                self.dublin_core.characters(text);
                Ok(())
            }
        }
        let mut handler = Metadata {
            metadata: TikaMetadata::new(),
            dublin_core: ElementHandlers::dublin_core(),
        };
        xml::parse_sax(input, "test", &mut handler).unwrap();
        handler.metadata
    }

    const TIKA_NAME: &str = "org.apache.tika.parser.xml.DcXMLParser";
    /// A ZIP package detected by its name as 3MF XML, which both paths fail to read as XML.
    const NOT_XML: &str = "test3mf.3mf";

    #[test]
    fn reads_dublin_core_elements_and_bags() {
        let metadata = metadata_of(
            br#"<r xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                <dc:title> A title </dc:title><dc:title>A title</dc:title>
                <dc:subject><rdf:Bag><rdf:li>one</rdf:li><rdf:li>two</rdf:li></rdf:Bag></dc:subject>
                <dc:date>2024-01-02</dc:date><dc:date>2025-01-02</dc:date></r>"#,
        );
        assert_eq!(metadata.values("dc:title"), ["A title"]);
        assert_eq!(metadata.values("dc:subject"), ["one", "two"]);
        assert_eq!(metadata.values("dcterms:created"), ["2025-01-02"]);
    }

    #[test]
    fn extracts_xml_as_tikas_path_does() {
        let scratch = Scratch::new("dc-xml");
        let documents_written = [
            (
                "dublin-core.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>Caf&#233; notes</dc:title>
  <dc:creator>Ada</dc:creator><dc:creator>Charles</dc:creator>
  <dc:date>2024-03-05</dc:date>
  <body>Text <b>bold</b> &amp; <![CDATA[<raw>]]> more</body>
</metadata>"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "latin1.xml",
                b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r>caf\xe9 cr\xe8me</r>".to_vec(),
            ),
            (
                "broken.xml",
                b"<?xml version=\"1.0\"?><r><a>text before the error</r>".to_vec(),
            ),
        ];
        for (name, bytes) in &documents_written {
            scratch.write(name, bytes);
        }
        let (parsed, deferred) = documents(&DcXmlParser, &[scratch.directory()]);
        assert!(deferred.is_empty());
        assert_eq!(parsed.len(), documents_written.len());
        let paths: Vec<&Path> = parsed
            .iter()
            .map(PathBuf::as_path)
            .filter(|path| !path.ends_with("broken.xml"))
            .collect();
        assert_same_as_tika(&paths, TIKA_NAME);
        crate::formats::parity::assert_fails_as_tika(&[&scratch.directory().join("broken.xml")]);
    }

    #[test]
    fn extracts_the_corpora_xml_as_tikas_path_does() {
        let roots = [
            Path::new("../test_files/documents"),
            Path::new("../quality/corpus"),
        ];
        let (parsed, deferred) = documents(&DcXmlParser, &roots);
        assert!(deferred.is_empty());
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_xml_as_tikas_path_does() {
        let (parsed, deferred) = documents(&DcXmlParser, &[&conformance_corpus()]);
        eprintln!(
            "{TIKA_NAME}: {} parsed, deferred {deferred:?}",
            parsed.len()
        );
        assert!(deferred.is_empty());
        let (failing, read): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| path.ends_with(NOT_XML));
        assert_eq!(failing.len(), 1);
        assert_same_as_tika(&read, TIKA_NAME);
        crate::formats::parity::assert_fails_as_tika(&failing);
    }
}
