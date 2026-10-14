use crate::tika::native;

/// The licenses of the tesseract and the language data the build bundled, or None when it
/// bundled none.
pub fn bundled_ocr_licenses() -> Option<&'static str> {
    native::bundled_ocr_licenses()
}

/// The licenses of the native libraries linked into the static executable beyond tesseract's:
/// those of AWT's fonts, libheif with its decoders and DjVuLibre, or None in a build that links
/// none.
pub fn linked_native_licenses() -> Option<&'static str> {
    native::linked_native_licenses()
}
