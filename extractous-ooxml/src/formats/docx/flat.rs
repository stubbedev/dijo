//! Word 2006 XML documents, a Word package flattened into one XML file of `pkg:part` elements,
//! as Tika's `Word2006MLParser` reads them: part by part in the order of the file, the text of
//! the document, its notes, headers, footers, comments and glossary written by the Word text
//! handler without styles, list numbers or links, the core and extended properties taken as
//! metadata, and the base64 data of every other part parsed as an embedded document.

use std::collections::HashMap;
use std::io::Write;

use super::body::{Body, Context};
use super::notes::InlineParts;
use super::text::{TextHandler, TextOptions};
use super::{GLOSSARY_TYPE, MAIN_DOCUMENT_TYPES};
use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::{CORE_PROPERTIES_CONTENT_TYPE, RELATIONSHIPS_CONTENT_TYPE};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xml::{self, attribute, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Spool};
use crate::java::CommonsBase64;
use crate::Error;

/// `Word2006MLDocHandler.PKG_NS`.
const PACKAGE_NAMESPACE: &str = "http://schemas.microsoft.com/office/2006/xmlPackage";
const EXTENDED_PROPERTIES_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.extended-properties+xml";
/// The content types of the parts the Word text handler reads, besides the main document's and
/// the glossary's.
const TEXT_PART_TYPES: [&str; 5] = [
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
];
const WHAT: &str = "the Word 2006 XML document";
/// What `Word2006MLParser` makes of the `WriteLimitReachedException` the text's length limit
/// throws: the parse's `SAXException`, which it throws again as an XML parse error, failing the
/// extraction where the limit alone would end it.
const LIMIT_RETHROWN: &str = "XML parse error: org.apache.tika.exception.\
                              WriteLimitReachedException: the text reached the extraction's \
                              length limit, which Word2006MLParser rethrows";

pub struct Word2006MlParser;

impl Parser for Word2006MlParser {
    fn name(&self) -> &'static str {
        "extractous::Word2006MLParser"
    }

    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.ooxml.xwpf.ml2006.Word2006MLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/vnd.ms-word2006ml"]
    }

    /// The document is read as it is written, its parts' binary data decoded to a spool.
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
        let input = Input::Within(input, pages);
        Ok(Box::new(move |xhtml| write(&input, &configs.office, xhtml)))
    }
}

/// `Word2006MLParser.parse`: a document Xerces fails on fails the parse, after what was written
/// before, and is not ended.
fn write(input: &Input, config: &OfficeParserConfig, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let inline = InlineParts::default();
    let links = HashMap::new();
    let mut flat = Flat {
        reader: Some(Reader::Outside(xhtml)),
        inline: &inline,
        links: &links,
        config,
        core: PropertiesReader::new(&CORE_PROPERTIES),
        extended: PropertiesReader::new(&EXTENDED_PROPERTIES),
        binary: BinaryData::default(),
    };
    xml::parse_part(input, WHAT, &mut flat).map_err(|failure| match failure {
        Failure::Malformed(reason) => Failure::Malformed(format!("XML parse error: {reason}")),
        Failure::Stopped(Stop::LimitReached) => Failure::Malformed(LIMIT_RETHROWN.to_owned()),
        other => other,
    })
}

/// What reads a part, by its content type: `Word2006MLDocHandler`'s part handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    /// `WordAndPowerPointTextPartHandler`.
    Text,
    Data(DataPart),
}

/// A part read by another handler than the text handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataPart {
    /// `CorePropertiesHandler`.
    Core,
    /// `ExtendedPropertiesHandler`.
    Extended,
    /// `RelationshipsHandler`, whose relationships no other handler looks up.
    Relationships,
    /// `BinaryDataHandler`, which takes every part of another type.
    Binary,
}

impl PartKind {
    fn of(content_type: Option<&str>) -> Self {
        match content_type {
            Some(RELATIONSHIPS_CONTENT_TYPE) => Self::Data(DataPart::Relationships),
            Some(CORE_PROPERTIES_CONTENT_TYPE) => Self::Data(DataPart::Core),
            Some(EXTENDED_PROPERTIES_CONTENT_TYPE) => Self::Data(DataPart::Extended),
            Some(text)
                if text == MAIN_DOCUMENT_TYPES[0]
                    || text == GLOSSARY_TYPE
                    || TEXT_PART_TYPES.contains(&text) =>
            {
                Self::Text
            }
            _ => Self::Data(DataPart::Binary),
        }
    }
}

/// A metadata property a properties handler takes from the element named `local_name` in
/// `namespace`, its value the text since the last end of an element.
struct Property {
    /// The namespace, without the slash that may end it.
    namespace: &'static str,
    local_name: &'static str,
    name: &'static str,
    /// Whether a value is added to those the property has, rather than replacing them.
    multiple: bool,
}

const fn property(
    namespace: &'static str,
    local_name: &'static str,
    name: &'static str,
    multiple: bool,
) -> Property {
    Property {
        namespace,
        local_name,
        name,
        multiple,
    }
}

const DC: &str = "http://purl.org/dc/elements/1.1";
const DC_TERMS: &str = "http://purl.org/dc/terms";
const CP: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const EP: &str = "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";

/// `CorePropertiesHandler.addProperties`.
const CORE_PROPERTIES: [Property; 12] = [
    property(DC, "creator", "dc:creator", true),
    property(DC, "title", "dc:title", true),
    property(DC, "description", "dc:description", true),
    property(DC_TERMS, "created", "dcterms:created", false),
    property(DC_TERMS, "modified", "dcterms:modified", false),
    property(CP, "category", "cp:category", false),
    property(CP, "contentStatus", "cp:contentStatus", false),
    property(CP, "lastModifiedBy", "meta:last-author", false),
    property(CP, "lastPrinted", "cp:lastPrinted", false),
    property(CP, "revision", "cp:revision", false),
    property(CP, "subject", "dc:subject", true),
    property(CP, "version", "cp:version", false),
];

/// `ExtendedPropertiesHandler.addProperties`.
const EXTENDED_PROPERTIES: [Property; 17] = [
    property(EP, "AppVersion", "extended-properties:AppVersion", false),
    property(EP, "Application", "extended-properties:Application", false),
    property(EP, "Comments", "w:Comments", true),
    property(EP, "Company", "extended-properties:Company", false),
    property(EP, "DocSecurity", "extended-properties:DocSecurity", false),
    property(
        EP,
        "HiddenSlides",
        "extended-properties:HiddenSlides",
        false,
    ),
    property(EP, "Manager", "extended-properties:Manager", true),
    property(EP, "Notes", "extended-properties:Notes", false),
    property(
        EP,
        "PresentationFormat",
        "extended-properties:PresentationFormat",
        false,
    ),
    property(EP, "Template", "extended-properties:Template", false),
    property(EP, "TotalTime", "extended-properties:TotalTime", false),
    property(EP, "Pages", "meta:page-count", false),
    property(EP, "Words", "meta:word-count", false),
    property(EP, "Characters", "meta:character-count", false),
    property(
        EP,
        "CharactersWithSpaces",
        "meta:character-count-with-spaces",
        false,
    ),
    property(EP, "Paragraphs", "meta:paragraph-count", false),
    property(EP, "Lines", "meta:line-count", false),
];

/// `CorePropertiesHandler`: the text since the last end of an element, the value of the
/// property an element ending names.
struct PropertiesReader {
    properties: &'static [Property],
    buffer: String,
}

impl PropertiesReader {
    fn new(properties: &'static [Property]) -> Self {
        Self {
            properties,
            buffer: String::new(),
        }
    }

    fn end_element(&mut self, namespace: &str, local_name: &str, metadata: &mut TikaMetadata) {
        let namespace = namespace.strip_suffix('/').unwrap_or(namespace);
        let found = self
            .properties
            .iter()
            .find(|property| property.namespace == namespace && property.local_name == local_name);
        if let Some(property) = found {
            if property.multiple {
                metadata.add(property.name, self.buffer.as_str());
            } else {
                metadata.set(property.name, self.buffer.as_str());
            }
        }
        self.buffer.clear();
    }
}

/// `BinaryDataHandler`: the base64 text of the `pkg:binaryData` elements of the parts it reads,
/// until a part with some ends, decoded to a spool as it is read.
#[derive(Default)]
struct BinaryData {
    in_data: bool,
    /// The data decoded so far, once some text was read.
    decoding: Option<Decoding>,
}

/// The data of a part being decoded.
struct Decoding {
    decoder: CommonsBase64,
    spool: Spool,
    /// What the text read last decoded into, reused for the next.
    decoded: Vec<u8>,
}

impl BinaryData {
    /// Decodes `text` to the spool.
    fn add(&mut self, text: &str) -> Result<(), Failure> {
        let decoding = self.decoding.get_or_insert_with(|| Decoding {
            decoder: CommonsBase64::default(),
            spool: Spool::new(None),
            decoded: Vec::new(),
        });
        decoding.decoded.clear();
        decoding.decoder.decode(text, &mut decoding.decoded);
        decoding
            .spool
            .write_all(&decoding.decoded)
            .map_err(unspooled)
    }

    /// The data decoded, if the parts read since the last had some text.
    fn take(&mut self) -> Result<Option<Input<'static>>, Failure> {
        let Some(mut decoding) = self.decoding.take() else {
            return Ok(None);
        };
        decoding.decoded.clear();
        decoding.decoder.finish(&mut decoding.decoded);
        decoding
            .spool
            .write_all(&decoding.decoded)
            .map_err(unspooled)?;
        decoding.spool.finish().map(Some).map_err(unspooled)
    }
}

fn unspooled(error: std::io::Error) -> Failure {
    Failure::Extraction(Error::IoError(format!(
        "Could not spool the binary data of a part of {WHAT}: {error}"
    )))
}

/// The handler reading the part the document is in, which holds the output.
enum Reader<'x, 'h, 'c> {
    /// No part.
    Outside(&'x mut Xhtml<'h>),
    Text(Box<TextHandler<'x, 'h, 'c, 'c>>),
    Data(&'x mut Xhtml<'h>, DataPart),
}

impl<'x, 'h> Reader<'x, 'h, '_> {
    fn into_xhtml(self) -> &'x mut Xhtml<'h> {
        match self {
            Self::Outside(xhtml) | Self::Data(xhtml, _) => xhtml,
            Self::Text(handler) => handler.into_body().into_xhtml(),
        }
    }
}

/// `Word2006MLDocHandler`: hands the events within each `pkg:part` to the handler of the part's
/// content type, and ends the part with it.
struct Flat<'x, 'h, 'c> {
    /// Always present between events.
    reader: Option<Reader<'x, 'h, 'c>>,
    inline: &'c InlineParts,
    links: &'c HashMap<String, String>,
    config: &'c OfficeParserConfig,
    core: PropertiesReader,
    extended: PropertiesReader,
    binary: BinaryData,
}

/// The reader of the part the document is in.
fn current<'r, 'x, 'h, 'c>(
    reader: &'r mut Option<Reader<'x, 'h, 'c>>,
) -> &'r mut Reader<'x, 'h, 'c> {
    reader
        .as_mut()
        .expect("the reader is put back after every event")
}

impl<'x, 'h> Flat<'x, 'h, '_> {
    fn take_xhtml(&mut self) -> &'x mut Xhtml<'h> {
        self.reader
            .take()
            .expect("the reader is put back after every event")
            .into_xhtml()
    }

    fn start_part(&mut self, content_type: Option<&str>) {
        let xhtml = self.take_xhtml();
        self.reader = Some(match PartKind::of(content_type) {
            PartKind::Text => {
                let context = Context {
                    styles: None,
                    lists: None,
                    inline: self.inline,
                    records_metadata: false,
                    include_deleted: self.config.includes_deleted_content(),
                    include_move_from: self.config.includes_move_from_content(),
                };
                Reader::Text(Box::new(TextHandler::new(
                    Body::new(xhtml, context),
                    self.links,
                    TextOptions::default(),
                )))
            }
            PartKind::Data(part) => Reader::Data(xhtml, part),
        });
    }

    /// `endPart`: the data of a binary part parsed as an embedded document without a name.
    fn end_part(&mut self) -> Result<(), Failure> {
        let binary = matches!(current(&mut self.reader), Reader::Data(_, DataPart::Binary));
        let xhtml = self.take_xhtml();
        let data = if binary { self.binary.take() } else { Ok(None) };
        let parsed = match data {
            Ok(Some(data)) => {
                xhtml.parse_embedded_input(&data, &TikaMetadata::new(), Wrapping::PackageEntry)
            }
            Ok(None) => Ok(()),
            Err(failure) => Err(failure),
        };
        self.reader = Some(Reader::Outside(xhtml));
        parsed
    }
}

fn is_part(namespace: &str, local_name: &str) -> bool {
    namespace == PACKAGE_NAMESPACE && local_name == "part"
}

fn is_binary_data(namespace: &str, local_name: &str) -> bool {
    namespace == PACKAGE_NAMESPACE && local_name == "binaryData"
}

impl SaxHandler for Flat<'_, '_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if is_part(namespace, local_name) {
            self.start_part(attribute(attributes, PACKAGE_NAMESPACE, "contentType"));
            return Ok(());
        }
        match current(&mut self.reader) {
            Reader::Text(handler) => {
                return handler.start_element(namespace, local_name, attributes)
            }
            Reader::Data(_, DataPart::Binary) if is_binary_data(namespace, local_name) => {
                self.binary.in_data = true;
            }
            Reader::Outside(_) | Reader::Data(..) => {}
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if is_part(namespace, local_name) {
            return self.end_part();
        }
        match current(&mut self.reader) {
            Reader::Text(handler) => return handler.end_element(namespace, local_name),
            Reader::Data(xhtml, DataPart::Core) => {
                self.core
                    .end_element(namespace, local_name, xhtml.metadata_mut());
            }
            Reader::Data(xhtml, DataPart::Extended) => {
                self.extended
                    .end_element(namespace, local_name, xhtml.metadata_mut());
            }
            Reader::Data(_, DataPart::Binary) if is_binary_data(namespace, local_name) => {
                self.binary.in_data = false;
            }
            Reader::Outside(_) | Reader::Data(..) => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        match current(&mut self.reader) {
            Reader::Text(handler) => return handler.characters(text),
            Reader::Data(_, DataPart::Core) => self.core.buffer.push_str(text),
            Reader::Data(_, DataPart::Extended) => self.extended.buffer.push_str(text),
            Reader::Data(_, DataPart::Binary) if self.binary.in_data && !text.is_empty() => {
                self.binary.add(text)?;
            }
            Reader::Outside(_) | Reader::Data(..) => {}
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests;
