//! Flat OpenDocument files (FODT, FODS, FODP and the rest), one XML document each, read by the
//! rules of the archived format: the metadata of the outermost `office:meta`, the keywords
//! joining the subjects, with the document's version as `odf:version`, and the text of its
//! master pages, headers and footers among them, and of its body, in document order, styled by
//! its styles, through the handlers of the archived format. As in an archive, where they are
//! parts of their own, the body does not see the common styles. Settings and scripts are left
//! out, and macros are not extracted, as Tika's default leaves them. Base64 binary data, in the
//! styles as in the body, is parsed as embedded documents, as it is in an archive.
//!
//! Tika's `FlatOpenDocumentParser` reads `office:meta` and `office:body` alone, so styles, master
//! pages, headers and footers are lost, and routes them by local name in any namespace. It never
//! merges the keywords into the subjects, never sets `odf:version`, as its metadata handler's
//! root is `office:meta`, and on a malformed number ends the document without ending the
//! elements left open (#69, #91).

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::odf::body::{Body, DefinedStyles};
use crate::formats::odf::meta::{attribute_value, merge_subjects, MetaHandlers};
use crate::formats::odf::OFFICE_NS;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;

const WHAT: &str = "The flat OpenDocument XML";
const DOCUMENT: &str = "document";

/// The flat media types, by the media type a document's `office:mimetype` names.
const FLAT_TYPES: [(&str, &str); 3] = [
    (
        "application/vnd.oasis.opendocument.text",
        "application/vnd.oasis.opendocument.flat.text",
    ),
    (
        "application/vnd.oasis.opendocument.presentation",
        "application/vnd.oasis.opendocument.flat.presentation",
    ),
    (
        "application/vnd.oasis.opendocument.spreadsheet",
        "application/vnd.oasis.opendocument.flat.spreadsheet",
    ),
];

/// A child of the outermost `office:document`, by what is done with its events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Dropped: settings, scripts and anything unknown.
    Other,
    /// `office:meta`, read for metadata.
    Meta,
    /// `office:styles`, the common styles, which an archive has in `styles.xml`.
    CommonStyles,
    /// The font faces and automatic styles, which an archive has in `styles.xml` and
    /// `content.xml`.
    Styles,
    /// `office:master-styles`, the master pages, with their headers and footers, which an
    /// archive has in `styles.xml`.
    MasterStyles,
    /// `office:body`, which an archive has in `content.xml`.
    Body,
}

impl Section {
    fn of(namespace: &str, local_name: &str) -> Self {
        if namespace != OFFICE_NS {
            return Section::Other;
        }
        match local_name {
            "meta" => Section::Meta,
            "styles" => Section::CommonStyles,
            "font-face-decls" | "automatic-styles" => Section::Styles,
            "master-styles" => Section::MasterStyles,
            "body" => Section::Body,
            _ => Section::Other,
        }
    }

    /// Whether its character data goes to the body handler too. Style definitions hold no
    /// text, only the white space a flat document is indented with, which an archive's parts
    /// do not have.
    fn has_text(self) -> bool {
        matches!(self, Section::MasterStyles | Section::Body)
    }
}

/// What the start of an element is to the routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// The outermost `office:document`.
    Document,
    /// A child of it, starting a section.
    Section,
    /// Anything else.
    Within,
}

/// The routing of a flat document's events: each child of the outermost `office:document`, by
/// namespace and local name, and everything within it, to the handler of its section.
#[derive(Debug)]
struct Sections {
    depth: usize,
    is_document: bool,
    current: Section,
}

impl Sections {
    fn new() -> Self {
        Self {
            depth: 0,
            is_document: false,
            current: Section::Other,
        }
    }

    /// Notes the start of an element before its event is handed on.
    fn start(&mut self, namespace: &str, local_name: &str) -> Start {
        self.depth += 1;
        match self.depth {
            1 => {
                self.is_document = namespace == OFFICE_NS && local_name == DOCUMENT;
                if self.is_document {
                    Start::Document
                } else {
                    Start::Within
                }
            }
            2 if self.is_document => {
                self.current = Section::of(namespace, local_name);
                Start::Section
            }
            _ => Start::Within,
        }
    }

    /// Notes the end of an element after its event is handed on: the section it ends, if any.
    fn end(&mut self) -> Option<Section> {
        self.depth -= 1;
        if self.depth != 1 {
            return None;
        }
        Some(std::mem::replace(&mut self.current, Section::Other))
    }
}

/// The flat media type the outermost document's `office:mimetype` attribute names, if any.
fn flat_type(attributes: &[Attribute]) -> Option<&'static str> {
    let mimetype = attribute(attributes, OFFICE_NS, "mimetype")?;
    FLAT_TYPES
        .iter()
        .find(|(archived, _)| *archived == mimetype)
        .map(|(_, flat)| *flat)
}

/// The routing of a flat document with the body and metadata handlers it routes to.
struct Flat<'a, 'x> {
    sections: Sections,
    body: Body<'a, 'x>,
    meta: MetaHandlers,
    detected: Option<&'static str>,
    /// The styles defined before the common styles started.
    before_common_styles: DefinedStyles,
    /// The common styles, which the body is not to see.
    common_styles: Option<DefinedStyles>,
}

impl SaxHandler for Flat<'_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match self.sections.start(namespace, local_name) {
            Start::Document => {
                self.detected = flat_type(attributes);
                let metadata = self.body.metadata_mut();
                attribute_value(metadata, "odf:version", attributes, OFFICE_NS, "version");
            }
            Start::Section => match self.sections.current {
                Section::CommonStyles => self.before_common_styles = self.body.defined_styles(),
                Section::Body => {
                    if let Some(common) = self.common_styles.take() {
                        self.body.forget_styles(&common);
                    }
                }
                Section::Other | Section::Meta | Section::Styles | Section::MasterStyles => {}
            },
            Start::Within => {}
        }
        match self.sections.current {
            Section::Other => Ok(()),
            Section::Meta => {
                let metadata = self.body.metadata_mut();
                self.meta.start(metadata, namespace, local_name, attributes);
                Ok(())
            }
            _ => self.body.start_element(namespace, local_name, attributes),
        }
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        match self.sections.current {
            Section::Other => {}
            Section::Meta => self
                .meta
                .end(self.body.metadata_mut(), namespace, local_name),
            _ => self.body.end_element(namespace, local_name)?,
        }
        match self.sections.end() {
            Some(Section::Meta) => merge_subjects(self.body.metadata_mut()),
            Some(Section::CommonStyles) => {
                let before = std::mem::take(&mut self.before_common_styles);
                self.common_styles = Some(self.body.defined_styles().since(&before));
            }
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        match self.sections.current {
            Section::Meta => {
                self.meta.characters(text);
                Ok(())
            }
            section if section.has_text() || self.body.reads_binary_data() => {
                self.body.characters(text)
            }
            _ => Ok(()),
        }
    }
}

pub struct FlatOpenDocumentParser;

impl Parser for FlatOpenDocumentParser {
    fn name(&self) -> &'static str {
        "dijo::FlatOpenDocumentParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.odf.FlatOpenDocumentParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.oasis.opendocument.tika.flat.document",
            "application/vnd.oasis.opendocument.flat.text",
            "application/vnd.oasis.opendocument.flat.presentation",
            "application/vnd.oasis.opendocument.flat.spreadsheet",
        ]
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
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| write(input, pages, configs, xhtml)))
    }
}

/// Parses the document, releasing the pages of its input as it goes, and when the parse fails,
/// ends the elements left open and the document.
fn write(
    input: &[u8],
    pages: Pages,
    configs: &ParserConfigs,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let mut flat = Flat {
        sections: Sections::new(),
        body: Body::new(xhtml, configs.office.includes_deleted_content()),
        meta: MetaHandlers::new(),
        detected: None,
        before_common_styles: DefinedStyles::default(),
        common_styles: None,
    };
    let mut failure = match xml::parse_paged(input, pages, WHAT, &mut flat) {
        Ok(()) => {
            if let Some(detected) = flat.detected {
                flat.body.metadata_mut().set("Content-Type", detected);
            }
            return Ok(());
        }
        Err(failure) => failure,
    };
    if let Err(stop) = flat.body.drain_open_elements() {
        failure = stop.into();
    }
    drop(flat);
    if let Err(stop) = xhtml.end_document() {
        failure = stop.into();
    }
    Err(failure)
}

#[cfg(test)]
mod tests {
    use crate::formats::odf::tests::{document, MANIFEST};

    /// Documents whose master pages have text, headers and footers or placeholders, which
    /// Tika leaves out (#69), with text of each the native parser extracts.
    const WITH_MASTER_PAGES: [(&str, &[&str]); 2] = [
        (
            "../test_files/documents/table.fods",
            &["<p>Page </p>", "<td><p>1921</p>"],
        ),
        (
            "../test_files/documents/lighthouse.fodp",
            &["<p class=\"notes\"><div />", "<p>Fyret ved havet</p>"],
        ),
    ];
    const NAMESPACES: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0""#;
    const TEXT: &str = "application/vnd.oasis.opendocument.text";

    /// A flat document whose `office:document` names `mimetype` and holds `sections`.
    fn flat(mimetype: &str, sections: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document {NAMESPACES} office:version="1.3" office:mimetype="{mimetype}">{sections}</office:document>"#
        )
        .into_bytes()
    }

    /// The XHTML and metadata the native parser extracts from `input`.
    fn extract(input: &[u8]) -> (String, crate::Metadata) {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(input)
            .unwrap()
    }

    #[test]
    fn extracts_master_pages() {
        for (document, texts) in WITH_MASTER_PAGES {
            let (xml, _) = extract(&std::fs::read(document).unwrap());
            for text in texts {
                assert!(xml.contains(text), "{document}: {text} in {xml}");
            }
        }
    }

    /// A flat document reads as the same document archived does (#69): its styles, master
    /// pages, headers and footers too, the keywords joining the subjects, with its version; the
    /// body without the common styles, which an archive keeps in another part.
    #[test]
    fn reads_as_the_archived_document_does() {
        let meta = r#"<office:meta><dc:title>Flad</dc:title><dc:subject>Lys</dc:subject><meta:keyword>hav</meta:keyword><meta:keyword>sten</meta:keyword></office:meta>"#;
        let common = r#"<office:styles><style:style style:name="Strong" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style></office:styles>"#;
        let automatic = r#"<office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style></office:automatic-styles>"#;
        let master = r#"<office:master-styles><style:master-page style:name="Standard"><style:header><text:p>Header <text:span text:style-name="Strong">strong</text:span></text:p></style:header><style:footer><text:p>Footer</text:p></style:footer></style:master-page></office:master-styles>"#;
        let body = r#"<office:body><office:text><text:h text:outline-level="2">Fyret</text:h><text:p>Lys <text:span text:style-name="T1">hver</text:span> <text:span text:style-name="Strong">aften</text:span></text:p></office:text></office:body>"#;
        let settings = r#"<office:settings><config:config-item-set config:name="ooo:view-settings" xmlns:config="urn:oasis:names:tc:opendocument:xmlns:config:1.0"><config:config-item config:name="x" config:type="int">1</config:config-item></config:config-item-set></office:settings>"#;
        let (flat_xml, flat_metadata) = extract(&flat(
            TEXT,
            &format!("{meta}{settings}{common}{automatic}{master}{body}"),
        ));
        let part = |root: &str, children: &str| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><office:{root} {NAMESPACES} office:version="1.3">{children}</office:{root}>"#
            )
        };
        let (archived_xml, archived_metadata) = extract(&document(
            TEXT,
            &[
                ("meta.xml", &part("document-meta", meta)),
                (
                    "styles.xml",
                    &part("document-styles", &format!("{common}{master}")),
                ),
                (
                    "content.xml",
                    &part("document-content", &format!("{automatic}{body}")),
                ),
                MANIFEST,
            ],
        ));
        let body = |xml: &str| xml[xml.find("<body>").unwrap()..].to_owned();
        assert_eq!(body(&flat_xml), body(&archived_xml));
        assert!(
            flat_xml.contains(
                "<p>Header <b>strong</b></p>\n<p>Footer</p>\n<h2>Fyret</h2>\n\
                 <p>Lys <b>hver</b> aften</p>"
            ),
            "{flat_xml}"
        );
        assert_eq!(flat_metadata["dc:subject"], ["hav", "sten", "Lys"]);
        assert_eq!(flat_metadata["dc:subject"], archived_metadata["dc:subject"]);
        assert_eq!(flat_metadata["odf:version"], ["1.3"]);
        assert_eq!(
            flat_metadata["Content-Type"],
            ["application/vnd.oasis.opendocument.flat.text"]
        );
    }

    /// Only the OpenDocument office namespace routes a section, where Tika takes any
    /// element of the local name (#91).
    #[test]
    fn routes_sections_by_namespace() {
        let (xml, metadata) = extract(&flat(
            TEXT,
            r#"<x:meta xmlns:x="urn:x"><dc:title>Not meta</dc:title></x:meta><x:body xmlns:x="urn:x"><text:p>Not body</text:p></x:body><office:body><office:text><text:p>Body</text:p></office:text></office:body>"#,
        ));
        assert!(!xml.contains("Not body"), "{xml}");
        assert!(xml.contains("<p>Body</p>"), "{xml}");
        assert!(!metadata.contains_key("dc:title"));
    }

    /// A malformed font weight or outline level, on which Tika fails without ending the
    /// open elements, is ignored (#69, #90).
    #[test]
    fn ignores_malformed_style_values() {
        let (xml, _) = extract(&flat(
            TEXT,
            r#"<office:body><office:text><text:p>before</text:p><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="6x"/></style:style><text:h text:outline-level="x">h <text:span text:style-name="T1">t</text:span></text:h></office:text></office:body>"#,
        ));
        assert!(xml.contains("<p>before</p>\n<h1>h t</h1>\n"), "{xml}");
    }

    /// Binary data in the styles, which Tika never reads, is parsed as in an archive,
    /// where the styles are a part the body handler reads; in the settings it is left out.
    #[test]
    fn extracts_binary_data_in_the_styles_and_not_the_settings() {
        let text = "SGVqIHZlcmRlbiE=";
        let (styles, _) = extract(&flat(
            TEXT,
            &format!(
                "<office:styles><style:style style:name=\"S\"><office:binary-data>{text}\
                 </office:binary-data></style:style></office:styles><office:body><office:text>\
                 <text:p>x</text:p></office:text></office:body>"
            ),
        ));
        assert!(styles.contains("Hej verden!"), "{styles}");
        let (settings, _) = extract(&flat(
            TEXT,
            &format!(
                "<office:settings><office:binary-data>{text}</office:binary-data>\
                 </office:settings><office:body><office:text><text:p>x</text:p></office:text>\
                 </office:body>"
            ),
        ));
        assert!(!settings.contains("Hej verden!"), "{settings}");
    }
}
