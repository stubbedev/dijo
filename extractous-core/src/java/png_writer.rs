//! The JDK's `PNGImageWriter`, as PDFBox's `ImageIOUtil.writeImage` drives it: at 72 dpi, which
//! the writer records as 2835 pixels a metre, compressed at quality 0, which is deflate level 9,
//! every row unfiltered, as the writer's `RowFilter` leaves every image without a palette, and
//! the compressed data cut into `IDAT` chunks of 32768 bytes.

use super::deflater::Deflater;
use crate::compress::crc32;
use crate::ocr::bitmap::Bitmap;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
/// `IDATOutputStream`'s chunk length.
const IDAT_LENGTH: usize = 32768;
/// `(int)(dpi / 25.4f * 1000 + 0.5F)` for 72 dpi.
const PIXELS_PER_METRE: u32 = 2835;
const COLOR_GRAY: u8 = 0;
const COLOR_RGB: u8 = 2;
const COLOR_RGB_ALPHA: u8 = 6;

/// `writeImage(image, "png", out)`: the PNG file of a `TYPE_BYTE_GRAY`, `TYPE_INT_RGB` or
/// `TYPE_INT_ARGB` image.
pub(crate) fn write(image: &Bitmap) -> Vec<u8> {
    let (width, height) = image.dimensions();
    let (color_type, samples): (u8, &[u8]) = match image {
        Bitmap::Gray(gray) => (COLOR_GRAY, gray.as_raw()),
        Bitmap::Rgb(rgb) => (COLOR_RGB, rgb.as_raw()),
        Bitmap::Rgba(rgba) => (COLOR_RGB_ALPHA, rgba.as_raw()),
    };
    let mut out = SIGNATURE.to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, color_type, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    let mut physical = Vec::with_capacity(9);
    physical.extend_from_slice(&PIXELS_PER_METRE.to_be_bytes());
    physical.extend_from_slice(&PIXELS_PER_METRE.to_be_bytes());
    physical.push(1);
    chunk(&mut out, b"pHYs", &physical);
    let mut deflater = Deflater::best_compression();
    let row_length = samples.len() / height.max(1) as usize;
    for row in samples.chunks_exact(row_length.max(1)) {
        deflater.write(&[0]);
        deflater.write(row);
    }
    let compressed = deflater.finish();
    for data in compressed.chunks(IDAT_LENGTH) {
        chunk(&mut out, b"IDAT", data);
    }
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A chunk of `kind` holding `data`, with its CRC.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    let length = u32::try_from(data.len()).expect("a chunk is at most 32768 bytes");
    out.extend_from_slice(&length.to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use image::{GrayImage, Luma, Rgb, RgbImage, Rgba, RgbaImage};
    use sha2::{Digest, Sha256};

    use super::*;

    /// `java.awt.image.BufferedImage`'s types.
    const TYPE_INT_RGB: u32 = 1;
    const TYPE_INT_ARGB: u32 = 2;
    const TYPE_BYTE_GRAY: u32 = 10;

    /// The generator of `ImageWriterReference.java`.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
    }

    fn input(kind: u64, length: usize) -> Vec<u8> {
        let mut lcg = Lcg(kind + 1);
        (0..length)
            .map(|index| {
                let x = lcg.next();
                match kind {
                    0 => (x >> 56) as u8,
                    1 => (x >> 61) as u8,
                    2 => ((index / 300) % 7 * 30) as u8,
                    _ => match (x >> 58) % 27 {
                        26 => b' ',
                        letter => b'a' + (letter % 6) as u8,
                    },
                }
            })
            .collect()
    }

    fn deflated(data: &[u8], piece: usize) -> Vec<u8> {
        let mut deflater = Deflater::best_compression();
        if piece == 0 {
            deflater.write(data);
        } else {
            for chunk in data.chunks(piece) {
                deflater.write(&chunk[..1]);
                deflater.write(&chunk[1..]);
            }
        }
        deflater.finish()
    }

    fn image(kind: u32, width: u32, height: u32) -> Bitmap {
        let mut lcg = Lcg(u64::from(kind) * 1000 + u64::from(width));
        let mut pixels = Vec::new();
        for _ in 0..height {
            for x in 0..width {
                let mask = if x % 7 == 0 { 0xffff_ffff } else { 0x3f3f_3f3f };
                pixels.push(((lcg.next() >> 32) & mask) as u32);
            }
        }
        let at = |x: u32, y: u32| pixels[(y * width + x) as usize];
        match kind {
            TYPE_BYTE_GRAY => Bitmap::Gray(GrayImage::from_fn(width, height, |x, y| {
                Luma([at(x, y) as u8])
            })),
            TYPE_INT_RGB => Bitmap::Rgb(RgbImage::from_fn(width, height, |x, y| {
                let [_, red, green, blue] = at(x, y).to_be_bytes();
                Rgb([red, green, blue])
            })),
            TYPE_INT_ARGB => Bitmap::Rgba(RgbaImage::from_fn(width, height, |x, y| {
                let [alpha, red, green, blue] = at(x, y).to_be_bytes();
                Rgba([red, green, blue, alpha])
            })),
            other => panic!("no image type {other}"),
        }
    }

    fn digest(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn writes_what_the_jdks_writers_write() {
        let reference = include_str!("../../tests/fixtures/pdf/image-writers.tsv");
        let mut compared = 0;
        for line in reference.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split('\t').collect();
            let numbers: Vec<usize> = fields[1..4].iter().map(|n| n.parse().unwrap()).collect();
            let written = match fields[0] {
                "deflate" => deflated(&input(numbers[0] as u64, numbers[1]), numbers[2]),
                "png" => write(&image(
                    numbers[0] as u32,
                    numbers[1] as u32,
                    numbers[2] as u32,
                )),
                other => panic!("no writer {other}"),
            };
            assert_eq!(
                (written.len().to_string(), digest(&written)),
                (fields[4].to_owned(), fields[5].to_owned()),
                "{line}"
            );
            compared += 1;
        }
        assert_eq!(compared, 60);
    }
}
