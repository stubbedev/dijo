//! PDFBox's `SampledImageReader.getRGBImage` and `PDImageXObject.getImage`: an image's samples
//! read by their bits per component through its decode array and converted by its colour
//! space, its colour key, mask or soft mask applied as alpha, as the image Java writes.

use image::{GrayImage, RgbaImage};
use lopdf::Object;

use super::super::cos::float_value;
use super::super::Unported;
use super::colorspace::ColorSpace;
use super::image::PdImage;
use super::{Thrown, Unwritten};
use crate::ocr::bitmap::{luma, Bitmap};

const IO_EXCEPTION: &str = "java.io.IOException";

/// `PDImage.getImage`.
pub(super) fn image(image: &PdImage) -> Result<Bitmap, Unwritten> {
    let soft_mask = image.soft_mask();
    let mask = image.mask();
    if let Some(soft_mask) = soft_mask {
        let base = rgb_image(image, image.color_key())?;
        let alpha = rgb_image(&soft_mask, None)?;
        let matte = match soft_mask.matte() {
            Some(matte) => {
                let color_space = image.color_space()?;
                if matte.len() < color_space.components() {
                    None
                } else {
                    Some(color_space.to_rgb(&matte)?)
                }
            }
            None => None,
        };
        return apply_mask(base, alpha, Mask::Soft(matte));
    }
    if let Some(mask) = mask.filter(PdImage::is_stencil) {
        let base = rgb_image(image, image.color_key())?;
        let alpha = rgb_image(&mask, None)?;
        return apply_mask(base, alpha, Mask::Stencil);
    }
    rgb_image(image, image.color_key())
}

/// How a mask is applied.
enum Mask {
    /// A stencil mask, whose samples are inverted to alpha.
    Stencil,
    /// A soft mask, whose samples are the alpha, and its matte colour in RGB, if it has one.
    Soft(Option<[f32; 3]>),
}

/// `SampledImageReader.getRGBImage`.
fn rgb_image(image: &PdImage, color_key: Option<&[Object]>) -> Result<Bitmap, Unwritten> {
    if image.is_empty() {
        return Err(thrown("Image stream is empty"));
    }
    let color_space = image.color_space()?;
    let components = color_space.components();
    let (width, height) = (image.width(), image.height());
    let bits = image.bits_per_component();
    if width <= 0 || height <= 0 {
        return Err(thrown("image width and height must be positive"));
    }
    if !matches!(bits, 1 | 2 | 4 | 8 | 16) {
        return Err(Unported(format!(
            "an image of {bits} bits per component, which PDFBox reads with errors of its own"
        ))
        .into());
    }
    let samples = width as u64 * height as u64 * components as u64;
    if samples > i32::MAX as u64 {
        return Err(Unported(format!(
            "an image of {width} by {height} pixels, more samples than a Java raster holds"
        ))
        .into());
    }
    let (width, height) = (width as u32, height as u32);
    let data = image.data()?;
    if bits == 1 && color_key.is_none() && components == 1 {
        return from_1_bit(image, &color_space, &data, width, height);
    }
    let decode = decode_array(image, &color_space);
    if bits == 8 && color_key.is_none() && decode == color_space.default_decode(8) {
        let mut raster = vec![0u8; samples as usize];
        let read = raster.len().min(data.len());
        raster[..read].copy_from_slice(&data[..read]);
        return color_space.to_rgb_image(width, height, raster);
    }
    from_any(
        &color_space,
        &data,
        color_key,
        &decode,
        (width, height),
        bits as u32,
    )
}

/// `getDecodeArray`: the image's decode array when it has a number for each bound, the colour
/// space's default otherwise.
fn decode_array(image: &PdImage, color_space: &ColorSpace) -> Vec<f32> {
    let components = color_space.components();
    if let Some(items) = image.decode() {
        if items.len() >= components * 2 {
            let decode: Option<Vec<f32>> =
                items[..components * 2].iter().map(float_value).collect();
            if let Some(decode) = decode {
                let out_of_range = |value: f32| !(0.0..=1.0).contains(&value);
                if !(image.is_stencil() && (out_of_range(decode[0]) || out_of_range(decode[1]))) {
                    return decode;
                }
            }
        }
    }
    color_space.default_decode(image.bits_per_component())
}

/// `from1Bit`: each bit one sample of 0 or 255, inverted by the decode array, a grey image's
/// kept as `TYPE_BYTE_GRAY`.
fn from_1_bit(
    image: &PdImage,
    color_space: &ColorSpace,
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Bitmap, Unwritten> {
    let decode = decode_array(image, color_space);
    let invert: u8 = if decode[0] < decode[1] { 0 } else { 0xff };
    let (width, height) = (width as usize, height as usize);
    let stride = width.div_ceil(8);
    let mut output = vec![0u8; width * height];
    for (y, row) in output.chunks_exact_mut(width).enumerate() {
        let start = (y * stride).min(data.len());
        let read = &data[start..(start + stride).min(data.len())];
        for (x, sample) in row.iter_mut().enumerate() {
            let Some(&byte) = read.get(x / 8) else {
                break;
            };
            if (byte ^ invert) & (0x80 >> (x % 8)) != 0 {
                *sample = 255;
            }
        }
        if read.len() != stride {
            break;
        }
    }
    if color_space.is_device_gray() {
        let gray =
            GrayImage::from_raw(width as u32, height as u32, output).expect("one sample a pixel");
        return Ok(Bitmap::Gray(gray));
    }
    color_space.to_rgb_image(width as u32, height as u32, output)
}

/// `fromAny`: the samples read bit by bit, mapped through the decode array to bytes, a
/// palette's to indices, and masked by the colour key.
fn from_any(
    color_space: &ColorSpace,
    data: &[u8],
    color_key: Option<&[Object]>,
    decode: &[f32],
    (width, height): (u32, u32),
    bits: u32,
) -> Result<Bitmap, Unwritten> {
    let components = color_space.components();
    let sample_max = 2f32.powi(bits as i32) - 1.0;
    let indexed = color_space.is_indexed();
    let ranges: Option<Vec<f32>> = color_key
        .filter(|key| key.len() >= components * 2)
        .map(|key| {
            key.iter()
                .map(|item| float_value(item).unwrap_or(0.0))
                .collect()
        });
    let row_bits = width as usize * components * bits as usize;
    let padding = match row_bits % 8 {
        0 => 0,
        bits => 8 - bits,
    };
    let mut reader = Bits { data, at: 0 };
    let pixels = width as usize * height as usize;
    let mut raster = Vec::with_capacity(pixels * components);
    let mut key_mask = ranges.as_ref().map(|_| Vec::with_capacity(pixels));
    for _ in 0..height {
        for _ in 0..width {
            let mut masked = true;
            for c in 0..components {
                let value = reader.read(bits)?;
                if let Some(ranges) = &ranges {
                    let value = value as f32;
                    masked &= value >= ranges[c * 2] && value <= ranges[c * 2 + 1];
                }
                let (d_min, d_max) = (decode[c * 2], decode[c * 2 + 1]);
                let output = d_min + (value as f32 * ((d_max - d_min) / sample_max));
                let sample = if indexed {
                    java_round(output)
                } else {
                    java_round(((output - d_min.min(d_max)) / (d_max - d_min).abs()) * 255.0)
                };
                raster.push(sample as u8);
            }
            if let Some(key_mask) = key_mask.as_mut() {
                key_mask.push(if masked { 255u8 } else { 0 });
            }
        }
        reader.read(padding as u32)?;
    }
    let rgb = color_space.to_rgb_image(width, height, raster)?;
    let Some(key_mask) = key_mask else {
        return Ok(rgb);
    };
    let Bitmap::Rgb(rgb) = rgb else {
        unreachable!("toRGBImage gives an RGB image")
    };
    let masked = RgbaImage::from_fn(width, height, |x, y| {
        let [red, green, blue] = rgb.get_pixel(x, y).0;
        let alpha = 255 - key_mask[(y * width + x) as usize];
        image::Rgba([red, green, blue, alpha])
    });
    Ok(Bitmap::Rgba(masked))
}

/// `ImageInputStream.readBits`, most significant bit first, failing past the end of the data.
struct Bits<'a> {
    data: &'a [u8],
    /// The next bit to read.
    at: usize,
}

impl Bits<'_> {
    fn read(&mut self, count: u32) -> Result<u32, Unwritten> {
        let mut value = 0u32;
        for _ in 0..count {
            let Some(&byte) = self.data.get(self.at / 8) else {
                return Err(Unwritten::Stream(Thrown::bare("java.io.EOFException")));
            };
            let bit = (byte >> (7 - self.at % 8)) & 1;
            value = (value << 1) | u32::from(bit);
            self.at += 1;
        }
        Ok(value)
    }
}

/// `Math.round(float)`: the closest integer, halves rounded up, as an `int`.
fn java_round(value: f32) -> i32 {
    (f64::from(value) + 0.5).floor() as i32
}

/// `PDImageXObject.applyMask` of a mask the size of its image: the image as `TYPE_INT_ARGB`,
/// its alpha the mask's grey levels, inverted for a stencil mask, its colours unmixed from a
/// soft mask's matte.
fn apply_mask(image: Bitmap, mask: Bitmap, kind: Mask) -> Result<Bitmap, Unwritten> {
    if image.dimensions() != mask.dimensions() {
        return Err(Unported(
            "a mask of other dimensions than its image, which Java2D scales".to_owned(),
        )
        .into());
    }
    let (width, height) = image.dimensions();
    let alpha: GrayImage = match mask {
        Bitmap::Gray(gray) => gray,
        Bitmap::Rgb(rgb) => GrayImage::from_fn(width, height, |x, y| {
            let [red, green, blue] = rgb.get_pixel(x, y).0;
            image::Luma([luma(red, green, blue)])
        }),
        Bitmap::Rgba(_) => {
            return Err(Unported(
                "a mask with a colour key of its own, which Java2D draws into grey".to_owned(),
            )
            .into())
        }
    };
    let mut argb = match image {
        Bitmap::Rgba(rgba) => rgba,
        Bitmap::Rgb(rgb) => RgbaImage::from_fn(width, height, |x, y| {
            let [red, green, blue] = rgb.get_pixel(x, y).0;
            image::Rgba([red, green, blue, 255])
        }),
        Bitmap::Gray(gray) => RgbaImage::from_fn(width, height, |x, y| {
            let level = gray.get_pixel(x, y).0[0];
            image::Rgba([level, level, level, 255])
        }),
    };
    for (pixel, mask) in argb.pixels_mut().zip(alpha.pixels()) {
        let a = mask.0[0];
        match kind {
            Mask::Stencil => pixel.0[3] = !a,
            Mask::Soft(None) => pixel.0[3] = a,
            Mask::Soft(Some(matte)) => {
                if a != 0 {
                    for (component, &matte) in pixel.0[..3].iter_mut().zip(&matte) {
                        *component = unmatted(*component, a, matte);
                    }
                }
                pixel.0[3] = a;
            }
        }
    }
    Ok(Bitmap::Rgba(argb))
}

/// A colour component a soft mask's matte was mixed into, unmixed as `applyMask` does it in
/// fixed point with 15 bits of fraction.
fn unmatted(component: u8, alpha: u8, matte: f32) -> u8 {
    const FRACTION: i32 = 15;
    const FACTOR: i32 = 255 << FRACTION;
    let m = java_round(FACTOR as f32 * matte).wrapping_mul(255);
    let half = m / 255 + (1 << (FRACTION - 1));
    let value = ((i32::from(component) * FACTOR - m) / i32::from(alpha) + half) >> FRACTION;
    value.clamp(0, 255) as u8
}

fn thrown(message: &str) -> Unwritten {
    Unwritten::Stream(Thrown::new(IO_EXCEPTION, message.to_owned()))
}
