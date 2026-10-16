//! Images decoded for OCR, as our Java `PreparingOcrParser` has ImageIO decode them: every page
//! of a TIFF, the first image of anything else, in grey as Java2D draws colour into grey.
//! Transparency is ignored, as tesseract ignores it.

use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat};
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use tiff::ColorType;

use super::raster;
use super::recognize::MAX_PIXELS;

/// The formats this module decodes, by the media types Tika's detection gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Image(ImageFormat),
    Tiff,
    Photoshop,
    Pcx,
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
            _ => return None,
        })
    }
}

/// A decoded image or page, or why it could not be decoded.
pub(crate) type Page = Result<GrayImage, String>;

/// The pages of a TIFF, decoded in turn.
pub(crate) struct TiffPages<'a> {
    decoder: Decoder<Cursor<&'a [u8]>>,
    count: usize,
}

impl TiffPages<'_> {
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// A page, decoded unless it has more than [`MAX_PIXELS`] pixels or is damaged.
    pub(crate) fn page(&mut self, index: usize) -> Page {
        self.decoder
            .seek_to_image(index)
            .map_err(|e| format!("cannot find page {}: {e}", index + 1))?;
        tiff_page(&mut self.decoder)
    }
}

/// An image file's decoded content.
pub(crate) enum Decoded<'a> {
    /// One image.
    Single(GrayImage),
    /// The pages of a TIFF of more than one page.
    Pages(Box<TiffPages<'a>>),
}

/// Decodes an image file of a format, or tells why it cannot.
pub(crate) fn decode(input: &[u8], format: Format) -> Result<Decoded<'_>, String> {
    match format {
        Format::Image(format) => {
            let image = image::load_from_memory_with_format(input, format)
                .map_err(|e| format!("cannot decode the image: {e}"))?;
            Ok(Decoded::Single(gray(image)))
        }
        Format::Tiff => {
            let mut decoder = Decoder::new(Cursor::new(input))
                .map_err(|e| format!("cannot read the TIFF: {e}"))?
                .with_limits(tiff::decoder::Limits::unlimited());
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
                return Ok(Decoded::Pages(Box::new(TiffPages { decoder, count })));
            }
            Ok(Decoded::Single(tiff_page(&mut decoder)?))
        }
        Format::Photoshop => raster::photoshop(input).map(Decoded::Single),
        Format::Pcx => raster::pcx(input).map(Decoded::Single),
    }
}

/// The grey level Java2D draws a colour as: ITU-R BT.601 luma, rounded.
pub(crate) fn luma(red: u8, green: u8, blue: u8) -> u8 {
    ((77 * u32::from(red) + 150 * u32::from(green) + 29 * u32::from(blue) + 128) >> 8) as u8
}

/// An image in grey.
fn gray(image: DynamicImage) -> GrayImage {
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
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("the page has {width}x{height} pixels"));
    }
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
        ColorType::GrayA(8 | 16) => levels.as_chunks::<2>().0.iter().map(|pixel| pixel[0]).collect(),
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
                .map(|index| palette.get(usize::from(index)).copied().unwrap_or(0))
                .collect()
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

    #[test]
    fn unpacks_bilevel_rows() {
        let samples: Vec<u8> = unpack(&[0b1010_0000, 0b0100_0000], 3, 1).collect();
        assert_eq!(samples, [1, 0, 1, 0, 1, 0]);
    }
}
