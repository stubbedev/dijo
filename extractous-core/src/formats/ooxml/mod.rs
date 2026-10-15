//! What the Office Open XML formats share at the level of their package, as POI's `OPCPackage`
//! opens it and Tika's `OOXMLExtractorFactory` and `SAXBasedMetadataExtractor` read it: the
//! parts with their content types, the relationships of the package and of each part, and the
//! core, extended and custom properties as Tika's metadata; and what Tika's
//! `AbstractOOXMLExtractor` writes of any package: links, embedded documents and thumbnails.
//!
//! An encrypted document is no ZIP archive but an OLE2 file, which Tika's detection names
//! `application/x-tika-ooxml-protected`, so it never reaches a native parser; were it to,
//! [`Package::open`] would leave it to Tika as an archive it cannot read.

mod embedded;
mod package;
mod properties;
#[cfg(test)]
pub(crate) mod testing;
mod uri;

pub(crate) use embedded::{
    external_reference, kind, linked_relationships, read_ole_object, write_embedded_parts,
    write_thumbnails, OLE_OBJECT_TYPE, SMALLEST_OLE_OBJECT,
};
pub(crate) use package::{Package, Relationship, Unported, RELATIONSHIPS_CONTENT_TYPE};
pub(crate) use properties::extract as extract_properties;

use crate::formats::metadata::TikaMetadata;
use crate::formats::xml::{self, Attribute, SaxHandler, StartTag};
use crate::formats::Failure;

const SIGNATURE_RELATIONSHIP: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin";

/// What `OOXMLExtractorFactory` records before its `extractor` runs: a signature, the parts no
/// relationship of the package reaches, and the extractor among the parsers.
pub(crate) fn write_factory_metadata(
    package: &Package,
    extractor: &str,
    metadata: &mut TikaMetadata,
) {
    if Package::relationships_of(&package.relationships, SIGNATURE_RELATIONSHIP)
        .next()
        .is_some()
    {
        metadata.set("tk:has-signature", "true");
    }
    let mut reachable = vec![false; package.parts.len()];
    let mut queue = std::collections::VecDeque::new();
    for relationship in &package.relationships {
        if !relationship.is_internal() {
            continue;
        }
        if let Some(part) = package.package_part(relationship) {
            if !std::mem::replace(&mut reachable[part], true) {
                queue.push_back(part);
            }
        }
    }
    while let Some(source) = queue.pop_front() {
        for relationship in &package.parts[source].relationships {
            if let (true, Some(part)) = (relationship.is_internal(), relationship.part) {
                if !std::mem::replace(&mut reachable[part], true) {
                    queue.push_back(part);
                }
            }
        }
    }
    for (part, reached) in package.parts.iter().zip(&reachable) {
        if part.content_type != RELATIONSHIPS_CONTENT_TYPE && !reached {
            metadata.set("msoffice:has-unreferenced-parts", "true");
            metadata.add("msoffice:unreferenced-part-names", part.name.clone());
        }
    }
    metadata.add("tk:parsed-by", extractor);
}

/// Reads `part` into `handler` as Tika reads a part whose failure it passes over: `None` where
/// the XML fails. A part declaring a document type is [`Unported`], as Java reads the
/// declarations this port skips.
pub(crate) fn read_quietly<H: SaxHandler>(
    package: &Package,
    part: usize,
    handler: H,
) -> Result<Option<H>, Unported> {
    let bytes = package
        .read(part)
        .map_err(|_| Unported(format!("the part {} cannot be read", package.name(part))))?;
    let mut watch = DocumentTypeWatch {
        inner: handler,
        declared: false,
    };
    let result = xml::parse_sax(&bytes, package.name(part), &mut watch);
    if watch.declared {
        return Err(Unported(format!(
            "{} declares a document type",
            package.name(part)
        )));
    }
    Ok(result.ok().map(|()| watch.inner))
}

/// Passes events on, noting a document type declaration.
struct DocumentTypeWatch<H> {
    inner: H,
    declared: bool,
}

impl<H: SaxHandler> SaxHandler for DocumentTypeWatch<H> {
    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        self.inner.start_tag(tag)
    }

    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.inner.start_element(namespace, local_name, attributes)
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.inner.end_element(namespace, local_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.inner.characters(text)
    }

    fn document_type(&mut self) -> Result<(), Failure> {
        self.declared = true;
        self.inner.document_type()
    }
}

/// A handler that only lets the XML be read.
pub(crate) struct Ignore;

impl SaxHandler for Ignore {
    fn start_element(
        &mut self,
        _namespace: &str,
        _local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}
