//! Whether Tika's path would OCR a page, which the port leaves to Tika: never under `NO_OCR`;
//! under `AUTO`, by `AbstractPDF2XHTML.ocrWanted` with the thresholds
//! `PdfConfig.setOcrStrategyAuto` sets, or, when tesseract runs, by our
//! `TextLayerCheckingPdfParser`'s doubt about the page's text.

use super::{quality, Unported};
use crate::config::{PdfOcrStrategy, PdfParserConfig};

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

    /// Why Tika would OCR page `page` of `glyphs` glyphs, `unmapped` of them without
    /// characters, whose `div` holds `text` when the pages are held, if it would.
    pub(super) fn deferral(
        self,
        page: usize,
        glyphs: i32,
        unmapped: i32,
        text: Option<&str>,
    ) -> Result<(), Unported> {
        let ocr = || {
            Unported(format!(
                "page {page} shows {glyphs} glyphs, {unmapped} of them without characters, \
                 which Tika OCRs"
            ))
        };
        let thresholds = match self {
            Self::Never => return Ok(()),
            Self::Wanted(thresholds) => {
                return match thresholds.ocr_wanted(glyphs, unmapped) {
                    true => Err(ocr()),
                    false => Ok(()),
                };
            }
            Self::Assessed(thresholds) => thresholds,
        };
        if glyphs <= thresholds.total_chars {
            return Err(ocr());
        }
        let text = text.expect("a held page's text is kept for its assessment");
        let assessment = quality::assess(text, glyphs, unmapped, thresholds.unmapped);
        if assessment.badness >= quality::DOUBTFUL {
            return Err(Unported(format!(
                "the text layer of page {page} is doubtful ({}), which Tika OCRs",
                assessment.fault
            )));
        }
        Ok(())
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
}
