//! Flat OpenDocument files (FODT, FODS, FODP and the rest), one XML document each, as Tika's
//! `FlatOpenDocumentParser` reads them: the metadata of the outermost `office:meta` and the text
//! of the outermost `office:body`, through the handlers of the archived format. Everything else,
//! styles and master pages among it, is left out, and macros are not extracted, as Tika's
//! default leaves them. Base64 binary data in the body is parsed as an embedded document.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::odf::body::{Body, Namespaces};
use crate::formats::odf::meta::MetaHandlers;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};

const WHAT: &str = "The flat OpenDocument XML";
const DOCUMENT: &str = "document";
const META: &str = "meta";
const BODY: &str = "body";

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

/// Which of the handlers of Tika's `FlatOpenDocumentParserHandler` takes an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Its `DefaultHandler`, which drops the event.
    Other,
    Meta,
    Body,
}

/// The routing of Tika's `FlatOpenDocumentParserHandler`: the outermost document's `meta` and
/// `body`, by local name, go to their handlers, nested ones to the handler already taking them.
#[derive(Debug)]
struct Sections {
    document_depth: usize,
    section_depth: usize,
    current: Section,
}

impl Sections {
    fn new() -> Self {
        Self {
            document_depth: 0,
            section_depth: 0,
            current: Section::Other,
        }
    }

    /// Notes the start of `local_name` before its event is handed on, and whether it starts the
    /// outermost document.
    fn start(&mut self, local_name: &str) -> bool {
        let is_section = matches!(local_name, META | BODY);
        if local_name == DOCUMENT {
            self.document_depth += 1;
            return self.document_depth == 1;
        }
        if self.document_depth == 1 && self.section_depth == 0 {
            let section = match local_name {
                META => Section::Meta,
                BODY => Section::Body,
                _ => return false,
            };
            self.current = section;
            self.section_depth = 1;
        } else if self.section_depth > 0 && is_section {
            self.section_depth += 1;
        }
        false
    }

    /// Notes the end of `local_name` after its event is handed on.
    fn end(&mut self, local_name: &str) {
        if local_name == DOCUMENT {
            self.document_depth -= 1;
        } else if self.section_depth > 0 && matches!(local_name, META | BODY) {
            self.section_depth -= 1;
            if self.section_depth == 0 {
                self.current = Section::Other;
            }
        }
    }
}

/// The flat media type the outermost document's `mimetype` attribute names, if any.
fn flat_type(attributes: &[Attribute]) -> Option<&'static str> {
    let mimetype = attributes
        .iter()
        .find(|attribute| attribute.local_name == "mimetype")?;
    FLAT_TYPES
        .iter()
        .find(|(archived, _)| *archived == mimetype.value)
        .map(|(_, flat)| *flat)
}

/// Tika's `FlatOpenDocumentParserHandler` with the body and metadata handlers it routes to.
struct Flat<'a, 'x> {
    sections: Sections,
    body: Body<'a, 'x>,
    meta: MetaHandlers,
    detected: Option<&'static str>,
}

impl SaxHandler for Flat<'_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if self.sections.start(local_name) {
            if let Some(detected) = flat_type(attributes) {
                self.detected = Some(detected);
            }
        }
        match self.sections.current {
            Section::Other => Ok(()),
            Section::Meta => {
                let metadata = self.body.metadata_mut();
                self.meta.start(metadata, namespace, local_name, attributes);
                Ok(())
            }
            Section::Body => self.body.start_element(namespace, local_name, attributes),
        }
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        match self.sections.current {
            Section::Other => {}
            Section::Meta => self
                .meta
                .end(self.body.metadata_mut(), namespace, local_name),
            Section::Body => self.body.end_element(namespace, local_name)?,
        }
        self.sections.end(local_name);
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        match self.sections.current {
            Section::Other => Ok(()),
            Section::Meta => {
                self.meta.characters(text);
                Ok(())
            }
            Section::Body => self.body.characters(text),
        }
    }
}

pub(crate) struct FlatOpenDocumentParser;

impl Parser for FlatOpenDocumentParser {
    fn name(&self) -> &'static str {
        "extractous::FlatOpenDocumentParser"
    }

    #[cfg(test)]
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

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        Ok(Box::new(move |xhtml| write(input, configs, xhtml)))
    }
}

/// Parses as `FlatOpenDocumentParser.parse`, which ends the document even when the parse
/// fails, after ending the elements left open, unless an unchecked exception skips that.
fn write(input: &[u8], configs: &ParserConfigs, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut flat = Flat {
        sections: Sections::new(),
        body: Body::new(
            xhtml,
            configs.office.includes_deleted_content(),
            Namespaces::AsWritten,
        ),
        meta: MetaHandlers::new(),
        detected: None,
    };
    let outcome = xml::parse_sax(input, WHAT, &mut flat);
    let mut failure = match outcome {
        Ok(()) => {
            if let Some(detected) = flat.detected {
                flat.body.metadata_mut().set("Content-Type", detected);
            }
            return Ok(());
        }
        Err(failure) => failure,
    };
    if !flat.body.failed_unchecked() {
        if let Err(stop) = flat.body.drain_open_elements() {
            failure = stop.into();
        }
    }
    drop(flat);
    if let Err(stop) = xhtml.end_document() {
        failure = stop.into();
    }
    Err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };
    use std::path::{Path, PathBuf};

    const TIKA_NAME: &str = "org.apache.tika.parser.odf.FlatOpenDocumentParser";
    const DOCUMENTS: [&str; 5] = [
        "../test_files/documents/notes.fodt",
        "../test_files/documents/revisions.fodt",
        "../test_files/documents/table.fods",
        "../test_files/documents/lighthouse.fodp",
        "../test_files/documents/lighthouse.fodg",
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

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let documents: Vec<&Path> = DOCUMENTS.iter().map(Path::new).collect();
        assert_same_as_tika(&documents, TIKA_NAME);
    }

    #[test]
    fn extracts_only_the_outermost_meta_and_body_as_tikas_path_does() {
        let scratch = Scratch::new("flat-sections");
        let meta = r#"<office:meta><meta:generator>Test</meta:generator><dc:title>Flad</dc:title><dc:subject>Lys</dc:subject><meta:keyword>hav</meta:keyword><meta:document-statistic meta:page-count="1"/></office:meta>"#;
        let styles = r#"<office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style></office:automatic-styles><office:master-styles><style:master-page style:name="Standard"><style:header><text:p>Header</text:p></style:header></style:master-page></office:master-styles>"#;
        let body = r#"<office:body><office:text><text:h text:outline-level="2">Fyret</text:h><text:p>Lys <text:span text:style-name="T1">hver</text:span> aften<text:note><text:note-body><text:p>Note</text:p></text:note-body></text:note> efter.</text:p><text:p><draw:frame><draw:object><office:document office:mimetype="application/vnd.oasis.opendocument.chart"><office:meta><dc:title>Inner</dc:title></office:meta><office:body><office:chart><text:p>Inner body</text:p></office:chart></office:body></office:document></draw:object></draw:frame>after object</text:p></office:text></office:body>"#;
        let documents = [
            scratch.write(
                "sections.fodt",
                &flat(TEXT, &format!("{meta}{styles}{body}")),
            ),
            scratch.write(
                "drawing.fodg",
                &flat(
                    "application/vnd.oasis.opendocument.graphics",
                    &format!("{meta}{body}"),
                ),
            ),
            scratch.write(
                "styles-in-body.fodt",
                &flat(
                    TEXT,
                    r#"<office:body><office:text><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style><text:p>x <text:span text:style-name="T1">bold</text:span></text:p></office:text></office:body>"#,
                ),
            ),
            scratch.write(
                "binary-data-in-styles.fodt",
                &flat(
                    TEXT,
                    r#"<office:styles><draw:fill-image draw:name="i"><office:binary-data>AAAA</office:binary-data></draw:fill-image></office:styles><office:body><office:text><text:p>x</text:p></office:text></office:body>"#,
                ),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    fn fails_where_tikas_path_fails() {
        let scratch = Scratch::new("flat-failures");
        let documents = [
            scratch.write(
                "unclosed.fodt",
                &flat(
                    TEXT,
                    "<office:body><office:text><text:p>open <text:span>span</office:text></office:body>",
                ),
            ),
            scratch.write(
                "font-weight.fodt",
                &flat(
                    TEXT,
                    r#"<office:body><office:text><text:p>before</text:p><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="6x"/></style:style></office:text></office:body>"#,
                ),
            ),
            scratch.write(
                "outline-level.fodt",
                &flat(
                    TEXT,
                    r#"<office:body><office:text><text:p>before</text:p><text:h text:outline-level="x">h</text:h></office:text></office:body>"#,
                ),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
    }

    #[test]
    fn extracts_binary_data_in_the_body_as_tikas_path_does() {
        let scratch = Scratch::new("flat-binary-data");
        let image = |data: &str| {
            format!(
                "<office:body><office:text><text:p>before</text:p><text:p><draw:frame>\
                 <draw:image><office:binary-data>{data}</office:binary-data></draw:image>\
                 </draw:frame>after</text:p></office:text></office:body>"
            )
        };
        let files = [
            scratch.write("zeros.fodt", &flat(TEXT, &image("AAAA"))),
            scratch.write(
                "text.fodt",
                &flat(TEXT, &image("SGVq\n  IHZl cmRl\nbiE=ignored")),
            ),
            scratch.write("unpadded.fodt", &flat(TEXT, &image("SGVqIHZlcmRlbiE"))),
            scratch.write("url-safe.fodt", &flat(TEXT, &image("-_-_SGVq"))),
            scratch.write("empty.fodt", &flat(TEXT, &image(""))),
        ];
        let (parsed, deferred) = documents(&FlatOpenDocumentParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), files.len());
        let paths: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    #[ignore]
    fn extracts_what_tikas_path_extracts_from_tikas_documents() {
        let corpus = conformance_corpus();
        let (parsed, deferred) = documents(&FlatOpenDocumentParser, &[&corpus]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert!(!parsed.is_empty());
        let parsed: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&parsed, TIKA_NAME);
    }
}
