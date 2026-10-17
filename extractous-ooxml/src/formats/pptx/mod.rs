//! PowerPoint presentations (PPTX, PPTM, PPSX, PPSM, POTX, POTM and the rest) as Tika's
//! `OOXMLParser` reads them with its streaming PowerPoint extractor,
//! `SXSLFPowerPointExtractorDecorator`: the package's metadata and its comment authors, then each
//! slide in a `slide-content` div with its layouts, notes, notes master, comments, diagram data
//! and charts, the slide masters and handout master, and the documents the slides, their parts,
//! the presentation and its masters embed, parsed as embedded documents.
//!
//! A package POI would read in a degraded way or refuse, and one whose parts or OLE objects Tika
//! records failing on, are left to Tika: see [`Presentation::check`].
//!
//! Where Tika slips, this port does not (#102): each comment holds its own text; only the
//! content of a placeholder is skipped, in the handout master too; each part resolves links by
//! its own relationships; relationships are ordered by the numbers of their ids, and slides
//! numbered among those the package has; and a link to an anchor inside the document is a link,
//! where Tika records it failing as a part POI cannot name.

use std::collections::HashMap;

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::docx::body::{Body, Context};
use crate::formats::docx::notes::InlineParts;
use crate::formats::docx::numbering::ListManager;
use crate::formats::docx::text::{TextHandler, TextOptions};
use crate::formats::metadata::{apply_page_numbers, TikaMetadata};
use crate::formats::ooxml::extractor::{self, Ignore};
use crate::formats::ooxml::{
    self, Package, Properties, Relationship, Unported, HYPERLINK_RELATIONSHIP,
};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::java::is_blank;

/// Relationship types, as POI's `XSLFRelation` and Tika name them.
mod kind {
    pub(super) use crate::formats::ooxml::extractor::kind::*;

    pub(super) const SLIDE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
    pub(super) const SLIDE_LAYOUT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout";
    pub(super) const SLIDE_MASTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster";
    pub(super) const HANDOUT_MASTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/handoutMaster";
    pub(super) const NOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
    pub(super) const NOTES_MASTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster";
    pub(super) const COMMENT_AUTHORS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/commentAuthors";
    pub(super) const VML_DRAWING: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing";
    pub(super) const DIAGRAM_DRAWING: &str =
        "http://schemas.microsoft.com/office/2007/relationships/diagramDrawing";
}

const THEME_MANAGER_TYPE: &str = "application/vnd.openxmlformats-officedocument.themeManager+xml";

/// The content types of a main presentation part, in the order `MAIN_STORY_PART_RELATIONS`
/// looks for the main document with them.
const MAIN_TYPES: [&str; 7] = [
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
    "application/vnd.ms-powerpoint.presentation.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.presentationml.slideshow.main+xml",
    "application/vnd.openxmlformats-officedocument.presentationml.template.main+xml",
    "application/vnd.ms-powerpoint.slideshow.macroEnabled.main+xml",
    "application/vnd.ms-powerpoint.template.macroEnabled.main+xml",
    THEME_MANAGER_TYPE,
];

/// The content types of a Word main document, which `OOXMLExtractorFactory` tries before those
/// of a presentation.
const WORD_MAIN_TYPES: [&str; 4] = [
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
    "application/vnd.ms-word.document.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
];

/// The kinds of relationships of a slide whose parts `addSlideParts` searches for what they
/// embed, in its order.
const SLIDE_PART_KINDS: [&str; 6] = [
    kind::VML_DRAWING,
    kind::SLIDE_LAYOUT,
    kind::NOTES_MASTER,
    kind::NOTES,
    kind::CHART,
    kind::DIAGRAM_DRAWING,
];

/// Metadata names this parser sets.
mod name {
    pub(super) use crate::formats::ooxml::extractor::name::*;

    pub(super) use crate::formats::metadata::office::{
        HAS_ANIMATIONS, HAS_HIDDEN_SLIDES, NUM_HIDDEN_SLIDES,
    };
}

/// The extractor Tika records having used.
const POWERPOINT_EXTRACTOR: &str =
    "org.apache.tika.parser.microsoft.ooxml.xslf.XSLFEventBasedPowerPointExtractor";

pub struct PptxParser;

impl Parser for PptxParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser"
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "application/vnd.ms-powerpoint.presentation.macroenabled.12",
            "application/vnd.openxmlformats-officedocument.presentationml.template",
            "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
            "application/vnd.ms-powerpoint.slideshow.macroenabled.12",
            "application/vnd.ms-powerpoint.addin.macroenabled.12",
            "application/vnd.ms-powerpoint.template.macroenabled.12",
            "application/vnd.ms-powerpoint.slide.macroenabled.12",
            "application/vnd.openxmlformats-officedocument.presentationml.slide",
        ]
    }

    /// Defers presentations whose parts or OLE objects Tika records failing on, which hold
    /// macros Tika extracts when asked to, and packages POI does not open as this port does.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let read = Presentation::open(input, pages, &configs.office)
            .and_then(Presentation::prepare)
            .map_err(|Unported(reason)| reason)?;
        Ok(Box::new(move |xhtml| read.write(xhtml)))
    }
}

/// A PowerPoint package being read.
struct Presentation<'a> {
    package: Package<'a>,
    config: &'a OfficeParserConfig,
    /// The main presentation part.
    main: usize,
}

impl<'a> Presentation<'a> {
    fn open(
        input: &'a [u8],
        pages: Pages<'a>,
        config: &'a OfficeParserConfig,
    ) -> Result<Self, Unported> {
        let mut package = Package::open(input, pages)?;
        package.order_relationships_numerically();
        let core = [kind::OFFICE_DOCUMENT, kind::STRICT_OFFICE_DOCUMENT]
            .iter()
            .find_map(|kind| Package::relationships_of(&package.relationships, kind).next())
            .and_then(|relationship| package.package_part(relationship))
            .map(|part| package.parts[part].content_type.as_str());
        let presentation =
            core.is_some_and(|core| !WORD_MAIN_TYPES.contains(&core) && MAIN_TYPES.contains(&core));
        if !presentation {
            return Err(Unported(format!(
                "the package's main part has the content type {core:?}, which Tika does not \
                 read as a presentation"
            )));
        }
        let main = MAIN_TYPES
            .iter()
            .find_map(|content_type| package.parts_by_content_type(content_type).first().copied())
            .ok_or_else(|| Unported("the package has no main presentation part".to_owned()))?;
        Ok(Self {
            package,
            config,
            main,
        })
    }

    fn relationships(&self, part: usize) -> &[Relationship] {
        &self.package.parts[part].relationships
    }

    /// `loadLinkedRelationships` of `part`, with its links to anchors inside the document,
    /// which Tika fails on (#102).
    fn links(&self, part: usize) -> HashMap<String, String> {
        let relationships = self.relationships(part);
        let mut links = extractor::linked_relationships(relationships, false);
        for relationship in Package::relationships_of(relationships, HYPERLINK_RELATIONSHIP) {
            if is_anchor(relationship) {
                links.insert(relationship.id.clone(), relationship.target.clone());
            }
        }
        links
    }

    /// The parts the relationships of `part` of `kind` lead to, skipping those that lead nowhere.
    fn related(&self, part: usize, kind: &str) -> Vec<usize> {
        Package::related(self.relationships(part), kind)
    }

    fn includes_master_content(&self) -> bool {
        self.config.includes_slide_master_content()
    }

    fn includes_notes(&self) -> bool {
        self.config.includes_slide_notes()
    }

    /// The parts `handleGeneralTextContainingPart` reads for a slide, in its order.
    fn slide_text_parts(&self, slide: usize) -> Vec<usize> {
        let mut parts = Vec::new();
        if self.includes_master_content() {
            parts.extend(self.related(slide, kind::SLIDE_LAYOUT));
        }
        if self.includes_notes() {
            parts.extend(self.related(slide, kind::NOTES));
            if self.includes_master_content() {
                parts.extend(self.related(slide, kind::NOTES_MASTER));
            }
        }
        for kind in [kind::COMMENTS, kind::DIAGRAM_DATA, kind::CHART] {
            parts.extend(self.related(slide, kind));
        }
        parts
    }

    /// Finds what this port leaves to Tika in a package it could open (parts whose XML Tika
    /// records failing on, pictures it cannot place, links and OLE objects it fails to read, and
    /// macros to extract), and reads what Tika reads whole: the properties and the comment
    /// authors. The parts written as they are read are only checked here.
    fn prepare(self) -> Result<Read<'a>, Unported> {
        let slides = self.related(self.main, kind::SLIDE);
        let mut authors = CommentAuthors::default();
        for part in self.related(self.main, kind::COMMENT_AUTHORS) {
            authors = ooxml::read_quietly(&self.package, part, authors)?.ok_or_else(|| {
                Unported(format!(
                    "Tika records a failure on {}",
                    self.package.name(part)
                ))
            })?;
        }
        let mut parts: Vec<usize> = Vec::new();
        for &slide in &slides {
            parts.push(slide);
            parts.extend(self.slide_text_parts(slide));
        }
        if self.includes_master_content() {
            parts.extend(self.related(self.main, kind::SLIDE_MASTER));
            parts.extend(self.related(self.main, kind::HANDOUT_MASTER));
        }
        for part in parts {
            if ooxml::read_quietly(&self.package, part, Ignore)?.is_none() {
                return Err(Unported(format!(
                    "Tika records a failure on {}",
                    self.package.name(part)
                )));
            }
        }
        for &slide in &slides {
            for relationship in Package::relationships_of(self.relationships(slide), kind::IMAGE) {
                if relationship.is_internal() && relationship.part.is_none() {
                    return Err(Unported(format!(
                        "the picture {} of {} is missing, which Tika records",
                        relationship.target_uri(),
                        self.package.name(slide)
                    )));
                }
            }
        }
        for source in self.embedding_parts() {
            for relationship in self.relationships(source) {
                if is_anchor(relationship) {
                    continue;
                }
                extractor::check_embedded(&self.package, self.config, source, relationship)?;
            }
        }
        let properties = Properties::read(&self.package)?;
        Ok(Read {
            presentation: self,
            properties,
            authors,
        })
    }

    /// `getMainDocumentParts`: each slide after the parts of it that may embed documents, then
    /// the presentation, its slide masters and its handout master.
    fn embedding_parts(&self) -> Vec<usize> {
        let mut parts = Vec::new();
        for slide in self.related(self.main, kind::SLIDE) {
            for kind in SLIDE_PART_KINDS {
                parts.extend(
                    Package::relationships_of(self.relationships(slide), kind)
                        .filter(|relationship| relationship.is_internal())
                        .filter_map(|relationship| relationship.part),
                );
            }
            parts.push(slide);
        }
        parts.push(self.main);
        for kind in [kind::SLIDE_MASTER, kind::HANDOUT_MASTER] {
            parts.extend(self.related(self.main, kind));
        }
        parts
    }

    /// `recordPicturePageRefs`: the slides each picture is on, numbered by the slide's place among
    /// the presentation's slides. Tika counts the slide relationships that lead to no part too
    /// (#102).
    fn picture_pages(&self) -> HashMap<usize, Vec<i32>> {
        let mut pages: HashMap<usize, Vec<i32>> = HashMap::new();
        for (number, slide) in (1..).zip(self.related(self.main, kind::SLIDE)) {
            for relationship in Package::relationships_of(self.relationships(slide), kind::IMAGE) {
                if let (true, Some(picture)) = (relationship.is_internal(), relationship.part) {
                    let on = pages.entry(picture).or_default();
                    if !on.contains(&number) {
                        on.push(number);
                    }
                }
            }
        }
        pages
    }

    fn write(
        &self,
        properties: &Properties,
        authors: &CommentAuthors,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        extractor::write_package_metadata(&self.package, xhtml.metadata_mut());
        xhtml
            .metadata_mut()
            .add(name::PARSED_BY, POWERPOINT_EXTRACTOR);
        properties.write(xhtml.metadata_mut());
        for name in &authors.names {
            if !is_blank(name) {
                xhtml
                    .metadata_mut()
                    .add(name::COMMENT_PERSONS, name.clone());
            }
        }
        let mut hidden = 0;
        for slide in self.related(self.main, kind::SLIDE) {
            if self.write_slide(slide, authors, xhtml)? {
                hidden += 1;
            }
        }
        if hidden > 0 {
            xhtml
                .metadata_mut()
                .set(name::NUM_HIDDEN_SLIDES, hidden.to_string());
        }
        if self.includes_master_content() {
            self.write_text_part(kind::SLIDE_MASTER, "slide-master", self.main, true, xhtml)?;
            // Tika skips no placeholder of the handout master (#102).
            self.write_text_part(
                kind::HANDOUT_MASTER,
                "slide-handout-master",
                self.main,
                true,
                xhtml,
            )?;
        }
        let pages = self.picture_pages();
        extractor::write_embedded_parts(
            &self.package,
            &self.embedding_parts(),
            &extractor::DescribedParts::new(),
            xhtml,
            &|part, metadata| {
                if let Some(pages) = pages.get(&part) {
                    apply_page_numbers(metadata, pages);
                }
            },
        )?;
        extractor::write_thumbnails(&self.package, xhtml)?;
        Ok(())
    }

    fn read<H: SaxHandler>(&self, part: usize, mut handler: H) -> Result<H, Failure> {
        let bytes = self.package.read(part)?;
        xml::parse_part(&bytes, self.package.name(part), &mut handler)?;
        Ok(handler)
    }

    /// `handleSlidePart`: the slide in a `slide-content` div, then its layouts, notes, notes
    /// master, comments, diagram data and charts, each part with its own links, where Tika
    /// resolves those of the layouts and notes by the slide's (#102). Whether the slide is
    /// hidden.
    fn write_slide(
        &self,
        slide: usize,
        authors: &CommentAuthors,
        xhtml: &mut Xhtml,
    ) -> Result<bool, Failure> {
        let links = self.links(slide);
        xhtml.start_element_with("div", &[("class", "slide-content")])?;
        let mut lists = ListManager::default();
        let inline = InlineParts::default();
        let body = Body::new(xhtml, Context::plain(&mut lists, &inline, true));
        let handler = self.read(
            slide,
            TextHandler::new(body, &links, TextOptions::default()),
        )?;
        let (hidden, animated) = (handler.is_hidden_slide(), handler.has_animations());
        drop(handler);
        if hidden {
            xhtml.metadata_mut().set(name::HAS_HIDDEN_SLIDES, "true");
        }
        if animated {
            xhtml.metadata_mut().set(name::HAS_ANIMATIONS, "true");
        }
        xhtml.end_element("div")?;
        if self.includes_master_content() {
            self.write_text_part(
                kind::SLIDE_LAYOUT,
                "slide-master-content",
                slide,
                true,
                xhtml,
            )?;
        }
        if self.includes_notes() {
            self.write_text_part(kind::NOTES, "slide-notes", slide, false, xhtml)?;
            if self.includes_master_content() {
                self.write_text_part(
                    kind::NOTES_MASTER,
                    "slide-notes-master",
                    slide,
                    false,
                    xhtml,
                )?;
            }
        }
        let mut comments = Comments::new(xhtml, authors);
        for part in self.related(slide, kind::COMMENTS) {
            extractor::write_general_text_part(&self.package, part, &mut comments)?;
        }
        self.write_text_part(kind::DIAGRAM_DATA, "diagram-data", slide, false, xhtml)?;
        self.write_text_part(kind::CHART, "chart", slide, false, xhtml)?;
        Ok(hidden)
    }

    /// `handleGeneralTextContainingPart` with the text handler, behind a `PlaceHolderSkipper`
    /// when `skip_placeholders`, in a div of the class `label`, each part resolving links by its
    /// own relationships. Tika hands the div to the text handler, which never writes it (#102).
    fn write_text_part(
        &self,
        kind: &str,
        label: &str,
        parent: usize,
        skip_placeholders: bool,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let parts: Vec<(usize, HashMap<String, String>)> = self
            .related(parent, kind)
            .into_iter()
            .map(|part| (part, self.links(part)))
            .collect();
        if parts.is_empty() {
            return Ok(());
        }
        xhtml.start_element_with("div", &[("class", label)])?;
        let none = HashMap::new();
        let mut lists = ListManager::default();
        let inline = InlineParts::default();
        let body = Body::new(xhtml, Context::plain(&mut lists, &inline, false));
        let mut handler = TextHandler::new(body, &none, TextOptions::default());
        for (part, links) in &parts {
            handler.set_relationships(links);
            if skip_placeholders {
                let mut skipper = PlaceHolderSkipper::new(&mut handler);
                extractor::write_general_text_part(&self.package, *part, &mut skipper)?;
            } else {
                extractor::write_general_text_part(&self.package, *part, &mut handler)?;
            }
        }
        drop(handler);
        Ok(xhtml.end_element("div")?)
    }
}

/// A presentation checked and read as far as Tika reads parts whole: what writing it takes.
struct Read<'a> {
    presentation: Presentation<'a>,
    properties: Properties,
    /// `loadCommentAuthors`.
    authors: CommentAuthors,
}

impl Read<'_> {
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        self.presentation
            .write(&self.properties, &self.authors, xhtml)
    }
}

/// `CommentAuthors`: the names and initials of the authors of comments, by id, the names in the
/// order the part first gives their ids. Tika keeps the names in Java `HashMap` order (#102).
#[derive(Default)]
struct CommentAuthors {
    names: Vec<String>,
    name_at: HashMap<String, usize>,
    initials: HashMap<String, String>,
}

impl CommentAuthors {
    fn set_name(&mut self, id: String, name: String) {
        match self.name_at.get(&id) {
            Some(&at) => self.names[at] = name,
            None => {
                self.name_at.insert(id, self.names.len());
                self.names.push(name);
            }
        }
    }

    fn name(&self, id: &str) -> Option<&str> {
        self.name_at.get(id).map(|&at| self.names[at].as_str())
    }
}

/// `XSLFCommentAuthorHandler`.
impl SaxHandler for CommentAuthors {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name != "cmAuthor" {
            return Ok(());
        }
        let value = |local: &str| {
            attributes
                .iter()
                .rev()
                .find(|attribute| attribute.local_name == local)
                .map(|attribute| attribute.value.clone())
        };
        let Some(id) = value("id") else {
            return Ok(());
        };
        if let Some(initials) = value("initials") {
            self.initials.insert(id.clone(), initials);
        }
        if let Some(name) = value("name") {
            self.set_name(id, name);
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

/// `XSLFCommentsHandler`: each comment in a paragraph headed by its author's name and initials,
/// with its text. Tika gives it all the text of the part since the previous comment (#102).
struct Comments<'a, 'x, 'h> {
    xhtml: &'a mut Xhtml<'x>,
    authors: &'h CommentAuthors,
    author: Option<String>,
    in_text: bool,
    text: String,
}

impl<'a, 'x, 'h> Comments<'a, 'x, 'h> {
    fn new(xhtml: &'a mut Xhtml<'x>, authors: &'h CommentAuthors) -> Self {
        Self {
            xhtml,
            authors,
            author: None,
            in_text: false,
            text: String::new(),
        }
    }
}

impl SaxHandler for Comments<'_, '_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "cm" => {
                self.author = xml::attribute(attributes, "", "authorId").map(str::to_owned);
                self.text.clear();
            }
            "text" => self.in_text = true,
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name == "text" {
            self.in_text = false;
        }
        if local_name != "cm" {
            return Ok(());
        }
        self.xhtml
            .start_element_with("p", &[("class", "slide-comment")])?;
        let id = self.author.take();
        let name = id.as_deref().and_then(|id| self.authors.name(id));
        let initials = id.as_deref().and_then(|id| self.authors.initials.get(id));
        if name.is_some() || initials.is_some() {
            self.xhtml.start_element("b")?;
            if let Some(name) = name {
                self.xhtml.characters(name)?;
            }
            if let (Some(_), Some(initials)) = (name, initials) {
                self.xhtml.characters(" (")?;
                self.xhtml.characters(initials)?;
                self.xhtml.characters(")")?;
            } else if let Some(initials) = initials {
                self.xhtml.characters(initials)?;
            }
            self.xhtml.end_element("b")?;
        }
        let text = std::mem::take(&mut self.text);
        self.xhtml.characters(&text)?;
        self.xhtml.end_element("p")?;
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_text {
            self.text.push_str(text);
        }
        Ok(())
    }
}

/// Whether `relationship` is a link to an anchor inside the document, such as `#_ftn1`.
fn is_anchor(relationship: &Relationship) -> bool {
    relationship.kind == HYPERLINK_RELATIONSHIP
        && relationship.is_internal()
        && relationship.target.starts_with('#')
}

/// The elements of a shape tree that may be a placeholder.
const SHAPES: [&str; 5] = ["sp", "pic", "graphicFrame", "cxnSp", "grpSp"];

/// `PlaceHolderSkipper`: drops what a placeholder shape holds from its placeholder mark to the
/// shape's end, the elements open at the mark still ended. Tika drops their ends too, and ends a
/// placeholder only at the end of a `sp`, so one of a picture or a frame goes on into the shapes
/// after it (#102).
struct PlaceHolderSkipper<'h, H> {
    inner: &'h mut H,
    /// The elements handed on that are open.
    open: Vec<String>,
    /// While a placeholder is skipped, how many elements of `open` stay open past its shape.
    shape: Option<usize>,
    /// The elements started and dropped since the mark that are open.
    skipped: usize,
}

impl<'h, H> PlaceHolderSkipper<'h, H> {
    fn new(inner: &'h mut H) -> Self {
        Self {
            inner,
            open: Vec::new(),
            shape: None,
            skipped: 0,
        }
    }
}

impl<H: SaxHandler> SaxHandler for PlaceHolderSkipper<'_, H> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if self.shape.is_none() && local_name == "ph" {
            self.shape = Some(
                self.open
                    .iter()
                    .rposition(|open| SHAPES.contains(&open.as_str()))
                    .unwrap_or(self.open.len()),
            );
        }
        if self.shape.is_some() {
            self.skipped += 1;
            return Ok(());
        }
        self.open.push(local_name.to_owned());
        self.inner.start_element(namespace, local_name, attributes)
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if self.skipped > 0 {
            self.skipped -= 1;
            if self.skipped == 0 && self.shape == Some(self.open.len()) {
                self.shape = None;
            }
            return Ok(());
        }
        self.open.pop();
        if self.shape == Some(self.open.len()) {
            self.shape = None;
        }
        self.inner.end_element(namespace, local_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.shape.is_some() {
            return Ok(());
        }
        self.inner.characters(text)
    }
}

#[cfg(test)]
mod tests;
