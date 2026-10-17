//! The OCR of a PDF's pages. Whether Tika's path would OCR a page: never under `NO_OCR`; under
//! `AUTO`, by `AbstractPDF2XHTML.ocrWanted` with the thresholds `PdfConfig.setOcrStrategyAuto`
//! sets, pages the port leaves to Tika, or, when tesseract runs, by our
//! `TextLayerCheckingPdfParser`, which decides page by page from the text the page's text layer
//! yields ([`quality`]): a page with next to no text gets OCR text added, a page whose text layer
//! is garbage has it replaced by OCR text, and a doubtful one is read too and keeps whichever
//! text reads better. A replaced text layer is listed in [`REJECTED_TEXT_LAYER`]. The pages to
//! read are rendered and read side by side while the document is written, each page's output
//! passed on in order.

use std::convert::Infallible;
use std::sync::Arc;

use super::quality::{self, Assessment};
use super::Unported;
use crate::config::{PdfOcrStrategy, PdfParserConfig};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;
use crate::ocr::{
    self, Attempt, Bitmap, FirstPage, Image, OcrError, OcrSettings, Outputs, PdfPages, Recognized,
    Recognizer, Tesseract,
};

/// Each page whose text layer was replaced by OCR text, as `page:fault`.
const REJECTED_TEXT_LAYER: &str = "extractous:rejected-text-layer";
/// `TikaCoreProperties.TIKA_META_EXCEPTION_WARNING`.
const WARNING: &str = "tk:exception:warn";

/// `PagesConfig.Auto`: a page of at most `total_chars` glyphs is OCRed, and so is one with more
/// glyphs without characters than `unmapped`, a share of the page's glyphs below 1 and a count
/// from 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Thresholds {
    total_chars: i32,
    unmapped: f32,
}

/// `PagesConfig.Auto.defaults()`, which `better` keeps.
const DEFAULT: Thresholds = Thresholds {
    total_chars: 10,
    unmapped: 10.0,
};
/// The unmapped glyphs `faster` takes: a tenth of the page's.
const FASTER_UNMAPPED: f32 = 0.1;

impl Thresholds {
    /// `PdfConfig.setOcrStrategyAuto`: `better`, `faster` or `unmapped[%][,characters]`, as its
    /// pattern `\s*(?:(better|faster)|(\d{1,3})(%)?)(?:,\s*(\d{1,3}))?\s*` matches the whole
    /// setting; `None` where the setter throws.
    fn parse(setting: &str) -> Option<Self> {
        let trimmed = setting.trim_matches(is_java_space);
        let (head, total) = match trimmed.split_once(',') {
            Some((head, total)) => (head, Some(total.trim_start_matches(is_java_space))),
            None => (trimmed, None),
        };
        let total_chars = match total {
            Some(total) => digits(total)?,
            None => DEFAULT.total_chars,
        };
        let unmapped = match head {
            "better" => DEFAULT.unmapped,
            "faster" => FASTER_UNMAPPED,
            _ => match head.strip_suffix('%') {
                Some(share) => {
                    let share = digits(share)? as f32;
                    if share > 100.0 {
                        return None;
                    }
                    share / 100.0
                }
                None => digits(head)? as f32,
            },
        };
        Some(Self {
            total_chars,
            unmapped,
        })
    }

    /// `AbstractPDF2XHTML.ocrWanted`.
    fn ocr_wanted(self, glyphs: i32, unmapped: i32) -> bool {
        if glyphs <= self.total_chars {
            return true;
        }
        if self.unmapped < 1.0 {
            unmapped as f32 / glyphs as f32 > self.unmapped
        } else {
            unmapped as f32 > self.unmapped
        }
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

/// How Tika's path decides whether to OCR a page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum OcrCheck {
    /// `NO_OCR`: it OCRs no page.
    Never,
    /// As `AbstractPDF2XHTML.ocrWanted` decides it.
    Wanted(Thresholds),
    /// As our `TextLayerCheckingPdfParser` decides it from the page's text, holding each page
    /// back until it ends, when tesseract runs.
    Assessed(Thresholds),
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
}

impl OcrCheck {
    /// The check Tika's path makes under the PDF settings, with tesseract running or not, or
    /// why the port leaves the document to Tika: OCR in place of the text layer, and
    /// thresholds Tika rejects.
    pub(super) fn of(config: &PdfParserConfig, ocr_runs: bool) -> Result<Self, Unported> {
        let thresholds = match config.ocr_strategy_auto() {
            None => DEFAULT,
            Some(setting) => Thresholds::parse(setting).ok_or_else(|| {
                Unported(format!(
                    "the automatic OCR thresholds {setting:?}, which Tika rejects"
                ))
            })?,
        };
        match config.ocr_strategy() {
            PdfOcrStrategy::NO_OCR => Ok(Self::Never),
            PdfOcrStrategy::AUTO if ocr_runs => Ok(Self::Assessed(thresholds)),
            PdfOcrStrategy::AUTO => Ok(Self::Wanted(thresholds)),
            strategy @ (PdfOcrStrategy::OCR_ONLY | PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION) => {
                Err(Unported(format!(
                    "the OCR strategy {strategy}, which the port does not run"
                )))
            }
        }
    }

    /// Whether pages are held back until they end, and their text kept for the assessment.
    pub(super) fn holds_pages(self) -> bool {
        matches!(self, Self::Assessed(_))
    }

    /// What is done with page `page` of `glyphs` glyphs, `unmapped` of them without
    /// characters, whose `div` holds `text` when the pages are held, or why Tika's path takes
    /// the document: a page Tika's PDF parser wants OCR for with no OCR parser to run.
    pub(super) fn page(
        self,
        page: usize,
        glyphs: i32,
        unmapped: i32,
        text: Option<&str>,
    ) -> Result<PageCheck, Unported> {
        let thresholds = match self {
            Self::Never => return Ok(PageCheck::Keep),
            Self::Wanted(thresholds) => {
                return match thresholds.ocr_wanted(glyphs, unmapped) {
                    true => Err(Unported(format!(
                        "page {page} shows {glyphs} glyphs, {unmapped} of them without \
                         characters, which Tika's PDF parser wants OCR for with no OCR parser \
                         to run"
                    ))),
                    false => Ok(PageCheck::Keep),
                };
            }
            Self::Assessed(thresholds) => thresholds,
        };
        if glyphs <= thresholds.total_chars {
            return Ok(PageCheck::Add);
        }
        let text = text.expect("a held page's text is kept for its assessment");
        let assessment = quality::assess(text, glyphs, unmapped, thresholds.unmapped);
        if assessment.badness < quality::DOUBTFUL {
            return Ok(PageCheck::Keep);
        }
        Ok(PageCheck::Weigh(assessment))
    }

    /// The unmapped glyphs that make a page garbage, which OCR text is weighed against.
    fn unmapped(self) -> f32 {
        match self {
            Self::Never => DEFAULT.unmapped,
            Self::Wanted(thresholds) | Self::Assessed(thresholds) => thresholds.unmapped,
        }
    }
}

/// How the pages to read are read: with tesseract, or with OCR skipped, reading nothing.
enum Reader {
    Tesseract {
        tesseract: &'static Tesseract,
        settings: OcrSettings,
    },
    Skipping,
}

/// The OCR of a document's pages, decided as the document is read.
pub(super) struct PageOcr {
    /// The pages to read, by their index, in order.
    pages: Vec<usize>,
    unmapped: f32,
    reader: Reader,
    document: Arc<Vec<u8>>,
}

impl PageOcr {
    /// The OCR of the `pages` of a document of `page_count` pages under `check`, rendered from
    /// `input`, or none when it reads no page. A document the renderer cannot open, or whose
    /// pages it counts otherwise, is left to Tika's path, as is one whose pages tesseract cannot
    /// read in this process.
    pub(super) fn of(
        pages: Vec<usize>,
        page_count: usize,
        check: OcrCheck,
        input: &[u8],
        skips_ocr: bool,
        settings: OcrSettings,
    ) -> Result<Option<Self>, Unported> {
        if pages.is_empty() {
            return Ok(None);
        }
        let document = Arc::new(input.to_vec());
        let reader = if skips_ocr {
            Reader::Skipping
        } else {
            let tesseract = ocr::tesseract().map_err(|reason| {
                Unported(format!(
                    "OCR of PDF pages in this process, which needs tesseract's library: {reason}"
                ))
            })?;
            let rendered = PdfPages::open(document.clone()).map_err(Unported)?;
            if rendered.count() != page_count {
                return Err(Unported(format!(
                    "a document the renderer finds {} pages in, where the parser finds \
                     {page_count}",
                    rendered.count()
                )));
            }
            Reader::Tesseract {
                tesseract,
                settings,
            }
        };
        Ok(Some(Self {
            pages,
            unmapped: check.unmapped(),
            reader,
            document,
        }))
    }

    /// Writes the document with `write`, the pages to read rendered and read side by side
    /// meanwhile, each page to read passed on by the hook `write` is handed. The extraction then
    /// assumes the script the first page read learned.
    pub(super) fn write_document<T>(
        &self,
        assumed: Option<String>,
        write: impl FnOnce(&mut PageHook<'_, '_, '_>) -> T,
    ) -> (T, Option<String>) {
        let first_page = FirstPage::new(assumed);
        let written = ocr::read_pages(
            self.pages.len(),
            &first_page,
            || match &self.reader {
                Reader::Tesseract { .. } => Some(PdfPages::open(self.document.clone())),
                Reader::Skipping => None,
            },
            |pages, at, script| {
                Ok::<_, Infallible>(self.read(pages.as_ref(), self.pages[at], script))
            },
            |outputs| write(&mut PageHook { ocr: self, outputs }),
        );
        (written, first_page.learned())
    }

    /// Renders a page and reads it in a script: what it read, and the script it learned.
    fn read(
        &self,
        pages: Option<&Result<PdfPages, String>>,
        index: usize,
        script: Option<&str>,
    ) -> (PageReading, Option<String>) {
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
        let rendered = pages
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|pages| pages.render(index));
        let gray = match rendered {
            Ok(gray) => gray,
            Err(failure) => {
                return (
                    PageReading::Failed(format!("java.io.IOException: {failure}")),
                    None,
                )
            }
        };
        let pixels = u64::from(gray.width()) * u64::from(gray.height());
        let bitmap = Bitmap::Gray(gray);
        let image = Image::of(&bitmap, None, pixels, None);
        match Recognizer::new(tesseract, settings).recognize(&image, script) {
            Ok(Recognized { read, learned }) => (
                PageReading::Read {
                    attempt: read,
                    page: index as u32 + 1,
                },
                learned.filter(|_| settings.auto()),
            ),
            Err(failure) => (PageReading::Failed(described(&failure)), None),
        }
    }
}

/// An OCR failure as `Throwables.describe` renders the exception Tika's path throws for it.
fn described(failure: &OcrError) -> String {
    let class = match failure {
        OcrError::TimedOut { .. } => "org.apache.tika.exception.TikaTimeoutException",
        OcrError::Failed { .. } | OcrError::Model { .. } | OcrError::Models { .. } => {
            "org.apache.tika.exception.TikaException"
        }
    };
    format!("{class}: {failure}")
}

/// What reading a page by OCR gave.
pub(super) enum PageReading {
    /// The reading of the page, on its page number.
    Read { attempt: Attempt, page: u32 },
    /// Nothing, OCR being skipped.
    Skipped,
    /// The page could not be rendered or read, for this reason.
    Failed(String),
}

impl PageReading {
    /// The text the reading writes, as Tika's page checker records it: the text, and the line
    /// break after its `div`.
    fn recorded_text(&self) -> String {
        match self {
            Self::Read { attempt, .. } => attempt
                .text()
                .map_or_else(String::new, |text| format!("{text}\n")),
            Self::Skipped | Self::Failed(_) => String::new(),
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
        let reading = match self.outputs.next() {
            Some(Ok(reading)) => reading,
            Some(Err(never)) => match never {},
            None => unreachable!("a page to read has its reading"),
        };
        if let PageReading::Failed(failure) = &reading {
            xhtml.metadata_mut().add(
                WARNING,
                format!("OCR of page {} failed: {failure}", index + 1),
            );
        }
        match check {
            PageCheck::Keep => held(xhtml),
            PageCheck::Add => {
                held(xhtml)?;
                Ok(reading.write(xhtml)?)
            }
            PageCheck::Weigh(layer) => {
                let failed = matches!(reading, PageReading::Failed(_));
                if failed
                    || !(layer.badness >= quality::GARBAGE || self.reads_better(&reading, &layer))
                {
                    return held(xhtml);
                }
                xhtml.metadata_mut().add(
                    REJECTED_TEXT_LAYER,
                    format!("{}:{}", index + 1, layer.fault),
                );
                Ok(reading.write(xhtml)?)
            }
        }
    }
}

impl PageHook<'_, '_, '_> {
    /// Whether the OCR text reads better than the text layer it would replace.
    fn reads_better(&self, reading: &PageReading, layer: &Assessment) -> bool {
        let text = reading.recorded_text();
        let length = text.encode_utf16().count() as i32;
        quality::assess(&text, length, 0, self.ocr.unmapped).badness < layer.badness
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(setting: &str) -> Option<(i32, f32)> {
        Thresholds::parse(setting).map(|thresholds| (thresholds.total_chars, thresholds.unmapped))
    }

    #[test]
    fn reads_the_thresholds_as_our_pdf_config_does() {
        assert_eq!(parsed("better"), Some((10, 10.0)));
        assert_eq!(parsed(" faster,\t5 "), Some((5, 0.1)));
        assert_eq!(parsed("25%"), Some((10, 0.25)));
        assert_eq!(parsed("100%,0"), Some((0, 1.0)));
        assert_eq!(parsed("7"), Some((10, 7.0)));
        assert_eq!(parsed("faster , 5"), None);
        assert_eq!(parsed("101%"), None);
        assert_eq!(parsed("1000"), None);
        assert_eq!(parsed("better,"), None);
        assert_eq!(parsed("best"), None);
        assert_eq!(parsed("5 %"), None);
    }

    #[test]
    fn wants_ocr_as_tika_does() {
        assert!(DEFAULT.ocr_wanted(10, 0));
        assert!(!DEFAULT.ocr_wanted(11, 10));
        assert!(DEFAULT.ocr_wanted(11, 11));
        let share = Thresholds {
            total_chars: 10,
            unmapped: 0.25,
        };
        assert!(!share.ocr_wanted(100, 25));
        assert!(share.ocr_wanted(100, 26));
        let whole = Thresholds::parse("100%").unwrap();
        assert!(!whole.ocr_wanted(100, 1));
        assert!(whole.ocr_wanted(100, 2));
    }

    #[test]
    fn checks_pages_as_text_layer_checking_pdf_parser_does() {
        let assessed = OcrCheck::Assessed(DEFAULT);
        assert_eq!(assessed.page(1, 3, 0, Some("ab")), Ok(PageCheck::Add));
        let prose = "The keeper climbed the stairs every evening and wrote one line. ".repeat(5);
        assert_eq!(assessed.page(1, 300, 0, Some(&prose)), Ok(PageCheck::Keep));
        assert!(matches!(
            assessed.page(1, 300, 200, Some(&prose)),
            Ok(PageCheck::Weigh(_))
        ));
        assert!(OcrCheck::Wanted(DEFAULT).page(1, 3, 0, None).is_err());
        assert_eq!(OcrCheck::Never.page(1, 3, 0, None), Ok(PageCheck::Keep));
    }
}
