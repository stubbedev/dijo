//! Tika's `AbstractPDF2XHTML` and `PDF2XHTML`: each page a `div` of paragraphs the stripper's
//! calls open and close, the page's annotations after its text, then the bookmarks and the
//! form, and the metadata the writer collects on the way, recorded as they are written.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Display;
use std::ops::ControlFlow;

use lopdf::{Dictionary, Object};

use super::attachment::{self, FileSpec, SpecKey, Written};
use super::cos::{as_dict, decode_java, Identity, Pdf};
use super::document::Page;
use super::engine::{differ_past, Engine, PageContent};
use super::form;
use super::images::Images;
use super::ocr::{Layer, PageCheck};
use super::position::TextPosition;
use super::reading_order;
use super::recording::Recording;
use super::stripper::{self, Callbacks, WORD_SEPARATOR};
use super::tagged::{Tagged, MARKED_CONTENT_REJECTIONS};
use super::xfa::{self, Extracted};
use super::{jfloat, ImageExtraction, Settings, Unported};
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::thrown::Thrown;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Difference;
use crate::java::is_blank;

/// How many annotation types and subtypes Tika lists, at most.
const MAX_ANNOTATION_TYPES: usize = 100;
/// `AbstractPDF2XHTML.MAX_BOOKMARK_ITEMS`.
const MAX_BOOKMARK_ITEMS: usize = 100_000;
/// `RenderSettings.defaults()`: the resolution Tika renders pages at, the fewest pixels a side
/// takes, and the most pixels a page takes.
const RENDER_DPI: f64 = 300.0;
const MIN_RENDER_PIXELS: f64 = 2.0;
const MAX_RENDER_PIXELS: f64 = 100_000_000.0;
/// `AbstractPDF2XHTML.MAX_BOOKMARK_DEPTH`.
const MAX_BOOKMARK_DEPTH: usize = 50;

/// Why recording the document stopped.
pub(super) enum WriteFailure {
    /// The recording outgrew the extraction's memory budget.
    Stop(Stop),
    /// Tika's `PDFParser` throws this, which fails the document.
    Throws(Thrown),
}

impl From<Stop> for WriteFailure {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

/// Why writing a part of a page stopped: the recording stopped, or the part is one the port
/// does not read, which the writer goes past ([`gone_past`]).
pub(super) enum PartFailure {
    Stop(Stop),
    Unported(Unported),
}

impl From<Stop> for PartFailure {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

impl From<Unported> for PartFailure {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

/// What writing the part `what` of a document left: where the port does not read it, it goes on
/// past it, `pdf` noting the difference ([`differ_past`]).
pub(super) fn gone_past(
    pdf: &Pdf,
    what: impl Display,
    written: Result<(), PartFailure>,
) -> Result<(), Stop> {
    match written {
        Ok(()) => Ok(()),
        Err(PartFailure::Unported(unported)) => {
            differ_past(pdf, what, unported);
            Ok(())
        }
        Err(PartFailure::Stop(stop)) => Err(stop),
    }
}

/// The stripper's callbacks as `PDF2XHTML` and, for a tagged document,
/// `PDFMarkedContent2XHTML` take them.
pub(super) struct PageWriter<'p, 'r> {
    pdf: &'p Pdf<'p>,
    pub(super) out: &'r mut Recording,
    /// `PDFTextStripper.inParagraph`.
    in_paragraph: bool,
    pub(super) tagged: Option<Tagged<'p>>,
    /// The text of the current page's `div`, while it is kept for the page's assessment.
    layer_text: Option<String>,
    /// Whether the current page's `div` holds files an annotation embeds, or images.
    pub(super) embeds: bool,
}

impl PageWriter<'_, '_> {
    /// Writes characters, kept as Tika's page checker keeps them.
    pub(super) fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if let Some(layer_text) = self.layer_text.as_mut() {
            layer_text.extend(text.chars().map(Xhtml::safe_char));
        }
        self.out.characters(text)
    }

    pub(super) fn newline(&mut self) -> Result<(), Stop> {
        if let Some(layer_text) = self.layer_text.as_mut() {
            layer_text.push('\n');
        }
        self.out.newline()
    }

    /// Starts an element, its indentation kept.
    pub(super) fn start(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        if Xhtml::is_indented(name) {
            if let Some(layer_text) = self.layer_text.as_mut() {
                layer_text.push('\t');
            }
        }
        self.out.start_element_with(name, attributes)
    }

    /// Ends an element, the line break after a block kept.
    pub(super) fn end(&mut self, name: &str) -> Result<(), Stop> {
        if Xhtml::is_endline(name) {
            if let Some(layer_text) = self.layer_text.as_mut() {
                layer_text.push('\n');
            }
        }
        self.out.end_element(name)
    }

    /// `PDF2XHTML.writeParagraphStart`: the stripper's, then a `p`.
    pub(super) fn plain_paragraph_start(&mut self) -> Result<(), Stop> {
        if self.in_paragraph {
            self.write_paragraph_end()?;
            self.in_paragraph = false;
        }
        self.in_paragraph = true;
        self.start("p", &[])
    }

    /// `PDF2XHTML.writeParagraphEnd`: the stripper's, then the `p` ended.
    pub(super) fn plain_paragraph_end(&mut self) -> Result<(), Stop> {
        if !self.in_paragraph {
            self.write_paragraph_start()?;
        }
        self.in_paragraph = false;
        self.end("p")
    }

    pub(super) fn plain_string(&mut self, text: &str) -> Result<(), Stop> {
        self.characters(text)
    }

    pub(super) fn plain_word_separator(&mut self) -> Result<(), Stop> {
        self.characters(WORD_SEPARATOR)
    }

    pub(super) fn plain_line_separator(&mut self) -> Result<(), Stop> {
        self.newline()
    }
}

impl Callbacks for PageWriter<'_, '_> {
    fn differ(&mut self, difference: Difference) {
        self.pdf.differ(difference);
    }

    fn write_page_start(&mut self, articles: &mut [Vec<TextPosition>]) {
        if let Some(tagged) = &self.tagged {
            if tagged.reading_order {
                for article in articles.iter_mut() {
                    reading_order::order_lines(article);
                }
            }
        }
    }

    fn write_paragraph_start(&mut self) -> Result<(), Stop> {
        match self.tagged.as_mut() {
            None => self.plain_paragraph_start(),
            Some(_) => Tagged::write_paragraph_start(self),
        }
    }

    fn write_paragraph_end(&mut self) -> Result<(), Stop> {
        match self.tagged.as_mut() {
            None => self.plain_paragraph_end(),
            Some(_) => Tagged::write_paragraph_end(self),
        }
    }

    fn write_string(&mut self, text: &str, positions: &[&TextPosition]) -> Result<(), Stop> {
        match self.tagged.as_mut() {
            None => self.plain_string(text),
            Some(_) => Tagged::write_string(self, text, positions),
        }
    }

    fn write_word_separator(&mut self) -> Result<(), Stop> {
        match self.tagged.as_mut() {
            None => self.plain_word_separator(),
            Some(_) => Tagged::write_word_separator(self),
        }
    }

    fn write_line_separator(&mut self) -> Result<(), Stop> {
        match self.tagged.as_mut() {
            None => self.plain_line_separator(),
            Some(_) => Tagged::write_line_separator(self),
        }
    }
}

/// The document being written: its pages each read in a scope of its own, and the rest of it as
/// a whole, through `pdf`.
pub(super) struct DocumentWriter<'p, 'r> {
    pdf: &'p Pdf<'p>,
    engine: Engine,
    pub(super) page: PageWriter<'p, 'r>,
    /// `getCurrentPageNo`.
    page_number: usize,
    settings: Settings,
    annotation_types: BTreeSet<String>,
    annotation_subtypes: BTreeSet<String>,
    total_glyphs: i64,
    total_unmapped: i64,
    /// Tika's `extractedFiles`: the file specifications annotations led to.
    extracted: HashSet<SpecKey>,
    /// The images of the pages, when they are extracted.
    images: Option<Images>,
}

impl<'p, 'r> DocumentWriter<'p, 'r> {
    pub(super) fn new(
        pdf: &'p Pdf,
        out: &'r mut Recording,
        tagged: Option<Tagged<'p>>,
        settings: Settings,
    ) -> Self {
        let labels = tagged.as_ref().is_some_and(Tagged::has_index);
        if let Some(reason) = tagged.as_ref().and_then(|tagged| tagged.document_rejection) {
            out.add(MARKED_CONTENT_REJECTIONS, format!("document:{reason}"));
        }
        Self {
            pdf,
            engine: Engine::new(labels),
            page: PageWriter {
                pdf,
                out,
                in_paragraph: false,
                tagged,
                layer_text: None,
                embeds: false,
            },
            page_number: 0,
            settings,
            annotation_types: BTreeSet::new(),
            annotation_subtypes: BTreeSet::new(),
            total_glyphs: 0,
            total_unmapped: 0,
            extracted: HashSet::new(),
            images: match settings.images {
                ImageExtraction::Off => None,
                ImageExtraction::On { unique_only } => Some(Images::new(unique_only)),
            },
        }
    }

    /// `PDFTextStripper.writeText` as `AbstractPDF2XHTML` runs it: every page, then the end
    /// of the document.
    /// Writes the pages and the end of the document, handing the recording to `after_page`
    /// after each page, which may stop the writing there. Each page is read in a scope of its
    /// own, let go of once it is written.
    pub(super) fn write(
        &mut self,
        after_page: &mut dyn FnMut(&mut Recording) -> ControlFlow<()>,
    ) -> Result<ControlFlow<()>, WriteFailure> {
        let pdf = self.pdf;
        for page in pdf.pages() {
            self.page.out.enter_page();
            let scope = pdf.scoped();
            self.process_page(&scope, scope.page(page))?;
            drop(scope);
            self.end_scope();
            self.page.out.leave_page();
            if after_page(self.page.out).is_break() {
                return Ok(ControlFlow::Break(()));
            }
        }
        self.end_document()?;
        Ok(ControlFlow::Continue(()))
    }

    /// `PDFTextStripper.processPage`, as `PDF2XHTML` and Tika's tagged writer run it, or
    /// `OCR2XHTML.processPage`, which reads the page's OCR text in place of its text.
    fn process_page(&mut self, pdf: &Pdf, page: Page) -> Result<(), WriteFailure> {
        self.page_number += 1;
        if self.settings.ocr.reads_only_ocr() {
            self.page
                .out
                .start_element_with("div", &[("class", "page")])?;
            if self.renders(pdf, page) {
                self.page
                    .out
                    .release_read(self.page_number - 1, PageCheck::Recognize);
            }
            return self.close_page(pdf, page, 0, 0);
        }
        self.start_page()?;
        let mut content = self.engine.process_page(pdf, page);
        if self.page.tagged.is_none() && self.settings.reading_order {
            for article in content.articles.iter_mut() {
                reading_order::arrange(article);
            }
        }
        match self.page.tagged.is_some() {
            true => Tagged::write_page(&mut self.page, self.page_number, &mut content)?,
            false => stripper::write_page(&mut content.articles, &mut self.page)?,
        }
        self.end_page(pdf, page, &content)
    }

    /// Ends the scope a page was read in: what is kept by where a direct object of it was held
    /// is let go of, as a later page's objects may be held there.
    fn end_scope(&mut self) {
        self.extracted.retain(|key| !key.is_direct());
        if let Some(images) = self.images.as_mut() {
            images.end_scope();
        }
    }

    /// `AbstractPDF2XHTML.startPage`, the page's content held back from there when Tika's
    /// page checker holds it.
    fn start_page(&mut self) -> Result<(), WriteFailure> {
        self.page
            .out
            .start_element_with("div", &[("class", "page")])?;
        if self.settings.ocr.holds_pages() {
            self.page.out.hold();
            self.page.layer_text = Some(String::new());
        }
        self.page.embeds = false;
        match self.page.tagged.as_mut() {
            Some(tagged) => tagged.start_page(),
            None => self.page.plain_paragraph_start()?,
        }
        Ok(())
    }

    /// `PDF2XHTML.endPage`.
    fn end_page(
        &mut self,
        pdf: &Pdf,
        page: Page,
        content: &PageContent,
    ) -> Result<(), WriteFailure> {
        self.page.write_paragraph_end()?;
        if let Some(images) = self.images.as_mut() {
            images.extract(pdf, page, self.page_number, &mut self.page)?;
        }
        self.close_page(pdf, page, content.glyphs, content.unmapped)
    }

    /// `AbstractPDF2XHTML.endPage` of a page of `glyphs` glyphs, `unmapped` of them without
    /// characters: its annotations, then the page kept for OCR when our
    /// `TextLayerCheckingPdfParser` reads it, or its OCR text when Tika's PDF parser reads it.
    fn close_page(
        &mut self,
        pdf: &Pdf,
        page: Page,
        glyphs: i32,
        unmapped: i32,
    ) -> Result<(), WriteFailure> {
        let out = &mut *self.page.out;
        out.add("pdf:chars-per-page", glyphs.to_string());
        out.add("pdf:unmapped-unicode-chars-per-page", unmapped.to_string());
        self.total_glyphs += i64::from(glyphs);
        self.total_unmapped += i64::from(unmapped);
        self.annotations(pdf, page)?;
        let layer_text = self.page.layer_text.take();
        let layer = match (layer_text.as_deref(), self.page.embeds) {
            (None, _) => Layer::Unheld,
            (Some(_), true) => Layer::Embedding,
            (Some(text), false) => Layer::Text(text),
        };
        let check = match self.settings.ocr.page(glyphs, unmapped, layer) {
            PageCheck::Recognize if !self.renders(pdf, page) => PageCheck::Keep,
            check => check,
        };
        match check {
            PageCheck::Keep => self.page.out.release(),
            PageCheck::Unrecognized => {
                pdf.differ(Difference(format!(
                    "page {} shows {glyphs} glyphs, {unmapped} of them without characters, \
                     which Tika's PDF parser wants OCR for, read without OCR as no tesseract \
                     runs",
                    self.page_number
                )));
                self.page.out.count_unrecognized();
                self.page.out.release();
            }
            check => self.page.out.release_read(self.page_number - 1, check),
        }
        self.page.out.end_element("div")?;
        Ok(())
    }

    /// Whether Tika's renderer renders `page` for the OCR parser: not a page too small to hold
    /// anything (`RenderSettings.belowMinimum`). A page above its limit of pixels, whose failure
    /// Tika records, is read without OCR, the difference noted.
    fn renders(&self, pdf: &Pdf, page: Page) -> bool {
        let [left, bottom, right, top] = page.crop_box(pdf);
        let (width, height) = (f64::from(right - left), f64::from(top - bottom));
        let (width, height) = match page.rotation(pdf) {
            90 | 270 => (height, width),
            _ => (width, height),
        };
        let pixels = |points: f64| (points / 72.0 * RENDER_DPI).ceil();
        if pixels(width) < MIN_RENDER_PIXELS || pixels(height) < MIN_RENDER_PIXELS {
            return false;
        }
        if pixels(width) * pixels(height) > MAX_RENDER_PIXELS {
            pdf.differ(Difference(format!(
                "page {} renders to more pixels than Tika's renderer allows, whose failure Tika \
                 records, read without OCR",
                self.page_number
            )));
            return false;
        }
        true
    }

    /// The annotations of a page, as `processPageAnnotation` writes and records them.
    fn annotations(&mut self, pdf: &Pdf, page: Page) -> Result<(), WriteFailure> {
        let Some((listed, Object::Array(annotations))) = pdf.get_identified(page.dict, b"Annots")
        else {
            return Ok(());
        };
        let holder = match listed {
            Identity::Direct(_) => page.identity,
            listed => listed,
        };
        for item in annotations {
            let Some((identity, object)) = pdf.resolve_identified(item) else {
                continue;
            };
            let Some(annotation) = as_dict(object) else {
                pdf.differ(Difference::new(
                    "the port leaves out an annotation that is not a dictionary, which PDFBox \
                     fails on",
                ));
                continue;
            };
            let identity = pdf.held_within(holder, identity, annotation);
            let annotated = self.annotation(pdf, identity, annotation);
            gone_past(pdf, "an annotation", annotated)?;
        }
        Ok(())
    }

    fn annotation(
        &mut self,
        pdf: &Pdf,
        identity: Identity,
        annotation: &Dictionary,
    ) -> Result<(), PartFailure> {
        let name = pdf.text(annotation, b"NM");
        if self.annotation_types.len() < MAX_ANNOTATION_TYPES {
            self.annotation_types
                .insert(name.unwrap_or_else(|| "null".to_owned()));
        }
        let subtype = pdf.name_as_string(annotation, b"Subtype");
        if self.annotation_subtypes.len() < MAX_ANNOTATION_TYPES {
            self.annotation_subtypes
                .insert(subtype.clone().unwrap_or_else(|| "null".to_owned()));
        }
        match subtype.as_deref() {
            Some("FileAttachment") => {
                let source = "annotationFileAttachment";
                let spec = attachment::file_spec(pdf.get_identified(annotation, b"FS").map(
                    |(spec, object)| match as_dict(object) {
                        Some(dict) => (pdf.held_within(identity, spec, dict), object),
                        None => (spec, object),
                    },
                ))?;
                self.annotation_file(pdf, source, spec)?;
            }
            Some("Widget") => {}
            _ => {
                if subtype.as_deref() == Some("3D") || annotation.has(b"3DD") {
                    return Err(Unported(
                        "a 3D annotation, whose scripts Tika parses as embedded documents"
                            .to_owned(),
                    )
                    .into());
                }
                let source = subtype.as_deref().unwrap_or("unknown");
                for (identity, dict) in attachment::find_file_specs(pdf, identity, annotation) {
                    let spec = FileSpec::Complex { identity, dict };
                    self.annotation_file(pdf, source, Some(spec))?;
                }
            }
        }
        if !self.settings.annotation_text {
            return Ok(());
        }
        if matches!(subtype.as_deref(), Some("Link" | "Widget")) {
            if let Some(uri) = action_uri(pdf, annotation) {
                if !is_blank(&uri) {
                    let page = &mut self.page;
                    page.start("div", &[("class", "annotation")])?;
                    page.start("a", &[("href", &uri)])?;
                    page.characters(&uri)?;
                    page.end("a")?;
                    page.end("div")?;
                }
            }
        }
        if is_markup(subtype.as_deref()) {
            let title = pdf.text(annotation, b"T");
            let subject = pdf.text(annotation, b"Subj");
            let contents = pdf.text(annotation, b"Contents");
            if title.is_some() || subject.is_some() || contents.is_some() {
                let page = &mut self.page;
                page.start("div", &[("class", "annotation")])?;
                for (class, value) in [
                    ("annotationTitle", title),
                    ("annotationSubject", subject),
                    ("annotationContents", contents),
                ] {
                    if let Some(value) = value {
                        page.start("div", &[("class", class)])?;
                        page.characters(&value)?;
                        page.end("div")?;
                    }
                }
                page.end("div")?;
            }
        }
        Ok(())
    }

    /// `AbstractPDF2XHTML.endDocument`: the bookmarks, the form, and the document's totals.
    fn end_document(&mut self) -> Result<(), WriteFailure> {
        if let Some(tagged) = &self.page.tagged {
            let (pages_tagged, pages_fallback) = (tagged.pages_tagged, tagged.pages_fallback);
            let out = &mut *self.page.out;
            out.set("pdf:marked-content-pages-tagged", pages_tagged.to_string());
            out.set(
                "pdf:marked-content-pages-fallback",
                pages_fallback.to_string(),
            );
        }
        if self.settings.bookmarks {
            self.bookmarks()?;
        }
        self.embedded_documents()?;
        self.acro_form()?;
        let out = &mut *self.page.out;
        for annotation_type in std::mem::take(&mut self.annotation_types) {
            out.add("pdf:annotation-types", annotation_type);
        }
        for subtype in std::mem::take(&mut self.annotation_subtypes) {
            out.add("pdf:annotation-subtypes", subtype);
        }
        out.end_document()?;
        let total_glyphs = self.total_glyphs as i32;
        let total_unmapped = self.total_unmapped as i32;
        out.set(
            "pdf:total-unmapped-unicode-chars",
            total_unmapped.to_string(),
        );
        if total_glyphs > 0 {
            let share = total_unmapped as f32 / total_glyphs as f32;
            out.set(
                "pdf:overall-percentage-unmapped-unicode-chars",
                jfloat::double_to_string(f64::from(share)),
            );
        }
        out.set(
            "pdf:contains-damaged-font",
            self.engine.damaged_font.to_string(),
        );
        out.set(
            "pdf:contains-non-embedded-font",
            self.engine.non_embedded_font.to_string(),
        );
        out.set("pdf:num-3d-annotations", "0");
        Ok(())
    }

    /// `extractBookmarkText`: the outline as nested lists, walked without recursion.
    fn bookmarks(&mut self) -> Result<(), WriteFailure> {
        let pdf = self.pdf;
        let Some(outline) = pdf
            .catalog()
            .and_then(|catalog| pdf.dict(catalog, b"Outlines"))
        else {
            return Ok(());
        };
        let item = |dict: &'p Dictionary, key: &[u8]| -> Option<(Identity, &'p Dictionary)> {
            let (identity, object) = pdf.get_identified(dict, key)?;
            as_dict(object).map(|dict| (identity, dict))
        };
        let Some(mut current) = item(outline, b"First") else {
            return Ok(());
        };
        let out = &mut *self.page.out;
        let mut seen: HashSet<Identity> = HashSet::new();
        let mut parents: Vec<(Identity, &'p Dictionary)> = Vec::new();
        let mut opened: Vec<bool> = Vec::new();
        out.start_element("ul")?;
        let mut lists = 1;
        let mut items = 0;
        let mut truncated = false;
        let mut next: Option<(Identity, &'p Dictionary)> = Some(current);
        loop {
            if items >= MAX_BOOKMARK_ITEMS && next.is_some_and(|(id, _)| !seen.contains(&id)) {
                truncated = true;
                next = None;
            }
            let Some(entry) = next.filter(|(id, _)| !seen.contains(id)) else {
                let Some(parent) = parents.pop() else {
                    break;
                };
                if opened.pop() == Some(true) {
                    out.end_element("ul")?;
                    lists -= 1;
                }
                next = item(parent.1, b"Next");
                continue;
            };
            current = entry;
            seen.insert(current.0);
            items += 1;
            out.start_element("li")?;
            if let Some(title) = pdf.text(current.1, b"Title") {
                out.characters(&title)?;
            }
            out.end_element("li")?;
            match item(current.1, b"First").filter(|(id, _)| !seen.contains(id)) {
                Some(child) => {
                    let open = lists < MAX_BOOKMARK_DEPTH;
                    if open {
                        out.start_element("ul")?;
                        lists += 1;
                    }
                    parents.push(current);
                    opened.push(open);
                    next = Some(child);
                }
                None => next = item(current.1, b"Next"),
            }
        }
        out.end_element("ul")?;
        if truncated {
            out.add(
                EXCEPTION_WARNING,
                format!("bookmark outline truncated at {MAX_BOOKMARK_ITEMS} items"),
            );
        }
        Ok(())
    }

    /// `processDocOnAction` for the file specification of an annotation: its files, written
    /// in the page's `div` unless an annotation led to the specification before. Our
    /// `TextLayerCheckingPdfParser` weighs the text of such a file with the page's, once it is
    /// parsed ([`PageCheck::Settle`]).
    fn annotation_file(
        &mut self,
        pdf: &Pdf,
        source: &str,
        spec: Option<FileSpec>,
    ) -> Result<(), PartFailure> {
        let Some(spec) = spec else {
            return Ok(());
        };
        let key = spec.key();
        if !self.extracted.contains(&key) {
            let attributes = vec![("source".to_owned(), source.to_owned())];
            for written in attachment::written(pdf, "", source, spec, attributes)? {
                match written {
                    Written::Link(attributes) => {
                        self.page.start("div", &borrowed(&attributes))?;
                        self.page.end("div")?;
                    }
                    Written::Embedded { attributes, file } => {
                        self.page.start("div", &borrowed(&attributes))?;
                        self.page.end("div")?;
                        self.page.embeds = true;
                        self.page.out.embedded(file)?;
                    }
                }
            }
        }
        self.extracted.insert(key);
        Ok(())
    }

    /// `extractEmbeddedDocuments`: the files of the document's file specifications that no
    /// annotation led to, each parsed as an embedded document.
    fn embedded_documents(&mut self) -> Result<(), WriteFailure> {
        let pdf = self.pdf;
        let specs = attachment::document_specs(pdf);
        for (name, spec) in specs.specs {
            if self.extracted.contains(&spec.key()) {
                continue;
            }
            let written = match attachment::written(pdf, &name, "", spec, Vec::new()) {
                Ok(written) => written,
                Err(unported) => {
                    differ_past(pdf, "an embedded file", unported);
                    continue;
                }
            };
            for written in written {
                let out = &mut *self.page.out;
                match written {
                    Written::Link(attributes) => {
                        out.start_element_with("div", &borrowed(&attributes))?;
                        out.end_element("div")?;
                    }
                    Written::Embedded { attributes, file } => {
                        out.start_element_with("div", &borrowed(&attributes))?;
                        out.end_element("div")?;
                        out.embedded(file)?;
                    }
                }
            }
        }
        if let Some(thrown) = specs.thrown {
            self.page.out.catch_io(thrown);
        }
        Ok(())
    }

    /// `extractAcroForm`: an XFA form as `XFAExtractor` reads it, otherwise the form's fields,
    /// as nested ordered lists; both when the XFA form's XML fails, its failure recorded.
    fn acro_form(&mut self) -> Result<(), WriteFailure> {
        let pdf = self.pdf;
        let xfa = pdf
            .catalog()
            .and_then(|catalog| pdf.dict(catalog, b"AcroForm"))
            .and_then(|form| pdf.get(form, b"XFA"));
        let bytes = xfa.map(|xfa| xfa::bytes(pdf, xfa)).transpose();
        let bytes = bytes.unwrap_or_else(|unported| {
            differ_past(pdf, "the XFA form", unported);
            None
        });
        if let Some(bytes) = bytes {
            match xfa::extract(&bytes) {
                Extracted::Read(form) => return self.xfa(&form),
                Extracted::Failed {
                    paragraphs,
                    failure,
                } => {
                    let form = xfa::Form {
                        paragraphs,
                        fields: Vec::new(),
                    };
                    self.xfa(&form)?;
                    self.page.out.add(EXCEPTION_WARNING, failure);
                }
            }
        }
        let out = &mut *self.page.out;
        let lines = form::lines(pdf, out).unwrap_or_else(|unported| {
            differ_past(pdf, "the form's fields", unported);
            None
        });
        let Some(lines) = lines else {
            return Ok(());
        };
        out.start_element_with("div", &[("class", "acroform")])?;
        out.start_element("ol")?;
        for line in &lines {
            match line {
                form::Line::Field { alternate, text } => {
                    out.start_element_with("li", &alternate_name(alternate))?;
                    out.characters(text)?;
                    out.end_element("li")?;
                }
                form::Line::Signature { alternate, data } => {
                    out.start_element_with("li", &alternate_name(alternate))?;
                    out.start_element_with("ol", &[("type", "signaturedata")])?;
                    for (key, value) in data {
                        out.start_element_with("li", &[("signdata", key)])?;
                        out.characters(value)?;
                        out.end_element("li")?;
                    }
                    out.end_element("ol")?;
                    out.end_element("li")?;
                }
                form::Line::StartList => out.start_element("ol")?,
                form::Line::EndList => out.end_element("ol")?,
            }
        }
        out.end_element("ol")?;
        out.end_element("div")?;
        Ok(())
    }

    /// `XFAExtractor.extract`'s output: the form's paragraphs, then its fields.
    fn xfa(&mut self, form: &xfa::Form) -> Result<(), WriteFailure> {
        let out = &mut *self.page.out;
        out.start_element_with("div", &[("class", "xfa_content")])?;
        for paragraph in &form.paragraphs {
            out.element("p", paragraph)?;
        }
        if !form.fields.is_empty() {
            out.start_element_with("div", &[("class", "xfa_form")])?;
            out.start_element("ol")?;
            for (name, text) in &form.fields {
                out.start_element_with("li", &[("fieldName", name)])?;
                out.characters(text)?;
                out.end_element("li")?;
            }
            out.end_element("ol")?;
            out.end_element("div")?;
        }
        out.end_element("div")?;
        Ok(())
    }
}

/// Whether the annotation subtype is one PDFBox reads as `PDAnnotationMarkup`.
fn is_markup(subtype: Option<&str>) -> bool {
    matches!(
        subtype,
        Some(
            "FileAttachment"
                | "Line"
                | "Stamp"
                | "Square"
                | "Circle"
                | "Polygon"
                | "PolyLine"
                | "Ink"
                | "Text"
                | "Highlight"
                | "Underline"
                | "StrikeOut"
                | "Squiggly"
                | "FreeText"
                | "Caret"
                | "Sound"
        )
    )
}

/// The URI of a link or widget's URI action, as `PDActionURI.getURI` reads it.
fn action_uri(pdf: &Pdf, annotation: &Dictionary) -> Option<String> {
    let action = pdf.dict(annotation, b"A")?;
    if pdf.name_as_string(action, b"S").as_deref() != Some("URI") {
        return None;
    }
    match pdf.get(action, b"URI") {
        Some(Object::String(bytes, _)) => Some(match bytes.as_slice() {
            [0xFE, 0xFF, ..] | [0xFF, 0xFE, ..] => super::cos::text_string(bytes),
            _ => decode_java("UTF-8", bytes),
        }),
        _ => None,
    }
}

/// The attributes of a form field's item: its alternate name, if it has one.
fn alternate_name(alternate: &Option<String>) -> Vec<(&str, &str)> {
    alternate
        .iter()
        .map(|name| ("altName", name.as_str()))
        .collect()
}

/// Attributes as the XHTML writer takes them.
fn borrowed(attributes: &[(String, String)]) -> Vec<(&str, &str)> {
    attributes
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}
