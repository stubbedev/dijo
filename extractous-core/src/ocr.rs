use std::sync::OnceLock;

use crate::errors::{Error, ExtractResult};
use crate::tika::native;

/// The version of the tesseract OCR runs, since OCR output depends on it: the one the build
/// bundled (`EXTRACTOUS_OCR_BUNDLE`), linked into the static executable or loaded from the
/// bundle's library, or `tesseract` on `PATH` as `tesseract --version` reports it.
pub fn tesseract_version() -> ExtractResult<String> {
    native::tesseract_version().map_err(Error::IoError)
}

/// Whether tesseract runs, found once per process as `TikaNativeMain` finds it once: the bundled
/// one, or one on `PATH` that answers `--version`. With one, Tika reads images and PDF pages with
/// our OCR parsers.
pub(crate) fn tesseract_runs() -> bool {
    static RUNS: OnceLock<bool> = OnceLock::new();
    *RUNS.get_or_init(|| tesseract_version().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tesseract_ocr_runs_reports_its_version() {
        let version = tesseract_version().unwrap();
        assert!(version.starts_with("tesseract 5."), "{version}");
    }
}
