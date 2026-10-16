//! Tika's `ElementMetadataHandler` and the `AbstractMetadataHandler` it extends: the text of an
//! element, or of the `rdf:li`s of an `rdf:Bag` in it, trimmed, under a metadata name. Tika's XML
//! metadata parsers, such as `DcXMLParser` and `OpenDocumentMetaParser`, tee their events to one
//! for each Dublin Core element they read.

use crate::formats::metadata::TikaMetadata;
use crate::java;

/// The Dublin Core namespace.
pub(crate) const DUBLIN_CORE_NS: &str = "http://purl.org/dc/elements/1.1/";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDF_BAG: &str = "Bag";
const RDF_LI: &str = "li";

/// How a handler stores a value under a name that already has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Values {
    /// A bag of distinct values: a new one is added.
    Bag,
    /// One value: a new one replaces it.
    Single,
}

/// An `ElementMetadataHandler` for the element `local_name` of `namespace`.
#[derive(Debug)]
pub(crate) struct ElementMetadata {
    namespace: &'static str,
    local_name: &'static str,
    name: &'static str,
    values: Values,
    match_level: usize,
    parent_match_level: usize,
    bagged: String,
    bagless: String,
    is_bagless: bool,
}

impl ElementMetadata {
    pub(crate) fn new(
        namespace: &'static str,
        local_name: &'static str,
        name: &'static str,
        values: Values,
    ) -> Self {
        Self {
            namespace,
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

    /// A handler for the Dublin Core element `local_name`.
    pub(crate) fn dublin_core(
        local_name: &'static str,
        name: &'static str,
        values: Values,
    ) -> Self {
        Self::new(DUBLIN_CORE_NS, local_name, name, values)
    }

    fn is_parent(&self, namespace: &str, local_name: &str) -> bool {
        namespace == self.namespace && local_name == self.local_name
    }

    fn is_matching(&self, namespace: &str, local_name: &str) -> bool {
        self.is_parent(namespace, local_name)
            || (self.parent_match_level > 0
                && namespace == RDF_NS
                && (local_name == RDF_BAG || local_name == RDF_LI))
    }

    pub(crate) fn start(&mut self, namespace: &str, local_name: &str) {
        if self.is_matching(namespace, local_name) {
            self.match_level += 1;
        }
        if self.is_parent(namespace, local_name) {
            self.parent_match_level += 1;
        }
    }

    pub(crate) fn end(&mut self, namespace: &str, local_name: &str, metadata: &mut TikaMetadata) {
        if self.is_parent(namespace, local_name) {
            self.parent_match_level -= 1;
        }
        if !self.is_matching(namespace, local_name) {
            return;
        }
        self.match_level -= 1;
        if self.match_level == 2 {
            let value = java::trim(&self.bagged).to_owned();
            self.add(value, metadata);
            self.bagged.clear();
            self.is_bagless = false;
        }
        if self.match_level == 0 && self.is_bagless {
            if !self.bagless.is_empty() && !self.bagless.contains(RDF_BAG) {
                let value = java::trim(&self.bagless).to_owned();
                self.add(value, metadata);
                self.bagless.clear();
            }
            self.is_bagless = true;
        }
    }

    pub(crate) fn characters(&mut self, text: &str) {
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

/// The Dublin Core elements Tika's `DcXMLParser` reads, as `(element, name, values)`, in the
/// order of its handlers.
const DC_XML_ELEMENTS: [(&str, &str, Values); 13] = [
    ("title", "dc:title", Values::Bag),
    ("subject", "dc:subject", Values::Bag),
    ("creator", "dc:creator", Values::Bag),
    ("description", "dc:description", Values::Bag),
    ("publisher", "dc:publisher", Values::Bag),
    ("contributor", "dc:contributor", Values::Bag),
    ("date", "dcterms:created", Values::Single),
    ("type", "dc:type", Values::Bag),
    ("format", "dc:format", Values::Single),
    ("identifier", "dc:identifier", Values::Bag),
    ("language", "dc:language", Values::Bag),
    ("source", "dc:source", Values::Bag),
    ("rights", "dc:rights", Values::Bag),
];

/// The handlers `DcXMLParser` tees an XML document's events to, one for each Dublin Core
/// element it reads.
#[derive(Debug)]
pub(crate) struct DublinCore(Vec<ElementMetadata>);

impl DublinCore {
    pub(crate) fn new() -> Self {
        Self(
            DC_XML_ELEMENTS
                .iter()
                .map(|&(element, name, values)| ElementMetadata::dublin_core(element, name, values))
                .collect(),
        )
    }

    pub(crate) fn start(&mut self, namespace: &str, local_name: &str) {
        for element in &mut self.0 {
            element.start(namespace, local_name);
        }
    }

    pub(crate) fn end(&mut self, namespace: &str, local_name: &str, metadata: &mut TikaMetadata) {
        for element in &mut self.0 {
            element.end(namespace, local_name, metadata);
        }
    }

    pub(crate) fn characters(&mut self, text: &str) {
        for element in &mut self.0 {
            element.characters(text);
        }
    }
}

/// Tika's `AbstractMetadataHandler.addMetadata` for a name that takes one value.
pub(crate) fn set_unless_equal(name: &str, value: String, metadata: &mut TikaMetadata) {
    if metadata.get(name) != Some(value.as_str()) {
        metadata.set(name, value);
    }
}
