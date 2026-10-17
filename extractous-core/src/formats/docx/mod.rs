//! Word documents (DOCX, DOCM, DOTX, DOTM) as Tika's `OOXMLParser` reads them with its
//! streaming Word extractor, `SXWPFWordExtractorDecorator`: the package's metadata, then the
//! main document with its headers, notes, comments, diagrams, charts and footers, the glossary,
//! links to what lies outside the package and the thumbnail.
//!
//! The pictures, embedded objects and attachments of a document are parsed recursively, as
//! Tika parses them, unless the document embeds an OLE object in its text, whose name Tika's
//! `EmbeddedPartMetadata` may take from the icon of its picture: such a document is left to
//! Tika, as is a package POI would read in a degraded way or refuse, and one using what this port
//! does not read: see [`DocxParser::prepare`].

pub(crate) mod body;
pub(crate) mod notes;
pub(crate) mod numbering;
mod styles;
pub(crate) mod text;

use std::collections::{HashMap, HashSet};

use body::{unreferenced_comment, Body, Context};
use notes::{Collector, InlineParts};
use numbering::{ListManager, Numbering};
use styles::Styles;
use text::{TextHandler, TextOptions};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor::{self, Ignore};
use crate::formats::ooxml::{self, Package, Properties, Relationship, Unported};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler, StartTag};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::JavaHashMap;

/// The WordprocessingML namespace.
const W_NAMESPACE: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Relationship types of Word documents, as POI's `XWPFRelation` names them.
mod kind {
    pub(super) use crate::formats::ooxml::extractor::kind::*;

    pub(super) const HEADER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
    pub(super) const FOOTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
    pub(super) const FOOTNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
    pub(super) const ENDNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
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
    pub(super) use crate::formats::ooxml::extractor::name::*;

    pub(super) use crate::formats::metadata::office::{HAS_HIDDEN_TEXT, HAS_TRACK_CHANGES};
    pub(super) const HAS_ATTACHED_TEMPLATE: &str = "msoffice:doc:has-attached-template";
    pub(super) const HAS_SUBDOCUMENTS: &str = "msoffice:doc:has-subdocuments";
    pub(super) const HAS_MAIL_MERGE: &str = "msoffice:doc:has-mail-merge";
    pub(super) const HAS_FRAMESETS: &str = "msoffice:doc:has-framesets";
}

/// The extractor Tika records having used.
const WORD_EXTRACTOR: &str =
    "org.apache.tika.parser.microsoft.ooxml.xwpf.XWPFEventBasedWordExtractor";

pub(crate) struct DocxParser;

impl Parser for DocxParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.ms-word.document.macroenabled.12",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
            "application/vnd.ms-word.template.macroenabled.12",
        ]
    }

    /// Defers documents whose text embeds OLE objects, whose embedded files Tika may name from
    /// the objects' icons, macros it extracts when asked to, parts whose XML Tika reports
    /// failing on, and packages POI does not open as this port does.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let read = Document::open(input, &configs.office)
            .and_then(Document::prepare)
            .map_err(|Unported(reason)| reason)?;
        Ok(Box::new(move |xhtml| read.write(xhtml)))
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

    /// The parts of a main document `handleDocumentPart` writes as it reads them with the text
    /// handler, in order: headers, the document, its diagrams and charts, and footers.
    fn streamed_parts(&self, story: usize) -> Vec<usize> {
        let relationships = self.relationships(story);
        let footers_too = self.config.includes_headers_and_footers();
        let mut parts = Vec::new();
        if footers_too {
            parts.extend(Package::related(relationships, kind::HEADER));
        }
        parts.push(story);
        parts.extend(Package::related(relationships, kind::DIAGRAM_DATA));
        parts.extend(Package::related(relationships, kind::CHART));
        if footers_too {
            parts.extend(Package::related(relationships, kind::FOOTER));
        }
        parts
    }

    /// Finds what this port leaves to Tika in a package it could open (embedded documents,
    /// macros to extract, parts whose XML Tika fails on, properties it does not read), and reads
    /// what Tika reads whole before it writes anything of a main document: the properties, each
    /// story's numbering, styles, notes and comments, and the first story's features and
    /// settings. The parts written as they are read are only checked here.
    fn prepare(self) -> Result<Read<'a>, Unported> {
        let mut stories = Vec::new();
        for (index, &story) in self.stories.iter().chain(&self.glossaries).enumerate() {
            stories.push(self.read_story(story, index == 0 && !self.stories.is_empty())?);
        }
        let named = stories.iter().any(|story| story.embeds_ole_objects);
        for &source in &self.main_parts() {
            for relationship in self.relationships(source) {
                extractor::check_embedded(&self.package, self.config, source, relationship, named)?;
            }
        }
        let properties = Properties::read(&self.package)?;
        Ok(Read {
            document: self,
            properties,
            stories,
        })
    }

    /// Reads what writing `story` takes and checks the parts it writes; the security features
    /// and settings too for the `first` story.
    fn read_story(&self, story: usize, first: bool) -> Result<StoryParts, Unported> {
        let relationships = self.relationships(story);
        let styles = match Package::first_related(relationships, kind::STYLES) {
            Some(part) => Some(self.read_whole(part, Styles::default())?),
            None => None,
        };
        let inline = self.inline_parts(story)?;
        let mut security = None;
        let mut embeds_ole_objects = false;
        for part in self.streamed_parts(story) {
            if first && part == story {
                let read = self.read_whole(part, OleEmbeds::new(Features::default()))?;
                embeds_ole_objects |= read.found;
                security = Some(read.inner);
            } else {
                embeds_ole_objects |= self.read_whole(part, OleEmbeds::new(Ignore))?.found;
            }
        }
        let numbering = match Package::first_related(relationships, kind::NUMBERING) {
            Some(part) => ooxml::read_quietly(&self.package, part, Numbering::default())?,
            None => None,
        };
        let settings = match Package::first_related(relationships, kind::SETTINGS) {
            Some(part) => ooxml::read_quietly(&self.package, part, Settings::default())?,
            None => None,
        };
        let web = match Package::first_related(relationships, kind::WEB_SETTINGS) {
            Some(part) => ooxml::read_quietly(&self.package, part, WebSettings::default())?,
            None => None,
        };
        Ok(StoryParts {
            story,
            numbering,
            styles,
            inline,
            embeds_ole_objects,
            security: security.map(|features| Security {
                features,
                settings,
                web,
            }),
        })
    }

    /// Reads `part` into `handler`, as a part whose failure Tika records, which this port leaves
    /// to Tika.
    fn read_whole<H: SaxHandler>(&self, part: usize, handler: H) -> Result<H, Unported> {
        ooxml::read_quietly(&self.package, part, handler)?.ok_or_else(|| {
            Unported(format!(
                "Tika records a failure on {}",
                self.package.name(part)
            ))
        })
    }

    /// What `OOXMLExtractorFactory` records before the extractor runs.
    fn write_package_metadata(&self, metadata: &mut TikaMetadata) {
        extractor::write_package_metadata(&self.package, metadata);
        metadata.add(name::PARSED_BY, WORD_EXTRACTOR);
    }

    /// `handleDocumentPart`.
    fn write_document_part(
        &self,
        parts: StoryParts,
        xhtml: &mut Xhtml,
    ) -> Result<Option<Security>, Failure> {
        let StoryParts {
            story,
            numbering,
            styles,
            inline,
            security,
            ..
        } = parts;
        let relationships = self.relationships(story);
        let mut lists = ListManager::new(numbering);
        let no_notes = InlineParts::default();
        let footers_too = self.config.includes_headers_and_footers();
        if footers_too {
            for header in Package::related(relationships, kind::HEADER) {
                self.write_part(header, styles.as_ref(), &mut lists, &no_notes, xhtml)?;
            }
        }
        let emitted = self.write_part(story, styles.as_ref(), &mut lists, &inline, xhtml)?;
        let mut trailing = Package::related(relationships, kind::DIAGRAM_DATA);
        trailing.extend(Package::related(relationships, kind::CHART));
        if footers_too {
            trailing.extend(Package::related(relationships, kind::FOOTER));
        }
        for part in trailing {
            self.write_part(part, styles.as_ref(), &mut lists, &no_notes, xhtml)?;
        }
        for (id, events) in inline.comments.iter() {
            if !emitted.contains(id) {
                unreferenced_comment(xhtml, &inline, events)?;
            }
        }
        Ok(security)
    }

    /// `handlePart`: writes a part with the text handler; returns the comments it inlined.
    fn write_part(
        &self,
        part: usize,
        styles: Option<&Styles>,
        lists: &mut ListManager,
        inline: &InlineParts,
        xhtml: &mut Xhtml,
    ) -> Result<HashSet<String>, Failure> {
        let relationships = self.linked_relationships(part);
        let body = Body::new(
            xhtml,
            Context {
                styles,
                lists,
                inline,
                records_metadata: true,
                include_deleted: self.config.includes_deleted_content(),
                include_move_from: self.config.includes_move_from_content(),
            },
        );
        let options = TextOptions {
            include_text_boxes: self.config.includes_shape_based_content(),
            concatenate_phonetic_runs: self.config.concatenates_phonetic_runs(),
            prefer_alternate_content_choice: true,
        };
        let mut handler = TextHandler::new(body, &relationships, options);
        let bytes = self.read(part)?;
        xml::parse_sax(&bytes, self.package.name(part), &mut handler)?;
        Ok(handler.into_body().into_emitted_comments())
    }

    /// `loadLinkedRelationships`: the targets of a part's links by id, and the file names of
    /// what it embeds.
    fn linked_relationships(&self, part: usize) -> HashMap<String, String> {
        extractor::linked_relationships(self.relationships(part), true)
    }

    /// `collectInlineParts`: the footnotes, endnotes and comments of a main document.
    fn inline_parts(&self, story: usize) -> Result<InlineParts, Unported> {
        let mut inline = InlineParts::default();
        let sources = [
            (kind::FOOTNOTES, "footnote", &["0", "-1"][..]),
            (kind::ENDNOTES, "endnote", &["0", "-1"][..]),
            (kind::COMMENTS, "comment", &[][..]),
        ];
        for (index, (kind, wrapper, skipped)) in sources.into_iter().enumerate() {
            let mut collector = Collector::new(wrapper, skipped);
            for part in Package::related(self.relationships(story), kind) {
                inline.relationships.extend(self.linked_relationships(part));
                collector = self.read_whole(part, collector)?;
            }
            for (id, events) in collector.into_notes() {
                match index {
                    0 => {
                        inline.footnotes.insert(id, events);
                    }
                    1 => {
                        inline.endnotes.insert(id, events);
                    }
                    _ => inline.comments.insert(&id, events),
                }
            }
        }
        Ok(inline)
    }

    /// `detectSecurityFeatures` of `story`, which `security` holds the reads of.
    fn write_security_features(
        &self,
        story: usize,
        security: Security,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        security.features.write(xhtml.metadata_mut());
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
                extractor::external_reference(xhtml, class, relationship.target_uri())?;
            }
        }
        if security
            .settings
            .is_some_and(|settings| settings.mail_merge)
        {
            xhtml.metadata_mut().set(name::HAS_MAIL_MERGE, "true");
        }
        if security.web.is_some_and(|web| web.frameset) {
            xhtml.metadata_mut().set(name::HAS_FRAMESETS, "true");
        }
        Ok(())
    }
}

/// What writing a story takes that Tika reads whole before it writes the story: its numbering,
/// styles, notes and comments, and for the first story its security features and settings.
struct StoryParts {
    story: usize,
    numbering: Option<Numbering>,
    styles: Option<Styles>,
    inline: InlineParts,
    /// Whether a part `handlePart` reads embeds an OLE object, for which Tika's body handler
    /// may record an `EmbeddedPartMetadata`.
    embeds_ole_objects: bool,
    security: Option<Security>,
}

/// What `detectSecurityFeatures` reads of the first story.
struct Security {
    features: Features,
    settings: Option<Settings>,
    web: Option<WebSettings>,
}

/// A Word document checked and read as far as Tika reads parts whole: what writing it takes.
struct Read<'a> {
    document: Document<'a>,
    properties: Properties,
    /// The parts of each story, then of each glossary.
    stories: Vec<StoryParts>,
}

impl Read<'_> {
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let Read {
            document,
            properties,
            stories,
        } = self;
        document.write_package_metadata(xhtml.metadata_mut());
        properties.write(xhtml.metadata_mut());
        let mut stories = stories.into_iter();
        let mut security = None;
        for parts in stories.by_ref().take(document.stories.len()) {
            let read = document.write_document_part(parts, xhtml)?;
            security = security.or(read);
        }
        if !document.glossaries.is_empty() {
            xhtml.start_element_with("div", &[("class", "glossary")])?;
            for parts in stories {
                document.write_document_part(parts, xhtml)?;
            }
            xhtml.end_element("div")?;
        }
        if let (Some(&story), Some(security)) = (document.stories.first(), security) {
            document.write_security_features(story, security, xhtml)?;
        }
        extractor::write_embedded_parts(
            &document.package,
            &document.main_parts(),
            xhtml,
            &|_, _| {},
        )?;
        extractor::write_thumbnails(&document.package, xhtml)?;
        Ok(())
    }
}

/// What `handleGeneralTextContainingPart` writes for `part`, read by one of Tika's text
/// handlers at its defaults, `new OOXMLWordAndPowerPointTextHandler(new
/// OOXMLTikaBodyPartHandler(xhtml), links)`, without styles, lists, notes or metadata.
pub(crate) fn write_general_text_part(
    package: &Package,
    part: usize,
    links: &HashMap<String, String>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let mut lists = ListManager::new(None);
    let inline = InlineParts::default();
    let body = Body::new(xhtml, Context::plain(&mut lists, &inline, false));
    let mut handler = TextHandler::new(body, links, TextOptions::default());
    extractor::write_general_text_part(package, part, &mut handler)
}

/// A handler passing a part on to `inner`, finding whether it embeds an OLE object, an
/// `OLEObject` of the type `Embed`, whose relationship Tika's `OOXMLTikaBodyPartHandler` records
/// an `EmbeddedPartMetadata` for when it names a program or follows a picture.
struct OleEmbeds<H> {
    inner: H,
    found: bool,
}

impl<H: SaxHandler> OleEmbeds<H> {
    fn new(inner: H) -> Self {
        Self {
            inner,
            found: false,
        }
    }

    fn note(&mut self, local_name: &str, attributes: &[Attribute]) {
        self.found |= local_name == "OLEObject"
            && attributes
                .iter()
                .any(|attribute| attribute.local_name == "Type" && attribute.value == "Embed");
    }
}

impl<H: SaxHandler> SaxHandler for OleEmbeds<H> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.note(local_name, attributes);
        self.inner.start_element(namespace, local_name, attributes)
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.inner.end_element(namespace, local_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.inner.characters(text)
    }

    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        self.note(tag.local_name, tag.attributes);
        self.inner.start_tag(tag)
    }

    fn document_type(&mut self) -> Result<(), Failure> {
        self.inner.document_type()
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
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "vanish" => self.hidden = true,
            "ins" | "del" | "moveFrom" | "moveTo" => {
                if let Some(author) = attributes
                    .iter()
                    .find(|attribute| attribute.local_name == "author")
                    .map(|attribute| attribute.value.as_ref())
                    .filter(|author| !crate::java::is_blank(author))
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
        _attributes: &[Attribute],
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
        attributes: &[Attribute],
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
