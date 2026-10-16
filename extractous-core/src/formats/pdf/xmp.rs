//! Tika's `XmpExtractor` with `PDMetadataExtractor`'s fixups: an XMP packet flattened into
//! leaves as `XmpSaxFlattener` flattens it, mapped to Tika's metadata properties, its dates
//! normalized, and unmapped leaves kept under `xmp-raw:`.

use std::collections::{BTreeMap, HashMap};

use super::content::parse_literal;
use super::cos::text_string;
use super::date;
use super::Unported;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xml::{self, Attribute, SaxHandler, StartTag};
use crate::formats::Failure;
use crate::java::time::format_instant;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const META: &str = "adobe:ns:meta/";
const RESOURCE_EVENT_NS: &str = "http://ns.adobe.com/xap/1.0/sType/ResourceEvent#";
const RESOURCE_REF_NS: &str = "http://ns.adobe.com/xap/1.0/sType/ResourceRef#";
const GOOGLE_CAMERA: &str = "http://ns.google.com/photos/1.0/camera/";
const GOOGLE_CONTAINER: &str = "http://ns.google.com/photos/1.0/container/";
const GOOGLE_ITEM: &str = "http://ns.google.com/photos/1.0/container/item/";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const XMP: &str = "http://ns.adobe.com/xap/1.0/";
const XMP_MM: &str = "http://ns.adobe.com/xap/1.0/mm/";
const PDF: &str = "http://ns.adobe.com/pdf/1.3/";
const XMP_RIGHTS: &str = "http://ns.adobe.com/xap/1.0/rights/";
const PHOTOSHOP: &str = "http://ns.adobe.com/photoshop/1.0/";
const AUX: &str = "http://ns.adobe.com/exif/1.0/aux/";
const TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
const EXIF: &str = "http://ns.adobe.com/exif/1.0/";
const PDFA_ID: &str = "http://www.aiim.org/pdfa/ns/id/";
const PDFUA_ID: &str = "http://www.aiim.org/pdfua/ns/id/";
const PDFX: &str = "http://ns.adobe.com/pdfx/1.3/";
const PDFX_ID: &str = "http://www.npes.org/pdfx/ns/id/";
const PDFVT: &str = "http://www.npes.org/pdfvt/ns/id/";
const ILLUSTRATOR: &str = "http://ns.adobe.com/illustrator/1.0/";
const RAW_PREFIX: &str = "xmp-raw:";
const MAX_LEAVES: usize = 50_000;
const MAX_PATH: usize = 512;
const MAX_VALUE: usize = 1 << 20;
const MAX_HISTORY_EVENTS: usize = 1024;

/// A Tika metadata `Property`, as far as setting it differs: a bag or sequence takes several
/// values, a date is normalized, and a composite sets its secondaries too.
#[derive(Debug, Clone, Copy)]
pub(super) struct Property {
    pub(super) name: &'static str,
    multi: bool,
    date: bool,
    secondary: &'static [Property],
}

impl Property {
    /// `isMultiValuePermitted`.
    pub(super) fn is_multi(&self) -> bool {
        self.multi
    }
}

/// A single-valued text property.
pub(super) const fn text(name: &'static str) -> Property {
    Property {
        name,
        multi: false,
        date: false,
        secondary: &[],
    }
}

/// A property of several text values.
pub(super) const fn bag(name: &'static str) -> Property {
    Property {
        name,
        multi: true,
        date: false,
        secondary: &[],
    }
}

const fn date_property(name: &'static str) -> Property {
    Property {
        name,
        multi: false,
        date: true,
        secondary: &[],
    }
}

pub(super) const DC_TITLE: Property = bag("dc:title");
pub(super) const DC_CREATOR: Property = bag("dc:creator");
pub(super) const DC_SUBJECT: Property = bag("dc:subject");
pub(super) const DCTERMS_CREATED: Property = date_property("dcterms:created");
pub(super) const DCTERMS_MODIFIED: Property = date_property("dcterms:modified");
const OFFICE_KEYWORDS: Property = Property {
    name: "meta:keyword",
    multi: true,
    date: false,
    secondary: &[DC_SUBJECT],
};

/// `XmpExtractor.TABLE`: the properties a top-level `(namespace, local name)` maps to.
fn table(namespace: &str, local_name: &str) -> Option<&'static [Property]> {
    Some(match (namespace, local_name) {
        (DC, "title") => const { &[DC_TITLE, text("xmp:dc:title")] },
        (DC, "creator") => const { &[DC_CREATOR, bag("xmp:dc:creator")] },
        (DC, "subject") => const { &[DC_SUBJECT, bag("xmp:dc:subject")] },
        (DC, "description") => const { &[bag("dc:description"), text("xmp:dc:description")] },
        (DC, "publisher") => const { &[bag("dc:publisher"), text("xmp:dc:publisher")] },
        (DC, "contributor") => const { &[bag("dc:contributor"), bag("xmp:dc:contributor")] },
        (DC, "type") => const { &[bag("dc:type"), text("xmp:dc:type")] },
        (DC, "format") => const { &[text("xmp:dc:format")] },
        (DC, "identifier") => const { &[bag("dc:identifier"), text("xmp:dc:identifier")] },
        (DC, "language") => const { &[bag("dc:language"), text("xmp:dc:language")] },
        (DC, "relation") => const { &[bag("dc:relation"), text("xmp:dc:relation")] },
        (DC, "source") => const { &[bag("dc:source"), text("xmp:dc:source")] },
        (DC, "coverage") => const { &[bag("dc:coverage"), text("xmp:dc:coverage")] },
        (DC, "rights") => const { &[bag("dc:rights"), text("xmp:dc:rights")] },
        (DC, "date") => const { &[date_property("xmp:dc:date")] },
        (XMP, "Title") => const { &[DC_TITLE, text("xmp:Title")] },
        (XMP, "CreateDate") => const { &[date_property("xmp:CreateDate")] },
        (XMP, "ModifyDate") => const { &[date_property("xmp:ModifyDate")] },
        (XMP, "MetadataDate") => const { &[date_property("xmp:MetadataDate")] },
        (XMP, "CreatorTool") => const { &[text("xmp:CreatorTool")] },
        (XMP, "Rating") => const { &[text("xmp:Rating")] },
        (XMP, "Label") => const { &[text("xmp:Label")] },
        (XMP, "Nickname") => const { &[text("xmp:NickName")] },
        (XMP, "Identifier") => const { &[bag("xmp:Identifier")] },
        (XMP, "Advisory") => const { &[bag("xmp:Advisory")] },
        (RDF, "about") => const { &[bag("xmp:About")] },
        (XMP_MM, "DocumentID") => const { &[text("xmpMM:DocumentID")] },
        (XMP_MM, "InstanceID") => const { &[text("xmpMM:InstanceID")] },
        (XMP_MM, "OriginalDocumentID") => const { &[text("xmpMM:OriginalDocumentID")] },
        (XMP_MM, "RenditionClass") => const { &[text("xmpMM:RenditionClass")] },
        (XMP_MM, "RenditionParams") => const { &[text("xmpMM:RenditionParams")] },
        (PDF, "Producer") => const { &[text("pdf:producer"), text("xmp:pdf:Producer")] },
        (PDF, "Keywords") => const { &[OFFICE_KEYWORDS, bag("xmp:pdf:Keywords")] },
        (PDF, "PDFVersion") => const { &[text("pdf:pdf-version"), text("xmp:pdf:PDFVersion")] },
        (XMP_RIGHTS, "Marked") => const { &[text("xmpRights:Marked")] },
        (XMP_RIGHTS, "WebStatement") => const { &[text("xmpRights:WebStatement")] },
        (XMP_RIGHTS, "UsageTerms") => const { &[text("xmpRights:UsageTerms")] },
        (XMP_RIGHTS, "Owner") => const { &[bag("xmpRights:Owner")] },
        (XMP_RIGHTS, "Certificate") => const { &[text("xmpRights:Certificate")] },
        (PHOTOSHOP, "ColorMode") => const { &[text("photoshop:ColorMode")] },
        (PHOTOSHOP, "ICCProfile") => const { &[text("photoshop:ICCProfile")] },
        (PHOTOSHOP, "DateCreated") => const { &[date_property("photoshop:DateCreated")] },
        ("http://ns.adobe.com/xap/1.0/t/pg/", "NPages") => const { &[text("xmpTPg:NPages")] },
        (PDF, "Trapped") => const { &[text("pdf:trapped"), text("xmp:pdf:Trapped")] },
        (AUX, "SerialNumber") => const { &[text("aux:SerialNumber")] },
        (AUX, "Lens") => const { &[text("aux:Lens")] },
        (AUX, "LensInfo") => const { &[text("aux:LensInfo")] },
        (AUX, "LensID") => const { &[text("aux:LensID")] },
        (TIFF, "Make") => const { &[text("tiff:Make"), text("xmp:tiff:Make")] },
        (TIFF, "Model") => const { &[text("tiff:Model"), text("xmp:tiff:Model")] },
        (TIFF, "Software") => const { &[text("tiff:Software"), text("xmp:tiff:Software")] },
        (TIFF, "ImageWidth") => const { &[text("tiff:ImageWidth"), text("xmp:tiff:ImageWidth")] },
        (TIFF, "ImageLength") => {
            const { &[text("tiff:ImageLength"), text("xmp:tiff:ImageLength")] }
        }
        (TIFF, "BitsPerSample") => {
            const { &[bag("tiff:BitsPerSample"), bag("xmp:tiff:BitsPerSample")] }
        }
        (TIFF, "SamplesPerPixel") => {
            const {
                &[
                    text("tiff:SamplesPerPixel"),
                    text("xmp:tiff:SamplesPerPixel"),
                ]
            }
        }
        (TIFF, "Orientation") => {
            const { &[text("tiff:Orientation"), text("xmp:tiff:Orientation")] }
        }
        (EXIF, "DateTimeOriginal") => {
            const {
                &[
                    date_property("exif:DateTimeOriginal"),
                    date_property("xmp:exif:DateTimeOriginal"),
                ]
            }
        }
        (EXIF, "ISOSpeedRatings") => {
            const { &[bag("exif:IsoSpeedRatings"), bag("xmp:exif:IsoSpeedRatings")] }
        }
        (EXIF, "PixelXDimension") => {
            const { &[text("tiff:ImageWidth"), text("xmp:exif:PixelXDimension")] }
        }
        (EXIF, "PixelYDimension") => {
            const { &[text("tiff:ImageLength"), text("xmp:exif:PixelYDimension")] }
        }
        (PDFA_ID, "part") => const { &[text("pdfaid:part")] },
        (PDFA_ID, "conformance") => const { &[text("pdfaid:conformance")] },
        (PDFUA_ID, "part") => const { &[text("pdfuaid:part")] },
        (PDFX, "GTS_PDFXVersion") => const { &[text("pdfx:version")] },
        (PDFX, "GTS_PDFXConformance") => const { &[text("pdfx:conformance")] },
        (PDFX_ID, "GTS_PDFXVersion") => const { &[text("pdfxid:version")] },
        (PDFVT, "GTS_PDFVTVersion") => const { &[text("pdfvt:version")] },
        (PDFVT, "GTS_PDFVTModDate") => const { &[date_property("pdfvt:modified")] },
        (ILLUSTRATOR, "Type") => const { &[text("pdf:illustrator:type")] },
        _ => return None,
    })
}

/// `XmpExtractor.HISTORY`, in the order the bags are written.
const HISTORY: [(&str, &str); 6] = [
    ("action", "xmpMM:History:Action"),
    ("when", "xmpMM:History:When"),
    ("instanceID", "xmpMM:History:InstanceID"),
    ("softwareAgent", "xmpMM:History:SoftwareAgent"),
    ("changed", "xmpMM:History:Changed"),
    ("parameters", "xmpMM:History:Parameters"),
];

fn derived_from(local_name: &str) -> Option<Property> {
    Some(text(match local_name {
        "documentID" => "xmpMM:DerivedFrom:DocumentID",
        "instanceID" => "xmpMM:DerivedFrom:InstanceID",
        "originalDocumentID" => "xmpMM:DerivedFrom:OriginalDocumentID",
        "renditionClass" => "xmpMM:DerivedFrom:RenditionClass",
        _ => return None,
    }))
}

fn google_camera(local_name: &str) -> Option<&'static str> {
    Some(match local_name {
        "MotionPhoto" => "Camera:MotionPhoto",
        "MotionPhotoVersion" => "Camera:MotionPhotoVersion",
        "MotionPhotoPresentationTimestampUs" => "Camera:MotionPhotoPresentationTimestampUs",
        "MicroVideo" => "Camera:MicroVideo",
        "MicroVideoVersion" => "Camera:MicroVideoVersion",
        "MicroVideoOffset" => "Camera:MicroVideoOffset",
        "MicroVideoPresentationTimestampUs" => "Camera:MicroVideoPresentationTimestampUs",
        _ => return None,
    })
}

fn fill_if_absent(namespace: &str, local_name: &str) -> Option<Property> {
    match (namespace, local_name) {
        (XMP, "CreateDate") | (PHOTOSHOP, "DateCreated") | (EXIF, "DateTimeOriginal") => {
            Some(DCTERMS_CREATED)
        }
        (XMP, "ModifyDate") => Some(DCTERMS_MODIFIED),
        _ => None,
    }
}

fn dropped(namespace: &str, local_name: &str) -> bool {
    matches!(
        (namespace, local_name),
        (TIFF, "NativeDigest")
            | (EXIF, "NativeDigest")
            | ("http://ns.adobe.com/xap/1.0/g/img/", "image")
    )
}

/// `XmpProperty`: a leaf of the packet.
#[derive(Debug, Clone)]
struct Leaf {
    namespace: String,
    path: String,
    value: String,
    lang: Option<String>,
}

impl Leaf {
    /// `XmpProperty.localName`.
    fn local_name(&self) -> &str {
        let last = self.path.rsplit('/').next().unwrap_or(&self.path);
        let last = strip_trailing_index(last);
        last.rsplit(':').next().unwrap_or(last)
    }
}

/// Without a trailing `[n]` array index.
fn strip_trailing_index(path: &str) -> &str {
    if let Some(stripped) = path.strip_suffix(']') {
        if let Some(open) = stripped.rfind('[') {
            let digits = &stripped[open + 1..];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return &path[..open];
            }
        }
    }
    path
}

/// A frame of `XmpSaxFlattener.Handler`.
struct Frame {
    segment: String,
    namespace: String,
    text: String,
    children: usize,
    lang: Option<String>,
    has_value: bool,
}

/// `XmpSaxFlattener.Handler`.
#[derive(Default)]
struct Flattener {
    leaves: Vec<Leaf>,
    frames: Vec<Frame>,
    li_counts: Vec<usize>,
}

impl Flattener {
    fn add(&mut self, leaf: Leaf) {
        if self.leaves.len() >= MAX_LEAVES
            || leaf.path.encode_utf16().count() > MAX_PATH
            || leaf.value.encode_utf16().count() > MAX_VALUE
        {
            return;
        }
        self.leaves.push(leaf);
    }

    fn path(&self) -> String {
        let mut path = String::new();
        for frame in &self.frames {
            if frame.segment.starts_with('[') {
                path.push_str(&frame.segment);
            } else {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(&frame.segment);
            }
        }
        path
    }

    fn push_frame(&mut self, segment: String, namespace: String) {
        if let Some(parent) = self.frames.last_mut() {
            parent.children += 1;
        }
        self.frames.push(Frame {
            segment,
            namespace,
            text: String::new(),
            children: 0,
            lang: None,
            has_value: false,
        });
    }

    fn canonical(namespace: &str, qualified: &str, local: &str) -> String {
        match namespace {
            GOOGLE_CONTAINER => format!("Container:{local}"),
            GOOGLE_ITEM => format!("Item:{local}"),
            _ => qualified.to_owned(),
        }
    }

    fn emit_attributes(&mut self, attributes: &[Attribute], value_frame: bool) {
        let base = self.path();
        for attribute in attributes {
            let value = crate::java::trim(&attribute.value);
            if value.is_empty() {
                continue;
            }
            if attribute.namespace == XMLNS_XML {
                if attribute.local_name == "lang" {
                    if value_frame {
                        if let Some(frame) = self.frames.last_mut() {
                            frame.lang = Some(value.to_owned());
                        }
                    }
                    if !base.is_empty() {
                        self.add(Leaf {
                            namespace: XMLNS_XML.to_owned(),
                            path: format!("{base}/xml:lang"),
                            value: value.to_owned(),
                            lang: None,
                        });
                    }
                }
                continue;
            }
            if attribute.namespace == RDF {
                if attribute.local_name == "resource" {
                    let namespace = self
                        .frames
                        .last()
                        .map_or_else(String::new, |frame| frame.namespace.clone());
                    self.add(Leaf {
                        namespace,
                        path: base.clone(),
                        value: value.to_owned(),
                        lang: None,
                    });
                } else if attribute.local_name == "about" {
                    self.add(Leaf {
                        namespace: RDF.to_owned(),
                        path: if base.is_empty() {
                            "rdf:about".to_owned()
                        } else {
                            format!("{base}/rdf:about")
                        },
                        value: value.to_owned(),
                        lang: None,
                    });
                }
                continue;
            }
            if attribute.namespace.is_empty() {
                continue;
            }
            let name = Self::canonical(
                &attribute.namespace,
                &attribute.qualified_name,
                &attribute.local_name,
            );
            self.add(Leaf {
                namespace: attribute.namespace.clone(),
                path: if base.is_empty() {
                    name
                } else {
                    format!("{base}/{name}")
                },
                value: value.to_owned(),
                lang: None,
            });
        }
    }
}

/// The namespace of `xml:` attributes.
const XMLNS_XML: &str = "http://www.w3.org/XML/1998/namespace";

impl SaxHandler for Flattener {
    fn start_element(
        &mut self,
        _namespace: &str,
        _local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        unreachable!("start_tag handles every start tag")
    }

    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        let (namespace, local) = (tag.namespace, tag.local_name);
        if namespace == META {
            return Ok(());
        }
        let rdf = namespace == RDF;
        if rdf && local == "RDF" {
            return Ok(());
        }
        if rdf && local == "Description" {
            self.emit_attributes(tag.attributes, false);
            return Ok(());
        }
        if rdf && matches!(local, "Bag" | "Seq" | "Alt") {
            self.li_counts.push(0);
            return Ok(());
        }
        if rdf && local == "li" {
            let index = match self.li_counts.last_mut() {
                Some(count) => {
                    *count += 1;
                    *count
                }
                None => 1,
            };
            let namespace = self
                .frames
                .last()
                .map_or_else(String::new, |frame| frame.namespace.clone());
            self.push_frame(format!("[{index}]"), namespace);
        } else if rdf && local == "value" {
            if let Some(frame) = self.frames.last_mut() {
                frame.has_value = true;
            }
            return Ok(());
        } else if !rdf {
            self.push_frame(
                Self::canonical(namespace, tag.qualified_name, local),
                namespace.to_owned(),
            );
        } else {
            return Ok(());
        }
        self.emit_attributes(tag.attributes, true);
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local: &str) -> Result<(), Failure> {
        if namespace == META {
            return Ok(());
        }
        let rdf = namespace == RDF;
        if rdf && (local == "RDF" || local == "Description") {
            return Ok(());
        }
        if rdf && matches!(local, "Bag" | "Seq" | "Alt") {
            self.li_counts.pop();
            return Ok(());
        }
        if rdf && local != "li" {
            return Ok(());
        }
        let path = self.path();
        let Some(frame) = self.frames.pop() else {
            return Ok(());
        };
        let text = crate::java::trim(&frame.text);
        if !text.is_empty() && (frame.children == 0 || frame.has_value) {
            self.add(Leaf {
                namespace: frame.namespace.clone(),
                path,
                value: text.to_owned(),
                lang: frame.lang.clone(),
            });
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if let Some(frame) = self.frames.last_mut() {
            frame.text.push_str(text);
        }
        Ok(())
    }
}

/// `PDMetadataExtractor.decode`: a value written as a PDF literal string with a byte order
/// mark, decoded as PDFBox decodes text strings.
pub(super) fn decode(value: &str) -> String {
    let encoded = ["\\376\\377", "\\377\\376", "\\357\\273\\277"];
    if value.encode_utf16().count() < 8 || !encoded.iter().any(|bom| value.starts_with(bom)) {
        return value.to_owned();
    }
    let latin1: Vec<u8> = std::iter::once(b'(')
        .chain(
            value
                .chars()
                .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')),
        )
        .chain(std::iter::once(b')'))
        .collect();
    match parse_literal(&latin1) {
        Some(bytes) => text_string(&bytes),
        None => value.to_owned(),
    }
}

/// `XmpDates.normalize`: a date's instant as Tika writes dates, if PDFBox reads it.
fn normalize_date(raw: &str) -> Result<Option<String>, Unported> {
    let trimmed = crate::java::trim(raw);
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(date::to_calendar(trimmed)?.map(format_instant))
}

/// `valueFor`: a date property's value normalized when it reads as a date.
fn value_for(property: &Property, value: &str) -> Result<String, Unported> {
    if property.date {
        if let Some(normalized) = normalize_date(value)? {
            return Ok(normalized);
        }
    }
    Ok(value.to_owned())
}

/// `Metadata.set(Property, String)`.
pub(super) fn set(metadata: &mut TikaMetadata, property: &Property, value: &str) {
    metadata.set(property.name, value);
    for secondary in property.secondary {
        set(metadata, secondary, value);
    }
}

/// `Metadata.add(Property, String)`: unported where Tika throws on a second value of a
/// single-valued property.
pub(super) fn add(
    metadata: &mut TikaMetadata,
    property: &Property,
    value: &str,
) -> Result<(), Unported> {
    if !property.secondary.is_empty() {
        add(
            metadata,
            &Property {
                secondary: &[],
                ..*property
            },
            value,
        )?;
        for secondary in property.secondary {
            add(metadata, secondary, value)?;
        }
        return Ok(());
    }
    if metadata.values(property.name).is_empty() {
        metadata.set(property.name, value);
    } else if property.multi {
        metadata.add(property.name, value);
    } else {
        return Err(Unported(format!(
            "a second value for the single-valued property {}, which Tika throws on",
            property.name
        )));
    }
    Ok(())
}

/// `emit`.
fn emit(metadata: &mut TikaMetadata, property: &Property, value: &str) -> Result<(), Unported> {
    let value = value_for(property, value)?;
    if property.multi {
        add(metadata, property, &value)
    } else {
        set(metadata, property, &value);
        Ok(())
    }
}

/// `PDMetadataExtractor.extract`: the XMP packet `packet` mapped into `metadata`, and the
/// PDF/A version derived from it. A packet that is not well-formed XML is unported: Tika
/// records the parser's exception, with its stack trace, in the metadata.
pub(super) fn extract(packet: &[u8], metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let mut flattener = Flattener::default();
    xml::parse_sax(packet, "the XMP packet", &mut flattener).map_err(|failure| {
        Unported(format!(
            "an XMP packet that is not well-formed, whose exception Tika records: {failure:?}"
        ))
    })?;
    map(&flattener.leaves, metadata)?;
    if let Some(conformance) = metadata.get("pdfaid:conformance").map(str::to_owned) {
        let part = metadata
            .get("pdfaid:part")
            .map_or_else(|| "UNKNOWN".to_owned(), str::to_owned);
        metadata.set(
            "pdfa:pdf-version",
            format!("A-{part}{}", conformance.to_lowercase()),
        );
    }
    Ok(())
}

fn is_history_event(leaf: &Leaf) -> bool {
    leaf.namespace == RESOURCE_EVENT_NS
        && leaf.path.contains("History[")
        && HISTORY.iter().any(|(name, _)| *name == leaf.local_name())
}

fn is_lang_alternative(leaf: &Leaf) -> bool {
    leaf.lang.as_deref().is_some_and(|lang| !lang.is_empty())
        && !leaf.path.contains('/')
        && table(&leaf.namespace, leaf.local_name()).is_some()
}

fn history_index(path: &str) -> Option<i32> {
    let start = path.find("History[")? + "History[".len();
    let end = start + path[start..].find(']')?;
    crate::java::parse_int(&path[start..end])
}

/// `XmpExtractor.map`.
fn map(leaves: &[Leaf], metadata: &mut TikaMetadata) -> Result<(), Unported> {
    emit_history(leaves, metadata)?;
    emit_lang_alternatives(leaves, metadata)?;
    for leaf in leaves {
        if !is_history_event(leaf) && !is_lang_alternative(leaf) {
            map_leaf(leaf, metadata)?;
        }
    }
    Ok(())
}

fn emit_history(leaves: &[Leaf], metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let mut events: BTreeMap<i32, HashMap<&'static str, String>> = BTreeMap::new();
    for leaf in leaves.iter().filter(|leaf| is_history_event(leaf)) {
        let Some(index) = history_index(&leaf.path).filter(|&index| index >= 0) else {
            continue;
        };
        let mut value = decode(&leaf.value);
        if value.is_empty() {
            continue;
        }
        let local = leaf.local_name();
        if local == "when" {
            if let Some(normalized) = normalize_date(&value)? {
                value = normalized;
            }
        }
        let Some((_, property)) = HISTORY.iter().find(|(name, _)| *name == local) else {
            continue;
        };
        events.entry(index).or_default().insert(property, value);
    }
    let present: Vec<&'static str> = HISTORY
        .iter()
        .map(|(_, property)| *property)
        .filter(|property| events.values().any(|event| event.contains_key(property)))
        .collect();
    for event in events.values().take(MAX_HISTORY_EVENTS) {
        for property in &present {
            let value = event.get(property).map_or("", String::as_str);
            add(metadata, &bag(property), value)?;
        }
    }
    Ok(())
}

fn emit_lang_alternatives(leaves: &[Leaf], metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let mut keys: Vec<(String, String)> = Vec::new();
    let mut groups: HashMap<(String, String), Vec<&Leaf>> = HashMap::new();
    for leaf in leaves.iter().filter(|leaf| is_lang_alternative(leaf)) {
        let key = (leaf.namespace.clone(), leaf.local_name().to_owned());
        if !groups.contains_key(&key) {
            keys.push(key.clone());
        }
        groups.entry(key).or_default().push(leaf);
    }
    for key in keys {
        let alternatives = &groups[&key];
        let primary = alternatives
            .iter()
            .find(|leaf| leaf.lang.as_deref() == Some("x-default"))
            .unwrap_or(&alternatives[0]);
        let primary_value = decode(&primary.value);
        let properties = table(&key.0, &key.1).unwrap_or(&[]);
        for property in properties {
            if property.multi {
                if !primary_value.is_empty() {
                    emit(metadata, property, &primary_value)?;
                }
                for alternative in alternatives {
                    if std::ptr::eq(*alternative, *primary) {
                        continue;
                    }
                    let value = decode(&alternative.value);
                    if !value.is_empty() {
                        emit(metadata, property, &value)?;
                    }
                }
            } else if !primary_value.is_empty() {
                emit(metadata, property, &primary_value)?;
            }
            for alternative in alternatives {
                let value = decode(&alternative.value);
                if !value.is_empty() {
                    let lang = alternative.lang.as_deref().unwrap_or_default();
                    metadata.add(
                        &format!("{}:{lang}", property.name),
                        value_for(property, &value)?,
                    );
                }
            }
        }
    }
    Ok(())
}

fn map_leaf(leaf: &Leaf, metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let namespace = leaf.namespace.as_str();
    let local = leaf.local_name();
    if namespace == XMLNS_XML || dropped(namespace, local) {
        return Ok(());
    }
    let value = decode(&leaf.value);
    if value.is_empty() {
        return Ok(());
    }
    if namespace == RESOURCE_REF_NS && leaf.path.contains("DerivedFrom") {
        if let Some(property) = derived_from(local) {
            return emit(metadata, &property, &value);
        }
    }
    if namespace == GOOGLE_CAMERA {
        if let Some(name) = google_camera(local) {
            if metadata.get(name).is_none() {
                metadata.set(name, value);
            }
            return Ok(());
        }
    }
    let top_level = !leaf.path.contains('/');
    if let Some(fill) = fill_if_absent(namespace, local) {
        if top_level && metadata.get(fill.name).is_none() {
            let value = value_for(&fill, &value)?;
            set(metadata, &fill, &value);
        }
    }
    if let (true, Some(properties)) = (top_level, table(namespace, local)) {
        for property in properties {
            emit(metadata, property, &value)?;
        }
        return Ok(());
    }
    let name = strip_trailing_index(&leaf.path);
    if name.is_empty() || name.chars().all(crate::unicode::is_whitespace) {
        return Ok(());
    }
    if name.encode_utf16().count() > 1024 {
        return Ok(());
    }
    metadata.add(&format!("{RAW_PREFIX}{name}"), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET: &[u8] = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:pdf="http://ns.adobe.com/pdf/1.3/" xmlns:dc="http://purl.org/dc/elements/1.1/">
   <xmp:CreatorTool>Writer</xmp:CreatorTool>
   <xmp:CreateDate>2026-10-13T21:30:18+02:00</xmp:CreateDate>
   <pdf:Producer>LibreOffice</pdf:Producer>
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">A title</rdf:li></rdf:Alt></dc:title>
   <dc:creator><rdf:Seq><rdf:li>One</rdf:li><rdf:li>Two</rdf:li></rdf:Seq></dc:creator>
   <pdf:Unmapped>raw</pdf:Unmapped>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    #[test]
    fn maps_xmp_as_tika_does() {
        let mut metadata = TikaMetadata::new();
        extract(PACKET, &mut metadata).unwrap();
        assert_eq!(metadata.get("xmp:CreatorTool"), Some("Writer"));
        assert_eq!(metadata.get("xmp:CreateDate"), Some("2026-10-13T19:30:18Z"));
        assert_eq!(
            metadata.get("dcterms:created"),
            Some("2026-10-13T19:30:18Z")
        );
        assert_eq!(metadata.get("pdf:producer"), Some("LibreOffice"));
        assert_eq!(metadata.get("xmp:pdf:Producer"), Some("LibreOffice"));
        assert_eq!(metadata.values("dc:title"), ["A title"]);
        assert_eq!(metadata.get("dc:title:x-default"), Some("A title"));
        assert_eq!(metadata.values("dc:creator"), ["One", "Two"]);
        assert_eq!(metadata.values("xmp:dc:creator"), ["One", "Two"]);
        assert_eq!(metadata.get("xmp-raw:pdf:Unmapped"), Some("raw"));
    }

    #[test]
    fn decodes_pdf_encoded_values() {
        assert_eq!(decode("\\376\\377\\000A\\000B"), "AB");
        assert_eq!(decode("plain text"), "plain text");
    }

    #[test]
    fn strips_trailing_array_indices() {
        assert_eq!(strip_trailing_index("dc:subject[2]"), "dc:subject");
        assert_eq!(strip_trailing_index("a/b"), "a/b");
    }
}
