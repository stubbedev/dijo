//! Whether Tika's path would OCR a page, which the port leaves to Tika: by
//! `AbstractPDF2XHTML.ocrWanted` under the automatic strategy, or, when tesseract runs, by our
//! `TextLayerCheckingPdfParser`'s doubt about the page's text.

use super::{quality, Unported};

/// `PagesConfig.Auto`: a page of at most `total_chars` glyphs is OCRed, and so is one with more
/// glyphs without characters than `unmapped`, a share of the page's glyphs below 1 and a count
/// from 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Thresholds {
    pub(super) total_chars: i32,
    pub(super) unmapped: f32,
}

/// `PagesConfig.Auto.defaults()`.
const DEFAULT: Thresholds = Thresholds {
    total_chars: 10,
    unmapped: 10.0,
};

impl Thresholds {
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

/// How Tika's path decides whether to OCR a page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum OcrCheck {
    /// As `AbstractPDF2XHTML.ocrWanted` decides it.
    Wanted(Thresholds),
    /// As our `TextLayerCheckingPdfParser` decides it from the page's text, holding each page
    /// back until it ends, when tesseract runs.
    Assessed(Thresholds),
}

impl OcrCheck {
    /// The check Tika's path makes, with tesseract running or not.
    pub(super) fn of(ocr_runs: bool) -> Self {
        if ocr_runs {
            Self::Assessed(DEFAULT)
        } else {
            Self::Wanted(DEFAULT)
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
    }
}
