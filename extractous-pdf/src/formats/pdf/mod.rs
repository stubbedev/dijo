//! PDF documents, as Tika's `PDFParser` reads them with PDFBox, under extractous's
//! `TextLayerCheckingPdfParser` when tesseract runs: the document's objects as PDFBox's
//! `COSParser` reads them, each when it is first needed, then the metadata `PDFParser` collects, and the text of each page as PDFBox's
//! `PDFTextStripper` assembles it from the glyphs its content stream shows, written by
//! `PDF2XHTML` in our reading order, or for a tagged document by `PDFMarkedContent2XHTML`
//! through its structure tree, each page's images after its text when they are extracted
//! ([`images`]), then the bookmarks, the files the document embeds, parsed as
//! embedded documents, and the form. The pages `TextLayerCheckingPdfParser` reads by OCR are
//! rendered and read with tesseract ([`ocr`]). An encrypted document opens with the password
//! the extraction gives, or the empty one, as PDFBox opens it ([`security`]), and fails as
//! Tika's path fails it where that password does not open it. Documents with a page Tika would
//! OCR with no tesseract to run, and documents with what the port does not do (XFA forms with a
//! document type declaration and fonts it cannot measure, among others), are left to Tika.

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
mod function;
mod images;
mod matrix;
mod metadata;
mod ocr;
mod position;
mod quality;
mod reading_order;
mod recording;
mod security;
mod stripper;
mod structure;
mod tagged;
pub(crate) mod truetype;
mod type1;
mod writer;
mod xfa;
mod xref;

use crate::java::jfloat;

use std::ops::ControlFlow;

use lopdf::EncryptionState;

use crate::config::{MarkedContentStrategy, ParserConfigs, Password};
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{
    crypt, Decline, Declined, Failure, Footprint, Parser, Prepared, PASSWORD_USED,
};
use crate::input::{Pages, Paging};

use cos::Pdf;
use ocr::{OcrCheck, PageOcr};
use recording::Recording;
use security::{Access, Opening, Refusal, WRONG_PASSWORD};
use structure::StructureIndex;
use tagged::Tagged;
use writer::{DocumentWriter, WriteFailure};

const MEDIA_TYPE: &str = "application/pdf";
const TEXT_LAYER_CHECKING_PARSER: &str = "extractous::TextLayerCheckingPdfParser";
const PDF_PARSER: &str = "extractous::PDFParser";
const TIKA_PDF_PARSER: &str = "org.apache.tika.parser.pdf.PDFParser";
const TIKA_TEXT_LAYER_CHECKING_PARSER: &str = "ai.yobix.TextLayerCheckingPdfParser";
/// The parser our `TextLayerCheckingPdfParser` records delegating to, and the OCR parser Tika's
/// PDF parser hands the pages it OCRs itself, under both names.
const DELEGATES: [(&str, &str); 2] = [
    (PDF_PARSER, TIKA_PDF_PARSER),
    (
        crate::formats::image::PREPARING_OCR_PARSER,
        "ai.yobix.PreparingOcrParser",
    ),
];
/// `AbstractPDF2XHTML.MAX_RECURSION_DEPTH`.
const MAX_RECURSION_DEPTH: usize = 100;

pub(super) use crate::formats::Unported;
use crate::ocr::{tesseract_runs as ocr_runs, OcrSettings};

/// For the parity tests: the failure of an XFA form's XML, which Tika records as the stack trace
/// of Woodstox's exception and the native parser as the failure its own XML parser reports
/// (#125).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const XFA_FAILURE: crate::formats::parity::Departures =
    &[crate::formats::parity::Departure::Values(
        crate::formats::embedded::EXCEPTION_WARNING,
        "#125",
    )];

/// For the parity tests: the document of Tika's tests whose XFA form's XML fails, which departs
/// by [`XFA_FAILURE`].
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const MALFORMED_XFA: &str = "testPDF_malformedXFA.pdf";

/// Tika's `PDFParser`, wrapped in our `TextLayerCheckingPdfParser` when tesseract runs.
pub struct PdfParser;

impl Parser for PdfParser {
    fn name(&self) -> &'static str {
        if ocr_runs() {
            TEXT_LAYER_CHECKING_PARSER
        } else {
            PDF_PARSER
        }
    }
    fn tika_name(&self) -> &'static str {
        if ocr_runs() {
            TIKA_TEXT_LAYER_CHECKING_PARSER
        } else {
            TIKA_PDF_PARSER
        }
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &DELEGATES
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        prepare(input, Pages::IN_MEMORY, metadata, configs, RECORDING_LIMIT)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        prepare(input, pages, metadata, configs, RECORDING_LIMIT)
    }
}

/// [`Parser::prepare_paged`], streaming a document whose recording grows past `limit` bytes.
fn prepare<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    metadata: &TikaMetadata,
    configs: &'a ParserConfigs,
    limit: usize,
) -> Result<Prepared<'a>, Decline<'a>> {
    match record(input, pages, metadata, configs, limit)? {
        Recorded::Whole {
            mut recording,
            page_ocr,
            ocr_pages,
        } => Ok(Box::new(move |xhtml| {
            let written = match page_ocr {
                None => recording.flush(xhtml, None),
                Some(page_ocr) => write_read(&page_ocr, &mut recording, xhtml, configs),
            };
            ended(xhtml, written, ocr_pages)
        })),
        Recorded::Large(loaded, settings) => {
            let metadata = metadata.clone();
            Ok(Box::new(move |xhtml| {
                let ocr_pages = match settings.ocr.dispatches_every_page() {
                    true => loaded.pdf.pages().map_err(late)?.len(),
                    false => 0,
                };
                let written = stream(&loaded, input, settings, metadata, configs, xhtml);
                ended(xhtml, written, ocr_pages)
            }))
        }
    }
}

/// The document `written` to `xhtml`, with the count of pages Tika's `PDFParser` handed its OCR
/// parser, `ocr_pages`: none where our `TextLayerCheckingPdfParser` reads them.
fn ended(xhtml: &mut Xhtml, written: Result<(), Failure>, ocr_pages: usize) -> Result<(), Failure> {
    xhtml
        .metadata_mut()
        .set("pdf:ocr-page-count", ocr_pages.to_string());
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

/// Whether the images of the pages are extracted, as `setExtractInlineImages` asks of
/// `PDF2XHTML`; `OCR2XHTML`, which reads every page by OCR alone, extracts none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageExtraction {
    Off,
    On {
        /// `setExtractUniqueInlineImagesOnly`.
        unique_only: bool,
    },
}

impl Settings {
    /// The settings `configs` set, or why the port leaves documents to Tika under them.
    fn of(configs: &ParserConfigs) -> Result<Self, Unported> {
        let config = &configs.pdf;
        let ocr = OcrCheck::of(config, ocr_runs())?;
        let images = match config.extracts_inline_images() {
            false => ImageExtraction::Off,
            true if ocr.reads_only_ocr() => ImageExtraction::Off,
            true => ImageExtraction::On {
                unique_only: config.extracts_unique_inline_images_only(),
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
enum Recorded<'a> {
    /// The whole extraction, the OCR of the pages read, and how many pages Tika's PDF parser
    /// hands its OCR parser.
    Whole {
        recording: Box<Recording>,
        page_ocr: Option<PageOcr>,
        ocr_pages: usize,
    },
    /// The document, loaded, whose recording grew past its limit before a page to read by OCR:
    /// it is streamed.
    Large(Box<Loaded<'a>>, Settings),
}

/// A recording of a document whose parser starts from `metadata`, marked as our Java's
/// `PasswordProvider` marks it once `PDFParser` asks it for the password `configs` give.
fn new_recording(metadata: TikaMetadata, configs: &ParserConfigs) -> Recording {
    let mut recording = Recording::new(metadata);
    if ocr_runs() {
        recording.delegate_to(PDF_PARSER);
    }
    if configs.password.is_some() {
        recording.set(PASSWORD_USED, "true");
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
fn record<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    metadata: &TikaMetadata,
    configs: &ParserConfigs,
    limit: usize,
) -> Result<Recorded<'a>, Declined> {
    let deferred = |Unported(reason)| Decline::Deferred(reason);
    let settings = Settings::of(configs).map_err(deferred)?;
    let loaded = load(input, pages, configs)?;
    let mut recording = new_recording(metadata.clone(), configs);
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
                WriteFailure::Unported(Unported(reason)) => Decline::Deferred(reason),
                WriteFailure::Stop(Stop::MemoryLimit(limit)) => {
                    Decline::Fails(Error::MemoryLimit(limit))
                }
                WriteFailure::Stop(stop) => {
                    unreachable!("a recording stops only at the memory budget, not at {stop:?}")
                }
            }
        });
    if let Err(failure @ Decline::Fails(_)) = recorded {
        return Err(failure);
    }
    if let Some(reason) = unread_objects(&loaded).map_err(deferred)? {
        return Err(Decline::Deferred(reason));
    }
    if recorded?.is_break() {
        return Ok(Recorded::Large(Box::new(loaded), settings));
    }
    let page_count = loaded.pdf.pages().map_err(deferred)?.len();
    let page_ocr = PageOcr::of(
        recording.pages_read(),
        page_count,
        settings.ocr,
        input,
        configs,
    )
    .map_err(deferred)?;
    Ok(Recorded::Whole {
        recording: Box::new(recording),
        page_ocr,
        ocr_pages: match settings.ocr.dispatches_every_page() {
            true => page_count,
            false => 0,
        },
    })
}

/// An object the extraction needed that the port could not read, or that the cross-reference
/// table does not list and PDFBox finds, if any.
fn unread_objects(loaded: &Loaded) -> Result<Option<String>, Unported> {
    if let Some(((number, generation), Unported(reason))) = loaded.pdf.unread_object_needed() {
        return Ok(Some(format!(
            "object {number} {generation}, which the port cannot read: {reason}"
        )));
    }
    let unlisted = loaded.pdf.unlisted_objects_needed();
    Ok(loaded.pdf.first_found(&unlisted)?.map(|(number, _)| {
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
    let pages = StreamedPages {
        input,
        page_count,
        check: settings.ocr,
        configs,
    };
    let mut recording = new_recording(metadata, configs);
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
    if let Some(reason) = unread_objects(loaded).map_err(late)? {
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

/// What a streamed document's pages need to be written: what reads pages by OCR.
struct StreamedPages<'a> {
    input: &'a [u8],
    page_count: usize,
    check: OcrCheck,
    configs: &'a ParserConfigs,
}

impl StreamedPages<'_> {
    /// Writes what `recording` holds to `xhtml`, a page OCR reads read first.
    fn flush(&self, recording: &mut Recording, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let pages = recording.pages_read();
        let page_ocr = PageOcr::of(pages, self.page_count, self.check, self.input, self.configs)
            .map_err(late)?;
        match page_ocr {
            None => recording.flush(xhtml, None),
            Some(page_ocr) => write_read(&page_ocr, recording, xhtml, self.configs),
        }
    }
}

/// A document's objects, and what the password opens it with.
struct Loaded<'a> {
    pdf: Pdf<'a>,
    access: Access,
}

/// The document's objects where PDFBox finds them, decrypted with the password Tika's
/// `PDFParser` asks its `PasswordProvider` for, or the empty one: a document that password does
/// not open fails as Tika's path fails it. RC4 is opened as PDFBox opens it and decrypted with
/// the user password; AES-256 is opened and decrypted by `lopdf`. The objects decrypt as they
/// are read.
fn load<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    configs: &ParserConfigs,
) -> Result<Loaded<'a>, Declined> {
    let paging = Paging::new(pages);
    let resolution = xref::resolve(input, &paging)?;
    let password = configs.password.as_ref().map_or("", Password::expose);
    let opening = Opening::of(resolution.encryption(input)?, password).map_err(refused)?;
    let mut pdf = Pdf::new(input, paging, &resolution)?;
    let (decryption_password, access) = match &opening {
        Opening::Unencrypted => {
            return Ok(Loaded {
                pdf,
                access: Access::Unencrypted,
            })
        }
        Opening::Rc4 {
            access,
            user_password,
        } => (user_password.as_str(), Some(*access)),
        Opening::Aes256 => (password, None),
    };
    let mut encryption = pdf
        .encryption_document()
        .ok_or_else(|| Unported("an encryption dictionary the port cannot read".to_owned()))?;
    if encryption
        .authenticate_password(decryption_password)
        .is_err()
    {
        return Err(match opening {
            Opening::Aes256 => refused(security::aes256_rejection(password)),
            _ => Unported("encryption lopdf does not decrypt".to_owned()).into(),
        });
    }
    let state = EncryptionState::decode(&encryption, decryption_password)
        .map_err(|error| Unported(format!("encryption lopdf does not decrypt: {error}")))?;
    let access = match access {
        Some(access) => access,
        None => {
            encryption.encryption_state = Some(state.clone());
            security::aes256_access(&mut encryption, password).map_err(refused)?
        }
    };
    let id = match pdf.trailer().get(b"Encrypt") {
        Ok(lopdf::Object::Reference(id)) => Some(*id),
        _ => None,
    };
    pdf.decrypt_with(state, id);
    let found = resolution
        .object_stream_placements(input, &|id, dict, data| pdf.decrypted(id, dict, data))?;
    pdf.place(found);
    Ok(Loaded { pdf, access })
}

/// How a document the password does not open declines: as Tika's path fails it, where PDFBox
/// throws `InvalidPasswordException`.
fn refused(refusal: Refusal) -> Declined {
    match refusal {
        Refusal::WrongPassword => Decline::Fails(crypt::encrypted(Some(&format!(
            "org.apache.pdfbox.pdmodel.encryption.InvalidPasswordException: {WRONG_PASSWORD}"
        )))),
        Refusal::Unported(unported) => unported.into(),
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
    for offset in metadata::scan_startxrefs(input, pdf.paging()) {
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
        _ if !has_marked_content || settings.ocr.reads_only_ocr() => None,
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
    #[cfg(feature = "tika")]
    use crate::config::PdfOcrStrategy;
    use crate::config::{PdfParserConfig, TesseractOcrConfig};
    use crate::formats::embedded::EXCEPTION_WARNING;
    use crate::formats::parity::documents;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_same_as_tika_delegating, assert_same_as_tika_delegating_departing_with,
        assert_same_as_tika_delegating_with, conformance_corpus, Scratch,
    };
    use std::collections::BTreeMap;
    use std::path::Path;
    #[cfg(feature = "tika")]
    use std::path::PathBuf;

    pub(super) const CORPORA: [&str; 2] = ["../quality/corpus", "../test_files/documents"];

    /// Why OCR reads pages of a document of the corpora.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Why {
        /// Tika OCRs a page of next to no glyphs, whatever its text layer reads.
        Ocr,
        /// Tika OCRs a page of too many glyphs without characters: by their count, or for its
        /// doubtful text layer when tesseract runs.
        Unmapped,
        /// Our `TextLayerCheckingPdfParser` doubts a page's text layer, when tesseract runs.
        Doubtful,
    }

    impl Why {
        /// Whether the port leaves a document for this reason to Tika: one whose pages Tika's
        /// PDF parser wants OCR for when no tesseract runs.
        fn defers(self) -> bool {
            match self {
                Self::Ocr | Self::Unmapped => !ocr_runs(),
                Self::Doubtful => false,
            }
        }

        /// Whether the port reads pages of a document for this reason by OCR.
        #[cfg(feature = "tika")]
        fn reads_by_ocr(self) -> bool {
            ocr_runs() && matches!(self, Self::Ocr | Self::Unmapped | Self::Doubtful)
        }
    }

    /// The documents of the corpora whose pages OCR reads, or which the port leaves to Tika,
    /// and why.
    const NOT_THE_TEXT_LAYER: [(&str, Why); 8] = [
        ("arabic.pdf", Why::Unmapped),
        ("deu-ocr.pdf", Why::Ocr),
        ("eng-ocr.pdf", Why::Ocr),
        ("hindi.pdf", Why::Unmapped),
        ("prose-broken-offset.pdf", Why::Doubtful),
        ("prose-broken-private-use.pdf", Why::Doubtful),
        ("prose-scan.pdf", Why::Ocr),
        ("thai.pdf", Why::Unmapped),
    ];

    #[cfg(feature = "tika")]
    pub(super) fn tika_name() -> &'static str {
        PdfParser.tika_name()
    }

    #[cfg(feature = "tika")]
    pub(super) fn as_paths(paths: &[PathBuf]) -> Vec<&Path> {
        paths.iter().map(PathBuf::as_path).collect()
    }

    fn corpora() -> Vec<&'static Path> {
        CORPORA.iter().map(Path::new).collect()
    }

    /// The documents of the corpora whose pages OCR reads, when tesseract runs.
    #[cfg(feature = "tika")]
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
    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
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
    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
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
            let recorded = record(
                &input,
                Pages::IN_MEMORY,
                &TikaMetadata::new(),
                &configs,
                RECORDING_LIMIT,
            );
            assert!(
                matches!(
                    recorded,
                    Ok(Recorded::Whole {
                        page_ocr: Some(_),
                        ..
                    })
                ),
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
        ) -> Result<Prepared<'a>, Decline<'a>> {
            prepare(input, Pages::IN_MEMORY, metadata, configs, 0)
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
            let routed = crate::formats::parity::route_file(path).unwrap();
            let input = &routed.input;
            let recorded = record(input, input.pages(), &TikaMetadata::new(), &configs, 0);
            streamed += usize::from(matches!(recorded, Ok(Recorded::Large(..))));
            for output in [crate::formats::Output::Text, crate::formats::Output::Xml] {
                for limit in [200, usize::MAX >> 1] {
                    let extract = |parser: &dyn Parser| {
                        let extracted = crate::formats::parity::parse_to_string(
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
    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_documents_with_a_byte_order_mark_as_tikas_path_does() {
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

    /// A page showing `codes` in a composite font of the CMap `encoding` whose descendant, not
    /// embedded, is of the character collection `ordering` of `kind`.
    #[cfg(feature = "tika")]
    fn composite(encoding: &str, ordering: &str, kind: &str, codes: &str) -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 72 720 Td <{codes}> Tj ET");
        let mut objects = page_objects(&content);
        objects[4] = format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /CJKFont /Encoding /{encoding} \
             /DescendantFonts [6 0 R] >>"
        );
        objects.extend([
            format!(
                "<< /Type /Font /Subtype /{kind} /BaseFont /CJKFont \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering ({ordering}) /Supplement 2 >> \
                 /FontDescriptor 7 0 R /DW 1000 >>"
            ),
            "<< /Type /FontDescriptor /FontName /CJKFont /Flags 4 /FontBBox [0 -200 1000 900] \
             /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 >>"
                .to_owned(),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        document(&objects, b"", &[])
    }

    #[cfg(feature = "tika")]
    #[test]
    fn maps_predefined_cmaps_to_unicode_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-predefined-cmaps");
        let documents = [
            (
                "unijis.pdf",
                composite(
                    "UniJIS-UCS2-H",
                    "Japan1",
                    "CIDFontType0",
                    "30423044304630483048",
                ),
            ),
            (
                "rksj.pdf",
                composite("90ms-RKSJ-H", "Japan1", "CIDFontType0", "82a082a282a441"),
            ),
            (
                "uniks.pdf",
                composite("UniKS-UCS2-H", "Korea1", "CIDFontType2", "D55CAE00"),
            ),
            (
                "gbk.pdf",
                composite("GBK-EUC-H", "GB1", "CIDFontType0", "D6D0CEC4"),
            ),
            (
                "b5.pdf",
                composite("ETen-B5-H", "CNS1", "CIDFontType0", "A4A4A4E5"),
            ),
            (
                "identity.pdf",
                composite("Identity-H", "Japan1", "CIDFontType0", "034B034C"),
            ),
            (
                "vertical.pdf",
                composite("UniJIS-UCS2-HW-V", "Japan1", "CIDFontType0", "30423044"),
            ),
        ];
        let paths: Vec<PathBuf> = documents
            .iter()
            .map(|(name, bytes)| scratch.write(name, bytes))
            .collect();
        // The pages show next to no glyphs, which Tika would read by OCR: the text layer alone
        // is compared.
        let pdf = PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::NO_OCR);
        let configs = with_pdf(pdf.clone());
        for (_, bytes) in &documents {
            let deferral = PdfParser.defers(bytes, &TikaMetadata::new(), &configs);
            assert_eq!(deferral, None);
        }
        assert_same_as_tika_delegating_with(&as_paths(&paths), tika_name(), &DELEGATES, &configs);
        for ((_, bytes), text) in documents.iter().zip([
            "\u{3042}\u{3044}\u{3046}\u{3048}\u{3048}",
            "\u{3042}\u{3044}\u{3046}A",
            "\u{d55c}\u{ae00}",
            "\u{4e2d}\u{6587}",
            "\u{4e2d}\u{6587}",
        ]) {
            let (xml, _) = crate::Extractor::new()
                .set_xml_output(true)
                .set_pdf_config(pdf.clone())
                .extract_bytes_to_string(bytes)
                .unwrap();
            assert!(xml.contains(text), "{text}: {xml}");
        }
    }

    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
    #[test]
    fn parses_embedded_files_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-embedded");
        let embedding = scratch.write("embedding.pdf", &embedding("", ""));
        assert_same_as_tika_delegating(&[&embedding], tika_name(), &DELEGATES);
    }

    /// A document of a page drawn by `content` with a file attachment annotation embedding a
    /// text file of `text`.
    #[cfg(feature = "tika")]
    fn attaching(content: &str, text: &str) -> Vec<u8> {
        let mut objects = page_objects(content);
        objects[2] = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                      /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>"
            .to_owned();
        objects.extend([
            "<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] /FS 7 0 R >>".to_owned(),
            "<< /Type /Filespec /F (vedlagt.txt) /EF << /F 8 0 R >> >>".to_owned(),
            format!(
                "<< /Type /EmbeddedFile /Length {} >>\nstream\n{text}\nendstream",
                text.len()
            ),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        document(&objects, b"", &[])
    }

    #[cfg(feature = "tika")]
    #[test]
    fn weighs_the_files_an_annotation_embeds_with_the_page_as_tikas_path_does() {
        assert!(ocr_runs(), "tesseract is not installed");
        let scratch = Scratch::new("pdf-annotation-file");
        let prose = "Det var en gang en konge, som havde tre d\u{f8}tre, og den yngste var den \
                     smukkeste af dem alle sammen.";
        let documents = [
            (
                "few-glyphs.pdf",
                attaching("BT /F1 12 Tf 72 720 Td (Kort.) Tj ET", prose),
            ),
            ("prose.pdf", attaching(TEXT, prose)),
            (
                "noise.pdf",
                attaching(TEXT, &"#%&/ {}[] 0x7f ~^ ".repeat(40)),
            ),
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

    #[cfg(feature = "tika")]
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

    /// A document of an XFA form whose data `data` holds, and of the AcroForm fields `fields`,
    /// numbered from 11 on.
    fn xfa_form(data: &str, fields: &[&str]) -> Vec<u8> {
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
        let references: String = (0..fields.len())
            .map(|index| format!("{} 0 R ", index + 11))
            .collect();
        objects.extend([
            format!(
                "<< /Fields [{references}] /XFA [(preamble) 7 0 R (template) 8 0 R \
                 (datasets) 9 0 R (postamble) 10 0 R] >>"
            ),
            stream("<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\">"),
            stream(template),
            stream(&data),
            stream("</xdp:xdp>"),
        ]);
        objects.extend(fields.iter().map(|field| (*field).to_owned()));
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        document(&objects, b"", &[])
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_xfa_forms_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-xfa");
        let xfa = scratch.write(
            "xfa.pdf",
            &xfa_form(
                "<form1><navn>Kari</navn><by>Oslo</by><by>Bergen</by></form1>",
                &[],
            ),
        );
        assert_same_as_tika_delegating(&[&xfa], tika_name(), &DELEGATES);
    }

    /// The fields of an AcroForm, which a document of an XFA form shows once the form fails.
    const ACRO_FIELDS: [&str; 2] = [
        "<< /FT /Tx /T (navn) /TU (Fulde navn) /V (Kari Nordmann) >>",
        "<< /FT /Tx /T (by) /V (Bergen) >>",
    ];

    /// The XFA data of a document whose form's XML fails at its mismatched end tag.
    const MALFORMED_XFA_DATA: &str = "<form1><navn>Kari</nav></form1>";

    #[cfg(feature = "tika")]
    #[test]
    fn reads_the_acroform_of_a_failing_xfa_form_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-xfa-malformed");
        let malformed = scratch.write("malformed.pdf", &xfa_form(MALFORMED_XFA_DATA, &ACRO_FIELDS));
        assert_same_as_tika_delegating_departing_with(
            &[&malformed],
            tika_name(),
            &DELEGATES,
            XFA_FAILURE,
            &ParserConfigs::default(),
        );
    }

    #[test]
    fn records_why_an_xfa_form_fails_and_reads_its_acroform() {
        let (xml, metadata) = native_xml(&xfa_form(MALFORMED_XFA_DATA, &ACRO_FIELDS));
        let content = "<div class=\"xfa_content\"><p>Ans\u{f8}gning om tilskud</p>\n</div>";
        assert!(xml.contains(content), "{xml}");
        assert!(
            xml.contains("<li altName=\"Fulde navn\">navn: Kari Nordmann</li>"),
            "{xml}"
        );
        assert!(xml.contains("<li>by: Bergen</li>"), "{xml}");
        let warnings = metadata.get(EXCEPTION_WARNING).unwrap();
        assert!(
            warnings.len() == 1 && warnings[0].starts_with("javax.xml.stream.XMLStreamException: "),
            "{warnings:?}"
        );
    }

    #[test]
    fn takes_the_values_of_xfa_fields_from_their_own_elements() {
        // Tika takes the line breaks before `navn` into its value, and the text since `by`
        // ended as a value of `form1` (#109).
        let (xml, _) = native_xml(&xfa_form(
            "\n<form1>\n<navn>Kari</navn>\n<by>Oslo</by>\n</form1>\n",
            &[],
        ));
        assert!(
            xml.contains("<li fieldName=\"navn\">Dit fulde\n navn: Kari</li>"),
            "{xml}"
        );
        assert!(xml.contains("<li fieldName=\"by\">by: Oslo</li>"), "{xml}");
        assert!(xml.contains("<li fieldName=\"tom\">tom: </li>"), "{xml}");
    }

    /// The documents qpdf 12.3.2 wrote of one page and an information dictionary, encrypted
    /// for the empty user password with RC4, AES-128 and AES-256 and with every object it
    /// could put in an object stream there:
    /// `qpdf [--allow-weak-crypto] --object-streams=generate --encrypt "" eier 128|256
    /// [--use-aes=n|y] -- plain.pdf out.pdf`.
    const ENCRYPTED_OBJECT_STREAMS: [&str; 3] = [
        "tests/fixtures/pdf/object-streams-rc4.pdf",
        "tests/fixtures/pdf/object-streams-aes.pdf",
        "tests/fixtures/pdf/object-streams-aes256.pdf",
    ];

    /// The document at `path` with a comment before its first object, which moves every object
    /// off the offset its cross-reference stream gives, so that PDFBox searches the document for
    /// its objects and object streams.
    fn moved_objects(path: &str) -> Vec<u8> {
        let input = std::fs::read(path).unwrap();
        replace(&input, b"\n1 0 obj", b"\n%flyttet\n1 0 obj")
    }

    #[test]
    fn decrypts_the_object_streams_a_search_finds() {
        for path in ENCRYPTED_OBJECT_STREAMS {
            let input = moved_objects(path);
            let deferral =
                PdfParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default());
            assert_eq!(deferral, None, "{path}");
            let (xml, _) = native_xml(&input);
            assert!(
                xml.contains("Den yngste var den smukkeste"),
                "{path}: {xml}"
            );
            assert!(
                xml.contains("<meta name=\"dc:creator\" content=\"Kari\" />"),
                "{path}: {xml}"
            );
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    fn decrypts_the_object_streams_a_search_finds_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-encrypted-search");
        let moved: Vec<PathBuf> = ENCRYPTED_OBJECT_STREAMS
            .iter()
            .map(|path| {
                let name = Path::new(path).file_name().unwrap().to_str().unwrap();
                scratch.write(name, &moved_objects(path))
            })
            .collect();
        assert_same_as_tika_delegating(&as_paths(&moved), tika_name(), &DELEGATES);
    }

    /// The offset the last `startxref` of `document` gives.
    fn startxref(document: &[u8]) -> usize {
        let at = document
            .windows(b"startxref".len())
            .rposition(|window| window == b"startxref")
            .unwrap();
        let rest = std::str::from_utf8(&document[at + b"startxref".len()..]).unwrap();
        rest.split_whitespace().next().unwrap().parse().unwrap()
    }

    /// Writes the object `number` holding `object` at the end of `out`, returning its offset.
    fn object_at(out: &mut Vec<u8>, number: usize, object: &[u8]) -> usize {
        let offset = out.len();
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
        offset
    }

    /// The rows of a cross-reference stream of the widths `[1 4 2]` placing objects in the file
    /// at `offsets`.
    fn stream_rows(offsets: &[usize]) -> Vec<u8> {
        offsets
            .iter()
            .flat_map(|&offset| {
                let mut row = vec![1];
                row.extend_from_slice(&(offset as u32).to_be_bytes());
                row.extend_from_slice(&[0, 0]);
                row
            })
            .collect()
    }

    /// A document of a page, updated with an information dictionary in a section whose trailer
    /// gives its `/Prev` as an object of its own.
    fn indirect_prev() -> Vec<u8> {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let mut out = document(&objects, b"", &[]);
        let prev = startxref(&out);
        let info = object_at(&mut out, 6, b"<< /Title (Opdateret) /Author (Kari) >>");
        let number = object_at(&mut out, 7, prev.to_string().as_bytes());
        let xref = out.len();
        out.extend_from_slice(
            format!(
                "xref\n6 2\n{info:010} 00000 n \n{number:010} 00000 n \ntrailer\n<< /Size 8 \
                 /Root 1 0 R /Info 6 0 R /Prev 7 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    /// A document of a page, updated in a hybrid section whose trailer gives its `/XRefStm` as an
    /// object of its own, the stream placing the information dictionary the table leaves out.
    fn indirect_xref_stream_offset() -> Vec<u8> {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let mut out = document(&objects, b"", &[]);
        let prev = startxref(&out);
        let info = object_at(&mut out, 6, b"<< /Title (Hybrid) /Author (Kari) >>");
        let number_at = out.len();
        let digits = 10;
        let stream_at = number_at + "7 0 obj\n".len() + digits + "\nendobj\n".len();
        let number = object_at(&mut out, 7, format!("{stream_at:0digits$}").as_bytes());
        let rows = stream_rows(&[info]);
        let mut stream = format!(
            "<< /Type /XRef /Size 9 /W [1 4 2] /Index [6 1] /Length {} >>\nstream\n",
            rows.len()
        )
        .into_bytes();
        stream.extend_from_slice(&rows);
        stream.extend_from_slice(b"\nendstream");
        assert_eq!(object_at(&mut out, 8, &stream), stream_at);
        let xref = out.len();
        out.extend_from_slice(
            format!(
                "xref\n7 2\n{number:010} 00000 n \n{stream_at:010} 00000 n \ntrailer\n<< /Size 9 \
                 /Root 1 0 R /Info 6 0 R /Prev {prev} /XRefStm 7 0 R >>\nstartxref\n{xref}\n\
                 %%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    /// A document of a page and an information dictionary whose cross-reference stream gives
    /// its `/Length` as an object of its own.
    fn indirect_xref_stream_length() -> Vec<u8> {
        let mut objects = page_objects(TEXT);
        objects.push("<< /Title (Str\\370mmen) /Author (Kari) >>".to_owned());
        let mut out = b"%PDF-1.5\n".to_vec();
        let mut offsets: Vec<usize> = objects
            .iter()
            .enumerate()
            .map(|(index, object)| object_at(&mut out, index + 1, object.as_bytes()))
            .collect();
        let length_number = objects.len() + 1;
        let stream_number = length_number + 1;
        let rows_length = 7 * stream_number;
        offsets.push(object_at(
            &mut out,
            length_number,
            rows_length.to_string().as_bytes(),
        ));
        offsets.push(out.len());
        let rows = stream_rows(&offsets);
        assert_eq!(rows.len(), rows_length);
        let mut stream = format!(
            "<< /Type /XRef /Size {} /W [1 4 2] /Index [1 {stream_number}] /Root 1 0 R \
             /Info 6 0 R /Length {length_number} 0 R >>\nstream\n",
            stream_number + 1
        )
        .into_bytes();
        stream.extend_from_slice(&rows);
        stream.extend_from_slice(b"\nendstream");
        let xref = object_at(&mut out, stream_number, &stream);
        out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
        out
    }

    /// The documents whose cross-reference gives what PDFBox reads as it reads it as objects
    /// of their own, and the title each gives.
    fn indirect_cross_references() -> [(Vec<u8>, &'static str); 3] {
        [
            (indirect_prev(), "Opdateret"),
            (indirect_xref_stream_offset(), "Hybrid"),
            (indirect_xref_stream_length(), "Str\u{f8}mmen"),
        ]
    }

    #[test]
    fn follows_the_references_of_the_cross_reference_as_pdfbox_does() {
        for (input, title) in indirect_cross_references() {
            let deferral =
                PdfParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default());
            assert_eq!(deferral, None, "{title}");
            let (xml, _) = native_xml(&input);
            assert!(xml.contains(&format!("<title>{title}</title>")), "{xml}");
            assert!(xml.contains("Den yngste var den smukkeste"), "{xml}");
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    fn follows_the_references_of_the_cross_reference_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-indirect-xref");
        let paths: Vec<PathBuf> = indirect_cross_references()
            .iter()
            .enumerate()
            .map(|(index, (input, _))| scratch.write(&format!("indirect-{index}.pdf"), input))
            .collect();
        assert_same_as_tika_delegating(&as_paths(&paths), tika_name(), &DELEGATES);
    }

    /// A document of a page and a file specification, object 6, whose update gives the object
    /// a generation 1 of another file, the table listing both generations.
    fn two_generations() -> Vec<u8> {
        let stream = |text: &str| {
            format!(
                "<< /Type /EmbeddedFile /Length {} >>\nstream\n{text}\nendstream",
                text.len() + 1
            )
        };
        let mut objects = page_objects(TEXT);
        objects.extend([
            "<< /Type /Filespec /F (gammel.txt) /EF << /F 7 0 R >> >>".to_owned(),
            stream("Den gamle udgave af filen."),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let mut out = document(&objects, b"", &[]);
        let prev = startxref(&out);
        let spec = out.len();
        out.extend_from_slice(
            b"6 1 obj\n<< /Type /Filespec /F (ny.txt) /EF << /F 8 0 R >> >>\nendobj\n",
        );
        let file = object_at(&mut out, 8, stream("Den nye udgave af filen.").as_bytes());
        let xref = out.len();
        out.extend_from_slice(
            format!(
                "xref\n6 1\n{spec:010} 00001 n \n8 1\n{file:010} 00000 n \ntrailer\n<< /Size 9 \
                 /Root 1 0 R /Prev {prev} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn extracts_the_files_of_both_generations_of_an_object() {
        let input = two_generations();
        let deferral = PdfParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default());
        assert_eq!(deferral, None);
        let (xml, _) = native_xml(&input);
        let old = xml.find("Den gamle udgave af filen.").expect(&xml);
        let new = xml.find("Den nye udgave af filen.").expect(&xml);
        assert!(old < new, "{xml}");
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_the_files_of_both_generations_of_an_object_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-generations");
        let path = scratch.write("generations.pdf", &two_generations());
        assert_same_as_tika_delegating(&[&path], tika_name(), &DELEGATES);
    }

    #[test]
    fn defers_a_needed_object_the_port_cannot_read() {
        let mut objects = page_objects(TEXT);
        objects[3] = format!("<< /Length 5 /X 4294967296 0 R >>\nstream\n{TEXT}\nendstream");
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        assert_eq!(
            PdfParser
                .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some(
                "object 4 0, which the port cannot read: a reference to object 4294967296 0, \
                 which lopdf cannot hold"
            )
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    fn reads_objects_as_pdfbox_reads_them_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-objects");
        let mut hex = page_objects(TEXT);
        hex[3] = format!("<< /Length 5 /X <zz> >>\nstream\n{TEXT}\nendstream");
        let mut unmeasured = page_objects(TEXT);
        unmeasured[3] = format!("<< /Length 9 0 R >>\nstream\n{TEXT}\nendstream");
        let mut missing = page_objects(TEXT);
        missing[3] = format!("<< /Length 99 >>\nstream\n{TEXT}\r\nendstream");
        let paths: Vec<PathBuf> = [
            ("hex.pdf", hex),
            ("unmeasured.pdf", unmeasured),
            ("missing.pdf", missing),
        ]
        .into_iter()
        .map(|(name, objects)| {
            let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
            scratch.write(name, &document(&objects, b"", &[]))
        })
        .collect();
        for path in &paths {
            assert_eq!(deferral(path), None, "{}", path.display());
        }
        assert_same_as_tika_delegating(&as_paths(&paths), tika_name(), &DELEGATES);
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

    #[cfg(feature = "tika")]
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
            matches!(prepared, Err(Decline::Fails(Error::MemoryLimit(1)))),
            "{prepared:?}"
        );
    }

    fn with_pdf(pdf: PdfParserConfig) -> ParserConfigs {
        ParserConfigs {
            pdf,
            ..ParserConfigs::default()
        }
    }

    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
    #[test]
    fn reads_pages_by_ocr_under_the_ocr_strategies_as_tikas_path_does() {
        assert!(ocr_runs(), "tesseract is not installed");
        let scratch = Scratch::new("pdf-ocr-strategies");
        let mut documents =
            corpus_documents(&["eng-ocr.pdf", "danish.pdf", "notes.pdf", "table.pdf"]);
        let embedding = scratch.write("embedding.pdf", &embedding("", ""));
        documents.push(embedding);
        for strategy in [
            PdfOcrStrategy::OCR_ONLY,
            PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION,
        ] {
            let configs = with_pdf(PdfParserConfig::new().set_ocr_strategy(strategy));
            for document in &documents {
                let input = std::fs::read(document).unwrap();
                let deferral = PdfParser.defers(&input, &TikaMetadata::new(), &configs);
                assert_eq!(deferral, None, "{} under {strategy}", document.display());
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
        let pdf = PdfParserConfig::new().set_ocr_strategy_auto("faster , 4");
        let deferral = PdfParser
            .defers(&input, &TikaMetadata::new(), &with_pdf(pdf))
            .unwrap_or_default();
        assert!(
            deferral.starts_with("the automatic OCR thresholds \"faster , 4\""),
            "{deferral}"
        );
    }

    /// Asserts that each encrypted document, named with the password that opens it, fails as
    /// Tika's path fails on it without a password and with a wrong one, and extracts with its
    /// password as Tika's path does.
    #[cfg(feature = "tika")]
    fn assert_opens_as_tika(documents: &[(PathBuf, &str)]) {
        for (document, password) in documents {
            let configs = ParserConfigs::default().with_password(Some(&Password::new(*password)));
            let input = std::fs::read(document).unwrap();
            assert_eq!(
                PdfParser.defers(&input, &TikaMetadata::new(), &configs),
                None,
                "{}",
                document.display()
            );
            assert_same_as_tika_delegating_with(&[document], tika_name(), &DELEGATES, &configs);
            let wrong = ParserConfigs::default().with_password(Some(&Password::new("wrong")));
            for configs in [ParserConfigs::default(), wrong] {
                assert_eq!(
                    PdfParser.defers(&input, &TikaMetadata::new(), &configs),
                    None,
                    "{}",
                    document.display()
                );
                let (native, tika) = crate::formats::parity::errors(document, &configs);
                assert_eq!(native, tika, "{}", document.display());
            }
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    fn opens_documents_with_the_password_as_tikas_path_does() {
        let document = PathBuf::from("../test_files/documents/encrypted/user-password.pdf");
        assert_opens_as_tika(&[(document, "extractous")]);
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn opens_the_conformance_documents_with_their_passwords_as_tikas_path_does() {
        let (parsed, deferred) = documents(&PdfParser, &[&conformance_corpus()]);
        let owner_user: Vec<PathBuf> = parsed
            .into_iter()
            .chain(deferred)
            .filter(|path| path.to_string_lossy().ends_with("_owner_user.pdf"))
            .collect();
        assert_eq!(owner_user.len(), 2, "{owner_user:?}");
        let documents: Vec<(PathBuf, &str)> = owner_user
            .iter()
            .flat_map(|path| [(path.clone(), "user"), (path.clone(), "owner")])
            .collect();
        assert_opens_as_tika(&documents);
    }

    /// How many tests the documents of the corpora are compared in, each in time.
    #[cfg(feature = "tika")]
    const PARTS: usize = 4;

    /// The documents of the corpora the native parser extracts, by size, dealt in turn to
    /// [`PARTS`] parts of about the same size: those of `part`.
    #[cfg(feature = "tika")]
    fn part_of_the_corpora(part: usize) -> Vec<PathBuf> {
        let (mut parsed, _) = documents(&PdfParser, &corpora());
        parsed.sort_by_key(|path| (std::fs::metadata(path).unwrap().len(), path.clone()));
        parsed
            .into_iter()
            .enumerate()
            .filter(|(index, _)| index % PARTS == part)
            .map(|(_, path)| path)
            .collect()
    }

    /// Compares the documents of `part` of the corpora, and the locked one in the first part:
    /// it fails without its password, as on Tika's path.
    #[cfg(feature = "tika")]
    fn extracts_part_of_what_tikas_path_extracts(part: usize) {
        let paths = part_of_the_corpora(part);
        let (locked, open): (Vec<&Path>, Vec<&Path>) = as_paths(&paths)
            .into_iter()
            .partition(|path| path.ends_with("encrypted/user-password.pdf"));
        crate::formats::parity::assert_fails_as_tika(&locked);
        assert_same_as_tika_delegating(&open, tika_name(), &DELEGATES);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn deals_every_document_of_the_corpora_to_one_part() {
        let (mut parsed, _) = documents(&PdfParser, &corpora());
        let mut dealt: Vec<PathBuf> = (0..PARTS).flat_map(part_of_the_corpora).collect();
        parsed.sort();
        dealt.sort();
        assert_eq!(dealt, parsed);
        let locked = parsed
            .iter()
            .filter(|path| path.ends_with("encrypted/user-password.pdf"))
            .count();
        assert_eq!(locked, 1);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_part_0() {
        extracts_part_of_what_tikas_path_extracts(0);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_part_1() {
        extracts_part_of_what_tikas_path_extracts(1);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_part_2() {
        extracts_part_of_what_tikas_path_extracts(2);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_part_3() {
        extracts_part_of_what_tikas_path_extracts(3);
    }

    #[cfg(feature = "tika")]
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
            };
            assert!(as_expected, "{name}: {reason}");
        }
    }

    /// Why the extraction of a PDF that embeds PDFs, while tesseract runs, is not compared with
    /// Tika's path.
    #[cfg(feature = "tika")]
    const NESTED_PAGES: &str = "our TextLayerCheckingPdfParser takes the pages of a PDF a PDF \
                                embeds outside its pages for pages of its own, assessing them \
                                by the embedding document's last page and mangling them";

    /// Asserts that the PDF at `path`, a chain of PDFs each embedding the next, each of one page
    /// showing `a`, is read with every page whole, its text and OCR in it, where Tika's path
    /// mangles the embedded pages ([`NESTED_PAGES`]).
    #[cfg(feature = "tika")]
    fn assert_reads_the_pages_of_embedded_pdfs(path: &Path) {
        let (xml, _) = native_xml(&std::fs::read(path).unwrap());
        let pages = xml.matches("<div class=\"page\">").count();
        let embedded = xml.matches("class=\"embedded\"").count();
        assert_eq!(pages, embedded + 1, "{NESTED_PAGES}: {xml}");
        assert_eq!(
            xml.matches("<p>a</p>").count(),
            pages,
            "{NESTED_PAGES}: {xml}"
        );
        assert_eq!(
            xml.matches("class=\"ocr\"").count(),
            pages,
            "{NESTED_PAGES}: {xml}"
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_the_conformance_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&PdfParser, &[&conformance_corpus()]);
        let reasons: Vec<String> = deferred.iter().map(|path| deferral_of(path)).collect();
        assert!(deferred.is_empty(), "{deferred:?}: {reasons:?}");
        // Tika extracts its Mac, DOS and Unix copies of one file as files of their own (#109).
        let (copies, paths): (Vec<&Path>, Vec<&Path>) = as_paths(&parsed)
            .into_iter()
            .partition(|path| path.ends_with("testPDF_multiFormatEmbFiles.pdf"));
        let (locked, paths): (Vec<&Path>, Vec<&Path>) = paths
            .into_iter()
            .partition(|path| path.to_string_lossy().ends_with("_owner_user.pdf"));
        assert_eq!(locked.len(), 2, "{locked:?}");
        crate::formats::parity::assert_fails_as_tika(&locked);
        let (nested, paths): (Vec<&Path>, Vec<&Path>) = paths
            .into_iter()
            .partition(|path| path.ends_with("testPDF_deeplyEmbeddedAttachments.pdf"));
        assert_eq!(nested.len(), usize::from(ocr_runs()), "{nested:?}");
        for path in nested {
            assert_reads_the_pages_of_embedded_pdfs(path);
        }
        let (malformed, paths): (Vec<&Path>, Vec<&Path>) = paths
            .into_iter()
            .partition(|path| path.ends_with(MALFORMED_XFA));
        assert_eq!(malformed.len(), 1, "{malformed:?}");
        assert_same_as_tika_delegating_departing_with(
            &malformed,
            tika_name(),
            &DELEGATES,
            XFA_FAILURE,
            &ParserConfigs::default(),
        );
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
