use extractous_core::{
    Extractor, OfficeParserConfig, Password, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig,
};

use std::time::Duration;

use crate::cli::{Format, Ocr};

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
    let mut ocr_config = TesseractOcrConfig::new().set_skip_ocr(skip_ocr);
    if let Some(languages) = ocr_lang {
        ocr_config = ocr_config.set_language(languages);
    }
    let extractor = Extractor::new()
        .set_passwords(passwords)
        .set_xml_output(matches!(
            format,
            Format::Xhtml | Format::Pages | Format::Elements
        ))
        .set_pdf_config(PdfParserConfig::new().set_ocr_strategy(strategy))
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
