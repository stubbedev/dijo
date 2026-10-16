//! What every Office Open XML extractor shares, from Tika's `OOXMLExtractorFactory` and
//! `AbstractOOXMLExtractor`: the package's signature and unreferenced parts, the links of a part
//! its text handler resolves, the text-holding parts it reads beside a main part, the
//! relationships of the main parts that embed documents (OLE objects opened as POI opens them)
//! or lead out of the package, and the thumbnail.

use std::collections::{HashMap, HashSet, VecDeque};

use super::package::{
    Package, Relationship, Unported, HYPERLINK_RELATIONSHIP, RELATIONSHIPS_CONTENT_TYPE,
};
use crate::config::OfficeParserConfig;
use crate::formats::embedded::{self, ResourceType, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole10_native::{self, Ole10Native, Unread};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::mime::{is_ole10_native, Lookup, OleFile};

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
    pub(crate) const ORIGINAL_RESOURCE_NAME: &str = "tk:orig-resource-name";
}

pub(crate) const OLE_OBJECT_TYPE: &str = "application/vnd.openxmlformats-officedocument.oleObject";
/// The smallest OLE object Tika opens: three blocks of a compound file.
const SMALLEST_OLE_OBJECT: u64 = 512 * 3;
/// The stream of an OLE 1.0 object, `Ole10Native.OLE10_NATIVE`.
const OLE10_NATIVE: &str = "\u{1}Ole10Native";

/// The kinds of relationships whose targets `loadLinkedRelationships` names by file.
const LINKED_FILE_KINDS: [&str; 5] = [
    kind::AUDIO,
    kind::IMAGE,
    kind::PACKAGE,
    kind::OFFICE_DOCUMENT,
    kind::DIAGRAM_DATA,
];

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

/// `loadLinkedRelationships`: the targets of a part's links by id, those inside the package only
/// when `include_internal`, and the file names of what it embeds.
pub(crate) fn linked_relationships(
    relationships: &[Relationship],
    include_internal: bool,
) -> HashMap<String, String> {
    let mut linked = HashMap::new();
    for relationship in Package::relationships_of(relationships, HYPERLINK_RELATIONSHIP) {
        if include_internal || !relationship.is_internal() {
            linked.insert(
                relationship.id.clone(),
                relationship.target_uri().to_owned(),
            );
        }
    }
    for kind in LINKED_FILE_KINDS {
        for relationship in Package::relationships_of(relationships, kind) {
            linked.insert(
                relationship.id.clone(),
                file_name(relationship.target_uri()),
            );
        }
    }
    linked
}

/// `FileHelper.getFilename(new File(uri))`: the last segment of the path `File` makes of it.
fn file_name(uri: &str) -> String {
    let path = uri.trim_end_matches('/');
    path.rfind('/')
        .map_or(path, |index| &path[index + 1..])
        .to_owned()
}

/// `handleGeneralTextContainingPart`: hands `handler` a `div` of class `class` around the parts
/// `parent`'s relationships of `kind` lead to, each read behind an `XHTMLBalancingHandler`. The
/// handlers Tika passes are no XHTML handlers, so the div is never written. The parts' XML is
/// expected to have been checked well-formed: a failure ends the extraction, after the elements
/// the part opened are ended.
pub(crate) fn write_general_text_parts<H: SaxHandler>(
    package: &Package,
    parent: usize,
    kind: &str,
    class: &str,
    handler: &mut H,
) -> Result<(), Failure> {
    let mut parts = Package::relationships_of(&package.parts[parent].relationships, kind)
        .map(|relationship| relationship.part)
        .peekable();
    if parts.peek().is_none() {
        return Ok(());
    }
    let div = Attribute {
        qualified_name: "class".to_owned(),
        namespace: String::new(),
        local_name: "class".to_owned(),
        value: class.to_owned(),
    };
    handler.start_element("", "div", &[div])?;
    for part in parts.flatten() {
        let mut balancer = Balancer {
            inner: &mut *handler,
            open: Vec::new(),
        };
        let bytes = package.read(part)?;
        if let Err(failure) = xml::parse_sax(&bytes, package.name(part), &mut balancer) {
            balancer.drain()?;
            return Err(failure);
        }
    }
    handler.end_element("", "div")
}

/// `XHTMLBalancingHandler` over the events of one part: what it opened, to end on a failure.
struct Balancer<'h, H> {
    inner: &'h mut H,
    open: Vec<(String, String)>,
}

impl<H: SaxHandler> Balancer<'_, H> {
    /// `drainOpenElements`.
    fn drain(&mut self) -> Result<(), Failure> {
        while let Some((namespace, local_name)) = self.open.pop() {
            self.inner.end_element(&namespace, &local_name)?;
        }
        Ok(())
    }
}

impl<H: SaxHandler> SaxHandler for Balancer<'_, H> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.open
            .push((namespace.to_owned(), local_name.to_owned()));
        self.inner.start_element(namespace, local_name, attributes)
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.open.pop();
        self.inner.end_element(namespace, local_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.inner.characters(text)
    }
}

/// What `handleEmbeddedPart` does with the part an internal relationship of a main part leads
/// to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Embedding {
    /// A file Tika parses as an embedded document of this resource type.
    File(ResourceType),
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
        kind::IMAGE => Embedding::File(ResourceType::Inline),
        kind::MEDIA | kind::VIDEO | kind::AUDIO | kind::PACKAGE | kind::OLE_OBJECT => {
            Embedding::File(ResourceType::Attachment)
        }
        kind::VBA_PROJECT => Embedding::Macros,
        kind::ALTERNATE_FORMAT_CHUNK => Embedding::File(ResourceType::AlternateFormatChunk),
        _ => return None,
    };
    Some((target, embedding))
}

/// What of `handleEmbeddedPart`'s work on one relationship of a main part this port does not
/// do: following an internal relationship POI names no part for, which Tika records failing on;
/// reading an OLE object Tika records failing to read; and extracting macros when asked to.
/// With `files`, for an extractor whose `EmbeddedPartMetadata` is not ported, also parsing an
/// embedded file or OLE object.
pub(crate) fn check_embedded(
    package: &Package,
    config: &OfficeParserConfig,
    source: usize,
    relationship: &Relationship,
    files: bool,
) -> Result<(), Unported> {
    if relationship.is_internal() && !relationship.names_a_part() {
        return Err(Unported(format!(
            "{} links to {} inside the package, which POI names no part for, a failure Tika \
             records with its stack trace",
            package.name(source),
            relationship.target
        )));
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
        read_ole_object(package, target)?;
    }
    Ok(())
}

/// `handleEmbeddedParts` for the relationships of the `main_parts`, once [`check_embedded`]
/// passed them all: each embedded file and the document each OLE object holds parsed, with the
/// metadata `anchor` adds to a file's, and a link for each relationship that leads out of the
/// package, each target once. A file is named by its part's name, as Tika names it when its
/// extractor knows no `EmbeddedPartMetadata` for it.
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
            let id = format!(
                "{}{}",
                source_description(package.name(source)),
                relationship.id
            );
            match embedding {
                Embedding::File(resource_type) => {
                    write_embedded_file(package, target, &id, resource_type, anchor, xhtml)?;
                }
                Embedding::OleObject => write_ole_object(package, target, &id, anchor, xhtml)?,
                Embedding::SmallOleObject | Embedding::Macros => {}
            }
            handled.insert(relationship.target_uri());
        }
    }
    Ok(())
}

/// `handleEmbeddedFile`: a part parsed as an embedded document, with the metadata Tika gives it.
fn write_embedded_file(
    package: &Package,
    part: usize,
    id: &str,
    resource_type: ResourceType,
    anchor: &dyn Fn(usize, &mut TikaMetadata),
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let name = package.name(part);
    let mut metadata = part_metadata(id, resource_type, name);
    metadata.set(
        embedded::RESOURCE_NAME,
        &name[name.rfind('/').map_or(0, |at| at + 1)..],
    );
    metadata.set("Content-Type", package.parts[part].content_type.clone());
    anchor(part, &mut metadata);
    xhtml.parse_embedded(&package.read(part)?, &metadata, Wrapping::PackageEntry)
}

/// What Tika's metadata names of every part it parses as an embedded document.
fn part_metadata(id: &str, resource_type: ResourceType, name: &str) -> TikaMetadata {
    let mut metadata = TikaMetadata::new();
    metadata.set(embedded::RELATIONSHIP_ID, id);
    resource_type.set(&mut metadata);
    metadata.set(embedded::INTERNAL_PATH, name);
    metadata
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

/// What `handleEmbeddedOLE` finds in an OLE object part large enough to open.
#[derive(Debug)]
enum OleObject {
    /// An OLE 1.0 object whose record POI rejects: nothing is parsed.
    Skipped,
    /// The stream of an OLE 2.0 object that holds its document.
    Entry(Vec<u8>),
    /// The record of an OLE 1.0 object.
    Native(Ole10Native),
    /// Any other compound document, parsed whole.
    Whole,
}

/// `handleEmbeddedOLE`'s reading of an OLE object part, or, as [`Unported`], the failure Tika
/// records in the embedding document's metadata instead.
fn read_ole_object(package: &Package, part: usize) -> Result<OleObject, Unported> {
    let name = package.name(part);
    let fails =
        |what: String| Unported(format!("Tika fails to read the OLE object {name}: {what}"));
    let bytes = package
        .read(part)
        .map_err(|_| fails("the part cannot be read".to_owned()))?;
    let file = OleFile::open(&bytes)
        .ok_or_else(|| fails("POI cannot open it as a compound document".to_owned()))?;
    if let Some(entry) = package_entry_name(&file) {
        return file
            .root_document(entry, Lookup::Exact)
            .map(OleObject::Entry)
            .map_err(fails);
    }
    if !is_ole10_native(&file) {
        return Ok(OleObject::Whole);
    }
    let record = file
        .root_document(OLE10_NATIVE, Lookup::IgnoringCase)
        .map_err(fails)?;
    if record.len() > ole10_native::MAX_RECORD_LENGTH as usize {
        return Err(fails(format!(
            "its record of {} bytes is longer than POI reads",
            record.len()
        )));
    }
    match Ole10Native::parse(&record) {
        Ok(native) => Ok(OleObject::Native(native)),
        Err(Unread::Invalid) => Ok(OleObject::Skipped),
        Err(Unread::Failed(message)) => Err(fails(message)),
    }
}

/// `getPackageEntryName`: the stream of an OLE 2.0 object that holds its document.
fn package_entry_name(file: &OleFile<'_>) -> Option<&'static str> {
    let names: &[&'static str] = if file.has_root_entry("\u{1}Ole") {
        &["Package", "CONTENTS", "package"]
    } else {
        &["package"]
    };
    names
        .iter()
        .copied()
        .find(|&name| file.has_root_entry(name))
}

/// `handleEmbeddedOLE`: the document an OLE object holds, parsed as an attachment.
fn write_ole_object(
    package: &Package,
    part: usize,
    id: &str,
    anchor: &dyn Fn(usize, &mut TikaMetadata),
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let object =
        read_ole_object(package, part).map_err(|Unported(reason)| Failure::Malformed(reason))?;
    let mut metadata = part_metadata(id, ResourceType::Attachment, package.name(part));
    let data = match object {
        OleObject::Skipped => return Ok(()),
        OleObject::Whole => {
            return write_embedded_file(package, part, id, ResourceType::Attachment, anchor, xhtml);
        }
        OleObject::Entry(data) => data,
        OleObject::Native(native) => {
            if let Some(label) = &native.label {
                metadata.set(embedded::RESOURCE_NAME, label);
            }
            for name in [native.command, native.file_name].into_iter().flatten() {
                metadata.add(name::ORIGINAL_RESOURCE_NAME, name);
            }
            native.data
        }
    };
    xhtml.parse_embedded(&data, &metadata, Wrapping::PackageEntry)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::ooxml::testing;
    use crate::mime::compound_document;

    /// What `read_ole_object` makes of an OLE object part of `content`.
    fn ole_object(content: &[u8]) -> Result<OleObject, Unported> {
        let input = testing::package(
            "main.xml",
            &[
                ("main.xml", "application/xml", b"<main/>"),
                ("oleObject1.bin", OLE_OBJECT_TYPE, content),
            ],
        );
        let package = Package::open(&input).unwrap();
        let [part] = package.parts_by_content_type(OLE_OBJECT_TYPE)[..] else {
            panic!("one OLE object");
        };
        read_ole_object(&package, part)
    }

    fn native_record(body: &[u8]) -> Vec<u8> {
        [&i32::try_from(body.len()).unwrap().to_le_bytes()[..], body].concat()
    }

    #[test]
    fn fails_as_tika_records_on_what_poi_cannot_open() {
        assert!(matches!(
            ole_object(&[0; 1536]),
            Err(Unported(reason)) if reason.contains("cannot open it as a compound document")
        ));
    }

    #[test]
    fn reads_the_stream_holding_an_ole2_objects_document() {
        let entry = |names: &[&str]| match ole_object(&compound_document(names, b"PK\x03\x04")) {
            Ok(OleObject::Entry(data)) => data,
            other => panic!("{names:?}: {other:?}"),
        };
        assert_eq!(entry(&["\u{1}Ole", "CONTENTS"]), b"PK\x03\x04");
        assert_eq!(entry(&["package"]), b"PK\x03\x04");
        assert!(matches!(
            ole_object(&compound_document(&["CONTENTS"], b"x")),
            Ok(OleObject::Whole)
        ));
        assert!(matches!(
            ole_object(&compound_document(&["Workbook", "\u{1}Ole"], b"x")),
            Ok(OleObject::Whole)
        ));
    }

    #[test]
    fn reads_the_record_of_an_ole1_object() {
        let mut body = vec![2, 0];
        body.extend_from_slice(b"a.txt\0C:\\a.txt\0\0\0\0\0");
        body.extend_from_slice(&1i32.to_le_bytes());
        body.push(0);
        body.extend_from_slice(&1i32.to_le_bytes());
        body.extend_from_slice(b"x\0\0");
        let content = compound_document(&["\u{1}Ole10Native"], &native_record(&body));
        let Ok(OleObject::Native(native)) = ole_object(&content) else {
            panic!("an OLE 1.0 object");
        };
        assert_eq!(native.label.as_deref(), Some("a.txt"));
        assert_eq!(native.file_name.as_deref(), Some("C:\\a.txt"));
        assert_eq!(native.command.as_deref(), Some(""));
        assert_eq!(native.data, b"x");
    }

    #[test]
    fn skips_ole1_records_poi_rejects_and_fails_on_those_it_fails_on() {
        let mut short = vec![2, 0];
        short.extend_from_slice(b"a\0b\0\0\0\0\0");
        short.extend_from_slice(&0i32.to_le_bytes());
        short.extend_from_slice(&9i32.to_le_bytes());
        short.push(b'x');
        let content = compound_document(&["\u{1}Ole10Native"], &native_record(&short));
        assert!(matches!(ole_object(&content), Ok(OleObject::Skipped)));
        let content = compound_document(&["\u{1}Ole10Native"], &0i32.to_le_bytes());
        assert!(matches!(
            ole_object(&content),
            Err(Unported(reason)) if reason.contains("Buffer overrun")
        ));
    }
}
