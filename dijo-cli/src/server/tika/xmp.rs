//! The XMP packet tika-server answers `/meta` with for `Accept: application/rdf+xml`: Tika
//! 4.2.0's `new XMPMetadata(metadata).toString()`, which converts a document's metadata to XMP
//! with the tika-xmp converter its `Content-Type` selects and serializes it as Adobe XMPCore
//! 6.1.11 writes a compact packet without its wrapper.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::Value;

use super::output::CONTENT_TYPE;

/// The toolkit XMPCore names in a packet: the version Tika 4.2.0 ships.
const TOOLKIT: &str = "Adobe XMP Core 6.1.11";
const ADOBE_META_URI: &str = "adobe:ns:meta/";
const RDF_URI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const ALT: &str = "rdf:Alt";
/// The language of the one item Tika sets in a language alternative.
const DEFAULT_LANGUAGE: &str = "x-default";

const FORMAT: &str = "dc:format";
const TITLE: &str = "dc:title";
const DESCRIPTION: &str = "dc:description";
const CREATOR: &str = "dc:creator";
const SUBJECT: &str = "dc:subject";
const LANGUAGE: &str = "dc:language";
const IDENTIFIER: &str = "dc:identifier";
const PUBLISHER: &str = "dc:publisher";
const CREATED: &str = "dcterms:created";
const MODIFIED: &str = "dcterms:modified";
const COMMENTS: &str = "w:Comments";
const CATEGORY: &str = "cp:category";
const CONTENT_STATUS: &str = "cp:contentStatus";
const REVISION: &str = "cp:revision";
const VERSION: &str = "cp:version";
const APPLICATION: &str = "extended-properties:Application";
const APP_VERSION: &str = "extended-properties:AppVersion";
const COMPANY: &str = "extended-properties:Company";
const MANAGER: &str = "extended-properties:Manager";
const NOTES: &str = "extended-properties:Notes";
const PRESENTATION_FORMAT: &str = "extended-properties:PresentationFormat";
const TEMPLATE: &str = "extended-properties:Template";
const TOTAL_TIME: &str = "extended-properties:TotalTime";
const MODIFIER: &str = "meta:last-author";
const PRINT_DATE: &str = "meta:print-date";
const CHARACTER_COUNT: &str = "meta:character-count";
const CHARACTER_COUNT_WITH_SPACES: &str = "meta:character-count-with-spaces";
const IMAGE_COUNT: &str = "meta:image-count";
const LINE_COUNT: &str = "meta:line-count";
const OBJECT_COUNT: &str = "meta:object-count";
const PAGE_COUNT: &str = "meta:page-count";
const PARAGRAPH_COUNT: &str = "meta:paragraph-count";
const SLIDE_COUNT: &str = "meta:slide-count";
const TABLE_COUNT: &str = "meta:table-count";
const WORD_COUNT: &str = "meta:word-count";
const N_PAGES: &str = "xmpTPg:NPages";
/// The bare keys OpenDocumentConverter reads, which no Tika parser writes.
const EDITING_CYCLES: &str = "editing-cycles";
const GENERATOR: &str = "generator";
const INITIAL_CREATOR: &str = "initial-creator";

/// The XMP packet of a document's metadata, each value a string or an array of strings, as
/// tika-server writes it for `application/rdf+xml`.
pub fn packet(metadata: &BTreeMap<String, Value>) -> String {
    Converter::of(metadata).convert(metadata).serialize()
}

/// A namespace a packet's properties are in, with the prefix XMPCore declares it under in a
/// fresh tika-server: its own for the ones it knows, the one a converter registers for the
/// rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Namespace {
    Dc,
    Xmp,
    XmpRights,
    XmpMm,
    XmpTPg,
    XmpDm,
    XmpIdq,
    Pdf,
    Pdfx,
    PdfxId,
    PdfaId,
    Photoshop,
    Tiff,
    Exif,
    Aux,
    IptcCore,
    IptcExt,
    Plus,
    Cp,
    ExtendedProperties,
    OdfMeta,
}

impl Namespace {
    fn prefix(self) -> &'static str {
        match self {
            Namespace::Dc => "dc",
            Namespace::Xmp => "xmp",
            Namespace::XmpRights => "xmpRights",
            Namespace::XmpMm => "xmpMM",
            Namespace::XmpTPg => "xmpTPg",
            Namespace::XmpDm => "xmpDM",
            Namespace::XmpIdq => "xmpidq",
            Namespace::Pdf => "pdf",
            Namespace::Pdfx => "pdfx",
            Namespace::PdfxId => "pdfxid",
            Namespace::PdfaId => "pdfaid",
            Namespace::Photoshop => "photoshop",
            Namespace::Tiff => "tiff",
            Namespace::Exif => "exif",
            Namespace::Aux => "aux",
            Namespace::IptcCore => "Iptc4xmpCore",
            Namespace::IptcExt => "Iptc4xmpExt",
            Namespace::Plus => "plus",
            Namespace::Cp => "cp",
            Namespace::ExtendedProperties => "extended-properties",
            Namespace::OdfMeta => "meta",
        }
    }

    fn uri(self) -> &'static str {
        match self {
            Namespace::Dc => "http://purl.org/dc/elements/1.1/",
            Namespace::Xmp => "http://ns.adobe.com/xap/1.0/",
            Namespace::XmpRights => "http://ns.adobe.com/xap/1.0/rights/",
            Namespace::XmpMm => "http://ns.adobe.com/xap/1.0/mm/",
            Namespace::XmpTPg => "http://ns.adobe.com/xap/1.0/t/pg/",
            Namespace::XmpDm => "http://ns.adobe.com/xmp/1.0/DynamicMedia/",
            Namespace::XmpIdq => "http://ns.adobe.com/xmp/Identifier/qual/1.0/",
            Namespace::Pdf => "http://ns.adobe.com/pdf/1.3/",
            Namespace::Pdfx => "http://ns.adobe.com/pdfx/1.3/",
            Namespace::PdfxId => "http://www.npes.org/pdfx/ns/id/",
            Namespace::PdfaId => "http://www.aiim.org/pdfa/ns/id/",
            Namespace::Photoshop => "http://ns.adobe.com/photoshop/1.0/",
            Namespace::Tiff => "http://ns.adobe.com/tiff/1.0/",
            Namespace::Exif => "http://ns.adobe.com/exif/1.0/",
            Namespace::Aux => "http://ns.adobe.com/exif/1.0/aux/",
            Namespace::IptcCore => "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
            Namespace::IptcExt => "http://iptc.org/std/Iptc4xmpExt/2008-02-29/",
            Namespace::Plus => "http://ns.useplus.org/ldf/xmp/1.0/",
            Namespace::Cp => {
                "http://schemas.openxmlformats.org/package/2006/metadata/core-properties/"
            }
            Namespace::ExtendedProperties => {
                "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties/"
            }
            Namespace::OdfMeta => "urn:oasis:names:tc:opendocument:xmlns:meta:1.0",
        }
    }
}

/// The kind of an XMP array of plain items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Array {
    Bag,
    Seq,
}

impl Array {
    fn element(self) -> &'static str {
        match self {
            Array::Bag => "rdf:Bag",
            Array::Seq => "rdf:Seq",
        }
    }
}

/// How a Tika key's values become an XMP property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mapping {
    /// The first value as a simple property, none when it is empty.
    Simple,
    /// The first value as the default item of a language alternative, none when it is empty.
    LangAlt,
    /// Every value as an item of an array.
    Values(Array),
    /// The first value split into the distinct items of an array as XMPCore's
    /// `separateArrayItems` splits it, none when it is empty.
    Separated(Array),
}

const SIMPLE: Mapping = Mapping::Simple;
const LANG_ALT: Mapping = Mapping::LangAlt;
const BAG: Mapping = Mapping::Values(Array::Bag);
const SEQ: Mapping = Mapping::Values(Array::Seq);

/// The value of a property in a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node<'m> {
    Simple(Cow<'m, str>),
    LangAlt(&'m str),
    Array(Array, Vec<Cow<'m, str>>),
}

impl Mapping {
    fn node(self, value: &Value) -> Option<Node<'_>> {
        match self {
            Mapping::Values(array) => Some(Node::Array(
                array,
                strings(value).map(Cow::Borrowed).map(scrub).collect(),
            )),
            Mapping::Simple => filled(value).map(|text| Node::Simple(scrub(Cow::Borrowed(text)))),
            Mapping::LangAlt => filled(value).map(Node::LangAlt),
            Mapping::Separated(array) => {
                filled(value).map(|text| Node::Array(array, separate(text)))
            }
        }
    }
}

/// The strings of a value: itself, or the items of an array.
fn strings(value: &Value) -> impl Iterator<Item = &str> {
    match value {
        Value::Array(items) => items.as_slice(),
        value => std::slice::from_ref(value),
    }
    .iter()
    .filter_map(Value::as_str)
}

/// The first string of a value, as Tika's `Metadata.get` answers it.
fn first(value: &Value) -> Option<&str> {
    strings(value).next()
}

/// The first string of a value unless it is empty, which the converters skip.
fn filled(value: &Value) -> Option<&str> {
    first(value).filter(|text| !text.is_empty())
}

/// A value as XMPCore stores it through `setProperty` and `appendArrayItem`: each ASCII control
/// character but tab, line feed and carriage return replaced by a space.
fn scrub(text: Cow<'_, str>) -> Cow<'_, str> {
    let control = |c: char| (c <= '\u{1f}' && !matches!(c, '\t' | '\n' | '\r')) || c == '\u{7f}';
    if !text.chars().any(control) {
        return text;
    }
    Cow::Owned(
        text.chars()
            .map(|c| if control(c) { ' ' } else { c })
            .collect(),
    )
}

/// What XMPCore's `separateArrayItems` makes of a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Normal,
    Space,
    Quote,
    /// A comma, semicolon or control character, which ends an item since Tika does not ask for
    /// commas to be kept.
    Separator,
}

fn class(c: char) -> Class {
    match c {
        ' ' | '\u{3000}' | '\u{303f}' | '\u{2000}'..='\u{200b}' => Class::Space,
        ',' | '\u{ff0c}' | '\u{ff64}' | '\u{fe50}' | '\u{fe51}' | '\u{3001}' | '\u{060c}'
        | '\u{055d}' => Class::Separator,
        ';' | '\u{ff1b}' | '\u{fe54}' | '\u{061b}' | '\u{037e}' => Class::Separator,
        '"'
        | '\u{ab}'
        | '\u{bb}'
        | '\u{301d}'
        | '\u{301e}'
        | '\u{301f}'
        | '\u{2015}'
        | '\u{2039}'
        | '\u{203a}'
        | '\u{3008}'..='\u{300f}'
        | '\u{2018}'..='\u{201f}' => Class::Quote,
        '\u{0}'..='\u{1f}' | '\u{2028}' | '\u{2029}' => Class::Separator,
        _ => Class::Normal,
    }
}

/// The quote that closes a value opened by `open`, if any.
fn closing_quote(open: char) -> Option<char> {
    Some(match open {
        '"' => '"',
        '\u{ab}' => '\u{bb}',
        '\u{bb}' => '\u{ab}',
        '\u{2015}' => '\u{2015}',
        '\u{2018}' => '\u{2019}',
        '\u{201a}' => '\u{201b}',
        '\u{201c}' => '\u{201d}',
        '\u{201e}' => '\u{201f}',
        '\u{2039}' => '\u{203a}',
        '\u{203a}' => '\u{2039}',
        '\u{3008}' => '\u{3009}',
        '\u{300a}' => '\u{300b}',
        '\u{300c}' => '\u{300d}',
        '\u{300e}' => '\u{300f}',
        '\u{301d}' => '\u{301f}',
        _ => return None,
    })
}

/// Whether `c` closes a value opened by `open`, as XMPCore decides it, which takes U+301F to
/// close a value whatever quote opened it.
fn closes(c: char, open: char) -> bool {
    closing_quote(open) == Some(c) || (open == '\u{301d}' && c == '\u{301e}') || c == '\u{301f}'
}

/// The distinct items XMPCore's `separateArrayItems` splits `text` into: runs of characters
/// separated by commas, semicolons, controls or more than one space, or quoted values with
/// their doubled quotes undoubled.
fn separate(text: &str) -> Vec<Cow<'_, str>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let offset = |index: usize| chars.get(index).map_or(text.len(), |&(offset, _)| offset);
    let class_at = |index: usize| chars.get(index).map(|&(_, c)| class(c));
    let mut items: Vec<Cow<'_, str>> = Vec::new();
    let mut end = 0;
    while end < chars.len() {
        let Some(start) = (end..chars.len())
            .find(|&index| matches!(class_at(index), Some(Class::Normal | Class::Quote)))
        else {
            break;
        };
        let open = chars[start].1;
        let item = if class(open) == Class::Quote {
            let mut item = String::new();
            end = start + 1;
            while let Some(&(_, c)) = chars.get(end) {
                end += 1;
                if class(c) != Class::Quote || (c != open && !closes(c, open)) {
                    item.push(c);
                } else if chars.get(end).map(|&(_, next)| next) == Some(c) {
                    item.push(c);
                    end += 1;
                } else if closes(c, open) {
                    break;
                } else {
                    item.push(c);
                }
            }
            Cow::Owned(item)
        } else {
            end = start;
            while let Some(kind) = class_at(end) {
                let continues = match kind {
                    Class::Normal | Class::Quote => true,
                    Class::Space => matches!(class_at(end + 1), Some(Class::Normal | Class::Quote)),
                    Class::Separator => false,
                };
                if !continues {
                    break;
                }
                end += 1;
            }
            Cow::Borrowed(&text[offset(start)..offset(end)])
        };
        if !items.contains(&item) {
            items.push(item);
        }
    }
    items
}

/// A step of a converter for one kind of document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// A Tika key's values mapped into a property.
    Map(&'static str, Mapping, Namespace, &'static str),
    /// MSOfficeXMLConverter's creator tool: the application, followed by its version after a
    /// space when there is one.
    ApplicationVersion,
}

const DC_FORMAT: Step = Step::Map(CONTENT_TYPE, SIMPLE, Namespace::Dc, "format");
const DC_CREATOR: Step = Step::Map(
    CREATOR,
    Mapping::Separated(Array::Seq),
    Namespace::Dc,
    "creator",
);
const DC_SUBJECT: Step = Step::Map(
    SUBJECT,
    Mapping::Separated(Array::Bag),
    Namespace::Dc,
    "subject",
);
const DC_TITLE: Step = Step::Map(TITLE, LANG_ALT, Namespace::Dc, "title");
const DC_DESCRIPTION: Step = Step::Map(DESCRIPTION, LANG_ALT, Namespace::Dc, "description");
const XMP_CREATE_DATE: Step = Step::Map(CREATED, SIMPLE, Namespace::Xmp, "CreateDate");
const XMP_MODIFY_DATE: Step = Step::Map(MODIFIED, SIMPLE, Namespace::Xmp, "ModifyDate");
const PDFX_COMMENTS: Step = Step::Map(COMMENTS, SIMPLE, Namespace::Pdfx, "Comments");
const IPTC_GENRE: Step = Step::Map(CATEGORY, SIMPLE, Namespace::IptcCore, "intellectualGenre");
const CP_LAST_PRINTED: Step = Step::Map(PRINT_DATE, SIMPLE, Namespace::Cp, "lastPrinted");
const CP_REVISION: Step = Step::Map(REVISION, SIMPLE, Namespace::Cp, "revision");
const PAGES: Step = Step::Map(PAGE_COUNT, SIMPLE, Namespace::XmpTPg, "NPages");
const EXTENDED_CHARACTERS: Step = Step::Map(
    CHARACTER_COUNT,
    SIMPLE,
    Namespace::ExtendedProperties,
    "Characters",
);
const EXTENDED_COMPANY: Step = Step::Map(COMPANY, SIMPLE, Namespace::ExtendedProperties, "Company");
const EXTENDED_MANAGER: Step = Step::Map(MANAGER, SIMPLE, Namespace::ExtendedProperties, "Manager");
const EXTENDED_TEMPLATE: Step =
    Step::Map(TEMPLATE, SIMPLE, Namespace::ExtendedProperties, "Template");
const EXTENDED_WORDS: Step = Step::Map(WORD_COUNT, SIMPLE, Namespace::ExtendedProperties, "Words");

/// MSOfficeBinaryConverter's steps, in its order.
const MS_OFFICE_BINARY: &[Step] = &[
    DC_FORMAT,
    Step::Map(APPLICATION, SIMPLE, Namespace::Xmp, "CreatorTool"),
    DC_CREATOR,
    IPTC_GENRE,
    XMP_CREATE_DATE,
    EXTENDED_CHARACTERS,
    PDFX_COMMENTS,
    EXTENDED_COMPANY,
    DC_SUBJECT,
    DC_DESCRIPTION,
    Step::Map(LANGUAGE, SIMPLE, Namespace::Cp, "language"),
    CP_LAST_PRINTED,
    XMP_MODIFY_DATE,
    PAGES,
    CP_REVISION,
    Step::Map(SLIDE_COUNT, SIMPLE, Namespace::ExtendedProperties, "Pages"),
    EXTENDED_TEMPLATE,
    DC_TITLE,
    EXTENDED_WORDS,
];

/// MSOfficeXMLConverter's steps, in its order.
const MS_OFFICE_XML: &[Step] = &[
    DC_FORMAT,
    IPTC_GENRE,
    Step::Map(CONTENT_STATUS, SIMPLE, Namespace::Cp, "contentStatus"),
    XMP_CREATE_DATE,
    DC_CREATOR,
    PDFX_COMMENTS,
    Step::Map(IDENTIFIER, SIMPLE, Namespace::Dc, "identifier"),
    DC_SUBJECT,
    DC_DESCRIPTION,
    Step::Map(LANGUAGE, SIMPLE, Namespace::Dc, "language"),
    Step::Map(MODIFIER, SIMPLE, Namespace::Cp, "lastModifiedBy"),
    CP_LAST_PRINTED,
    XMP_MODIFY_DATE,
    CP_REVISION,
    DC_TITLE,
    Step::Map(VERSION, SIMPLE, Namespace::Cp, "version"),
    Step::ApplicationVersion,
    EXTENDED_CHARACTERS,
    Step::Map(
        CHARACTER_COUNT_WITH_SPACES,
        SIMPLE,
        Namespace::ExtendedProperties,
        "CharactersWithSpaces",
    ),
    Step::Map(PUBLISHER, SIMPLE, Namespace::ExtendedProperties, "Company"),
    Step::Map(LINE_COUNT, SIMPLE, Namespace::ExtendedProperties, "Lines"),
    EXTENDED_MANAGER,
    Step::Map(NOTES, SIMPLE, Namespace::ExtendedProperties, "Notes"),
    PAGES,
    Step::Map(
        PARAGRAPH_COUNT,
        SIMPLE,
        Namespace::ExtendedProperties,
        "Paragraphs",
    ),
    Step::Map(
        PRESENTATION_FORMAT,
        SIMPLE,
        Namespace::ExtendedProperties,
        "PresentationFormat",
    ),
    Step::Map(SLIDE_COUNT, SIMPLE, Namespace::ExtendedProperties, "Slides"),
    EXTENDED_TEMPLATE,
    Step::Map(
        TOTAL_TIME,
        SIMPLE,
        Namespace::ExtendedProperties,
        "TotalTime",
    ),
    EXTENDED_WORDS,
];

/// OpenDocumentConverter's steps, in its order.
const OPEN_DOCUMENT: &[Step] = &[
    DC_FORMAT,
    Step::Map(
        CHARACTER_COUNT,
        SIMPLE,
        Namespace::OdfMeta,
        "character-count",
    ),
    XMP_CREATE_DATE,
    DC_CREATOR,
    XMP_MODIFY_DATE,
    PDFX_COMMENTS,
    DC_SUBJECT,
    DC_DESCRIPTION,
    Step::Map(TOTAL_TIME, SIMPLE, Namespace::OdfMeta, "editing-duration"),
    Step::Map(EDITING_CYCLES, SIMPLE, Namespace::OdfMeta, "editing-cycles"),
    Step::Map(GENERATOR, SIMPLE, Namespace::Xmp, "CreatorTool"),
    Step::Map(IMAGE_COUNT, SIMPLE, Namespace::OdfMeta, "image-count"),
    Step::Map(
        INITIAL_CREATOR,
        SIMPLE,
        Namespace::OdfMeta,
        "initial-creator",
    ),
    Step::Map(OBJECT_COUNT, SIMPLE, Namespace::OdfMeta, "object-count"),
    Step::Map(N_PAGES, SIMPLE, Namespace::XmpTPg, "NPages"),
    Step::Map(
        PARAGRAPH_COUNT,
        SIMPLE,
        Namespace::OdfMeta,
        "paragraph-count",
    ),
    Step::Map(TABLE_COUNT, SIMPLE, Namespace::OdfMeta, "table-count"),
    DC_TITLE,
    Step::Map(WORD_COUNT, SIMPLE, Namespace::OdfMeta, "word-count"),
];

/// RTFConverter's steps, in its order.
const RTF: &[Step] = &[
    DC_FORMAT,
    DC_CREATOR,
    DC_TITLE,
    DC_DESCRIPTION,
    DC_SUBJECT,
    IPTC_GENRE,
    EXTENDED_TEMPLATE,
    PDFX_COMMENTS,
    EXTENDED_COMPANY,
    EXTENDED_MANAGER,
];

/// The properties GenericConverter writes, by namespace and local name, each sorted by name:
/// the keys of Tika 4.2.0's registered properties in a namespace XMPCore knows, as Tika's
/// property type maps them, but for `dc:title`, `dc:description` and `xmpRights:UsageTerms`,
/// which it writes as language alternatives, and `dc:creator`, which it writes as a sequence.
const REGISTERED: &[(Namespace, &[(&str, Mapping)])] = &[
    (
        Namespace::IptcCore,
        &[
            ("CiAdrCity", SIMPLE),
            ("CiAdrCtry", SIMPLE),
            ("CiAdrExtadr", BAG),
            ("CiAdrPcode", SIMPLE),
            ("CiAdrRegion", SIMPLE),
            ("CiEmailWork", BAG),
            ("CiTelWork", BAG),
            ("CiUrlWork", BAG),
            ("CountryCode", SIMPLE),
            ("CreatorContactInfo", SIMPLE),
            ("IntellectualGenre", SIMPLE),
            ("Location", SIMPLE),
            ("Scene", BAG),
            ("SubjectCode", BAG),
        ],
    ),
    (
        Namespace::IptcExt,
        &[
            ("AOCopyrightNotice", BAG),
            ("AOCreator", BAG),
            ("AODateCreated", BAG),
            ("AOSource", BAG),
            ("AOSourceInvNo", BAG),
            ("AOTitle", BAG),
            ("AddlModelInfo", SIMPLE),
            ("ArtworkOrObject", BAG),
            ("CVterm", BAG),
            ("DigImageGUID", SIMPLE),
            ("DigitalSourceType", SIMPLE),
            ("Event", SIMPLE),
            ("IptcLastEdited", SIMPLE),
            ("LocationCreated", BAG),
            ("LocationCreatedCity", SIMPLE),
            ("LocationCreatedCountryCode", SIMPLE),
            ("LocationCreatedCountryName", SIMPLE),
            ("LocationCreatedProvinceState", SIMPLE),
            ("LocationCreatedSublocation", SIMPLE),
            ("LocationCreatedWorldRegion", SIMPLE),
            ("LocationShown", BAG),
            ("LocationShownCity", BAG),
            ("LocationShownCountryCode", BAG),
            ("LocationShownCountryName", BAG),
            ("LocationShownProvinceState", BAG),
            ("LocationShownSublocation", BAG),
            ("LocationShownWorldRegion", BAG),
            ("MaxAvailHeight", SIMPLE),
            ("MaxAvailWidth", SIMPLE),
            ("ModelAge", BAG),
            ("OrganisationInImageCode", BAG),
            ("OrganisationInImageName", BAG),
            ("PersonInImage", BAG),
            ("RegItemId", BAG),
            ("RegOrgId", BAG),
            ("RegistryId", BAG),
        ],
    ),
    (
        Namespace::Aux,
        &[
            ("Lens", SIMPLE),
            ("LensID", SIMPLE),
            ("LensInfo", SIMPLE),
            ("SerialNumber", SIMPLE),
        ],
    ),
    (
        Namespace::Dc,
        &[
            ("contributor", BAG),
            ("coverage", BAG),
            ("creator", SEQ),
            ("date", SIMPLE),
            ("description", LANG_ALT),
            ("format", SIMPLE),
            ("identifier", BAG),
            ("language", BAG),
            ("publisher", BAG),
            ("relation", BAG),
            ("rights", BAG),
            ("source", BAG),
            ("subject", BAG),
            ("title", LANG_ALT),
            ("type", BAG),
        ],
    ),
    (
        Namespace::Exif,
        &[
            ("DateTimeOriginal", SIMPLE),
            ("ExposureTime", SIMPLE),
            ("FNumber", SIMPLE),
            ("Flash", SIMPLE),
            ("FocalLength", SIMPLE),
            ("IsoSpeedRatings", SEQ),
            ("PageCount", SIMPLE),
        ],
    ),
    (
        Namespace::Pdf,
        &[
            ("action-trigger", SIMPLE),
            ("action-triggers", BAG),
            ("action-types", BAG),
            ("annotation-subtypes", BAG),
            ("annotation-types", BAG),
            ("associated-file-relationship", SIMPLE),
            ("chars-per-page", SEQ),
            ("contains-damaged-font", SIMPLE),
            ("contains-non-embedded-font", SIMPLE),
            ("embedded-file-annotation-type", SIMPLE),
            ("embedded-file-description", SIMPLE),
            ("embedded-file-subtype", SIMPLE),
            ("encrypted", SIMPLE),
            ("eof-offsets", SEQ),
            ("has-3d", SIMPLE),
            ("has-acro-form-fields", SIMPLE),
            ("has-collection", SIMPLE),
            ("has-marked-content", SIMPLE),
            ("has-signature-fields", SIMPLE),
            ("has-xfa", SIMPLE),
            ("has-xmp", SIMPLE),
            ("incremental-update-count", SIMPLE),
            ("incremental-update-number", SIMPLE),
            ("js-name", SIMPLE),
            ("marked-content-pages-fallback", SIMPLE),
            ("marked-content-pages-tagged", SIMPLE),
            ("marked-content-rejections", BAG),
            ("num-3d-annotations", SIMPLE),
            ("ocr-page-count", SIMPLE),
            ("overall-percentage-unmapped-unicode-chars", SIMPLE),
            ("pdf-extension-version", SIMPLE),
            ("pdf-version", SIMPLE),
            ("producer", SIMPLE),
            ("total-unmapped-unicode-chars", SIMPLE),
            ("trapped", SIMPLE),
            ("unmapped-unicode-chars-per-page", SEQ),
            ("xmp-location", SIMPLE),
        ],
    ),
    (
        Namespace::PdfaId,
        &[("conformance", SIMPLE), ("part", SIMPLE)],
    ),
    (
        Namespace::Pdfx,
        &[("conformance", SIMPLE), ("version", SIMPLE)],
    ),
    (Namespace::PdfxId, &[("version", SIMPLE)]),
    (
        Namespace::Photoshop,
        &[
            ("AuthorsPosition", SIMPLE),
            ("CaptionWriter", SIMPLE),
            ("Category", SIMPLE),
            ("City", SIMPLE),
            ("ColorMode", SIMPLE),
            ("Country", SIMPLE),
            ("Credit", SIMPLE),
            ("DateCreated", SIMPLE),
            ("Headline", SIMPLE),
            ("ICCProfile", SIMPLE),
            ("Instructions", SIMPLE),
            ("Source", SIMPLE),
            ("State", SIMPLE),
            ("SupplementalCategories", BAG),
            ("TransmissionReference", SIMPLE),
            ("Urgency", SIMPLE),
        ],
    ),
    (
        Namespace::Plus,
        &[
            ("CopyrightOwner", BAG),
            ("CopyrightOwnerID", BAG),
            ("CopyrightOwnerName", BAG),
            ("ImageCreator", BAG),
            ("ImageCreatorID", BAG),
            ("ImageCreatorName", BAG),
            ("ImageSupplier", SIMPLE),
            ("ImageSupplierID", SIMPLE),
            ("ImageSupplierImageID", SIMPLE),
            ("ImageSupplierName", SIMPLE),
            ("Licensor", BAG),
            ("LicensorCity", BAG),
            ("LicensorCountry", BAG),
            ("LicensorEmail", BAG),
            ("LicensorExtendedAddress", BAG),
            ("LicensorID", BAG),
            ("LicensorName", BAG),
            ("LicensorPostalCode", BAG),
            ("LicensorRegion", BAG),
            ("LicensorStreetAddress", BAG),
            ("LicensorTelephone1", BAG),
            ("LicensorTelephone2", BAG),
            ("LicensorURL", BAG),
            ("MinorModelAgeDisclosure", SIMPLE),
            ("ModelReleaseID", BAG),
            ("ModelReleaseStatus", SIMPLE),
            ("PropertyReleaseID", BAG),
            ("PropertyReleaseStatus", SIMPLE),
            ("Version", SIMPLE),
        ],
    ),
    (
        Namespace::Tiff,
        &[
            ("BitsPerSample", SEQ),
            ("ImageLength", SIMPLE),
            ("ImageWidth", SIMPLE),
            ("Make", SIMPLE),
            ("Model", SIMPLE),
            ("Orientation", SIMPLE),
            ("ResolutionUnit", SIMPLE),
            ("SamplesPerPixel", SIMPLE),
            ("Software", SIMPLE),
            ("XResolution", SIMPLE),
            ("YResolution", SIMPLE),
        ],
    ),
    (
        Namespace::Xmp,
        &[
            ("About", BAG),
            ("Advisory", BAG),
            ("CreateDate", SIMPLE),
            ("CreatorTool", SIMPLE),
            ("Identifier", BAG),
            ("Label", SIMPLE),
            ("MetadataDate", SIMPLE),
            ("ModifyDate", SIMPLE),
            ("NickName", SIMPLE),
            ("Rating", SIMPLE),
            ("Title", SIMPLE),
        ],
    ),
    (
        Namespace::XmpDm,
        &[
            ("absPeakAudioFilePath", SIMPLE),
            ("album", SIMPLE),
            ("albumArtist", SIMPLE),
            ("altTapeName", SIMPLE),
            ("artist", SIMPLE),
            ("audioChannelType", SIMPLE),
            ("audioCompressor", SIMPLE),
            ("audioModDate", SIMPLE),
            ("audioSampleRate", SIMPLE),
            ("audioSampleType", SIMPLE),
            ("compilation", SIMPLE),
            ("composer", SIMPLE),
            ("copyright", SIMPLE),
            ("discNumber", SIMPLE),
            ("duration", SIMPLE),
            ("engineer", SIMPLE),
            ("fileDataRate", SIMPLE),
            ("genre", SIMPLE),
            ("instrument", SIMPLE),
            ("key", SIMPLE),
            ("logComment", BAG),
            ("loop", SIMPLE),
            ("metadataModDate", SIMPLE),
            ("numberOfBeats", SIMPLE),
            ("pullDown", SIMPLE),
            ("relativePeakAudioFilePath", SIMPLE),
            ("releaseDate", SIMPLE),
            ("scaleType", SIMPLE),
            ("scene", SIMPLE),
            ("shotDate", SIMPLE),
            ("shotLocation", SIMPLE),
            ("shotName", SIMPLE),
            ("speakerPlacement", SIMPLE),
            ("stretchMode", SIMPLE),
            ("tapeName", SIMPLE),
            ("tempo", SIMPLE),
            ("timeSignature", SIMPLE),
            ("trackNumber", SIMPLE),
            ("videoAlphaMode", SIMPLE),
            ("videoAlphaUnityIsTransparent", SIMPLE),
            ("videoColorSpace", SIMPLE),
            ("videoCompressor", SIMPLE),
            ("videoFieldOrder", SIMPLE),
            ("videoFrameRate", SIMPLE),
            ("videoModDate", SIMPLE),
            ("videoPixelAspectRatio", SIMPLE),
            ("videoPixelDepth", SIMPLE),
        ],
    ),
    (
        Namespace::XmpMm,
        &[
            ("DocumentID", SIMPLE),
            ("InstanceID", SIMPLE),
            ("OriginalDocumentID", SIMPLE),
            ("RenditionClass", SIMPLE),
            ("RenditionParams", SIMPLE),
        ],
    ),
    (
        Namespace::XmpRights,
        &[
            ("Certificate", SIMPLE),
            ("Marked", SIMPLE),
            ("Owner", BAG),
            ("UsageTerms", LANG_ALT),
            ("WebStatement", SIMPLE),
        ],
    ),
    (Namespace::XmpTPg, &[("NPages", SIMPLE)]),
    (Namespace::XmpIdq, &[("Scheme", SIMPLE)]),
];

/// A property of a schema: its namespace and its name in it.
type Property = (Namespace, &'static str);

/// The aliases XMPCore registers that a registered property's key names, each to the
/// property it stands for.
const ALIASES: &[(Property, Property)] = &[
    (
        (Namespace::Tiff, "Software"),
        (Namespace::Xmp, "CreatorTool"),
    ),
    ((Namespace::Xmp, "Title"), (Namespace::Dc, "title")),
];

/// The property GenericConverter writes a key's values into and how, if any.
fn registered(key: &str) -> Option<(Namespace, &'static str, Mapping)> {
    let (prefix, local) = key.split_once(':')?;
    let &(namespace, properties) = REGISTERED
        .iter()
        .find(|(namespace, _)| namespace.prefix() == prefix)?;
    let index = properties
        .binary_search_by(|(name, _)| name.cmp(&local))
        .ok()?;
    let (name, mapping) = properties[index];
    let (namespace, name) = ALIASES
        .iter()
        .find(|&&(alias, _)| alias == (namespace, name))
        .map_or((namespace, name), |&(_, actual)| actual);
    Some((namespace, name, mapping))
}

/// The entries of a metadata map in the order Tika's `Metadata.names()` lists them: the bucket
/// order of the `java.util.HashMap` it keeps them in, grown from its default capacity, with the
/// keys of a bucket, whose insertion order is lost, in key order.
fn java_order(metadata: &BTreeMap<String, Value>) -> Vec<(&str, &Value)> {
    let mut capacity = 16;
    while metadata.len() > capacity / 4 * 3 {
        capacity *= 2;
    }
    let mut entries: Vec<(&str, &Value)> = metadata
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect();
    entries.sort_by_key(|&(key, _)| java_hash(key) as usize & (capacity - 1));
    entries
}

/// `java.util.HashMap`'s spread of `String.hashCode()`.
fn java_hash(key: &str) -> u32 {
    let hash = key.encode_utf16().fold(0u32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(u32::from(unit))
    });
    hash ^ (hash >> 16)
}

/// The tika-xmp converter `TikaToXMP` picks for a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Converter {
    MsOfficeBinary,
    MsOfficeXml,
    OpenDocument,
    Rtf,
    /// The fallback for every other type, which writes only the registered properties.
    Generic,
}

impl Converter {
    /// The converter of a document's `Content-Type`, else of its `dc:format`.
    fn of(metadata: &BTreeMap<String, Value>) -> Self {
        let value = |key: &str| metadata.get(key).and_then(first);
        value(CONTENT_TYPE)
            .or_else(|| value(FORMAT))
            .and_then(media_type)
            .map_or(Converter::Generic, |media_type| {
                Converter::serving(&media_type)
            })
    }

    /// The converter registered for a media type in Tika's canonical form: the types the
    /// OfficeParser, RTFParser, OOXMLParser and OpenDocumentParser of Tika 4.2.0 support.
    fn serving(media_type: &str) -> Self {
        match media_type {
            "application/msword"
            | "application/sldworks"
            | "application/vnd.ms-excel"
            | "application/vnd.ms-outlook"
            | "application/vnd.ms-powerpoint"
            | "application/vnd.ms-project"
            | "application/vnd.visio"
            | "application/x-mspublisher"
            | "application/x-tika-msoffice"
            | "application/x-tika-msoffice-embedded; format=ole10_native"
            | "application/x-tika-msworks-spreadsheet"
            | "application/x-tika-ole-drm-encrypted"
            | "application/x-tika-ooxml-protected" => Converter::MsOfficeBinary,
            "application/rtf" => Converter::Rtf,
            "application/vnd.ms-excel.addin.macroenabled.12"
            | "application/vnd.ms-excel.sheet.binary.macroenabled.12"
            | "application/vnd.ms-excel.sheet.macroenabled.12"
            | "application/vnd.ms-excel.template.macroenabled.12"
            | "application/vnd.ms-powerpoint.addin.macroenabled.12"
            | "application/vnd.ms-powerpoint.presentation.macroenabled.12"
            | "application/vnd.ms-powerpoint.slide.macroenabled.12"
            | "application/vnd.ms-powerpoint.slideshow.macroenabled.12"
            | "application/vnd.ms-powerpoint.template.macroenabled.12"
            | "application/vnd.ms-visio.drawing"
            | "application/vnd.ms-visio.drawing.macroenabled.12"
            | "application/vnd.ms-visio.stencil"
            | "application/vnd.ms-visio.stencil.macroenabled.12"
            | "application/vnd.ms-visio.template"
            | "application/vnd.ms-visio.template.macroenabled.12"
            | "application/vnd.ms-word.document.macroenabled.12"
            | "application/vnd.ms-word.template.macroenabled.12"
            | "application/vnd.ms-xpsdocument"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            | "application/vnd.openxmlformats-officedocument.presentationml.slide"
            | "application/vnd.openxmlformats-officedocument.presentationml.slideshow"
            | "application/vnd.openxmlformats-officedocument.presentationml.template"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.template"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.template"
            | "model/vnd.dwfx+xps" => Converter::MsOfficeXml,
            "application/vnd.oasis.opendocument.chart"
            | "application/vnd.oasis.opendocument.chart-template"
            | "application/vnd.oasis.opendocument.formula"
            | "application/vnd.oasis.opendocument.formula-template"
            | "application/vnd.oasis.opendocument.graphics"
            | "application/vnd.oasis.opendocument.graphics-template"
            | "application/vnd.oasis.opendocument.image"
            | "application/vnd.oasis.opendocument.image-template"
            | "application/vnd.oasis.opendocument.presentation"
            | "application/vnd.oasis.opendocument.presentation-template"
            | "application/vnd.oasis.opendocument.spreadsheet"
            | "application/vnd.oasis.opendocument.spreadsheet-template"
            | "application/vnd.oasis.opendocument.text"
            | "application/vnd.oasis.opendocument.text-master"
            | "application/vnd.oasis.opendocument.text-template"
            | "application/vnd.oasis.opendocument.text-web"
            | "application/vnd.sun.xml.writer"
            | "application/x-vnd.oasis.opendocument.chart"
            | "application/x-vnd.oasis.opendocument.chart-template"
            | "application/x-vnd.oasis.opendocument.formula"
            | "application/x-vnd.oasis.opendocument.formula-template"
            | "application/x-vnd.oasis.opendocument.graphics"
            | "application/x-vnd.oasis.opendocument.graphics-template"
            | "application/x-vnd.oasis.opendocument.image"
            | "application/x-vnd.oasis.opendocument.image-template"
            | "application/x-vnd.oasis.opendocument.presentation"
            | "application/x-vnd.oasis.opendocument.presentation-template"
            | "application/x-vnd.oasis.opendocument.spreadsheet"
            | "application/x-vnd.oasis.opendocument.spreadsheet-template"
            | "application/x-vnd.oasis.opendocument.text"
            | "application/x-vnd.oasis.opendocument.text-master"
            | "application/x-vnd.oasis.opendocument.text-template"
            | "application/x-vnd.oasis.opendocument.text-web" => Converter::OpenDocument,
            _ => Converter::Generic,
        }
    }

    fn convert(self, metadata: &BTreeMap<String, Value>) -> Packet<'_> {
        match self {
            Converter::MsOfficeBinary => Packet::mapped(MS_OFFICE_BINARY, metadata),
            Converter::MsOfficeXml => Packet::mapped(MS_OFFICE_XML, metadata),
            Converter::OpenDocument => Packet::mapped(OPEN_DOCUMENT, metadata),
            Converter::Rtf => Packet::mapped(RTF, metadata),
            Converter::Generic => Packet::generic(metadata),
        }
    }
}

/// Text trimmed as Java's `String.trim` trims it, of every character up to the space.
fn java_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c <= ' ')
}

/// A media type in the canonical form Tika's `MediaType.parse` gives it: type and subtype
/// trimmed and lowercased, then the parameters by lowercased name, each value unquoted. None
/// when it has no subtype.
fn media_type(text: &str) -> Option<String> {
    let (essence, parameters) = text.split_once(';').unwrap_or((text, ""));
    let (kind, subtype) = essence.split_once('/')?;
    if essence.contains('\u{1c}') {
        return None;
    }
    let parameters: BTreeMap<String, &str> = parameters
        .split(';')
        .filter_map(|parameter| {
            let (name, value) = parameter.split_once('=').unwrap_or((parameter, ""));
            let name = java_trim(name);
            let value = java_trim(value)
                .trim_start_matches(['"', '\''])
                .trim_end_matches(['"', '\'']);
            (!name.is_empty()).then(|| (name.to_lowercase(), value))
        })
        .collect();
    let mut canonical = [java_trim(kind), java_trim(subtype)]
        .map(str::to_lowercase)
        .join("/");
    for (name, value) in &parameters {
        canonical.push_str("; ");
        canonical.push_str(name);
        canonical.push('=');
        canonical.push_str(value);
    }
    Some(canonical)
}

/// The XMP of a document: its schemas in the order their first property was set, each with
/// its properties in the order they were first set, as XMPCore keeps them.
#[derive(Debug, Default)]
struct Packet<'m> {
    schemas: Vec<Schema<'m>>,
}

#[derive(Debug)]
struct Schema<'m> {
    namespace: Namespace,
    properties: Vec<(&'static str, Node<'m>)>,
}

/// Where in a packet's markup a value is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    Attribute,
    Text,
}

impl<'m> Packet<'m> {
    /// The packet a converter's steps write.
    fn mapped(steps: &[Step], metadata: &'m BTreeMap<String, Value>) -> Self {
        let mut packet = Packet::default();
        for step in steps {
            match *step {
                Step::Map(key, mapping, namespace, name) => {
                    if let Some(node) = metadata.get(key).and_then(|value| mapping.node(value)) {
                        packet.put(namespace, name, node);
                    }
                }
                Step::ApplicationVersion => {
                    let value_of = |key: &str| metadata.get(key).and_then(filled);
                    if let Some(application) = value_of(APPLICATION) {
                        let tool = match value_of(APP_VERSION) {
                            Some(version) => Cow::Owned(format!("{application} {version}")),
                            None => Cow::Borrowed(application),
                        };
                        packet.put(Namespace::Xmp, "CreatorTool", Node::Simple(scrub(tool)));
                    }
                }
            }
        }
        packet
    }

    /// The packet GenericConverter writes: each registered property in the order Tika lists
    /// the keys.
    fn generic(metadata: &'m BTreeMap<String, Value>) -> Self {
        let mut packet = Packet::default();
        for (key, value) in java_order(metadata) {
            let Some((namespace, name, mapping)) = registered(key) else {
                continue;
            };
            if let Some(node) = mapping.node(value) {
                packet.put(namespace, name, node);
            }
        }
        packet
    }

    /// Sets a property, in place when it is set already. A simple value never replaces an
    /// array nor an array a simple value: XMPCore refuses both, failing Tika's conversion, and
    /// the property keeps the value it was set to first.
    fn put(&mut self, namespace: Namespace, name: &'static str, node: Node<'m>) {
        let index = match self
            .schemas
            .iter()
            .position(|schema| schema.namespace == namespace)
        {
            Some(index) => index,
            None => {
                self.schemas.push(Schema {
                    namespace,
                    properties: Vec::new(),
                });
                self.schemas.len() - 1
            }
        };
        let properties = &mut self.schemas[index].properties;
        match properties.iter().position(|&(set, _)| set == name) {
            None => properties.push((name, node)),
            Some(index) => {
                let set = &mut properties[index].1;
                if matches!(
                    (&*set, &node),
                    (Node::Simple(_), Node::Simple(_)) | (Node::LangAlt(_), Node::LangAlt(_))
                ) {
                    *set = node;
                }
            }
        }
    }

    /// The packet as XMPCore serializes it with `omitPacketWrapper` and `useCompactFormat`:
    /// one description declaring every schema's namespace, the simple properties as its
    /// attributes and the arrays as its elements.
    fn serialize(&self) -> String {
        let mut out = String::with_capacity(1024);
        push(
            &mut out,
            &[
                "<x:xmpmeta xmlns:x=\"",
                ADOBE_META_URI,
                "\" x:xmptk=\"",
                TOOLKIT,
                "\">\n  <rdf:RDF xmlns:rdf=\"",
                RDF_URI,
                "\">\n    <rdf:Description rdf:about=\"\"",
            ],
        );
        for schema in &self.schemas {
            let namespace = schema.namespace;
            push(
                &mut out,
                &[
                    "\n        xmlns:",
                    namespace.prefix(),
                    "=\"",
                    namespace.uri(),
                    "\"",
                ],
            );
        }
        let properties = || {
            self.schemas.iter().flat_map(|schema| {
                schema
                    .properties
                    .iter()
                    .map(move |(name, node)| (schema.namespace, *name, node))
            })
        };
        let mut arrays = false;
        for (namespace, name, node) in properties() {
            match node {
                Node::Simple(value) => {
                    push(
                        &mut out,
                        &["\n      ", namespace.prefix(), ":", name, "=\""],
                    );
                    escape(&mut out, value, Context::Attribute);
                    out.push('"');
                }
                Node::LangAlt(_) | Node::Array(..) => arrays = true,
            }
        }
        if arrays {
            out.push_str(">\n");
            for (namespace, name, node) in properties() {
                let element = Element { namespace, name };
                match node {
                    Node::Simple(_) => {}
                    Node::LangAlt(value) => element.write(
                        &mut out,
                        ALT,
                        Some(DEFAULT_LANGUAGE),
                        &[Cow::Borrowed(*value)],
                    ),
                    Node::Array(array, items) => {
                        element.write(&mut out, array.element(), None, items)
                    }
                }
            }
            out.push_str("    </rdf:Description>\n");
        } else {
            out.push_str("/>\n");
        }
        out.push_str("  </rdf:RDF>\n</x:xmpmeta>\n");
        out
    }
}

fn push(out: &mut String, parts: &[&str]) {
    for part in parts {
        out.push_str(part);
    }
}

/// The element of a property whose value is an array.
struct Element {
    namespace: Namespace,
    name: &'static str,
}

impl Element {
    /// Writes the property with its array's container and items, each item in the language
    /// given if any.
    fn write(
        &self,
        out: &mut String,
        container: &str,
        language: Option<&str>,
        items: &[Cow<'_, str>],
    ) {
        let prefix = self.namespace.prefix();
        push(out, &["      <", prefix, ":", self.name, ">\n"]);
        write_container(out, container, language, items);
        push(out, &["      </", prefix, ":", self.name, ">\n"]);
    }
}

fn write_container(
    out: &mut String,
    container: &str,
    language: Option<&str>,
    items: &[Cow<'_, str>],
) {
    if items.is_empty() {
        push(out, &["        <", container, "/>\n"]);
        return;
    }
    push(out, &["        <", container, ">\n"]);
    for item in items {
        out.push_str("          <rdf:li");
        if let Some(language) = language {
            push(out, &[" xml:lang=\"", language, "\""]);
        }
        if item.is_empty() {
            out.push_str("/>\n");
        } else {
            out.push('>');
            escape(out, item, Context::Text);
            out.push_str("</rdf:li>\n");
        }
    }
    push(out, &["        </", container, ">\n"]);
}

/// Text escaped as XMPCore's `Utils.escapeXML` escapes a value, with whitespace escaped.
fn escape(out: &mut String, text: &str, context: Context) {
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' if context == Context::Attribute => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(entries: &[(&str, &[&str])]) -> BTreeMap<String, Value> {
        entries
            .iter()
            .map(|&(key, values)| {
                let value = match values {
                    [value] => Value::from(*value),
                    values => Value::from(values.to_vec()),
                };
                (key.to_string(), value)
            })
            .collect()
    }

    /// Asserts the packet of `entries` is `expected`, which Tika 4.2.0's `XMPMetadata` wrote
    /// for the same metadata.
    fn assert_packet(entries: &[(&str, &[&str])], expected: &str) {
        assert_eq!(packet(&metadata(entries)), expected);
    }

    #[test]
    fn items_are_separated_at_separators_and_runs_of_spaces() {
        assert_eq!(
            separate("Alice, Bob;\"Carol, D\" Eve  Frank\tGrace Hopper"),
            ["Alice", "Bob", "Carol, D", "Eve", "Frank", "Grace Hopper"]
        );
    }

    #[test]
    fn repeated_items_are_kept_once() {
        assert_eq!(
            separate("keyword1,keyword2,keyword1"),
            ["keyword1", "keyword2"]
        );
    }

    #[test]
    fn nothing_but_separators_is_no_item() {
        assert!(separate(" ,; \u{3001} ").is_empty());
    }

    #[test]
    fn doubled_quotes_are_undoubled_and_unclosed_quotes_run_to_the_end() {
        assert_eq!(
            separate("\"a \"\"b\"\"\", \u{ab}c\u{bb}\u{bb}d\u{bb}, \"open"),
            ["a \"b\"", "c\u{bb}d", "open"]
        );
    }

    #[test]
    fn media_types_are_canonical_as_tika_parses_them() {
        assert_eq!(
            media_type(" Application/RTF ").as_deref(),
            Some("application/rtf")
        );
        assert_eq!(
            media_type("application/x-tika-msoffice-embedded; FORMAT=\"ole10_native\"").as_deref(),
            Some("application/x-tika-msoffice-embedded; format=ole10_native")
        );
        assert_eq!(media_type("text"), None);
    }

    #[test]
    fn registered_properties_are_sorted_for_their_binary_search() {
        for (namespace, properties) in REGISTERED {
            assert!(
                properties.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "{namespace:?}"
            );
        }
    }

    #[test]
    fn registered_namespaces_are_listed_once() {
        for (index, (namespace, _)) in REGISTERED.iter().enumerate() {
            assert!(
                REGISTERED[index + 1..]
                    .iter()
                    .all(|(other, _)| other != namespace),
                "{namespace:?}"
            );
        }
    }

    #[test]
    fn the_office_open_xml_creator_tool_appends_the_application_version() {
        let with_version = packet(&metadata(&[
            (
                CONTENT_TYPE,
                &["application/vnd.ms-excel.sheet.macroenabled.12"],
            ),
            (APPLICATION, &["Microsoft Excel"]),
            (APP_VERSION, &["16.0300"]),
        ]));
        assert!(with_version.contains("xmp:CreatorTool=\"Microsoft Excel 16.0300\""));
        let without_version = packet(&metadata(&[
            (
                CONTENT_TYPE,
                &["application/vnd.ms-excel.sheet.macroenabled.12"],
            ),
            (APP_VERSION, &["16.0300"]),
        ]));
        assert!(!without_version.contains("xmp:CreatorTool"));
    }

    #[test]
    fn a_property_set_first_keeps_its_form_when_an_alias_conflicts() {
        let written = packet(&metadata(&[(TITLE, &["Title"]), ("xmp:Title", &["Alias"])]));
        assert!(written.contains("dc:title=\"Alias\""));
        assert!(!written.contains("<dc:title>"));
    }

    /// Tika's `convert_OOXMLMetadataWithMimetype_everythingConverted`.
    #[test]
    fn an_ooxml_document_goes_through_the_office_open_xml_converter() {
        assert_packet(
            &[
                (
                    "Content-Type",
                    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
                ),
                ("dc:language", &["language"]),
                ("dc:subject", &["keyword1,keyword2"]),
                ("dc:title", &["title"]),
                ("meta:last-author", &["lastModifiedBy"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties/"
      dc:format="application/vnd.openxmlformats-officedocument.wordprocessingml.document"
      dc:language="language"
      cp:lastModifiedBy="lastModifiedBy">
      <dc:subject>
        <rdf:Bag>
          <rdf:li>keyword1</rdf:li>
          <rdf:li>keyword2</rdf:li>
        </rdf:Bag>
      </dc:subject>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">title</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    /// Tika's `convert_OOXMLMetadataWithoutMimetype_onlyGeneralMetadataconverted`.
    #[test]
    fn without_a_type_only_registered_properties_are_written() {
        assert_packet(
            &[
                ("dc:language", &["language"]),
                ("dc:subject", &["keyword1,keyword2"]),
                ("dc:title", &["title"]),
                ("meta:last-author", &["lastModifiedBy"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/">
      <dc:language>
        <rdf:Bag>
          <rdf:li>language</rdf:li>
        </rdf:Bag>
      </dc:language>
      <dc:subject>
        <rdf:Bag>
          <rdf:li>keyword1,keyword2</rdf:li>
        </rdf:Bag>
      </dc:subject>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">title</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    /// Tika's `convert_genericMetadataAllQualified_allConverted`.
    #[test]
    fn registered_properties_of_an_unknown_type_are_written() {
        assert_packet(
            &[
                ("dc:format", &["generic/mimetype"]),
                ("dc:subject", &["keyword1", "keyword2"]),
                ("dc:title", &["title"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
      dc:format="generic/mimetype">
      <dc:subject>
        <rdf:Bag>
          <rdf:li>keyword1</rdf:li>
          <rdf:li>keyword2</rdf:li>
        </rdf:Bag>
      </dc:subject>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">title</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    /// Tika's `convert_wrongGenericMetadata_notConverted`.
    #[test]
    fn unqualified_and_unknown_keys_are_dropped() {
        assert_packet(
            &[
                ("noprefix", &["wrongKeyValue"]),
                ("unknown:key", &["unknownPrefixValue"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""/>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn a_pdf_is_written_in_the_order_tika_lists_its_keys() {
        assert_packet(
            &[
                ("Content-Type", &["application/pdf"]),
                ("dc:creator", &["Alice", "Bob"]),
                ("dc:description", &["desc"]),
                ("dc:format", &["application/pdf; version=1.4"]),
                ("dc:language", &["en"]),
                ("dc:subject", &["k1", "k2"]),
                ("dc:title", &["A <b> & \"c\""]),
                ("dcterms:created", &["2020-01-02T03:04:05Z"]),
                ("noprefix", &["v"]),
                ("pdf:chars-per-page", &["10", "20"]),
                ("pdf:docinfo:title", &["x"]),
                ("pdf:encrypted", &["false"]),
                ("pdf:pdf-version", &["1.4"]),
                ("pdf:producer", &["LibreOffice"]),
                ("pdf:unmapped-unicode-chars-per-page", &[""]),
                ("tiff:Software", &["Soft"]),
                ("unknown:key", &["v"]),
                ("xmp:CreateDate", &["2020-01-02T03:04:05Z"]),
                ("xmp:CreatorTool", &["Writer"]),
                ("xmpMM:DocumentID", &["uuid:1"]),
                ("xmpRights:UsageTerms", &["terms"]),
                ("xmpTPg:NPages", &["3"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:xmp="http://ns.adobe.com/xap/1.0/"
        xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
        xmlns:xmpTPg="http://ns.adobe.com/xap/1.0/t/pg/"
        xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
        xmlns:xmpRights="http://ns.adobe.com/xap/1.0/rights/"
      dc:format="application/pdf; version=1.4"
      xmp:CreatorTool="Soft"
      xmp:CreateDate="2020-01-02T03:04:05Z"
      pdf:producer="LibreOffice"
      pdf:pdf-version="1.4"
      pdf:encrypted="false"
      xmpTPg:NPages="3"
      xmpMM:DocumentID="uuid:1">
      <dc:description>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">desc</rdf:li>
        </rdf:Alt>
      </dc:description>
      <dc:language>
        <rdf:Bag>
          <rdf:li>en</rdf:li>
        </rdf:Bag>
      </dc:language>
      <dc:subject>
        <rdf:Bag>
          <rdf:li>k1</rdf:li>
          <rdf:li>k2</rdf:li>
        </rdf:Bag>
      </dc:subject>
      <dc:creator>
        <rdf:Seq>
          <rdf:li>Alice</rdf:li>
          <rdf:li>Bob</rdf:li>
        </rdf:Seq>
      </dc:creator>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">A &lt;b&gt; &amp; "c"</rdf:li>
        </rdf:Alt>
      </dc:title>
      <pdf:chars-per-page>
        <rdf:Seq>
          <rdf:li>10</rdf:li>
          <rdf:li>20</rdf:li>
        </rdf:Seq>
      </pdf:chars-per-page>
      <pdf:unmapped-unicode-chars-per-page>
        <rdf:Seq>
          <rdf:li/>
        </rdf:Seq>
      </pdf:unmapped-unicode-chars-per-page>
      <xmpRights:UsageTerms>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">terms</rdf:li>
        </rdf:Alt>
      </xmpRights:UsageTerms>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn the_office_open_xml_converter_maps_and_drops_as_tika_does() {
        assert_packet(
            &[
                (
                    "Content-Type",
                    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
                ),
                ("comment", &["hello"]),
                ("cp:category", &["Cat"]),
                ("cp:contentStatus", &["Draft"]),
                ("cp:revision", &["4"]),
                ("cp:version", &["1.0"]),
                (
                    "dc:creator",
                    &["Alice, Bob;\"Carol, D\" Eve  Frank", "Second"],
                ),
                ("dc:description", &["desc & more"]),
                ("dc:format", &["x"]),
                ("dc:identifier", &["id1"]),
                ("dc:language", &["en-US"]),
                ("dc:publisher", &["Pub"]),
                ("dc:subject", &["keyword1,keyword2,keyword1"]),
                ("dc:title", &["Title"]),
                ("dcterms:created", &["2020-01-02T03:04:05Z"]),
                ("dcterms:modified", &["2021-01-02T03:04:05Z"]),
                ("extended-properties:AppVersion", &["16.0000"]),
                (
                    "extended-properties:Application",
                    &["Microsoft Office Word"],
                ),
                ("extended-properties:Company", &["ACME"]),
                ("extended-properties:Manager", &["Boss"]),
                ("extended-properties:Notes", &["2"]),
                ("extended-properties:PresentationFormat", &["Wide"]),
                ("extended-properties:Template", &["Normal.dotm"]),
                ("extended-properties:TotalTime", &["5"]),
                ("meta:character-count", &["100"]),
                ("meta:character-count-with-spaces", &["120"]),
                ("meta:last-author", &["Mod"]),
                ("meta:line-count", &["10"]),
                ("meta:page-count", &["2"]),
                ("meta:paragraph-count", &["3"]),
                ("meta:print-date", &["2019-01-01T00:00:00Z"]),
                ("meta:slide-count", &["0"]),
                ("meta:word-count", &["20"]),
                ("w:Comments", &["hello2"]),
                ("xmpTPg:NPages", &["2"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"
        xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties/"
        xmlns:xmp="http://ns.adobe.com/xap/1.0/"
        xmlns:pdfx="http://ns.adobe.com/pdfx/1.3/"
        xmlns:extended-properties="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties/"
        xmlns:xmpTPg="http://ns.adobe.com/xap/1.0/t/pg/"
      dc:format="application/vnd.openxmlformats-officedocument.wordprocessingml.document"
      dc:identifier="id1"
      dc:language="en-US"
      Iptc4xmpCore:intellectualGenre="Cat"
      cp:contentStatus="Draft"
      cp:lastModifiedBy="Mod"
      cp:lastPrinted="2019-01-01T00:00:00Z"
      cp:revision="4"
      cp:version="1.0"
      xmp:CreateDate="2020-01-02T03:04:05Z"
      xmp:ModifyDate="2021-01-02T03:04:05Z"
      xmp:CreatorTool="Microsoft Office Word 16.0000"
      pdfx:Comments="hello2"
      extended-properties:Characters="100"
      extended-properties:CharactersWithSpaces="120"
      extended-properties:Company="Pub"
      extended-properties:Lines="10"
      extended-properties:Manager="Boss"
      extended-properties:Notes="2"
      extended-properties:Paragraphs="3"
      extended-properties:PresentationFormat="Wide"
      extended-properties:Slides="0"
      extended-properties:Template="Normal.dotm"
      extended-properties:TotalTime="5"
      extended-properties:Words="20"
      xmpTPg:NPages="2">
      <dc:creator>
        <rdf:Seq>
          <rdf:li>Alice</rdf:li>
          <rdf:li>Bob</rdf:li>
          <rdf:li>Carol, D</rdf:li>
          <rdf:li>Eve</rdf:li>
          <rdf:li>Frank</rdf:li>
        </rdf:Seq>
      </dc:creator>
      <dc:subject>
        <rdf:Bag>
          <rdf:li>keyword1</rdf:li>
          <rdf:li>keyword2</rdf:li>
        </rdf:Bag>
      </dc:subject>
      <dc:description>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">desc &amp; more</rdf:li>
        </rdf:Alt>
      </dc:description>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">Title</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn the_binary_office_converter_maps_and_drops_as_tika_does() {
        assert_packet(
            &[
                ("Content-Type", &["application/msword"]),
                ("cp:category", &["c"]),
                ("cp:revision", &["3"]),
                ("dc:creator", &[","]),
                ("dc:description", &["d"]),
                ("dc:language", &["da"]),
                ("dc:subject", &[" ; "]),
                ("dc:title", &[""]),
                ("dcterms:created", &["c1"]),
                ("dcterms:modified", &["m1"]),
                ("extended-properties:AppVersion", &["9"]),
                ("extended-properties:Application", &["Word"]),
                ("extended-properties:Company", &["ACME"]),
                ("extended-properties:Template", &["t"]),
                ("meta:character-count", &["7"]),
                ("meta:page-count", &["5"]),
                ("meta:print-date", &["p"]),
                ("meta:slide-count", &["2"]),
                ("meta:word-count", &["8"]),
                ("w:Comments", &["cm"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:xmp="http://ns.adobe.com/xap/1.0/"
        xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"
        xmlns:extended-properties="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties/"
        xmlns:pdfx="http://ns.adobe.com/pdfx/1.3/"
        xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties/"
        xmlns:xmpTPg="http://ns.adobe.com/xap/1.0/t/pg/"
      dc:format="application/msword"
      xmp:CreatorTool="Word"
      xmp:CreateDate="c1"
      xmp:ModifyDate="m1"
      Iptc4xmpCore:intellectualGenre="c"
      extended-properties:Characters="7"
      extended-properties:Company="ACME"
      extended-properties:Pages="2"
      extended-properties:Template="t"
      extended-properties:Words="8"
      pdfx:Comments="cm"
      cp:language="da"
      cp:lastPrinted="p"
      cp:revision="3"
      xmpTPg:NPages="5">
      <dc:creator>
        <rdf:Seq/>
      </dc:creator>
      <dc:subject>
        <rdf:Bag/>
      </dc:subject>
      <dc:description>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">d</rdf:li>
        </rdf:Alt>
      </dc:description>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn the_open_document_converter_maps_and_drops_as_tika_does() {
        assert_packet(
            &[
                ("Content-Type", &["application/vnd.oasis.opendocument.text"]),
                ("dc:creator", &["A"]),
                ("dc:description", &["d"]),
                ("dc:subject", &["s"]),
                ("dc:title", &["T"]),
                ("dcterms:created", &["c"]),
                ("dcterms:modified", &["m"]),
                ("editing-cycles", &["2"]),
                ("extended-properties:TotalTime", &["PT1M"]),
                ("generator", &["LO"]),
                ("initial-creator", &["IC"]),
                ("meta:character-count", &["1"]),
                ("meta:image-count", &["3"]),
                ("meta:object-count", &["4"]),
                ("meta:page-count", &["9"]),
                ("meta:paragraph-count", &["6"]),
                ("meta:table-count", &["7"]),
                ("meta:word-count", &["8"]),
                ("w:Comments", &["cm"]),
                ("xmpTPg:NPages", &["5"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0"
        xmlns:xmp="http://ns.adobe.com/xap/1.0/"
        xmlns:pdfx="http://ns.adobe.com/pdfx/1.3/"
        xmlns:xmpTPg="http://ns.adobe.com/xap/1.0/t/pg/"
      dc:format="application/vnd.oasis.opendocument.text"
      meta:character-count="1"
      meta:editing-duration="PT1M"
      meta:editing-cycles="2"
      meta:image-count="3"
      meta:initial-creator="IC"
      meta:object-count="4"
      meta:paragraph-count="6"
      meta:table-count="7"
      meta:word-count="8"
      xmp:CreateDate="c"
      xmp:ModifyDate="m"
      xmp:CreatorTool="LO"
      pdfx:Comments="cm"
      xmpTPg:NPages="5">
      <dc:creator>
        <rdf:Seq>
          <rdf:li>A</rdf:li>
        </rdf:Seq>
      </dc:creator>
      <dc:subject>
        <rdf:Bag>
          <rdf:li>s</rdf:li>
        </rdf:Bag>
      </dc:subject>
      <dc:description>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">d</rdf:li>
        </rdf:Alt>
      </dc:description>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">T</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn the_rtf_converter_is_chosen_whatever_the_case_and_spacing_of_the_type() {
        assert_packet(
            &[
                ("Content-Type", &["  Application/RTF  "]),
                ("cp:category", &["c"]),
                ("dc:creator", &["A;B"]),
                ("dc:description", &["d"]),
                ("dc:language", &["x"]),
                ("dc:subject", &["s"]),
                ("dc:title", &["T"]),
                ("extended-properties:Company", &["co"]),
                ("extended-properties:Manager", &["ma"]),
                ("extended-properties:Template", &["t"]),
                ("w:Comments", &["cm"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"
        xmlns:extended-properties="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties/"
        xmlns:pdfx="http://ns.adobe.com/pdfx/1.3/"
      dc:format="  Application/RTF  "
      Iptc4xmpCore:intellectualGenre="c"
      extended-properties:Template="t"
      extended-properties:Company="co"
      extended-properties:Manager="ma"
      pdfx:Comments="cm">
      <dc:creator>
        <rdf:Seq>
          <rdf:li>A</rdf:li>
          <rdf:li>B</rdf:li>
        </rdf:Seq>
      </dc:creator>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">T</rdf:li>
        </rdf:Alt>
      </dc:title>
      <dc:description>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">d</rdf:li>
        </rdf:Alt>
      </dc:description>
      <dc:subject>
        <rdf:Bag>
          <rdf:li>s</rdf:li>
        </rdf:Bag>
      </dc:subject>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn a_type_with_parameters_tika_does_not_register_falls_back_to_the_generic_converter() {
        assert_packet(
            &[
                ("Content-Type", &["APPLICATION/RTF ; charset=\"x\""]),
                ("dc:title", &["T"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/">
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">T</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn without_a_content_type_the_format_chooses_the_converter() {
        assert_packet(
            &[
                (
                    "dc:format",
                    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
                ),
                ("dc:title", &["T"]),
                ("meta:last-author", &["M"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties/"
        xmlns:dc="http://purl.org/dc/elements/1.1/"
      cp:lastModifiedBy="M">
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">T</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn markup_and_control_characters_are_escaped_as_xmpcore_escapes_them() {
        assert_packet(
            &[
                ("dc:format", &["a\tb\nc\rd\u{1}e > \"q\" <&'"]),
                ("dc:title", &["a\tb\nc\rd\u{1}e > 'q' 😀"]),
            ],
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Adobe XMP Core 6.1.11\">
  <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">
    <rdf:Description rdf:about=\"\"
        xmlns:dc=\"http://purl.org/dc/elements/1.1/\"
      dc:format=\"a&#x9;b&#xA;c&#xD;d e &gt; &quot;q&quot; &lt;&amp;'\">
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang=\"x-default\">a&#x9;b&#xA;c&#xD;d\u{1}e &gt; 'q' 😀</rdf:li>
        </rdf:Alt>
      </dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
",
        );
    }

    #[test]
    fn simple_properties_take_the_first_value_and_arrays_every_value() {
        assert_packet(
            &[
                ("dc:format", &["f1", "f2"]),
                ("dc:title", &["t1", "t2"]),
                ("exif:IsoSpeedRatings", &["100"]),
                ("xmp:About", &["a", "", "a"]),
                ("xmp:Rating", &[""]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:exif="http://ns.adobe.com/exif/1.0/"
        xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:xmp="http://ns.adobe.com/xap/1.0/"
      dc:format="f1">
      <exif:IsoSpeedRatings>
        <rdf:Seq>
          <rdf:li>100</rdf:li>
        </rdf:Seq>
      </exif:IsoSpeedRatings>
      <dc:title>
        <rdf:Alt>
          <rdf:li xml:lang="x-default">t1</rdf:li>
        </rdf:Alt>
      </dc:title>
      <xmp:About>
        <rdf:Bag>
          <rdf:li>a</rdf:li>
          <rdf:li/>
          <rdf:li>a</rdf:li>
        </rdf:Bag>
      </xmp:About>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn creators_are_separated_as_xmpcore_separates_them() {
        assert_packet(
            &[
                (
                    "Content-Type",
                    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
                ),
                ("dc:creator", &["“A, B” C,«D»»E», 〝F〞G〟 \"H\"\"I\" \"J"]),
            ],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
      dc:format="application/vnd.openxmlformats-officedocument.wordprocessingml.document">
      <dc:creator>
        <rdf:Seq>
          <rdf:li>A, B</rdf:li>
          <rdf:li>C</rdf:li>
          <rdf:li>D»E</rdf:li>
          <rdf:li>F</rdf:li>
          <rdf:li>G〟 "H""I" "J</rdf:li>
        </rdf:Seq>
      </dc:creator>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }

    #[test]
    fn an_alias_is_written_as_the_property_it_stands_for() {
        assert_packet(
            &[("xmp:Title", &["XT"])],
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 6.1.11">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/"
      dc:title="XT"/>
  </rdf:RDF>
</x:xmpmeta>
"#,
        );
    }
}
