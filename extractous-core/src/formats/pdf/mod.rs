//! PDF documents, as Tika's `PDFParser` reads them with PDFBox, under extractous's
//! `TextLayerCheckingPdfParser` when tesseract runs: the document's objects through `lopdf`,
//! then the metadata `PDFParser` collects, and the text of each page as PDFBox's
//! `PDFTextStripper` assembles it from the glyphs its content stream shows, written by
//! `PDF2XHTML` in our reading order, or for a tagged document by `PDFMarkedContent2XHTML`
//! through its structure tree, each page's images after its text when they are extracted
//! ([`images`]), then the bookmarks, the files the document embeds, parsed as
//! embedded documents, and the form. The pages `TextLayerCheckingPdfParser` reads by OCR are
//! rendered and read with tesseract ([`ocr`]). Documents with a page Tika would OCR with no
//! tesseract to run, and documents with what the port does not do (encryption the empty
//! password does not open, XFA forms and fonts it cannot measure, among others), are left to
//! Tika.

mod afm;
mod attachment;
mod cff;
mod cmap;
mod content;
mod cos;
mod document;
mod encoding;
mod engine;
mod font;
mod form;
mod images;
pub(crate) mod jfloat;
mod matrix;
mod metadata;
mod ocr;
mod position;
mod quality;
mod reading_order;
mod recording;
mod stripper;
mod structure;
mod tagged;
pub(crate) mod truetype;
mod type1;
mod writer;
mod xfa;
mod xref;

use std::ops::ControlFlow;

use lopdf::Document;

use crate::config::{MarkedContentStrategy, ParserConfigs};
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Declined, Failure, Parser, Prepared};

use cos::Pdf;
use ocr::{OcrCheck, PageOcr};
use recording::Recording;
use structure::StructureIndex;
use tagged::Tagged;
use writer::{DocumentWriter, WriteFailure};

const MEDIA_TYPE: &str = "application/pdf";
const TEXT_LAYER_CHECKING_PARSER: &str = "extractous::TextLayerCheckingPdfParser";
const PDF_PARSER: &str = "extractous::PDFParser";
#[cfg(test)]
const TIKA_PDF_PARSER: &str = "org.apache.tika.parser.pdf.PDFParser";
#[cfg(test)]
const TIKA_TEXT_LAYER_CHECKING_PARSER: &str = "ai.yobix.TextLayerCheckingPdfParser";
/// The parser our `TextLayerCheckingPdfParser` records delegating to, under both names.
#[cfg(test)]
const DELEGATES: [(&str, &str); 1] = [(PDF_PARSER, TIKA_PDF_PARSER)];
/// `AbstractPDF2XHTML.MAX_RECURSION_DEPTH`.
const MAX_RECURSION_DEPTH: usize = 100;

pub(super) use crate::formats::Unported;
use crate::ocr::{tesseract_runs as ocr_runs, OcrSettings};

/// Tika's `PDFParser`, wrapped in our `TextLayerCheckingPdfParser` when tesseract runs.
pub(crate) struct PdfParser;

impl Parser for PdfParser {
    fn name(&self) -> &'static str {
        if ocr_runs() {
            TEXT_LAYER_CHECKING_PARSER
        } else {
            PDF_PARSER
        }
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        if ocr_runs() {
            TIKA_TEXT_LAYER_CHECKING_PARSER
        } else {
            TIKA_PDF_PARSER
        }
    }

    #[cfg(test)]
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &DELEGATES
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(input, metadata, configs, RECORDING_LIMIT)
    }
}

/// [`Parser::prepare`], streaming a document whose recording grows past `limit` bytes.
fn prepare<'a>(
    input: &'a [u8],
    metadata: &TikaMetadata,
    configs: &'a ParserConfigs,
    limit: usize,
) -> Result<Prepared<'a>, Declined> {
    match record(input, metadata, configs, limit)? {
        Recorded::Whole(mut recording, page_ocr) => Ok(Box::new(move |xhtml| {
            let written = match page_ocr {
                None => recording.flush(xhtml, None),
                Some(page_ocr) => write_read(&page_ocr, &mut recording, xhtml, configs),
            };
            ended(xhtml, written)
        })),
        Recorded::Large(loaded, settings) => {
            let metadata = metadata.clone();
            Ok(Box::new(move |xhtml| {
                let written = stream(&loaded, input, settings, metadata, configs, xhtml);
                ended(xhtml, written)
            }))
        }
    }
}

/// The document `written` to `xhtml`, with the count of pages Tika's `PDFParser` read by OCR,
/// none, our `TextLayerCheckingPdfParser` reading them.
fn ended(xhtml: &mut Xhtml, written: Result<(), Failure>) -> Result<(), Failure> {
    xhtml.metadata_mut().set("pdf:ocr-page-count", "0");
    written
}

/// The PDF settings the port reads, as Tika's `PDFParser` and our `TextLayerCheckingPdfParser`
/// take them.
#[derive(Debug, Clone, Copy)]
struct Settings {
    ocr: OcrCheck,
    /// Whether untagged pages are put into reading order.
    reading_order: bool,
    marked_content: MarkedContentStrategy,
    bookmarks: bool,
    annotation_text: bool,
    images: ImageExtraction,
}

/// Whether the images of the pages are extracted, as `setExtractInlineImages` asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageExtraction {
    Off,
    On {
        /// `setExtractUniqueInlineImagesOnly`.
        unique_only: bool,
        /// Whether our `TextLayerCheckingPdfParser` weighs each page's text with what OCR
        /// reads in its images.
        assessed_with_ocr: bool,
    },
}

impl Settings {
    /// The settings `configs` set, or why the port leaves documents to Tika under them.
    fn of(configs: &ParserConfigs) -> Result<Self, Unported> {
        let config = &configs.pdf;
        let ocr = OcrCheck::of(config, ocr_runs())?;
        let images = match config.extracts_inline_images() {
            false => ImageExtraction::Off,
            true => ImageExtraction::On {
                unique_only: config.extracts_unique_inline_images_only(),
                assessed_with_ocr: ocr.holds_pages() && !configs.ocr.skips_ocr(),
            },
        };
        Ok(Self {
            ocr,
            reading_order: config.reads_in_reading_order(),
            marked_content: config.marked_content(),
            bookmarks: config.extracts_bookmarks_text(),
            annotation_text: config.extracts_annotation_text(),
            images,
        })
    }
}

/// How many bytes a document's recording may hold before the document is streamed instead.
const RECORDING_LIMIT: usize = 16 << 20;

/// What recording a document gave.
enum Recorded {
    /// The whole extraction, and the OCR of the pages our `TextLayerCheckingPdfParser` reads.
    Whole(Box<Recording>, Option<PageOcr>),
    /// The document, loaded, whose recording grew past its limit before a page to read by OCR:
    /// it is streamed.
    Large(Box<Loaded>, Settings),
}

/// A recording of a document whose parser starts from `metadata`.
fn new_recording(metadata: TikaMetadata) -> Recording {
    let mut recording = Recording::new(metadata);
    if ocr_runs() {
        recording.delegate_to(PDF_PARSER);
    }
    recording
}

/// The document's extraction, recorded from `metadata`, and the OCR of the pages our
/// `TextLayerCheckingPdfParser` reads, unless the document is left to Tika: for a setting the
/// port does not read, something in the document it does not do, or a page Tika would OCR whose
/// OCR the port does not run. The document is read once, into the recording, which holds no
/// more of it than its output and the files it embeds. A recording that grows past `limit`
/// bytes while no page is read by OCR ends there, what it saw decided as a whole
/// recording's is, and the document is streamed.
fn record(
    input: &[u8],
    metadata: &TikaMetadata,
    configs: &ParserConfigs,
    limit: usize,
) -> Result<Recorded, Declined> {
    let deferred = |Unported(reason)| Declined::Deferred(reason);
    let settings = Settings::of(configs).map_err(deferred)?;
    let loaded = load(input).map_err(deferred)?;
    let mut recording = new_recording(metadata.clone());
    let mut large = |recording: &mut Recording| {
        if recording.size() > limit && !recording.reads_pages() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let recorded =
        extract(&loaded, input, settings, &mut recording, &mut large).map_err(|failure| {
            match failure {
                WriteFailure::Unported(Unported(reason)) => Declined::Deferred(reason),
                WriteFailure::Stop(Stop::MemoryLimit(limit)) => {
                    Declined::Fails(Error::MemoryLimit(limit))
                }
                WriteFailure::Stop(stop) => {
                    unreachable!("a recording stops only at the memory budget, not at {stop:?}")
                }
            }
        });
    if let Err(failure @ Declined::Fails(_)) = recorded {
        return Err(failure);
    }
    if let Some(reason) = unread_objects(&loaded, input).map_err(deferred)? {
        return Err(Declined::Deferred(reason));
    }
    if recorded?.is_break() {
        return Ok(Recorded::Large(Box::new(loaded), settings));
    }
    let page_ocr = PageOcr::of(
        recording.pages_read(),
        loaded.pdf.pages().map_err(deferred)?.len(),
        settings.ocr,
        input,
        configs.ocr.skips_ocr(),
        OcrSettings::of(&configs.ocr),
    )
    .map_err(deferred)?;
    if page_ocr.is_some() && recording.embeds_files() {
        return Err(Declined::Deferred(EMBEDDED_AND_READ.to_owned()));
    }
    Ok(Recorded::Whole(Box::new(recording), page_ocr))
}

/// Why a document that embeds files and has pages read by OCR is left to Tika.
const EMBEDDED_AND_READ: &str = "files the document embeds, which Tika's path parses while our \
     TextLayerCheckingPdfParser holds back the pages it reads by OCR";

/// An object the extraction needed that lopdf lost or the cross-reference table does not list,
/// which PDFBox recovers or finds, if any.
fn unread_objects(loaded: &Loaded, input: &[u8]) -> Result<Option<String>, Unported> {
    if let Some(number) = loaded.pdf.lost_object_needed() {
        return Ok(Some(format!(
            "object {number}, which lopdf cannot read and PDFBox may recover"
        )));
    }
    let unlisted = loaded.pdf.unlisted_objects_needed();
    Ok(xref::first_found(input, &unlisted)?.map(|(number, _)| {
        format!(
            "object {number}, which the cross-reference table does not list and PDFBox finds \
             by searching the document"
        )
    }))
}

/// Writes `recording` to `xhtml`, with the pages `page_ocr` reads read side by side, the script
/// the first of them learned kept for the extraction.
fn write_read(
    page_ocr: &PageOcr,
    recording: &mut Recording,
    xhtml: &mut Xhtml,
    configs: &ParserConfigs,
) -> Result<(), Failure> {
    let context = xhtml.context();
    let auto = OcrSettings::of(&configs.ocr).auto();
    let assumed = auto.then(|| context.assumed_ocr_script()).flatten();
    let (written, learned) =
        page_ocr.write_document(assumed, |page_ends| recording.flush(xhtml, Some(page_ends)));
    if let (Ok(()), Some(learned)) = (&written, learned) {
        context.learn_ocr_script(learned);
    }
    written
}

/// Extracts a document too large to record into `xhtml` page by page, as it is read from its
/// start once more. What would leave the document to Tika is decided for its first pages,
/// which the recording held; met on a later page, it fails the extraction, the pages before it
/// written.
fn stream(
    loaded: &Loaded,
    input: &[u8],
    settings: Settings,
    metadata: TikaMetadata,
    configs: &ParserConfigs,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let page_count = loaded.pdf.pages().map_err(late)?.len();
    let mut pages = StreamedPages {
        input,
        page_count,
        check: settings.ocr,
        configs,
        embeds: false,
        reads: false,
    };
    let mut recording = new_recording(metadata);
    let mut flushed = Ok(());
    let written = extract(loaded, input, settings, &mut recording, &mut |recording| {
        flushed = pages.flush(recording, xhtml);
        match flushed {
            Ok(()) => ControlFlow::Continue(()),
            Err(_) => ControlFlow::Break(()),
        }
    });
    flushed?;
    match written {
        Ok(_) => {}
        Err(WriteFailure::Stop(stop)) => return Err(Failure::Stopped(stop)),
        Err(WriteFailure::Unported(unported)) => return Err(late(unported)),
    }
    if let Some(reason) = unread_objects(loaded, input).map_err(late)? {
        return Err(late(Unported(reason)));
    }
    pages.flush(&mut recording, xhtml)
}

/// The failure of a streamed document on what leaves a document to Tika.
fn late(Unported(reason): Unported) -> Failure {
    Failure::Extraction(Error::ParseError(format!(
        "The PDF parser met {reason} after it had written the document's first pages, where \
         Tika's path would have taken the whole document"
    )))
}

/// What a streamed document's pages need to be written: what reads pages by OCR, and whether
/// the document embedded files or had pages read so far.
struct StreamedPages<'a> {
    input: &'a [u8],
    page_count: usize,
    check: OcrCheck,
    configs: &'a ParserConfigs,
    embeds: bool,
    reads: bool,
}

impl StreamedPages<'_> {
    /// Writes what `recording` holds to `xhtml`, a page OCR reads read first.
    fn flush(&mut self, recording: &mut Recording, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let pages = recording.pages_read();
        self.embeds |= recording.embeds_files();
        self.reads |= !pages.is_empty();
        if self.embeds && self.reads {
            return Err(late(Unported(EMBEDDED_AND_READ.to_owned())));
        }
        let page_ocr = PageOcr::of(
            pages,
            self.page_count,
            self.check,
            self.input,
            self.configs.ocr.skips_ocr(),
            OcrSettings::of(&self.configs.ocr),
        )
        .map_err(late)?;
        match page_ocr {
            None => recording.flush(xhtml, None),
            Some(page_ocr) => write_read(&page_ocr, recording, xhtml, self.configs),
        }
    }
}

/// A document's objects, as `lopdf` reads them, and what the empty password opens it with.
struct Loaded {
    pdf: Pdf,
    access: Access,
}

/// The document's objects where PDFBox finds them, decrypted with the empty password as Tika's
/// `PDFParser` opens an encrypted document without one. A document the empty password does
/// not open is left to Tika, which asks for a password.
fn load(input: &[u8]) -> Result<Loaded, Unported> {
    let resolution = xref::resolve(input)?;
    let mut doc = match Document::load_mem(input) {
        Ok(doc) if !resolution.needs_rewrite(&doc, input)? => doc,
        _ => Document::load_mem(&resolution.rewritten(input)?)
            .map_err(|error| Unported(format!("lopdf cannot read the document: {error}")))?,
    };
    resolution.set_trailer(&mut doc);
    if doc.trailer.has(b"Encrypt") {
        return Err(Unported(
            "encryption the empty password does not open, which Tika asks a password for"
                .to_owned(),
        ));
    }
    let access = match doc.encryption_state.clone() {
        None => Access::Unencrypted,
        Some(state) => Access::of(&mut doc, &state)?,
    };
    Ok(Loaded {
        pdf: Pdf::new(doc, input, resolution.listed()),
        access,
    })
}

/// What a document lets the one who opened it do, as PDFBox's `AccessPermission` holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Unencrypted,
    /// Opened with the owner password: everything.
    Owner,
    /// Opened with the user password: what the permission bits `P` grant.
    User(u64),
}

impl Access {
    /// `StandardSecurityHandler.prepareForDecryption` with the empty password, which opens the
    /// document as its owner when it is the owner password too, else as a user.
    fn of(doc: &mut Document, state: &lopdf::EncryptionState) -> Result<Self, Unported> {
        let dictionary = state.encode().map_err(|error| {
            Unported(format!(
                "an encryption dictionary lopdf cannot restate: {error}"
            ))
        })?;
        let id = (
            doc.objects
                .keys()
                .next_back()
                .map_or(1, |&(number, _)| number + 1),
            0,
        );
        doc.objects
            .insert(id, lopdf::Object::Dictionary(dictionary));
        doc.trailer.set("Encrypt", lopdf::Object::Reference(id));
        let owner = doc.authenticate_raw_owner_password(b"").is_ok();
        doc.trailer.remove(b"Encrypt");
        doc.objects.remove(&id);
        Ok(if owner {
            Self::Owner
        } else {
            Self::User(state.permissions().bits())
        })
    }

    /// `AccessPermission.isPermissionBitOn`.
    fn grants(self, bit: u32) -> bool {
        match self {
            Self::Unencrypted | Self::Owner => true,
            Self::User(permissions) => permissions & (1 << (bit - 1)) != 0,
        }
    }
}

/// `PDFParser.parse` from the cross-reference scan on: the metadata, then the pages and the
/// end of the document, written by the tagged writer for a document with marked content, into
/// `out`.
fn extract(
    loaded: &Loaded,
    input: &[u8],
    settings: Settings,
    out: &mut Recording,
    after_page: &mut dyn FnMut(&mut Recording) -> ControlFlow<()>,
) -> Result<ControlFlow<()>, WriteFailure> {
    let pdf = &loaded.pdf;
    let mut startxrefs = 0;
    for offset in metadata::scan_startxrefs(input) {
        if offset.startxref == 0 {
            continue;
        }
        startxrefs += 1;
        out.add("pdf:eof-offsets", offset.end_eof_offset.to_string());
    }
    let updates = (startxrefs - 1).max(0).to_string();
    out.set("pdf:incremental-update-count", updates.clone());
    out.set("tk:version-count", updates);
    let catalog = pdf.catalog().ok_or_else(|| {
        Unported("a document without a catalog, which PDFBox fails on".to_owned())
    })?;
    out.set("pdf:has-collection", catalog.has(b"Collection").to_string());
    let acro_form = pdf.dict(catalog, b"AcroForm");
    let has_xfa = acro_form.is_some_and(|form| form.has(b"XFA"));
    out.set("pdf:has-xfa", has_xfa.to_string());
    let has_marked_content = StructureIndex::<()>::has_marked_content(pdf);
    out.set("pdf:has-marked-content", has_marked_content.to_string());
    out.set("Content-Type", MEDIA_TYPE);
    for (name, bit) in [
        ("access-permission:extract-for-accessibility", 10),
        ("access-permission:extract-content", 5),
        ("access-permission:assemble-document", 11),
        ("access-permission:fill-in-form", 9),
        ("access-permission:can-modify", 4),
        ("access-permission:modify-annotations", 6),
        ("access-permission:can-print", 3),
        ("access-permission:can-print-faithful", 12),
    ] {
        out.set(name, loaded.access.grants(bit).to_string());
    }
    let encrypted = loaded.access != Access::Unencrypted;
    out.set("pdf:encrypted", encrypted.to_string());
    let has_fields = form::has_fields(pdf)?;
    let version = metadata::header_version(input)?;
    metadata::document(pdf, out, version, has_fields)?;
    form::signatures(pdf, out)?;
    if is_illustrator(pdf)? {
        out.set("Content-Type", "application/illustrator");
    }
    let tagged = match settings.marked_content {
        _ if !has_marked_content => None,
        MarkedContentStrategy::None => None,
        MarkedContentStrategy::Auto => Some(Tagged::new(pdf, settings.reading_order, true)),
        MarkedContentStrategy::Tags => Some(Tagged::new(pdf, settings.reading_order, false)),
    };
    DocumentWriter::new(pdf, out, tagged, settings).write(after_page)
}

/// `PDFParser.checkIllustrator`: whether the first page holds Illustrator's private data.
fn is_illustrator(pdf: &Pdf) -> Result<bool, Unported> {
    let pages = pdf.pages()?;
    let Some(page) = pages.first() else {
        return Ok(false);
    };
    Ok(pdf
        .dict(page.dict, b"PieceInfo")
        .and_then(|piece_info| pdf.dict(piece_info, b"Illustrator"))
        .and_then(|illustrator| pdf.dict(illustrator, b"Private"))
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig};
    use crate::formats::parity::{
        assert_same_as_tika_delegating, assert_same_as_tika_delegating_with, conformance_corpus,
        documents, Scratch,
    };
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    pub(super) const CORPORA: [&str; 2] = ["../quality/corpus", "../test_files/documents"];

    /// Why OCR reads pages of a document of the corpora, or the port leaves it to Tika.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Why {
        /// Tika OCRs a page of next to no glyphs, whatever its text layer reads.
        Ocr,
        /// Tika OCRs a page of too many glyphs without characters: by their count, or for its
        /// doubtful text layer when tesseract runs.
        Unmapped,
        /// Our `TextLayerCheckingPdfParser` doubts a page's text layer, when tesseract runs.
        Doubtful,
        /// The port does not do what the document needs, as the reason starts.
        Unported(&'static str),
    }

    impl Why {
        /// Whether the port leaves a document for this reason to Tika: one whose pages Tika's
        /// PDF parser wants OCR for when no tesseract runs, and one it does not do.
        fn defers(self) -> bool {
            match self {
                Self::Ocr | Self::Unmapped => !ocr_runs(),
                Self::Doubtful => false,
                Self::Unported(_) => true,
            }
        }

        /// Whether the port reads pages of a document for this reason by OCR.
        fn reads_by_ocr(self) -> bool {
            ocr_runs() && matches!(self, Self::Ocr | Self::Unmapped | Self::Doubtful)
        }
    }

    /// The documents of the corpora whose pages OCR reads, or which the port leaves to Tika,
    /// and why.
    const NOT_THE_TEXT_LAYER: [(&str, Why); 9] = [
        ("arabic.pdf", Why::Unmapped),
        ("deu-ocr.pdf", Why::Ocr),
        ("eng-ocr.pdf", Why::Ocr),
        ("hindi.pdf", Why::Unmapped),
        ("prose-broken-offset.pdf", Why::Doubtful),
        ("prose-broken-private-use.pdf", Why::Doubtful),
        ("prose-scan.pdf", Why::Ocr),
        ("thai.pdf", Why::Unmapped),
        (
            "user-password.pdf",
            Why::Unported("encryption the empty password does not open"),
        ),
    ];

    pub(super) fn tika_name() -> &'static str {
        PdfParser.tika_name()
    }

    pub(super) fn as_paths(paths: &[PathBuf]) -> Vec<&Path> {
        paths.iter().map(PathBuf::as_path).collect()
    }

    fn corpora() -> Vec<&'static Path> {
        CORPORA.iter().map(Path::new).collect()
    }

    /// The documents of the corpora whose pages OCR reads, when tesseract runs.
    fn read_by_ocr() -> Vec<PathBuf> {
        NOT_THE_TEXT_LAYER
            .iter()
            .filter(|(_, why)| why.reads_by_ocr())
            .filter_map(|(name, _)| {
                CORPORA
                    .iter()
                    .map(|corpus| Path::new(corpus).join(name))
                    .find(|path| path.is_file())
            })
            .collect()
    }

    fn deferral_of(path: &Path) -> String {
        let input = std::fs::read(path).unwrap();
        PdfParser
            .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
            .unwrap_or_else(|| panic!("{} is not deferred", path.display()))
    }

    /// A document of the given objects, numbered from 1 with the catalog first, and a
    /// cross-reference table whose entries `shift` moves off their objects.
    pub(super) fn document(objects: &[&str], prefix: &[u8], shift: &[(usize, usize)]) -> Vec<u8> {
        let mut out = prefix.to_vec();
        let start = out.len();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len() - start);
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len() - start;
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for (index, offset) in offsets.iter().enumerate() {
            let moved = shift
                .iter()
                .find(|(object, _)| *object == index + 1)
                .map_or(0, |(_, by)| *by);
            out.extend_from_slice(format!("{:010} 00000 n \n", offset + moved).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A document of the given objects, numbered from 1 with the catalog first, and a
    /// cross-reference stream listing them, and each object of `dangling` at offset 0.
    fn stream_document(objects: &[&str], dangling: &[u32]) -> Vec<u8> {
        let mut out = b"%PDF-1.5\n".to_vec();
        let mut rows: Vec<(u32, u32)> = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            rows.push((index as u32 + 1, out.len() as u32));
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        rows.extend(dangling.iter().map(|&number| (number, 0)));
        let stream_number = rows.iter().map(|&(number, _)| number).max().unwrap_or(0) + 1;
        let start = out.len() as u32;
        rows.push((stream_number, start));
        rows.sort_unstable();
        let mut data = Vec::new();
        for (_, offset) in &rows {
            data.push(1);
            data.extend_from_slice(&offset.to_be_bytes());
            data.extend_from_slice(&0u16.to_be_bytes());
        }
        let index: Vec<String> = rows
            .iter()
            .map(|(number, _)| format!("{number} 1"))
            .collect();
        out.extend_from_slice(
            format!(
                "{stream_number} 0 obj\n<< /Type /XRef /Size {} /Root 1 0 R /W [1 4 2] \
                 /Index [{}] /Length {} >>\nstream\n",
                stream_number + 1,
                index.join(" "),
                data.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&data);
        out.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes(),
        );
        out
    }

    /// Where `needle` first stands in `haystack`.
    fn find(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap_or_else(|| panic!("{} is missing", String::from_utf8_lossy(needle)))
    }

    /// `haystack` with the first `needle` replaced by `replacement`.
    fn replace(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
        let at = find(haystack, needle);
        [&haystack[..at], replacement, &haystack[at + needle.len()..]].concat()
    }

    fn deferral(path: &Path) -> Option<String> {
        let input = std::fs::read(path).unwrap();
        PdfParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
    }

    /// A page of Helvetica text drawn by `content`.
    pub(super) fn page_objects(content: &str) -> Vec<String> {
        vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len() + 1
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_owned(),
        ]
    }

    /// The XHTML the native parser extracts of a document, with its metadata.
    fn native_xml(input: &[u8]) -> (String, crate::Metadata) {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(input)
            .unwrap()
    }

    const TEXT: &str =
        "BT /F1 12 Tf 72 720 Td (Det var en gang en konge, som havde tre d\\370tre.) Tj \
                        0 -14 Td (Den yngste var den smukkeste af dem alle sammen.) Tj ET";

    /// A document of one page scanned at 300 dpi, the image in grey and no text on it.
    fn scanned(image: &[u8]) -> Vec<u8> {
        use std::io::Write;
        const POINTS_PER_PIXEL: f64 = 72.0 / 300.0;
        let gray = image::load_from_memory(image).unwrap().to_luma8();
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(gray.as_raw()).unwrap();
        let mut hex: String = encoder
            .finish()
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        hex.push('>');
        let width = f64::from(gray.width()) * POINTS_PER_PIXEL;
        let height = f64::from(gray.height()) * POINTS_PER_PIXEL;
        let content = format!("q {width:.2} 0 0 {height:.2} 0 0 cm /Scan Do Q");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width:.2} {height:.2}] \
                 /Contents 4 0 R /Resources << /XObject << /Scan 5 0 R >> >> >>"
            ),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len() + 1
            ),
            format!(
                "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray \
                 /BitsPerComponent 8 /Filter [/ASCIIHexDecode /FlateDecode] /Length {} >>\n\
                 stream\n{hex}\nendstream",
                gray.width(),
                gray.height(),
                hex.len() + 1
            ),
        ];
        document(&objects.each_ref().map(String::as_str), b"", &[])
    }

    #[test]
    fn reads_right_to_left_scanned_pages_by_ocr_as_tikas_path_does() {
        assert!(ocr_runs(), "tesseract is not installed");
        let scratch = Scratch::new("pdf-right-to-left");
        let image = std::fs::read("../test_files/documents/ara-ocr.png").unwrap();
        let scan = scratch.write("ara-ocr.pdf", &scanned(&image));
        let input = std::fs::read(&scan).unwrap();
        for language in [TesseractOcrConfig::AUTO_LANGUAGE, "ara"] {
            let configs = ParserConfigs {
                ocr: TesseractOcrConfig::new().set_language(language),
                ..ParserConfigs::default()
            };
            let recorded = record(&input, &TikaMetadata::new(), &configs, RECORDING_LIMIT);
            assert!(
                matches!(recorded, Ok(Recorded::Whole(_, Some(_)))),
                "the scan is not read by OCR with {language}"
            );
            assert_same_as_tika_delegating_with(&[&scan], tika_name(), &DELEGATES, &configs);
        }
    }

    /// The PDF parser with a recording limit of nothing: it streams every document whose first
    /// page OCR does not read.
    struct Streaming;

    impl Parser for Streaming {
        fn name(&self) -> &'static str {
            PdfParser.name()
        }

        fn tika_name(&self) -> &'static str {
            PdfParser.tika_name()
        }

        fn media_types(&self) -> &'static [&'static str] {
            PdfParser.media_types()
        }

        fn prepare<'a>(
            &self,
            input: &'a [u8],
            metadata: &TikaMetadata,
            configs: &'a ParserConfigs,
        ) -> Result<Prepared<'a>, Declined> {
            prepare(input, metadata, configs, 0)
        }
    }

    #[test]
    fn streams_a_document_page_by_page_as_it_records_it() {
        let configs = ParserConfigs {
            ocr: TesseractOcrConfig::new().set_skip_ocr(true),
            ..ParserConfigs::default()
        };
        let roots = CORPORA.map(Path::new);
        let (paths, _) = documents(&PdfParser, &roots);
        let mut streamed = 0;
        for path in &paths {
            let path = path.to_str().unwrap();
            let routed = crate::formats::route_file(path).unwrap();
            let input = &routed.input;
            let recorded = record(input, &TikaMetadata::new(), &configs, 0);
            streamed += usize::from(matches!(recorded, Ok(Recorded::Large(..))));
            for output in [crate::formats::Output::Text, crate::formats::Output::Xml] {
                for limit in [200, usize::MAX >> 1] {
                    let extract = |parser: &dyn Parser| {
                        let extracted = crate::formats::parse_to_string(
                            parser,
                            input,
                            &routed.detected,
                            &configs,
                            limit,
                            output,
                        );
                        let sorted = extracted.map(|extracted| {
                            extracted.map(|(text, metadata)| {
                                (text, metadata.into_iter().collect::<BTreeMap<_, _>>())
                            })
                        });
                        format!("{sorted:?}")
                    };
                    assert_eq!(
                        extract(&Streaming),
                        extract(&PdfParser),
                        "{path} as {output:?} up to {limit}"
                    );
                }
            }
        }
        assert!(streamed > 20, "{streamed} documents streamed");
    }

    /// Standard 14 fonts without an `/Encoding` take their AFM file's, whose unencoded glyphs
    /// have the code -1.
    #[test]
    fn extracts_standard_fonts_in_their_own_encoding_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-builtin-encoding");
        let paths: Vec<PathBuf> = ["Helvetica", "Times-Bold", "Courier", "Symbol"]
            .iter()
            .map(|font| {
                let mut objects = page_objects(TEXT);
                objects[4] = format!("<< /Type /Font /Subtype /Type1 /BaseFont /{font} >>");
                let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
                scratch.write(&format!("{font}.pdf"), &document(&objects, b"", &[]))
            })
            .collect();
        for path in &paths {
            assert_eq!(deferral(path), None, "{}", path.display());
        }
        assert_same_as_tika_delegating(&as_paths(&paths), tika_name(), &DELEGATES);
    }

    #[test]
    fn extracts_documents_lopdf_recovers_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-recovered");
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let plain = scratch.write("plain.pdf", &document(&objects, b"", &[]));
        let marked = scratch.write("marked.pdf", &document(&objects, b"\xef\xbb\xbf", &[]));
        assert_same_as_tika_delegating(&[&plain, &marked], tika_name(), &DELEGATES);
    }

    /// A document of a form with a field of each kind, a non-terminal field holding `address`
    /// as its value and the signature `blank`.
    fn form_objects(address: &str, blank: &str) -> Vec<String> {
        let mut objects = page_objects(TEXT);
        objects[0] = "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>".to_owned();
        objects.extend(
            [
                "<< /Fields [7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 15 0 R 17 0 R 18 0 R] \
                 /DA (/Helv 0 Tf 0 g) >>",
                "<< /FT /Tx /T (name) /TU (Your name) /V (Kari Nordmann) >>",
                "<< /FT /Btn /T (agree) /V /1 /Opt [(no) (yes)] >>",
                "<< /FT /Btn /Ff 65536 /T (send) /TU (Send the form) >>",
                "<< /FT /Ch /T (colours) /V [(red) (bl\\345)] >>",
                &format!("<< /T (address) {address} /Kids [13 0 R 14 0 R] >>"),
                "<< /T (orphan) /V (no type) >>",
                "<< /FT /Tx /Parent 11 0 R /T (street) /V (Storgata 1) >>",
                "<< /Parent 11 0 R /T (city) /FT /Tx >>",
                "<< /FT /Sig /T (signature) /V 16 0 R >>",
                "<< /Type /Sig /Filter /Adobe.PPKLite /Name (Ola Nordmann) \
                 /M (D:20240102030405+01'30') /Reason (Approval) /Location () \
                 /ContactInfo ( ) >>",
                &format!("<< /FT /Sig /T (unsigned) {blank} >>"),
                "<< /FT /Btn /Ff 32768 /T (size) /V /2 /Kids [19 0 R] >>",
                "<< /Parent 18 0 R /Subtype /Widget /Rect [0 0 1 1] >>",
            ]
            .map(str::to_owned),
        );
        objects
    }

    #[test]
    fn extracts_form_fields_and_signatures_as_tikas_path_does() {
        let objects = form_objects("", "");
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let scratch = Scratch::new("pdf-form");
        let form = scratch.write("form.pdf", &document(&objects, b"", &[]));
        assert_same_as_tika_delegating(&[&form], tika_name(), &DELEGATES);
    }

    #[test]
    fn writes_values_of_non_terminal_fields_and_no_empty_signatures() {
        let objects = form_objects("/V [(Hjem) 2]", "/V << /Type /Sig >>");
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let (xml, metadata) = native_xml(&document(&objects, b"", &[]));
        // Tika writes `COSArray{...}`, and an empty list for the signature (#109).
        assert!(xml.contains("<li>address: [Hjem, 2]</li>"), "{xml}");
        assert_eq!(xml.matches("signaturedata").count(), 1, "{xml}");
        assert_eq!(metadata.get("tk:has-signature").unwrap(), &["true"]);
        let objects = form_objects("/V /Home", "");
        let mut objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        objects[15] = "<< /Type /Sig >>";
        let (xml, metadata) = native_xml(&document(&objects, b"", &[]));
        assert!(xml.contains("<li>address: Home</li>"), "{xml}");
        assert!(!xml.contains("signaturedata"), "{xml}");
        assert_eq!(metadata.get("tk:has-signature"), None);
    }

    /// A document embedding files by an annotation, among its objects and by its tree of
    /// embedded files, `copies` among the embedded files of `second.txt` and `names` at the
    /// end of the tree's names.
    fn embedding(copies: &str, names: &str) -> Vec<u8> {
        let hello = "Hej verden, her er en fil.";
        let second = "Anden fil, ogs\u{e5} med.";
        let third = "Tredje fil.";
        let stream = |dict: &str, text: &str| {
            format!(
                "<< {dict} /Length {} >>\nstream\n{text}\nendstream",
                text.len()
            )
        };
        let mut objects = page_objects(TEXT);
        objects[0] =
            "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 9 0 R >> >>".to_owned();
        objects[2] = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                      /Resources << /Font << /F1 5 0 R >> >> /Annots [8 0 R] >>"
            .to_owned();
        objects.extend([
            "<< /Type /Filespec /F (hello.txt) /UF (hello.txt) /Desc (A greeting) \
             /AFRelationship /Data /EF << /F 7 0 R >> >>"
                .to_owned(),
            stream(
                &format!("/Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size {} >>", hello.len()),
                hello,
            ),
            "<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] /FS (note.txt) \
             /Contents (A note) >>"
                .to_owned(),
            format!(
                "<< /Names [(second.txt) << /F (  ) /UF (second.txt) /EF << /F 10 0 R {copies} >> >> \
                 (hello) 6 0 R {names}] >>"
            ),
            stream("", second),
            "<< /Type /Filespec /F (third.txt) /EF << /F 12 0 R >> >>".to_owned(),
            stream("", third),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        document(&objects, b"", &[])
    }

    #[test]
    fn parses_embedded_files_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-embedded");
        let embedding = scratch.write("embedding.pdf", &embedding("", ""));
        assert_same_as_tika_delegating(&[&embedding], tika_name(), &DELEGATES);
    }

    #[test]
    fn parses_each_embedded_file_once_and_every_name_of_the_tree() {
        // Tika extracts the Mac copy apart, under its name `  ` before `UF`, keeps only the
        // last value of the repeated name and drops the node for its one bad entry (#109).
        let input = embedding(
            "/Mac 10 0 R",
            "(second.txt) << /F (fourth.txt) /EF << /F 12 0 R >> >> (bad) 3",
        );
        let (xml, _) = native_xml(&input);
        assert_eq!(xml.matches("Anden fil").count(), 1, "{xml}");
        assert_eq!(xml.matches("id=\"second.txt\"").count(), 1, "{xml}");
        assert!(xml.contains("id=\"fourth.txt\""), "{xml}");
        assert_eq!(xml.matches("Tredje fil.").count(), 2, "{xml}");
    }

    #[test]
    fn measures_type3_glyphs_from_their_procedures_as_tikas_path_does() {
        let content = format!(
            "{TEXT} BT /F2 12 Tf 72 600 Td (abba abab baba) Tj 0 -14 Td (ab ba ab ba) Tj ET"
        );
        let mut objects = page_objects(&content);
        objects[2] = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                      /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>"
            .to_owned();
        let procedure =
            |text: &str| format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len());
        objects.extend([
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 0 0] \
             /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a 7 0 R /b 8 0 R /space 9 0 R >> \
             /Encoding << /Type /Encoding /Differences [32 /space 97 /a /b] >> >>"
                .to_owned(),
            procedure("520 0 -10 -200 480 700 d1 0 0 450 700 re f"),
            procedure("610 0 d0 0 0 500 650 re f"),
            procedure("250 0 0 0 0 0 d1"),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let scratch = Scratch::new("pdf-type3");
        let type3 = scratch.write("type3.pdf", &document(&objects, b"", &[]));
        assert_same_as_tika_delegating(&[&type3], tika_name(), &DELEGATES);
    }

    /// A document of an XFA form whose data `data` holds.
    fn xfa_form(data: &str) -> Vec<u8> {
        let template = "<template xmlns=\"http://www.xfa.org/schema/xfa-template/3.0/\">\n\
             <subform><draw><value><text>Ans&#248;gning om tilskud</text></value></draw>\n\
             <field name=\"navn\"><assist><toolTip>Dit <p>fulde</p> navn</toolTip></assist>\
             </field>\n<field name=\"by\"/><field name=\"tom\"/></subform>\n</template>";
        let data = format!(
            "<xfa:datasets xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\">\n\
             <xfa:data>{data}</xfa:data></xfa:datasets>"
        );
        let stream =
            |text: &str| format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len());
        let mut objects = page_objects(TEXT);
        objects[0] = "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>".to_owned();
        objects.extend([
            "<< /Fields [] /XFA [(preamble) 7 0 R (template) 8 0 R (datasets) 9 0 R \
             (postamble) 10 0 R] >>"
                .to_owned(),
            stream("<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\">"),
            stream(template),
            stream(&data),
            stream("</xdp:xdp>"),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        document(&objects, b"", &[])
    }

    #[test]
    fn extracts_xfa_forms_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-xfa");
        let xfa = scratch.write(
            "xfa.pdf",
            &xfa_form("<form1><navn>Kari</navn><by>Oslo</by><by>Bergen</by></form1>"),
        );
        assert_same_as_tika_delegating(&[&xfa], tika_name(), &DELEGATES);
    }

    #[test]
    fn takes_the_values_of_xfa_fields_from_their_own_elements() {
        // Tika takes the line breaks before `navn` into its value, and the text since `by`
        // ended as a value of `form1` (#109).
        let (xml, _) = native_xml(&xfa_form(
            "\n<form1>\n<navn>Kari</navn>\n<by>Oslo</by>\n</form1>\n",
        ));
        assert!(
            xml.contains("<li fieldName=\"navn\">Dit fulde\n navn: Kari</li>"),
            "{xml}"
        );
        assert!(xml.contains("<li fieldName=\"by\">by: Oslo</li>"), "{xml}");
        assert!(xml.contains("<li fieldName=\"tom\">tom: </li>"), "{xml}");
    }

    #[test]
    fn defers_a_needed_object_lopdf_cannot_read() {
        let mut objects = page_objects(TEXT);
        objects[3] = format!("<< /Length 5 /X <zz> >>\nstream\n{TEXT}\nendstream");
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        assert_eq!(
            PdfParser
                .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some("object 4, which lopdf cannot read and PDFBox may recover")
        );
    }

    #[test]
    fn defers_a_needed_object_the_table_does_not_list_but_pdfbox_finds() {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        let font = find(&input, b"5 0 obj");
        let entry = format!("{font:010} 00000 n \n");
        let input = replace(&input, entry.as_bytes(), b"0000000000 65535 f \n");
        assert_eq!(
            PdfParser
                .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some(
                "object 5, which the cross-reference table does not list and PDFBox finds by \
                 searching the document"
            )
        );
    }

    #[test]
    fn extracts_documents_pdfbox_repairs_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-repaired");
        let mut objects = page_objects(TEXT);
        objects.push("<< /Title (Eventyr) /Author (H. C. Andersen) >>".to_owned());
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let plain = document(&objects, b"", &[]);
        let start = find(&plain, b"xref\n");
        let startxref = format!("startxref\n{start}\n");
        let documents = [
            ("shifted.pdf", document(&objects, b"", &[(4, 3), (6, 1)])),
            (
                "startxref.pdf",
                replace(
                    &plain,
                    startxref.as_bytes(),
                    format!("startxref\n{}\n", start + 2).as_bytes(),
                ),
            ),
            ("no-table.pdf", plain[..start].to_vec()),
            ("stream.pdf", stream_document(&objects, &[9])),
        ];
        let paths: Vec<PathBuf> = documents
            .iter()
            .map(|(name, bytes)| scratch.write(name, bytes))
            .collect();
        for path in &paths {
            assert_eq!(deferral(path), None, "{}", path.display());
        }
        assert_same_as_tika_delegating(&as_paths(&paths), tika_name(), &DELEGATES);
    }

    #[test]
    fn fails_a_document_whose_recording_outgrows_the_memory_budget() {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        let configs = ParserConfigs::default();
        let prepared = crate::memory::within(1, || {
            PdfParser
                .prepare(&input, &TikaMetadata::new(), &configs)
                .map(drop)
        });
        assert!(
            matches!(prepared, Err(Declined::Fails(Error::MemoryLimit(1)))),
            "{prepared:?}"
        );
    }

    fn with_pdf(pdf: PdfParserConfig) -> ParserConfigs {
        ParserConfigs {
            pdf,
            ..ParserConfigs::default()
        }
    }

    fn corpus_documents(names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| {
                CORPORA
                    .iter()
                    .map(|corpus| Path::new(corpus).join(name))
                    .find(|path| path.exists())
                    .unwrap_or_else(|| panic!("no corpus holds {name}"))
            })
            .collect()
    }

    #[test]
    fn extracts_under_the_settings_the_port_reads_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-settings");
        let objects = page_objects("BT /F1 12 Tf 72 720 Td (Kort.) Tj ET");
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let short = scratch.write("short.pdf", &document(&objects, b"", &[]));
        let tagged = corpus_documents(&["danish.pdf", "report.pdf", "table.pdf"]);
        let settings: [(PdfParserConfig, Vec<PathBuf>); 8] = [
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::NO_OCR),
                [corpus_documents(&["deu-ocr.pdf"]), vec![short.clone()]].concat(),
            ),
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::AUTO),
                corpus_documents(&["notes.pdf"]),
            ),
            (
                PdfParserConfig::new().set_ocr_strategy_auto("faster, 4"),
                [corpus_documents(&["notes.pdf"]), vec![short]].concat(),
            ),
            (
                PdfParserConfig::new().set_extract_marked_content(false),
                tagged.clone(),
            ),
            (
                PdfParserConfig::new().set_extract_marked_content(true),
                [tagged.clone(), corpus_documents(&["prose-columns.pdf"])].concat(),
            ),
            (
                PdfParserConfig::new().set_extract_bookmarks_text(false),
                tagged,
            ),
            (
                PdfParserConfig::new().set_extract_annotation_text(false),
                corpus_documents(&["notes.pdf", "2022_Q3_AAPL.pdf"]),
            ),
            (
                PdfParserConfig::new()
                    .set_extract_inline_images(false)
                    .set_extract_unique_inline_images_only(false),
                corpus_documents(&["danish.pdf"]),
            ),
        ];
        for (pdf, documents) in settings {
            let configs = with_pdf(pdf);
            for document in &documents {
                let input = std::fs::read(document).unwrap();
                let deferral = PdfParser.defers(&input, &TikaMetadata::new(), &configs);
                assert_eq!(deferral, None, "{} under {configs:?}", document.display());
            }
            assert_same_as_tika_delegating_with(
                &as_paths(&documents),
                tika_name(),
                &DELEGATES,
                &configs,
            );
        }
    }

    #[test]
    fn defers_under_the_settings_the_port_does_not_read() {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        for (pdf, start) in [
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::OCR_ONLY),
                "the OCR strategy OCR_ONLY",
            ),
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION),
                "the OCR strategy OCR_AND_TEXT_EXTRACTION",
            ),
            (
                PdfParserConfig::new().set_ocr_strategy_auto("faster , 4"),
                "the automatic OCR thresholds \"faster , 4\"",
            ),
        ] {
            let deferral = PdfParser
                .defers(&input, &TikaMetadata::new(), &with_pdf(pdf))
                .unwrap_or_default();
            assert!(deferral.starts_with(start), "{deferral}");
        }
    }

    #[test]
    fn grants_what_the_permission_bits_grant_to_a_user() {
        let user = Access::User(0xFFFF_FFEC);
        assert!(user.grants(3) && user.grants(4) && !user.grants(2));
        let no_extraction = Access::User(-532_i64 as u64);
        assert!(!no_extraction.grants(5) && !no_extraction.grants(10) && no_extraction.grants(3));
        assert!(Access::Owner.grants(5) && Access::Unencrypted.grants(10));
    }

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let (parsed, _) = documents(&PdfParser, &corpora());
        assert_same_as_tika_delegating(&as_paths(&parsed), tika_name(), &DELEGATES);
    }

    #[test]
    fn reads_pages_by_ocr_as_tikas_path_does_as_the_command_line_sets_it() {
        assert!(ocr_runs(), "tesseract is not installed");
        assert_same_as_tika_delegating_with(
            &as_paths(&read_by_ocr()),
            tika_name(),
            &DELEGATES,
            &ParserConfigs {
                pdf: PdfParserConfig::new()
                    .set_ocr_strategy(PdfOcrStrategy::AUTO)
                    .set_ocr_strategy_auto("50%,10")
                    .set_extract_bookmarks_text(false),
                ocr: TesseractOcrConfig::new().set_language(TesseractOcrConfig::AUTO_LANGUAGE),
                ..ParserConfigs::default()
            },
        );
    }

    #[test]
    fn defers_what_the_port_does_not_do() {
        let (_, deferred) = documents(&PdfParser, &corpora());
        let mut found: Vec<(String, String)> = deferred
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_str().unwrap().to_owned();
                (name, deferral_of(path))
            })
            .collect();
        found.sort();
        let expected: Vec<(&str, Why)> = NOT_THE_TEXT_LAYER
            .iter()
            .copied()
            .filter(|(_, why)| why.defers())
            .collect();
        assert_eq!(
            found
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(|(name, _)| *name).collect::<Vec<_>>()
        );
        for ((name, reason), (_, why)) in found.iter().zip(&expected) {
            let as_expected = match why {
                Why::Ocr | Why::Unmapped => {
                    reason.starts_with("page ") && reason.contains("wants OCR")
                }
                Why::Doubtful => unreachable!("{name} is deferred for a doubtful page"),
                Why::Unported(start) => reason.starts_with(start),
            };
            assert!(as_expected, "{name}: {reason}");
        }
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_the_conformance_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&PdfParser, &[&conformance_corpus()]);
        for path in &deferred {
            println!("deferred {}: {}", path.display(), deferral_of(path));
        }
        println!("{} parsed, {} deferred", parsed.len(), deferred.len());
        // Tika extracts its Mac, DOS and Unix copies of one file as files of their own (#109).
        let (copies, paths): (Vec<&Path>, Vec<&Path>) = as_paths(&parsed)
            .into_iter()
            .partition(|path| path.ends_with("testPDF_multiFormatEmbFiles.pdf"));
        assert_same_as_tika_delegating(&paths, tika_name(), &DELEGATES);
        let [copies] = copies[..] else {
            panic!("{copies:?}");
        };
        let (xml, _) = native_xml(&std::fs::read(copies).unwrap());
        assert_eq!(xml.matches("class=\"embedded\"").count(), 1, "{xml}");
        assert!(
            xml.contains("id=\"Test.txt\"") && xml.contains("non os specific"),
            "{xml}"
        );
    }
}
