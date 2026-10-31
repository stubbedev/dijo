//! The OCR of a PDF's pages. Whether Tika would OCR a page: never under `NO_OCR`; under `AUTO`,
//! with the thresholds `PdfConfig.setOcrStrategyAuto` sets, by our
//! `TextLayerCheckingPdfParser`, which decides page by page from the text the page's text layer
//! yields ([`quality`]): a page with next to no text gets OCR text added, a page whose text layer
//! is garbage has it replaced by OCR text, and a doubtful one is read too and keeps whichever
//! text reads better. A replaced text layer is listed in [`REJECTED_TEXT_LAYER`]. The pages to
//! read are rendered and read side by side while the document is written, each page's output
//! passed on in order.

use std::convert::Infallible;
use std::sync::Arc;

use super::cos::Document;
use super::quality::{self, Assessment, Unmapped};
use crate::config::{ParserConfigs, Password, PdfOcrStrategy, PdfParserConfig};
use crate::errors::Error;
use crate::formats::embedded::{OcrLanguage, EXCEPTION_WARNING};
use crate::formats::image::enricher::write_text;
use crate::formats::image::PREPARING_OCR_PARSER;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Difference, Failure};
use crate::ocr::{
    self, Attempt, Bitmap, Image, LanguageHint, Learning, OcrSettings, Outputs, PdfPages,
    Recognized, Recognizer, Resolution,
};

/// Each page whose text layer was replaced by OCR text, as `page:fault`.
const REJECTED_TEXT_LAYER: &str = "dijo:rejected-text-layer";

/// `PagesConfig.Auto`: a page of at most `total_chars` glyphs is OCRed, and so is one with more
/// glyphs without characters than `unmapped`. Our `PdfConfig` and Tika's `limit < 1` check
/// took a limit below 1 as a share of the page's glyphs and one from 1 as a count, so that
/// `100%` read as one glyph (#115).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Thresholds {
    total_chars: i32,
    unmapped: Unmapped,
}

/// `PagesConfig.Auto.defaults()`, which `better` keeps.
const DEFAULT: Thresholds = Thresholds {
    total_chars: 10,
    unmapped: Unmapped::Count(10.0),
};
/// The unmapped glyphs `faster` takes: a tenth of the page's.
const FASTER_UNMAPPED: Unmapped = Unmapped::Share(0.1);

impl Thresholds {
    /// `PdfConfig.setOcrStrategyAuto`: `better`, `faster` or `unmapped[%][,characters]`, as its
    /// pattern `\s*(?:(better|faster)|(\d{1,3})(%)?)(?:,\s*(\d{1,3}))?\s*` matches the whole
    /// setting, or why the setter throws.
    fn parse(setting: &str) -> Result<Self, RejectedThresholds> {
        let mismatch = || {
            RejectedThresholds(format!(
                "Automatic OCR thresholds are better, faster or unmapped[%][,characters]: \
                 {setting}"
            ))
        };
        let trimmed = setting.trim_matches(is_java_space);
        let (head, total) = match trimmed.split_once(',') {
            Some((head, total)) => (head, Some(total.trim_start_matches(is_java_space))),
            None => (trimmed, None),
        };
        let total_chars = match total {
            Some(total) => digits(total).ok_or_else(mismatch)?,
            None => DEFAULT.total_chars,
        };
        let unmapped = match head {
            "better" => DEFAULT.unmapped,
            "faster" => FASTER_UNMAPPED,
            _ => match head.strip_suffix('%') {
                Some(share) => {
                    let share = digits(share).ok_or_else(mismatch)? as f32;
                    if share > 100.0 {
                        return Err(RejectedThresholds(format!(
                            "The share of unmapped glyphs cannot exceed 100%: {setting}"
                        )));
                    }
                    Unmapped::Share(share / 100.0)
                }
                None => Unmapped::Count(digits(head).ok_or_else(mismatch)? as f32),
            },
        };
        Ok(Self {
            total_chars,
            unmapped,
        })
    }
}

/// The message of the `IllegalArgumentException` our Java's `PdfConfig.setOcrStrategyAuto`
/// throws on thresholds it does not read, which fails every extraction under them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RejectedThresholds(String);

impl RejectedThresholds {
    /// The error Tika fails with, a Java exception our Java throws as it applies the
    /// settings.
    pub(super) fn into_error(self) -> Error {
        let exception = format!("java.lang.IllegalArgumentException: {}", self.0);
        Error::Unknown(format!("Java exception: {exception}"))
    }
}

/// Java's `\s`.
fn is_java_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// `\d{1,3}`, read by `Integer.parseInt`.
fn digits(text: &str) -> Option<i32> {
    if text.is_empty() || text.len() > 3 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// How Tika decides whether to OCR a page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum OcrCheck {
    /// `NO_OCR`: it OCRs no page.
    Never,
    /// As our `TextLayerCheckingPdfParser` decides it from the page's text, holding each page
    /// back until it ends.
    Assessed(Thresholds),
    /// `OCR_ONLY`: `OCR2XHTML` OCRs every page in place of its text.
    Only,
    /// `OCR_AND_TEXT_EXTRACTION`: every page's text, then its OCR text.
    AndText,
}

/// What our `TextLayerCheckingPdfParser` does with a page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum PageCheck {
    /// Its text layer is kept.
    Keep,
    /// It has next to no text: OCR text is added.
    Add,
    /// Its text layer is doubtful or garbage: OCR text replaces it when it reads better, or
    /// whenever the layer is garbage.
    Weigh(Assessment),
    /// Its text, which the files it embeds add to, is assessed once they are parsed, the page of
    /// `glyphs` glyphs, `unmapped` of them without characters, read for a doubtful layer.
    Settle { glyphs: i32, unmapped: i32 },
    /// Tika's PDF parser OCRs the page itself, its rendering read by the OCR parser it hands
    /// it to.
    Recognize,
}

impl OcrCheck {
    /// The check Tika makes under the PDF settings, or the thresholds our Java rejects.
    pub(super) fn of(config: &PdfParserConfig) -> Result<Self, RejectedThresholds> {
        let thresholds = match config.ocr_strategy_auto() {
            None => DEFAULT,
            Some(setting) => Thresholds::parse(setting)?,
        };
        Ok(match config.ocr_strategy() {
            PdfOcrStrategy::NO_OCR => Self::Never,
            PdfOcrStrategy::AUTO => Self::Assessed(thresholds),
            PdfOcrStrategy::OCR_ONLY => Self::Only,
            PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION => Self::AndText,
        })
    }

    /// Whether Tika's PDF parser reads no page's text, only its OCR text.
    pub(super) fn reads_only_ocr(self) -> bool {
        self == Self::Only
    }

    /// Whether Tika's PDF parser dispatches every page to its OCR parser itself, as
    /// `pdf:ocr-page-count` counts them.
    pub(super) fn dispatches_every_page(self) -> bool {
        matches!(self, Self::Only | Self::AndText)
    }

    /// The resolution pages are rendered at for OCR: by our `TextLayerCheckingPdfParser`, or by
    /// Tika's renderer for the pages Tika's PDF parser OCRs itself.
    pub(super) fn resolution(self) -> Resolution {
        match self {
            Self::Only | Self::AndText => Resolution::Full,
            Self::Never | Self::Assessed(_) => Resolution::Capped,
        }
    }

    /// Whether pages are held back until they end, and their text kept for the assessment.
    pub(super) fn holds_pages(self) -> bool {
        matches!(self, Self::Assessed(_))
    }

    /// What is done with a page of `glyphs` glyphs, `unmapped` of them without characters,
    /// whose `div` holds `layer`.
    pub(super) fn page(self, glyphs: i32, unmapped: i32, layer: Layer<'_>) -> PageCheck {
        let thresholds = match self {
            Self::Never | Self::Only => return PageCheck::Keep,
            Self::AndText => return PageCheck::Recognize,
            Self::Assessed(thresholds) => thresholds,
        };
        if glyphs <= thresholds.total_chars {
            return PageCheck::Add;
        }
        let text = match layer {
            Layer::Text(text) => text,
            Layer::Embedding => return PageCheck::Settle { glyphs, unmapped },
            Layer::Unheld => unreachable!("a page is held for its assessment"),
        };
        let assessment = quality::assess(text, glyphs, unmapped, thresholds.unmapped);
        if assessment.badness < quality::DOUBTFUL {
            return PageCheck::Keep;
        }
        PageCheck::Weigh(assessment)
    }

    /// The unmapped glyphs that make a page garbage, which OCR text is weighed against.
    fn unmapped(self) -> Unmapped {
        match self {
            Self::Never | Self::Only | Self::AndText => DEFAULT.unmapped,
            Self::Assessed(thresholds) => thresholds.unmapped,
        }
    }
}

/// What a page's `div` holds, as our `TextLayerCheckingPdfParser` sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Layer<'t> {
    /// Nothing: pages are not held.
    Unheld,
    /// This text.
    Text(&'t str),
    /// Files it embeds too, whose text is known once they are parsed.
    Embedding,
}

/// How the pages to read are read: with tesseract, or with OCR skipped, reading nothing.
enum Reader {
    Tesseract {
        tesseract: ocr::Reader,
        settings: OcrSettings,
    },
    Skipping,
}

/// The OCR of a document's pages, decided as the document is read.
pub(super) struct PageOcr {
    /// The pages to read, by their index, in order.
    pages: Vec<usize>,
    unmapped: Unmapped,
    reader: Reader,
    document: Arc<Vec<u8>>,
    /// The password the document opens with.
    password: String,
    resolution: Resolution,
}

impl PageOcr {
    /// The OCR of the `pages` of a document of `page_count` pages under `check`, rendered from
    /// `input` opened with the password of `configs`, under its OCR settings, or none when it
    /// reads no page. Where tesseract cannot read the pages in this process, or the renderer
    /// cannot open the document or counts its pages otherwise, the pages are read as with OCR
    /// skipped, which reads nothing, and `pdf` notes the difference.
    pub(super) fn of(
        pages: Vec<usize>,
        page_count: usize,
        check: OcrCheck,
        input: &[u8],
        configs: &ParserConfigs,
        pdf: &Document,
    ) -> Option<Self> {
        if pages.is_empty() {
            return None;
        }
        let document = Arc::new(input.to_vec());
        let password = configs
            .password
            .as_ref()
            .map_or("", Password::expose)
            .to_owned();
        let reader = match configs.ocr.skips_ocr() {
            true => Reader::Skipping,
            false => {
                Self::tesseract(&document, &password, page_count, configs).unwrap_or_else(|why| {
                    pdf.differ(Difference(format!(
                        "pages Tika reads by OCR, read without it: {why}"
                    )));
                    Reader::Skipping
                })
            }
        };
        Some(Self {
            pages,
            unmapped: check.unmapped(),
            reader,
            document,
            password,
            resolution: check.resolution(),
        })
    }

    /// Tesseract reading the pages of `document` of `page_count` pages, which the renderer
    /// opens with `password`, under the OCR settings of `configs`, or why it cannot. A tesseract
    /// the build carries but cannot set up still reads them, each reading failing with why.
    fn tesseract(
        document: &Arc<Vec<u8>>,
        password: &str,
        page_count: usize,
        configs: &ParserConfigs,
    ) -> Result<Reader, String> {
        let tesseract = ocr::tesseract();
        let rendered = PdfPages::open(document.clone(), password)
            .map_err(|reason| format!("the renderer does not open the document: {reason}"))?;
        if rendered.count() != page_count {
            return Err(format!(
                "the renderer finds {} pages in the document, where the parser finds \
                 {page_count}",
                rendered.count()
            ));
        }
        Ok(Reader::Tesseract {
            tesseract,
            settings: OcrSettings::of(&configs.ocr),
        })
    }

    pub(super) fn assumed(&self, hint: LanguageHint) -> Option<OcrLanguage> {
        let Reader::Tesseract {
            tesseract: Ok(tesseract),
            settings,
        } = &self.reader
        else {
            return None;
        };
        Recognizer::new(tesseract, settings)
            .assumed(hint)
            .ok()
            .flatten()
    }

    /// Writes the document with `write`, the pages to read rendered and read side by side
    /// meanwhile, each page to read passed on by the hook `write` is handed. The extraction then
    /// assumes the script and model the latest page read in one was read in.
    pub(super) fn write_document<T>(
        &self,
        assumed: Option<OcrLanguage>,
        write: impl FnOnce(&mut PageHook<'_, '_, '_>) -> T,
    ) -> (T, Option<OcrLanguage>) {
        let learning = Learning::new(assumed);
        let written = ocr::read_pages(
            self.pages.len(),
            &learning,
            || match &self.reader {
                Reader::Tesseract { .. } => {
                    Some(PdfPages::open(self.document.clone(), &self.password))
                }
                Reader::Skipping => None,
            },
            |pages, at, assumed| {
                Ok::<_, Infallible>(self.read(pages.as_ref(), self.pages[at], assumed))
            },
            |outputs| write(&mut PageHook { ocr: self, outputs }),
        );
        (written, learning.language())
    }

    /// Renders a page and reads it in a script and model: what it read, and the script and
    /// model it was read in.
    fn read(
        &self,
        pages: Option<&Result<PdfPages, String>>,
        index: usize,
        assumed: Option<&OcrLanguage>,
    ) -> (PageReading, Option<OcrLanguage>) {
        let (
            Reader::Tesseract {
                tesseract,
                settings,
            },
            Some(pages),
        ) = (&self.reader, pages)
        else {
            return (PageReading::Skipped, None);
        };
        let tesseract = match tesseract {
            Ok(tesseract) => *tesseract,
            Err(failure) => return (PageReading::Failed(failure.to_string()), None),
        };
        let rendered = pages
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|pages| pages.render(index, self.resolution));
        let (gray, density) = match rendered {
            Ok(rendered) => rendered,
            Err(failure) => {
                return (
                    PageReading::Failed(format!("Rendering the page failed: {failure}")),
                    None,
                )
            }
        };
        let pixels = u64::from(gray.width()) * u64::from(gray.height());
        let bitmap = Bitmap::Gray(gray);
        let image = Image::of(&bitmap, None, pixels, None, Some(density));
        match Recognizer::new(tesseract, settings).recognize(&image, assumed) {
            Ok(Recognized { read, language }) => (
                PageReading::Read {
                    attempt: Box::new(read),
                    page: index as u32 + 1,
                },
                language,
            ),
            Err(failure) => (PageReading::Failed(failure.to_string()), None),
        }
    }
}

/// What reading a page by OCR gave.
pub(super) enum PageReading {
    /// The reading of the page, on its page number.
    Read { attempt: Box<Attempt>, page: u32 },
    /// Nothing, OCR being skipped.
    Skipped,
    /// The page could not be rendered or read, for this reason.
    Failed(String),
}

impl PageReading {
    /// Whether the reading replaces a page's text layer assessed as `layer`: when OCR read the
    /// page, and the layer is garbage or the OCR text reads better. Our Java replaced a
    /// garbage layer with the empty output of a skipped OCR too, so that the page's text
    /// disappeared (#117).
    fn replaces(&self, layer: &Assessment, unmapped: Unmapped) -> bool {
        let Self::Read { attempt, .. } = self else {
            return false;
        };
        layer.badness >= quality::GARBAGE
            || reads_better(attempt.text().unwrap_or_default(), layer, unmapped)
    }

    /// `AbstractPDF2XHTML.enrichCurrentPage` with Tika's PDF parser's own OCR parser: the
    /// parser recorded and its text written, unless OCR is skipped, where Tika records it
    /// though it reads nothing (#114). A page the OCR parser fails
    /// on fails the document, as `OCR2XHTML` and `PDF2XHTML` throw the first such failure once
    /// every page is written.
    fn recognize(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match self {
            Self::Read { attempt, .. } => {
                xhtml.record_parser(PREPARING_OCR_PARSER);
                Ok(write_text(xhtml, attempt)?)
            }
            Self::Skipped => Ok(()),
            Self::Failed(failure) => Err(Failure::Extraction(Error::ParseError(format!(
                "org.apache.tika.exception.TikaException: Unable to extract all PDF content \
                 <- caused by {failure}"
            )))),
        }
    }

    /// Notes a page `index` that could not be read, as our `TextLayerCheckingPdfParser` notes
    /// it once the page is passed on.
    fn warn(&self, xhtml: &mut Xhtml, index: usize) {
        if let Self::Failed(failure) = self {
            xhtml.metadata_mut().add(
                EXCEPTION_WARNING,
                format!("OCR of page {} failed: {failure}", index + 1),
            );
        }
    }

    /// `TextLayerCheckingPdfParser.useOcr`: the reading's text in a div of class `ocr`, as the
    /// OCR parser's own handler wrote it, then its report.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        let Self::Read { attempt, page } = self else {
            return Ok(());
        };
        if let Some(text) = attempt.text() {
            xhtml.start_element_with("div", &[("class", "ocr")])?;
            xhtml.characters(text)?;
            xhtml.end_element_passed_on("div")?;
            xhtml.ignorable_whitespace("\n")?;
        }
        xhtml
            .metadata_mut()
            .add(ocr::REPORT, attempt.report(Some(*page)));
        Ok(())
    }
}

/// What passes each page to read on once it ends: our `TextLayerCheckingPdfParser`.
pub(super) trait EndPage {
    /// Passes on the page `index`, of `check`, whose held content `held` writes.
    fn end_page(
        &mut self,
        xhtml: &mut Xhtml,
        index: usize,
        check: PageCheck,
        held: &mut dyn FnMut(&mut Xhtml) -> Result<(), Failure>,
    ) -> Result<(), Failure>;
}

/// Ends each page to read as `TextLayerCheckingPdfParser` passes it on: with OCR text added, or
/// with its text replaced by OCR text.
pub(super) struct PageHook<'o, 'r, 's> {
    ocr: &'o PageOcr,
    outputs: &'r mut Outputs<'s, PageReading, Infallible>,
}

impl EndPage for PageHook<'_, '_, '_> {
    fn end_page(
        &mut self,
        xhtml: &mut Xhtml,
        index: usize,
        check: PageCheck,
        held: &mut dyn FnMut(&mut Xhtml) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        xhtml.flush()?;
        let reading = match self.outputs.next() {
            Some(Ok(reading)) => reading,
            Some(Err(never)) => match never {},
            None => unreachable!("a page to read has its reading"),
        };
        match check {
            PageCheck::Keep => held(xhtml),
            PageCheck::Add => {
                reading.warn(xhtml, index);
                held(xhtml)?;
                Ok(reading.write(xhtml)?)
            }
            PageCheck::Weigh(layer) => {
                reading.warn(xhtml, index);
                if !reading.replaces(&layer, self.ocr.unmapped) {
                    return held(xhtml);
                }
                self.replace(xhtml, index, &layer, &reading)
            }
            PageCheck::Recognize => reading.recognize(xhtml),
            PageCheck::Settle { glyphs, unmapped } => {
                let page = xhtml.hold(|xhtml| held(xhtml))?;
                let layer = quality::assess(page.text(), glyphs, unmapped, self.ocr.unmapped);
                if layer.badness < quality::DOUBTFUL {
                    return Ok(xhtml.release(page)?);
                }
                reading.warn(xhtml, index);
                if !reading.replaces(&layer, self.ocr.unmapped) {
                    return Ok(xhtml.release(page)?);
                }
                self.replace(xhtml, index, &layer, &reading)
            }
        }
    }
}

impl PageHook<'_, '_, '_> {
    /// Writes `reading` in place of page `index`'s text layer, assessed as `layer`.
    fn replace(
        &self,
        xhtml: &mut Xhtml,
        index: usize,
        layer: &Assessment,
        reading: &PageReading,
    ) -> Result<(), Failure> {
        xhtml.metadata_mut().add(
            REJECTED_TEXT_LAYER,
            format!("{}:{}", index + 1, layer.fault),
        );
        Ok(reading.write(xhtml)?)
    }
}

/// Whether the OCR text `text` reads better than the text layer it would replace, its glyphs
/// counted as the characters it shows. Our Java counted the UTF-16 length of the text with the
/// line break after it (#117).
fn reads_better(text: &str, layer: &Assessment, unmapped: Unmapped) -> bool {
    let glyphs = text.chars().filter(|c| !c.is_whitespace()).count();
    let glyphs = i32::try_from(glyphs).unwrap_or(i32::MAX);
    quality::assess(text, glyphs, 0, unmapped).badness < layer.badness
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(setting: &str) -> Option<(i32, Unmapped)> {
        Thresholds::parse(setting)
            .ok()
            .map(|thresholds| (thresholds.total_chars, thresholds.unmapped))
    }

    #[test]
    fn reads_the_thresholds_as_our_pdf_config_does() {
        assert_eq!(parsed("better"), Some((10, Unmapped::Count(10.0))));
        assert_eq!(parsed(" faster,\t5 "), Some((5, Unmapped::Share(0.1))));
        assert_eq!(parsed("25%"), Some((10, Unmapped::Share(0.25))));
        assert_eq!(parsed("100%,0"), Some((0, Unmapped::Share(1.0))));
        assert_eq!(parsed("7"), Some((10, Unmapped::Count(7.0))));
        assert_eq!(parsed("1"), Some((10, Unmapped::Count(1.0))));
        assert_eq!(parsed("faster , 5"), None);
        assert_eq!(parsed("101%"), None);
        assert_eq!(parsed("1000"), None);
        assert_eq!(parsed("better,"), None);
        assert_eq!(parsed("best"), None);
        assert_eq!(parsed("5 %"), None);
    }

    #[test]
    fn rejects_thresholds_with_our_pdf_configs_messages() {
        let rejected = |setting| {
            Thresholds::parse(setting)
                .unwrap_err()
                .into_error()
                .to_string()
        };
        assert_eq!(
            rejected("best"),
            "Java exception: java.lang.IllegalArgumentException: Automatic OCR thresholds are \
             better, faster or unmapped[%][,characters]: best"
        );
        assert_eq!(
            rejected(" 101%,5"),
            "Java exception: java.lang.IllegalArgumentException: The share of unmapped glyphs \
             cannot exceed 100%:  101%,5"
        );
    }

    #[test]
    fn keeps_a_text_layer_ocr_did_not_read() {
        let garbage = Assessment {
            badness: quality::GARBAGE,
            fault: "no-text",
        };
        for reading in [PageReading::Skipped, PageReading::Failed("no".to_owned())] {
            assert!(!reading.replaces(&garbage, DEFAULT.unmapped));
        }
    }

    #[test]
    fn weighs_ocr_text_by_the_characters_it_shows() {
        let doubtful = Assessment {
            badness: 0.6,
            fault: "symbols",
        };
        let prose = "The keeper climbed the stairs every evening and wrote one line.";
        assert!(reads_better(prose, &doubtful, DEFAULT.unmapped));
        assert!(!reads_better("", &doubtful, DEFAULT.unmapped));
    }

    #[test]
    fn ocrs_every_page_under_the_ocr_strategies() {
        for (strategy, check) in [
            (PdfOcrStrategy::OCR_ONLY, OcrCheck::Only),
            (PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION, OcrCheck::AndText),
        ] {
            let config = PdfParserConfig::new().set_ocr_strategy(strategy);
            assert_eq!(OcrCheck::of(&config), Ok(check));
        }
        assert_eq!(OcrCheck::Only.page(3, 0, Layer::Unheld), PageCheck::Keep);
        assert_eq!(
            OcrCheck::AndText.page(300, 0, Layer::Unheld),
            PageCheck::Recognize
        );
    }

    #[test]
    fn checks_pages_as_text_layer_checking_pdf_parser_does() {
        let assessed = OcrCheck::Assessed(DEFAULT);
        assert_eq!(assessed.page(3, 0, Layer::Text("ab")), PageCheck::Add);
        let prose = "The keeper climbed the stairs every evening and wrote one line. ".repeat(5);
        assert_eq!(assessed.page(300, 0, Layer::Text(&prose)), PageCheck::Keep);
        assert!(matches!(
            assessed.page(300, 200, Layer::Text(&prose)),
            PageCheck::Weigh(_)
        ));
        assert_eq!(OcrCheck::Never.page(3, 0, Layer::Unheld), PageCheck::Keep);
    }
}
