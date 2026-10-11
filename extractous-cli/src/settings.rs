use extractous_core::{Extractor, Password, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig};

use crate::cli::{Format, Ocr};

/// Builds the extractor for one set of extraction options, shared by the command line and the
/// server.
pub fn extractor(
    format: Format,
    ocr: Ocr,
    ocr_lang: Option<&str>,
    passwords: Vec<Password>,
) -> Extractor {
    let (strategy, skip_ocr) = match ocr {
        Ocr::Auto => (PdfOcrStrategy::AUTO, false),
        Ocr::Never => (PdfOcrStrategy::NO_OCR, true),
        Ocr::Always => (PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION, false),
    };
    let mut ocr_config = TesseractOcrConfig::new().set_skip_ocr(skip_ocr);
    if let Some(languages) = ocr_lang {
        ocr_config = ocr_config.set_language(languages);
    }
    Extractor::new()
        .set_passwords(passwords)
        .set_xml_output(matches!(
            format,
            Format::Xhtml | Format::Pages | Format::Elements
        ))
        .set_pdf_config(PdfParserConfig::new().set_ocr_strategy(strategy))
        .set_ocr_config(ocr_config)
}
