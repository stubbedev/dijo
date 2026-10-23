//! PDFBox's `SampledImageReader.getRGBImage` and `PDImageXObject.getImage`: an image's samples
//! read by their bits per component through its decode array and converted by its colour
//! space, its colour key, mask or soft mask applied as alpha, as the image Java writes.

use image::imageops::{self, FilterType};
use image::{GrayImage, ImageBuffer, Pixel, RgbaImage};
use lopdf::Object;

use super::super::cos::float_value;
use super::super::Unported;
use super::colorspace::ColorSpace;
use super::image::PdImage;
use super::{Exception, Unwritten};
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
        let smooth = (image.interpolates(), soft_mask.interpolates());
        return apply_mask(base, alpha, Mask::Soft(matte), smooth);
    }
    if let Some(mask) = mask.filter(PdImage::is_stencil) {
        let base = rgb_image(image, image.color_key())?;
        let alpha = rgb_image(&mask, None)?;
        let smooth = (image.interpolates(), mask.interpolates());
        return apply_mask(base, alpha, Mask::Stencil, smooth);
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
    if image.is_empty()? {
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

/// `fromAny`: the samples read as `readBits` reads them, mapped through the decode array to
/// bytes, a palette's to indices, and masked by the colour key.
fn from_any(
    color_space: &ColorSpace,
    data: &[u8],
    color_key: Option<&[Object]>,
    decode: &[f32],
    (width, height): (u32, u32),
    bits: u32,
) -> Result<Bitmap, Unwritten> {
    let components = color_space.components();
    let ranges: Option<Vec<f32>> = color_key
        .filter(|key| key.len() >= components * 2)
        .map(|key| {
            key.iter()
                .map(|item| float_value(item).unwrap_or(0.0))
                .collect()
        });
    let (raster, key_mask) = decoded_samples(
        data,
        Sampling {
            components,
            bits,
            indexed: color_space.is_indexed(),
            decode,
            ranges: ranges.as_deref(),
        },
        (width, height),
    )?;
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

/// How an image's samples are read and decoded.
#[derive(Clone, Copy)]
struct Sampling<'a> {
    components: usize,
    bits: u32,
    /// Whether the samples are a palette's indices.
    indexed: bool,
    decode: &'a [f32],
    /// The colour key's range of each component, if it masks the image.
    ranges: Option<&'a [f32]>,
}

impl Sampling<'_> {
    /// Sample `value` of component `c` mapped through the decode array to a byte, and whether
    /// it lies in the colour key's range.
    fn decoded(&self, c: usize, value: u32) -> (u8, bool) {
        let keyed = self.ranges.is_none_or(|ranges| {
            let value = value as f32;
            value >= ranges[c * 2] && value <= ranges[c * 2 + 1]
        });
        let sample_max = 2f32.powi(self.bits as i32) - 1.0;
        let (d_min, d_max) = (self.decode[c * 2], self.decode[c * 2 + 1]);
        let output = d_min + (value as f32 * ((d_max - d_min) / sample_max));
        let sample = if self.indexed {
            java_round(output)
        } else {
            java_round(((output - d_min.min(d_max)) / (d_max - d_min).abs()) * 255.0)
        };
        (sample as u8, keyed)
    }
}

/// The samples of a `width` by `height` image, each row padded to a byte, mapped through the
/// decode array to bytes, and the colour key's mask, 255 for each pixel it masks, if it has
/// one. A component of up to 8 bits is decoded through a table of its values; one without
/// decode bounds fails as `fromAny` fails, once its first sample is read.
#[expect(clippy::needless_range_loop)]
fn decoded_samples(
    data: &[u8],
    sampling: Sampling,
    (width, height): (u32, u32),
) -> Result<(Vec<u8>, Option<Vec<u8>>), Unwritten> {
    let Sampling {
        components, bits, ..
    } = sampling;
    let tables: Vec<Vec<(u8, bool)>> = match bits {
        16 => Vec::new(),
        _ => (0..components.min(sampling.decode.len() / 2))
            .map(|c| {
                (0..1u32 << bits)
                    .map(|value| sampling.decoded(c, value))
                    .collect()
            })
            .collect(),
    };
    let samples = Samples {
        data,
        bits: bits as usize,
        row_bytes: (width as usize * components * bits as usize).div_ceil(8),
    };
    let pixels = width as usize * height as usize;
    let mut raster = Vec::with_capacity(pixels * components);
    let mut key_mask = sampling.ranges.map(|_| Vec::with_capacity(pixels));
    for y in 0..height as usize {
        for x in 0..width as usize {
            let mut masked = true;
            for c in 0..components {
                let value = samples.get(y, x * components + c)?;
                let (sample, keyed) = match bits {
                    16 => sampling.decoded(c, value),
                    _ => tables[c][value as usize],
                };
                masked &= keyed;
                raster.push(sample);
            }
            if let Some(key_mask) = key_mask.as_mut() {
                key_mask.push(if masked { 255u8 } else { 0 });
            }
        }
    }
    Ok((raster, key_mask))
}

/// The samples of an image's rows, each row padded to a byte, read as `ImageInputStream.readBits`
/// reads them, most significant bit first, failing past the end of the data.
struct Samples<'a> {
    data: &'a [u8],
    /// The bits of a sample: 1, 2, 4, 8 or 16.
    bits: usize,
    row_bytes: usize,
}

impl Samples<'_> {
    /// Sample `index` of row `row`.
    fn get(&self, row: usize, index: usize) -> Result<u32, Unwritten> {
        let at = row * self.row_bytes * 8 + index * self.bits;
        let byte = |i: usize| {
            self.data
                .get(i)
                .map(|&byte| u32::from(byte))
                .ok_or_else(|| Unwritten::Stream(Exception::bare("java.io.EOFException")))
        };
        Ok(match self.bits {
            16 => (byte(at / 8)? << 8) | byte(at / 8 + 1)?,
            bits => (byte(at / 8)? >> (8 - bits - at % 8)) & ((1 << bits) - 1),
        })
    }
}

/// `Math.round(float)`: the closest integer, halves rounded up, as an `int`.
fn java_round(value: f32) -> i32 {
    (f64::from(value) + 0.5).floor() as i32
}

/// `PDImageXObject.applyMask`: the image as `TYPE_INT_ARGB`, its alpha the mask's grey levels,
/// inverted for a stencil mask, its colours unmixed from a soft mask's matte. Where the image
/// and the mask differ in size, the smaller is scaled to the larger's width and height, which
/// Java2D draws (#125): here nearest neighbour, or Catmull-Rom where the image or the mask asks
/// to be interpolated, as `smooth` says of each. A mask with an alpha of its own, from a colour
/// key, is drawn onto black, as Java2D draws it into a grey image.
fn apply_mask(
    image: Bitmap,
    mask: Bitmap,
    kind: Mask,
    (smooth_image, smooth_mask): (bool, bool),
) -> Result<Bitmap, Unwritten> {
    let (image_width, image_height) = image.dimensions();
    let (mask_width, mask_height) = mask.dimensions();
    let (width, height) = (image_width.max(mask_width), image_height.max(mask_height));
    let alpha: GrayImage = match mask {
        Bitmap::Gray(gray) => gray,
        Bitmap::Rgb(rgb) => GrayImage::from_fn(mask_width, mask_height, |x, y| {
            let [red, green, blue] = rgb.get_pixel(x, y).0;
            image::Luma([luma(red, green, blue)])
        }),
        Bitmap::Rgba(rgba) => GrayImage::from_fn(mask_width, mask_height, |x, y| {
            let [red, green, blue, alpha] = rgba.get_pixel(x, y).0;
            let over_black = |c: u8| mul8(c, alpha);
            image::Luma([luma(over_black(red), over_black(green), over_black(blue))])
        }),
    };
    let alpha = scaled(alpha, (width, height), smooth_mask);
    let argb = match image {
        Bitmap::Rgba(rgba) => rgba,
        Bitmap::Rgb(rgb) => RgbaImage::from_fn(image_width, image_height, |x, y| {
            let [red, green, blue] = rgb.get_pixel(x, y).0;
            image::Rgba([red, green, blue, 255])
        }),
        Bitmap::Gray(gray) => RgbaImage::from_fn(image_width, image_height, |x, y| {
            let level = gray.get_pixel(x, y).0[0];
            image::Rgba([level, level, level, 255])
        }),
    };
    let mut argb = scaled(argb, (width, height), smooth_image);
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

/// `image` at `size`, nearest neighbour unless `smooth`, as it is where it is that size.
fn scaled<P>(
    image: ImageBuffer<P, Vec<u8>>,
    (width, height): (u32, u32),
    smooth: bool,
) -> ImageBuffer<P, Vec<u8>>
where
    P: Pixel<Subpixel = u8> + 'static,
{
    if image.dimensions() == (width, height) {
        return image;
    }
    let filter = match smooth {
        true => FilterType::CatmullRom,
        false => FilterType::Nearest,
    };
    imageops::resize(&image, width, height, filter)
}

/// Java2D's `mul8table`: the product of two levels, as levels.
fn mul8(a: u8, b: u8) -> u8 {
    ((u32::from(a) * u32::from(b) + 127) / 255) as u8
}

fn thrown(message: &str) -> Unwritten {
    Unwritten::Stream(Exception::new(IO_EXCEPTION, message.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use super::*;

    /// `fromAny`'s samples as it read them bit by bit and decoded each on its own.
    fn decoded_bit_by_bit(
        data: &[u8],
        sampling: Sampling,
        (width, height): (u32, u32),
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), Unwritten> {
        let Sampling {
            components,
            bits,
            indexed,
            decode,
            ranges,
        } = sampling;
        let sample_max = 2f32.powi(bits as i32) - 1.0;
        let row_bits = width as usize * components * bits as usize;
        let padding = match row_bits % 8 {
            0 => 0,
            bits => 8 - bits,
        };
        let mut at = 0;
        let mut read = |count: u32| {
            let mut value = 0u32;
            for _ in 0..count {
                let Some(&byte) = data.get(at / 8) else {
                    return Err(Unwritten::Stream(Exception::bare("java.io.EOFException")));
                };
                value = (value << 1) | u32::from((byte >> (7 - at % 8)) & 1);
                at += 1;
            }
            Ok(value)
        };
        let pixels = width as usize * height as usize;
        let mut raster = Vec::with_capacity(pixels * components);
        let mut key_mask = ranges.map(|_| Vec::with_capacity(pixels));
        for _ in 0..height {
            for _ in 0..width {
                let mut masked = true;
                for c in 0..components {
                    let value = read(bits)?;
                    if let Some(ranges) = ranges {
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
            read(padding as u32)?;
        }
        Ok((raster, key_mask))
    }

    #[test]
    fn decodes_samples_through_tables_as_bit_by_bit() {
        let data: Vec<u8> = (0..400u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let decodes: [&[f32]; 5] = [
            &[0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            &[1.0, 0.0, 0.25, 0.75, -0.5, 2.0],
            &[0.0, 15.0, 0.0, 255.0, 3.0, 1.0],
            &[0.0, 1.0, 0.0],
            &[0.0, 1.0],
        ];
        let ranges: [Option<&[f32]>; 3] = [
            None,
            Some(&[0.0, 1.0, 2.0, 200.0, 5.0, 9.0]),
            Some(&[3.0, 3.0, 0.0, 70000.0, 0.0, 0.0]),
        ];
        let mut checked = 0;
        for bits in [1, 2, 4, 8, 16] {
            for components in [1, 3] {
                for indexed in [false, true] {
                    for decode in decodes {
                        for ranges in ranges {
                            for (size, length) in
                                [((7, 5), 400), ((13, 3), 400), ((5, 9), 11), ((1, 1), 0)]
                            {
                                let sampling = Sampling {
                                    components,
                                    bits,
                                    indexed,
                                    decode,
                                    ranges,
                                };
                                let data = &data[..length];
                                let decoded = |by_table: bool| {
                                    catch_unwind(AssertUnwindSafe(|| match by_table {
                                        true => {
                                            format!("{:?}", decoded_samples(data, sampling, size))
                                        }
                                        false => format!(
                                            "{:?}",
                                            decoded_bit_by_bit(data, sampling, size)
                                        ),
                                    }))
                                    .ok()
                                };
                                assert_eq!(
                                    decoded(true),
                                    decoded(false),
                                    "{bits} bits, {components} components, {indexed}, {decode:?}, \
                                     {ranges:?}, {size:?}, {length}"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 5 * 2 * 2 * 5 * 3 * 4);
    }
}
