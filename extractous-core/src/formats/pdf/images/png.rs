//! The PNG file of an image read from a page, at 72 dpi as PDFBox's `ImageIOUtil.writeImage`
//! records it, its rows filtered and compressed by the `png` crate. The JDK's PNG writer, which
//! PDFBox drives, leaves every row unfiltered, its filter check inverted (#131).

use png::{BitDepth, ColorType, Compression, Encoder, PixelDimensions, Unit};

use crate::ocr::bitmap::Bitmap;

/// 72 dpi in pixels a metre, as `ImageIOUtil` rounds it.
const PIXELS_PER_METRE: u32 = 2835;

/// The PNG file of `image`.
pub(super) fn write(image: &Bitmap) -> Vec<u8> {
    let (width, height) = image.dimensions();
    let (color, samples): (ColorType, &[u8]) = match image {
        Bitmap::Gray(gray) => (ColorType::Grayscale, gray.as_raw()),
        Bitmap::Rgb(rgb) => (ColorType::Rgb, rgb.as_raw()),
        Bitmap::Rgba(rgba) => (ColorType::Rgba, rgba.as_raw()),
    };
    let mut out = Vec::new();
    let mut encoder = Encoder::new(&mut out, width, height);
    encoder.set_color(color);
    encoder.set_depth(BitDepth::Eight);
    encoder.set_compression(Compression::High);
    encoder.set_pixel_dims(Some(PixelDimensions {
        xppu: PIXELS_PER_METRE,
        yppu: PIXELS_PER_METRE,
        unit: Unit::Meter,
    }));
    let mut writer = encoder
        .write_header()
        .expect("a PNG header of an image's own dimensions writes to memory");
    writer
        .write_image_data(samples)
        .expect("an image's own samples write to memory as PNG");
    writer.finish().expect("a PNG file in memory finishes");
    out
}

#[cfg(test)]
mod tests {
    use image::{GrayImage, Luma, Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    /// Samples that change smoothly along each row, which filtering makes compress well.
    fn gradient(x: u32, y: u32) -> u8 {
        (x * 3 + y * 5) as u8
    }

    #[test]
    fn writes_filtered_images_that_read_back_as_they_were() {
        let images = [
            Bitmap::Gray(GrayImage::from_fn(97, 61, |x, y| Luma([gradient(x, y)]))),
            Bitmap::Rgb(RgbImage::from_fn(97, 61, |x, y| {
                Rgb([gradient(x, y), gradient(y, x), 128])
            })),
            Bitmap::Rgba(RgbaImage::from_fn(97, 61, |x, y| {
                Rgba([gradient(x, y), gradient(y, x), 7, gradient(x, x)])
            })),
        ];
        for image in &images {
            let written = write(image);
            let decoder = png::Decoder::new(std::io::Cursor::new(&written));
            let mut reader = decoder.read_info().unwrap();
            let dims = reader.info().pixel_dims.unwrap();
            assert_eq!(
                (dims.xppu, dims.yppu, dims.unit),
                (PIXELS_PER_METRE, PIXELS_PER_METRE, Unit::Meter)
            );
            let mut samples = vec![0; reader.output_buffer_size().unwrap()];
            reader.next_frame(&mut samples).unwrap();
            let raw: &[u8] = match image {
                Bitmap::Gray(gray) => gray.as_raw(),
                Bitmap::Rgb(rgb) => rgb.as_raw(),
                Bitmap::Rgba(rgba) => rgba.as_raw(),
            };
            assert_eq!(samples, raw);
            let mut unfiltered = Vec::new();
            for row in raw.chunks(raw.len() / 61) {
                unfiltered.push(0);
                unfiltered.extend_from_slice(row);
            }
            let mut deflated =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            std::io::Write::write_all(&mut deflated, &unfiltered).unwrap();
            assert!(
                written.len() < deflated.finish().unwrap().len(),
                "{} bytes",
                written.len()
            );
        }
    }
}
