//! Images decoded for OCR, as our Java `PreparingOcrParser` has ImageIO decode them: every page
//! of a TIFF, the first image of anything else, in grey, colour or colour with alpha
//! ([`Bitmap`]). JPEG is decoded as the JDK's libjpeg decodes it ([`super::jpeg`]), its colours
//! converted from the ICC profile it embeds as the JDK's reader converts them ([`super::icc`]).
//! JPEG 2000 images, which tesseract reads where Java has no decoder, are decoded by
//! hayro-jpeg2000, JBIG2 images, which jbig2-imageio decodes for Java, by hayro-jbig2, AVIF
//! images by rav1d, and HEIC and HEIF images, and AVIF images of other codecs, by libheif, which
//! decodes all three for Java. JPEG XL images, which Tika does not read at all (#132), are
//! decoded by jxl-oxide. Each keeps its colours, as Java keeps them; JBIG2, of black and white
//! alone, is grey.
//!
//! An image too large to prepare ([`MAX_PIXELS`]) is still decoded, up to the largest tesseract
//! can hold ([`MAX_DECODED_PIXELS`]), and read as it is, as Java has tesseract read it; only a
//! page of a TIFF of several is not, as Java decodes none.

use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, RgbImage, RgbaImage};
use tiff::decoder::ifd::Value;
use tiff::decoder::{Decoder, DecodingSampleType, Limits};
use tiff::tags::Tag;
use tiff::{ColorType, TiffError};

use super::bitmap::{cmyk_rgb, Bitmap, Density};
use super::prepare::Orientation;
use super::recognize::MAX_PIXELS;
use super::{avif, heif, jbig2, raster};
use crate::input::Pages;

/// The formats this module decodes, by the media types Tika's detection gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Jpeg,
    Image(ImageFormat),
    Tiff,
    Photoshop,
    Pcx,
    Jpeg2000,
    Jbig2,
    Avif,
    Heif,
    Jxl,
}

impl Format {
    /// The format of a media type, without its parameters, or none for one this module does
    /// not decode.
    pub(crate) fn of(media_type: &str) -> Option<Self> {
        Some(match media_type {
            "image/png" => Self::Image(ImageFormat::Png),
            "image/jpeg" => Self::Jpeg,
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
            "image/jxl" => Self::Jxl,
            _ => return None,
        })
    }

    /// Whether `PreparingOcrParser` hands tesseract the file of an image of the format, which
    /// tesseract decodes itself, rather than the pixels Java decoded: for every format but
    /// those Java decodes for tesseract, Photoshop, PCX, TGA and JBIG2, HEIC, HEIF and AVIF,
    /// which libheif converts first, and JPEG XL, which tesseract does not read.
    fn read_by_tesseract(self) -> bool {
        !matches!(
            self,
            Self::Photoshop
                | Self::Pcx
                | Self::Image(ImageFormat::Tga)
                | Self::Jbig2
                | Self::Avif
                | Self::Heif
                | Self::Jxl
        )
    }
}

/// The most pixels an image is decoded with: those of the largest colour image Leptonica, and
/// so tesseract, holds, four bytes a pixel in under 2 GiB (`pixCreateHeader`).
pub(crate) const MAX_DECODED_PIXELS: u64 = ((1 << 31) - 1) / 4;

/// A decoded image or page, or why it could not be decoded.
pub(crate) type Page = Result<Bitmap, String>;

/// The pages of a TIFF of more than one page, and the pages of memory its file is mapped to.
pub(crate) struct TiffPages<'a> {
    input: &'a [u8],
    count: usize,
    memory: Pages<'a>,
}

impl<'a> TiffPages<'a> {
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// A reader of the pages, which reads each in turn from where it read the last.
    pub(crate) fn reader(&self) -> Result<TiffPageReader<'a>, String> {
        Ok(TiffPageReader {
            decoder: tiff_decoder(self.input)?,
            memory: self.memory,
        })
    }
}

/// A reader of the pages of a TIFF, one at a time.
pub(crate) struct TiffPageReader<'a> {
    decoder: Decoder<Cursor<&'a [u8]>>,
    memory: Pages<'a>,
}

impl TiffPageReader<'_> {
    /// A page, decoded unless it has more than [`MAX_PIXELS`] pixels or is damaged, with the
    /// pixels to the inch it records. The pages of a mapped file read to decode it are released.
    pub(crate) fn page(&mut self, index: usize) -> Result<(Bitmap, Option<Density>), String> {
        self.decoder
            .seek_to_image(index)
            .map_err(|e| format!("cannot find page {}: {e}", index + 1))?;
        let density = tiff_density(&mut self.decoder);
        let page = tiff_page(&mut self.decoder, MAX_PIXELS);
        self.memory.release();
        Ok((page?, density))
    }
}

/// The bytes a pixel of the widest page read takes: four samples of 16 bits.
const MOST_BYTES_A_PIXEL: u64 = 8;

/// A decoder of a TIFF that allocates for a tag's values or a compressed strip no more than
/// `input` can hold, as both are read from it.
fn tiff_decoder(input: &[u8]) -> Result<Decoder<Cursor<&[u8]>>, String> {
    let mut limits = Limits::unlimited();
    limits.decoding_buffer_size = input.len().saturating_mul(size_of::<Value>());
    limits.ifd_value_size = input.len();
    limits.intermediate_buffer_size = input.len();
    Ok(Decoder::new(Cursor::new(input))
        .map_err(|e| format!("cannot read the TIFF: {e}"))?
        .with_limits(limits))
}

/// An image file's decoded content.
pub(crate) enum Decoded<'a> {
    /// One image, its file when tesseract reads the file itself, and the pixels to the inch the
    /// file records.
    Single {
        bitmap: Bitmap,
        file: Option<&'a [u8]>,
        density: Option<Density>,
    },
    /// The pages of a TIFF of more than one page.
    Pages(TiffPages<'a>),
}

/// Why an image file was not decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Undecoded {
    /// No decoder of its format is bundled or installed, as the static executable links them.
    #[cfg(not(feature = "static"))]
    NoDecoder,
    /// It could not be decoded, for this reason.
    Failed(String),
    /// libheif, which Tika decodes it with as well, cannot decode it, failing with this
    /// message, which Tika throws.
    Rejected(String),
}

impl From<String> for Undecoded {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

/// Decodes an image file of a format, any program decoding it given `timeout`, or tells why it
/// cannot. The pages of a TIFF of several, mapped to the pages of `memory`, are decoded one at a
/// time as they are read.
pub(crate) fn decode<'a>(
    input: &'a [u8],
    memory: Pages<'a>,
    format: Format,
    timeout: Option<std::time::Duration>,
) -> Result<Decoded<'a>, Undecoded> {
    let single = |bitmap, density| Decoded::Single {
        bitmap,
        file: format.read_by_tesseract().then_some(input),
        density,
    };
    match format {
        Format::Jpeg => {
            let (bitmap, density) = jpeg(input)?;
            Ok(single(bitmap, density))
        }
        Format::Image(image_format) => {
            let image = image::load_from_memory_with_format(input, image_format)
                .map_err(|e| format!("cannot decode the image: {e}"))?;
            let density = match image_format {
                ImageFormat::Png => png_density(input),
                _ => None,
            };
            Ok(single(bitmap(image), density))
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
                return Ok(Decoded::Pages(TiffPages {
                    input,
                    count,
                    memory,
                }));
            }
            let density = tiff_density(&mut decoder);
            Ok(single(
                tiff_page(&mut decoder, MAX_DECODED_PIXELS)?,
                density,
            ))
        }
        Format::Photoshop => Ok(single(raster::photoshop(input)?, None)),
        Format::Pcx => Ok(single(raster::pcx(input)?, None)),
        Format::Jpeg2000 => Ok(single(jpeg2000(input)?, None)),
        Format::Jbig2 => Ok(single(Bitmap::Gray(jbig2::decode(input)?), None)),
        Format::Avif => match avif::decode(input) {
            Ok(avif::Avif::Decoded(bitmap)) => Ok(single(bitmap, None)),
            Ok(avif::Avif::OfOtherCodecs) | Err(_) => {
                heif::decode(input, timeout).map(|bitmap| single(bitmap, None))
            }
        },
        Format::Heif => heif::decode(input, timeout).map(|bitmap| single(bitmap, None)),
        Format::Jxl => Ok(single(jxl(input)?, None)),
    }
}

/// Fails for an image of more than `most` pixels, which is not decoded.
fn within(width: u32, height: u32, most: u64) -> Result<(), String> {
    if u64::from(width) * u64::from(height) > most {
        return Err(format!(
            "the image has {width}x{height} pixels, more than the {most} it may have"
        ));
    }
    Ok(())
}

/// Fails for an image of more than [`MAX_DECODED_PIXELS`] pixels, which is not decoded.
pub(crate) fn decodable(width: u32, height: u32) -> Result<(), String> {
    within(width, height, MAX_DECODED_PIXELS)
}

/// A JPEG 2000 image or codestream, its colours converted as its colour space says.
fn jpeg2000(input: &[u8]) -> Page {
    let image = hayro_jpeg2000::Image::new(input, &hayro_jpeg2000::DecodeSettings::default())
        .map_err(|e| format!("cannot read the JPEG 2000 image: {e}"))?;
    decodable(image.width(), image.height())?;
    let image = DynamicImage::from_decoder(image)
        .map_err(|e| format!("cannot decode the JPEG 2000 image: {e}"))?;
    Ok(bitmap(image))
}

/// A JPEG XL image's first frame, CMYK printed as sRGB, its pixels laid out as stored: the
/// image is turned as its orientation says where it is read, as every image is, so the turn
/// jxl-oxide renders it with is undone. jxl-oxide is given the codestream alone, read in place,
/// so that the file's other boxes are not read.
fn jxl(input: &[u8]) -> Page {
    let unreadable = |e: String| format!("cannot read the JPEG XL image: {e}");
    let container = crate::jxl::Container::read(input).map_err(unreadable)?;
    let image = jxl_oxide::JxlImage::builder()
        .read(container.codestream())
        .map_err(|e| unreadable(e.to_string()))?;
    let orientation = Orientation::of(image.image_header().metadata.orientation);
    let upright = jxl_rendered(image)?;
    Ok(match orientation {
        Some(orientation) => upright.oriented(orientation.undone()),
        None => upright,
    })
}

/// The first frame of the JPEG XL `image` as jxl-oxide renders it, the image and its rendering
/// freed once its samples are taken.
fn jxl_rendered(mut image: jxl_oxide::JxlImage) -> Page {
    decodable(image.width(), image.height())?;
    if image.pixel_format().has_black() {
        image.request_color_encoding(jxl_oxide::EnumColourEncoding::srgb(
            jxl_oxide::RenderingIntent::Relative,
        ));
    }
    let render = image
        .render_frame(0)
        .map_err(|e| format!("cannot decode the JPEG XL image: {e}"))?;
    let mut stream = render.stream();
    let (width, height) = (stream.width(), stream.height());
    let mut samples = vec![0u8; width as usize * height as usize * stream.channels() as usize];
    stream.write_to_buffer(&mut samples);
    let short = || "the JPEG XL image holds fewer samples than its size needs".to_owned();
    let format = image.pixel_format();
    drop(render);
    drop(image);
    match format {
        jxl_oxide::PixelFormat::Gray => GrayImage::from_raw(width, height, samples)
            .map(Bitmap::Gray)
            .ok_or_else(short),
        jxl_oxide::PixelFormat::Graya => image::GrayAlphaImage::from_raw(width, height, samples)
            .map(|gray_alpha| bitmap(DynamicImage::ImageLumaA8(gray_alpha)))
            .ok_or_else(short),
        jxl_oxide::PixelFormat::Rgb => RgbImage::from_raw(width, height, samples)
            .map(Bitmap::Rgb)
            .ok_or_else(short),
        jxl_oxide::PixelFormat::Rgba => RgbaImage::from_raw(width, height, samples)
            .map(Bitmap::Rgba)
            .ok_or_else(short),
        other @ (jxl_oxide::PixelFormat::Cmyk | jxl_oxide::PixelFormat::Cmyka) => {
            Err(format!("the JPEG XL image renders as {other:?}, not sRGB"))
        }
    }
}

/// A JPEG as the JDK decodes it: grey, or colours converted from the profile it embeds, with the
/// pixels to the inch its JFIF segment records. CMYK, which the JDK's reader refuses, is printed
/// with the CMYK profile it embeds as LittleCMS prints it, or else without a profile.
pub(crate) fn jpeg(input: &[u8]) -> Result<(Bitmap, Option<Density>), String> {
    let decoded = super::jpeg::decode(input)?;
    let density = decoded.density;
    let (width, height) = (decoded.width as u32, decoded.height as u32);
    let short = || "the JPEG holds fewer samples than its size needs".to_owned();
    let rgb = |samples| {
        RgbImage::from_raw(width, height, samples)
            .map(Bitmap::Rgb)
            .ok_or_else(short)
    };
    let bitmap = match decoded.samples {
        super::jpeg::Samples::Gray(samples) => GrayImage::from_raw(width, height, samples)
            .map(Bitmap::Gray)
            .ok_or_else(short),
        super::jpeg::Samples::Rgb(mut samples) => {
            if let Some(profile) = &decoded.icc_profile {
                super::icc::to_srgb(profile, &mut samples)
                    .map_err(|e| format!("cannot convert the JPEG's colours: {e}"))?;
            }
            rgb(samples)
        }
        super::jpeg::Samples::Cmyk(inks) => {
            let profile = decoded
                .icc_profile
                .as_deref()
                .filter(|profile| super::icc::accepted_components(profile) == Some(4))
                .and_then(|profile| super::icc::ToSrgb::new(profile, 4));
            rgb(match profile {
                Some(profile) => profile.convert(&inks),
                None => inks
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|&pixel| cmyk_rgb(pixel))
                    .collect(),
            })
        }
    }?;
    Ok((bitmap, density))
}

/// The pixels to the inch a PNG's pHYs chunk records in pixels to the metre. It comes before the
/// image data, so the chunks are read up to the first IDAT.
fn png_density(input: &[u8]) -> Option<Density> {
    const SIGNATURE: usize = 8;
    const METRE: u8 = 1;
    let mut chunks = input.get(SIGNATURE..)?;
    while let [l0, l1, l2, l3, k0, k1, k2, k3, rest @ ..] = chunks {
        let length = u32::from_be_bytes([*l0, *l1, *l2, *l3]) as usize;
        let data = rest.get(..length)?;
        match &[*k0, *k1, *k2, *k3] {
            b"pHYs" => {
                let [a0, a1, a2, a3, d0, d1, d2, d3, unit] = *data else {
                    return None;
                };
                let across = f64::from(u32::from_be_bytes([a0, a1, a2, a3]));
                let down = f64::from(u32::from_be_bytes([d0, d1, d2, d3]));
                return (unit == METRE)
                    .then(|| Density::per_centimetre(across / 100.0, down / 100.0))
                    .flatten();
            }
            b"IDAT" => return None,
            _ => chunks = rest.get(length + 4..)?,
        }
    }
    None
}

/// The pixels to the inch the page a TIFF decoder is at records in its resolution tags, to the
/// inch unless its unit says the centimetre.
fn tiff_density(decoder: &mut Decoder<Cursor<&[u8]>>) -> Option<Density> {
    const INCH: u16 = 2;
    const CENTIMETRE: u16 = 3;
    let mut resolution = |tag| match decoder.find_tag(tag).ok().flatten()? {
        Value::Rational(numerator, denominator) => {
            Some(f64::from(numerator) / f64::from(denominator))
        }
        _ => None,
    };
    let across = resolution(Tag::XResolution)?;
    let down = resolution(Tag::YResolution)?;
    match decoder
        .find_tag_unsigned::<u16>(Tag::ResolutionUnit)
        .ok()
        .flatten()
        .unwrap_or(INCH)
    {
        INCH => Density::per_inch(across, down),
        CENTIMETRE => Density::per_centimetre(across, down),
        _ => None,
    }
}

/// An image the `image` crate decoded, in the kind of bitmap ImageIO decodes it in: grey, colour
/// or colour with alpha, of 8 bits a sample.
pub(crate) fn bitmap(image: DynamicImage) -> Bitmap {
    match image {
        DynamicImage::ImageLuma8(gray) => Bitmap::Gray(gray),
        DynamicImage::ImageLuma16(gray) => {
            Bitmap::Gray(GrayImage::from_fn(gray.width(), gray.height(), |x, y| {
                image::Luma([(gray.get_pixel(x, y).0[0] >> 8) as u8])
            }))
        }
        DynamicImage::ImageRgb8(rgb) => Bitmap::Rgb(rgb),
        other if other.color().has_alpha() => Bitmap::Rgba(other.to_rgba8()),
        other => Bitmap::Rgb(other.to_rgb8()),
    }
}

/// The page a TIFF decoder is at, unless it has more than `most` pixels.
fn tiff_page(decoder: &mut Decoder<Cursor<&[u8]>>, most: u64) -> Page {
    let (width, height) = decoder
        .dimensions()
        .map_err(|e| format!("cannot read the page's size: {e}"))?;
    within(width, height, most)?;
    let color = decoder
        .colortype()
        .map_err(|e| format!("cannot read the page's colours: {e}"))?;
    let palette = match color {
        ColorType::Palette(_) => Some(colour_map(decoder)?),
        _ => None,
    };
    let layout = decoder
        .image_buffer_layout()
        .map_err(|e| format!("cannot decode the page: {e}"))?;
    let sixteen_bits = match layout.sample_type {
        Some(DecodingSampleType::U8) => false,
        Some(DecodingSampleType::U16) => true,
        _ => return Err(format!("cannot read samples of {color:?}")),
    };
    let length = if sixteen_bits {
        layout.len.next_multiple_of(2)
    } else {
        layout.len
    };
    if length as u64 > u64::from(width) * u64::from(height) * MOST_BYTES_A_PIXEL {
        return Err(format!(
            "cannot decode the page: {}",
            TiffError::LimitsExceeded
        ));
    }
    let mut samples = vec![0; length];
    decoder
        .read_image_bytes(&mut samples)
        .map_err(|e| format!("cannot decode the page: {e}"))?;
    let levels = if sixteen_bits {
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&word| (u16::from_ne_bytes(word) >> 8) as u8)
            .collect()
    } else {
        samples
    };
    let short = || "the page holds fewer samples than its size needs".to_owned();
    let gray = |levels: Vec<u8>| {
        GrayImage::from_raw(width, height, levels)
            .map(Bitmap::Gray)
            .ok_or_else(short)
    };
    let rgb = |samples: Vec<u8>| {
        RgbImage::from_raw(width, height, samples)
            .map(Bitmap::Rgb)
            .ok_or_else(short)
    };
    let rgba = |samples: Vec<u8>| {
        RgbaImage::from_raw(width, height, samples)
            .map(Bitmap::Rgba)
            .ok_or_else(short)
    };
    let columns = width as usize;
    match color {
        ColorType::Gray(8 | 16) => gray(levels),
        ColorType::Gray(bits @ (1 | 2 | 4)) => {
            let max = (1u32 << bits) - 1;
            gray(
                unpack(&levels, columns, bits)
                    .map(|sample| (u32::from(sample) * 255 / max) as u8)
                    .collect(),
            )
        }
        ColorType::GrayA(8 | 16) => rgba(
            levels
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|&[level, alpha]| [level, level, level, alpha])
                .collect(),
        ),
        ColorType::RGB(8 | 16) => rgb(levels),
        ColorType::RGBA(8 | 16) => rgba(levels),
        ColorType::CMYK(8) => rgb(levels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&pixel| cmyk_rgb(pixel))
            .collect()),
        ColorType::Palette(bits @ (1 | 2 | 4 | 8)) => {
            let palette = palette.expect("a palette page has its colour map");
            let colours: Vec<[u8; 3]> = unpack(&levels, columns, bits)
                .map(|index| {
                    palette.get(usize::from(index)).copied().ok_or_else(|| {
                        format!(
                            "a pixel of colour {index}, which the colour map of {} lacks",
                            palette.len()
                        )
                    })
                })
                .collect::<Result<_, _>>()?;
            rgb(colours.into_iter().flatten().collect())
        }
        other => Err(format!("cannot read pages of {other:?}")),
    }
}

/// The colour of each entry of a palette page's colour map.
fn colour_map(decoder: &mut Decoder<Cursor<&[u8]>>) -> Result<Vec<[u8; 3]>, String> {
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
            [channel(0), channel(1), channel(2)]
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
    use std::io::Read;
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};

    use super::*;

    /// The grey pixels ImageIO and Java2D give our Java `PreparingOcrParser` for the images of
    /// the corpora, written by `just ocr-image-reference`.
    const REFERENCE: &[u8] = include_bytes!("../../tests/fixtures/ocr/image-reference.tsv.gz");

    /// The reference's lines of the corpora with the given labels, each with the directory its
    /// label names.
    fn reference(corpora: &[(&str, PathBuf)]) -> Vec<(PathBuf, String, String)> {
        let mut lines = String::new();
        flate2::read::GzDecoder::new(REFERENCE)
            .read_to_string(&mut lines)
            .unwrap();
        lines
            .lines()
            .filter_map(|line| {
                let (name, expected) = line.split_once('\t').unwrap();
                let (label, path) = name.split_once('/').unwrap();
                let (_, root) = corpora.iter().find(|(corpus, _)| *corpus == label)?;
                Some((root.clone(), path.to_owned(), expected.to_owned()))
            })
            .collect()
    }

    /// The bytes of a file, or of an entry of a ZIP-based file: `path!entry`.
    fn image_bytes(root: &Path, path: &str) -> Vec<u8> {
        let Some((container, entry)) = path.split_once('!') else {
            return std::fs::read(root.join(path)).unwrap();
        };
        let file = std::fs::File::open(root.join(container)).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut bytes = Vec::new();
        archive
            .by_name(entry)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    /// What the native decoding gives an image, as the reference writes it.
    fn decoded_as_referenced(path: &str, bytes: &[u8]) -> String {
        let extension = path.rsplit('.').next().unwrap().to_ascii_lowercase();
        let media_type = match extension.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            other => panic!("{path}: no image of extension {other} is referenced"),
        };
        match decode(
            bytes,
            Pages::IN_MEMORY,
            Format::of(media_type).unwrap(),
            None,
        ) {
            Ok(Decoded::Single { bitmap, .. }) => {
                let gray = bitmap.gray();
                let digest: String = Sha256::digest(gray.as_raw())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                format!("{}\t{}\t{digest}", gray.width(), gray.height())
            }
            Ok(Decoded::Pages(_)) => format!("{path} decodes as pages"),
            Err(_) => "undecodable".to_owned(),
        }
    }

    fn decodes_as_imageio_does(corpora: &[(&str, PathBuf)]) {
        let lines = reference(corpora);
        assert!(!lines.is_empty(), "no image of {corpora:?} is referenced");
        let differences: Vec<String> = lines
            .iter()
            .filter_map(|(root, path, expected)| {
                let native = decoded_as_referenced(path, &image_bytes(root, path));
                (native != *expected).then(|| format!("{path}: {expected} natively {native}"))
            })
            .collect();
        assert!(
            differences.is_empty(),
            "{} of {} images differ:\n{}",
            differences.len(),
            lines.len(),
            differences.join("\n")
        );
    }

    #[test]
    fn decodes_the_images_of_the_corpora_as_imageio_does() {
        decodes_as_imageio_does(&[
            ("documents", PathBuf::from("../test_files/documents")),
            ("corpus", PathBuf::from("../quality/corpus")),
        ]);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn decodes_tikas_test_images_as_imageio_does() {
        decodes_as_imageio_does(&[("tika", crate::formats::testing::conformance_corpus())]);
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
            let Decoded::Single { bitmap, .. } = decode(&input, Pages::IN_MEMORY, format, None)
                .unwrap_or_else(|e| panic!("{file}: {e:?}"))
            else {
                panic!("{file} is one image");
            };
            let decoded = bitmap.gray();
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

    fn single(input: &[u8], format: Format) -> Bitmap {
        match decode(input, Pages::IN_MEMORY, format, None) {
            Ok(Decoded::Single { bitmap, .. }) => bitmap,
            Ok(Decoded::Pages(_)) => panic!("{format:?} decodes as pages"),
            Err(e) => panic!("{format:?}: {e:?}"),
        }
    }

    /// An image of more pixels than OCR prepares is decoded, to be read as it is.
    #[test]
    fn decodes_images_too_large_to_prepare() {
        let large = std::fs::read("tests/fixtures/jbig2/large.jb2").unwrap();
        let bitmap = single(&large, Format::Jbig2);
        assert_eq!(bitmap.dimensions(), (8000, 7600));
        assert!(!super::super::Image::of(&bitmap, None, large.len() as u64, None, None).preparable);
        assert!(decodable(8000, 7600).is_ok());
        assert!(decodable(1 << 15, 1 << 15).is_err());
    }

    #[test]
    fn reads_the_resolution_an_image_file_records() {
        let dpi = |file: &str, format| {
            let input = std::fs::read(format!("../quality/corpus/{file}")).unwrap();
            match decode(&input, Pages::IN_MEMORY, format, None).unwrap() {
                Decoded::Single { density, .. } => density.map(Density::dpi),
                Decoded::Pages(pages) => {
                    let (_, density) = pages.reader().unwrap().page(0).unwrap();
                    density.map(Density::dpi)
                }
            }
        };
        assert_eq!(
            dpi("prose-columns-scan.png", Format::Image(ImageFormat::Png)),
            Some(300)
        );
        assert_eq!(dpi("prose-photo-upside-down.jpg", Format::Jpeg), Some(300));
        assert_eq!(dpi("report-scan.tif", Format::Tiff), Some(200));
        assert_eq!(dpi("prose-scan.psd", Format::Photoshop), None);
    }

    /// A CMYK JPEG, which the JDK's reader refuses, decodes in colour.
    #[test]
    fn decodes_cmyk_jpegs_in_colour() {
        let cmyk = std::fs::read("tests/fixtures/jpeg/ycck.jpg").unwrap();
        let Bitmap::Rgb(rgb) = single(&cmyk, Format::Jpeg) else {
            panic!("a CMYK JPEG decodes in colour");
        };
        let [red, green, blue] = rgb.get_pixel(0, 0).0;
        assert!(red > 200 && green < 40 && blue < 40, "{red} {green} {blue}");
    }

    /// An AVIF file whose primary image is no AV1 image decodes as libheif decodes it.
    #[test]
    fn decodes_avif_files_of_other_codecs_with_libheif() {
        let heic = std::fs::read("../quality/corpus/prose-scan.heic").unwrap();
        assert_eq!(
            single(&heic, Format::Avif).gray(),
            single(&heic, Format::Heif).gray()
        );
    }

    /// A truncated HEIC or AVIF file, which libheif rejects for Tika too, is rejected with
    /// libheif's failure (#173).
    #[test]
    fn rejects_heif_files_libheif_cannot_decode() {
        for (file, format) in [
            ("prose-scan.avif", Format::Avif),
            ("prose-scan.heic", Format::Heif),
        ] {
            let input = std::fs::read(format!("../quality/corpus/{file}")).unwrap();
            match decode(&input[..input.len() / 2], Pages::IN_MEMORY, format, None) {
                Err(Undecoded::Rejected(message)) => assert!(!message.is_empty(), "{file}"),
                Ok(_) => panic!("{file}: a truncated file decodes"),
                Err(other) => panic!("{file}: {other:?}"),
            }
        }
    }

    #[test]
    fn unpacks_bilevel_rows() {
        let samples: Vec<u8> = unpack(&[0b1010_0000, 0b0100_0000], 3, 1).collect();
        assert_eq!(samples, [1, 0, 1, 0, 1, 0]);
    }

    fn tiff_of(image: DynamicImage) -> Vec<u8> {
        let mut tiff = Cursor::new(Vec::new());
        image.write_to(&mut tiff, ImageFormat::Tiff).unwrap();
        tiff.into_inner()
    }

    #[test]
    fn decodes_tiff_pages_of_eight_and_sixteen_bit_samples() {
        let rgb = RgbImage::from_fn(3, 2, |x, y| image::Rgb([x as u8 * 80, y as u8 * 90, 7]));
        let Bitmap::Rgb(decoded) =
            single(&tiff_of(DynamicImage::ImageRgb8(rgb.clone())), Format::Tiff)
        else {
            panic!("an RGB page decodes in colour");
        };
        assert_eq!(decoded, rgb);
        let deep = image::ImageBuffer::<image::Luma<u16>, _>::from_fn(5, 3, |x, y| {
            image::Luma([(x * 13_000 + y * 700) as u16])
        });
        let expected = GrayImage::from_fn(5, 3, |x, y| {
            image::Luma([(deep.get_pixel(x, y).0[0] >> 8) as u8])
        });
        let Bitmap::Gray(decoded) = single(&tiff_of(DynamicImage::ImageLuma16(deep)), Format::Tiff)
        else {
            panic!("a grey page decodes in grey");
        };
        assert_eq!(decoded, expected);
    }

    /// A directory whose tag claims more values than the whole file holds fails to be read,
    /// rather than reserving memory for them all.
    #[test]
    fn refuses_tags_claiming_more_values_than_the_file_holds() {
        let directory = |strip_offsets: u32, next: u32| {
            let entries: [(u16, u16, u32, u32); 8] = [
                (256, 3, 1, 1),
                (257, 3, 1, 1),
                (258, 3, 1, 8),
                (259, 3, 1, 1),
                (262, 3, 1, 1),
                (273, 4, strip_offsets, 8),
                (278, 3, 1, 1),
                (279, 4, 1, 1),
            ];
            let mut bytes = (entries.len() as u16).to_le_bytes().to_vec();
            for (tag, kind, count, value) in entries {
                bytes.extend(tag.to_le_bytes());
                bytes.extend(kind.to_le_bytes());
                bytes.extend(count.to_le_bytes());
                bytes.extend(value.to_le_bytes());
            }
            bytes.extend(next.to_le_bytes());
            bytes
        };
        let first = directory(1, 0);
        let second_at = 8 + first.len() as u32;
        let mut input = b"II*\0".to_vec();
        input.extend(8u32.to_le_bytes());
        input.extend(directory(1, second_at));
        input.extend(directory(u32::MAX, 0));
        assert!(matches!(
            decode(&input, Pages::IN_MEMORY, Format::Tiff, None),
            Err(Undecoded::Failed(_))
        ));
    }
}
