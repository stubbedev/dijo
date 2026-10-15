//! PowerPoint presentations (PPTX, PPTM, PPSX, PPSM, POTX, POTM and the rest) as Tika's
//! `OOXMLParser` reads them with its streaming PowerPoint extractor,
//! `SXSLFPowerPointExtractorDecorator`: the package's metadata and its comment authors, then each
//! slide in a `slide-content` div with its layouts, notes, notes master, comments, diagram data
//! and charts, the slide masters and handout master, and the documents the slides, their parts,
//! the presentation and its masters embed, parsed as embedded documents.
//!
//! A package POI would read in a degraded way or refuse, and one whose parts or OLE objects Tika
//! records failing on, are left to Tika: see [`Presentation::check`].

use std::collections::HashMap;

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::docx::body::{Body, Context};
use crate::formats::docx::notes::InlineParts;
use crate::formats::docx::numbering::ListManager;
use crate::formats::docx::text::{TextHandler, TextOptions};
use crate::formats::docx::write_general_text_parts;
use crate::formats::handlers::is_blank;
use crate::formats::java::JavaHashMap;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor::{self, Ignore};
use crate::formats::ooxml::{self, Package, Properties, Relationship, Unported};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Deferral, Failure, Parser, Prepared};

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

    pub(super) const HAS_HIDDEN_SLIDES: &str = "msoffice:ppt:has-hidden-slides";
    pub(super) const NUM_HIDDEN_SLIDES: &str = "msoffice:ppt:num-hidden-slides";
    pub(super) const HAS_ANIMATIONS: &str = "msoffice:ppt:has-animations";
    pub(super) const PAGE_NUMBER: &str = "tk:page:number";
    pub(super) const PAGE_NUMBERS: &str = "tk:page:numbers";
}

/// The extractor Tika records having used.
const POWERPOINT_EXTRACTOR: &str =
    "org.apache.tika.parser.microsoft.ooxml.xslf.XSLFEventBasedPowerPointExtractor";

pub(crate) struct PptxParser;

impl Parser for PptxParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
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
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Deferral> {
        let read = Presentation::open(input, &configs.office)
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
    fn open(input: &'a [u8], config: &'a OfficeParserConfig) -> Result<Self, Unported> {
        let package = Package::open(input)?;
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
                extractor::check_embedded(&self.package, self.config, source, relationship, false)?;
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
    /// the presentation's slide relationships.
    fn picture_pages(&self) -> HashMap<usize, Vec<i32>> {
        let mut pages: HashMap<usize, Vec<i32>> = HashMap::new();
        let slides = Package::relationships_of(self.relationships(self.main), kind::SLIDE);
        for (number, slide) in (1..).zip(slides) {
            let Some(slide) = slide.part else {
                continue;
            };
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
        for name in authors.names.iter().map(|(_, name)| name) {
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
            let no_links = HashMap::new();
            self.write_text_part(
                kind::SLIDE_MASTER,
                "slide-master",
                self.main,
                &no_links,
                true,
                xhtml,
            )?;
            self.write_text_part(
                kind::HANDOUT_MASTER,
                "slide-handout-master",
                self.main,
                &no_links,
                false,
                xhtml,
            )?;
        }
        let pages = self.picture_pages();
        extractor::write_embedded_parts(
            &self.package,
            &self.embedding_parts(),
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
        xml::parse_sax(&bytes, self.package.name(part), &mut handler)?;
        Ok(handler)
    }

    /// `handleSlidePart`: the slide in a `slide-content` div, then its layouts, notes, notes
    /// master, comments, diagram data and charts. Whether the slide is hidden.
    fn write_slide(
        &self,
        slide: usize,
        authors: &CommentAuthors,
        xhtml: &mut Xhtml,
    ) -> Result<bool, Failure> {
        let links = extractor::linked_relationships(self.relationships(slide), false);
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
                &links,
                true,
                xhtml,
            )?;
        }
        if self.includes_notes() {
            self.write_text_part(kind::NOTES, "slide-notes", slide, &links, false, xhtml)?;
            if self.includes_master_content() {
                self.write_text_part(
                    kind::NOTES_MASTER,
                    "slide-notes-master",
                    slide,
                    &links,
                    false,
                    xhtml,
                )?;
            }
        }
        let mut comments = Comments::new(xhtml, authors);
        extractor::write_general_text_parts(
            &self.package,
            slide,
            kind::COMMENTS,
            "",
            &mut comments,
        )?;
        self.write_text_part(
            kind::DIAGRAM_DATA,
            "diagram-data",
            slide,
            &links,
            false,
            xhtml,
        )?;
        self.write_text_part(kind::CHART, "chart", slide, &links, false, xhtml)?;
        Ok(hidden)
    }

    /// `handleGeneralTextContainingPart` with the text handler, behind a `PlaceHolderSkipper`
    /// when `skip_placeholders`.
    fn write_text_part(
        &self,
        kind: &str,
        label: &str,
        parent: usize,
        links: &HashMap<String, String>,
        skip_placeholders: bool,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        if !skip_placeholders {
            return write_general_text_parts(&self.package, parent, kind, label, links, xhtml);
        }
        let mut lists = ListManager::default();
        let inline = InlineParts::default();
        let body = Body::new(xhtml, Context::plain(&mut lists, &inline, false));
        let mut handler = TextHandler::new(body, links, TextOptions::default());
        let mut skipper = PlaceHolderSkipper {
            inner: &mut handler,
            in_placeholder: false,
        };
        extractor::write_general_text_parts(&self.package, parent, kind, label, &mut skipper)
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

/// `CommentAuthors`: the names and initials of the authors of comments, by id.
#[derive(Default)]
struct CommentAuthors {
    names: JavaHashMap<String>,
    initials: HashMap<String, String>,
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
        if let Some(name) = value("name") {
            self.names.insert(&id, name);
        }
        if let Some(initials) = value("initials") {
            self.initials.insert(id, initials);
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
/// with all the text of the part since the previous comment.
struct Comments<'a, 'x, 'h> {
    xhtml: &'a mut Xhtml<'x>,
    authors: &'h CommentAuthors,
    author: Option<String>,
    text: String,
}

impl<'a, 'x, 'h> Comments<'a, 'x, 'h> {
    fn new(xhtml: &'a mut Xhtml<'x>, authors: &'h CommentAuthors) -> Self {
        Self {
            xhtml,
            authors,
            author: None,
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
        if local_name == "cm" {
            self.author = xml::attribute(attributes, "", "authorId").map(str::to_owned);
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name != "cm" {
            return Ok(());
        }
        self.xhtml
            .start_element_with("p", &[("class", "slide-comment")])?;
        let id = self.author.take();
        let name = id.as_deref().and_then(|id| self.authors.names.get(id));
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
        self.text.push_str(text);
        Ok(())
    }
}

/// `PlaceHolderSkipper`: drops a shape from its placeholder mark to its end.
struct PlaceHolderSkipper<'h, H> {
    inner: &'h mut H,
    in_placeholder: bool,
}

impl<H: SaxHandler> SaxHandler for PlaceHolderSkipper<'_, H> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "ph" {
            self.in_placeholder = true;
        }
        if self.in_placeholder {
            return Ok(());
        }
        self.inner.start_element(namespace, local_name, attributes)
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if !self.in_placeholder {
            self.inner.end_element(namespace, local_name)?;
        }
        if local_name == "sp" {
            self.in_placeholder = false;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_placeholder {
            return Ok(());
        }
        self.inner.characters(text)
    }
}

/// `PageAnchoring.applyPageMetadata`: the slides a picture is on in order, and the slide when
/// there is one.
fn apply_page_numbers(metadata: &mut TikaMetadata, pages: &[i32]) {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        sorted.push(-1);
    }
    for page in &sorted {
        metadata.add(name::PAGE_NUMBERS, page.to_string());
    }
    if let [page] = sorted[..] {
        if page != -1 {
            metadata.set(name::PAGE_NUMBER, page.to_string());
        }
    }
}

#[cfg(test)]
mod tests;
