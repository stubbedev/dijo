//! Word documents (DOCX, DOCM, DOTX, DOTM) as Tika's `OOXMLParser` reads them with its
//! streaming Word extractor, `SXWPFWordExtractorDecorator`: the package's metadata, then the
//! main document with its headers, notes, comments, diagrams, charts and footers, the glossary,
//! links to what lies outside the package and the thumbnail.
//!
//! Tika parses the pictures, embedded objects and attachments of a document recursively, with
//! OCR, so a document holding any is left to Tika, as is a package POI would read in a degraded
//! way or refuse, and one using what this port does not read: see [`DocxParser::defers_with`].

mod body;
mod notes;
mod numbering;
mod styles;
mod text;

use std::collections::{HashMap, HashSet};

use body::{unreferenced_comment, Body, Context};
use notes::{Collector, InlineParts};
use numbering::{ListManager, Numbering};
use styles::Styles;
use text::{TextHandler, TextOptions};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::java::JavaHashMap;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor::{self, Ignore};
use crate::formats::ooxml::{self, Package, Relationship, Unported, HYPERLINK_RELATIONSHIP};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{Failure, Parser};

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
    pub(super) use crate::formats::ooxml::extractor::name::*;

    pub(super) const HAS_HIDDEN_TEXT: &str = "msoffice:doc:has-hidden-text";
    pub(super) const HAS_TRACK_CHANGES: &str = "msoffice:has-track-changes";
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
    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        configs: &ParserConfigs,
    ) -> Option<String> {
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
        let footers_too = self.config.includes_headers_and_footers();
        let mut parts = Vec::new();
        if footers_too {
            parts.extend(Package::related(relationships, kind::HEADER));
        }
        for kind in [kind::FOOTNOTES, kind::ENDNOTES, kind::COMMENTS] {
            parts.extend(Package::related(relationships, kind));
        }
        parts.push(story);
        parts.extend(Package::related(relationships, kind::DIAGRAM_DATA));
        parts.extend(Package::related(relationships, kind::CHART));
        if footers_too {
            parts.extend(Package::related(relationships, kind::FOOTER));
        }
        parts.extend(Package::first_related(relationships, kind::STYLES));
        parts
    }

    /// Finds what this port leaves to Tika in a package it could open: embedded documents,
    /// macros to extract, parts whose XML Tika fails on, and properties it does not read.
    fn check(&self) -> Result<(), Unported> {
        for &source in &self.main_parts() {
            for relationship in self.relationships(source) {
                extractor::check_embedded(&self.package, self.config, source, relationship, true)?;
            }
        }
        let mut stories = self.stories.clone();
        stories.extend(&self.glossaries);
        for story in stories {
            for part in self.text_parts(story) {
                if ooxml::read_quietly(&self.package, part, Ignore)?.is_none() {
                    return Err(Unported(format!(
                        "Tika records a failure on {}",
                        self.package.name(part)
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
        extractor::write_embedded_parts(&self.package, &self.main_parts(), xhtml, &|_, _| {})?;
        extractor::write_thumbnails(&self.package, xhtml)?;
        Ok(())
    }

    /// What `OOXMLExtractorFactory` records before the extractor runs.
    fn write_package_metadata(&self, metadata: &mut TikaMetadata) {
        extractor::write_package_metadata(&self.package, metadata);
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
        let no_notes = InlineParts::default();
        let footers_too = self.config.includes_headers_and_footers();
        if footers_too {
            for header in Package::related(relationships, kind::HEADER) {
                self.write_part(header, styles.as_ref(), &mut lists, &no_notes, xhtml)?;
            }
        }
        let inline = self.inline_parts(story)?;
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
        Ok(())
    }

    /// Reads a part into `handler`, failing as Tika records a failure.
    fn read_part<H: SaxHandler>(&self, part: usize, mut handler: H) -> Result<H, Failure> {
        let bytes = self.read(part)?;
        xml::parse_sax(&bytes, self.package.name(part), &mut handler)?;
        Ok(handler)
    }

    /// Reads a part into `handler`, `None` where it fails, which Tika passes over.
    fn read_quietly<H: SaxHandler>(&self, part: usize, handler: H) -> Result<Option<H>, Failure> {
        ooxml::read_quietly(&self.package, part, handler)
            .map_err(|Unported(reason)| Failure::Malformed(reason))
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

    /// `collectInlineParts`: the footnotes, endnotes and comments of a main document.
    fn inline_parts(&self, story: usize) -> Result<InlineParts, Failure> {
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
                collector = self.read_part(part, collector)?;
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
                extractor::external_reference(xhtml, class, relationship.target_uri())?;
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
}

/// `FileHelper.getFilename(new File(uri))`: the last segment of the path `File` makes of it.
fn file_name(uri: &str) -> String {
    let path = uri.trim_end_matches('/');
    path.rfind('/')
        .map_or(path, |index| &path[index + 1..])
        .to_owned()
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
