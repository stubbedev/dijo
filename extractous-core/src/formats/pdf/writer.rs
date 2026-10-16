//! Tika's `AbstractPDF2XHTML` and `PDF2XHTML`: each page a `div` of paragraphs the stripper's
//! calls open and close, the page's annotations after its text, then the bookmarks and the
//! form, and the metadata the writer collects on the way.

use std::collections::{BTreeSet, HashSet};

use lopdf::{Dictionary, Object};

use super::attachment::{self, EmbeddedFile, FileSpec, SpecKey, Written};
use super::cos::{as_dict, decode_java, Identity, Pdf};
use super::document::Page;
use super::engine::{Engine, PageContent};
use super::form;
use super::position::TextPosition;
use super::reading_order;
use super::stripper::{self, Callbacks, WriteError, WORD_SEPARATOR};
use super::tagged::{Tagged, MARKED_CONTENT_REJECTIONS};
use super::{jfloat, xfa, Unported};
use crate::formats::embedded::Wrapping;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;
use crate::java::is_blank;

/// How many annotation types and subtypes Tika lists, at most.
const MAX_ANNOTATION_TYPES: usize = 100;
/// `AbstractPDF2XHTML.MAX_BOOKMARK_ITEMS`.
const MAX_BOOKMARK_ITEMS: usize = 100_000;
/// `AbstractPDF2XHTML.MAX_BOOKMARK_DEPTH`.
const MAX_BOOKMARK_DEPTH: usize = 50;

/// Why writing the document stopped.
pub(super) enum WriteFailure {
    Stop(Stop),
    Unported(Unported),
    /// Parsing a document embedded in it failed in a way that fails the document.
    Embedded(Failure),
}

impl From<Failure> for WriteFailure {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Stopped(stop) => Self::Stop(stop),
            other => Self::Embedded(other),
        }
    }
}

impl From<Stop> for WriteFailure {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

impl From<Unported> for WriteFailure {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

impl From<WriteError> for WriteFailure {
    fn from(error: WriteError) -> Self {
        match error {
            WriteError::Stop(stop) => Self::Stop(stop),
            WriteError::Unported(unported) => Self::Unported(unported),
        }
    }
}

impl From<WriteFailure> for Failure {
    fn from(failure: WriteFailure) -> Self {
        match failure {
            WriteFailure::Stop(stop) => Failure::Stopped(stop),
            WriteFailure::Unported(unported) => Failure::Malformed(format!(
                "the document needs what the port does not do, though it was not deferred: {}",
                unported.0
            )),
            WriteFailure::Embedded(failure) => failure,
        }
    }
}

/// What a page's text layer was, for the decision whether OCR reads it instead.
#[derive(Debug, Clone)]
pub(super) struct PageLayer {
    pub(super) glyphs: i32,
    pub(super) unmapped: i32,
    /// The characters and whitespace the page's `div` holds, as Tika's page checker records them.
    pub(super) text: String,
}

/// What a page's `div` holds, held back until the page ends.
#[derive(Debug)]
enum Held {
    Start {
        name: String,
        attributes: Vec<(String, String)>,
    },
    End(String),
    Characters(String),
    Newline,
    Embedded(EmbeddedFile),
}

/// The stripper's callbacks as `PDF2XHTML` and, for a tagged document,
/// `PDFMarkedContent2XHTML` take them.
pub(super) struct PageWriter<'p, 'x, 'h> {
    pub(super) xhtml: &'x mut Xhtml<'h>,
    /// `PDFTextStripper.inParagraph`.
    in_paragraph: bool,
    pub(super) tagged: Option<Tagged<'p>>,
    /// The text of the current page's `div`, while it is recorded.
    recording: Option<String>,
    /// What the current page's `div` holds so far, when pages are written only once they end,
    /// as our `TextLayerCheckingPdfParser` passes them on.
    held: Option<Vec<Held>>,
}

impl PageWriter<'_, '_, '_> {
    /// Writes characters, recorded as Tika's page checker records them.
    pub(super) fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if let Some(recording) = self.recording.as_mut() {
            recording.extend(text.chars().map(Xhtml::safe_char));
        }
        match self.held.as_mut() {
            Some(held) => {
                held.push(Held::Characters(text.to_owned()));
                Ok(())
            }
            None => self.xhtml.characters(text),
        }
    }

    pub(super) fn newline(&mut self) -> Result<(), Stop> {
        if let Some(recording) = self.recording.as_mut() {
            recording.push('\n');
        }
        match self.held.as_mut() {
            Some(held) => {
                held.push(Held::Newline);
                Ok(())
            }
            None => self.xhtml.newline(),
        }
    }

    /// Starts an element, its indentation recorded.
    pub(super) fn start(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        if Xhtml::is_indented(name) {
            if let Some(recording) = self.recording.as_mut() {
                recording.push('\t');
            }
        }
        match self.held.as_mut() {
            Some(held) => {
                held.push(Held::Start {
                    name: name.to_owned(),
                    attributes: attributes
                        .iter()
                        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                        .collect(),
                });
                Ok(())
            }
            None => self.xhtml.start_element_with(name, attributes),
        }
    }

    /// Ends an element, the line break after a block recorded.
    pub(super) fn end(&mut self, name: &str) -> Result<(), Stop> {
        if Xhtml::is_endline(name) {
            if let Some(recording) = self.recording.as_mut() {
                recording.push('\n');
            }
        }
        match self.held.as_mut() {
            Some(held) => {
                held.push(Held::End(name.to_owned()));
                Ok(())
            }
            None => self.xhtml.end_element(name),
        }
    }

    /// Parses a file the page's annotations embed, in what the page's `div` holds when it is
    /// held back.
    fn embedded(&mut self, file: EmbeddedFile) -> Result<(), WriteFailure> {
        match self.held.as_mut() {
            Some(held) => {
                held.push(Held::Embedded(file));
                Ok(())
            }
            None => parse_embedded(self.xhtml, &file),
        }
    }

    /// Drops what the page's `div` holds, when it was held back: its text, which OCR text
    /// replaces. A held page holds no embedded file: a dry run defers the document then.
    pub(super) fn discard(&mut self) {
        if let Some(held) = self.held.as_mut() {
            held.clear();
        }
    }

    /// Writes what the page's `div` holds, when it was held back.
    pub(super) fn release(&mut self) -> Result<(), WriteFailure> {
        let Some(held) = self.held.as_mut() else {
            return Ok(());
        };
        for item in std::mem::take(held) {
            match item {
                Held::Start { name, attributes } => {
                    self.xhtml
                        .start_element_with(&name, &borrowed(&attributes))?;
                }
                Held::End(name) => self.xhtml.end_element(&name)?,
                Held::Characters(text) => self.xhtml.characters(&text)?,
                Held::Newline => self.xhtml.newline()?,
                Held::Embedded(file) => parse_embedded(self.xhtml, &file)?,
            }
        }
        Ok(())
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

impl Callbacks for PageWriter<'_, '_, '_> {
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

/// What passes each held page on once it ends: our `TextLayerCheckingPdfParser`.
pub(super) trait EndPage {
    /// Passes on the page `index`, which `page` holds back, and ends it.
    fn end_page(
        &mut self,
        index: usize,
        page: &mut PageWriter<'_, '_, '_>,
    ) -> Result<(), WriteFailure>;
}

/// The document being written.
pub(super) struct DocumentWriter<'p, 'x, 'h, 'e> {
    pdf: &'p Pdf,
    engine: Engine<'p>,
    pub(super) page: PageWriter<'p, 'x, 'h>,
    /// `getCurrentPageNo`.
    page_number: usize,
    /// Whether untagged pages are put into reading order.
    reading_order: bool,
    annotation_types: BTreeSet<String>,
    annotation_subtypes: BTreeSet<String>,
    total_glyphs: i64,
    total_unmapped: i64,
    /// Tika's `extractedFiles`: the file specifications annotations led to.
    extracted: HashSet<SpecKey>,
    /// Whether the document is written only to decide whether it is deferred, its embedded
    /// files not parsed and the text layer of each page recorded.
    dry_run: bool,
    /// The text layer of each page, when recorded.
    pub(super) layers: Option<Vec<PageLayer>>,
    /// What passes each held page on, when it is not passed on as it is.
    page_ends: Option<&'e mut dyn EndPage>,
}

impl<'p, 'x, 'h, 'e> DocumentWriter<'p, 'x, 'h, 'e> {
    pub(super) fn new(
        pdf: &'p Pdf,
        xhtml: &'x mut Xhtml<'h>,
        tagged: Option<Tagged<'p>>,
        reading_order: bool,
        hold_pages: bool,
        dry_run: bool,
        page_ends: Option<&'e mut dyn EndPage>,
    ) -> Self {
        let labels = tagged.as_ref().is_some_and(Tagged::has_index);
        if let Some(reason) = tagged.as_ref().and_then(|tagged| tagged.document_rejection) {
            xhtml
                .metadata_mut()
                .add(MARKED_CONTENT_REJECTIONS, format!("document:{reason}"));
        }
        Self {
            pdf,
            engine: Engine::new(pdf, labels),
            page: PageWriter {
                xhtml,
                in_paragraph: false,
                tagged,
                recording: None,
                held: hold_pages.then(Vec::new),
            },
            page_number: 0,
            reading_order,
            annotation_types: BTreeSet::new(),
            annotation_subtypes: BTreeSet::new(),
            total_glyphs: 0,
            total_unmapped: 0,
            extracted: HashSet::new(),
            dry_run,
            layers: dry_run.then(Vec::new),
            page_ends,
        }
    }

    /// `PDFTextStripper.writeText` as `AbstractPDF2XHTML` runs it: every page, then the end
    /// of the document.
    pub(super) fn write(&mut self) -> Result<(), WriteFailure> {
        for page in self.pdf.pages()? {
            self.process_page(page)?;
        }
        self.end_document()
    }

    /// `PDF2XHTML.processPage`, which records that the length limit was reached on the page
    /// as `handleCatchableIOE` does.
    fn process_page(&mut self, page: Page<'p>) -> Result<(), WriteFailure> {
        let processed = self.process_page_content(page);
        if let Err(WriteFailure::Stop(Stop::LimitReached)) = processed {
            self.page
                .xhtml
                .metadata_mut()
                .set("tk:exception:write-limit-reached", "true");
        }
        processed
    }

    /// `PDFTextStripper.processPage`, as `PDF2XHTML` and Tika's tagged writer run it.
    fn process_page_content(&mut self, page: Page<'p>) -> Result<(), WriteFailure> {
        self.page_number += 1;
        self.start_page()?;
        let mut content = self.engine.process_page(page)?;
        if self.page.tagged.is_none() && self.reading_order {
            for article in content.articles.iter_mut() {
                reading_order::arrange(article);
            }
        }
        match self.page.tagged.is_some() {
            true => Tagged::write_page(&mut self.page, self.page_number, &mut content)?,
            false => stripper::write_page(&mut content.articles, &mut self.page)?,
        }
        self.end_page(page, &content)
    }

    /// `AbstractPDF2XHTML.startPage`.
    fn start_page(&mut self) -> Result<(), WriteFailure> {
        self.page
            .xhtml
            .start_element_with("div", &[("class", "page")])?;
        if self.layers.is_some() {
            self.page.recording = Some(String::new());
        }
        match self.page.tagged.as_mut() {
            Some(tagged) => tagged.start_page(),
            None => self.page.plain_paragraph_start()?,
        }
        Ok(())
    }

    /// `PDF2XHTML.endPage` and `AbstractPDF2XHTML.endPage`.
    fn end_page(&mut self, page: Page<'p>, content: &PageContent) -> Result<(), WriteFailure> {
        self.page.write_paragraph_end()?;
        let metadata = self.page.xhtml.metadata_mut();
        metadata.add("pdf:chars-per-page", content.glyphs.to_string());
        metadata.add(
            "pdf:unmapped-unicode-chars-per-page",
            content.unmapped.to_string(),
        );
        self.total_glyphs += i64::from(content.glyphs);
        self.total_unmapped += i64::from(content.unmapped);
        self.annotations(page)?;
        if let Some(text) = self.page.recording.take() {
            if let Some(layers) = self.layers.as_mut() {
                layers.push(PageLayer {
                    glyphs: content.glyphs,
                    unmapped: content.unmapped,
                    text,
                });
            }
        }
        match self.page_ends.as_mut() {
            Some(page_ends) => page_ends.end_page(self.page_number - 1, &mut self.page)?,
            None => self.page.release()?,
        }
        self.page.xhtml.end_element("div")?;
        Ok(())
    }

    /// The annotations of a page, as `processPageAnnotation` writes and records them.
    fn annotations(&mut self, page: Page<'p>) -> Result<(), WriteFailure> {
        let pdf = self.pdf;
        let Some(annotations) = pdf.array(page.dict, b"Annots") else {
            return Ok(());
        };
        for item in annotations {
            let Some(object) = pdf.resolve(item) else {
                continue;
            };
            let Some(annotation) = as_dict(object) else {
                return Err(Unported(
                    "an annotation that is not a dictionary, which PDFBox fails on".to_owned(),
                )
                .into());
            };
            self.annotation(annotation)?;
        }
        Ok(())
    }

    fn annotation(&mut self, annotation: &'p Dictionary) -> Result<(), WriteFailure> {
        let pdf = self.pdf;
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
                let spec = attachment::file_spec(pdf.get(annotation, b"FS"))?;
                self.annotation_file(source, spec)?;
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
                for spec in attachment::find_file_specs(pdf, annotation) {
                    self.annotation_file(source, Some(FileSpec::Complex(spec)))?;
                }
            }
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
            let metadata = self.page.xhtml.metadata_mut();
            metadata.set("pdf:marked-content-pages-tagged", pages_tagged.to_string());
            metadata.set(
                "pdf:marked-content-pages-fallback",
                pages_fallback.to_string(),
            );
        }
        self.bookmarks()?;
        self.embedded_documents()?;
        self.acro_form()?;
        let types: Vec<String> = std::mem::take(&mut self.annotation_types)
            .into_iter()
            .collect();
        let subtypes: Vec<String> = std::mem::take(&mut self.annotation_subtypes)
            .into_iter()
            .collect();
        let metadata = self.page.xhtml.metadata_mut();
        for annotation_type in types {
            metadata.add("pdf:annotation-types", annotation_type);
        }
        for subtype in subtypes {
            metadata.add("pdf:annotation-subtypes", subtype);
        }
        self.page.xhtml.end_document()?;
        let total_glyphs = self.total_glyphs as i32;
        let total_unmapped = self.total_unmapped as i32;
        let metadata = self.page.xhtml.metadata_mut();
        metadata.set(
            "pdf:total-unmapped-unicode-chars",
            total_unmapped.to_string(),
        );
        if total_glyphs > 0 {
            let share = total_unmapped as f32 / total_glyphs as f32;
            metadata.set(
                "pdf:overall-percentage-unmapped-unicode-chars",
                jfloat::double_to_string(f64::from(share)),
            );
        }
        metadata.set(
            "pdf:contains-damaged-font",
            self.engine.damaged_font.to_string(),
        );
        metadata.set(
            "pdf:contains-non-embedded-font",
            self.engine.non_embedded_font.to_string(),
        );
        metadata.set("pdf:num-3d-annotations", "0");
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
        let xhtml = &mut self.page.xhtml;
        let mut seen: HashSet<Identity> = HashSet::new();
        let mut parents: Vec<(Identity, &'p Dictionary)> = Vec::new();
        let mut opened: Vec<bool> = Vec::new();
        xhtml.start_element("ul")?;
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
                    xhtml.end_element("ul")?;
                    lists -= 1;
                }
                next = item(parent.1, b"Next");
                continue;
            };
            current = entry;
            seen.insert(current.0);
            items += 1;
            xhtml.start_element("li")?;
            if let Some(title) = pdf.text(current.1, b"Title") {
                xhtml.characters(&title)?;
            }
            xhtml.end_element("li")?;
            match item(current.1, b"First").filter(|(id, _)| !seen.contains(id)) {
                Some(child) => {
                    let open = lists < MAX_BOOKMARK_DEPTH;
                    if open {
                        xhtml.start_element("ul")?;
                        lists += 1;
                    }
                    parents.push(current);
                    opened.push(open);
                    next = Some(child);
                }
                None => next = item(current.1, b"Next"),
            }
        }
        xhtml.end_element("ul")?;
        if truncated {
            self.page.xhtml.metadata_mut().add(
                "tk:exception:warn",
                format!("bookmark outline truncated at {MAX_BOOKMARK_ITEMS} items"),
            );
        }
        Ok(())
    }

    /// `processDocOnAction` for the file specification of an annotation: its files, written
    /// in the page's `div` unless an annotation led to the specification before.
    fn annotation_file(
        &mut self,
        source: &str,
        spec: Option<FileSpec<'p>>,
    ) -> Result<(), WriteFailure> {
        let Some(spec) = spec else {
            return Ok(());
        };
        let key = spec.key();
        if !self.extracted.contains(&key) {
            let attributes = vec![("source".to_owned(), source.to_owned())];
            for written in attachment::written(self.pdf, "", source, spec, attributes)? {
                match written {
                    Written::Link(attributes) => {
                        self.page.start("div", &borrowed(&attributes))?;
                        self.page.end("div")?;
                    }
                    Written::Embedded { attributes, file } => {
                        self.page.start("div", &borrowed(&attributes))?;
                        self.page.end("div")?;
                        if !self.dry_run {
                            self.page.embedded(file)?;
                        } else if self.page.held.is_some() {
                            return Err(Unported(
                                "a file an annotation embeds, whose text our \
                                 TextLayerCheckingPdfParser weighs with the page's"
                                    .to_owned(),
                            )
                            .into());
                        }
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
        for (name, spec) in attachment::document_specs(pdf)? {
            let spec = FileSpec::Complex(spec);
            if self.extracted.contains(&spec.key()) {
                continue;
            }
            for written in attachment::written(pdf, &name, "", spec, Vec::new())? {
                let xhtml = &mut *self.page.xhtml;
                match written {
                    Written::Link(attributes) => {
                        xhtml.start_element_with("div", &borrowed(&attributes))?;
                        xhtml.end_element("div")?;
                    }
                    Written::Embedded { attributes, file } => {
                        xhtml.start_element_with("div", &borrowed(&attributes))?;
                        xhtml.end_element("div")?;
                        if !self.dry_run {
                            parse_embedded(xhtml, &file)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// `extractAcroForm`: an XFA form as `XFAExtractor` reads it, otherwise the form's fields,
    /// as nested ordered lists.
    fn acro_form(&mut self) -> Result<(), WriteFailure> {
        let pdf = self.pdf;
        let xfa = pdf
            .catalog()
            .and_then(|catalog| pdf.dict(catalog, b"AcroForm"))
            .and_then(|form| pdf.get(form, b"XFA"));
        if let Some(xfa) = xfa {
            return self.xfa(&xfa::extract(&xfa::bytes(pdf, xfa)?)?);
        }
        let Some(lines) = form::lines(pdf, self.page.xhtml.metadata_mut())? else {
            return Ok(());
        };
        let xhtml = &mut self.page.xhtml;
        xhtml.start_element_with("div", &[("class", "acroform")])?;
        xhtml.start_element("ol")?;
        for line in &lines {
            match line {
                form::Line::Field { alternate, text } => {
                    xhtml.start_element_with("li", &alternate_name(alternate))?;
                    xhtml.characters(text)?;
                    xhtml.end_element("li")?;
                }
                form::Line::Signature { alternate, data } => {
                    xhtml.start_element_with("li", &alternate_name(alternate))?;
                    xhtml.start_element_with("ol", &[("type", "signaturedata")])?;
                    for (key, value) in data {
                        xhtml.start_element_with("li", &[("signdata", key)])?;
                        xhtml.characters(value)?;
                        xhtml.end_element("li")?;
                    }
                    xhtml.end_element("ol")?;
                    xhtml.end_element("li")?;
                }
                form::Line::StartList => xhtml.start_element("ol")?,
                form::Line::EndList => xhtml.end_element("ol")?,
            }
        }
        xhtml.end_element("ol")?;
        xhtml.end_element("div")?;
        Ok(())
    }

    /// `XFAExtractor.extract`'s output: the form's paragraphs, then its fields.
    fn xfa(&mut self, form: &xfa::Form) -> Result<(), WriteFailure> {
        let xhtml = &mut self.page.xhtml;
        xhtml.start_element_with("div", &[("class", "xfa_content")])?;
        for paragraph in &form.paragraphs {
            xhtml.element("p", paragraph)?;
        }
        if !form.fields.is_empty() {
            xhtml.start_element_with("div", &[("class", "xfa_form")])?;
            xhtml.start_element("ol")?;
            for (name, text) in &form.fields {
                xhtml.start_element_with("li", &[("fieldName", name)])?;
                xhtml.characters(text)?;
                xhtml.end_element("li")?;
            }
            xhtml.end_element("ol")?;
            xhtml.end_element("div")?;
        }
        xhtml.end_element("div")?;
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

/// `parseEmbedded` of a file the document embeds, which Tika writes without a `package-entry`
/// div of its own.
fn parse_embedded(xhtml: &mut Xhtml, file: &EmbeddedFile) -> Result<(), WriteFailure> {
    xhtml
        .parse_embedded(&file.data, &file.metadata, Wrapping::Bare)
        .map_err(WriteFailure::from)
}
