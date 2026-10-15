//! What every Office Open XML extractor shares, from Tika's `OOXMLExtractorFactory` and
//! `AbstractOOXMLExtractor`: the package's signature and unreferenced parts, the relationships
//! of the main parts that embed documents or lead out of the package, and the thumbnail.

use std::collections::{HashSet, VecDeque};

use super::package::{
    Package, Relationship, Unported, HYPERLINK_RELATIONSHIP, RELATIONSHIPS_CONTENT_TYPE,
};
use crate::config::OfficeParserConfig;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::object::{self, OleObject};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xml::{Attribute, SaxHandler};
use crate::formats::Failure;

/// Relationship types, as POI's `PackageRelationshipTypes`, `XSSFRelation` and `XWPFRelation`
/// and Tika name them.
pub(crate) mod kind {
    pub(crate) const OFFICE_DOCUMENT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    pub(crate) const STRICT_OFFICE_DOCUMENT: &str =
        "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument";
    pub(crate) const SIGNATURE: &str =
        "http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin";
    pub(crate) const THUMBNAIL: &str =
        "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";
    pub(crate) const COMMENTS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
    pub(crate) const DIAGRAM_DATA: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData";
    pub(crate) const CHART: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
    pub(crate) const STYLES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
    pub(crate) const IMAGE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
    pub(crate) const AUDIO: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/audio";
    pub(crate) const VIDEO: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/video";
    pub(crate) const MEDIA: &str = "http://schemas.microsoft.com/office/2007/relationships/media";
    pub(crate) const PACKAGE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/package";
    pub(crate) const OLE_OBJECT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject";
    pub(crate) const ALTERNATE_FORMAT_CHUNK: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/aFChunk";
    pub(crate) const VBA_PROJECT: &str =
        "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
}

/// Metadata names the Office Open XML extractors share.
pub(crate) mod name {
    pub(crate) const PARSED_BY: &str = "tk:parsed-by";
    pub(crate) const HAS_SIGNATURE: &str = "tk:has-signature";
    pub(crate) const HAS_UNREFERENCED_PARTS: &str = "msoffice:has-unreferenced-parts";
    pub(crate) const UNREFERENCED_PART_NAMES: &str = "msoffice:unreferenced-part-names";
    pub(crate) const HAS_COMMENTS: &str = "msoffice:has-comments";
    pub(crate) const COMMENT_PERSONS: &str = "msoffice:comment-person-display-name";
    pub(crate) const HAS_EXTERNAL_OLE_OBJECTS: &str = "msoffice:has-external-ole-objects";
}

pub(crate) const OLE_OBJECT_TYPE: &str = "application/vnd.openxmlformats-officedocument.oleObject";
/// The smallest OLE object Tika opens: three blocks of a compound file.
const SMALLEST_OLE_OBJECT: u64 = 512 * 3;

/// What `OOXMLExtractorFactory` records of the package before the extractor runs: a digital
/// signature, and the parts no relationship reaches (`markUnreferencedParts`).
pub(crate) fn write_package_metadata(package: &Package, metadata: &mut TikaMetadata) {
    if Package::relationships_of(&package.relationships, kind::SIGNATURE)
        .next()
        .is_some()
    {
        metadata.set(name::HAS_SIGNATURE, "true");
    }
    let mut reachable = vec![false; package.parts.len()];
    let mut queue = VecDeque::new();
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
            metadata.set(name::HAS_UNREFERENCED_PARTS, "true");
            metadata.add(name::UNREFERENCED_PART_NAMES, part.name.clone());
        }
    }
}

/// `getPartsByRelationshipType(CORE_DOCUMENT)`: for each of the package's main document
/// relationships the part `getPart` gives, that of the first of them, sorted by name.
pub(crate) fn core_document_parts(package: &Package) -> Vec<usize> {
    let mut parts: Vec<usize> =
        Package::relationships_of(&package.relationships, kind::OFFICE_DOCUMENT)
            .filter_map(|relationship| package.package_part(relationship))
            .collect();
    parts.sort_unstable();
    parts
}

/// What `handleEmbeddedPart` does with the part an internal relationship of a main part leads
/// to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Embedding {
    /// A file Tika parses as an embedded document of this resource type.
    File(&'static str),
    /// An OLE object too small to be a compound file, which Tika passes over.
    SmallOleObject,
    /// An OLE object Tika opens as a compound file.
    OleObject,
    /// Macros, which Tika extracts when asked to.
    Macros,
}

/// How `handleEmbeddedPart` treats `relationship`, if it is internal and leads to a part.
fn embedding(package: &Package, relationship: &Relationship) -> Option<(usize, Embedding)> {
    if !relationship.is_internal() {
        return None;
    }
    let target = relationship.part?;
    let embedding = match relationship.kind.as_str() {
        kind::OLE_OBJECT if package.parts[target].content_type == OLE_OBJECT_TYPE => {
            if package.size(target) < SMALLEST_OLE_OBJECT {
                Embedding::SmallOleObject
            } else {
                Embedding::OleObject
            }
        }
        kind::IMAGE => Embedding::File("INLINE"),
        kind::MEDIA | kind::VIDEO | kind::AUDIO | kind::PACKAGE | kind::OLE_OBJECT => {
            Embedding::File("ATTACHMENT")
        }
        kind::VBA_PROJECT => Embedding::Macros,
        kind::ALTERNATE_FORMAT_CHUNK => Embedding::File("ALTERNATE_FORMAT_CHUNK"),
        _ => return None,
    };
    Some((target, embedding))
}

/// What of `handleEmbeddedPart`'s work on one relationship of a main part this port does not
/// do: an OLE object POI throws on, and extracting macros when asked to; with `files`, also
/// parsing an embedded file or OLE object.
pub(crate) fn check_embedded(
    package: &Package,
    config: &OfficeParserConfig,
    source: usize,
    relationship: &Relationship,
    files: bool,
) -> Result<(), Unported> {
    if relationship.is_internal() && relationship.kind == HYPERLINK_RELATIONSHIP {
        let path = relationship
            .target
            .split_once('#')
            .map_or(relationship.target.as_str(), |(path, _)| path);
        if !path.starts_with('/') {
            return Err(Unported(format!(
                "{} links to {} inside the package, which is no part name, a failure Tika \
                 records with its stack trace",
                package.name(source),
                relationship.target
            )));
        }
    }
    let Some((target, embedding)) = embedding(package, relationship) else {
        return Ok(());
    };
    let unported = (embedding == Embedding::Macros && config.extracts_macros())
        || (files && matches!(embedding, Embedding::File(_) | Embedding::OleObject));
    if unported {
        return Err(Unported(format!(
            "{} embeds {} ({}), which Tika parses",
            package.name(source),
            package.name(target),
            relationship.kind
        )));
    }
    if embedding == Embedding::OleObject {
        ole_object(package, source, relationship, target)?;
    }
    Ok(())
}

/// `handleEmbeddedOLE`'s reading of the OLE object a relationship leads to.
fn ole_object(
    package: &Package,
    source: usize,
    relationship: &Relationship,
    target: usize,
) -> Result<OleObject, Unported> {
    let bytes = package
        .read(target)
        .map_err(|_| Unported(format!("the part {} cannot be read", package.name(target))))?;
    object::open(
        &bytes,
        &relationship_id(package, source, relationship),
        package.name(target),
    )
}

/// `handleEmbeddedParts` for the relationships of the `main_parts`, once [`check_embedded`]
/// passed them all: each embedded file parsed, with the metadata `anchor` adds, and a link for
/// each relationship that leads out of the package, each target once.
pub(crate) fn write_embedded_parts(
    package: &Package,
    main_parts: &[usize],
    xhtml: &mut Xhtml,
    anchor: &dyn Fn(usize, &mut TikaMetadata),
) -> Result<(), Failure> {
    let mut handled: HashSet<&str> = HashSet::new();
    for &source in main_parts {
        for relationship in &package.parts[source].relationships {
            if handled.contains(relationship.target_uri()) {
                continue;
            }
            if !relationship.is_internal() {
                let class = match relationship.kind.as_str() {
                    kind::OLE_OBJECT => "externalOleObject",
                    kind::IMAGE => "externalImage",
                    _ => "externalResource",
                };
                external_reference(xhtml, class, relationship.target_uri())?;
                if relationship.kind == kind::OLE_OBJECT {
                    xhtml
                        .metadata_mut()
                        .set(name::HAS_EXTERNAL_OLE_OBJECTS, "true");
                }
                continue;
            }
            let Some((target, embedding)) = embedding(package, relationship) else {
                continue;
            };
            let file = |resource_type, xhtml: &mut Xhtml| {
                let metadata = embedded_file_metadata(
                    package,
                    source,
                    relationship,
                    target,
                    resource_type,
                    anchor,
                );
                xhtml.parse_embedded(&package.read(target)?, &metadata, Wrapping::PackageEntry)
            };
            match embedding {
                Embedding::File(resource_type) => file(resource_type, xhtml)?,
                Embedding::OleObject => {
                    match ole_object(package, source, relationship, target)
                        .map_err(|Unported(reason)| Failure::Malformed(reason))?
                    {
                        OleObject::Document { data, metadata } => {
                            xhtml.parse_embedded(&data, &metadata, Wrapping::PackageEntry)?;
                        }
                        OleObject::Part => file("ATTACHMENT", xhtml)?,
                        OleObject::Nothing => {}
                    }
                }
                Embedding::SmallOleObject | Embedding::Macros => {}
            }
            handled.insert(relationship.target_uri());
        }
    }
    Ok(())
}

/// The metadata `handleEmbeddedFile` gives an embedded file.
fn embedded_file_metadata(
    package: &Package,
    source: usize,
    relationship: &Relationship,
    target: usize,
    resource_type: &str,
    anchor: &dyn Fn(usize, &mut TikaMetadata),
) -> TikaMetadata {
    let mut metadata = TikaMetadata::new();
    metadata.set(
        "tk:embedded-relationship-id",
        relationship_id(package, source, relationship),
    );
    metadata.set("tk:embedded-resource-type", resource_type);
    let name = package.name(target);
    metadata.set("tk:internal-path", name);
    metadata.set(
        "tk:resource-name",
        &name[name.rfind('/').map_or(0, |at| at + 1)..],
    );
    metadata.set("Content-Type", package.parts[target].content_type.clone());
    anchor(target, &mut metadata);
    metadata
}

/// The relationship id `handleEmbeddedPart` gives an embedded document: the relationship's,
/// after its source's description.
fn relationship_id(package: &Package, source: usize, relationship: &Relationship) -> String {
    format!(
        "{}{}",
        source_description(package.name(source)),
        relationship.id
    )
}

/// The prefix `handleEmbeddedPart` gives the relationship ids of a slide's embedded files: the
/// slide's file name without its extension and an underscore.
fn source_description(source: &str) -> String {
    let name = &source[source.rfind('/').map_or(0, |at| at + 1)..];
    let name = name.rfind('.').map_or(name, |at| &name[..at]);
    if name.starts_with("slide") {
        format!("{name}_")
    } else {
        String::new()
    }
}

/// `handleThumbnail`: a marker for each thumbnail, which extractous does not parse.
pub(crate) fn write_thumbnails(package: &Package, xhtml: &mut Xhtml) -> Result<(), Stop> {
    for relationship in Package::relationships_of(&package.relationships, kind::THUMBNAIL) {
        if let Some(part) = package.package_part(relationship) {
            xhtml
                .start_element_with("div", &[("class", "embedded"), ("id", package.name(part))])?;
            xhtml.end_element("div")?;
        }
    }
    Ok(())
}

/// `emitExternalRef`: an empty link of class `external-ref-…`, unless the URL is empty.
pub(crate) fn external_reference(xhtml: &mut Xhtml, class: &str, url: &str) -> Result<(), Stop> {
    if url.is_empty() {
        return Ok(());
    }
    let class = format!("external-ref-{class}");
    xhtml.start_element_with("a", &[("class", &class), ("href", url)])?;
    xhtml.end_element("a")
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
