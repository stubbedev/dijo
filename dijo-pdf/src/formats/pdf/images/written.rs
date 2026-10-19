//! The images Java's own writers write in Tika, written with the `image` crate's encoders
//! instead (#125): a JPEG in a colour space other than grey or RGB, which PDFBox decodes and
//! `ImageIOUtil` encodes anew, at 72 dots an inch and the highest quality, and a CCITT image,
//! which it writes as a black and white TIFF, here of 8-bit grey levels, uncompressed, where
//! Java compresses it with CCITT T.6.

use std::io::Cursor;

use image::codecs::jpeg::{JpegEncoder, PixelDensity};
use image::codecs::tiff::TiffEncoder;
use image::{ExtendedColorType, GrayImage, ImageEncoder};

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
    let level = |[red, green, blue]: [u8; 3]| {
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
    };
    Bitmap::Gray(GrayImage::from_fn(width, height, |x, y| {
        image::Luma([level(match bitmap {
            Bitmap::Gray(gray) => [gray.get_pixel(x, y)[0]; 3],
            Bitmap::Rgb(rgb) => rgb.get_pixel(x, y).0,
            Bitmap::Rgba(rgba) => {
                let [red, green, blue, _] = rgba.get_pixel(x, y).0;
                [red, green, blue]
            }
        })])
    }))
}
