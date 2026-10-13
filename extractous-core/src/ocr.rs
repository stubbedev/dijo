use std::path::PathBuf;

use crate::errors::{Error, ExtractResult};
use crate::tika::native;

/// The tesseract executable OCR runs: the one the build bundled (`EXTRACTOUS_OCR_BUNDLE`),
/// unpacked on first use, or `tesseract` on `PATH`.
pub fn tesseract_program() -> ExtractResult<PathBuf> {
    Ok(native::bundled_tesseract()
        .map_err(Error::NativeLibrary)?
        .unwrap_or_else(|| PathBuf::from("tesseract")))
}

/// The licenses of the tesseract and the language data the build bundled, or None when it
/// bundled none.
pub fn bundled_ocr_licenses() -> Option<&'static str> {
    native::bundled_ocr_licenses()
}
