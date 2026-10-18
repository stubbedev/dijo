//! GeoGebra files, as Tika's `GeoGebraParser` reads them: the archive's `geogebra.xml`, its
//! `geogebra_macro.xml` and each slide's `geogebra.xml`, in the order the slides'
//! `structure.json` gives and else by number, read by its `GeoGebraXMLHandler` into the
//! document's metadata and paragraphs of the construction's texts; then the thumbnail and every
//! other entry handed to the embedded document extractor.

mod json;

use std::collections::HashMap;

use json::{Json, Unread};

use crate::compress::zip::file::{ZipEntry, ZipFile};
use crate::compress::zip::Content;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{self, ResourceType, Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::{apply_page_numbers, TikaMetadata};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Opened, Parser, Prepared};
use crate::java;
use crate::mime::Detected;

const GEOGEBRA_XML: &str = "geogebra.xml";
const MACRO_XML: &str = "geogebra_macro.xml";
const STRUCTURE_JSON: &str = "structure.json";
const THUMBNAIL_PNG: &str = "geogebra_thumbnail.png";
const JAVASCRIPT_JS: &str = "geogebra_javascript.js";
/// `HOUSEKEEPING_NAMES`, the entries of a known directory that are not handed on.
const HOUSEKEEPING: [&str; 5] = [
    GEOGEBRA_XML,
    MACRO_XML,
    THUMBNAIL_PNG,
    "geogebra_defaults2d.xml",
    "geogebra_defaults3d.xml",
];
const SLIDE_PREFIX: &str = "_slide";
/// `MAX_STRUCTURE_JSON_LENGTH`: the most of `structure.json` Jackson is given.
const MAX_STRUCTURE_LENGTH: usize = 1024 * 1024;
/// `GeoGebraXMLHandler.MAX_CONTENT_LENGTH`, in UTF-16 units.
const MAX_CONTENT_LENGTH: usize = 1024 * 1024;

mod name {
    pub(super) const APP_NAME: &str = "geogebra:app-name";
    pub(super) const APP_VERSION: &str = "geogebra:app-version";
    pub(super) const FORMAT_VERSION: &str = "geogebra:format-version";
    pub(super) const ID: &str = "geogebra:id";
    pub(super) const DATE: &str = "geogebra:date";
    pub(super) const TOOL_NAME: &str = "geogebra:toolName";
    pub(super) const N_PAGES: &str = "xmpTPg:NPages";
}

/// Tika's `GeoGebraParser`.
pub struct GeoGebraParser;

impl Parser for GeoGebraParser {
    fn name(&self) -> &'static str {
        "extractous::GeoGebraParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.geogebra.GeoGebraParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.geogebra.file",
            "application/vnd.geogebra.slides",
            "application/vnd.geogebra.tool",
        ]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let zip = ZipFile::open(input).map_err(|_| {
            Decline::Fails(Error::ParseError(
                "commons-compress's ZipFile cannot open the GeoGebra archive".to_owned(),
            ))
        })?;
        let book =
            Book::read(&zip).map_err(|Unread(reason)| Decline::Deferred(reason.to_owned()))?;
        Ok(Box::new(move |xhtml| book.write(xhtml)))
    }
}

/// What the parse reads of an entry: its data, or why commons-compress cannot read it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Data {
    Read(Vec<u8>),
    /// `canReadEntryData` is false: an encrypted entry, or one compressed a way commons-compress
    /// does not read.
    Unsupported,
}

/// An XML document of the archive, read by `GeoGebraXMLHandler`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Xml {
    name: String,
    data: Data,
}

/// An entry handed to the embedded document extractor, with its resource type where the parser
/// names it and its slide's page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attachment {
    name: String,
    data: Data,
    kind: Option<ResourceType>,
    page: Option<i32>,
}

/// The archive, read as far as the parse needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Book {
    root: Option<Xml>,
    tool: Option<Xml>,
    slides: Vec<Xml>,
    thumbnail: Option<Attachment>,
    others: Vec<Attachment>,
}

/// The data of an entry, or why the port leaves the archive to Tika.
fn data(zip: &ZipFile<'_>, entry: &ZipEntry) -> Result<Data, Unread> {
    if !entry.can_read() {
        return Ok(Data::Unsupported);
    }
    match zip.read(entry) {
        Some(Content::Complete(data)) => Ok(Data::Read(data.into_owned())),
        _ => Err(Unread("an entry the port does not decompress whole")),
    }
}

/// The XML document named `name`, checked for being well-formed: Tika records the exception of
/// one that is not, which only its path can tell.
fn xml(zip: &ZipFile<'_>, name: &str) -> Result<Option<Xml>, Unread> {
    let Some(entry) = zip.entry(name) else {
        return Ok(None);
    };
    let data = data(zip, entry)?;
    if let Data::Read(bytes) = &data {
        let mut handler = Handler {
            xhtml: None,
            document_metadata: false,
            depth: 0,
            icons: Vec::new(),
        };
        xml::parse_sax(bytes, "the GeoGebra XML", &mut handler).map_err(|_| {
            Unread(
                "XML Tika's parser fails on, or a text's JSON serde_json does not read, of \
                     which Tika records the exception or which Jackson may read",
            )
        })?;
    }
    Ok(Some(Xml {
        name: name.to_owned(),
        data,
    }))
}

/// `compareSlideIds`: by number, its leading zeros aside, then by name.
fn compare_slides(a: &str, b: &str) -> std::cmp::Ordering {
    let digits = |id: &str| -> String {
        let digits = &id[SLIDE_PREFIX.len()..];
        let zeros = digits
            .bytes()
            .take(digits.len().saturating_sub(1))
            .take_while(|&byte| byte == b'0')
            .count();
        digits[zeros..].to_owned()
    };
    let (da, db) = (digits(a), digits(b));
    da.len()
        .cmp(&db.len())
        .then_with(|| java::compare(&da, &db))
        .then_with(|| java::compare(a, b))
}

/// The slide of an entry named `_slide<digits>/geogebra.xml`.
fn slide_id(name: &str) -> Option<&str> {
    let id = name.strip_suffix(&format!("/{GEOGEBRA_XML}"))?;
    let digits = id.strip_prefix(SLIDE_PREFIX)?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}

impl Book {
    fn read(zip: &ZipFile<'_>) -> Result<Self, Unread> {
        let slide_ids = slide_ids(zip)?;
        let root = xml(zip, GEOGEBRA_XML)?;
        let tool = xml(zip, MACRO_XML)?;
        let icons = match &tool {
            Some(Xml {
                data: Data::Read(bytes),
                ..
            }) => icon_files(bytes),
            _ => Vec::new(),
        };
        let mut slides = Vec::with_capacity(slide_ids.len());
        for id in &slide_ids {
            slides.push(
                xml(zip, &format!("{id}/{GEOGEBRA_XML}"))?.expect("a slide is named by its entry"),
            );
        }
        let thumbnail_entry = zip
            .entry(THUMBNAIL_PNG)
            .or_else(|| {
                slide_ids
                    .iter()
                    .find_map(|id| zip.entry(&format!("{id}/{THUMBNAIL_PNG}")))
            })
            .or_else(|| icons.iter().find_map(|icon| zip.entry(icon)));
        let thumbnail = thumbnail_entry
            .map(|entry| -> Result<Attachment, Unread> {
                Ok(Attachment {
                    name: entry.name.clone(),
                    data: data(zip, entry)?,
                    kind: Some(ResourceType::Thumbnail),
                    page: None,
                })
            })
            .transpose()?;
        let pages: HashMap<&str, i32> = slide_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index as i32 + 1))
            .collect();
        let mut others = Vec::new();
        for entry in zip.entries() {
            let name = entry.name.as_str();
            if entry.is_directory()
                || thumbnail
                    .as_ref()
                    .is_some_and(|thumbnail| thumbnail.name == name)
            {
                continue;
            }
            let (directory, base) = name.split_once('/').unwrap_or(("", name));
            let known = directory.is_empty() || pages.contains_key(directory);
            if known
                && (HOUSEKEEPING.contains(&base)
                    || (directory.is_empty() && base == STRUCTURE_JSON))
            {
                continue;
            }
            let kind = (known && base == JAVASCRIPT_JS).then_some(ResourceType::Macro);
            others.push(Attachment {
                name: name.to_owned(),
                data: data(zip, entry)?,
                kind,
                page: pages.get(directory).copied(),
            });
        }
        Ok(Self {
            root,
            tool,
            slides,
            thumbnail,
            others,
        })
    }

    /// `parse` once the archive is open.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let mut pending = true;
        if let Some(root) = &self.root {
            pending = false;
            parse_xml(root, xhtml, true)?;
        }
        if let Some(tool) = &self.tool {
            parse_xml(tool, xhtml, pending)?;
            pending = false;
        }
        if !self.slides.is_empty() {
            xhtml
                .metadata_mut()
                .set(name::N_PAGES, self.slides.len().to_string());
            for slide in &self.slides {
                xhtml.start_element_with("div", &[("class", "slide")])?;
                let parsed = parse_xml(slide, xhtml, pending);
                pending = false;
                xhtml.end_element("div")?;
                parsed?;
            }
        }
        if let Some(thumbnail) = &self.thumbnail {
            embed(thumbnail, xhtml)?;
        }
        for other in &self.others {
            embed(other, xhtml)?;
        }
        Ok(())
    }
}

/// `getSlideIds`: the slides by number, those `structure.json` lists first in its order.
fn slide_ids(zip: &ZipFile<'_>) -> Result<Vec<String>, Unread> {
    let mut numbered: Vec<String> = zip
        .entries()
        .iter()
        .filter_map(|entry| slide_id(&entry.name))
        .map(str::to_owned)
        .collect();
    if numbered.is_empty() {
        return Ok(numbered);
    }
    numbered.sort_by(|a, b| compare_slides(a, b));
    let mut ordered: Vec<String> = Vec::new();
    if let Some(entry) = zip.entry(STRUCTURE_JSON).filter(|entry| entry.can_read()) {
        let Data::Read(bytes) = data(zip, entry)? else {
            unreachable!("a readable entry is read");
        };
        if bytes.len() > MAX_STRUCTURE_LENGTH {
            return Err(Unread("a structure.json Jackson reads cut short"));
        }
        if !bytes.iter().all(u8::is_ascii_whitespace) {
            let root = json::read(&bytes)?;
            let chapters = root.member("chapters").into_iter().flat_map(Json::children);
            for chapter in chapters {
                let pages = chapter.member("pages").into_iter().flat_map(Json::children);
                for page in pages {
                    let elements = page.member("elements").into_iter().flat_map(Json::children);
                    for element in elements {
                        if let Some(Json::Text(id)) = element.member("id") {
                            if numbered.contains(id) && !ordered.contains(id) {
                                ordered.push(id.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    for id in numbered {
        if !ordered.contains(&id) {
            ordered.push(id);
        }
    }
    Ok(ordered)
}

/// The icon files a tool's macros name, which may hold its thumbnail.
fn icon_files(xml: &[u8]) -> Vec<String> {
    let mut handler = Handler {
        xhtml: None,
        document_metadata: false,
        depth: 0,
        icons: Vec::new(),
    };
    let _parsed = xml::parse_sax(xml, "the GeoGebra macros", &mut handler);
    handler.icons
}

/// `parseGeoGebraXml`: an entry the parser cannot read recorded, else its XML read by
/// `GeoGebraXMLHandler`.
fn parse_xml(entry: &Xml, xhtml: &mut Xhtml, document_metadata: bool) -> Result<(), Failure> {
    let Data::Read(bytes) = &entry.data else {
        unsupported(&entry.name, xhtml.metadata_mut());
        return Ok(());
    };
    let mut handler = Handler {
        xhtml: Some(&mut *xhtml),
        document_metadata,
        depth: 0,
        icons: Vec::new(),
    };
    xml::parse_sax(bytes, "the GeoGebra XML", &mut handler)
}

/// What Tika records of an entry `canReadEntryData` refuses.
fn unsupported(name: &str, metadata: &mut TikaMetadata) {
    embedded::record_stream_exception(
        metadata,
        &format!("java.io.IOException: Unsupported zip entry: {name}"),
    );
}

/// `handleEmbedded`: an entry handed to the embedded document extractor bare, named by its path,
/// with its slide's page; one without a resource type detected first, and an image of it inline.
fn embed(attachment: &Attachment, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let Data::Read(data) = &attachment.data else {
        unsupported(&attachment.name, xhtml.metadata_mut());
        return Ok(());
    };
    let mut metadata = TikaMetadata::new();
    metadata.set(RESOURCE_NAME, attachment.name.as_str());
    metadata.set(INTERNAL_PATH, attachment.name.as_str());
    if let Some(page) = attachment.page {
        apply_page_numbers(&mut metadata, &[page]);
    }
    let kind = match attachment.kind {
        Some(kind) => kind,
        None => {
            let detected =
                Detected::detect_embedded(data, &metadata, Opened::Alone).map_err(|_| {
                    Failure::Malformed(format!(
                        "Tika's detection throws on the entry {}",
                        attachment.name
                    ))
                })?;
            metadata = detected.metadata;
            metadata.set("Content-Type", detected.media_type.as_str());
            if detected.media_type.starts_with("image/") {
                ResourceType::Inline
            } else {
                ResourceType::Attachment
            }
        }
    };
    kind.set(&mut metadata);
    xhtml.parse_embedded_opened(data, &metadata, Wrapping::Bare, Opened::Alone)
}

/// `GeoGebraXMLHandler`: the document's metadata from the root element and its own construction,
/// and paragraphs of the texts of expressions, text contents, captions and tools.
struct Handler<'x, 'h> {
    /// Where the paragraphs go: `None` when the XML is only read for its icon files or checked.
    xhtml: Option<&'x mut Xhtml<'h>>,
    document_metadata: bool,
    depth: usize,
    icons: Vec<String>,
}

impl Handler<'_, '_> {
    fn metadata(&mut self) -> Option<&mut TikaMetadata> {
        self.xhtml.as_mut().map(|xhtml| xhtml.metadata_mut())
    }

    /// `setIfNotBlank`.
    fn set_if_not_blank(&mut self, name: &str, value: Option<&str>) {
        if let Some(value) = value.filter(|value| !java::is_blank(value)) {
            if let Some(metadata) = self.metadata() {
                metadata.set(name, java::trim(value));
            }
        }
    }

    /// `paragraph`: a paragraph of the text trimmed, unless it is blank.
    fn paragraph(&mut self, text: Option<&str>) -> Result<(), Failure> {
        let (Some(text), Some(xhtml)) = (text, self.xhtml.as_mut()) else {
            return Ok(());
        };
        if !java::is_blank(text) {
            xhtml.element("p", java::trim(text))?;
        }
        Ok(())
    }

    /// `handleExpression`: the string literals of an expression, joined.
    fn expression(&mut self, expression: Option<&str>) -> Result<(), Failure> {
        let Some(expression) = expression.filter(|expression| expression.contains('"')) else {
            return Ok(());
        };
        let mut joined = String::new();
        let mut rest = expression;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('"') else {
                break;
            };
            joined.push_str(&after[..close]);
            rest = &after[close + 1..];
        }
        self.paragraph(Some(&joined))
    }

    /// `handleContent`: the texts of a text's JSON runs, a paragraph to each line.
    fn content(&mut self, value: Option<&str>) -> Result<(), Failure> {
        let Some(value) = value else {
            return Ok(());
        };
        let trimmed = java::trim(value);
        if trimmed.is_empty()
            || java::utf16_length(trimmed) > MAX_CONTENT_LENGTH
            || !(trimmed.starts_with('[') || trimmed.starts_with('{'))
        {
            return Ok(());
        }
        let Ok(root) = json::read(trimmed.as_bytes()) else {
            return Err(Failure::Malformed(
                "a text's JSON, which serde_json does not read and Jackson may".to_owned(),
            ));
        };
        let mut texts = Vec::new();
        root.find_values_as_text("text", &mut texts)
            .map_err(|Unread(reason)| Failure::Malformed(reason.to_owned()))?;
        let joined = texts.concat();
        for line in joined
            .split("\r\n")
            .flat_map(|line| line.split(['\r', '\n']))
        {
            self.paragraph(Some(line))?;
        }
        Ok(())
    }
}

impl SaxHandler for Handler<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let value = |name: &str| attribute_named(attributes, name);
        if self.depth == 0 && local_name == "geogebra" {
            if self.document_metadata {
                self.set_if_not_blank(name::APP_NAME, value("app"));
                self.set_if_not_blank(name::APP_VERSION, value("version"));
                self.set_if_not_blank(name::FORMAT_VERSION, value("format"));
                self.set_if_not_blank(name::ID, value("id"));
            }
        } else if self.depth == 1 && local_name == "construction" {
            if self.document_metadata {
                self.set_if_not_blank("dc:title", value("title"));
                self.set_if_not_blank("dc:creator", value("author"));
                self.set_if_not_blank(name::DATE, value("date"));
            }
        } else if local_name == "expression" {
            self.expression(value("exp"))?;
        } else if local_name == "content" {
            self.content(value("val"))?;
        } else if local_name == "caption" {
            self.paragraph(value("val"))?;
        } else if local_name == "macro" {
            let tool = value("toolName")
                .filter(|tool| !java::is_blank(tool))
                .or_else(|| value("cmdName"));
            if let Some(tool) = tool.filter(|tool| !java::is_blank(tool)) {
                if let Some(metadata) = self.metadata() {
                    metadata.add(name::TOOL_NAME, java::trim(tool));
                }
            }
            self.paragraph(tool)?;
            self.paragraph(value("toolHelp"))?;
            if let Some(icon) = value("iconFile").filter(|icon| !java::is_blank(icon)) {
                self.icons.push(java::trim(icon).to_owned());
            }
        }
        self.depth += 1;
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
