use crate::errors::{Error, ExtractResult};
use crate::tika::native;

/// The version of the tesseract OCR runs, as `tesseract --version` reports it, since OCR output
/// depends on it: the one the build bundled (`EXTRACTOUS_OCR_BUNDLE`), linked into the static
/// executable or unpacked on first use, or `tesseract` on `PATH`.
pub fn tesseract_version() -> ExtractResult<String> {
    native::tesseract_version().map_err(Error::IoError)
}

/// The licenses of the tesseract and the language data the build bundled, or None when it
/// bundled none.
pub fn bundled_ocr_licenses() -> Option<&'static str> {
    native::bundled_ocr_licenses()
}
