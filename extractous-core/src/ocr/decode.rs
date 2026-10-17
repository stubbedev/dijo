//! Images decoded for OCR, as our Java `PreparingOcrParser` has ImageIO decode them: every page
//! of a TIFF, the first image of anything else, in grey, colour or colour with alpha
//! ([`Bitmap`]). JPEG is decoded as the JDK's libjpeg decodes it ([`super::jpeg`]), its colours
//! converted from the ICC profile it embeds as the JDK's reader converts them ([`super::icc`]).
//! JPEG 2000 images, which tesseract reads where Java has no decoder, are decoded by
//! hayro-jpeg2000, JBIG2 images, which jbig2-imageio decodes for Java, by hayro-jbig2, AVIF
//! images by rav1d, and HEIC and HEIF images by libheif, which decodes all three for Java; these,
//! and Photoshop and PCX images, are decoded in grey.

use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, RgbImage, RgbaImage};
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use tiff::ColorType;

pub(crate) use super::bitmap::luma;
use super::bitmap::Bitmap;
use super::recognize::MAX_PIXELS;
use super::{avif, heif, jbig2, raster};

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
            _ => return None,
        })
    }

    /// Whether `PreparingOcrParser` hands tesseract the file of an image of the format, which
    /// tesseract decodes itself, rather than the pixels Java decoded: for every format but
    /// those Java decodes for tesseract, Photoshop, PCX, TGA and JBIG2, and HEIC, HEIF and
    /// AVIF, which libheif converts first.
    fn read_by_tesseract(self) -> bool {
        !matches!(
            self,
            Self::Photoshop
                | Self::Pcx
                | Self::Image(ImageFormat::Tga)
                | Self::Jbig2
                | Self::Avif
                | Self::Heif
        )
    }
}

/// A decoded image or page, or why it could not be decoded.
pub(crate) type Page = Result<Bitmap, String>;

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
    /// One image, and its file when tesseract reads the file itself.
    Single {
        bitmap: Bitmap,
        file: Option<&'a [u8]>,
    },
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
    let single = |bitmap| Decoded::Single {
        bitmap,
        file: format.read_by_tesseract().then_some(input),
    };
    match format {
        Format::Jpeg => Ok(single(jpeg(input)?)),
        Format::Image(image_format) => {
            let image = image::load_from_memory_with_format(input, image_format)
                .map_err(|e| format!("cannot decode the image: {e}"))?;
            Ok(single(bitmap(image)))
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
            Ok(single(tiff_page(&mut decoder)?))
        }
        Format::Photoshop => Ok(single(Bitmap::Gray(raster::photoshop(input)?))),
        Format::Pcx => Ok(single(Bitmap::Gray(raster::pcx(input)?))),
        Format::Jpeg2000 => Ok(single(Bitmap::Gray(jpeg2000(input)?))),
        Format::Jbig2 => Ok(single(Bitmap::Gray(jbig2::decode(input)?))),
        Format::Avif => Ok(single(Bitmap::Gray(avif::decode(input)?))),
        Format::Heif => heif::decode(input, timeout).map(|gray| single(Bitmap::Gray(gray))),
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

/// An image in grey, its transparency ignored.
pub(crate) fn gray(image: DynamicImage) -> GrayImage {
    match bitmap(image) {
        Bitmap::Gray(gray) => gray,
        Bitmap::Rgb(rgb) => Bitmap::Rgb(rgb).gray().into_owned(),
        Bitmap::Rgba(rgba) => Bitmap::Rgb(DynamicImage::ImageRgba8(rgba).to_rgb8())
            .gray()
            .into_owned(),
    }
}

/// A JPEG as the JDK decodes it: grey, or colours converted from the profile it embeds.
pub(crate) fn jpeg(input: &[u8]) -> Page {
    let decoded = super::jpeg::decode(input)?;
    let (width, height) = (decoded.width as u32, decoded.height as u32);
    let short = || "the JPEG holds fewer samples than its size needs".to_owned();
    match decoded.samples {
        super::jpeg::Samples::Gray(samples) => GrayImage::from_raw(width, height, samples)
            .map(Bitmap::Gray)
            .ok_or_else(short),
        super::jpeg::Samples::Rgb(mut samples) => {
            if let Some(profile) = &decoded.icc_profile {
                super::icc::to_srgb(profile, &mut samples)
                    .map_err(|e| format!("cannot convert the JPEG's colours: {e}"))?;
            }
            RgbImage::from_raw(width, height, samples)
                .map(Bitmap::Rgb)
                .ok_or_else(short)
        }
    }
}

/// An image the `image` crate decoded, in the kind of bitmap ImageIO decodes it in: grey, colour
/// or colour with alpha, of 8 bits a sample.
fn bitmap(image: DynamicImage) -> Bitmap {
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

/// The page a TIFF decoder is at.
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
            .flat_map(|pixel| {
                let ink = |colour: u8| {
                    ((255 - u32::from(colour)) * (255 - u32::from(pixel[3])) / 255) as u8
                };
                [ink(pixel[0]), ink(pixel[1]), ink(pixel[2])]
            })
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
        match decode(bytes, Format::of(media_type).unwrap(), None) {
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
        decodes_as_imageio_does(&[("tika", crate::formats::parity::conformance_corpus())]);
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
            let Decoded::Single { bitmap, .. } =
                decode(&input, format, None).unwrap_or_else(|e| panic!("{file}: {e:?}"))
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

    #[test]
    fn unpacks_bilevel_rows() {
        let samples: Vec<u8> = unpack(&[0b1010_0000, 0b0100_0000], 3, 1).collect();
        assert_eq!(samples, [1, 0, 1, 0, 1, 0]);
    }
}
