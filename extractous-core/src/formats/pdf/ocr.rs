//! The OCR of a PDF's pages: Tika 4's page text policy as our Java `PdfConfig` sets it, and our
//! `TextLayerCheckingPdfParser`, which under the automatic policy decides page by page from the
//! text the page's text layer yields ([`quality`]): a page with next to no text gets OCR text
//! added, a page whose text layer is garbage has it replaced by OCR text, and a doubtful one is
//! read too and keeps whichever text reads better. A replaced text layer is listed in
//! [`REJECTED_TEXT_LAYER`]. The pages to read are rendered and read side by side while the
//! document is written, each page's output passed on in order.

use std::convert::Infallible;
use std::sync::Arc;

use super::quality::{self, Assessment};
use super::writer::{EndPage, PageLayer, PageWriter, WriteFailure};
use super::Unported;
use crate::config::{PdfOcrStrategy, PdfParserConfig};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::java::is_regex_space;
use crate::ocr::{
    self, Attempt, FirstPage, Image, OcrError, OcrSettings, Outputs, PdfPages, Recognized,
    Recognizer, Tesseract,
};

/// Each page whose text layer was replaced by OCR text, as `page:fault`.
const REJECTED_TEXT_LAYER: &str = "extractous:rejected-text-layer";
/// `TikaCoreProperties.TIKA_META_EXCEPTION_WARNING`.
const WARNING: &str = "tk:exception:warn";
/// `PdfConfig`'s thresholds: `better` counts more than 10 unmapped glyphs, `faster` a tenth of
/// a page's glyphs, and both a page of at most 10 characters.
const BETTER_UNMAPPED: f32 = 10.0;
const FASTER_UNMAPPED: f32 = 0.1;
const DEFAULT_TOTAL_CHARACTERS: i32 = 10;

/// When the automatic policy OCRs a page: `PagesConfig.Auto`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Thresholds {
    /// More unmapped glyphs than this, a count from 1 and a share of the page's glyphs below.
    unmapped: f32,
    /// At most this many characters.
    total_characters: i32,
}

impl Default for Thresholds {
    /// `PagesConfig.Auto.defaults`.
    fn default() -> Self {
        Self {
            unmapped: BETTER_UNMAPPED,
            total_characters: DEFAULT_TOTAL_CHARACTERS,
        }
    }
}

impl Thresholds {
    /// `PdfConfig.setOcrStrategyAuto`: `better`, `faster` or `unmapped[%][,characters]`, with
    /// whitespace allowed before and after it and after the comma.
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim_matches(is_regex_space);
        let (unmapped, total_characters) = match text.split_once(',') {
            Some((unmapped, characters)) => {
                let characters = characters.trim_start_matches(is_regex_space);
                (unmapped, number(characters)?)
            }
            None => (text, DEFAULT_TOTAL_CHARACTERS),
        };
        Some(Self {
            unmapped: Self::unmapped(unmapped)?,
            total_characters,
        })
    }

    /// The unmapped glyphs of `better`, `faster`, or a count or share of at most 100%.
    fn unmapped(text: &str) -> Option<f32> {
        match text {
            "better" => return Some(BETTER_UNMAPPED),
            "faster" => return Some(FASTER_UNMAPPED),
            _ => {}
        }
        let (number, share) = match text.strip_suffix('%') {
            Some(number) => (number, true),
            None => (text, false),
        };
        let unmapped = self::number(number)? as f32;
        match share {
            true if unmapped > 100.0 => None,
            true => Some(unmapped / 100.0),
            false => Some(unmapped),
        }
    }

    /// `AbstractPDF2XHTML.ocrWanted`: whether Tika's automatic policy OCRs a page of `glyphs`
    /// glyphs, `unmapped` of them without characters.
    pub(super) fn wanted(&self, glyphs: i32, unmapped: i32) -> bool {
        if glyphs <= self.total_characters {
            return true;
        }
        let share = unmapped as f32 / glyphs as f32;
        if self.unmapped < 1.0 {
            share > self.unmapped
        } else {
            unmapped as f32 > self.unmapped
        }
    }
}

/// A number of one to three digits, as `\d{1,3}` matches it.
fn number(text: &str) -> Option<i32> {
    if !(1..=3).contains(&text.len()) || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Where the text of a document's pages comes from: Tika 4's `TextPolicy`, as our Java
/// `PdfConfig` maps the OCR strategies onto it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Policy {
    /// The text layer alone.
    Extract,
    /// The text layer, and OCR of the pages the thresholds pick.
    Auto(Thresholds),
}

impl Policy {
    /// The policy the settings give, unless it is one whose OCR Tika's PDF parser runs itself,
    /// or thresholds `PdfConfig` rejects.
    pub(super) fn of(config: &PdfParserConfig) -> Result<Self, Unported> {
        let thresholds = match config.ocr_strategy_auto() {
            None => Thresholds::default(),
            Some(text) => Thresholds::parse(text).ok_or_else(|| {
                Unported(format!(
                    "the automatic OCR thresholds {text:?}, which Tika's path rejects"
                ))
            })?,
        };
        match config.ocr_strategy().unwrap_or_default() {
            PdfOcrStrategy::NO_OCR => Ok(Self::Extract),
            PdfOcrStrategy::AUTO => Ok(Self::Auto(thresholds)),
            strategy @ (PdfOcrStrategy::OCR_ONLY | PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION) => {
                Err(Unported(format!(
                    "the OCR strategy {strategy}, whose OCR Tika's PDF parser runs itself"
                )))
            }
        }
    }
}

/// What our `TextLayerCheckingPdfParser` does with a page.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Check {
    /// Its text layer is kept.
    Keep,
    /// It has next to no text: OCR text is added.
    Add,
    /// Its text layer is doubtful or garbage: OCR text replaces it when it reads better, or
    /// whenever the layer is garbage.
    Weigh(Assessment),
}

/// `TextLayerCheckingPdfParser.endPage`'s decision for a page of a text layer.
fn check(layer: &PageLayer, thresholds: &Thresholds) -> Check {
    if layer.glyphs <= thresholds.total_characters {
        return Check::Add;
    }
    let assessment = quality::assess(
        &layer.text,
        layer.glyphs,
        layer.unmapped,
        thresholds.unmapped,
    );
    if assessment.badness < quality::DOUBTFUL {
        return Check::Keep;
    }
    Check::Weigh(assessment)
}

/// How the pages to read are read: with tesseract, or with OCR skipped, reading nothing.
#[derive(Clone)]
enum Reader {
    Tesseract {
        tesseract: &'static Tesseract,
        settings: OcrSettings,
    },
    Skipping,
}

/// The OCR of a document's pages, decided before it is written.
pub(super) struct PageOcr {
    checks: Vec<Check>,
    thresholds: Thresholds,
    reader: Reader,
    document: Arc<Vec<u8>>,
}

impl PageOcr {
    /// What our `TextLayerCheckingPdfParser` does with each page of a document of `layers`
    /// under `thresholds`, or none when it reads no page. The pages it reads are rendered from
    /// `input`; a document the renderer cannot open, or whose pages it counts otherwise, is
    /// left to Tika's path, as is one whose pages tesseract cannot read in this process.
    pub(super) fn of(
        layers: &[PageLayer],
        thresholds: Thresholds,
        input: &[u8],
        skips_ocr: bool,
        settings: OcrSettings,
    ) -> Result<Option<Self>, Unported> {
        let checks: Vec<Check> = layers
            .iter()
            .map(|layer| check(layer, &thresholds))
            .collect();
        if checks.iter().all(|check| *check == Check::Keep) {
            return Ok(None);
        }
        let reader = if skips_ocr {
            Reader::Skipping
        } else {
            let tesseract = ocr::tesseract().map_err(|reason| {
                Unported(format!(
                    "OCR of PDF pages in this process, which needs tesseract's library: {reason}"
                ))
            })?;
            Reader::Tesseract {
                tesseract,
                settings,
            }
        };
        let document = Arc::new(input.to_vec());
        if matches!(reader, Reader::Tesseract { .. }) {
            let pages = PdfPages::open(document.clone()).map_err(Unported)?;
            if pages.count() != layers.len() {
                return Err(Unported(format!(
                    "a document the renderer finds {} pages in, where the parser finds {}",
                    pages.count(),
                    layers.len()
                )));
            }
        }
        Ok(Some(Self {
            checks,
            thresholds,
            reader,
            document,
        }))
    }

    /// Writes the document with `write`, the pages to read rendered and read side by side
    /// meanwhile, each page ended by the hook `write` is handed. The extraction then assumes the
    /// script the first page read learned.
    pub(super) fn write_document<T>(
        &self,
        assumed: Option<String>,
        write: impl FnOnce(&mut PageHook<'_, '_, '_>) -> T,
    ) -> (T, Option<String>) {
        let read: Vec<usize> = self
            .checks
            .iter()
            .enumerate()
            .filter(|(_, check)| **check != Check::Keep)
            .map(|(index, _)| index)
            .collect();
        let first_page = FirstPage::new(assumed);
        let written = ocr::read_pages(
            read.len(),
            &first_page,
            || PdfPages::open(self.document.clone()),
            |pages, at, script| Ok::<_, Infallible>(self.read(pages, read[at], script)),
            |outputs| write(&mut PageHook { ocr: self, outputs }),
        );
        (written, first_page.learned())
    }

    /// Renders a page and reads it in a script: what it read, and the script it learned.
    fn read(
        &self,
        pages: &Result<PdfPages, String>,
        index: usize,
        script: Option<&str>,
    ) -> (PageReading, Option<String>) {
        let Reader::Tesseract {
            tesseract,
            settings,
        } = &self.reader
        else {
            return (PageReading::Skipped, None);
        };
        let page = index as u32 + 1;
        let rendered = pages
            .as_ref()
            .map_err(|failure| format!("java.io.IOException: {failure}"))
            .and_then(|pages| {
                pages
                    .render(index)
                    .map_err(|failure| format!("java.io.IOException: {failure}"))
            });
        let gray = match rendered {
            Ok(gray) => gray,
            Err(failure) => return (PageReading::Failed(failure), None),
        };
        let pixels = u64::from(gray.width()) * u64::from(gray.height());
        let image = Image::of(&gray, pixels, None);
        match Recognizer::new(tesseract, settings).recognize(&image, script) {
            Ok(Recognized { read, learned }) => (
                PageReading::Read {
                    attempt: read,
                    page,
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
enum PageReading {
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

/// Ends each page as `TextLayerCheckingPdfParser` passes it on: as it is, with OCR text added,
/// or with its text replaced by OCR text.
pub(super) struct PageHook<'o, 'r, 's> {
    ocr: &'o PageOcr,
    outputs: &'r mut Outputs<'s, PageReading, Infallible>,
}

impl EndPage for PageHook<'_, '_, '_> {
    /// Passes on the page `index`, whose text layer `page` holds back, as its check says.
    fn end_page(
        &mut self,
        index: usize,
        page: &mut PageWriter<'_, '_, '_>,
    ) -> Result<(), WriteFailure> {
        let check = self.ocr.checks[index];
        if check == Check::Keep {
            return page.release();
        }
        let reading = match self.outputs.next() {
            Some(Ok(reading)) => reading,
            Some(Err(never)) => match never {},
            None => unreachable!("a page to read has its reading"),
        };
        if let PageReading::Failed(failure) = &reading {
            page.xhtml.metadata_mut().add(
                WARNING,
                format!("OCR of page {} failed: {failure}", index + 1),
            );
        }
        match check {
            Check::Keep => unreachable!("a page kept is passed on above"),
            Check::Add => {
                page.release()?;
                Ok(reading.write(page.xhtml)?)
            }
            Check::Weigh(layer) => {
                let failed = matches!(reading, PageReading::Failed(_));
                if failed
                    || !(layer.badness >= quality::GARBAGE || self.reads_better(&reading, &layer))
                {
                    return page.release();
                }
                page.xhtml.metadata_mut().add(
                    REJECTED_TEXT_LAYER,
                    format!("{}:{}", index + 1, layer.fault),
                );
                page.discard();
                Ok(reading.write(page.xhtml)?)
            }
        }
    }
}

impl PageHook<'_, '_, '_> {
    /// Whether the OCR text reads better than the text layer it would replace.
    fn reads_better(&self, reading: &PageReading, layer: &Assessment) -> bool {
        let text = reading.recorded_text();
        let length = text.encode_utf16().count() as i32;
        quality::assess(&text, length, 0, self.ocr.thresholds.unmapped).badness < layer.badness
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_thresholds_as_pdf_config_does() {
        let parsed = |text| Thresholds::parse(text);
        assert_eq!(parsed("better"), Some(Thresholds::default()));
        assert_eq!(
            parsed("faster"),
            Some(Thresholds {
                unmapped: FASTER_UNMAPPED,
                total_characters: 10
            })
        );
        assert_eq!(
            parsed(" 50%, 7 "),
            Some(Thresholds {
                unmapped: 0.5,
                total_characters: 7
            })
        );
        assert_eq!(parsed("50% ,7"), None);
        assert_eq!(
            parsed("25"),
            Some(Thresholds {
                unmapped: 25.0,
                total_characters: 10
            })
        );
        assert_eq!(parsed("101%"), None);
        assert_eq!(parsed("1000"), None);
        assert_eq!(parsed("fast"), None);
        assert_eq!(parsed("50%,"), None);
    }

    #[test]
    fn wants_ocr_as_abstract_pdf2xhtml_does() {
        let share = Thresholds {
            unmapped: 0.5,
            total_characters: 10,
        };
        assert!(share.wanted(10, 0));
        assert!(share.wanted(100, 51));
        assert!(!share.wanted(100, 50));
        assert!(Thresholds::default().wanted(100, 11));
        assert!(!Thresholds::default().wanted(100, 10));
    }
}
