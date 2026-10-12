use extractous_core::{
    Extractor, OfficeParserConfig, Password, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig,
};

use std::time::Duration;

use crate::cli::{Format, Ocr};

/// Under `--ocr auto`, a PDF page is OCRed when it has at most 10 characters (a scan), and its text
/// replaced by OCR text when more than half of its glyphs map to no text or the text it yields is
/// otherwise garbage. Tika's default counts unmapped glyphs absolutely,
/// more than 10 per page, which OCRs every page of an Arabic, Devanagari or Thai PDF: their
/// fonts map a whole cluster to its first glyph, so about a quarter of the glyphs map to nothing
/// even when the text is intact.
const AUTO_OCR_PAGES: &str = "50%,10";

/// Builds the extractor for one set of extraction options, shared by the command line and the
/// server.
pub fn extractor(
    format: Format,
    ocr: Ocr,
    ocr_lang: Option<&str>,
    passwords: Vec<Password>,
    timeout: Option<Duration>,
) -> Extractor {
    let (strategy, skip_ocr) = match ocr {
        Ocr::Auto => (PdfOcrStrategy::AUTO, false),
        Ocr::Never => (PdfOcrStrategy::NO_OCR, true),
        Ocr::Always => (PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION, false),
    };
    let ocr_config = TesseractOcrConfig::new()
        .set_skip_ocr(skip_ocr)
        .set_language(ocr_lang.unwrap_or(TesseractOcrConfig::AUTO_LANGUAGE));
    let extractor = Extractor::new()
        .set_passwords(passwords)
        .set_xml_output(matches!(
            format,
            Format::Xhtml | Format::Pages | Format::Elements
        ))
        .set_pdf_config(
            PdfParserConfig::new()
                .set_ocr_strategy(strategy)
                .set_ocr_strategy_auto(AUTO_OCR_PAGES),
        )
        .set_office_config(
            OfficeParserConfig::new()
                .set_use_sax_docx_extractor(true)
                .set_use_sax_pptx_extractor(true),
        )
        .set_ocr_config(ocr_config);
    match timeout {
        Some(timeout) => extractor.set_timeout(timeout),
        None => extractor,
    }
}
