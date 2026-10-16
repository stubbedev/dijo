//! Images decoded for OCR, as our Java `PreparingOcrParser` has ImageIO decode them: every page
//! of a TIFF, the first image of anything else, in grey as Java2D draws colour into grey.
//! Transparency is ignored, as tesseract ignores it. JPEG 2000 images, which tesseract reads
//! where Java has no decoder, are decoded by hayro-jpeg2000, JBIG2 images, which jbig2-imageio
//! decodes for Java, by hayro-jbig2, AVIF images by rav1d, and HEIC and HEIF images by libheif,
//! which decodes all three for Java.

use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat};
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use tiff::ColorType;

use super::recognize::MAX_PIXELS;
use super::{avif, heif, jbig2, raster};

/// The formats this module decodes, by the media types Tika's detection gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Image(ImageFormat),
    Tiff,
    Photoshop,
    Pcx,
    Jpeg2000,
    Jbig2,
    Avif,
    Heif,
}

impl Format {
    /// The format of a media type, without its parameters, or none for one this module does
    /// not decode.
    pub(crate) fn of(media_type: &str) -> Option<Self> {
        Some(match media_type {
            "image/png" => Self::Image(ImageFormat::Png),
            "image/jpeg" => Self::Image(ImageFormat::Jpeg),
            "image/gif" => Self::Image(ImageFormat::Gif),
            "image/bmp" => Self::Image(ImageFormat::Bmp),
            "image/webp" => Self::Image(ImageFormat::WebP),
            "image/x-tga" => Self::Image(ImageFormat::Tga),
            "image/x-portable-bitmap"
            | "image/x-portable-graymap"
            | "image/x-portable-pixmap"
            | "image/x-portable-anymap" => Self::Image(ImageFormat::Pnm),
            "image/tiff" => Self::Tiff,
            "image/vnd.adobe.photoshop" => Self::Photoshop,
            "image/vnd.zbrush.pcx" => Self::Pcx,
            "image/jp2" | "image/jpx" => Self::Jpeg2000,
            "image/x-jbig2" => Self::Jbig2,
            "image/avif" => Self::Avif,
            "image/heic" | "image/heic-sequence" | "image/heif" | "image/heif-sequence" => {
                Self::Heif
            }
            _ => return None,
        })
    }
}

/// A decoded image or page, or why it could not be decoded.
pub(crate) type Page = Result<GrayImage, String>;

/// The pages of a TIFF of more than one page.
pub(crate) struct TiffPages<'a> {
    input: &'a [u8],
    count: usize,
}

impl TiffPages<'_> {
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// A page, decoded unless it has more than [`MAX_PIXELS`] pixels or is damaged.
    pub(crate) fn page(&self, index: usize) -> Page {
        let mut decoder = tiff_decoder(self.input)?;
        decoder
            .seek_to_image(index)
            .map_err(|e| format!("cannot find page {}: {e}", index + 1))?;
        tiff_page(&mut decoder)
    }
}

fn tiff_decoder(input: &[u8]) -> Result<Decoder<Cursor<&[u8]>>, String> {
    Ok(Decoder::new(Cursor::new(input))
        .map_err(|e| format!("cannot read the TIFF: {e}"))?
        .with_limits(tiff::decoder::Limits::unlimited()))
}

/// An image file's decoded content.
pub(crate) enum Decoded<'a> {
    /// One image.
    Single(GrayImage),
    /// The pages of a TIFF of more than one page.
    Pages(TiffPages<'a>),
}

/// Why an image file was not decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Undecoded {
    /// No decoder of its format is bundled or installed, as the static executable links them.
    #[cfg(not(feature = "static-image"))]
    NoDecoder,
    /// It could not be decoded, for this reason.
    Failed(String),
}

impl From<String> for Undecoded {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

/// Decodes an image file of a format, any program decoding it given `timeout`, or tells why it
/// cannot.
pub(crate) fn decode(
    input: &[u8],
    format: Format,
    timeout: Option<std::time::Duration>,
) -> Result<Decoded<'_>, Undecoded> {
    match format {
        Format::Image(format) => {
            let image = image::load_from_memory_with_format(input, format)
                .map_err(|e| format!("cannot decode the image: {e}"))?;
            Ok(Decoded::Single(gray(image)))
        }
        Format::Tiff => {
            let mut decoder = tiff_decoder(input)?;
            let mut count = 1;
            while decoder.more_images() {
                decoder
                    .next_image()
                    .map_err(|e| format!("cannot read the TIFF's directories: {e}"))?;
                count += 1;
            }
            decoder
                .seek_to_image(0)
                .map_err(|e| format!("cannot read the TIFF's first page: {e}"))?;
            if count > 1 {
                return Ok(Decoded::Pages(TiffPages { input, count }));
            }
            Ok(Decoded::Single(tiff_page(&mut decoder)?))
        }
        Format::Photoshop => Ok(Decoded::Single(raster::photoshop(input)?)),
        Format::Pcx => Ok(Decoded::Single(raster::pcx(input)?)),
        Format::Jpeg2000 => Ok(Decoded::Single(jpeg2000(input)?)),
        Format::Jbig2 => Ok(Decoded::Single(jbig2::decode(input)?)),
        Format::Avif => Ok(Decoded::Single(avif::decode(input)?)),
        Format::Heif => heif::decode(input, timeout).map(Decoded::Single),
    }
}

/// Fails for an image of more than [`MAX_PIXELS`] pixels, which is not decoded.
pub(crate) fn within_limits(width: u32, height: u32) -> Result<(), String> {
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("the image has {width}x{height} pixels"));
    }
    Ok(())
}

/// A JPEG 2000 image or codestream, its colours converted as its colour space says.
fn jpeg2000(input: &[u8]) -> Result<GrayImage, String> {
    let image = hayro_jpeg2000::Image::new(input, &hayro_jpeg2000::DecodeSettings::default())
        .map_err(|e| format!("cannot read the JPEG 2000 image: {e}"))?;
    within_limits(image.width(), image.height())?;
    let image = DynamicImage::from_decoder(image)
        .map_err(|e| format!("cannot decode the JPEG 2000 image: {e}"))?;
    Ok(gray(image))
}

/// The grey level Java2D draws a colour as: ITU-R BT.601 luma, rounded.
pub(crate) fn luma(red: u8, green: u8, blue: u8) -> u8 {
    ((77 * u32::from(red) + 150 * u32::from(green) + 29 * u32::from(blue) + 128) >> 8) as u8
}

/// An image in grey.
pub(crate) fn gray(image: DynamicImage) -> GrayImage {
    match image {
        DynamicImage::ImageLuma8(gray) => gray,
        DynamicImage::ImageLuma16(gray) => {
            GrayImage::from_fn(gray.width(), gray.height(), |x, y| {
                image::Luma([(gray.get_pixel(x, y).0[0] >> 8) as u8])
            })
        }
        DynamicImage::ImageLumaA8(_) | DynamicImage::ImageLumaA16(_) => image.to_luma8(),
        other => {
            let rgb = other.to_rgb8();
            GrayImage::from_fn(rgb.width(), rgb.height(), |x, y| {
                let [red, green, blue] = rgb.get_pixel(x, y).0;
                image::Luma([luma(red, green, blue)])
            })
        }
    }
}

/// The page a TIFF decoder is at, in grey.
fn tiff_page(decoder: &mut Decoder<Cursor<&[u8]>>) -> Page {
    let (width, height) = decoder
        .dimensions()
        .map_err(|e| format!("cannot read the page's size: {e}"))?;
    within_limits(width, height)?;
    let color = decoder
        .colortype()
        .map_err(|e| format!("cannot read the page's colours: {e}"))?;
    let palette = match color {
        ColorType::Palette(_) => Some(colour_map(decoder)?),
        _ => None,
    };
    let samples = decoder
        .read_image()
        .map_err(|e| format!("cannot decode the page: {e}"))?;
    let levels = match samples {
        DecodingResult::U8(bytes) => bytes,
        DecodingResult::U16(words) => words.iter().map(|&word| (word >> 8) as u8).collect(),
        _ => return Err(format!("cannot read samples of {color:?}")),
    };
    let width = width as usize;
    let gray: Vec<u8> = match color {
        ColorType::Gray(8 | 16) => levels,
        ColorType::Gray(bits @ (1 | 2 | 4)) => {
            let max = (1u32 << bits) - 1;
            unpack(&levels, width, bits)
                .map(|sample| (u32::from(sample) * 255 / max) as u8)
                .collect()
        }
        ColorType::GrayA(8 | 16) => levels
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pixel| pixel[0])
            .collect(),
        ColorType::RGB(8 | 16) => levels
            .as_chunks::<3>()
            .0
            .iter()
            .map(|pixel| luma(pixel[0], pixel[1], pixel[2]))
            .collect(),
        ColorType::RGBA(8 | 16) => levels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| luma(pixel[0], pixel[1], pixel[2]))
            .collect(),
        ColorType::CMYK(8) => levels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| {
                let ink = |colour: u8| {
                    ((255 - u32::from(colour)) * (255 - u32::from(pixel[3])) / 255) as u8
                };
                luma(ink(pixel[0]), ink(pixel[1]), ink(pixel[2]))
            })
            .collect(),
        ColorType::Palette(bits @ (1 | 2 | 4 | 8)) => {
            let palette = palette.expect("a palette page has its colour map");
            unpack(&levels, width, bits)
                .map(|index| {
                    palette.get(usize::from(index)).copied().ok_or_else(|| {
                        format!(
                            "a pixel of colour {index}, which the colour map of {} lacks",
                            palette.len()
                        )
                    })
                })
                .collect::<Result<_, _>>()?
        }
        other => return Err(format!("cannot read pages of {other:?}")),
    };
    GrayImage::from_raw(width as u32, height, gray)
        .ok_or_else(|| "the page holds fewer samples than its size needs".to_owned())
}

/// The grey level of each entry of a palette page's colour map.
fn colour_map(decoder: &mut Decoder<Cursor<&[u8]>>) -> Result<Vec<u8>, String> {
    let map = decoder
        .find_tag(Tag::ColorMap)
        .map_err(|e| format!("cannot read the colour map: {e}"))?
        .ok_or("a palette page without a colour map")?
        .into_u16_vec()
        .map_err(|e| format!("cannot read the colour map: {e}"))?;
    let entries = map.len() / 3;
    Ok((0..entries)
        .map(|index| {
            let channel = |offset: usize| (map[offset * entries + index] >> 8) as u8;
            luma(channel(0), channel(1), channel(2))
        })
        .collect())
}

/// The samples of rows of `bits` bits a sample, each row starting on a byte.
fn unpack(packed: &[u8], width: usize, bits: u8) -> impl Iterator<Item = u8> + '_ {
    let bits = usize::from(bits);
    let row_bytes = (width * bits).div_ceil(8);
    let mask = ((1u16 << bits) - 1) as u8;
    packed.chunks_exact(row_bytes).flat_map(move |row| {
        (0..width).map(move |x| {
            let bit = x * bits;
            (row[bit / 8] >> (8 - bits - bit % 8)) & mask
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_colours_in_grey_as_java2d_does() {
        assert_eq!(luma(255, 255, 255), 255);
        assert_eq!(luma(0, 0, 0), 0);
        assert_eq!(luma(255, 0, 0), 77);
    }

    /// The corpus made its other scans of the PNG's, in grey: the Photoshop file as it is, the
    /// PCX image reduced to a palette, and the rest compressed with loss. Each decodes to the
    /// PNG's size with under one pixel in a hundred more than its format's levels off.
    #[test]
    fn decodes_the_corpus_scans_as_the_png_holds_them() {
        let png = image::open("../quality/corpus/prose-scan-300dpi.png")
            .unwrap()
            .to_luma8();
        for (file, format, levels) in [
            ("prose-scan.psd", Format::Photoshop, 2),
            ("prose-scan.pcx", Format::Pcx, 2),
            ("prose-scan.jp2", Format::Jpeg2000, 16),
            ("prose-scan.avif", Format::Avif, 2),
            ("prose-scan.heic", Format::Heif, 2),
        ] {
            let input = std::fs::read(format!("../quality/corpus/{file}")).unwrap();
            let Decoded::Single(decoded) =
                decode(&input, format, None).unwrap_or_else(|e| panic!("{file}: {e:?}"))
            else {
                panic!("{file} is one image");
            };
            assert_eq!(decoded.dimensions(), png.dimensions(), "{file}");
            let differing = decoded
                .pixels()
                .zip(png.pixels())
                .filter(|(a, b)| a.0[0].abs_diff(b.0[0]) > levels)
                .count();
            assert!(
                differing * 100 < (png.width() * png.height()) as usize,
                "{file}: {differing} pixels differ"
            );
        }
    }

    #[test]
    fn unpacks_bilevel_rows() {
        let samples: Vec<u8> = unpack(&[0b1010_0000, 0b0100_0000], 3, 1).collect();
        assert_eq!(samples, [1, 0, 1, 0, 1, 0]);
    }
}
