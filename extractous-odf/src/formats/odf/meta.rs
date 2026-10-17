//! Tika's `OpenDocumentMetaParser`: `meta.xml` read into metadata by the chain of handlers Tika
//! tees its events to, in that order. Dublin Core elements add their values, the keywords and
//! the initial creator add theirs too, other `meta:` elements set theirs joined by `, `,
//! user-defined fields become `custom:` names, the statistics become counts, and finally the
//! keywords join the subjects.
//!
//! Tika joins the keywords into one value by `, `, and the initial creator to the creators
//! before it, where it adds each as a value of its own; and it finds the attributes of
//! OpenOffice.org 1 documents, and a user-defined field's name under any prefix but `meta:`,
//! nowhere, where they are read by their namespace (#90).

use std::borrow::Cow;

use crate::formats::element_metadata::{set_unless_equal, ElementMetadata, Values};
use crate::formats::metadata::TikaMetadata;
use crate::formats::odf::{normalized_attributes, normalized_namespace, META_NS, OFFICE_NS};
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java;
use crate::java::utf16_length;
use crate::unicode::is_whitespace;

const SUBJECT: &str = "dc:subject";
const DESCRIPTION: &str = "dc:description";
const KEYWORD: &str = "meta:keyword";
const USER_DEFINED_PREFIX: &str = "custom:";
const MAX_USER_DEFINED_NAME_LENGTH: usize = 1024;
const MAX_USER_DEFINED_NAMES: usize = 10_000;

/// Tika's `MetadataHandler` behind a match of `//meta:<element>` and the text within: each such
/// element's text, joined to the value already there by `, `.
struct MetaValue {
    local_name: &'static str,
    name: &'static str,
    open: usize,
    buffer: String,
}

impl MetaValue {
    fn new(local_name: &'static str, name: &'static str) -> Self {
        Self {
            local_name,
            name,
            open: 0,
            buffer: String::new(),
        }
    }

    fn matches(&self, namespace: &str, local_name: &str) -> bool {
        namespace == META_NS && local_name == self.local_name
    }

    fn start(&mut self, namespace: &str, local_name: &str) {
        if self.matches(namespace, local_name) {
            self.open += 1;
        }
    }

    fn end(&mut self, namespace: &str, local_name: &str, metadata: &mut TikaMetadata) {
        if !self.matches(namespace, local_name) {
            return;
        }
        self.open -= 1;
        let value = std::mem::take(&mut self.buffer);
        if value.is_empty() {
            return;
        }
        let joined = match metadata.get(self.name) {
            Some(previous) if !previous.is_empty() => format!("{previous}, {value}"),
            _ => value,
        };
        metadata.set(self.name, joined);
    }

    fn characters(&mut self, text: &str) {
        if self.open > 0 {
            self.buffer.push_str(text);
        }
    }
}

/// Tika's `AttributeDependantMetadataHandler` behind a match of `//meta:user-defined/@meta:name`
/// and the text within: the text of a field under `custom:` and the name its `meta:name`
/// attribute gives.
#[derive(Default)]
struct UserDefined {
    open: Vec<bool>,
    name: Option<String>,
    buffer: String,
    names: usize,
}

impl UserDefined {
    fn matches(namespace: &str, local_name: &str) -> bool {
        namespace == META_NS && local_name == "user-defined"
    }

    fn start(&mut self, namespace: &str, local_name: &str, attributes: &[Attribute]) {
        if !Self::matches(namespace, local_name) {
            return;
        }
        let name = attributes
            .iter()
            .find(|attribute| attribute.namespace == META_NS && attribute.local_name == "name");
        if let Some(name) = name {
            self.name = Some(name.value.clone());
        }
        self.open.push(name.is_some());
    }

    fn end(&mut self, namespace: &str, local_name: &str, metadata: &mut TikaMetadata) {
        if !Self::matches(namespace, local_name) {
            return;
        }
        if self.open.pop() != Some(true) {
            return;
        }
        let value = std::mem::take(&mut self.buffer);
        let Some(name) = &self.name else {
            return;
        };
        if value.is_empty()
            || name.chars().all(is_whitespace)
            || utf16_length(name) > MAX_USER_DEFINED_NAME_LENGTH
        {
            return;
        }
        let key = format!("{USER_DEFINED_PREFIX}{name}");
        if metadata.values(&key).is_empty() {
            if self.names >= MAX_USER_DEFINED_NAMES {
                return;
            }
            self.names += 1;
        }
        metadata.add(&key, value);
    }

    fn characters(&mut self, text: &str) {
        if !self.open.is_empty() {
            self.buffer.push_str(text);
        }
    }
}

/// The document statistics Tika reads, as `(attribute of meta:document-statistic, name)`, in the
/// order of its handlers.
const STATISTICS: [(&str, &str); 8] = [
    ("object-count", "meta:object-count"),
    ("image-count", "meta:image-count"),
    ("page-count", "meta:page-count"),
    ("page-count", "xmpTPg:NPages"),
    ("table-count", "meta:table-count"),
    ("paragraph-count", "meta:paragraph-count"),
    ("word-count", "meta:word-count"),
    ("character-count", "meta:character-count"),
];

/// The handlers Tika's `OpenDocumentMetaParser.getContentHandler` tees metadata events to, in
/// their order, behind its `NSNormalizerContentHandler`.
pub(super) struct MetaHandlers {
    depth: usize,
    dublin_core: [ElementMetadata; 10],
    created: MetaValue,
    modified: ElementMetadata,
    subject: ElementMetadata,
    keyword: ElementMetadata,
    initial_creator: ElementMetadata,
    meta: [MetaValue; 3],
    user_defined: UserDefined,
}

impl MetaHandlers {
    pub(super) fn new() -> Self {
        Self {
            depth: 0,
            dublin_core: [
                ElementMetadata::dublin_core("title", "dc:title", Values::Bag),
                ElementMetadata::dublin_core("creator", "dc:creator", Values::Bag),
                ElementMetadata::dublin_core("description", DESCRIPTION, Values::Bag),
                ElementMetadata::dublin_core("publisher", "dc:publisher", Values::Bag),
                ElementMetadata::dublin_core("contributor", "dc:contributor", Values::Bag),
                ElementMetadata::dublin_core("type", "dc:type", Values::Bag),
                ElementMetadata::dublin_core("format", "dc:format", Values::Single),
                ElementMetadata::dublin_core("identifier", "dc:identifier", Values::Bag),
                ElementMetadata::dublin_core("language", "dc:language", Values::Bag),
                ElementMetadata::dublin_core("rights", "dc:rights", Values::Bag),
            ],
            created: MetaValue::new("creation-date", "dcterms:created"),
            modified: ElementMetadata::dublin_core("date", "dcterms:modified", Values::Single),
            subject: ElementMetadata::dublin_core("subject", SUBJECT, Values::Bag),
            keyword: ElementMetadata::new(META_NS, "keyword", KEYWORD, Values::Bag),
            initial_creator: ElementMetadata::new(
                META_NS,
                "initial-creator",
                "dc:creator",
                Values::Bag,
            ),
            meta: [
                MetaValue::new("editing-duration", "extended-properties:TotalTime"),
                MetaValue::new("editing-cycles", "editing-cycles"),
                MetaValue::new("generator", "generator"),
            ],
            user_defined: UserDefined::default(),
        }
    }

    pub(super) fn start(
        &mut self,
        metadata: &mut TikaMetadata,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) {
        let namespace: Cow<str> = normalized_namespace(namespace);
        let attributes = normalized_attributes(attributes);
        let attributes = attributes.as_ref();
        let root = self.depth == 0;
        self.depth += 1;
        for element in &mut self.dublin_core {
            element.start(&namespace, local_name);
        }
        if root && namespace == OFFICE_NS && local_name == "document-meta" {
            attribute_value(metadata, "odf:version", attributes, OFFICE_NS, "version");
        }
        self.created.start(&namespace, local_name);
        self.modified.start(&namespace, local_name);
        self.subject.start(&namespace, local_name);
        self.keyword.start(&namespace, local_name);
        self.initial_creator.start(&namespace, local_name);
        for element in &mut self.meta {
            element.start(&namespace, local_name);
        }
        self.user_defined.start(&namespace, local_name, attributes);
        if namespace == META_NS && local_name == "document-statistic" {
            for (attribute, name) in STATISTICS {
                attribute_value(metadata, name, attributes, META_NS, attribute);
            }
        }
    }

    pub(super) fn end(&mut self, metadata: &mut TikaMetadata, namespace: &str, local_name: &str) {
        let namespace = normalized_namespace(namespace);
        self.depth -= 1;
        for element in &mut self.dublin_core {
            element.end(&namespace, local_name, metadata);
        }
        self.created.end(&namespace, local_name, metadata);
        self.modified.end(&namespace, local_name, metadata);
        self.subject.end(&namespace, local_name, metadata);
        self.keyword.end(&namespace, local_name, metadata);
        self.initial_creator.end(&namespace, local_name, metadata);
        for element in &mut self.meta {
            element.end(&namespace, local_name, metadata);
        }
        self.user_defined.end(&namespace, local_name, metadata);
    }

    pub(super) fn characters(&mut self, text: &str) {
        for element in &mut self.dublin_core {
            element.characters(text);
        }
        self.created.characters(text);
        self.modified.characters(text);
        self.subject.characters(text);
        self.keyword.characters(text);
        self.initial_creator.characters(text);
        for element in &mut self.meta {
            element.characters(text);
        }
        self.user_defined.characters(text);
    }
}

/// Tika's `AttributeMetadataHandler`: the trimmed value of an attribute under `name`.
pub(super) fn attribute_value(
    metadata: &mut TikaMetadata,
    name: &str,
    attributes: &[Attribute],
    namespace: &str,
    local_name: &str,
) {
    for attribute in attributes {
        if attribute.namespace == namespace && attribute.local_name == local_name {
            let value = java::trim(&attribute.value);
            if !value.is_empty() {
                set_unless_equal(name, value.to_owned(), metadata);
            }
        }
    }
}

/// `meta.xml` as Tika's `OpenDocumentMetaParser` reads it.
struct MetaFile<'m> {
    metadata: &'m mut TikaMetadata,
    handlers: MetaHandlers,
}

impl SaxHandler for MetaFile<'_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.handlers
            .start(self.metadata, namespace, local_name, attributes);
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.handlers.end(self.metadata, namespace, local_name);
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.handlers.characters(text);
        Ok(())
    }
}

/// What Tika's `OpenDocumentMetaParser.parse` does once its handlers are done: a subject is the
/// description too where there is none, and the keywords join the subjects.
pub(super) fn merge_subjects(metadata: &mut TikaMetadata) {
    if let Some(subject) = metadata.get(SUBJECT).filter(|subject| !subject.is_empty()) {
        if metadata.get(DESCRIPTION).is_none_or(str::is_empty) {
            let subject = subject.to_owned();
            metadata.set(DESCRIPTION, subject);
        }
    }
    let subjects: Vec<String> = metadata
        .values(KEYWORD)
        .iter()
        .chain(metadata.values(SUBJECT))
        .cloned()
        .collect();
    if !subjects.is_empty() {
        metadata.reset_values(SUBJECT, subjects);
    }
}

/// Reads the metadata of `meta.xml`, given as its bytes, into `metadata`.
pub(super) fn read_meta(input: &[u8], metadata: &mut TikaMetadata) -> Result<(), Failure> {
    let mut file = MetaFile {
        metadata,
        handlers: MetaHandlers::new(),
    };
    xml::parse_sax(input, "meta.xml", &mut file)?;
    merge_subjects(file.metadata);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(xml: &str) -> TikaMetadata {
        let mut metadata = TikaMetadata::new();
        read_meta(xml.as_bytes(), &mut metadata).unwrap();
        metadata
    }

    #[test]
    fn reads_dublin_core_meta_elements_and_statistics() {
        let metadata = meta(
            r#"<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"
 office:version=" 1.3 "><office:meta>
<meta:initial-creator>Anna</meta:initial-creator><dc:creator>Bo</dc:creator><dc:creator>Bo</dc:creator>
<dc:title> Fyret </dc:title><dc:subject>Lys</dc:subject><meta:keyword>hav</meta:keyword><meta:keyword>sten</meta:keyword>
<meta:user-defined meta:name="Info 1">Tekst</meta:user-defined><meta:user-defined meta:name="Info 1">Mere</meta:user-defined>
<meta:document-statistic meta:page-count="2" meta:word-count="10"/>
</office:meta></office:document-meta>"#,
        );
        assert_eq!(metadata.values("odf:version"), ["1.3"]);
        assert_eq!(metadata.values("dc:creator"), ["Anna", "Bo"]);
        assert_eq!(metadata.values("dc:title"), ["Fyret"]);
        assert_eq!(metadata.values("meta:keyword"), ["hav", "sten"]);
        assert_eq!(metadata.values("dc:subject"), ["hav", "sten", "Lys"]);
        assert_eq!(metadata.values("dc:description"), ["Lys"]);
        assert_eq!(metadata.values("custom:Info 1"), ["Tekst", "Mere"]);
        assert_eq!(metadata.values("meta:page-count"), ["2"]);
        assert_eq!(metadata.values("xmpTPg:NPages"), ["2"]);
        assert_eq!(metadata.values("meta:word-count"), ["10"]);
    }

    #[test]
    fn adds_a_creator_after_the_dublin_core_one_as_a_value_of_its_own() {
        let metadata = meta(
            r#"<m xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:creator>Bo</dc:creator><meta:initial-creator>Anna</meta:initial-creator><meta:initial-creator>Bo</meta:initial-creator></m>"#,
        );
        assert_eq!(metadata.values("dc:creator"), ["Bo", "Anna"]);
    }

    #[test]
    fn reads_old_openoffice_elements_and_attributes() {
        let metadata = meta(
            r#"<office:document-meta xmlns:office="http://openoffice.org/2000/office"
 xmlns:meta="http://openoffice.org/2000/meta" office:version="1.0"><office:meta>
<meta:generator>StarOffice</meta:generator><meta:document-statistic meta:page-count="1"/>
<meta:user-defined meta:name="Info 1">x</meta:user-defined></office:meta></office:document-meta>"#,
        );
        assert_eq!(metadata.values("generator"), ["StarOffice"]);
        assert_eq!(metadata.values("odf:version"), ["1.0"]);
        assert_eq!(metadata.values("meta:page-count"), ["1"]);
        assert_eq!(metadata.values("custom:Info 1"), ["x"]);
    }

    #[test]
    fn finds_a_user_defined_name_under_any_prefix() {
        let metadata = meta(
            r#"<m xmlns:m="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">
<m:user-defined m:name="Info 1">x</m:user-defined></m>"#,
        );
        assert_eq!(metadata.values("custom:Info 1"), ["x"]);
    }

    #[test]
    fn fails_on_malformed_xml() {
        let mut metadata = TikaMetadata::new();
        assert!(read_meta(b"<office:document-meta>", &mut metadata).is_err());
    }
}
