//! Word documents (DOCX, DOCM, DOTX, DOTM) as Tika's `OOXMLParser` reads them with its
//! streaming Word extractor, `SXWPFWordExtractorDecorator`: the package's metadata, then the
//! main document with its headers, notes, comments, diagrams, charts and footers, the glossary,
//! links to what lies outside the package and the thumbnail.
//!
//! The pictures, embedded objects and attachments of a document are parsed recursively, each
//! named as the document describes it: an OLE object by the file name the EMF picture of its
//! icon shows. A package POI would read in a degraded way or refuse, and one using what this
//! port does not read, is not read: see [`DocxParser::prepare`].

pub(crate) mod body;
pub mod flat;
pub(crate) mod notes;
pub(crate) mod numbering;
mod styles;
pub(crate) mod text;

use std::collections::{HashMap, HashSet};

use body::{unreferenced_comment, Body, Context, OleReferences};
use notes::{Collector, InlineParts};
use numbering::{ListManager, Numbering};
use styles::Styles;
use text::{TextHandler, TextOptions};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::embedded;
use crate::formats::metadata::TikaMetadata;
use crate::formats::metafile;
use crate::formats::ooxml::extractor::{self, Checked, DescribedPart, DescribedParts};
use crate::formats::ooxml::{self, Package, Properties, Relationship, Unported};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{unless_different, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Input;
use crate::input::Pages;
use crate::ordered_map::OrderedMap;

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
/// The content type of an EMF picture, whose name `resolveEmfNames` reads.
const EMF_TYPE: &str = "image/x-emf";
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

pub struct DocxParser;

impl Parser for DocxParser {
    fn name(&self) -> &'static str {
        "dijo::OOXMLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser"
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.ms-word.document.macroenabled.12",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
            "application/vnd.ms-word.template.macroenabled.12",
        ]
    }

    /// Leaves unread documents with an OLE object Tika fails to read, an EMF picture that embeds
    /// documents, parts read whole whose XML Tika reports failing on, parts declaring a document
    /// type, and packages POI does not open as this port does. Macros are extracted when asked
    /// to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let package = Package::open(input, pages)?;
        let read = Document::open(package, &configs.office)
            .and_then(Document::prepare)
            .map_err(|Unported(reason)| reason)?;
        let difference = read.checked.difference();
        unless_different(Box::new(move |xhtml| read.write(xhtml)), difference)
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
    fn open(package: Package<'a>, config: &'a OfficeParserConfig) -> Result<Self, Unported> {
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

    fn read(&self, part: usize) -> Result<Input<'a>, Failure> {
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

    /// Finds what this port does not read in a package it could open (embedded documents, parts
    /// read whole whose XML Tika fails on, document types, properties it does not read), and
    /// reads what Tika reads whole before it writes anything of a main document: the macros to
    /// extract, the properties, each story's numbering, styles, notes and comments, and the
    /// first story's settings. The parts written as they are read are only checked here, and
    /// parsed once, as they are written.
    fn prepare(self) -> Result<Read<'a>, Unported> {
        let mut checked = Checked::default();
        for &source in &self.main_parts() {
            for relationship in self.relationships(source) {
                extractor::check_embedded(
                    &self.package,
                    self.config,
                    source,
                    relationship,
                    &mut checked,
                )?;
            }
        }
        let icons = self.icon_names()?;
        let mut stories = Vec::new();
        for (index, &story) in self.stories.iter().chain(&self.glossaries).enumerate() {
            stories.push(self.read_story(story, index == 0 && !self.stories.is_empty())?);
        }
        let properties = Properties::read(&self.package)?;
        Ok(Read {
            document: self,
            properties,
            stories,
            icons,
            checked,
        })
    }

    /// The EMF pictures the parts `handleDocumentPart` writes relate to, each with the name of
    /// the file it shows when it is the icon of an OLE object, as `resolveEmfNames` reads that
    /// with Tika's `EMFParser` and swallows its failures: none where it names none, cannot be
    /// read or fails. A picture whose comments embed documents, which Tika would parse there, is
    /// not read.
    fn icon_names(&self) -> Result<IconNames, Unported> {
        let mut names = IconNames::new();
        let stories = self.stories.iter().chain(&self.glossaries);
        for part in stories.flat_map(|&story| self.streamed_parts(story)) {
            for relationship in self.relationships(part) {
                let Some(picture) = self.emf_picture(relationship) else {
                    continue;
                };
                if names.contains_key(&picture) {
                    continue;
                }
                let name = match self.package.read(picture) {
                    Ok(bytes) => metafile::emf_icon_name(&bytes).map_err(|Unported(reason)| {
                        Unported(format!("{}: {reason}", self.package.name(picture)))
                    })?,
                    Err(_) => None,
                };
                names.insert(picture, name);
            }
        }
        Ok(names)
    }

    /// The EMF picture an internal relationship leads to, as `resolveEmfNames` finds the icon
    /// of an OLE object by its picture's relationship.
    fn emf_picture(&self, relationship: &Relationship) -> Option<usize> {
        relationship.part.filter(|&part| {
            relationship.is_internal() && self.package.parts[part].content_type == EMF_TYPE
        })
    }

    /// `resolveEmfNames` of what the `references` of `part` say of the OLE objects it embeds,
    /// added to the `described` parts of the document under `part`.
    fn describe(
        &self,
        part: usize,
        references: OleReferences,
        icons: &IconNames,
        described: &mut DescribedParts,
    ) {
        for (id, reference) in references {
            let full_name = reference
                .icon
                .as_deref()
                .and_then(|icon| {
                    self.relationships(part)
                        .iter()
                        .find(|relationship| relationship.id == icon)
                })
                .and_then(|relationship| self.emf_picture(relationship))
                .and_then(|picture| {
                    icons
                        .get(&picture)
                        .expect("icon_names reads every EMF picture of a part written")
                        .clone()
                });
            let prog_id = reference.prog_id;
            described
                .entry(part)
                .or_default()
                .insert(id, DescribedPart { prog_id, full_name });
        }
    }

    /// Reads what writing `story` takes, and the parts it writes, which are parsed as they are
    /// written; the settings too for the `first` story.
    fn read_story(&self, story: usize, first: bool) -> Result<StoryParts<'a>, Unported> {
        let relationships = self.relationships(story);
        let styles = match Package::first_related(relationships, kind::STYLES) {
            Some(part) => Some(self.read_whole(part, Styles::default())?),
            None => None,
        };
        let inline = self.inline_parts(story)?;
        let streamed = self
            .streamed_parts(story)
            .into_iter()
            .map(|part| self.check_streamed(part))
            .collect::<Result<_, Unported>>()?;
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
            streamed,
            settings: first.then_some(StorySettings { settings, web }),
        })
    }

    /// Checks `part`, which is parsed as it is written. One declaring a document type, whose
    /// declarations Java reads and this port skips, is [`Unported`].
    fn check_streamed(&self, part: usize) -> Result<Streamed<'a>, Unported> {
        let name = self.package.name(part);
        let bytes = self
            .read(part)
            .map_err(|_| Unported(format!("the part {name} cannot be read")))?;
        if xml::has_document_type(&bytes) {
            return Err(Unported(format!("{name} declares a document type")));
        }
        Ok(Streamed {
            part,
            spooled: bytes.is_mapped().then_some(bytes),
        })
    }

    /// Reads `part` into `handler`, as a part whose failure Tika records, which this port does
    /// not read.
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

    /// `handleDocumentPart`, and of the first story the features `detectSecurityFeatures`
    /// reads of it, gathered as the story is written.
    fn write_document_part(
        &self,
        parts: StoryParts,
        icons: &IconNames,
        described: &mut DescribedParts,
        xhtml: &mut Xhtml,
    ) -> Result<Option<Security>, Failure> {
        let StoryParts {
            story,
            numbering,
            styles,
            inline,
            streamed,
            settings,
        } = parts;
        let mut lists = ListManager::new(numbering);
        let no_notes = InlineParts::default();
        let mut emitted = HashSet::new();
        let mut features = None;
        for Streamed { part, spooled } in streamed {
            let bytes = match spooled {
                Some(bytes) => bytes,
                None => self.read(part)?,
            };
            let is_story = part == story;
            let written = self.write_part(
                part,
                &bytes,
                PartContext {
                    styles: styles.as_ref(),
                    lists: &mut lists,
                    inline: if is_story { &inline } else { &no_notes },
                    features: (is_story && settings.is_some()).then(Features::default),
                },
                xhtml,
            )?;
            self.describe(part, written.references, icons, described);
            if is_story {
                emitted = written.emitted;
                features = written.features;
            }
        }
        for (id, events) in inline.comments.iter() {
            if !emitted.contains(id) {
                unreferenced_comment(xhtml, &inline, events)?;
            }
        }
        Ok(settings.map(|StorySettings { settings, web }| Security {
            features,
            settings,
            web,
        }))
    }

    /// `handlePart`: writes the part `part` of `bytes` with the text handler, and feeds the
    /// features asked for the same events. XML that fails is recorded as Tika records it, as a
    /// warning, after which the elements left open are closed and the document goes on.
    fn write_part(
        &self,
        part: usize,
        bytes: &Input,
        context: PartContext,
        xhtml: &mut Xhtml,
    ) -> Result<WrittenPart, Failure> {
        let PartContext {
            styles,
            lists,
            inline,
            mut features,
        } = context;
        let relationships = self.linked_relationships(part);
        let body = Body::new(
            xhtml,
            Context {
                styles,
                lists: Some(lists),
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
        let name = self.package.name(part);
        let parsed = match features.as_mut() {
            Some(features) => xml::parse_part(
                bytes,
                name,
                &mut WithFeatures {
                    text: &mut handler,
                    features,
                },
            ),
            None => xml::parse_part(bytes, name, &mut handler),
        };
        let mut body = handler.into_body();
        let thrown = ooxml::sax_exception(parsed)?;
        if thrown.is_some() {
            body.close_any_pending()?;
        }
        let (emitted, references) = body.finish();
        if let Some(thrown) = thrown {
            embedded::record_exception(xhtml.metadata_mut(), &thrown);
            features = None;
        }
        Ok(WrittenPart {
            emitted,
            references,
            features,
        })
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
        if let Some(features) = &security.features {
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

/// What writing a story takes: what Tika reads whole before it writes the story (its numbering,
/// styles, notes and comments, and for the first story its settings), and the parts it writes,
/// in order.
struct StoryParts<'a> {
    story: usize,
    numbering: Option<Numbering>,
    styles: Option<Styles>,
    inline: InlineParts,
    streamed: Vec<Streamed<'a>>,
    settings: Option<StorySettings>,
}

/// A part written as it is read, checked before anything is written: its bytes kept where they
/// were spooled to a file, which holds no memory, and decoded again as the part is written where
/// they were held in memory, so that what is kept stays bounded however many parts there are.
struct Streamed<'a> {
    part: usize,
    spooled: Option<Input<'a>>,
}

/// The settings `detectSecurityFeatures` reads of the first story.
struct StorySettings {
    settings: Option<Settings>,
    web: Option<WebSettings>,
}

/// What `detectSecurityFeatures` reads of the first story: its features, none where its XML
/// fails, which `XWPFFeatureExtractor` swallows, and its settings.
struct Security {
    features: Option<Features>,
    settings: Option<Settings>,
    web: Option<WebSettings>,
}

/// What writing a part of a story takes besides the part.
struct PartContext<'s, 'l> {
    styles: Option<&'s Styles>,
    lists: &'l mut ListManager,
    inline: &'s InlineParts,
    /// The features to gather of the part as it is written, if they are asked for.
    features: Option<Features>,
}

/// What writing a part found: the comments it inlined, what it says of the OLE objects it
/// embeds, and its features, if they were asked for and its XML did not fail.
struct WrittenPart {
    emitted: HashSet<String>,
    references: OleReferences,
    features: Option<Features>,
}

/// The text handler of a part, with its events fed to its features too.
struct WithFeatures<'t, 'f, T> {
    text: &'t mut T,
    features: &'f mut Features,
}

impl<T: SaxHandler> SaxHandler for WithFeatures<'_, '_, T> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.features
            .start_element(namespace, local_name, attributes)?;
        self.text.start_element(namespace, local_name, attributes)
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.text.end_element(namespace, local_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.text.characters(text)
    }
}

/// The name of the file each EMF picture of a document shows as the icon of an OLE object, if
/// it names one, by its part.
type IconNames = HashMap<usize, Option<String>>;

/// A Word document checked and read as far as Tika reads parts whole: what writing it takes.
struct Read<'a> {
    document: Document<'a>,
    properties: Properties,
    /// The parts of each story, then of each glossary.
    stories: Vec<StoryParts<'a>>,
    icons: IconNames,
    checked: Checked,
}

impl Read<'_> {
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let Read {
            document,
            properties,
            stories,
            icons,
            checked,
        } = self;
        let mut described = DescribedParts::new();
        document.write_package_metadata(xhtml.metadata_mut());
        properties.write(xhtml.metadata_mut());
        let mut stories = stories.into_iter();
        let mut security = None;
        for parts in stories.by_ref().take(document.stories.len()) {
            let read = document.write_document_part(parts, &icons, &mut described, xhtml)?;
            security = security.or(read);
        }
        if !document.glossaries.is_empty() {
            xhtml.start_element_with("div", &[("class", "glossary")])?;
            for parts in stories {
                document.write_document_part(parts, &icons, &mut described, xhtml)?;
            }
            xhtml.end_element("div")?;
        }
        if let (Some(&story), Some(security)) = (document.stories.first(), security) {
            document.write_security_features(story, security, xhtml)?;
        }
        extractor::write_embedded_parts(
            &document.package,
            &document.main_parts(),
            &checked,
            &described,
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
    let inline = InlineParts::default();
    let body = Body::new(xhtml, Context::plain(&inline, false));
    let mut handler = TextHandler::new(body, links, TextOptions::default());
    extractor::write_general_text_part(package, part, &mut handler)
}

/// `XWPFFeatureExtractor`'s handler: hidden text, tracked changes and their authors, comments.
#[derive(Default)]
struct Features {
    /// The authors of tracked changes, in the order they first appear, where Tika lists them in
    /// the order of a `java.util.HashSet` (#186).
    authors: OrderedMap<()>,
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
