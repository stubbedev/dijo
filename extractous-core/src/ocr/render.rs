//! PDF pages rendered for OCR, as our Java `TextLayerCheckingPdfParser` has PDFBox render them:
//! the crop box at Tika's 300 dpi in grey, at a lower resolution where the page would have more
//! than [`MAX_PIXELS`] pixels. hayro renders them, a renderer in pure Rust.

use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use image::GrayImage;

use super::bitmap::luma;
use super::recognize::MAX_PIXELS;

/// `RenderSettings`' default resolution.
const DPI: f64 = 300.0;
const POINTS_PER_INCH: f64 = 72.0;

/// A PDF's pages, as the renderer reads them.
pub(crate) struct PdfPages {
    pdf: Pdf,
}

impl PdfPages {
    /// The pages of a PDF, opened with the empty password as Tika's PDF parser opens it.
    pub(crate) fn open(data: Arc<Vec<u8>>) -> Result<Self, String> {
        let pdf = Pdf::new(data).map_err(|e| format!("cannot read the PDF to render it: {e:?}"))?;
        Ok(Self { pdf })
    }

    pub(crate) fn count(&self) -> usize {
        self.pdf.pages().len()
    }

    /// A page rendered in grey: at 300 dpi, or at the resolution that gives it [`MAX_PIXELS`]
    /// pixels when that is lower.
    pub(crate) fn render(&self, index: usize) -> Result<GrayImage, String> {
        let page = self
            .pdf
            .pages()
            .get(index)
            .ok_or_else(|| format!("the PDF has no page {}", index + 1))?;
        let crop_box = page.intersected_crop_box();
        let square_inches =
            crop_box.width() / POINTS_PER_INCH * (crop_box.height() / POINTS_PER_INCH);
        let dpi = DPI.min((MAX_PIXELS as f64 / square_inches).sqrt());
        let scale = dpi as f32 / POINTS_PER_INCH as f32;
        let pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let gray = pixmap
            .data()
            .iter()
            .map(|pixel| luma(pixel.r, pixel.g, pixel.b))
            .collect();
        GrayImage::from_raw(u32::from(pixmap.width()), u32::from(pixmap.height()), gray)
            .ok_or_else(|| format!("page {} rendered to fewer pixels than its size", index + 1))
    }
}
