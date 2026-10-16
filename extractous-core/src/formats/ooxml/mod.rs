//! What the Office Open XML formats share at the level of their package, as POI's `OPCPackage`
//! opens it and Tika's `OOXMLExtractorFactory` and `SAXBasedMetadataExtractor` read it: the
//! parts with their content types, the relationships of the package and of each part, and the
//! core, extended and custom properties as Tika's metadata.
//!
//! An encrypted document is no ZIP archive but an OLE2 file, which Tika's detection names
//! `application/x-tika-ooxml-protected`; the protected parser decrypts it and hands the package
//! to the parser of its format.

pub(crate) mod extractor;
mod package;
mod properties;
#[cfg(test)]
pub(crate) mod testing;
mod uri;

pub(crate) use package::{Package, Relationship, Unported, HYPERLINK_RELATIONSHIP};
pub(crate) use properties::Properties;

use crate::formats::xml::{self, Attribute, SaxHandler, StartTag};
use crate::formats::Failure;

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
