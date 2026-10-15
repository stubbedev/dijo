//! Tika's `OPCPackageDetector`: Office Open XML packages, named by the content type of the part
//! their package relationship calls the office document, as POI's `OPCPackage` opens them, or,
//! where it cannot, by the first known content type `[Content_Types].xml` lists.
//!
//! POI's checks are reproduced as far as they decide whether a package opens: the content
//! types, the part names, the relationships of every part, and the core properties' compliance
//! rules and dates. Core property dates are judged by the leading `number-number-number` that
//! `SimpleDateFormat`'s lenient parse needs, not by a port of it.

use std::collections::{BTreeMap, HashMap};

use crate::compress::zip::file::{ZipEntry, ZipFile};
use crate::compress::zip::Content;
use crate::mime::java::{self, JavaString};
use crate::mime::media_type::MediaType;
use crate::mime::uri;
use crate::xml::{self, Event, Flow, XMLNS};

const CONTENT_TYPES: &str = "[Content_Types].xml";
const TYPES_NAMESPACE: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const RELATIONSHIPS_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";
const CORE_PROPERTIES_PART: &str = "application/vnd.openxmlformats-package.core-properties+xml";
const CORE_PROPERTIES_RELATIONSHIP: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const CORE_PROPERTIES_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const MARKUP_COMPATIBILITY: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const HYPERLINK: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

/// `OOXML_CONTENT_TYPES`: the main part content types Tika knows, and the type each names.
const OOXML_CONTENT_TYPES: &[(&str, &str)] = &[
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "application/vnd.ms-word.document.macroEnabled.main+xml",
        "application/vnd.ms-word.document.macroEnabled.12",
    ),
    (
        "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
        "application/vnd.ms-word.template.macroenabled.12",
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
        "application/vnd.ms-excel.sheet.macroEnabled.12",
    ),
    (
        "application/vnd.ms-excel.sheet.binary.macroEnabled.main",
        "application/vnd.ms-excel.sheet.binary.macroenabled.12",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
    ),
    (
        "application/vnd.ms-excel.template.macroEnabled.main+xml",
        "application/vnd.ms-excel.template.macroenabled.12",
    ),
    (
        "application/vnd.ms-excel.addin.macroEnabled.main+xml",
        "application/vnd.ms-excel.addin.macroEnabled.12",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    (
        "application/vnd.ms-powerpoint.slideshow.macroEnabled.main+xml",
        "application/vnd.ms-powerpoint.slideshow.macroEnabled.12",
    ),
    (
        "application/vnd.ms-powerpoint.template.macroEnabled.main+xml",
        "application/vnd.ms-powerpoint.template.macroenabled.12",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.slideshow.main+xml",
        "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
    ),
    (
        "application/vnd.ms-powerpoint.presentation.macroEnabled.main+xml",
        "application/vnd.ms-powerpoint.presentation.macroEnabled.12",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.template.main+xml",
        "application/vnd.openxmlformats-officedocument.presentationml.template",
    ),
    (
        "application/vnd.openxmlformats-officedocument.themeManager+xml",
        "application/vnd.openxmlformats-officedocument",
    ),
    (
        "application/vnd.ms-visio.drawing.macroEnabled.main+xml",
        "application/vnd.ms-visio.drawing.macroEnabled.12",
    ),
    (
        "application/vnd.ms-visio.drawing.main+xml",
        "application/vnd.ms-visio.drawing",
    ),
    (
        "application/vnd.ms-visio.stencil.macroEnabled.main+xml",
        "application/vnd.ms-visio.stencil.macroenabled.12",
    ),
    (
        "application/vnd.ms-visio.stencil.main+xml",
        "application/vnd.ms-visio.stencil",
    ),
    (
        "application/vnd.ms-visio.template.macroEnabled.main+xml",
        "application/vnd.ms-visio.template.macroenabled.12",
    ),
    (
        "application/vnd.ms-visio.template.main+xml",
        "application/vnd.ms-visio.template",
    ),
    (
        "application/vnd.ms-package.xps-fixeddocumentsequence+xml",
        "application/vnd.ms-xpsdocument",
    ),
];

/// The streaming detector's hints: entries an OOXML package usually has.
pub(crate) const OOXML_HINTS: &[&str] = &[
    "word/",
    "xl/",
    "ppt/",
    "docProps/",
    "word/document.xml",
    "_rels/.rels",
    "[Content_Types].xml",
    "ppt/presentation.xml",
    "ppt/slides/slide1.xml",
    "xl/workbook.xml",
    "xl/sharedStrings.xml",
    "xl/worksheets/sheet1.xml",
    "docProps/core.xml",
    "docProps/app.xml",
    "xl/styles.xml",
    "word/styles.xml",
    "word/fontTable.xml",
];

fn ooxml_type(content_type: &str) -> Option<MediaType> {
    OOXML_CONTENT_TYPES
        .iter()
        .find(|(known, _)| *known == content_type)
        .map(|(_, kind)| MediaType::named(kind))
}

/// `parseOOXMLContentTypes`: the type of the first `ContentType` attribute Tika knows, read
/// until the content types fail to parse.
pub(crate) fn content_types_type(content: &[u8]) -> Option<MediaType> {
    let mut found = None;
    let _ = xml::parse(content, &mut |event| {
        if let Event::Start(_, attributes) = event {
            for attribute in attributes.iter().filter(|a| a.namespace != XMLNS) {
                if attribute.local_name == "ContentType" {
                    if let Some(kind) = ooxml_type(&attribute.value) {
                        found = Some(kind);
                        return Flow::Stop;
                    }
                }
            }
        }
        Flow::Continue
    });
    found
}

/// A part name POI accepts: its `toASCIIString`, which part names compare by ignoring case,
/// and its decoded path.
#[derive(Debug, Clone)]
struct PartName {
    ascii: String,
    path: String,
}

impl PartName {
    fn key(&self) -> String {
        self.ascii.to_ascii_lowercase()
    }

    /// `getExtension`.
    fn extension(&self) -> &str {
        self.path.rfind('.').map_or("", |dot| &self.path[dot + 1..])
    }

    /// `isRelationshipPartURI`: `^.*/_rels/.*\.rels$`.
    fn is_relationship(&self) -> bool {
        self.path.find("/_rels/").is_some_and(|at| {
            let rest = &self.path[at + 7..];
            self.path.ends_with(".rels")
                && !self.path[..at].contains(java::is_line_terminator)
                && rest.len() >= 5
                && !rest.contains(java::is_line_terminator)
        })
    }
}

fn is_pchar(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:@!$&'()*+,;=".contains(c)
}

/// `new PackagePartName(new URI(text), true)`, `None` where POI throws.
fn part_name(text: &str) -> Option<PartName> {
    let parsed = uri::parse(text)?;
    if parsed.scheme.is_some() {
        return None;
    }
    let path = uri::decode(parsed.path?);
    if path.is_empty() || path == "/" || !path.starts_with('/') || path.ends_with('/') {
        return None;
    }
    let ascii = uri::to_ascii_string(text);
    let relative = ascii.strip_prefix('/').unwrap_or(&ascii);
    let mut segments: Vec<&str> = relative.split('/').collect();
    while segments.len() > 1 && segments.last() == Some(&"") {
        segments.pop();
    }
    for segment in segments {
        if segment.is_empty() || segment.ends_with('.') || only_escaped_pairs(segment) {
            return None;
        }
        let bytes = segment.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = char::from(bytes[i]);
            if is_pchar(c) {
                i += 1;
                continue;
            }
            if c != '%' || bytes.len() - i < 3 {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let decoded = char::from(u8::from_str_radix(hex, 16).ok()?);
            if decoded == '/'
                || decoded == '\\'
                || decoded.is_ascii_alphanumeric()
                || "-._~".contains(decoded)
            {
                return None;
            }
            i += 3;
        }
    }
    Some(PartName { ascii, path })
}

/// `seg.replaceAll("\\\\.", "").isEmpty()`: the segment is nothing but backslashes each
/// followed by a character.
fn only_escaped_pairs(segment: &str) -> bool {
    let mut chars = segment.chars();
    while let Some(c) = chars.next() {
        if c != '\\' || chars.next().is_none_or(java::is_line_terminator) {
            return false;
        }
    }
    true
}

/// `PackagingURIHelper.toURI`'s rewriting of a string before it is parsed as a URI.
fn to_uri_text(value: &str) -> String {
    let encode = |text: &str| -> String {
        text.chars()
            .flat_map(|c| {
                if u32::from(c) >= 0x80 || c == '|' || is_java_whitespace(c) {
                    let mut buffer = [0u8; 4];
                    c.encode_utf8(&mut buffer)
                        .bytes()
                        .map(|b| format!("%{b:02X}"))
                        .collect::<Vec<_>>()
                } else {
                    vec![c.to_string()]
                }
            })
            .collect()
    };
    let mut value = value.replace('\\', "/");
    if let Some(hash) = value.find('#') {
        value = format!("{}#{}", &value[..hash], encode(&value[hash + 1..]));
    }
    let trimmed = value.trim_end_matches(|c: char| is_java_whitespace(c) || c == '\u{A0}');
    if trimmed.len() < value.len() {
        value = format!("{trimmed}{}", encode(&value[trimmed.len()..]));
    }
    let scheme_only = value.strip_suffix("://").is_some_and(|scheme| {
        !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphabetic())
    });
    if scheme_only {
        value.push('/');
    }
    value
}

/// `Character.isWhitespace`.
fn is_java_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{1C}'..='\u{1F}'
    ) || (c.is_whitespace() && !matches!(c, '\u{A0}' | '\u{2007}' | '\u{202F}'))
}

/// A relationship of the package or a part.
#[derive(Debug, Clone)]
struct Relationship {
    kind: String,
    /// The target as POI's `toURI` parses it, `None` where it falls back to its dummy URI.
    target: Option<String>,
    external: bool,
}

/// `PackageRelationshipCollection.parseRelationshipsPart`, by id, or `None` where it throws.
fn parse_relationships(content: &[u8]) -> Option<BTreeMap<JavaString, Relationship>> {
    let mut parsed: Vec<(String, Relationship)> = Vec::new();
    let mut depth = 0usize;
    let mut core_properties = false;
    let mut failed = false;
    let result = xml::parse(content, &mut |event| {
        match event {
            Event::Start(name, attributes) => {
                depth += 1;
                if depth > 1
                    && name.namespace == RELATIONSHIPS_NAMESPACE
                    && name.local_name == "Relationship"
                {
                    let attribute = |qname: &str| {
                        attributes
                            .iter()
                            .find(|a| a.qualified_name == qname)
                            .map(|a| a.value.clone())
                    };
                    let kind = attribute("Type").unwrap_or_default();
                    if kind == CORE_PROPERTIES_RELATIONSHIP {
                        if core_properties {
                            failed = true;
                            return Flow::Stop;
                        }
                        core_properties = true;
                    }
                    let external = attribute("TargetMode")
                        .is_some_and(|mode| java::to_lower_case(&mode) != "internal");
                    let text = to_uri_text(&attribute("Target").unwrap_or_default());
                    let target = uri::parse(&text).is_some().then_some(text);
                    parsed.push((
                        attribute("Id").unwrap_or_default(),
                        Relationship {
                            kind,
                            target,
                            external,
                        },
                    ));
                }
            }
            Event::End(_) => depth -= 1,
            Event::Text(..) | Event::DocumentType => {}
        }
        Flow::Continue
    });
    if failed || result.is_err() {
        return None;
    }
    let mut by_id: BTreeMap<JavaString, Relationship> = BTreeMap::new();
    for (id, relationship) in parsed {
        let id = if id.is_empty() {
            let mut next = by_id.len() + 1;
            loop {
                let candidate = format!("rId{next}");
                next += 1;
                if !by_id.contains_key(&JavaString(candidate.clone())) {
                    break candidate;
                }
            }
        } else {
            id
        };
        by_id.insert(JavaString(id), relationship);
    }
    Some(by_id)
}

/// The parts of a package POI opened.
struct Package {
    parts: HashMap<String, (PartName, String)>,
    relationships: BTreeMap<JavaString, Relationship>,
}

/// Why `OPCPackage.open` fails, which leaves Tika the content types to go by.
struct Invalid;

/// `PackagingURIHelper.getRelationshipPartName`.
fn relationship_part_name(part: &PartName) -> Option<PartName> {
    let file = part.path.rsplit('/').next().unwrap_or("");
    let directory = &part.path[..part.path.len() - file.len()];
    part_name(&to_uri_text(&format!("{directory}_rels/{file}.rels")))
}

fn read_entry(zip: &ZipFile<'_>, entry: &ZipEntry) -> Option<Content> {
    zip.read(entry)
}

/// The part names of the content types' overrides and defaults, or `Invalid`.
struct ContentTypes {
    defaults: HashMap<String, String>,
    overrides: HashMap<String, String>,
}

fn parse_content_types(content: &[u8]) -> Result<ContentTypes, Invalid> {
    let mut types = ContentTypes {
        defaults: HashMap::new(),
        overrides: HashMap::new(),
    };
    let mut depth = 0usize;
    let mut invalid = false;
    let result = xml::parse(content, &mut |event| {
        match event {
            Event::Start(name, attributes) => {
                depth += 1;
                let attribute = |qname: &str| {
                    attributes
                        .iter()
                        .find(|a| a.qualified_name == qname)
                        .map_or(String::new(), |a| a.value.clone())
                };
                if depth > 1 && name.namespace == TYPES_NAMESPACE {
                    if name.local_name == "Default" {
                        types.defaults.insert(
                            java::to_lower_case(&attribute("Extension")),
                            attribute("ContentType"),
                        );
                    } else if name.local_name == "Override" {
                        match part_name(&attribute("PartName")) {
                            Some(part) => {
                                types.overrides.insert(part.key(), attribute("ContentType"));
                            }
                            None => {
                                invalid = true;
                                return Flow::Stop;
                            }
                        }
                    }
                }
            }
            Event::End(_) => depth -= 1,
            Event::Text(..) | Event::DocumentType => {}
        }
        Flow::Continue
    });
    if invalid || result.is_err() {
        return Err(Invalid);
    }
    Ok(types)
}

/// `OPCPackage.open(zipEntrySource)`.
fn open(zip: &ZipFile<'_>, content_types_entry: &ZipEntry) -> Result<Package, Invalid> {
    let content = read_entry(zip, content_types_entry).ok_or(Invalid)?;
    let types = parse_content_types(content.bytes())?;
    let mut parts: HashMap<String, (PartName, String, &ZipEntry)> = HashMap::new();
    for entry in zip.entries() {
        if entry.name.starts_with("[trash]") || entry.name.ends_with('/') {
            continue;
        }
        if entry.name.eq_ignore_ascii_case(CONTENT_TYPES) {
            continue;
        }
        let opc_name = if entry.name.starts_with('/') {
            entry.name.clone()
        } else {
            format!("/{}", entry.name)
        };
        let Some(part) = part_name(&to_uri_text(&opc_name)) else {
            continue;
        };
        let content_type = types
            .overrides
            .get(&part.key())
            .or_else(|| types.defaults.get(&java::to_lower_case(part.extension())))
            .cloned()
            .ok_or(Invalid)?;
        if parts.contains_key(&part.key()) {
            return Err(Invalid);
        }
        parts.insert(part.key(), (part, content_type, entry));
    }
    let mut keys: Vec<&String> = parts.keys().collect();
    keys.sort();
    for key in keys {
        let (part, content_type, entry) = &parts[key];
        if !part.is_relationship() {
            if let Some(relationships) = relationship_part_name(part) {
                if let Some((_, _, relationships_entry)) = parts.get(&relationships.key()) {
                    let content = read_entry(zip, relationships_entry).ok_or(Invalid)?;
                    if matches!(content, Content::Truncated(_)) {
                        return Err(Invalid);
                    }
                    parse_relationships(content.bytes()).ok_or(Invalid)?;
                }
            }
        }
        if content_type == CORE_PROPERTIES_PART {
            if let Some(Content::Complete(content)) = read_entry(zip, entry) {
                check_core_properties(&content)?;
            }
        }
    }
    let relationships = parts
        .values()
        .find(|(part, ..)| part.ascii.eq_ignore_ascii_case("/_rels/.rels"))
        .and_then(|(_, _, entry)| match read_entry(zip, entry) {
            Some(Content::Complete(content)) => parse_relationships(&content),
            _ => None,
        })
        .unwrap_or_default();
    Ok(Package {
        parts: parts
            .into_iter()
            .map(|(key, (part, content_type, _))| (key, (part, content_type)))
            .collect(),
        relationships,
    })
}

/// `PackagePropertiesUnmarshaller`'s compliance checks and date parsing, for core properties
/// that parse as XML.
fn check_core_properties(content: &[u8]) -> Result<(), Invalid> {
    let mut invalid = false;
    let mut elements: Vec<(String, String, String)> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let result = xml::parse(content, &mut |event| {
        match event {
            Event::Start(name, attributes) => {
                let mut sorted: Vec<&xml::Attribute> = attributes.iter().collect();
                sorted.sort_by(|a, b| java::compare(&a.qualified_name, &b.qualified_name));
                if let Some(first) = sorted.first() {
                    if first.namespace == XMLNS && first.value == MARKUP_COMPATIBILITY {
                        invalid = true;
                    }
                }
                if name.namespace == DCTERMS {
                    let prefix = name
                        .qualified_name
                        .split_once(':')
                        .map_or("null", |(p, _)| p);
                    let typed = attributes
                        .iter()
                        .find(|a| a.namespace == XSI && a.local_name == "type")
                        .is_some_and(|a| a.value == format!("{prefix}:W3CDTF"));
                    if !matches!(name.local_name.as_str(), "created" | "modified") || !typed {
                        invalid = true;
                    }
                }
                if attributes
                    .iter()
                    .any(|a| a.namespace == XML_NAMESPACE && a.local_name == "lang")
                {
                    invalid = true;
                }
                open.push(elements.len());
                elements.push((
                    name.namespace.clone(),
                    name.local_name.clone(),
                    String::new(),
                ));
            }
            Event::DocumentType => {}
            Event::Text(text, _) => {
                for &index in &open {
                    elements[index].2.push_str(text);
                }
            }
            Event::End(_) => {
                open.pop();
            }
        }
        Flow::Continue
    });
    if result.is_err() {
        return Ok(());
    }
    if invalid {
        return Err(Invalid);
    }
    let first_text = |namespace: &str, local: &str| {
        elements
            .iter()
            .skip(1)
            .find(|(uri, name, _)| uri == namespace && name == local)
            .map(|(.., text)| text.clone())
    };
    for (namespace, local) in [
        (DCTERMS, "created"),
        (DCTERMS, "modified"),
        (CORE_PROPERTIES_NAMESPACE, "lastPrinted"),
    ] {
        if let Some(text) = first_text(namespace, local) {
            if !text.is_empty() && !is_lenient_date(&text) {
                return Err(Invalid);
            }
        }
    }
    Ok(())
}

/// Whether `SimpleDateFormat("yyyy-MM-dd")`, the most lenient of POI's formats, parses the
/// start of a date: three numbers, each after optional spaces or tabs, joined by hyphens.
fn is_lenient_date(text: &str) -> bool {
    let mut rest = text;
    for field in 0..3 {
        rest = rest.trim_start_matches([' ', '\t']);
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        rest = &rest[digits..];
        if field < 2 {
            match rest.strip_prefix('-') {
                Some(after) => rest = after,
                None => return false,
            }
        }
    }
    true
}

/// `URI.resolve` of a relative target against a part's path, normalized.
fn resolve(base_path: &str, target: &str) -> Option<String> {
    let parsed = uri::parse(target)?;
    let path = parsed.path?;
    let mut resolved = if path.starts_with('/')
        || (path.is_empty() && parsed.fragment.is_some() && parsed.query.is_none())
    {
        if path.is_empty() {
            base_path.to_owned()
        } else {
            path.to_owned()
        }
    } else {
        let directory = &base_path[..base_path.rfind('/').map_or(0, |slash| slash + 1)];
        normalize(&format!("{directory}{path}"))
    };
    if let Some(query) = parsed.query {
        resolved.push('?');
        resolved.push_str(query);
    }
    if let Some(fragment) = parsed.fragment {
        resolved.push('#');
        resolved.push_str(fragment);
    }
    Some(resolved)
}

/// `URI.normalize` of a path: `.` segments dropped, a segment and a following `..` removed.
fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len() - 1;
    for (index, segment) in parts.into_iter().enumerate() {
        match segment {
            "." => {
                if index == last {
                    segments.push("");
                }
            }
            ".." if segments.last().is_some_and(|s| !s.is_empty() && *s != "..") => {
                segments.pop();
                if index == last {
                    segments.push("");
                }
            }
            "" if index != last && index != 0 => {}
            other => segments.push(other),
        }
    }
    let joined = segments.join("/");
    if absolute && !joined.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

const CORE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const STRICT_CORE_DOCUMENT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument";
const VISIO_CORE_DOCUMENT: &str = "http://schemas.microsoft.com/visio/2010/relationships/document";
const XPS_DOCUMENT: &str = "http://schemas.microsoft.com/xps/2005/06/fixedrepresentation";
const OPEN_XPS_DOCUMENT: &str = "http://schemas.openxps.org/oxps/v1.0/fixedrepresentation";
const DWFX_DOCUMENT: &str = "http://schemas.autodesk.com/dwfx/2007/relationships/documentsequence";
const THREE_MF_DOCUMENT: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";

/// Why `detectOfficeOpenXML` throws.
struct Thrown;

impl Package {
    fn count(&self, kind: &str) -> usize {
        self.relationships
            .values()
            .filter(|r| r.kind == kind)
            .count()
    }

    /// `getPart(relationship)`: the part the first relationship of the type in id order whose
    /// target is a part name points at.
    fn part_for(&self, kind: &str) -> Result<Option<&(PartName, String)>, Thrown> {
        for relationship in self.relationships.values().filter(|r| r.kind == kind) {
            let unresolved = relationship.external || relationship.kind == HYPERLINK;
            let Some(target) = relationship.target.as_deref() else {
                if unresolved {
                    continue;
                }
                return Err(Thrown);
            };
            let resolved = if unresolved || uri::to_ascii_string(target).starts_with('/') {
                target.to_owned()
            } else {
                if uri::parse(target).and_then(|p| p.scheme).is_some() {
                    return Err(Thrown);
                }
                resolve("/", target).ok_or(Thrown)?
            };
            if let Some(part) = part_name(&resolved) {
                return Ok(self.parts.get(&part.key()));
            }
        }
        Ok(None)
    }

    /// `detectOfficeOpenXML`.
    fn detect(&self) -> Result<Option<MediaType>, Thrown> {
        let mut kind = CORE_DOCUMENT;
        let mut count = self.count(kind);
        if count == 0 {
            kind = STRICT_CORE_DOCUMENT;
            count = self.count(kind);
            if count == 0 {
                kind = VISIO_CORE_DOCUMENT;
                count = self.count(kind);
            }
            if count == 0 {
                if self.count(XPS_DOCUMENT) == 1 || self.count(OPEN_XPS_DOCUMENT) == 1 {
                    return Ok(Some(MediaType::named("application/vnd.ms-xpsdocument")));
                }
                kind = OPEN_XPS_DOCUMENT;
                count = self.count(kind);
            }
            if count == 0 {
                kind = DWFX_DOCUMENT;
                count = self.count(kind);
                if count == 1 {
                    return Ok(Some(MediaType::named("model/vnd.dwfx+xps")));
                }
            }
            if count == 0 {
                kind = THREE_MF_DOCUMENT;
                count = self.count(kind);
                if count == 1 {
                    return Ok(Some(MediaType::named(
                        "application/vnd.ms-package.3dmanufacturing-3dmodel+xml",
                    )));
                }
            }
        }
        if count != 1 {
            return Ok(None);
        }
        let Some((_, core_type)) = self.part_for(kind)? else {
            return Ok(None);
        };
        if core_type.contains(".xps") {
            return Ok(Some(MediaType::named("application/vnd.ms-package.xps")));
        }
        let dot = core_type.rfind('.').ok_or(Thrown)?;
        let mut document_type = core_type[..dot].to_owned();
        let lower = java::to_lower_case(&document_type);
        if lower.ends_with("macroenabled") {
            document_type = format!("{lower}.12");
        }
        let lower = java::to_lower_case(&document_type);
        if lower.ends_with("macroenabledtemplate") {
            let cut = document_type.len() - "macroenabledtemplate".len();
            document_type = format!("{}macroenabled.12", &document_type[..cut]);
        }
        Ok(MediaType::parse(&document_type))
    }
}

/// `ZipFileZipEntrySource.getEntry`: by name, else ignoring case.
fn entry_ignoring_case<'z>(zip: &'z ZipFile<'_>, name: &str) -> Option<&'z ZipEntry> {
    zip.entry(name).or_else(|| {
        zip.entries()
            .iter()
            .find(|entry| eq_ignoring_case(&entry.name.replace('\\', "/"), name))
    })
}

/// Java's `String.equalsIgnoreCase`.
fn eq_ignoring_case(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars().zip(b.chars()).all(|(x, y)| {
            x == y || x.to_uppercase().eq(y.to_uppercase()) || x.to_lowercase().eq(y.to_lowercase())
        })
}

/// `OPCPackageDetector.detect(zipFile, stream)`.
pub(crate) fn detect(zip: &ZipFile<'_>) -> Option<MediaType> {
    let content_types = entry_ignoring_case(zip, CONTENT_TYPES)?;
    let package = open(zip, content_types).ok();
    let detected = package.as_ref().and_then(|p| p.detect().ok().flatten());
    if detected.is_some() {
        return detected;
    }
    let content = read_entry(zip, content_types)?;
    content_types_type(content.bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_part_names_as_poi_does() {
        assert!(part_name("/word/document.xml").is_some());
        assert!(part_name("word/document.xml").is_none());
        assert!(part_name("/word/").is_none());
        assert!(part_name("/word//a.xml").is_none());
        assert!(part_name("/word/a.").is_none());
        assert!(part_name("/word/%41.xml").is_none());
        assert!(part_name("/word/a b.xml").is_none());
        assert!(part_name("/word/caf\u{E9}.xml").is_some());
        assert!(part_name("http://x/y").is_none());
        assert!(part_name(&to_uri_text("/media/image 1 ")).is_none());
    }

    #[test]
    fn names_relationship_parts() {
        let part = part_name("/word/document.xml").unwrap();
        assert_eq!(
            relationship_part_name(&part).unwrap().ascii,
            "/word/_rels/document.xml.rels"
        );
        assert!(part_name("/_rels/.rels").unwrap().is_relationship());
        assert!(!part.is_relationship());
    }

    #[test]
    fn resolves_targets() {
        assert_eq!(
            resolve("/", "word/document.xml").as_deref(),
            Some("/word/document.xml")
        );
        assert_eq!(
            resolve("/word/document.xml", "../media/a.png").as_deref(),
            Some("/media/a.png")
        );
        assert_eq!(resolve("/", "./a/./b.xml").as_deref(), Some("/a/b.xml"));
    }

    #[test]
    fn reads_content_types_up_to_the_first_known_one() {
        let types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.ms-word.document.macroEnabled.main+xml"/></Types>"#;
        assert_eq!(
            content_types_type(types).unwrap().as_str(),
            "application/vnd.ms-word.document.macroenabled.12"
        );
    }

    #[test]
    fn judges_dates_leniently() {
        assert!(is_lenient_date("2020-01-02T03:04:05Z"));
        assert!(is_lenient_date(" 2020-1-2"));
        assert!(!is_lenient_date("yesterday"));
        assert!(!is_lenient_date("\n2020-01-02"));
    }
}
