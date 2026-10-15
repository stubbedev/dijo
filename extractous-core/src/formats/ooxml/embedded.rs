//! What Tika's `AbstractOOXMLExtractor` writes of a package beyond its text: the links of a part
//! its text handler resolves, the documents its main parts embed, OLE objects opened as POI opens
//! them, parsed as embedded documents, the references they make to what lies outside the package,
//! and a marker for its thumbnail.

use std::collections::{HashMap, HashSet};

use super::package::{Package, Relationship, Unported, HYPERLINK_RELATIONSHIP};
use crate::formats::embedded::{self, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole10_native::{self, Ole10Native, Unread};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;
use crate::mime::{is_ole10_native, Lookup, OleFile};

/// Relationship types, as POI's `PackageRelationshipTypes` and Tika name them.
pub(crate) mod kind {
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
    pub(crate) const OFFICE_DOCUMENT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    pub(crate) const DIAGRAM_DATA: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData";
    pub(crate) const ALTERNATE_FORMAT_CHUNK: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/aFChunk";
    pub(crate) const VBA_PROJECT: &str =
        "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
    pub(crate) const THUMBNAIL: &str =
        "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";
}

/// The content type of an embedded OLE object.
pub(crate) const OLE_OBJECT_TYPE: &str = "application/vnd.openxmlformats-officedocument.oleObject";
/// The smallest OLE object Tika opens: three blocks of a compound file.
pub(crate) const SMALLEST_OLE_OBJECT: u64 = 512 * 3;
const HAS_EXTERNAL_OLE_OBJECTS: &str = "msoffice:has-external-ole-objects";
const RELATIONSHIP_ID: &str = "tk:embedded-relationship-id";
const ORIGINAL_RESOURCE_NAME: &str = "tk:orig-resource-name";
/// The stream of an OLE 1.0 object, `Ole10Native.OLE10_NATIVE`.
const OLE10_NATIVE: &str = "\u{1}Ole10Native";
const PAGE_NUMBER: &str = "tk:page:number";
const PAGE_NUMBERS: &str = "tk:page:numbers";

/// The kinds of relationships whose targets `loadLinkedRelationships` names by file.
const EMBEDDED_KINDS: [&str; 5] = [
    kind::AUDIO,
    kind::IMAGE,
    kind::PACKAGE,
    kind::OFFICE_DOCUMENT,
    kind::DIAGRAM_DATA,
];

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
    for kind in EMBEDDED_KINDS {
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

/// `emitExternalRef`: an empty link of class `external-ref-…`, unless the URL is empty.
pub(crate) fn external_reference(xhtml: &mut Xhtml, class: &str, url: &str) -> Result<(), Stop> {
    if url.is_empty() {
        return Ok(());
    }
    let class = format!("external-ref-{class}");
    xhtml.start_element_with("a", &[("class", &class), ("href", url)])?;
    xhtml.end_element("a")
}

/// How an embedded part is embedded, as its metadata names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceType {
    Inline,
    Attachment,
    AlternateFormatChunk,
}

impl ResourceType {
    fn name(self) -> &'static str {
        match self {
            Self::Inline => "INLINE",
            Self::Attachment => "ATTACHMENT",
            Self::AlternateFormatChunk => "ALTERNATE_FORMAT_CHUNK",
        }
    }
}

/// `getJustFileName` of a relationship's source, kept as the start of the relationship id of what
/// a slide, slide layout or slide master embeds.
fn source_description(source: &str) -> String {
    let name = source
        .rfind('/')
        .map_or(source, |index| &source[index + 1..]);
    let name = name.rfind('.').map_or(name, |index| &name[..index]);
    if name.starts_with("slide") {
        format!("{name}_")
    } else {
        String::new()
    }
}

/// `handleEmbeddedParts`: for each relationship of the `sources` in turn, a link for a target
/// outside the package, or else the target, or the document an OLE object holds, parsed as an
/// embedded document, each target once. `pages` gives the slides a picture is on, by part, where
/// the format has slides. The parts are named by their own names, as Tika names them when its
/// extractor knows no `EmbeddedPartMetadata` for them, as a presentation's does not.
pub(crate) fn write_embedded_parts(
    package: &Package,
    sources: &[usize],
    pages: &HashMap<usize, Vec<i32>>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let mut handled: HashSet<&str> = HashSet::new();
    for &source in sources {
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
                    xhtml.metadata_mut().set(HAS_EXTERNAL_OLE_OBJECTS, "true");
                }
                continue;
            }
            let Some(target) = relationship.part else {
                continue;
            };
            let id = format!(
                "{}{}",
                source_description(package.name(source)),
                relationship.id
            );
            let resource_type = match relationship.kind.as_str() {
                kind::OLE_OBJECT if package.parts[target].content_type == OLE_OBJECT_TYPE => {
                    write_ole_object(package, target, &id, pages, xhtml)?;
                    None
                }
                kind::IMAGE => Some(ResourceType::Inline),
                kind::MEDIA | kind::VIDEO | kind::AUDIO | kind::PACKAGE | kind::OLE_OBJECT => {
                    Some(ResourceType::Attachment)
                }
                kind::VBA_PROJECT => None,
                kind::ALTERNATE_FORMAT_CHUNK => Some(ResourceType::AlternateFormatChunk),
                _ => continue,
            };
            if let Some(resource_type) = resource_type {
                write_embedded_file(package, target, &id, resource_type, pages, xhtml)?;
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
    pages: &HashMap<usize, Vec<i32>>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let name = package.name(part);
    let mut metadata = part_metadata(id, resource_type, name);
    metadata.set(
        embedded::RESOURCE_NAME,
        name.rfind('/').map_or(name, |index| &name[index + 1..]),
    );
    metadata.set("Content-Type", package.parts[part].content_type.as_str());
    if let Some(pages) = pages.get(&part) {
        apply_page_numbers(&mut metadata, pages);
    }
    if !embedded::is_parsed(&metadata) {
        return Ok(());
    }
    let bytes = package.read(part)?;
    xhtml.parse_embedded(&bytes, &metadata, Wrapping::PackageEntry)
}

/// What Tika's metadata names of every part it parses as an embedded document.
fn part_metadata(id: &str, resource_type: ResourceType, name: &str) -> TikaMetadata {
    let mut metadata = TikaMetadata::new();
    metadata.set(RELATIONSHIP_ID, id);
    metadata.set(embedded::RESOURCE_TYPE, resource_type.name());
    metadata.set(embedded::INTERNAL_PATH, name);
    metadata
}

/// What `handleEmbeddedOLE` finds in an OLE object part.
#[derive(Debug)]
pub(crate) enum OleObject {
    /// Smaller than a compound document can be, or an OLE 1.0 object POI cannot read: nothing
    /// is parsed.
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
pub(crate) fn read_ole_object(package: &Package, part: usize) -> Result<OleObject, Unported> {
    if package.size(part) < SMALLEST_OLE_OBJECT {
        return Ok(OleObject::Skipped);
    }
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
    pages: &HashMap<usize, Vec<i32>>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let object =
        read_ole_object(package, part).map_err(|Unported(reason)| Failure::Malformed(reason))?;
    let mut metadata = part_metadata(id, ResourceType::Attachment, package.name(part));
    let data = match object {
        OleObject::Skipped => return Ok(()),
        OleObject::Whole => {
            return write_embedded_file(package, part, id, ResourceType::Attachment, pages, xhtml);
        }
        OleObject::Entry(data) => data,
        OleObject::Native(native) => {
            if let Some(label) = &native.label {
                metadata.set(embedded::RESOURCE_NAME, label);
            }
            for name in [native.command, native.file_name].into_iter().flatten() {
                metadata.add(ORIGINAL_RESOURCE_NAME, name);
            }
            native.data
        }
    };
    xhtml.parse_embedded(&data, &metadata, Wrapping::PackageEntry)
}

/// `PageAnchoring.applyPageMetadata`: the slides in order, and the slide when there is one.
fn apply_page_numbers(metadata: &mut TikaMetadata, pages: &[i32]) {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        sorted.push(-1);
    }
    for page in &sorted {
        metadata.add(PAGE_NUMBERS, page.to_string());
    }
    if let [page] = sorted[..] {
        if page != -1 {
            metadata.set(PAGE_NUMBER, page.to_string());
        }
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
    fn skips_objects_smaller_than_a_compound_document() {
        assert!(matches!(ole_object(&[0; 1535]), Ok(OleObject::Skipped)));
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
