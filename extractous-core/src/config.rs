use strum_macros::{Display, EnumString};

/// OCR Strategy for PDF parsing
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString)]
#[allow(non_camel_case_types)]
pub enum PdfOcrStrategy {
    NO_OCR,
    OCR_ONLY,
    OCR_AND_TEXT_EXTRACTION,
    #[default]
    AUTO,
}

pub(crate) enum JavaValue {
    Bool(bool),
    Int(i32),
    Str(String),
}

pub(crate) trait IntoJavaValue {
    fn into_java_value(self) -> JavaValue;
}

impl IntoJavaValue for bool {
    fn into_java_value(self) -> JavaValue {
        JavaValue::Bool(self)
    }
}

impl IntoJavaValue for i32 {
    fn into_java_value(self) -> JavaValue {
        JavaValue::Int(self)
    }
}

impl IntoJavaValue for String {
    fn into_java_value(self) -> JavaValue {
        JavaValue::Str(self)
    }
}

impl IntoJavaValue for PdfOcrStrategy {
    fn into_java_value(self) -> JavaValue {
        JavaValue::Str(self.to_string())
    }
}

macro_rules! tika_config {
    (
        $(#[$meta:meta])*
        $name:ident => $java_class:literal {
            $(
                $(#[$doc:meta])*
                $setter:ident($field:ident: $ty:ty) => $java_setter:literal,
            )*
        }
    ) => {
        $(#[$meta])*
        ///
        /// Every setting left unset keeps the default of Apache Tika's
        #[doc = concat!("`", $java_class, "`.")]
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct $name {
            $($field: Option<$ty>,)*
        }

        impl $name {
            /// Creates a configuration that keeps all of Apache Tika's defaults.
            pub fn new() -> Self {
                Self::default()
            }

            $(
                $(#[$doc])*
                pub fn $setter(mut self, val: impl Into<$ty>) -> Self {
                    self.$field = Some(val.into());
                    self
                }
            )*

            pub(crate) const JAVA_CLASS: &'static str = $java_class;

            pub(crate) fn java_settings(&self) -> impl Iterator<Item = (&'static str, JavaValue)> {
                [$(
                    self.$field
                        .clone()
                        .map(|val| ($java_setter, val.into_java_value())),
                )*]
                .into_iter()
                .flatten()
            }
        }
    };
}

tika_config! {
    /// PDF parsing configuration settings
    PdfParserConfig => "ai/yobix/PdfConfig" {
        /// Sets the OCR strategy for PDF parsing.
        set_ocr_strategy(ocr_strategy: PdfOcrStrategy) => "setOcrStrategy",

        /// Which pages the AUTO strategy OCRs: `better` (Tika's default: more than 10 glyphs
        /// without Unicode text, or at most 10 characters), `faster`, or
        /// `<unmapped>[%][,<characters>]`: more than `unmapped` such glyphs (a share of the
        /// page's glyphs with `%`), or at most `characters` characters.
        set_ocr_strategy_auto(ocr_strategy_auto: String) => "setOcrStrategyAuto",

        /// Whether an untagged PDF page is read in reading order: columns left to right (right to
        /// left on a page of right-to-left text), a title before the columns it spans, a block
        /// above the blocks below it, rather than in the order its content stream draws the
        /// text. Not a Tika setting: unless set, pages are read in reading order.
        set_reading_order(reading_order: bool) => "setReadingOrder",

        /// If true, extract the literal inline embedded OBXImages. Beware: some PDF documents of
        /// modest size (~4MB) can contain thousands of embedded images totaling > 2.5 GB.
        /// Also, there can be surprisingly large memory consumption
        /// and/ or out of memory errors. Along the same lines, note that this does not extract
        /// "logical" images. Some PDF writers break up a single logical image into hundreds of
        /// little images. With this option set to true, you might get those hundreds of little images.
        /// NOTE ALSO: this extracts the raw images without clipping, rotation, masks, color inversion,
        /// etc. The images that this extracts may look nothing like what a human would expect given
        /// the appearance of the PDF. Set to true only with the greatest caution.
        set_extract_inline_images(extract_inline_images: bool) => "setExtractInlineImages",

        /// Multiple pages within a PDF file might refer to the same underlying image.
        /// If false, the parser will call the EmbeddedExtractor each time the image appears on a
        /// page. This might be desired for some use cases. However, to avoid duplication of
        /// extracted images, set this to true.
        /// Note that uniqueness is determined only by the underlying PDF COSObject id, not by file hash
        /// or similar equality metric. If the PDF actually contains multiple copies of the same
        /// image -- all with different object ids -- then all images will be extracted.
        /// For this parameter to have any effect, extract_inline_images must be set to true.
        set_extract_unique_inline_images_only(extract_unique_inline_images_only: bool) => "setExtractUniqueInlineImagesOnly",

        /// If the PDF contains marked content, try to extract text and its marked structure.
        set_extract_marked_content(extract_marked_content: bool) => "setExtractMarkedContent",

        /// If true, the text of the PDF's bookmarks (its outline) follows the text of its pages.
        set_extract_bookmarks_text(extract_bookmarks_text: bool) => "setExtractBookmarksText",

        /// If the PDF contains annotations, try to extract the text of the annotations.
        set_extract_annotation_text(extract_annotation_text: bool) => "setExtractAnnotationText",
    }
}

tika_config! {
    /// Microsoft Office parser configuration settings
    OfficeParserConfig => "org/apache/tika/parser/microsoft/OfficeParserConfig" {
        /// Sets whether MSOffice parsers should extract macros.
        set_extract_macros(extract_macros: bool) => "setExtractMacros",

        /// Sets whether the docx parser should include deleted content.
        set_include_deleted_content(include_deleted_content: bool) => "setIncludeDeletedContent",

        /// With track changes on for the docx parser, when a section is moved, the content is stored in
        /// both the "moveFrom" section and in the "moveTo" section. If you'd like to include the
        /// section both in its original location (moveFrom) and in its new location (moveTo),
        /// set this to true.
        set_include_move_from_content(include_move_from_content: bool) => "setIncludeMoveFromContent",

        /// In Excel and Word, there can be text stored within drawing shapes.
        /// (In PowerPoint everything is in a Shape) If you'd like to skip processing these to look
        /// for text, set this to false.
        set_include_shape_based_content(include_shape_based_content: bool) => "setIncludeShapeBasedContent",

        /// Whether to include headers and footers. This only operates on headers and footers in
        /// Word and Excel, not master slide content in PowerPoint.
        set_include_headers_and_footers(include_headers_and_footers: bool) => "setIncludeHeadersAndFooters",

        /// For table-like formats, and tables within other formats, should missing rows in sparse
        /// tables be output where detected? Tika's default only outputs rows defined within the
        /// file, which avoids lots of blank lines, but means layout isn't preserved.
        set_include_missing_rows(include_missing_rows: bool) => "setIncludeMissingRows",

        /// Whether to process slide notes content. If set to false, the parser will skip the text
        /// content and all embedded objects from the slide notes in ppt and pptxm.
        set_include_slide_notes(include_slide_notes: bool) => "setIncludeSlideNotes",

        /// Whether to include contents from any of the three types of masters -- slide, notes,
        /// handout -- in a .ppt or pptxm file. If set to false, the parser will not extract text
        /// or embedded objects from any of the masters.
        set_include_slide_master_content(include_slide_master_content: bool) => "setIncludeSlideMasterContent",

        /// Microsoft Excel files can sometimes contain phonetic (furigana) strings.
        /// This sets whether the parser will concatenate the phonetic runs to the original text.
        /// This is currently only supported by the xls and xlsx parsers (not the xlsb parser).
        set_concatenate_phonetic_runs(concatenate_phonetic_runs: bool) => "setConcatenatePhoneticRuns",
    }
}

/// The settings native parsers read, unset ones as Tika's `OfficeParserConfig` defaults them.
impl OfficeParserConfig {
    /// Whether deleted content is extracted, which Tika leaves out unless asked.
    pub(crate) fn includes_deleted_content(&self) -> bool {
        self.include_deleted_content.unwrap_or(false)
    }

    /// Whether macros are extracted, which Tika leaves out unless asked.
    pub(crate) fn extracts_macros(&self) -> bool {
        self.extract_macros.unwrap_or(false)
    }

    /// Whether text moved away is extracted where it was too, which Tika leaves out unless asked.
    pub(crate) fn includes_move_from_content(&self) -> bool {
        self.include_move_from_content.unwrap_or(false)
    }

    /// Whether the text of shapes such as text boxes is extracted, as Tika does unless told not to.
    pub(crate) fn includes_shape_based_content(&self) -> bool {
        self.include_shape_based_content.unwrap_or(true)
    }

    /// Whether headers and footers are extracted, as Tika does unless told not to.
    pub(crate) fn includes_headers_and_footers(&self) -> bool {
        self.include_headers_and_footers.unwrap_or(true)
    }

    /// Whether the rows a sparse table leaves out are written empty, which Tika does not do
    /// unless asked.
    pub(crate) fn includes_missing_rows(&self) -> bool {
        self.include_missing_rows.unwrap_or(false)
    }

    /// Whether phonetic guides follow the text they annotate, as Tika has them unless told not to.
    pub(crate) fn concatenates_phonetic_runs(&self) -> bool {
        self.concatenate_phonetic_runs.unwrap_or(true)
    }

    /// Whether the notes of slides are extracted, as Tika does unless told not to.
    pub(crate) fn includes_slide_notes(&self) -> bool {
        self.include_slide_notes.unwrap_or(true)
    }

    /// Whether the text of slide masters and layouts is extracted, as Tika does unless told not
    /// to.
    pub(crate) fn includes_slide_master_content(&self) -> bool {
        self.include_slide_master_content.unwrap_or(true)
    }
}

tika_config! {
    /// Tesseract OCR configuration settings
    TesseractOcrConfig => "ai/yobix/OcrConfig" {
        /// Sets the maximum time in seconds that Tesseract should spend on OCR.
        set_timeout_seconds(timeout_seconds: i32) => "setTimeoutSeconds",

        /// If true, no OCR runs at all, not even on images or on PDFs with an OCR strategy.
        set_skip_ocr(skip_ocr: bool) => "setSkipOcr",

        /// Sets the tesseract language dictionary to be used for OCR.
        /// Languages are nominally an [ISO-639-2 codes](https://en.wikipedia.org/wiki/List_of_ISO_639-2_codes).
        /// Multiple languages may be specified, separated by plus characters. e.g.
        /// "chi_tra+chi_sim+script/Arabic". [`TesseractOcrConfig::AUTO_LANGUAGE`] detects the
        /// script of each image with tesseract's orientation and script detection, which needs
        /// the `osd` model, and reads it with an installed model for that script.
        set_language(language: String) => "setLanguage",
    }
}

impl TesseractOcrConfig {
    /// The language that makes OCR detect each image's script and pick a model for it.
    pub const AUTO_LANGUAGE: &'static str = "auto";
}

/// A password for encrypted documents. It never appears in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub fn new(password: impl Into<String>) -> Self {
        Self(password.into())
    }

    /// The password itself, for handing it on, such as to a worker process. It never appears in
    /// Debug output.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ParserConfigs {
    pub(crate) pdf: PdfParserConfig,
    pub(crate) office: OfficeParserConfig,
    pub(crate) ocr: TesseractOcrConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setters(settings: impl Iterator<Item = (&'static str, JavaValue)>) -> Vec<&'static str> {
        settings.map(|(setter, _)| setter).collect()
    }

    #[test]
    fn unset_settings_keep_tika_defaults() {
        assert!(setters(PdfParserConfig::new().java_settings()).is_empty());
        assert!(setters(OfficeParserConfig::new().java_settings()).is_empty());
        assert!(setters(TesseractOcrConfig::new().java_settings()).is_empty());
    }

    #[test]
    fn set_settings_reach_their_java_setters() {
        let settings: Vec<_> = PdfParserConfig::new()
            .set_extract_unique_inline_images_only(false)
            .set_ocr_strategy(PdfOcrStrategy::OCR_ONLY)
            .java_settings()
            .collect();

        assert!(matches!(
            settings.as_slice(),
            [
                ("setOcrStrategy", JavaValue::Str(strategy)),
                ("setExtractUniqueInlineImagesOnly", JavaValue::Bool(false)),
            ] if strategy == "OCR_ONLY"
        ));
    }

    #[test]
    fn passwords_are_redacted_from_debug_output() {
        assert_eq!(
            format!("{:?}", Password::new("hunter2")),
            "Password(<redacted>)"
        );
    }

    #[test]
    fn language_accepts_str() {
        let settings: Vec<_> = TesseractOcrConfig::new()
            .set_language("deu")
            .java_settings()
            .collect();

        assert!(matches!(
            settings.as_slice(),
            [("setLanguage", JavaValue::Str(language))] if language == "deu"
        ));
    }
}
