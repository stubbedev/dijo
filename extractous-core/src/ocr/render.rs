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
    /// The pages of a PDF, opened with `password` as Tika's PDF parser opens it.
    pub(crate) fn open(data: Arc<Vec<u8>>, password: &str) -> Result<Self, String> {
        let pdf = Pdf::new_with_password(data, password)
            .map_err(|e| format!("cannot read the PDF to render it: {e:?}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF of one page that shows an image of `width` by `height` pixels at 300 dpi, of the
    /// stream `data` and the image dictionary's further entries `entries`.
    fn showing(data: &[u8], entries: &str, width: u32, height: u32) -> Vec<u8> {
        let points = |pixels: u32| f64::from(pixels) * POINTS_PER_INCH / DPI;
        let (page_width, page_height) = (points(width), points(height));
        let content = format!("q {page_width} 0 0 {page_height} 0 0 cm /Im0 Do Q");
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_width} {page_height}] \
                 /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"
            )
            .into_bytes(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
            [
                format!(
                    "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
                     {entries} /Length {} >>\nstream\n",
                    data.len()
                )
                .as_bytes(),
                data,
                b"\nendstream",
            ]
            .concat(),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            pdf.extend_from_slice(object);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    /// Renders the page, of `expected`'s size but for a row or column the page's size in points
    /// rounds away, and counts its pixels more than 64 levels off `expected`'s.
    fn differing(pdf: Vec<u8>, expected: &GrayImage) -> usize {
        let rendered = PdfPages::open(Arc::new(pdf), "")
            .unwrap()
            .render(0)
            .unwrap();
        assert!(
            rendered.width().abs_diff(expected.width()) <= 1
                && rendered.height().abs_diff(expected.height()) <= 1,
            "{:?} rendered for {:?}",
            rendered.dimensions(),
            expected.dimensions()
        );
        let mut off = 0;
        for y in 0..rendered.height().min(expected.height()) {
            for x in 0..rendered.width().min(expected.width()) {
                if rendered.get_pixel(x, y).0[0].abs_diff(expected.get_pixel(x, y).0[0]) > 64 {
                    off += 1;
                }
            }
        }
        off
    }

    /// Scanned PDFs code their pages as JBIG2 and JPEG 2000 images, which hayro decodes with
    /// hayro-jbig2 and hayro-jpeg2000: the corpus's scans render as their decoded images, but
    /// for the edges of the strokes that resampling at the page's resolution moves.
    #[test]
    fn renders_jbig2_and_jpeg_2000_images_as_they_decode() {
        let bitmap = image::open("../quality/corpus/prose-scan.pbm")
            .unwrap()
            .to_luma8();
        let (width, height) = bitmap.dimensions();
        let pixels = (width * height) as usize;
        let jbig2 = std::fs::read("../quality/corpus/prose-scan.jb2").unwrap();
        let embedded = &jbig2[13..];
        let pdf = showing(
            embedded,
            "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode",
            width,
            height,
        );
        let off = differing(pdf, &bitmap);
        assert!(off * 100 < pixels, "{off} pixels of the JBIG2 page differ");

        let scan = image::open("../quality/corpus/prose-scan-300dpi.png")
            .unwrap()
            .to_luma8();
        let jpeg2000 = std::fs::read("../quality/corpus/prose-scan.jp2").unwrap();
        let pdf = showing(&jpeg2000, "/Filter /JPXDecode", width, height);
        let off = differing(pdf, &scan);
        assert!(
            off * 100 < pixels,
            "{off} pixels of the JPEG 2000 page differ"
        );
    }
}
