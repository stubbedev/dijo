//! PDF documents, as Tika's `PDFParser` reads them with PDFBox, under extractous's
//! `TextLayerCheckingPdfParser` when tesseract runs: the document's objects through `lopdf`,
//! then the metadata `PDFParser` collects, and the text of each page as PDFBox's
//! `PDFTextStripper` assembles it from the glyphs its content stream shows, written by
//! `PDF2XHTML` in our reading order, or for a tagged document by `PDFMarkedContent2XHTML`
//! through its structure tree. Documents with a page Tika would OCR, and documents with what the
//! port does not do yet (encryption, form fields, embedded files and fonts it cannot measure,
//! among others), are left to Tika.

mod afm;
mod cmap;
mod content;
mod cos;
mod date;
mod document;
mod encoding;
mod engine;
mod font;
mod jfloat;
mod matrix;
mod metadata;
mod position;
mod quality;
mod reading_order;
mod stripper;
mod structure;
mod tagged;
mod truetype;
mod writer;
mod xmp;

use std::sync::OnceLock;

use lopdf::Document;

use crate::config::ParserConfigs;
use crate::formats::handlers::Discard;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};

use cos::{as_dict, Pdf};
use structure::StructureIndex;
use tagged::Tagged;
use writer::{DocumentWriter, PageLayer, WriteFailure};

const MEDIA_TYPE: &str = "application/pdf";
const TEXT_LAYER_CHECKING_PARSER: &str = "extractous::TextLayerCheckingPdfParser";
const PDF_PARSER: &str = "extractous::PDFParser";
/// `PagesConfig.Auto`'s defaults: a page of at most this many glyphs is OCRed...
const OCR_MAX_GLYPHS: i32 = 10;
/// ...and so is one with more glyphs than this that map to no character.
const OCR_UNMAPPED_LIMIT: f32 = 10.0;

/// Why the port leaves a document to Tika: something in it the port does not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unported(pub(super) String);

/// Whether tesseract runs, which makes Tika read PDFs through our `TextLayerCheckingPdfParser`.
fn ocr_runs() -> bool {
    static RUNS: OnceLock<bool> = OnceLock::new();
    *RUNS.get_or_init(|| crate::ocr::tesseract_version().is_ok())
}

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

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        configs: &ParserConfigs,
    ) -> Option<String> {
        deferral(input, configs)
            .err()
            .map(|Unported(reason)| reason)
    }

    fn parse(
        &self,
        input: &[u8],
        configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        if ocr_runs() {
            xhtml.delegate_to(PDF_PARSER);
        }
        let pdf = load(input).map_err(|Unported(reason)| {
            Failure::Malformed(format!("the PDF was not deferred, though {reason}"))
        })?;
        extract(&pdf, input, configs, xhtml, false)?;
        Ok(())
    }
}

/// Why the document is left to Tika, if it is: a setting the port does not read, something in
/// the document it does not do, or a page Tika would OCR.
fn deferral(input: &[u8], configs: &ParserConfigs) -> Result<(), Unported> {
    if let Some(setting) = configs.pdf.unread_setting() {
        return Err(Unported(format!(
            "the PDF setting {setting}, which the port does not read"
        )));
    }
    let pdf = load(input)?;
    let mut layers = Vec::new();
    let (_, _, result) = Xhtml::write(&mut Discard, TikaMetadata::new(), configs, |xhtml| {
        layers = extract(&pdf, input, configs, xhtml, true)?;
        Ok(())
    });
    match result {
        Ok(()) => {}
        Err(WriteFailure::Unported(unported)) => return Err(unported),
        Err(WriteFailure::Stop(stop)) => {
            unreachable!("a discarded document stopped being written: {stop:?}")
        }
    }
    for (index, layer) in layers.iter().enumerate() {
        ocr_deferral(index + 1, layer)?;
    }
    Ok(())
}

/// Why Tika would OCR a page, if it would: `AbstractPDF2XHTML.ocrWanted` under its automatic
/// strategy, and our `TextLayerCheckingPdfParser`'s doubt about the page's text when tesseract
/// runs. A page of more glyphs than the limit without characters is both.
fn ocr_deferral(page: usize, layer: &PageLayer) -> Result<(), Unported> {
    if layer.glyphs <= OCR_MAX_GLYPHS || layer.unmapped as f32 > OCR_UNMAPPED_LIMIT {
        return Err(Unported(format!(
            "page {page} shows {} glyphs, {} of them without characters, which Tika OCRs",
            layer.glyphs, layer.unmapped
        )));
    }
    if !ocr_runs() {
        return Ok(());
    }
    let assessment = quality::assess(
        &layer.text,
        layer.glyphs,
        layer.unmapped,
        OCR_UNMAPPED_LIMIT,
    );
    if assessment.badness >= quality::DOUBTFUL {
        return Err(Unported(format!(
            "the text layer of page {page} is doubtful ({}), which Tika OCRs",
            assessment.fault
        )));
    }
    Ok(())
}

/// The document's objects, as `lopdf` reads them; an encrypted document is left to Tika.
fn load(input: &[u8]) -> Result<Pdf, Unported> {
    let doc = Document::load_mem(input)
        .map_err(|error| Unported(format!("lopdf cannot read the document: {error}")))?;
    if doc.was_encrypted() || doc.is_encrypted() {
        return Err(Unported(
            "encryption, whose permissions the port does not read".to_owned(),
        ));
    }
    Ok(Pdf { doc })
}

/// `PDFParser.parse` from the cross-reference scan on: the metadata, then the pages and the
/// end of the document, written by the tagged writer for a document with marked content. Each
/// page's text layer is returned when `record_layers` asks for it.
fn extract(
    pdf: &Pdf,
    input: &[u8],
    configs: &ParserConfigs,
    xhtml: &mut Xhtml,
    record_layers: bool,
) -> Result<Vec<PageLayer>, WriteFailure> {
    let metadata = xhtml.metadata_mut();
    let mut startxrefs = 0;
    for offset in metadata::scan_startxrefs(input) {
        if offset.startxref == 0 {
            continue;
        }
        startxrefs += 1;
        metadata.add("pdf:eof-offsets", offset.end_eof_offset.to_string());
    }
    let updates = (startxrefs - 1).max(0).to_string();
    metadata.set("pdf:incremental-update-count", updates.clone());
    metadata.set("tk:version-count", updates);
    let catalog = pdf.catalog().ok_or_else(|| {
        Unported("a document without a catalog, which PDFBox fails on".to_owned())
    })?;
    metadata.set("pdf:has-collection", catalog.has(b"Collection").to_string());
    let acro_form = pdf.dict(catalog, b"AcroForm");
    let has_xfa = acro_form.is_some_and(|form| form.has(b"XFA"));
    metadata.set("pdf:has-xfa", has_xfa.to_string());
    let has_marked_content = StructureIndex::<()>::has_marked_content(pdf);
    metadata.set("pdf:has-marked-content", has_marked_content.to_string());
    metadata.set("Content-Type", MEDIA_TYPE);
    metadata::access_permissions(metadata);
    metadata.set("pdf:encrypted", "false");
    let has_fields = acro_form
        .and_then(|form| pdf.array(form, b"Fields"))
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|field| pdf.resolve(field).and_then(as_dict).is_some())
        });
    if has_fields {
        return Err(Unported(
            "form fields, whose values and signatures the port does not read".to_owned(),
        )
        .into());
    }
    let version = metadata::header_version(input)?;
    metadata::document(pdf, metadata, version, has_fields)?;
    if is_illustrator(pdf)? {
        metadata.set("Content-Type", "application/illustrator");
    }
    let reading_order = configs.pdf.reads_in_reading_order();
    let tagged = has_marked_content.then(|| Tagged::new(pdf, reading_order));
    let mut writer =
        DocumentWriter::new(pdf, xhtml, tagged, reading_order, ocr_runs(), record_layers);
    let written = writer.write();
    let layers = writer.layers.take().unwrap_or_default();
    xhtml.metadata_mut().set("pdf:ocr-page-count", "0");
    written.map(|()| layers)
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
    use crate::formats::parity::{assert_same_as_tika_delegating, conformance_corpus, documents};
    use std::path::{Path, PathBuf};

    const TIKA_PDF_PARSER: &str = "org.apache.tika.parser.pdf.PDFParser";
    const TIKA_TEXT_LAYER_CHECKING_PARSER: &str = "ai.yobix.TextLayerCheckingPdfParser";
    const DELEGATES: [(&str, &str); 1] = [(PDF_PARSER, TIKA_PDF_PARSER)];
    const CORPORA: [&str; 2] = ["../quality/corpus", "../test_files/documents"];

    /// Why a document of the corpora is left to Tika.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Why {
        /// Tika OCRs a page whatever its text layer reads.
        Ocr,
        /// Our `TextLayerCheckingPdfParser` doubts a page's text layer, when tesseract runs.
        Doubtful,
        /// The port does not do what the document needs, as the reason starts.
        Unported(&'static str),
    }

    /// The documents of the corpora the port leaves to Tika, and why.
    const DEFERRED: [(&str, Why); 15] = [
        ("2022_Q3_AAPL.pdf", Why::Unported("encryption")),
        ("arabic.pdf", Why::Ocr),
        (
            "chinese.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        ("deu-ocr.pdf", Why::Ocr),
        ("eng-ocr.pdf", Why::Ocr),
        ("hindi.pdf", Why::Ocr),
        (
            "hyphenated.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        (
            "japanese-vertical.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        (
            "japanese.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        ("owner-password-only.pdf", Why::Unported("encryption")),
        ("prose-broken-offset.pdf", Why::Doubtful),
        (
            "prose-broken-private-use.pdf",
            Why::Unported("a font PDFBox measures with its fallback"),
        ),
        ("prose-scan.pdf", Why::Ocr),
        ("thai.pdf", Why::Ocr),
        ("user-password.pdf", Why::Unported("encryption")),
    ];

    fn tika_name() -> &'static str {
        if ocr_runs() {
            TIKA_TEXT_LAYER_CHECKING_PARSER
        } else {
            TIKA_PDF_PARSER
        }
    }

    fn as_paths(paths: &[PathBuf]) -> Vec<&Path> {
        paths.iter().map(PathBuf::as_path).collect()
    }

    fn corpora() -> Vec<&'static Path> {
        CORPORA.iter().map(Path::new).collect()
    }

    fn deferral_of(path: &Path) -> String {
        let input = std::fs::read(path).unwrap();
        PdfParser
            .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
            .unwrap_or_else(|| panic!("{} is not deferred", path.display()))
    }

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let (parsed, _) = documents(&PdfParser, &corpora());
        assert_same_as_tika_delegating(&as_paths(&parsed), tika_name(), &DELEGATES);
    }

    #[test]
    fn defers_what_tika_ocrs_and_what_the_port_does_not_do() {
        let (_, deferred) = documents(&PdfParser, &corpora());
        let mut found: Vec<(String, String)> = deferred
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_str().unwrap().to_owned();
                (name, deferral_of(path))
            })
            .collect();
        found.sort();
        let expected: Vec<(&str, Why)> = DEFERRED
            .iter()
            .copied()
            .filter(|(_, why)| ocr_runs() || *why != Why::Doubtful)
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
                Why::Ocr => reason.starts_with("page ") && reason.ends_with("which Tika OCRs"),
                Why::Doubtful => reason.contains("is doubtful"),
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
        assert_same_as_tika_delegating(&as_paths(&parsed), tika_name(), &DELEGATES);
    }
}
