//! metadata-extractor's `XmpReader.extract` of the XMP a TIFF tag holds: an `XmpDirectory` whose
//! one tag, the XMP Value Count, counts the leaf nodes of the tree Adobe's XMPCore 6.1.11 parses
//! the packet into, with an error in its place where XMPCore throws.
//!
//! The count is all the packet shows of that tree, so the port builds only its shape: the DOM
//! XMPCore reads (`XMPMetaParser`), the nodes `ParseRDF` makes of it, and the changes
//! `XMPNormalizer` makes to them. The packets XMPCore re-reads after fixing their bytes are read
//! here only when they are UTF-8 throughout. Two cases are left to Tika's path: a simple
//! `exif:UserComment` with qualifiers, on which XMPCore throws an exception XmpReader does not
//! catch, and an `xmpDM:copyright` beside an existing `dc:rights`, which XMPCore merges into it.

use std::cmp::Ordering;

use super::{java_string, Directory, Kind, Metadata, Unported, Value};
use crate::text::charset::Charset;
use crate::xml::{self, Attribute, Event, Flow, TextKind, XMLNS};

/// `XmpDirectory.TAG_XMP_VALUE_COUNT`.
pub(super) const TAG_XMP_VALUE_COUNT: i32 = 0xffff;

const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";
const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_DC_DEPRECATED: &str = "http://purl.org/dc/1.1/";
const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
const NS_XMP_RIGHTS: &str = "http://ns.adobe.com/xap/1.0/rights/";
const NS_XMP_MM: &str = "http://ns.adobe.com/xap/1.0/mm/";
const NS_PDF: &str = "http://ns.adobe.com/pdf/1.3/";
const NS_PHOTOSHOP: &str = "http://ns.adobe.com/photoshop/1.0/";
const NS_EXIF: &str = "http://ns.adobe.com/exif/1.0/";
const NS_TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
const NS_PNG: &str = "http://ns.adobe.com/png/1.0/";
const NS_DM: &str = "http://ns.adobe.com/xmp/1.0/DynamicMedia/";
const XML_LANG: &str = "xml:lang";
const X_DEFAULT: &str = "x-default";
/// `XmpReader.PARSE_OPTIONS`: the items of `photoshop:DocumentAncestors` read, by the index of
/// the array element's DOM children.
const DOCUMENT_ANCESTORS_LIMIT: usize = 1000;

/// The bags, sequences and alternative texts `XMPNormalizer.initDCArrays` makes these Dublin
/// Core properties.
const DC_ARRAYS: [(&str, Form); 11] = [
    ("contributor", Form::BAG),
    ("language", Form::BAG),
    ("publisher", Form::BAG),
    ("relation", Form::BAG),
    ("subject", Form::BAG),
    ("type", Form::BAG),
    ("creator", Form::SEQ),
    ("date", Form::SEQ),
    ("description", Form::ALT_TEXT),
    ("rights", Form::ALT_TEXT),
    ("title", Form::ALT_TEXT),
];

/// `XMPSchemaRegistryImpl.registerStandardAliases`: an alias, the property it stands for, and
/// the form of that property, simple or an array whose first item the alias is.
const ALIASES: [(&str, &str, &str, &str, Form); 35] = [
    (NS_XMP, "Author", NS_DC, "creator", Form::SEQ),
    (NS_XMP, "Authors", NS_DC, "creator", Form::SIMPLE),
    (NS_XMP, "Description", NS_DC, "description", Form::SIMPLE),
    (NS_XMP, "Format", NS_DC, "format", Form::SIMPLE),
    (NS_XMP, "Keywords", NS_DC, "subject", Form::SIMPLE),
    (NS_XMP, "Locale", NS_DC, "language", Form::SIMPLE),
    (NS_XMP, "Title", NS_DC, "title", Form::SIMPLE),
    (NS_XMP_RIGHTS, "Copyright", NS_DC, "rights", Form::SIMPLE),
    (NS_PDF, "Author", NS_DC, "creator", Form::SEQ),
    (NS_PDF, "BaseURL", NS_XMP, "BaseURL", Form::SIMPLE),
    (NS_PDF, "CreationDate", NS_XMP, "CreateDate", Form::SIMPLE),
    (NS_PDF, "Creator", NS_XMP, "CreatorTool", Form::SIMPLE),
    (NS_PDF, "ModDate", NS_XMP, "ModifyDate", Form::SIMPLE),
    (NS_PDF, "Subject", NS_DC, "description", Form::ALT_TEXT),
    (NS_PDF, "Title", NS_DC, "title", Form::ALT_TEXT),
    (NS_PHOTOSHOP, "Author", NS_DC, "creator", Form::SEQ),
    (
        NS_PHOTOSHOP,
        "Caption",
        NS_DC,
        "description",
        Form::ALT_TEXT,
    ),
    (NS_PHOTOSHOP, "Copyright", NS_DC, "rights", Form::ALT_TEXT),
    (NS_PHOTOSHOP, "Keywords", NS_DC, "subject", Form::SIMPLE),
    (
        NS_PHOTOSHOP,
        "Marked",
        NS_XMP_RIGHTS,
        "Marked",
        Form::SIMPLE,
    ),
    (NS_PHOTOSHOP, "Title", NS_DC, "title", Form::ALT_TEXT),
    (
        NS_PHOTOSHOP,
        "WebStatement",
        NS_XMP_RIGHTS,
        "WebStatement",
        Form::SIMPLE,
    ),
    (NS_TIFF, "Artist", NS_DC, "creator", Form::SEQ),
    (NS_TIFF, "Copyright", NS_DC, "rights", Form::SIMPLE),
    (NS_TIFF, "DateTime", NS_XMP, "ModifyDate", Form::SIMPLE),
    (
        NS_EXIF,
        "DateTimeDigitized",
        NS_XMP,
        "CreateDate",
        Form::SIMPLE,
    ),
    (
        NS_TIFF,
        "ImageDescription",
        NS_DC,
        "description",
        Form::SIMPLE,
    ),
    (NS_TIFF, "Software", NS_XMP, "CreatorTool", Form::SIMPLE),
    (NS_PNG, "Author", NS_DC, "creator", Form::SEQ),
    (NS_PNG, "Copyright", NS_DC, "rights", Form::ALT_TEXT),
    (NS_PNG, "CreationTime", NS_XMP, "CreateDate", Form::SIMPLE),
    (NS_PNG, "Description", NS_DC, "description", Form::ALT_TEXT),
    (
        NS_PNG,
        "ModificationTime",
        NS_XMP,
        "ModifyDate",
        Form::SIMPLE,
    ),
    (NS_PNG, "Software", NS_XMP, "CreatorTool", Form::SIMPLE),
    (NS_PNG, "Title", NS_DC, "title", Form::ALT_TEXT),
];

/// `XmpReader.extract`: the directory of the packet `bytes`, its value count or the error
/// XMPCore threw.
pub(super) fn extract(bytes: &[u8], metadata: &mut Metadata) -> Result<(), Unported> {
    let mut directory = Directory::new(Kind::Xmp);
    match value_count(bytes) {
        Ok(count) => directory.set(TAG_XMP_VALUE_COUNT, Value::Int(count)),
        Err(Failure::Xmp(message)) => {
            directory.add_error(format!("Error processing XMP data: {message}"))
        }
        Err(Failure::Unported(unported)) => return Err(unported),
    }
    metadata.add(directory);
    Ok(())
}

/// Why no count is made: XMPCore throws an `XMPException` with this message, or the packet is
/// one the port leaves to Tika's path.
#[derive(Debug)]
enum Failure {
    Xmp(String),
    Unported(Unported),
}

impl From<Unported> for Failure {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

fn bad<T>(message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Xmp(message.into()))
}

/// `XMPMetaFactory.parseFromBuffer` and `XmpDirectory.setXMPMeta`: the leaf nodes of the
/// packet's tree, none where it holds no `rdf:RDF`.
fn value_count(bytes: &[u8]) -> Result<i32, Failure> {
    let document = match dom(bytes) {
        Some(document) => document,
        None => reparsed(bytes)?,
    };
    let Some(rdf) = rdf_root(&document) else {
        return Ok(0);
    };
    let mut tree = Tree::default();
    rdf_rdf(&mut tree, rdf)?;
    normalize(&mut tree)?;
    let count: usize = tree
        .schemas
        .iter()
        .flat_map(|schema| &schema.properties)
        .map(XmpNode::leaves)
        .sum();
    Ok(i32::try_from(count).expect("a packet holds fewer than 2^31 nodes"))
}

/// A node of the DOM `XMPMetaParser` builds: without comments, its character data and references
/// in one text node, and CDATA sections and processing instructions of their own.
#[derive(Debug)]
enum Dom {
    Element(Element),
    Text(String),
    CData,
    Instruction,
}

#[derive(Debug)]
struct Element {
    namespace: String,
    local: String,
    qualified: String,
    /// In the order of their qualified names, as Xerces's attribute map keeps them, namespace
    /// declarations among them.
    attributes: Vec<Attribute>,
    children: Vec<Dom>,
}

impl Element {
    /// The attributes other than namespace declarations.
    fn plain_attributes(&self) -> impl Iterator<Item = &Attribute> {
        self.attributes.iter().filter(|a| a.namespace != XMLNS)
    }
}

/// The DOM of the XML document `bytes`, or none where Xerces fails on it, a document type
/// declaration among the failures.
fn dom(bytes: &[u8]) -> Option<Vec<Dom>> {
    let mut stack: Vec<Element> = Vec::new();
    let mut document = Vec::new();
    let mut document_type = false;
    let parsed = xml::parse(bytes, &mut |event| {
        match event {
            Event::DocumentType => {
                document_type = true;
                return Flow::Stop;
            }
            Event::Start(name, attributes) => {
                let mut attributes = attributes.to_vec();
                attributes.sort_by(|a, b| utf16_order(&a.qualified_name, &b.qualified_name));
                stack.push(Element {
                    namespace: name.namespace().to_owned(),
                    local: name.local_name().to_owned(),
                    qualified: name.qualified_name().to_owned(),
                    attributes,
                    children: Vec::new(),
                });
            }
            Event::End(_) => {
                let element = stack.pop().expect("an end tag ends a started element");
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Dom::Element(element)),
                    None => document.push(Dom::Element(element)),
                }
            }
            Event::Text(text, TextKind::Data | TextKind::Reference) => {
                if let Some(element) = stack.last_mut() {
                    match element.children.last_mut() {
                        Some(Dom::Text(previous)) => previous.push_str(text),
                        _ => element.children.push(Dom::Text(text.to_owned())),
                    }
                }
            }
            Event::Text(_, TextKind::CData) => {
                if let Some(element) = stack.last_mut() {
                    element.children.push(Dom::CData);
                }
            }
            Event::ProcessingInstruction(..) => match stack.last_mut() {
                Some(element) => element.children.push(Dom::Instruction),
                None => document.push(Dom::Instruction),
            },
            Event::Comment(_) => {}
        }
        Flow::Continue
    });
    (parsed.is_ok() && !document_type).then_some(document)
}

/// The order of Java's `String.compareTo`: by UTF-16 code units.
fn utf16_order(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `XMPMetaParser.parseXmlFromBytebuffer` after a failed parse: the bytes read again with
/// `Latin1Converter` and `FixASCIIControlsReader`, as UTF-8.
fn reparsed(bytes: &[u8]) -> Result<Vec<Dom>, Failure> {
    let unported = |what: &str| {
        Failure::Unported(Unported::new(format!(
            "XMP that XMPCore re-reads after a failed parse, {what}"
        )))
    };
    if !read_as_utf8(bytes) {
        return Err(unported("not in UTF-8"));
    }
    let converted = latin1_converted(bytes);
    if !read_as_utf8(&converted) {
        return Err(unported("not in UTF-8 once converted"));
    }
    let text = fixed_controls(&java_string(&converted, None));
    if text.starts_with('\u{feff}') {
        return bad("XML parsing failure");
    }
    if declares_other_encoding(&text) {
        return Err(unported("declaring an encoding other than UTF-8"));
    }
    match dom(text.as_bytes()) {
        Some(document) => Ok(document),
        None => bad("XML parsing failure"),
    }
}

/// Whether `ByteBuffer.getEncoding` finds the bytes in UTF-8.
fn read_as_utf8(bytes: &[u8]) -> bool {
    match bytes {
        [] | [_] => true,
        [0xef, ..] => true,
        [first, second, ..] => *first != 0 && *first < 0x80 && *second != 0,
    }
}

/// Whether the text starts with an XML declaration naming an encoding other than UTF-8, which
/// Xerces ignores in text it reads from characters.
fn declares_other_encoding(text: &str) -> bool {
    let Some(declaration) = text
        .strip_prefix("<?xml")
        .and_then(|rest| rest.split_once("?>"))
        .map(|(declaration, _)| declaration)
    else {
        return false;
    };
    let Some((_, after)) = declaration.split_once("encoding") else {
        return false;
    };
    let value = after
        .trim_start()
        .trim_start_matches('=')
        .trim_start()
        .trim_start_matches(['"', '\''])
        .split(['"', '\''])
        .next()
        .unwrap_or_default();
    !value.eq_ignore_ascii_case("UTF-8")
}

/// `Latin1Converter.convert`: bytes that are not UTF-8 read as Windows-1252, the five it leaves
/// undefined as spaces.
fn latin1_converted(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 4 / 3);
    let mut pending: Vec<u8> = Vec::with_capacity(8);
    let mut expected = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if pending.is_empty() {
            if byte < 0x7f {
                out.push(byte);
            } else if byte >= 0xc0 {
                expected = (byte.leading_ones() as usize).saturating_sub(1);
                pending.push(byte);
            } else {
                push_cp1252(&mut out, byte);
            }
        } else if expected > 0 && byte & 0xc0 == 0x80 {
            pending.push(byte);
            expected -= 1;
            if expected == 0 {
                out.append(&mut pending);
            }
        } else {
            push_cp1252(&mut out, pending[0]);
            i -= pending.len();
            pending.clear();
        }
        i += 1;
    }
    for byte in pending {
        push_cp1252(&mut out, byte);
    }
    out
}

/// `Latin1Converter.convertToUTF8`.
fn push_cp1252(out: &mut Vec<u8>, byte: u8) {
    if byte < 0x80 {
        out.push(byte);
        return;
    }
    if matches!(byte, 0x81 | 0x8d | 0x8f | 0x90 | 0x9d) {
        out.push(b' ');
        return;
    }
    let text = java_string(&[byte], Some(Charset::named("windows-1252")));
    out.extend_from_slice(text.as_bytes());
}

/// `Utils.isControlChar`.
fn is_control(c: u32) -> bool {
    (c <= 0x1f || c == 0x7f) && c != 0x09 && c != 0x0a && c != 0x0d
}

/// `FixASCIIControlsReader`: control characters, and references to them, as spaces.
fn fixed_controls(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&' {
            if let Some(length) = control_reference(&chars[i..]) {
                out.push(' ');
                i += length;
                continue;
            }
            out.push('&');
        } else if is_control(u32::from(chars[i])) {
            out.push(' ');
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    out
}

/// The length of a reference to a control character at the start of `chars`: `&#` and up to
/// five decimal digits or `&#x` and up to four hexadecimal ones, then `;`, read as a `char`.
fn control_reference(chars: &[char]) -> Option<usize> {
    if chars.get(1) != Some(&'#') {
        return None;
    }
    let (radix, start, max_digits) = match chars.get(2) {
        Some('x') => (16, 3, 4),
        Some(c) if c.is_ascii_digit() => (10, 2, 5),
        _ => return None,
    };
    let mut control = 0u32;
    let mut at = start;
    while let Some(digit) = chars.get(at).and_then(|c| c.to_digit(radix)) {
        if at - start == max_digits {
            return None;
        }
        control = control * radix + digit;
        at += 1;
    }
    (chars.get(at) == Some(&';') && is_control(control & 0xffff)).then_some(at + 1)
}

/// `XMPMetaParser.findRootNode`: the first `rdf:RDF` element, in document order.
fn rdf_root(nodes: &[Dom]) -> Option<&Element> {
    nodes.iter().find_map(|node| match node {
        Dom::Element(element) if element.namespace == NS_RDF && element.local == "RDF" => {
            Some(element)
        }
        Dom::Element(element) => rdf_root(&element.children),
        _ => None,
    })
}

/// A node's options that tell its shape, as `PropertyOptions` keeps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Form {
    structure: bool,
    array: bool,
    ordered: bool,
    alternate: bool,
    alt_text: bool,
    uri: bool,
}

impl Form {
    const SIMPLE: Self = Self {
        structure: false,
        array: false,
        ordered: false,
        alternate: false,
        alt_text: false,
        uri: false,
    };
    const BAG: Self = Self {
        array: true,
        ..Self::SIMPLE
    };
    const SEQ: Self = Self {
        ordered: true,
        ..Self::BAG
    };
    const ALT_TEXT: Self = Self {
        alternate: true,
        alt_text: true,
        ..Self::SEQ
    };

    fn is_simple(self) -> bool {
        !self.structure && !self.array
    }

    /// `mergeWith`: both forms' options, which `assertConsistency` checks.
    fn merged(self, other: Self) -> Result<Self, Failure> {
        let merged = Self {
            structure: self.structure || other.structure,
            array: self.array || other.array,
            ordered: self.ordered || other.ordered,
            alternate: self.alternate || other.alternate,
            alt_text: self.alt_text || other.alt_text,
            uri: self.uri || other.uri,
        };
        if merged.structure && merged.array {
            return bad("IsStruct and IsArray options are mutually exclusive");
        }
        if merged.uri && (merged.array || merged.structure) {
            return bad("Structs and arrays can't have \"value\" options");
        }
        Ok(merged)
    }
}

/// A node's name: a property or field, an array item, or a qualifier's.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Name {
    Property {
        namespace: String,
        local: String,
    },
    Item,
    /// `xml:lang`.
    Lang,
    /// `rdf:type`.
    Type,
    /// A qualifier named as an attribute is written, with its prefix.
    Written(String),
}

impl Name {
    fn property(namespace: &str, local: &str) -> Self {
        Self::Property {
            namespace: namespace.to_owned(),
            local: local.to_owned(),
        }
    }

    /// A qualifier's name written `qualified`, a language or type by its name alone, as
    /// `XMPNode.isLanguageNode` and `isTypeNode` tell them.
    fn written(qualified: &str) -> Self {
        match qualified {
            XML_LANG => Self::Lang,
            "rdf:type" => Self::Type,
            _ => Self::Written(qualified.to_owned()),
        }
    }

    /// A field's name as a qualifier: XMPCore names it with the prefix it registered for its
    /// namespace, `xml:` and `rdf:` for these.
    fn into_qualifier(self) -> Self {
        match &self {
            Self::Property { namespace, local } if namespace == NS_XML && local == "lang" => {
                Self::Lang
            }
            Self::Property { namespace, local } if namespace == NS_RDF && local == "type" => {
                Self::Type
            }
            _ => self,
        }
    }

    fn is(&self, namespace: &str, local: &str) -> bool {
        matches!(self, Self::Property { namespace: n, local: l } if n == namespace && l == local)
    }

    /// Whether `XMPNode.assertQualifierNotExisting` takes the names as the same. A written name
    /// and a field's are the same when the field's registered prefix is the written one, which
    /// depends on the namespaces the JVM registered before: such a pair is unported.
    fn same_qualifier(&self, other: &Self) -> Result<bool, Unported> {
        match (self, other) {
            (Self::Written(written), Self::Property { local, .. })
            | (Self::Property { local, .. }, Self::Written(written)) => {
                match written.rsplit_once(':').map(|(_, written)| written) == Some(local.as_str()) {
                    true => Err(Unported::new(
                        "an XMP qualifier named like a field, which XMPCore tells apart by the \
                         prefixes the JVM registered",
                    )),
                    false => Ok(false),
                }
            }
            _ => Ok(self == other),
        }
    }
}

/// An `XMPNode`, with what of it decides its shape.
#[derive(Debug, Clone)]
struct XmpNode {
    name: Name,
    value: Option<String>,
    form: Form,
    children: Vec<XmpNode>,
    qualifiers: Vec<XmpNode>,
    alias: bool,
    has_value_child: bool,
    /// `setArrayElementLimit`: the index of the last DOM child read as an item.
    limit: Option<usize>,
}

impl XmpNode {
    fn new(name: Name, value: Option<String>) -> Self {
        Self {
            name,
            value,
            form: Form::SIMPLE,
            children: Vec::new(),
            qualifiers: Vec::new(),
            alias: false,
            has_value_child: false,
            limit: None,
        }
    }

    fn has_language(&self) -> bool {
        self.qualifiers.iter().any(|q| q.name == Name::Lang)
    }

    /// The leaf nodes `XMPIteratorImpl` reports with a path: the node without children, and the
    /// leaves of its children and of its qualifiers.
    fn leaves(&self) -> usize {
        usize::from(self.children.is_empty())
            + self.children.iter().map(Self::leaves).sum::<usize>()
            + self.qualifiers.iter().map(Self::leaves).sum::<usize>()
    }

    /// `addChild`: fails on a second property or field of the same name.
    fn add_child(children: &mut Vec<XmpNode>, index: usize, node: XmpNode) -> Result<(), Failure> {
        if let Name::Property { namespace, local } = &node.name {
            if children.iter().any(|child| child.name == node.name) {
                return bad(format!(
                    "Duplicate property or field node '{namespace}{local}'"
                ));
            }
        }
        children.insert(index, node);
        Ok(())
    }

    /// `addQualifier`: a language first, a type after it, others last.
    fn add_qualifier(&mut self, qualifier: XmpNode) -> Result<(), Failure> {
        for existing in &self.qualifiers {
            if existing.name.same_qualifier(&qualifier.name)? {
                return bad(format!("Duplicate '{:?}' qualifier", qualifier.name));
            }
        }
        let index = match qualifier.name {
            Name::Lang => 0,
            Name::Type => usize::from(self.has_language()),
            _ => self.qualifiers.len(),
        };
        self.qualifiers.insert(index, qualifier);
        Ok(())
    }

    /// `ParseRDF.addQualifierNode`: a language's value normalized.
    fn add_qualifier_value(&mut self, name: Name, value: &str) -> Result<(), Failure> {
        let value = match name {
            Name::Lang => normalized_language(value),
            _ => value.to_owned(),
        };
        self.add_qualifier(XmpNode::new(name, Some(value)))
    }
}

/// `Utils.normalizeLangValue`: subtags joined by hyphens without spaces, the second upper case
/// and the others lower case; `x-default` as it is.
fn normalized_language(value: &str) -> String {
    if value == X_DEFAULT {
        return value.to_owned();
    }
    let mut subtag = 1;
    let mut normalized = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '-' | '_' => {
                normalized.push('-');
                subtag += 1;
            }
            ' ' => {}
            _ => {
                let mut mapped = match subtag {
                    2 => c.to_uppercase().collect::<String>(),
                    _ => c.to_lowercase().collect::<String>(),
                };
                if mapped.chars().count() != 1 {
                    mapped = c.to_string();
                }
                normalized.push_str(&mapped);
            }
        }
    }
    normalized
}

/// A schema node: the properties of one namespace.
#[derive(Debug, Default)]
struct Schema {
    namespace: String,
    properties: Vec<XmpNode>,
    has_aliases: bool,
}

/// The root of an `XMPMetaImpl`: the `rdf:about` it was named with, and its schemas.
#[derive(Debug, Default)]
struct Tree {
    name: Option<String>,
    schemas: Vec<Schema>,
    has_aliases: bool,
}

impl Tree {
    /// `XMPNodeUtils.findSchemaNode` creating the schema it does not find.
    fn schema(&mut self, namespace: &str) -> usize {
        match self.schemas.iter().position(|s| s.namespace == namespace) {
            Some(index) => index,
            None => {
                self.schemas.push(Schema {
                    namespace: namespace.to_owned(),
                    ..Schema::default()
                });
                self.schemas.len() - 1
            }
        }
    }
}

/// Where `ParseRDF` adds nodes: to the schemas of the tree at the top level, or to a node.
enum Target<'a> {
    Top(&'a mut Tree),
    Node(&'a mut XmpNode),
}

impl Target<'_> {
    fn is_top(&self) -> bool {
        matches!(self, Target::Top(_))
    }

    /// `ParseRDF.addChildNode`: a property, field or item of the namespace and local name,
    /// checked against its parent.
    fn add_child(
        &mut self,
        namespace: &str,
        local: &str,
        value: Option<String>,
    ) -> Result<&mut XmpNode, Failure> {
        if namespace.is_empty() {
            return bad("XML namespace required for all elements and attributes");
        }
        let namespace = match namespace {
            NS_DC_DEPRECATED => NS_DC,
            _ => namespace,
        };
        let is_array_item = namespace == NS_RDF
            && (local == "li"
                || local
                    .strip_prefix('_')
                    .is_some_and(|digits| digits.bytes().all(|b| b.is_ascii_digit())));
        let is_value = namespace == NS_RDF && local == "value";
        let mut node = XmpNode::new(Name::property(namespace, local), value);
        let (children, parent_is_array) = match self {
            Target::Top(tree) => {
                let schema = tree.schema(namespace);
                if ALIASES.iter().any(|a| a.0 == namespace && a.1 == local) {
                    node.alias = true;
                    tree.has_aliases = true;
                    tree.schemas[schema].has_aliases = true;
                }
                if is_value {
                    return bad("Misplaced rdf:value element");
                }
                (&mut tree.schemas[schema].properties, false)
            }
            Target::Node(parent) => {
                if is_value {
                    if !parent.form.structure {
                        return bad("Misplaced rdf:value element");
                    }
                    parent.has_value_child = true;
                }
                let is_array = parent.form.array;
                (&mut parent.children, is_array)
            }
        };
        let index = match is_value {
            true => 0,
            false => children.len(),
        };
        XmpNode::add_child(children, index, node)?;
        match (parent_is_array, is_array_item) {
            (true, true) => children[index].name = Name::Item,
            (false, true) => return bad("Misplaced rdf:li element"),
            (true, false) => return bad("Arrays cannot have arbitrary child names"),
            (false, false) => {}
        }
        Ok(&mut children[index])
    }
}

/// The RDF terms `ParseRDF.getRDFTermKind` tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Term {
    Other,
    Rdf,
    Id,
    About,
    ParseType,
    Resource,
    NodeId,
    Datatype,
    Description,
    Li,
    AboutEach,
    AboutEachPrefix,
    BagId,
}

impl Term {
    fn of(namespace: &str, local: &str) -> Self {
        if namespace != NS_RDF {
            return Self::Other;
        }
        match local {
            "li" => Self::Li,
            "parseType" => Self::ParseType,
            "Description" => Self::Description,
            "about" => Self::About,
            "resource" => Self::Resource,
            "RDF" => Self::Rdf,
            "ID" => Self::Id,
            "nodeID" => Self::NodeId,
            "datatype" => Self::Datatype,
            "aboutEach" => Self::AboutEach,
            "aboutEachPrefix" => Self::AboutEachPrefix,
            "bagID" => Self::BagId,
            _ => Self::Other,
        }
    }

    /// An attribute's term: an unprefixed `about` or `ID` of an element of RDF's namespace is
    /// RDF's.
    fn of_attribute(attribute: &Attribute, owner: &Element) -> Self {
        let rdf_unprefixed = attribute.namespace.is_empty()
            && matches!(attribute.local_name.as_str(), "about" | "ID")
            && owner.namespace == NS_RDF;
        match rdf_unprefixed {
            true => Self::of(NS_RDF, &attribute.local_name),
            false => Self::of(&attribute.namespace, &attribute.local_name),
        }
    }

    fn of_node(node: &Dom) -> Self {
        match node {
            Dom::Element(element) => Self::of(&element.namespace, &element.local),
            _ => Self::Other,
        }
    }

    /// `isPropertyElementName`: neither `rdf:Description`, an old term nor a core one.
    fn names_property(self) -> bool {
        !matches!(
            self,
            Self::Description
                | Self::AboutEach
                | Self::AboutEachPrefix
                | Self::BagId
                | Self::Rdf
                | Self::Id
                | Self::About
                | Self::ParseType
                | Self::Resource
                | Self::NodeId
                | Self::Datatype
        )
    }
}

/// `isWhitespaceNode`: a text node of `Character.isWhitespace` characters alone.
fn is_whitespace(node: &Dom) -> bool {
    matches!(node, Dom::Text(text) if text.chars().all(crate::unicode::is_whitespace))
}

fn is_rdf(attribute: &Attribute, local: &str) -> bool {
    attribute.namespace == NS_RDF && attribute.local_name == local
}

/// `rdf_RDF`: the node elements of `rdf:RDF`, which must have attributes.
fn rdf_rdf(tree: &mut Tree, rdf: &Element) -> Result<(), Failure> {
    if rdf.attributes.is_empty() {
        return bad("Invalid attributes of rdf:RDF element");
    }
    for child in &rdf.children {
        if !is_whitespace(child) {
            node_element(&mut Target::Top(tree), child)?;
        }
    }
    Ok(())
}

/// `rdf_NodeElement`: an `rdf:Description` or, below the top level, a typed node.
fn node_element(target: &mut Target, node: &Dom) -> Result<(), Failure> {
    let term = Term::of_node(node);
    if term != Term::Description && term != Term::Other {
        return bad("Node element must be rdf:Description or typed node");
    }
    let Dom::Element(element) = node else {
        return bad("Top level typed node not allowed");
    };
    if target.is_top() && term == Term::Other {
        return bad("Top level typed node not allowed");
    }
    node_element_attributes(target, element)?;
    property_element_list(target, element)
}

/// `rdf_NodeElementAttrs`: one of `rdf:about`, `rdf:ID` and `rdf:nodeID`, the top level's about
/// naming the tree, and properties.
fn node_element_attributes(target: &mut Target, element: &Element) -> Result<(), Failure> {
    let mut exclusive = 0;
    for attribute in element.plain_attributes() {
        match Term::of_attribute(attribute, element) {
            Term::Id | Term::NodeId | Term::About => {
                if exclusive > 0 {
                    return bad("Mutally exclusive about, ID, nodeID attributes");
                }
                exclusive += 1;
                if let (Target::Top(tree), Term::About) =
                    (&mut *target, Term::of_attribute(attribute, element))
                {
                    match tree.name.as_deref() {
                        Some(name) if !name.is_empty() => {
                            if name != attribute.value {
                                return bad("Mismatched top level rdf:about values");
                            }
                        }
                        _ => tree.name = Some(attribute.value.clone()),
                    }
                }
            }
            Term::Other => {
                target.add_child(
                    &attribute.namespace,
                    &attribute.local_name,
                    Some(attribute.value.clone()),
                )?;
            }
            _ => return bad("Invalid nodeElement attribute"),
        }
    }
    Ok(())
}

/// `rdf_PropertyElementList`: the property elements, up to the limit of an array that has one.
fn property_element_list(target: &mut Target, element: &Element) -> Result<(), Failure> {
    for (index, child) in element.children.iter().enumerate() {
        if is_whitespace(child) {
            continue;
        }
        let Dom::Element(child) = child else {
            return bad("Expected property element node not found");
        };
        if let Target::Node(parent) = target {
            if parent.limit.is_some_and(|limit| index > limit) {
                break;
            }
        }
        property_element(target, child)?;
    }
    Ok(())
}

/// `rdf_PropertyElement`: the kind of property element its attributes and children make it.
fn property_element(target: &mut Target, element: &Element) -> Result<(), Failure> {
    if !Term::of(&element.namespace, &element.local).names_property() {
        return bad("Invalid property element name");
    }
    let attributes: Vec<&Attribute> = element.plain_attributes().collect();
    if attributes.len() > 3 {
        return empty_property_element(target, element);
    }
    if let Some(attribute) = attributes.iter().find(|a| a.qualified_name != XML_LANG) {
        return if is_rdf(attribute, "datatype") {
            literal_property_element(target, element)
        } else if !is_rdf(attribute, "parseType") {
            empty_property_element(target, element)
        } else {
            match attribute.value.as_str() {
                "Literal" => bad("ParseTypeLiteral property element not allowed"),
                "Resource" => parse_type_resource_property_element(target, element),
                "Collection" => bad("ParseTypeCollection property element not allowed"),
                _ => bad("ParseTypeOther property element not allowed"),
            }
        };
    }
    if element.children.is_empty() {
        return empty_property_element(target, element);
    }
    match element.children.iter().all(|c| matches!(c, Dom::Text(_))) {
        true => literal_property_element(target, element),
        false => resource_property_element(target, element),
    }
}

/// `rdf_ResourcePropertyElement`: an array, or a struct of a description or typed node.
fn resource_property_element(target: &mut Target, element: &Element) -> Result<(), Failure> {
    if target.is_top() && element.qualified == "iX:changes" {
        return Ok(());
    }
    let compound = target.add_child(&element.namespace, &element.local, Some(String::new()))?;
    for attribute in element.plain_attributes() {
        if attribute.qualified_name == XML_LANG {
            compound.add_qualifier_value(Name::Lang, &attribute.value)?;
        } else if !is_rdf(attribute, "ID") {
            return bad("Invalid attribute for resource property element");
        }
    }
    let mut found = false;
    for child in &element.children {
        if is_whitespace(child) {
            continue;
        }
        match child {
            Dom::Element(node) if !found => {
                let in_rdf = node.namespace == NS_RDF;
                match (in_rdf, node.local.as_str()) {
                    (true, "Bag") => compound.form.array = true,
                    (true, "Seq") => {
                        compound.form.array = true;
                        compound.form.ordered = true;
                    }
                    (true, "Alt") => {
                        compound.form.array = true;
                        compound.form.ordered = true;
                        compound.form.alternate = true;
                    }
                    _ => {
                        compound.form.structure = true;
                        if !in_rdf && node.local != "Description" {
                            if node.namespace.is_empty() {
                                return bad("All XML elements must be in a namespace");
                            }
                            let type_name = format!("{}:{}", node.namespace, node.local);
                            compound.add_qualifier_value(Name::Type, &type_name)?;
                        }
                    }
                }
                if compound.form.array && compound.name.is(NS_PHOTOSHOP, "DocumentAncestors") {
                    compound.limit = Some(DOCUMENT_ANCESTORS_LIMIT);
                }
                node_element(&mut Target::Node(&mut *compound), child)?;
                if compound.has_value_child {
                    fixup_qualified_node(compound)?;
                } else if compound.form.alternate {
                    detect_alt_text(compound);
                }
                found = true;
            }
            _ if found => return bad("Invalid child of resource property element"),
            _ => return bad("Children of resource property element must be XML elements"),
        }
    }
    if !found {
        return bad("Missing child of resource property element");
    }
    Ok(())
}

/// `rdf_LiteralPropertyElement`: a simple value of text alone.
fn literal_property_element(target: &mut Target, element: &Element) -> Result<(), Failure> {
    let node = target.add_child(&element.namespace, &element.local, None)?;
    for attribute in element.plain_attributes() {
        if attribute.qualified_name == XML_LANG {
            node.add_qualifier_value(Name::Lang, &attribute.value)?;
        } else if !(is_rdf(attribute, "ID") || is_rdf(attribute, "datatype")) {
            return bad("Invalid attribute for literal property element");
        }
    }
    let mut value = String::new();
    for child in &element.children {
        match child {
            Dom::Text(text) => value.push_str(text),
            _ => return bad("Invalid child of literal property element"),
        }
    }
    node.value = Some(value);
    Ok(())
}

/// `rdf_ParseTypeResourcePropertyElement`: a struct of the property elements.
fn parse_type_resource_property_element(
    target: &mut Target,
    element: &Element,
) -> Result<(), Failure> {
    let structure = target.add_child(&element.namespace, &element.local, Some(String::new()))?;
    structure.form.structure = true;
    for attribute in element.plain_attributes() {
        if attribute.qualified_name == XML_LANG {
            structure.add_qualifier_value(Name::Lang, &attribute.value)?;
        } else if !(is_rdf(attribute, "ID") || is_rdf(attribute, "parseType")) {
            return bad("Invalid attribute for ParseTypeResource property element");
        }
    }
    property_element_list(&mut Target::Node(structure), element)?;
    if structure.has_value_child {
        fixup_qualified_node(structure)?;
    }
    Ok(())
}

/// `rdf_EmptyPropertyElement`: a value of `rdf:value` or `rdf:resource`, with the other
/// attributes as qualifiers, or a struct of the attributes.
fn empty_property_element(target: &mut Target, element: &Element) -> Result<(), Failure> {
    if !element.children.is_empty() {
        return bad("Nested content not allowed with rdf:resource or property attributes");
    }
    let attributes: Vec<&Attribute> = element.plain_attributes().collect();
    let (mut has_property, mut has_resource, mut has_node_id, mut has_value) =
        (false, false, false, false);
    let mut value_attribute = None;
    for (index, attribute) in attributes.iter().enumerate() {
        match Term::of_attribute(attribute, element) {
            Term::Id => {}
            Term::Resource => {
                if has_node_id {
                    return bad(
                        "Empty property element can't have both rdf:resource and rdf:nodeID",
                    );
                }
                if has_value {
                    return bad(
                        "Empty property element can't have both rdf:value and rdf:resource",
                    );
                }
                has_resource = true;
                value_attribute = Some(index);
            }
            Term::NodeId => {
                if has_resource {
                    return bad(
                        "Empty property element can't have both rdf:resource and rdf:nodeID",
                    );
                }
                has_node_id = true;
            }
            Term::Other if is_rdf(attribute, "value") => {
                if has_resource {
                    return bad(
                        "Empty property element can't have both rdf:value and rdf:resource",
                    );
                }
                has_value = true;
                value_attribute = Some(index);
            }
            Term::Other => has_property |= attribute.qualified_name != XML_LANG,
            _ => return bad("Unrecognized attribute of empty property element"),
        }
    }
    let node = target.add_child(&element.namespace, &element.local, Some(String::new()))?;
    let mut is_struct = false;
    if has_value || has_resource {
        node.value = Some(
            value_attribute
                .map(|index| attributes[index].value.clone())
                .unwrap_or_default(),
        );
        node.form.uri = !has_value;
    } else if has_property {
        node.form.structure = true;
        is_struct = true;
    }
    for (index, attribute) in attributes.iter().enumerate() {
        if Some(index) == value_attribute {
            continue;
        }
        match Term::of_attribute(attribute, element) {
            Term::Id | Term::NodeId => {}
            Term::Resource => node
                .add_qualifier_value(Name::Written("rdf:resource".to_owned()), &attribute.value)?,
            Term::Other if !is_struct => node
                .add_qualifier_value(Name::written(&attribute.qualified_name), &attribute.value)?,
            Term::Other if attribute.qualified_name == XML_LANG => {
                node.add_qualifier_value(Name::Lang, &attribute.value)?
            }
            Term::Other => {
                Target::Node(&mut *node).add_child(
                    &attribute.namespace,
                    &attribute.local_name,
                    Some(attribute.value.clone()),
                )?;
            }
            _ => return bad("Unrecognized attribute of empty property element"),
        }
    }
    Ok(())
}

/// `fixupQualifiedNode`: a struct with an `rdf:value` made the value, the value's qualifiers
/// and the struct's other fields its qualifiers.
fn fixup_qualified_node(parent: &mut XmpNode) -> Result<(), Failure> {
    let mut value = parent.children.remove(0);
    if let Some(index) = value.qualifiers.iter().position(|q| q.name == Name::Lang) {
        if parent.has_language() {
            return bad("Redundant xml:lang for rdf:value element");
        }
        let language = value.qualifiers.remove(index);
        parent.add_qualifier(language)?;
    }
    for qualifier in std::mem::take(&mut value.qualifiers) {
        parent.add_qualifier(qualifier)?;
    }
    for mut field in std::mem::take(&mut parent.children) {
        field.name = field.name.into_qualifier();
        parent.add_qualifier(field)?;
    }
    parent.has_value_child = false;
    parent.form.structure = false;
    parent.form = parent.form.merged(value.form)?;
    parent.value = value.value;
    parent.children = value.children;
    Ok(())
}

/// `XMPNodeUtils.detectAltText`: an alternative with an item in a language is alternative
/// text, its `x-default` item first.
fn detect_alt_text(array: &mut XmpNode) {
    if !array.form.alternate || array.children.is_empty() {
        return;
    }
    if array.children.iter().any(XmpNode::has_language) {
        array.form.alt_text = true;
        normalize_lang_array(array);
    }
}

/// `XMPNodeUtils.normalizeLangArray`: the first item after the first whose first qualifier's
/// value is `x-default` moved first, its value copied to the item it displaced from second.
fn normalize_lang_array(array: &mut XmpNode) {
    if !array.form.alt_text {
        return;
    }
    let found = (1..array.children.len()).find(|&index| {
        array.children[index]
            .qualifiers
            .first()
            .is_some_and(|q| q.value.as_deref() == Some(X_DEFAULT))
    });
    if let Some(index) = found {
        let child = array.children.remove(index);
        let moved_value = child.value.clone();
        array.children.insert(0, child);
        if index == 1 {
            array.children[1].value = moved_value;
        }
    }
}

/// `XMPNormalizer.process`.
fn normalize(tree: &mut Tree) -> Result<(), Failure> {
    touch_up_data_model(tree)?;
    move_explicit_aliases(tree)?;
    tweak_old_xmp(tree);
    Ok(())
}

/// `touchUpDataModel`: Dublin Core's arrays, `exif:UserComment` and `xmpRights:UsageTerms` as
/// alternative text, and `xmpDM:copyright` moved into `dc:rights`.
fn touch_up_data_model(tree: &mut Tree) -> Result<(), Failure> {
    let dc = tree.schema(NS_DC);
    for index in 0..tree.schemas.len() {
        match tree.schemas[index].namespace.as_str() {
            NS_DC => normalize_dc_arrays(&mut tree.schemas[index])?,
            NS_EXIF => {
                let schema = &mut tree.schemas[index];
                if let Some(comment) = schema
                    .properties
                    .iter_mut()
                    .find(|p| p.name.is(NS_EXIF, "UserComment"))
                {
                    if comment.form.is_simple() {
                        if !comment.qualifiers.is_empty() {
                            return Err(Unported::new(
                                "a simple exif:UserComment with qualifiers, on which XMPCore \
                                 throws an IndexOutOfBoundsException",
                            )
                            .into());
                        }
                        let mut item = XmpNode::new(Name::Item, comment.value.take());
                        item.form = comment.form;
                        item.add_qualifier_value(Name::Lang, X_DEFAULT)?;
                        comment.children.push(item);
                        comment.form = Form::ALT_TEXT;
                        comment.value = Some(String::new());
                    }
                    repair_alt_text(comment)?;
                }
            }
            NS_DM => {
                let Some(position) = tree.schemas[index]
                    .properties
                    .iter()
                    .position(|p| p.name.is(NS_DM, "copyright"))
                else {
                    continue;
                };
                if tree.schemas[dc]
                    .properties
                    .iter()
                    .any(|p| p.name.is(NS_DC, "rights"))
                {
                    return Err(Unported::new(
                        "an xmpDM:copyright beside a dc:rights, which XMPCore merges into it",
                    )
                    .into());
                }
                let copyright = tree.schemas[index].properties.remove(position);
                let mut item = XmpNode::new(
                    Name::Item,
                    Some(format!("\n\n{}", copyright.value.unwrap_or_default())),
                );
                item.add_qualifier_value(Name::Lang, X_DEFAULT)?;
                let mut rights = XmpNode::new(Name::property(NS_DC, "rights"), None);
                rights.form = Form::ALT_TEXT;
                rights.children.push(item);
                tree.schemas[dc].properties.push(rights);
            }
            NS_XMP_RIGHTS => {
                if let Some(terms) = tree.schemas[index]
                    .properties
                    .iter_mut()
                    .find(|p| p.name.is(NS_XMP_RIGHTS, "UsageTerms"))
                {
                    repair_alt_text(terms)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// `normalizeDCArrays`: a simple Dublin Core property that should be an array made its item, an
/// alternative text item given the default language; a compound one given the array's form.
fn normalize_dc_arrays(schema: &mut Schema) -> Result<(), Failure> {
    for property in &mut schema.properties {
        let Name::Property { local, .. } = &property.name else {
            continue;
        };
        let Some(&(_, form)) = DC_ARRAYS.iter().find(|(name, _)| name == local) else {
            continue;
        };
        if property.form.is_simple() {
            let mut array = XmpNode::new(property.name.clone(), None);
            array.form = form;
            let mut item = std::mem::replace(property, array);
            item.name = Name::Item;
            if form.alt_text && !item.has_language() {
                item.add_qualifier_value(Name::Lang, X_DEFAULT)?;
            }
            property.children.push(item);
        } else {
            let cleared = Form {
                array: false,
                ordered: false,
                alternate: false,
                alt_text: false,
                ..property.form
            };
            property.form = cleared.merged(form)?;
            if form.alt_text {
                repair_alt_text(property)?;
            }
        }
    }
    Ok(())
}

/// `repairAltText`: an array made alternative text, its compound items and empty items without
/// a language dropped, the others given the language `x-repair`.
fn repair_alt_text(node: &mut XmpNode) -> Result<(), Failure> {
    if !node.form.array {
        return Ok(());
    }
    node.form.ordered = true;
    node.form.alternate = true;
    node.form.alt_text = true;
    let mut kept = Vec::with_capacity(node.children.len());
    for mut child in std::mem::take(&mut node.children) {
        if !child.form.is_simple() {
            continue;
        }
        if !child.has_language() {
            if child.value.as_deref().is_none_or(str::is_empty) {
                continue;
            }
            child.add_qualifier_value(Name::Lang, "x-repair")?;
        }
        kept.push(child);
    }
    node.children = kept;
    Ok(())
}

/// `moveExplicitAliases`: each alias moved to the property it stands for, or its first item,
/// unless that is there already.
fn move_explicit_aliases(tree: &mut Tree) -> Result<(), Failure> {
    if !tree.has_aliases {
        return Ok(());
    }
    for schema in 0..tree.schemas.len() {
        if !tree.schemas[schema].has_aliases {
            continue;
        }
        let mut index = 0;
        while index < tree.schemas[schema].properties.len() {
            if !tree.schemas[schema].properties[index].alias {
                index += 1;
                continue;
            }
            let mut alias = tree.schemas[schema].properties.remove(index);
            alias.alias = false;
            let Name::Property { namespace, local } = &alias.name else {
                unreachable!("an alias is a property");
            };
            let &(_, _, base_namespace, base_local, form) = ALIASES
                .iter()
                .find(|a| a.0 == namespace && a.1 == local)
                .expect("an alias is in the table it was found in");
            let base_schema = tree.schema(base_namespace);
            let properties = &mut tree.schemas[base_schema].properties;
            let base = properties
                .iter()
                .position(|p| p.name.is(base_namespace, base_local));
            match (base, form.is_simple()) {
                (None, true) => {
                    alias.name = Name::property(base_namespace, base_local);
                    properties.push(alias);
                }
                (None, false) => {
                    let mut array = XmpNode::new(Name::property(base_namespace, base_local), None);
                    array.form = form;
                    transplant_array_item_alias(alias, &mut array)?;
                    properties.push(array);
                }
                (Some(_), true) => {}
                (Some(base), false) => {
                    let base = &mut properties[base];
                    let item = match form.alt_text {
                        true => lookup_language_item(base, X_DEFAULT)?,
                        false => (!base.children.is_empty()).then_some(0),
                    };
                    if item.is_none() {
                        transplant_array_item_alias(alias, base)?;
                    }
                }
            }
        }
        tree.schemas[schema].has_aliases = false;
    }
    tree.has_aliases = false;
    Ok(())
}

/// `transplantArrayItemAlias`: the alias made an item of the array, in the default language of
/// alternative text.
fn transplant_array_item_alias(mut alias: XmpNode, array: &mut XmpNode) -> Result<(), Failure> {
    if array.form.alt_text {
        if alias.has_language() {
            return bad("Alias to x-default already has a language qualifier");
        }
        alias.add_qualifier_value(Name::Lang, X_DEFAULT)?;
    }
    alias.name = Name::Item;
    array.children.push(alias);
    Ok(())
}

/// `XMPNodeUtils.lookupLanguageItem`: the index of the item whose first qualifier is a language
/// of the value `language`.
fn lookup_language_item(array: &XmpNode, language: &str) -> Result<Option<usize>, Failure> {
    if !array.form.array {
        return bad("Language item must be used on array");
    }
    Ok(array.children.iter().position(|child| {
        child
            .qualifiers
            .first()
            .is_some_and(|q| q.name == Name::Lang && q.value.as_deref() == Some(language))
    }))
}

/// `tweakOldXMP`: a tree named with a UUID gives it to `xmpMM:InstanceID`.
fn tweak_old_xmp(tree: &mut Tree) {
    let Some(name) = tree.name.as_deref() else {
        return;
    };
    if name.encode_utf16().count() < 36 {
        return;
    }
    let lower = name.to_lowercase();
    let uuid = lower.strip_prefix("uuid:").unwrap_or(&lower);
    if !is_uuid(uuid) {
        return;
    }
    let mut instance = XmpNode::new(
        Name::property(NS_XMP_MM, "InstanceID"),
        Some(format!("uuid:{uuid}")),
    );
    let schema = tree.schema(NS_XMP_MM);
    let properties = &mut tree.schemas[schema].properties;
    match properties
        .iter_mut()
        .find(|p| p.name.is(NS_XMP_MM, "InstanceID"))
    {
        Some(existing) => std::mem::swap(existing, &mut instance),
        None => properties.push(instance),
    }
    tree.name = None;
}

/// `Utils.checkUUIDFormat`: 36 UTF-16 units with hyphens at 8, 13, 18 and 23 alone.
fn is_uuid(text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().collect();
    let hyphens: Vec<usize> = units
        .iter()
        .enumerate()
        .filter(|(_, &unit)| unit == u16::from(b'-'))
        .map(|(index, _)| index)
        .collect();
    units.len() == 36 && hyphens == [8, 13, 18, 23]
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    /// The packets of `tests/fixtures/xmp`, handwritten ones for each construct XMPCore reads
    /// and those of the corpora, counted as `XmpValueCounts.java` has metadata-extractor and
    /// XMPCore count them (`just xmp-count-reference`): the value count, `fails` where XMPCore
    /// throws an `XMPException`, `throws` where another exception escapes. The packets XMPCore
    /// leaves the port behind on, a simple `exif:UserComment` with qualifiers and an
    /// `xmpDM:copyright` beside a `dc:rights`, are unported.
    #[test]
    fn counts_values_as_xmpcore_does() {
        let mut reference = String::new();
        flate2::read::GzDecoder::new(
            &include_bytes!("../../../../tests/fixtures/xmp/xmp-value-counts.tsv.gz")[..],
        )
        .read_to_string(&mut reference)
        .unwrap();
        let mut differences = Vec::new();
        let mut unported = Vec::new();
        let mut count = 0;
        for line in reference.lines() {
            let (expected, packet) = line.split_once('\t').unwrap();
            let packet = hex(packet);
            count += 1;
            let actual = match value_count(&packet) {
                Ok(count) => count.to_string(),
                Err(Failure::Xmp(_)) => "fails".to_owned(),
                Err(Failure::Unported(reason)) => {
                    unported.push((expected.to_owned(), format!("{reason:?}")));
                    continue;
                }
            };
            if actual != expected {
                differences.push(format!(
                    "{expected} but {actual}: {}",
                    String::from_utf8_lossy(&packet[..packet.len().min(600)])
                ));
            }
        }
        assert!(count > 200, "{count}");
        assert!(
            differences.is_empty(),
            "{} of {count} differ:\n{}",
            differences.len(),
            differences.join("\n\n")
        );
        assert_eq!(unported.len(), 1, "{unported:?}");
        assert_eq!(unported[0].0, "throws");
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }
}
