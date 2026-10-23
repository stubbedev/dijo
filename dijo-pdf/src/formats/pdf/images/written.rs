//! The images Java's own writers write in Tika, written with the `image` crate's encoders
//! instead (#125): a JPEG in a colour space other than grey or RGB, which PDFBox decodes and
//! `ImageIOUtil` encodes anew, at 72 dots an inch and the highest quality, and a CCITT image,
//! which it writes as a black and white TIFF, here of 8-bit grey levels, uncompressed, where
//! Java compresses it with CCITT T.6.

use std::io::Cursor;

use image::codecs::jpeg::{JpegEncoder, PixelDensity};
use image::codecs::tiff::TiffEncoder;
use image::{ExtendedColorType, GrayImage, ImageEncoder, Luma, Rgb, Rgba};

use crate::ocr::bitmap::Bitmap;

/// `ImageIOUtil.writeImage`'s resolution.
const DOTS_PER_INCH: u16 = 72;

/// The image as a JPEG of quality 100 at 72 dots an inch, alpha left out.
pub(super) fn jpeg(bitmap: &Bitmap) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, 100);
    encoder.set_pixel_density(PixelDensity::dpi(DOTS_PER_INCH));
    let (width, height) = bitmap.dimensions();
    let written = match bitmap {
        Bitmap::Gray(gray) => encoder.write_image(gray, width, height, ExtendedColorType::L8),
        Bitmap::Rgb(rgb) => encoder.write_image(rgb, width, height, ExtendedColorType::Rgb8),
        Bitmap::Rgba(rgba) => {
            let rgb: Vec<u8> = rgba
                .pixels()
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect();
            encoder.write_image(&rgb, width, height, ExtendedColorType::Rgb8)
        }
    };
    written.expect("an image in memory encodes as a JPEG");
    out
}

/// The image as a TIFF.
pub(super) fn tiff(bitmap: &Bitmap) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let encoder = TiffEncoder::new(&mut out);
    let (width, height) = bitmap.dimensions();
    let written = match bitmap {
        Bitmap::Gray(gray) => encoder.write_image(gray, width, height, ExtendedColorType::L8),
        Bitmap::Rgb(rgb) => encoder.write_image(rgb, width, height, ExtendedColorType::Rgb8),
        Bitmap::Rgba(rgba) => encoder.write_image(rgba, width, height, ExtendedColorType::Rgba8),
    };
    written.expect("an image in memory encodes as a TIFF");
    out.into_inner()
}

/// The image in black and white, as `writeToBuffer` copies it into a `TYPE_BYTE_BINARY` image
/// pixel by pixel, whose palette takes each colour for the nearer of black and white, black
/// on a tie.
pub(super) fn bitonal(bitmap: &Bitmap) -> Bitmap {
    let (width, height) = bitmap.dimensions();
    let levels: Vec<u8> = match bitmap {
        Bitmap::Gray(gray) => gray
            .pixels()
            .map(|&Luma([level])| bitonal_level([level; 3]))
            .collect(),
        Bitmap::Rgb(rgb) => rgb
            .pixels()
            .map(|&Rgb(colour)| bitonal_level(colour))
            .collect(),
        Bitmap::Rgba(rgba) => rgba
            .pixels()
            .map(|&Rgba([red, green, blue, _])| bitonal_level([red, green, blue]))
            .collect(),
    };
    Bitmap::Gray(GrayImage::from_raw(width, height, levels).expect("one level a pixel"))
}

/// The nearer of black and white to a colour, black on a tie.
fn bitonal_level([red, green, blue]: [u8; 3]) -> u8 {
    let distance = |to: i32| {
        [red, green, blue]
            .iter()
            .map(|&c| (i32::from(c) - to).pow(2))
            .sum::<i32>()
    };
    if distance(255) < distance(0) {
        255
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use image::{RgbImage, RgbaImage};

    use super::*;

    #[test]
    fn writes_each_pixel_bitonal_as_it_was_read_by_its_place() {
        let colour = |x: u32, y: u32| [(x * 37 + y) as u8, (y * 53 + x * 3) as u8, (x * y) as u8];
        let images = [
            Bitmap::Gray(GrayImage::from_fn(256, 3, |x, y| Luma([(x + y) as u8]))),
            Bitmap::Rgb(RgbImage::from_fn(41, 23, |x, y| Rgb(colour(x, y)))),
            Bitmap::Rgba(RgbaImage::from_fn(41, 23, |x, y| {
                let [red, green, blue] = colour(x, y);
                Rgba([red, green, blue, (x * 11) as u8])
            })),
            Bitmap::Rgb(RgbImage::new(0, 5)),
            Bitmap::Gray(GrayImage::new(7, 0)),
        ];
        for image in &images {
            let (width, height) = image.dimensions();
            let expected = GrayImage::from_fn(width, height, |x, y| {
                Luma([bitonal_level(match image {
                    Bitmap::Gray(gray) => [gray.get_pixel(x, y)[0]; 3],
                    Bitmap::Rgb(rgb) => rgb.get_pixel(x, y).0,
                    Bitmap::Rgba(rgba) => {
                        let [red, green, blue, _] = rgba.get_pixel(x, y).0;
                        [red, green, blue]
                    }
                })])
            });
            let Bitmap::Gray(written) = bitonal(image) else {
                panic!("a bitonal image is grey");
            };
            assert_eq!(written, expected);
        }
        assert_eq!(bitonal_level([127, 127, 127]), 0);
        assert_eq!(bitonal_level([128, 128, 128]), 255);
        assert_eq!(bitonal_level([255, 0, 0]), 0);
        assert_eq!(bitonal_level([255, 255, 0]), 255);
    }
}
