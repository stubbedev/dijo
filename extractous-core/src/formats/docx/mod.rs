//! Word documents (DOCX, DOCM, DOTX, DOTM) as Tika's `OOXMLParser` reads them with its
//! streaming Word extractor, `SXWPFWordExtractorDecorator`: the package's metadata, then the
//! main document with its diagrams and charts, the glossary,
//! links to what lies outside the package and the thumbnail.
//!
//! Tika parses the pictures, embedded objects and attachments of a document recursively, with
//! OCR, so a document holding any is left to Tika, as is a package POI would read in a degraded
//! way or refuse, and one using what this port does not read: see [`DocxParser::defers_with`].

mod body;
mod numbering;
mod styles;
mod text;

use std::collections::HashMap;

use body::{Body, Context};
use numbering::{ListManager, Numbering};
use styles::Styles;
use text::{TextHandler, TextOptions};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::java::JavaHashMap;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::{
    self, Package, Relationship, Unported, HYPERLINK_RELATIONSHIP, RELATIONSHIPS_CONTENT_TYPE,
};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{Failure, Parser};

/// The WordprocessingML namespace.
const W_NAMESPACE: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Relationship types, as POI's `XWPFRelation`, `PackageRelationshipTypes` and Tika name them.
mod kind {
    pub(super) const OFFICE_DOCUMENT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    pub(super) const STRICT_OFFICE_DOCUMENT: &str =
        "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument";
    pub(super) const SIGNATURE: &str =
        "http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin";
    pub(super) const THUMBNAIL: &str =
        "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";
    pub(super) const HEADER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
    pub(super) const FOOTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
    pub(super) const FOOTNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
    pub(super) const ENDNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
    pub(super) const COMMENTS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
    pub(super) const DIAGRAM_DATA: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData";
    pub(super) const CHART: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
    pub(super) const STYLES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
    pub(super) const NUMBERING: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
    pub(super) const SETTINGS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
    pub(super) const WEB_SETTINGS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/webSettings";
    pub(super) const ATTACHED_TEMPLATE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate";
    pub(super) const SUBDOCUMENT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/subDocument";
    pub(super) const IMAGE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
    pub(super) const AUDIO: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/audio";
    pub(super) const VIDEO: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/video";
    pub(super) const MEDIA: &str = "http://schemas.microsoft.com/office/2007/relationships/media";
    pub(super) const PACKAGE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/package";
    pub(super) const OLE_OBJECT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject";
    pub(super) const ALTERNATE_FORMAT_CHUNK: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/aFChunk";
    pub(super) const VBA_PROJECT: &str =
        "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
}

/// The content types of a main document part, as `XWPFWordExtractor.SUPPORTED_TYPES` and
/// `getStoryDocumentParts` list them.
const MAIN_DOCUMENT_TYPES: [&str; 4] = [
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
    "application/vnd.ms-word.document.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
];
const GLOSSARY_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.glossary+xml";
const OLE_OBJECT_TYPE: &str = "application/vnd.openxmlformats-officedocument.oleObject";
/// The smallest OLE object Tika opens: three blocks of a compound file.
const SMALLEST_OLE_OBJECT: u64 = 512 * 3;
/// The kinds of relationships whose targets `loadLinkedRelationships` names by file.
const EMBEDDED_KINDS: [&str; 5] = [
    kind::AUDIO,
    kind::IMAGE,
    kind::PACKAGE,
    kind::OFFICE_DOCUMENT,
    kind::DIAGRAM_DATA,
];
/// The kinds of relationships of a main document whose parts may embed documents.
const MAIN_PART_KINDS: [&str; 6] = [
    kind::HEADER,
    kind::FOOTER,
    kind::FOOTNOTES,
    kind::ENDNOTES,
    kind::COMMENTS,
    kind::DIAGRAM_DATA,
];

/// Metadata names this parser sets.
mod name {
    pub(super) const PARSED_BY: &str = "tk:parsed-by";
    pub(super) const HAS_SIGNATURE: &str = "tk:has-signature";
    pub(super) const HAS_UNREFERENCED_PARTS: &str = "msoffice:has-unreferenced-parts";
    pub(super) const UNREFERENCED_PART_NAMES: &str = "msoffice:unreferenced-part-names";
    pub(super) const HAS_COMMENTS: &str = "msoffice:has-comments";
    pub(super) const HAS_HIDDEN_TEXT: &str = "msoffice:doc:has-hidden-text";
    pub(super) const HAS_TRACK_CHANGES: &str = "msoffice:has-track-changes";
    pub(super) const COMMENT_PERSONS: &str = "msoffice:comment-person-display-name";
    pub(super) const HAS_ATTACHED_TEMPLATE: &str = "msoffice:doc:has-attached-template";
    pub(super) const HAS_SUBDOCUMENTS: &str = "msoffice:doc:has-subdocuments";
    pub(super) const HAS_MAIL_MERGE: &str = "msoffice:doc:has-mail-merge";
    pub(super) const HAS_FRAMESETS: &str = "msoffice:doc:has-framesets";
    pub(super) const HAS_EXTERNAL_OLE_OBJECTS: &str = "msoffice:has-external-ole-objects";
}

/// The extractor Tika records having used.
const WORD_EXTRACTOR: &str =
    "org.apache.tika.parser.microsoft.ooxml.xwpf.XWPFEventBasedWordExtractor";

pub(crate) struct DocxParser;

impl Parser for DocxParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.ms-word.document.macroenabled.12",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
            "application/vnd.ms-word.template.macroenabled.12",
        ]
    }

    /// Defers documents with pictures, embedded objects or attachments Tika parses, macros it
    /// extracts when asked to, parts whose XML Tika reports failing on, and packages POI does
    /// not open as this port does.
    fn defers_with(&self, input: &[u8], configs: &ParserConfigs) -> Option<String> {
        Document::open(input, &configs.office)
            .and_then(|document| document.check())
            .err()
            .map(|Unported(reason)| reason)
    }

    fn parse(
        &self,
        input: &[u8],
        configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let document = Document::open(input, &configs.office)
            .map_err(|Unported(reason)| Failure::Malformed(reason))?;
        document.write(xhtml)
    }
}

/// A Word package being read.
struct Document<'a> {
    package: Package<'a>,
    config: &'a OfficeParserConfig,
    /// The main document parts, as `getStoryDocumentParts` finds them.
    stories: Vec<usize>,
    glossaries: Vec<usize>,
}

impl<'a> Document<'a> {
    fn open(input: &'a [u8], config: &'a OfficeParserConfig) -> Result<Self, Unported> {
        let package = Package::open(input)?;
        let core = [kind::OFFICE_DOCUMENT, kind::STRICT_OFFICE_DOCUMENT]
            .iter()
            .find_map(|kind| Package::relationships_of(&package.relationships, kind).next())
            .and_then(|relationship| package.package_part(relationship))
            .map(|part| package.parts[part].content_type.as_str());
        if !core.is_some_and(|core| MAIN_DOCUMENT_TYPES.contains(&core)) {
            return Err(Unported(format!(
                "the package's main part has the content type {core:?}, which Tika does not \
                 read as a Word document"
            )));
        }
        let stories = MAIN_DOCUMENT_TYPES
            .iter()
            .map(|content_type| package.parts_by_content_type(content_type))
            .find(|parts| !parts.is_empty())
            .unwrap_or_default();
        let glossaries = package.parts_by_content_type(GLOSSARY_TYPE);
        Ok(Self {
            package,
            config,
            stories,
            glossaries,
        })
    }

    fn relationships(&self, part: usize) -> &[Relationship] {
        &self.package.parts[part].relationships
    }

    fn read(&self, part: usize) -> Result<Vec<u8>, Failure> {
        self.package.read(part)
    }

    /// The parts that might embed documents, as `getMainDocumentParts` lists them.
    fn main_parts(&self) -> Vec<usize> {
        let stories: Vec<usize> = self
            .stories
            .iter()
            .chain(&self.glossaries)
            .copied()
            .collect();
        let mut parts: Vec<usize> = stories
            .iter()
            .flat_map(|&story| {
                MAIN_PART_KINDS
                    .iter()
                    .flat_map(move |kind| Package::related(self.relationships(story), kind))
            })
            .collect();
        parts.extend(stories);
        parts
    }

    /// The parts of a main document `handleDocumentPart` parses with the text handler, in order.
    fn text_parts(&self, story: usize) -> Vec<usize> {
        let relationships = self.relationships(story);
        let mut parts = vec![story];
        parts.extend(Package::related(relationships, kind::DIAGRAM_DATA));
        parts.extend(Package::related(relationships, kind::CHART));
        parts.extend(Package::first_related(relationships, kind::STYLES));
        parts
    }

    /// Finds what this port leaves to Tika in a package it could open: embedded documents,
    /// macros to extract, parts whose XML Tika fails on, and properties it does not read.
    fn check(&self) -> Result<(), Unported> {
        for &source in &self.main_parts() {
            for relationship in self.relationships(source) {
                self.check_embedded(source, relationship)?;
            }
        }
        let mut stories = self.stories.clone();
        stories.extend(&self.glossaries);
        for story in stories {
            for (kind, feature) in LEFT_FOR_NOW_PARTS {
                if Package::relationships_of(self.relationships(story), kind)
                    .next()
                    .is_some()
                {
                    return Err(Unported(format!(
                        "{} has {feature}, which this port leaves to Tika for now",
                        self.package.name(story)
                    )));
                }
            }
            for part in self.text_parts(story) {
                let name = self.package.name(part);
                let Some(scan) = ooxml::read_quietly(&self.package, part, LeftForNow::default())?
                else {
                    return Err(Unported(format!("Tika records a failure on {name}")));
                };
                if let Some(feature) = scan.found {
                    return Err(Unported(format!(
                        "{name} uses {feature}, which this port leaves to Tika for now"
                    )));
                }
            }
            let quietly_read = [
                Package::first_related(self.relationships(story), kind::NUMBERING),
                Package::first_related(self.relationships(story), kind::SETTINGS),
                Package::first_related(self.relationships(story), kind::WEB_SETTINGS),
            ];
            for part in quietly_read.into_iter().flatten() {
                ooxml::read_quietly(&self.package, part, Ignore)?;
            }
        }
        let mut metadata = TikaMetadata::new();
        ooxml::extract_properties(&self.package, &mut metadata)
    }

    /// What `handleEmbeddedPart` does with one relationship: an embedded document Tika parses
    /// is [`Unported`].
    fn check_embedded(&self, source: usize, relationship: &Relationship) -> Result<(), Unported> {
        if !relationship.is_internal() {
            return Ok(());
        }
        let Some(target) = relationship.part else {
            return Ok(());
        };
        let embedded = || {
            Err(Unported(format!(
                "{} embeds {} ({}), which Tika parses",
                self.package.name(source),
                self.package.name(target),
                relationship.kind
            )))
        };
        let target_part = &self.package.parts[target];
        match relationship.kind.as_str() {
            kind::OLE_OBJECT
                if target_part.content_type == OLE_OBJECT_TYPE
                    && self.package.size(target) < SMALLEST_OLE_OBJECT =>
            {
                Ok(())
            }
            kind::OLE_OBJECT
            | kind::IMAGE
            | kind::MEDIA
            | kind::VIDEO
            | kind::AUDIO
            | kind::PACKAGE
            | kind::ALTERNATE_FORMAT_CHUNK => embedded(),
            kind::VBA_PROJECT if self.config.extracts_macros() => embedded(),
            _ => Ok(()),
        }
    }

    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        self.write_package_metadata(xhtml.metadata_mut());
        ooxml::extract_properties(&self.package, xhtml.metadata_mut())
            .map_err(|Unported(reason)| Failure::Malformed(reason))?;
        for &story in &self.stories {
            self.write_document_part(story, xhtml)?;
        }
        if !self.glossaries.is_empty() {
            xhtml.start_element_with("div", &[("class", "glossary")])?;
            for &glossary in &self.glossaries {
                self.write_document_part(glossary, xhtml)?;
            }
            xhtml.end_element("div")?;
        }
        if let Some(&story) = self.stories.first() {
            self.write_security_features(story, xhtml)?;
        }
        self.write_external_references(xhtml)?;
        self.write_thumbnails(xhtml)?;
        Ok(())
    }

    /// What `OOXMLExtractorFactory` records before the extractor runs.
    fn write_package_metadata(&self, metadata: &mut TikaMetadata) {
        let package = &self.package;
        if Package::relationships_of(&package.relationships, kind::SIGNATURE)
            .next()
            .is_some()
        {
            metadata.set(name::HAS_SIGNATURE, "true");
        }
        let mut reachable = vec![false; package.parts.len()];
        let mut queue = std::collections::VecDeque::new();
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
        metadata.add(name::PARSED_BY, WORD_EXTRACTOR);
    }

    /// `handleDocumentPart`.
    fn write_document_part(&self, story: usize, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let relationships = self.relationships(story);
        let numbering = match Package::first_related(relationships, kind::NUMBERING) {
            Some(part) => self.read_quietly(part, Numbering::default())?,
            None => None,
        };
        let mut lists = ListManager::new(numbering);
        let styles = match Package::first_related(relationships, kind::STYLES) {
            Some(part) => Some(self.read_part(part, Styles::default())?),
            None => None,
        };
        self.write_part(story, styles.as_ref(), &mut lists, xhtml)?;
        let mut trailing = Package::related(relationships, kind::DIAGRAM_DATA);
        trailing.extend(Package::related(relationships, kind::CHART));
        for part in trailing {
            self.write_part(part, styles.as_ref(), &mut lists, xhtml)?;
        }
        Ok(())
    }

    /// Reads a part into `handler`, failing as Tika records a failure.
    fn read_part<H: SaxHandler>(&self, part: usize, mut handler: H) -> Result<H, Failure> {
        let bytes = self.read(part)?;
        xml::read_sax(&bytes, self.package.name(part), &mut handler)?;
        Ok(handler)
    }

    /// Reads a part into `handler`, `None` where it fails, which Tika passes over.
    fn read_quietly<H: SaxHandler>(&self, part: usize, handler: H) -> Result<Option<H>, Failure> {
        ooxml::read_quietly(&self.package, part, handler)
            .map_err(|Unported(reason)| Failure::Malformed(reason))
    }

    /// `handlePart`: writes a part with the text handler.
    fn write_part(
        &self,
        part: usize,
        styles: Option<&Styles>,
        lists: &mut ListManager,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let relationships = self.linked_relationships(part);
        let body = Body::new(
            xhtml,
            Context {
                styles,
                lists,
                records_metadata: true,
            },
        );
        let options = TextOptions {
            include_text_boxes: self.config.includes_shape_based_content(),
            concatenate_phonetic_runs: self.config.concatenates_phonetic_runs(),
            prefer_alternate_content_choice: true,
        };
        let mut handler = TextHandler::new(body, &relationships, options);
        let bytes = self.read(part)?;
        xml::read_sax(&bytes, self.package.name(part), &mut handler)
    }

    /// `loadLinkedRelationships`: the targets of a part's links by id, and the file names of
    /// what it embeds.
    fn linked_relationships(&self, part: usize) -> HashMap<String, String> {
        let relationships = self.relationships(part);
        let mut linked = HashMap::new();
        for relationship in Package::relationships_of(relationships, HYPERLINK_RELATIONSHIP) {
            linked.insert(
                relationship.id.clone(),
                relationship.target_uri().to_owned(),
            );
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

    /// `detectSecurityFeatures`.
    fn write_security_features(&self, story: usize, xhtml: &mut Xhtml) -> Result<(), Failure> {
        if let Some(features) = self.read_quietly(story, Features::default())? {
            features.write(xhtml.metadata_mut());
        }
        let relationships = self.relationships(story);
        for (kind, flag, class) in [
            (
                kind::ATTACHED_TEMPLATE,
                name::HAS_ATTACHED_TEMPLATE,
                "attachedTemplate",
            ),
            (kind::SUBDOCUMENT, name::HAS_SUBDOCUMENTS, "subDocument"),
        ] {
            let mut found = Package::relationships_of(relationships, kind).peekable();
            if found.peek().is_some() {
                xhtml.metadata_mut().set(flag, "true");
            }
            for relationship in found.filter(|relationship| !relationship.is_internal()) {
                external_reference(xhtml, class, relationship.target_uri())?;
            }
        }
        let settings = match Package::first_related(relationships, kind::SETTINGS) {
            Some(part) => self.read_quietly(part, Settings::default())?,
            None => None,
        };
        if let Some(settings) = settings {
            if settings.mail_merge {
                xhtml.metadata_mut().set(name::HAS_MAIL_MERGE, "true");
            }
        }
        let web = match Package::first_related(relationships, kind::WEB_SETTINGS) {
            Some(part) => self.read_quietly(part, WebSettings::default())?,
            None => None,
        };
        if let Some(web) = web {
            if web.frameset {
                xhtml.metadata_mut().set(name::HAS_FRAMESETS, "true");
            }
        }
        Ok(())
    }

    /// The links `handleEmbeddedParts` writes for the relationships of the main parts that
    /// lead out of the package.
    fn write_external_references(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        for source in self.main_parts() {
            for relationship in self.relationships(source) {
                if relationship.is_internal() {
                    continue;
                }
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
            }
        }
        Ok(())
    }

    /// `handleThumbnail`: a marker for each thumbnail, which extractous does not parse.
    fn write_thumbnails(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        for relationship in Package::relationships_of(&self.package.relationships, kind::THUMBNAIL)
        {
            if let Some(part) = self.package.package_part(relationship) {
                xhtml.start_element_with(
                    "div",
                    &[("class", "embedded"), ("id", self.package.name(part))],
                )?;
                xhtml.end_element("div")?;
            }
        }
        Ok(())
    }
}

/// `emitExternalRef`: an empty link of class `external-ref-…`, unless the URL is empty.
fn external_reference(xhtml: &mut Xhtml, class: &str, url: &str) -> Result<(), Stop> {
    if url.is_empty() {
        return Ok(());
    }
    let class = format!("external-ref-{class}");
    xhtml.start_element_with("a", &[("class", &class), ("href", url)])?;
    xhtml.end_element("a")
}

/// `FileHelper.getFilename(new File(uri))`: the last segment of the path `File` makes of it.
fn file_name(uri: &str) -> String {
    let path = uri.trim_end_matches('/');
    path.rfind('/')
        .map_or(path, |index| &path[index + 1..])
        .to_owned()
}

/// A handler that only lets the XML be read.
struct Ignore;

impl SaxHandler for Ignore {
    fn start_element(
        &mut self,
        _namespace: &str,
        _local_name: &str,
        _attributes: &[Attribute<'_>],
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

/// The elements of the features this port leaves to Tika for now, with the feature of each.
const LEFT_FOR_NOW: [(&str, &str); 7] = [
    ("footnoteReference", "notes"),
    ("endnoteReference", "notes"),
    ("commentReference", "comments"),
    ("ins", "tracked changes"),
    ("del", "tracked changes"),
    ("moveFrom", "tracked changes"),
    ("moveTo", "tracked changes"),
];

/// The relationships to the parts of the features this port leaves to Tika for now.
const LEFT_FOR_NOW_PARTS: [(&str, &str); 5] = [
    (kind::HEADER, "headers"),
    (kind::FOOTER, "footers"),
    (kind::FOOTNOTES, "notes"),
    (kind::ENDNOTES, "notes"),
    (kind::COMMENTS, "comments"),
];

/// Finds the first element of a feature in [`LEFT_FOR_NOW`].
#[derive(Default)]
struct LeftForNow {
    found: Option<&'static str>,
}

impl SaxHandler for LeftForNow {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute<'_>],
    ) -> Result<(), Failure> {
        if self.found.is_none() {
            self.found = LEFT_FOR_NOW
                .iter()
                .find(|(element, _)| *element == local_name)
                .map(|(_, feature)| *feature);
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// `XWPFFeatureExtractor`'s handler: hidden text, tracked changes and their authors, comments.
#[derive(Default)]
struct Features {
    authors: JavaHashMap<()>,
    hidden: bool,
    track_changes: bool,
    comments: bool,
}

impl SaxHandler for Features {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute<'_>],
    ) -> Result<(), Failure> {
        match local_name {
            "vanish" => self.hidden = true,
            "ins" | "del" | "moveFrom" | "moveTo" => {
                if let Some(author) = attributes
                    .iter()
                    .find(|attribute| attribute.local_name == "author")
                    .map(|attribute| attribute.value.as_ref())
                    .filter(|author| !crate::formats::handlers::is_blank(author))
                {
                    self.authors.insert(author, ());
                }
                self.track_changes = true;
            }
            "commentReference" | "commentRangeStart" => self.comments = true,
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

impl Features {
    fn write(&self, metadata: &mut TikaMetadata) {
        if self.comments {
            metadata.set(name::HAS_COMMENTS, "true");
        }
        if self.hidden {
            metadata.set(name::HAS_HIDDEN_TEXT, "true");
        }
        if self.track_changes {
            metadata.set(name::HAS_TRACK_CHANGES, "true");
        }
        for (author, ()) in self.authors.iter() {
            metadata.add(name::COMMENT_PERSONS, author);
        }
    }
}

/// `WordSettingsHandler`.
#[derive(Default)]
struct Settings {
    mail_merge: bool,
}

impl SaxHandler for Settings {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute<'_>],
    ) -> Result<(), Failure> {
        if local_name == "mailMerge" {
            self.mail_merge = true;
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// `WebSettingsHandler`.
#[derive(Default)]
struct WebSettings {
    frameset: bool,
}

impl SaxHandler for WebSettings {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute<'_>],
    ) -> Result<(), Failure> {
        if local_name == "frameset"
            || (local_name == "frame"
                && attribute_named(attributes, "src").is_some_and(|src| !src.is_empty()))
        {
            self.frameset = true;
        }
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
mod tests;
