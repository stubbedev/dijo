//! An Office Open XML package as POI's `OPCPackage` opens it: the parts of a ZIP archive with the
//! content types `[Content_Types].xml` gives them, ordered as POI orders part names, and the
//! relationships of the package and of each part. A package POI would open in a degraded way or
//! refuse, or one using what this port does not read, is [`Unported`].

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::uri::{self, Resolution};
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::zip_file::central_directory_records;
use crate::formats::Failure;
pub(crate) use crate::formats::Unported;
use crate::java;

const CONTENT_TYPES: &str = "[Content_Types].xml";
const TYPES_NAMESPACE: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const RELATIONSHIPS_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";
pub(crate) const RELATIONSHIPS_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-package.relationships+xml";
pub(crate) const CORE_PROPERTIES_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-package.core-properties+xml";
const CORE_PROPERTIES_RELATIONSHIP: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
/// The package's relationship to custom XML, which Tika never resolves but in its search for
/// unreferenced parts, where it passes over a target POI cannot name.
const CUSTOM_XML_RELATIONSHIP: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
pub(crate) const HYPERLINK_RELATIONSHIP: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
/// POI's `ZipSecureFile` limit on the entries of an archive, as Tika raises it.
const MAX_ENTRIES: usize = 10_000;

/// Where a relationship's target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetMode {
    Internal,
    External,
}

/// A relationship of the package or of a part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Relationship {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) mode: TargetMode,
    /// The target as POI's `toURI` leaves it, or [`uri::INVALID_TARGET`].
    pub(crate) target: String,
    /// The part name an internal relationship of another kind than a hyperlink resolves to.
    resolved: Option<String>,
    /// The part it leads to, if the package has it.
    pub(crate) part: Option<usize>,
}

impl Relationship {
    /// `getTargetURI().toString()`: an internal target other than a hyperlink is resolved.
    pub(crate) fn target_uri(&self) -> &str {
        self.resolved.as_deref().unwrap_or(&self.target)
    }

    /// Whether the target is in the package, as opposed to a URL or file elsewhere.
    pub(crate) fn is_internal(&self) -> bool {
        self.mode == TargetMode::Internal
    }

    /// Whether POI makes a part name of the target, as `getRelatedPart` does, which throws for
    /// one it cannot: an internal link to an anchor or a relative path, say.
    pub(crate) fn names_a_part(&self) -> bool {
        self.resolved.is_some() || uri::is_valid_part_name(&self.target)
    }
}

/// A part of the package.
#[derive(Debug)]
pub(crate) struct Part {
    /// The name, such as `/word/document.xml`.
    pub(crate) name: String,
    pub(crate) content_type: String,
    entry: usize,
    /// The uncompressed size the archive records.
    size: u64,
    /// The relationships from this part, by id.
    pub(crate) relationships: Vec<Relationship>,
}

/// An opened package.
pub(crate) struct Package<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    /// The parts in POI's order of part names, which is that of `getParts`.
    pub(crate) parts: Vec<Part>,
    /// The package's own relationships, by id.
    pub(crate) relationships: Vec<Relationship>,
}

impl<'a> Package<'a> {
    /// Opens `input` as `OPCPackage.open` does.
    pub(crate) fn open(input: &'a [u8]) -> Result<Self, Unported> {
        let mut archive = ZipArchive::new(Cursor::new(input))
            .map_err(|e| Unported(format!("the ZIP archive cannot be read as a whole: {e}")))?;
        if archive.len() > MAX_ENTRIES {
            return Err(Unported(format!(
                "the archive has {} entries, more than POI opens",
                archive.len()
            )));
        }
        let names = entry_names(&archive, input)?;
        let content_types_entry = names
            .iter()
            .position(|name| name == CONTENT_TYPES)
            .ok_or_else(|| Unported(format!("the package has no {CONTENT_TYPES}")))?;
        let content_types = ContentTypes::read(&read_entry(&mut archive, content_types_entry)?)?;
        let mut entries: Vec<(String, String, usize)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            if name.eq_ignore_ascii_case(CONTENT_TYPES)
                || name.starts_with("[trash]")
                || name.ends_with('/')
            {
                continue;
            }
            if !name.chars().all(uri::is_plain_name_character) {
                return Err(Unported(format!(
                    "the entry {name:?} has characters this port does not read part names of"
                )));
            }
            let part_name = if name.starts_with('/') {
                name.clone()
            } else {
                format!("/{name}")
            };
            if !uri::is_valid_part_name(&part_name) {
                continue;
            }
            let content_type = content_types
                .of(&part_name)
                .ok_or_else(|| Unported(format!("the part {part_name} has no content type")))?;
            if !is_simple_content_type(content_type) {
                return Err(Unported(format!(
                    "the part {part_name} has the content type {content_type:?}, which this \
                     port does not read"
                )));
            }
            entries.push((part_name, content_type.to_owned(), index));
        }
        entries.sort_by(|(a, ..), (b, ..)| compare_names(a, b));
        let mut parts: Vec<Part> = Vec::with_capacity(entries.len());
        let mut by_name: HashMap<String, usize> = HashMap::with_capacity(entries.len());
        for (name, content_type, entry) in entries {
            let key = name.to_ascii_lowercase();
            if by_name.contains_key(&key) {
                return Err(Unported(format!("the package has two parts named {name}")));
            }
            by_name.insert(key, parts.len());
            let size = archive
                .by_index_raw(entry)
                .map_err(|e| Unported(format!("the ZIP entry of {name} cannot be read: {e}")))?
                .size();
            parts.push(Part {
                name,
                content_type,
                entry,
                size,
                relationships: Vec::new(),
            });
        }
        let mut package = Package {
            archive,
            parts,
            relationships: Vec::new(),
        };
        package.relationships = package.read_relationships("/", &by_name)?;
        for index in 0..package.parts.len() {
            if is_relationship_part(&package.parts[index].name) {
                continue;
            }
            let name = package.parts[index].name.clone();
            package.parts[index].relationships = package.read_relationships(&name, &by_name)?;
        }
        Ok(package)
    }

    /// The bytes of `part`.
    pub(crate) fn read(&self, part: usize) -> Result<Vec<u8>, Failure> {
        read_entry(&mut self.archive.clone(), self.parts[part].entry)
            .map_err(|Unported(reason)| Failure::Malformed(reason))
    }

    /// The uncompressed size of `part`.
    pub(crate) fn size(&self, part: usize) -> u64 {
        self.parts[part].size
    }

    /// The parts with this content type, in order of their names.
    pub(crate) fn parts_by_content_type(&self, content_type: &str) -> Vec<usize> {
        (0..self.parts.len())
            .filter(|&part| self.parts[part].content_type == content_type)
            .collect()
    }

    /// The relationships of the package of `kind`, by id.
    pub(crate) fn relationships_of<'r>(
        relationships: &'r [Relationship],
        kind: &'r str,
    ) -> impl Iterator<Item = &'r Relationship> + 'r {
        relationships
            .iter()
            .filter(move |relationship| relationship.kind == kind)
    }

    /// `OPCPackage.getPart(PackageRelationship)`: the part of the first of the package's
    /// relationships of the same kind.
    pub(crate) fn package_part(&self, relationship: &Relationship) -> Option<usize> {
        Self::relationships_of(&self.relationships, &relationship.kind)
            .next()
            .and_then(|first| first.part)
    }

    /// The first part `relationships` lead to with a relationship of `kind`, as Tika takes it
    /// with `safeGetRelatedPart(getRelationshipsByType(kind).getRelationship(0))`.
    pub(crate) fn first_related(relationships: &[Relationship], kind: &str) -> Option<usize> {
        Self::relationships_of(relationships, kind)
            .next()
            .and_then(|relationship| relationship.part)
    }

    /// The parts `relationships` of `kind` lead to, skipping those that lead nowhere.
    pub(crate) fn related(relationships: &[Relationship], kind: &str) -> Vec<usize> {
        Self::relationships_of(relationships, kind)
            .filter_map(|relationship| relationship.part)
            .collect()
    }

    /// Reads the relationships of the part named `source`, or of the package for `/`.
    fn read_relationships(
        &mut self,
        source: &str,
        by_name: &HashMap<String, usize>,
    ) -> Result<Vec<Relationship>, Unported> {
        let rels_name = relationship_part_name(source);
        let Some(&rels_part) = by_name.get(&rels_name.to_ascii_lowercase()) else {
            return Ok(Vec::new());
        };
        let bytes = read_entry(&mut self.archive, self.parts[rels_part].entry)?;
        let mut reader = RelationshipsReader::default();
        xml::parse_sax(&bytes, &rels_name, &mut reader)
            .map_err(|e| Unported(format!("POI fails on {rels_name}: {}", describe(&e))))?;
        if reader
            .found
            .iter()
            .filter(|found| found.kind == CORE_PROPERTIES_RELATIONSHIP)
            .count()
            > 1
        {
            return Err(Unported(format!(
                "{rels_name} has more than one core properties relationship, which POI refuses"
            )));
        }
        let mut by_id: Vec<Relationship> = Vec::with_capacity(reader.found.len());
        let mut next_id: Option<usize> = None;
        for found in reader.found {
            let id = if found.id.is_empty() {
                let mut next = next_id.unwrap_or(by_id.len() + 1);
                let mut id = format!("rId{next}");
                while by_id.iter().any(|relationship| relationship.id == id) {
                    next += 1;
                    id = format!("rId{next}");
                }
                next_id = Some(next + 1);
                id
            } else {
                found.id
            };
            let target = uri::to_uri(&found.target);
            let mode = found.mode;
            let resolved = if mode == TargetMode::Internal && found.kind != HYPERLINK_RELATIONSHIP {
                match uri::resolve(source, target.as_deref()) {
                    Resolution::Name(name) => Some(name),
                    Resolution::Fails if source == "/" && found.kind == CUSTOM_XML_RELATIONSHIP => {
                        None
                    }
                    Resolution::Fails | Resolution::Unported => {
                        return Err(Unported(format!(
                            "the relationship {id} of {source} has the target {:?}, which this \
                             port does not resolve as POI does",
                            found.target
                        )))
                    }
                }
            } else {
                None
            };
            let relationship = Relationship {
                id,
                kind: found.kind,
                mode,
                target: target.unwrap_or_else(|| uri::INVALID_TARGET.to_owned()),
                part: resolved
                    .as_ref()
                    .and_then(|name| by_name.get(&name.to_ascii_lowercase()).copied()),
                resolved,
            };
            match by_id.iter_mut().find(|known| known.id == relationship.id) {
                Some(known) => *known = relationship,
                None => by_id.push(relationship),
            }
        }
        by_id.sort_by(|a, b| java::compare(&a.id, &b.id));
        Ok(by_id)
    }

    /// Orders the relationships of every part by id with runs of digits compared as numbers,
    /// `rId2` before `rId10`, where POI orders them as strings (#102).
    pub(crate) fn order_relationships_numerically(&mut self) {
        for part in &mut self.parts {
            part.relationships.sort_by(|a, b| {
                compare_names(&a.id, &b.id).then_with(|| java::compare(&a.id, &b.id))
            });
        }
    }

    /// The name of `part`, for messages.
    pub(crate) fn name(&self, part: usize) -> &str {
        &self.parts[part].name
    }
}

fn describe(failure: &Failure) -> String {
    match failure {
        Failure::Malformed(reason) => reason.clone(),
        Failure::Stopped(stop) => format!("{stop:?}"),
        Failure::Extraction(error) | Failure::Embedded(error) => error.to_string(),
    }
}

/// The names of the entries in the order of the central directory, which must list each name
/// once and start the archive: POI's ZIP reader resolves neither duplicates nor prefixed data as
/// the `zip` crate does.
fn entry_names(archive: &ZipArchive<Cursor<&[u8]>>, input: &[u8]) -> Result<Vec<String>, Unported> {
    if archive.offset() != 0 {
        return Err(Unported("the archive is preceded by other data".to_owned()));
    }
    if central_directory_records(input, archive.central_directory_start()) != Some(archive.len()) {
        return Err(Unported(
            "the archive lists entries with the same name".to_owned(),
        ));
    }
    archive
        .file_names()
        .map(|name| {
            name.map(|name| name.into_owned())
                .map_err(|e| Unported(format!("an entry name cannot be read: {e}")))
        })
        .collect()
}

fn read_entry(archive: &mut ZipArchive<Cursor<&[u8]>>, index: usize) -> Result<Vec<u8>, Unported> {
    let mut file = archive
        .by_index(index)
        .map_err(|e| Unported(format!("the ZIP entry {index} cannot be read: {e}")))?;
    let mut bytes = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .map_err(|e| Unported(format!("the ZIP entry {index} cannot be inflated: {e}")))?;
    Ok(bytes)
}

/// `PackagingURIHelper.getRelationshipPartName`.
fn relationship_part_name(source: &str) -> String {
    if source == "/" {
        return "/_rels/.rels".to_owned();
    }
    let split = source.rfind('/').map_or(0, |index| index + 1);
    format!("{}_rels/{}.rels", &source[..split], &source[split..])
}

/// `PackagingURIHelper.isRelationshipPartURI`: the path matches `.*_rels.*.rels$`.
fn is_relationship_part(name: &str) -> bool {
    name.find("_rels")
        .map(|index| &name[index + "_rels".len()..])
        .is_some_and(|tail| tail.len() >= ".rels".len() && tail.ends_with("rels"))
}

/// A content type POI reads as `type/subtype`, without parameters.
fn is_simple_content_type(content_type: &str) -> bool {
    let token = |text: &str| {
        !text.is_empty()
            && text.bytes().all(|byte| {
                (0x21..=0x7e).contains(&byte) && !b"()<>@,;:\\/\"[]?={}".contains(&byte)
            })
    };
    content_type
        .split_once('/')
        .is_some_and(|(kind, subtype)| token(kind) && token(subtype))
}

/// `PackagePartName.compare`: case-insensitive, with runs of digits compared as numbers.
fn compare_names(a: &str, b: &str) -> Ordering {
    if a.eq_ignore_ascii_case(b) {
        return Ordering::Equal;
    }
    let a: Vec<u8> = a.bytes().map(|byte| byte.to_ascii_lowercase()).collect();
    let b: Vec<u8> = b.bytes().map(|byte| byte.to_ascii_lowercase()).collect();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (c1, c2) = (a[i], b[j]);
        i += 1;
        j += 1;
        if c1.is_ascii_digit() && c2.is_ascii_digit() {
            let start1 = i - 1;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let start2 = j - 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let order = compare_numbers(&a[start1..i], &b[start2..j]);
            if order != Ordering::Equal {
                return order;
            }
        } else if c1 != c2 {
            return c1.cmp(&c2);
        }
    }
    a.len().cmp(&b.len())
}

fn compare_numbers(a: &[u8], b: &[u8]) -> Ordering {
    let strip = |digits: &[u8]| -> usize { digits.iter().take_while(|&&d| d == b'0').count() };
    let (a, b) = (&a[strip(a)..], &b[strip(b)..]);
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// The content types of `[Content_Types].xml`.
struct ContentTypes {
    /// By extension, in lowercase.
    defaults: HashMap<String, String>,
    /// By part name, in lowercase.
    overrides: HashMap<String, String>,
}

impl ContentTypes {
    fn read(bytes: &[u8]) -> Result<Self, Unported> {
        let mut reader = ContentTypesReader::default();
        xml::parse_sax(bytes, CONTENT_TYPES, &mut reader)
            .map_err(|e| Unported(format!("POI fails on {CONTENT_TYPES}: {}", describe(&e))))?;
        let mut defaults = HashMap::new();
        let mut overrides = HashMap::new();
        for declared in reader.found {
            let (name, content_type) = match declared {
                DeclaredType::Default {
                    extension,
                    content_type,
                } => {
                    defaults.insert(extension.to_lowercase(), content_type);
                    continue;
                }
                DeclaredType::Override {
                    part_name,
                    content_type,
                } => (part_name, content_type),
            };
            let name = name.as_str();
            if !name.chars().all(uri::is_plain_name_character) || !uri::is_valid_part_name(name) {
                return Err(Unported(format!(
                    "{CONTENT_TYPES} overrides the content type of {name:?}, a part name this \
                     port does not read"
                )));
            }
            overrides.insert(name.to_ascii_lowercase(), content_type);
        }
        Ok(Self {
            defaults,
            overrides,
        })
    }

    /// `ContentTypeManager.getContentType`: the override, else the default of the extension.
    fn of(&self, part_name: &str) -> Option<&str> {
        if let Some(content_type) = self.overrides.get(&part_name.to_ascii_lowercase()) {
            return Some(content_type);
        }
        let extension = part_name
            .rfind('.')
            .map_or("", |index| &part_name[index + 1..]);
        self.defaults
            .get(&extension.to_lowercase())
            .map(String::as_str)
    }
}

/// A content type `[Content_Types].xml` declares.
enum DeclaredType {
    Default {
        extension: String,
        content_type: String,
    },
    Override {
        part_name: String,
        content_type: String,
    },
}

/// The `Default` and `Override` elements below the root, as DOM's `getElementsByTagNameNS` on
/// the document element finds them; their attributes as `getAttribute` reads them.
#[derive(Default)]
struct ContentTypesReader {
    depth: usize,
    found: Vec<DeclaredType>,
}

/// `Element.getAttribute`: the value of the attribute written `name`, else nothing.
fn dom_attribute(attributes: &[Attribute], name: &str) -> String {
    attribute_named(attributes, name)
        .unwrap_or_default()
        .to_owned()
}

/// POI's `DocumentHelper` refuses document type declarations.
pub(crate) fn refuse_document_type() -> Result<(), Failure> {
    Err(Failure::Malformed(
        "declares a document type, which POI refuses".to_owned(),
    ))
}

impl SaxHandler for ContentTypesReader {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if self.depth > 0 && namespace == TYPES_NAMESPACE {
            let content_type = dom_attribute(attributes, "ContentType");
            match local_name {
                "Default" => self.found.push(DeclaredType::Default {
                    extension: dom_attribute(attributes, "Extension"),
                    content_type,
                }),
                "Override" => self.found.push(DeclaredType::Override {
                    part_name: dom_attribute(attributes, "PartName"),
                    content_type,
                }),
                _ => {}
            }
        }
        self.depth += 1;
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        self.depth -= 1;
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn document_type(&mut self) -> Result<(), Failure> {
        refuse_document_type()
    }
}

/// A relationship as a relationships part declares it.
struct Declared {
    id: String,
    kind: String,
    mode: TargetMode,
    target: String,
}

#[derive(Default)]
struct RelationshipsReader {
    depth: usize,
    found: Vec<Declared>,
}

impl SaxHandler for RelationshipsReader {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if self.depth > 0 && namespace == RELATIONSHIPS_NAMESPACE && local_name == "Relationship" {
            let mode = match attribute_named(attributes, "TargetMode") {
                Some(mode) if mode.to_lowercase() != "internal" => TargetMode::External,
                _ => TargetMode::Internal,
            };
            self.found.push(Declared {
                id: dom_attribute(attributes, "Id"),
                kind: dom_attribute(attributes, "Type"),
                mode,
                target: dom_attribute(attributes, "Target"),
            });
        }
        self.depth += 1;
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        self.depth -= 1;
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn document_type(&mut self) -> Result<(), Failure> {
        refuse_document_type()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_part_names_as_poi_does() {
        let mut names = vec![
            "/word/media/image10.png",
            "/word/media/Image2.png",
            "/a.xml",
        ];
        names.sort_by(|a, b| compare_names(a, b));
        assert_eq!(
            names,
            [
                "/a.xml",
                "/word/media/Image2.png",
                "/word/media/image10.png"
            ]
        );
        assert_eq!(compare_names("/A.xml", "/a.xml"), Ordering::Equal);
    }

    #[test]
    fn names_relationship_parts_as_poi_does() {
        assert_eq!(relationship_part_name("/"), "/_rels/.rels");
        assert_eq!(
            relationship_part_name("/word/document.xml"),
            "/word/_rels/document.xml.rels"
        );
        assert!(is_relationship_part("/word/_rels/document.xml.rels"));
        assert!(!is_relationship_part("/word/document.xml"));
    }

    #[test]
    fn reads_simple_content_types_only() {
        assert!(is_simple_content_type("application/xml"));
        assert!(!is_simple_content_type("application/xml; charset=utf-8"));
        assert!(!is_simple_content_type("application"));
    }
}
