//! Tika's `OpenDocumentMetaParser`: `meta.xml` read into metadata by the chain of handlers Tika
//! tees its events to, in that order. Dublin Core elements add their values, `meta:` elements
//! set theirs joined by `, `, user-defined fields become `custom:` names, the statistics become
//! counts, and finally the keywords join the subjects.

use std::borrow::Cow;

use crate::formats::handlers::{is_java_whitespace, utf16_length};
use crate::formats::metadata::TikaMetadata;
use crate::formats::odf::{java_trim, normalized_namespace, META_NS, OFFICE_NS};
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::Failure;

const DC_NS: &str = "http://purl.org/dc/elements/1.1/";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDF_BAG: &str = "Bag";
const RDF_LI: &str = "li";
const SUBJECT: &str = "dc:subject";
const DESCRIPTION: &str = "dc:description";
const KEYWORD: &str = "meta:keyword";
const USER_DEFINED_PREFIX: &str = "custom:";
const USER_DEFINED_NAME: &str = "meta:name";
const MAX_USER_DEFINED_NAME_LENGTH: usize = 1024;
const MAX_USER_DEFINED_NAMES: usize = 10_000;

/// How a handler stores a value under a name that already has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Values {
    /// A bag of distinct values: a new one is added.
    Bag,
    /// One value: a new one replaces it.
    Single,
}

/// Tika's `ElementMetadataHandler`: the text of an element, or of the `rdf:li`s of an `rdf:Bag`
/// in it, trimmed, under a name.
struct ElementValue {
    local_name: &'static str,
    name: &'static str,
    values: Values,
    match_level: usize,
    parent_match_level: usize,
    bagged: String,
    bagless: String,
    is_bagless: bool,
}

impl ElementValue {
    fn new(local_name: &'static str, name: &'static str, values: Values) -> Self {
        Self {
            local_name,
            name,
            values,
            match_level: 0,
            parent_match_level: 0,
            bagged: String::new(),
            bagless: String::new(),
            is_bagless: true,
        }
    }

    fn is_parent(&self, namespace: &str, local_name: &str) -> bool {
        namespace == DC_NS && local_name == self.local_name
    }

    fn is_matching(&self, namespace: &str, local_name: &str) -> bool {
        self.is_parent(namespace, local_name)
            || (self.parent_match_level > 0
                && namespace == RDF_NS
                && (local_name == RDF_BAG || local_name == RDF_LI))
    }

    fn start(&mut self, namespace: &str, local_name: &str) {
        if self.is_matching(namespace, local_name) {
            self.match_level += 1;
        }
        if self.is_parent(namespace, local_name) {
            self.parent_match_level += 1;
        }
    }

    fn end(&mut self, namespace: &str, local_name: &str, metadata: &mut TikaMetadata) {
        if self.is_parent(namespace, local_name) {
            self.parent_match_level -= 1;
        }
        if !self.is_matching(namespace, local_name) {
            return;
        }
        self.match_level -= 1;
        if self.match_level == 2 {
            let value = java_trim(&self.bagged).to_owned();
            self.add(value, metadata);
            self.bagged.clear();
            self.is_bagless = false;
        }
        if self.match_level == 0 && self.is_bagless {
            if !self.bagless.is_empty() && !self.bagless.contains(RDF_BAG) {
                let value = java_trim(&self.bagless).to_owned();
                self.add(value, metadata);
                self.bagless.clear();
            }
            self.is_bagless = true;
        }
    }

    fn characters(&mut self, text: &str) {
        if self.parent_match_level > 0 && self.match_level > 2 {
            self.bagged.push_str(text);
        }
        if self.parent_match_level > 0 && self.match_level > 0 {
            self.bagless.push_str(text);
        }
    }

    fn add(&self, value: String, metadata: &mut TikaMetadata) {
        if value.is_empty() {
            return;
        }
        match self.values {
            Values::Bag => {
                if !metadata.values(self.name).contains(&value) {
                    metadata.add(self.name, value);
                }
            }
            Values::Single => set_unless_equal(self.name, value, metadata),
        }
    }
}

/// Tika's `AbstractMetadataHandler.addMetadata` for a name that takes one value.
fn set_unless_equal(name: &str, value: String, metadata: &mut TikaMetadata) {
    if metadata.get(name) != Some(value.as_str()) {
        metadata.set(name, value);
    }
}

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
/// attribute, as written with that prefix, last gave.
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
        let mut named = attributes
            .iter()
            .filter(|attribute| attribute.namespace == META_NS && attribute.local_name == "name")
            .peekable();
        let passed = named.peek().is_some();
        if let Some(name) = named.find(|attribute| attribute.qualified_name == USER_DEFINED_NAME) {
            self.name = Some(name.value.to_string());
        }
        self.open.push(passed);
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
            || name.chars().all(is_java_whitespace)
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

/// The handlers Tika tees `meta.xml`'s events to, in their order.
struct MetaHandler<'m> {
    metadata: &'m mut TikaMetadata,
    depth: usize,
    dublin_core: [ElementValue; 10],
    created: MetaValue,
    modified: ElementValue,
    subject: ElementValue,
    meta: [MetaValue; 5],
    user_defined: UserDefined,
}

impl<'m> MetaHandler<'m> {
    fn new(metadata: &'m mut TikaMetadata) -> Self {
        Self {
            metadata,
            depth: 0,
            dublin_core: [
                ElementValue::new("title", "dc:title", Values::Bag),
                ElementValue::new("creator", "dc:creator", Values::Bag),
                ElementValue::new("description", DESCRIPTION, Values::Bag),
                ElementValue::new("publisher", "dc:publisher", Values::Bag),
                ElementValue::new("contributor", "dc:contributor", Values::Bag),
                ElementValue::new("type", "dc:type", Values::Bag),
                ElementValue::new("format", "dc:format", Values::Single),
                ElementValue::new("identifier", "dc:identifier", Values::Bag),
                ElementValue::new("language", "dc:language", Values::Bag),
                ElementValue::new("rights", "dc:rights", Values::Bag),
            ],
            created: MetaValue::new("creation-date", "dcterms:created"),
            modified: ElementValue::new("date", "dcterms:modified", Values::Single),
            subject: ElementValue::new("subject", SUBJECT, Values::Bag),
            meta: [
                MetaValue::new("keyword", KEYWORD),
                MetaValue::new("editing-duration", "extended-properties:TotalTime"),
                MetaValue::new("editing-cycles", "editing-cycles"),
                MetaValue::new("initial-creator", "dc:creator"),
                MetaValue::new("generator", "generator"),
            ],
            user_defined: UserDefined::default(),
        }
    }

    fn attribute_value(
        &mut self,
        name: &str,
        attributes: &[Attribute],
        namespace: &str,
        local_name: &str,
    ) {
        for attribute in attributes {
            if attribute.namespace == namespace && attribute.local_name == local_name {
                let value = java_trim(&attribute.value);
                if !value.is_empty() {
                    set_unless_equal(name, value.to_owned(), self.metadata);
                }
            }
        }
    }

    /// What Tika's `OpenDocumentMetaParser.parse` does once the handlers are done.
    fn finish(self) {
        let metadata = self.metadata;
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
}

impl SaxHandler for MetaHandler<'_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let namespace: Cow<str> = normalized_namespace(namespace);
        let root = self.depth == 0;
        self.depth += 1;
        for element in &mut self.dublin_core {
            element.start(&namespace, local_name);
        }
        if root && namespace == OFFICE_NS && local_name == "document-meta" {
            self.attribute_value("odf:version", attributes, OFFICE_NS, "version");
        }
        self.created.start(&namespace, local_name);
        self.modified.start(&namespace, local_name);
        self.subject.start(&namespace, local_name);
        for element in &mut self.meta {
            element.start(&namespace, local_name);
        }
        self.user_defined.start(&namespace, local_name, attributes);
        if namespace == META_NS && local_name == "document-statistic" {
            for (attribute, name) in STATISTICS {
                self.attribute_value(name, attributes, META_NS, attribute);
            }
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        let namespace = normalized_namespace(namespace);
        self.depth -= 1;
        for element in &mut self.dublin_core {
            element.end(&namespace, local_name, self.metadata);
        }
        self.created.end(&namespace, local_name, self.metadata);
        self.modified.end(&namespace, local_name, self.metadata);
        self.subject.end(&namespace, local_name, self.metadata);
        for element in &mut self.meta {
            element.end(&namespace, local_name, self.metadata);
        }
        self.user_defined.end(&namespace, local_name, self.metadata);
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        for element in &mut self.dublin_core {
            element.characters(text);
        }
        self.created.characters(text);
        self.modified.characters(text);
        self.subject.characters(text);
        for element in &mut self.meta {
            element.characters(text);
        }
        self.user_defined.characters(text);
        Ok(())
    }
}

/// Reads the metadata of `meta.xml`, given as its bytes, into `metadata`.
pub(super) fn read_meta(input: &[u8], metadata: &mut TikaMetadata) -> Result<(), Failure> {
    let mut handler = MetaHandler::new(metadata);
    xml::parse_sax(input, "meta.xml", &mut handler)?;
    handler.finish();
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
        assert_eq!(metadata.values("meta:keyword"), ["hav, sten"]);
        assert_eq!(metadata.values("dc:subject"), ["hav, sten", "Lys"]);
        assert_eq!(metadata.values("dc:description"), ["Lys"]);
        assert_eq!(metadata.values("custom:Info 1"), ["Tekst", "Mere"]);
        assert_eq!(metadata.values("meta:page-count"), ["2"]);
        assert_eq!(metadata.values("xmpTPg:NPages"), ["2"]);
        assert_eq!(metadata.values("meta:word-count"), ["10"]);
    }

    #[test]
    fn joins_a_creator_after_the_dublin_core_one() {
        let metadata = meta(
            r#"<m xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:creator>Bo</dc:creator><meta:initial-creator>Anna</meta:initial-creator></m>"#,
        );
        assert_eq!(metadata.values("dc:creator"), ["Bo, Anna"]);
    }

    #[test]
    fn reads_old_openoffice_elements_but_not_their_attributes() {
        let metadata = meta(
            r#"<office:document-meta xmlns:office="http://openoffice.org/2000/office"
 xmlns:meta="http://openoffice.org/2000/meta" office:version="1.0"><office:meta>
<meta:generator>StarOffice</meta:generator><meta:document-statistic meta:page-count="1"/>
<meta:user-defined meta:name="Info 1">x</meta:user-defined></office:meta></office:document-meta>"#,
        );
        assert_eq!(metadata.values("generator"), ["StarOffice"]);
        assert!(metadata.get("odf:version").is_none());
        assert!(metadata.get("meta:page-count").is_none());
        assert!(metadata.get("custom:Info 1").is_none());
    }

    #[test]
    fn fails_on_malformed_xml() {
        let mut metadata = TikaMetadata::new();
        assert!(read_meta(b"<office:document-meta>", &mut metadata).is_err());
    }
}
