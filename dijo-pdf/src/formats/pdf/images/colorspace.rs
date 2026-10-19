//! The colour spaces of images, as PDFBox's `PDColorSpace.create` makes them and their
//! `toRGBImage` converts a raster of an image's samples to the image Java writes: grey and RGB;
//! CMYK and ICC profiles, which the JDK converts to sRGB with LittleCMS; the CIE-based spaces
//! (Lab, CalRGB, CalGray), through the JDK's CIE XYZ profile; separations and DeviceN, through
//! their tint transforms; and palettes over any of them. An image of one row or column in a
//! profile's colour space is drawn by Java2D, which converts each pixel on its own.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use lopdf::{Dictionary, Object};

use super::super::cos::{as_dict, float_value, int_value, name_string, Pdf};
use super::super::function::{self, Function};
use super::super::Unported;
use super::{Exception, Unwritten};
use crate::ocr::bitmap::Bitmap;
use crate::ocr::icc::{accepted_components, gray_to_srgb_levels, IccToRgb, ToSrgb};

/// How deep colour spaces may name colour spaces before the port stops following them.
const MAX_NESTING: usize = 8;
const MISSING_RESOURCE: &str = "org.apache.pdfbox.pdmodel.MissingResourceException";
const IO_EXCEPTION: &str = "java.io.IOException";
const UNSUPPORTED_OPERATION: &str = "java.lang.UnsupportedOperationException";
/// `ICC_Profile.icHdrModel`: where a profile's header names the device model.
const MODEL: std::ops::Range<usize> = 52..59;
/// `ICC_Profile.icHdrDeviceClass`.
const DEVICE_CLASS: std::ops::Range<usize> = 12..16;
/// `ICC_Profile.icHdrColorSpace`.
const COLOR_SPACE: std::ops::Range<usize> = 16..20;
/// `ICC_Profile.icHdrRenderingIntent`.
const RENDERING_INTENT: std::ops::Range<usize> = 64..68;
/// `ICC_Profile.icPerceptual`, as the header holds it.
const PERCEPTUAL: [u8; 4] = [0; 4];
const DISPLAY_CLASS: &[u8; 4] = b"mntr";
/// The CMYK profile `PDDeviceCMYK` converts with, from PDFBox's resources.
const CMYK_PROFILE: &[u8] = include_bytes!("../../../../models/pdf/CGATS001Compat-v2-micro.icc");
/// The JDK's `ColorSpace.CS_CIEXYZ` profile, written by `JdkCieXyzProfile.java`.
const CIEXYZ_PROFILE: &[u8] = include_bytes!("../../../../models/pdf/jdk-ciexyz.icc");

/// A colour space PDFBox reads an image's samples in.
pub(super) enum ColorSpace {
    /// `PDDeviceGray.INSTANCE`.
    Gray,
    /// `PDDeviceRGB.INSTANCE`.
    Rgb,
    /// `PDDeviceCMYK.INSTANCE`, converted by its profile.
    Cmyk(Box<Profiled>),
    /// `PDICCBased`.
    Icc(Box<Icc>),
    /// `PDIndexed`.
    Indexed(Box<Indexed>),
    /// `PDSeparation`.
    Separation(Box<Separation>),
    /// `PDDeviceN`.
    DeviceN(Box<DeviceN>),
    /// `PDLab`.
    Lab(Box<Lab>),
    /// `PDCalRGB`.
    CalRgb(Box<CalRgb>),
    /// `PDCalGray`.
    CalGray(Box<CalGray>),
    /// `PDPattern`, whose `toRGBImage` throws.
    Pattern,
}

/// A colour space a profile converts: as `ColorConvertOp` converts an image, and as
/// `ICC_ColorSpace.toRGB` converts one colour.
pub(super) struct Profiled {
    image: ToSrgb,
    colour: IccToRgb,
    components: usize,
    /// For a grey profile, the sRGB level of each grey level, as Java2D draws it.
    gray_levels: Option<[u8; 256]>,
}

impl Profiled {
    fn new(profile: &[u8], components: usize) -> Option<Self> {
        let gray_levels = match &profile[COLOR_SPACE] {
            b"GRAY" => Some(gray_to_srgb_levels(profile)?),
            _ => None,
        };
        Some(Self {
            image: ToSrgb::new(profile, components)?,
            colour: IccToRgb::new(profile)?,
            components,
            gray_levels,
        })
    }

    /// `ICC_ColorSpace.getMinValue` and `getMaxValue` of each component.
    fn ranges(&self) -> Vec<f32> {
        self.colour
            .ranges()
            .flat_map(|(min, max)| [min, max])
            .collect()
    }

    /// `toRGBImageAWT`: `ColorConvertOp`, or for an image of one row or column, which
    /// `ColorConvertOp` is slow on, Java2D's drawing, which converts each pixel's colour
    /// through `ComponentColorModel.getRGB`, a grey one through its table of levels.
    fn to_rgb_image(&self, width: u32, height: u32, samples: &[u8]) -> Vec<u8> {
        if width != 1 && height != 1 {
            return self.image.convert(samples);
        }
        if let Some(levels) = &self.gray_levels {
            return samples
                .iter()
                .flat_map(|&gray| [levels[usize::from(gray)]; 3])
                .collect();
        }
        let ranges: Vec<(f32, f32)> = self.colour.ranges().collect();
        let standard = ranges.iter().all(|&range| range == (0.0, 1.0));
        samples
            .chunks_exact(self.components)
            .flat_map(|pixel| {
                let normalized: Vec<f32> = pixel
                    .iter()
                    .zip(&ranges)
                    .map(|(&sample, &(min, max))| match standard {
                        true => f32::from(sample) / 255.0,
                        false => f32::from(sample) * ((max - min) / 255.0) + min,
                    })
                    .collect();
                self.colour
                    .to_rgb(&normalized)
                    .map(|component| (component * 255.0 + 0.5) as i32 as u8)
            })
            .collect()
    }

    /// `ICC_ColorSpace.toRGB`.
    fn to_rgb(&self, value: &[f32]) -> [f32; 3] {
        self.colour.to_rgb(value)
    }
}

/// `PDICCBased`: its count of components, from its stream's `N`, and how its samples become
/// sRGB.
pub(super) struct Icc {
    components: usize,
    conversion: IccConversion,
    /// Whether its `Alternate` is a Lab colour space, which a separation over it converts
    /// colour by colour.
    lab_alternate: bool,
}

enum IccConversion {
    /// A profile of the sRGB model, for which PDFBox takes the JDK's own sRGB profile: the
    /// samples are sRGB.
    Srgb,
    /// Another profile, which LittleCMS converts.
    Profile(Box<Profiled>),
    /// The alternate colour space PDFBox falls back to for a profile the JDK refuses.
    Alternate(ColorSpace),
}

/// `PDIndexed`: the RGB colour of each index up to the largest the lookup table holds.
pub(super) struct Indexed {
    rgb: Vec<[u8; 3]>,
}

/// `PDSeparation`: its tint transform into its alternate colour space.
pub(super) struct Separation {
    alternate: ColorSpace,
    tint: Function,
}

/// `PDDeviceN`: its colorants' tint transform into its alternate colour space, and its
/// attributes, if it has them.
pub(super) struct DeviceN {
    colorants: usize,
    alternate: ColorSpace,
    tint: Function,
    attributes: Option<Attributes>,
}

/// `PDDeviceN`'s colour conversion cache from its attributes: for each colorant, the process
/// colour space's component it is, or the separation it is.
struct Attributes {
    colorants: Vec<Colorant>,
    process: Option<ColorSpace>,
}

enum Colorant {
    Process(usize),
    Spot(ColorSpace),
    /// Neither: the image converts by the tint transform.
    Unknown,
}

/// `PDLab`: its white point and the ranges of a* and b*.
pub(super) struct Lab {
    white: [f32; 3],
    a: (f32, f32),
    b: (f32, f32),
    xyz: IccToRgb,
}

/// `PDCalRGB`: its white point, gamma and matrix.
pub(super) struct CalRgb {
    white: [f32; 3],
    gamma: [f32; 3],
    matrix: [f32; 9],
    xyz: IccToRgb,
}

/// `PDCalGray`: its white point and gamma.
pub(super) struct CalGray {
    white: [f32; 3],
    gamma: f32,
    xyz: IccToRgb,
}

/// `PDCIEDictionaryBasedColorSpace.isWhitePoint`: whether the white point is 1 1 1, under
/// which PDFBox converts through CIE XYZ.
fn is_unit(white: [f32; 3]) -> bool {
    white.iter().all(|&value| value.total_cmp(&1.0).is_eq())
}

/// `convXYZtoRGB`: negative tristimulus values taken as 0.
fn xyz_to_rgb(xyz: &IccToRgb, x: f32, y: f32, z: f32) -> [f32; 3] {
    xyz.to_rgb(&[x.max(0.0), y.max(0.0), z.max(0.0)])
}

impl ColorSpace {
    /// `getName`.
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Gray => "DeviceGray",
            Self::Rgb => "DeviceRGB",
            Self::Cmyk(_) => "DeviceCMYK",
            Self::Icc(_) => "ICCBased",
            Self::Indexed(_) => "Indexed",
            Self::Separation(_) => "Separation",
            Self::DeviceN(_) => "DeviceN",
            Self::Lab(_) => "Lab",
            Self::CalRgb(_) => "CalRGB",
            Self::CalGray(_) => "CalGray",
            Self::Pattern => "Pattern",
        }
    }

    /// `getNumberOfComponents`.
    pub(super) fn components(&self) -> usize {
        match self {
            Self::Gray | Self::Indexed(_) | Self::Separation(_) | Self::CalGray(_) => 1,
            Self::Pattern => 1,
            Self::Rgb | Self::Lab(_) | Self::CalRgb(_) => 3,
            Self::Cmyk(_) => 4,
            Self::Icc(icc) => icc.components,
            Self::DeviceN(device_n) => device_n.colorants,
        }
    }

    /// `getDefaultDecode`: every component from 0 to 1, a palette's index from 0 to its
    /// largest value, a profile's components over the profile's ranges, and Lab's over its own.
    pub(super) fn default_decode(&self, bits_per_component: i32) -> Vec<f32> {
        match self {
            Self::Indexed(_) => vec![0.0, 2f32.powi(bits_per_component) - 1.0],
            Self::Icc(icc) => match &icc.conversion {
                IccConversion::Alternate(alternate) => alternate.default_decode(bits_per_component),
                IccConversion::Profile(profiled) => profiled.ranges(),
                IccConversion::Srgb => [0.0, 1.0].repeat(icc.components),
            },
            Self::Lab(lab) => vec![0.0, 100.0, lab.a.0, lab.a.1, lab.b.0, lab.b.1],
            _ => [0.0, 1.0].repeat(self.components()),
        }
    }

    /// Whether this is `PDDeviceGray.INSTANCE`.
    pub(super) fn is_device_gray(&self) -> bool {
        matches!(self, Self::Gray)
    }

    /// Whether this is a palette, whose samples are indices.
    pub(super) fn is_indexed(&self) -> bool {
        matches!(self, Self::Indexed(_))
    }

    /// Whether this is Lab, or a profile whose alternate is, which `PDSeparation` converts
    /// colour by colour.
    fn is_lab_like(&self) -> bool {
        match self {
            Self::Lab(_) => true,
            Self::Icc(icc) => icc.lab_alternate,
            _ => false,
        }
    }

    /// `toRGBImage` of interleaved 8-bit samples of a `width` by `height` raster: the
    /// `TYPE_INT_RGB` image PDFBox converts them to.
    pub(super) fn to_rgb_image(
        &self,
        width: u32,
        height: u32,
        samples: Vec<u8>,
    ) -> Result<Bitmap, Unwritten> {
        let rgb = match self {
            Self::Rgb => samples,
            Self::Gray => samples.iter().flat_map(|&gray| [gray; 3]).collect(),
            Self::Indexed(indexed) => samples
                .iter()
                .flat_map(|&index| indexed.color(usize::from(index)))
                .collect(),
            Self::Cmyk(profiled) => profiled.to_rgb_image(width, height, &samples),
            Self::Icc(icc) => match &icc.conversion {
                IccConversion::Alternate(alternate) => {
                    return alternate.to_rgb_image(width, height, samples)
                }
                IccConversion::Srgb => samples,
                IccConversion::Profile(profiled) => profiled.to_rgb_image(width, height, &samples),
            },
            Self::Separation(separation) => {
                return separation.to_rgb_image(width, height, &samples)
            }
            Self::DeviceN(device_n) => device_n.to_rgb_image(width, height, &samples)?,
            Self::Lab(lab) => {
                let (delta_a, delta_b) = (lab.a.1 - lab.a.0, lab.b.1 - lab.b.0);
                per_pixel(&samples, 3, |abc| {
                    let [l, a, b] =
                        [abc[0], abc[1], abc[2]].map(|sample| f32::from(sample) / 255.0);
                    let value = [l * 100.0, lab.a.0 + a * delta_a, lab.b.0 + b * delta_b];
                    Ok(lab.to_rgb(&value).map(|c| (c * 255.0) as i32 as u8))
                })?
            }
            Self::CalRgb(_) | Self::CalGray(_) => {
                let components = self.components();
                per_pixel(&samples, components, |pixel| {
                    let value: Vec<f32> = pixel
                        .iter()
                        .map(|&sample| f32::from(sample) / 255.0)
                        .collect();
                    Ok(self.to_rgb(&value)?.map(|c| (c * 255.0) as i32 as u8))
                })?
            }
            Self::Pattern => return Err(unsupported()),
        };
        Ok(Bitmap::Rgb(
            image::RgbImage::from_raw(width, height, rgb)
                .expect("a raster holds three samples a pixel"),
        ))
    }

    /// `toRGB` of one colour, each component from 0 to 1.
    pub(super) fn to_rgb(&self, value: &[f32]) -> Result<[f32; 3], Unwritten> {
        let component = |i: usize| {
            value
                .get(i)
                .copied()
                .ok_or_else(|| thrown_io("a colour of fewer components than its colour space"))
        };
        Ok(match self {
            Self::Gray => [component(0)?; 3],
            Self::Rgb => [component(0)?, component(1)?, component(2)?],
            Self::Cmyk(profiled) => profiled.to_rgb(value),
            Self::Indexed(indexed) => {
                let index = (java_round(component(0)?).max(0) as usize).min(indexed.rgb.len() - 1);
                indexed.rgb[index].map(|component| f32::from(component) / 255.0)
            }
            Self::Icc(icc) => match &icc.conversion {
                IccConversion::Srgb => [component(0)?, component(1)?, component(2)?],
                IccConversion::Alternate(alternate) => alternate.to_rgb(value)?,
                IccConversion::Profile(profiled) => {
                    let clamped: Vec<f32> = value
                        .iter()
                        .zip(profiled.colour.ranges())
                        .map(|(&value, (min, max))| value.max(min).min(max))
                        .collect();
                    profiled.to_rgb(&clamped)
                }
            },
            Self::Separation(separation) => {
                let tinted = tint(&separation.tint, value)?;
                separation.alternate.to_rgb(&tinted)?
            }
            Self::DeviceN(device_n) => device_n.to_rgb(value)?,
            Self::Lab(lab) => lab.to_rgb(&[component(0)?, component(1)?, component(2)?]),
            Self::CalRgb(cal) => {
                if !is_unit(cal.white) {
                    return Ok([component(0)?, component(1)?, component(2)?]);
                }
                let powered: Vec<f32> = (0..3)
                    .map(|i| Ok(f64::from(component(i)?).powf(f64::from(cal.gamma[i])) as f32))
                    .collect::<Result<_, Unwritten>>()?;
                let m = cal.matrix;
                let (a, b, c) = (powered[0], powered[1], powered[2]);
                let x = m[0] * a + m[3] * b + m[6] * c;
                let y = m[1] * a + m[4] * b + m[7] * c;
                let z = m[2] * a + m[5] * b + m[8] * c;
                xyz_to_rgb(&cal.xyz, x, y, z)
            }
            Self::CalGray(cal) => {
                if !is_unit(cal.white) {
                    return Ok([component(0)?; 3]);
                }
                let powered = f64::from(component(0)?).powf(f64::from(cal.gamma)) as f32;
                xyz_to_rgb(&cal.xyz, powered, powered, powered)
            }
            Self::Pattern => return Err(unsupported()),
        })
    }
}

/// Converts each pixel of `components` samples with `convert`, once for each distinct pixel.
fn per_pixel(
    samples: &[u8],
    components: usize,
    mut convert: impl FnMut(&[u8]) -> Result<[u8; 3], Unwritten>,
) -> Result<Vec<u8>, Unwritten> {
    let mut converted: HashMap<&[u8], [u8; 3]> = HashMap::new();
    let mut rgb = Vec::with_capacity(samples.len() / components.max(1) * 3);
    for pixel in samples.chunks_exact(components.max(1)) {
        let colour = match converted.get(pixel) {
            Some(&colour) => colour,
            None => {
                let colour = convert(pixel)?;
                converted.insert(pixel, colour);
                colour
            }
        };
        rgb.extend_from_slice(&colour);
    }
    Ok(rgb)
}

/// `tintTransform.eval`, a failure of the function an exception of the image's stream.
fn tint(function: &Function, value: &[f32]) -> Result<Vec<f32>, Unwritten> {
    function.eval(value).map_err(function_failure)
}

fn function_failure(failed: function::Failed) -> Unwritten {
    match failed {
        function::Failed::Thrown(message) => thrown_io(&format!(
            "the tint transform of a colour space fails: {message}"
        )),
        function::Failed::Unported(unported) => Unwritten::Unported(unported),
    }
}

impl Lab {
    /// `PDLab.toRGB`.
    fn to_rgb(&self, value: &[f32; 3]) -> [f32; 3] {
        let lstar = (value[0] + 16.0) * (1.0 / 116.0);
        let x = self.white[0] * lab_inverse(lstar + value[1] * (1.0 / 500.0));
        let y = self.white[1] * lab_inverse(lstar);
        let z = self.white[2] * lab_inverse(lstar - value[2] * (1.0 / 200.0));
        xyz_to_rgb(&self.xyz, x, y, z)
    }
}

/// `PDLab.inverse`.
fn lab_inverse(x: f32) -> f32 {
    if f64::from(x) > 6.0 / 29.0 {
        x * x * x
    } else {
        (108.0 / 841.0) * (x - (4.0 / 29.0))
    }
}

impl Separation {
    /// `PDSeparation.toRGBImage`: each distinct sample's tint in the alternate colour space,
    /// truncated to bytes, converted as an image of the alternate space; over Lab, each
    /// distinct sample's colour converted on its own.
    fn to_rgb_image(&self, width: u32, height: u32, samples: &[u8]) -> Result<Bitmap, Unwritten> {
        if self.alternate.is_lab_like() {
            let rgb = per_pixel(samples, 1, |sample| {
                let tinted = tint(&self.tint, &[f32::from(sample[0]) / 255.0])?;
                let rgb = self.alternate.to_rgb(&tinted)?;
                Ok(rgb.map(|c| (c * 255.0) as i32 as u8))
            })?;
            return Ok(Bitmap::Rgb(
                image::RgbImage::from_raw(width, height, rgb)
                    .expect("a raster holds three samples a pixel"),
            ));
        }
        let components = self.alternate.components();
        let mut tints: HashMap<u8, Vec<u8>> = HashMap::new();
        let mut alternate = Vec::with_capacity(samples.len() * components);
        for &sample in samples {
            if let Entry::Vacant(vacant) = tints.entry(sample) {
                let tinted = tint(&self.tint, &[f32::from(sample) / 255.0])?;
                let bytes = (0..components)
                    .map(|i| {
                        tinted
                            .get(i)
                            .map(|&value| (value * 255.0) as i32 as u8)
                            .ok_or_else(|| {
                                thrown_io("a tint transform of fewer outputs than its alternate")
                            })
                    })
                    .collect::<Result<Vec<u8>, Unwritten>>()?;
                vacant.insert(bytes);
            }
            alternate.extend_from_slice(&tints[&sample]);
        }
        self.alternate.to_rgb_image(width, height, alternate)
    }
}

impl DeviceN {
    /// `PDDeviceN.toRGBImage`.
    fn to_rgb_image(&self, width: u32, height: u32, samples: &[u8]) -> Result<Vec<u8>, Unwritten> {
        if let Some(attributes) = &self.attributes {
            if let Some(rgb) = self.with_attributes(attributes, width, height, samples)? {
                return Ok(rgb);
            }
        }
        per_pixel(samples, self.colorants, |pixel| {
            let value: Vec<f32> = pixel
                .iter()
                .map(|&sample| f32::from(sample) / 255.0)
                .collect();
            let tinted = tint(&self.tint, &value)?;
            let rgb = self.alternate.to_rgb(&tinted)?;
            Ok(rgb.map(|c| (c * 255.0) as i32 as u8))
        })
    }

    /// `toRGBWithAttributes`: white, multiplied by each colorant converted as an image of its
    /// process colour space or its separation; `None` where a colorant is neither, which leaves
    /// the image to the tint transform.
    fn with_attributes(
        &self,
        attributes: &Attributes,
        width: u32,
        height: u32,
        samples: &[u8],
    ) -> Result<Option<Vec<u8>>, Unwritten> {
        let pixels = samples.len() / self.colorants.max(1);
        let mut rgb = vec![255u8; pixels * 3];
        for (c, colorant) in attributes.colorants.iter().enumerate() {
            let (space, index) = match colorant {
                Colorant::Process(index) => match &attributes.process {
                    Some(process) => (process, *index),
                    None => {
                        return Err(thrown_io(
                            "a DeviceN process without a colour space, which PDFBox fails on",
                        ))
                    }
                },
                Colorant::Spot(space) => (space, 0),
                Colorant::Unknown => return Ok(None),
            };
            let components = space.components();
            if index >= components {
                return Err(thrown_io(
                    "java.lang.ArrayIndexOutOfBoundsException: a DeviceN process component \
                     beyond its colour space",
                ));
            }
            let mut raster = vec![0u8; pixels * components];
            for (pixel, out) in samples
                .chunks_exact(self.colorants)
                .zip(raster.chunks_exact_mut(components))
            {
                out[index] = pixel[c];
            }
            let converted = space.to_rgb_image(width, height, raster)?;
            let Bitmap::Rgb(converted) = converted else {
                unreachable!("toRGBImage gives an RGB image")
            };
            for (composite, channel) in rgb.iter_mut().zip(converted.as_raw()) {
                *composite = ((u32::from(*channel) * u32::from(*composite)) >> 8) as u8;
            }
        }
        Ok(Some(rgb))
    }

    /// `PDDeviceN.toRGB`.
    fn to_rgb(&self, value: &[f32]) -> Result<[f32; 3], Unwritten> {
        if let Some(attributes) = &self.attributes {
            let mut rgb = [1f32; 3];
            for (c, colorant) in attributes.colorants.iter().enumerate() {
                let sample = value.get(c).copied().unwrap_or(0.0);
                let component = match colorant {
                    Colorant::Process(index) => {
                        let Some(process) = &attributes.process else {
                            return Err(thrown_io(
                                "a DeviceN process without a colour space, which PDFBox fails on",
                            ));
                        };
                        let mut samples = vec![0f32; process.components()];
                        let Some(slot) = samples.get_mut(*index) else {
                            return Err(thrown_io(
                                "java.lang.ArrayIndexOutOfBoundsException: a DeviceN process \
                                 component beyond its colour space",
                            ));
                        };
                        *slot = sample;
                        process.to_rgb(&samples)?
                    }
                    Colorant::Spot(space) => space.to_rgb(&[sample])?,
                    Colorant::Unknown => return self.alternate.to_rgb(&tint(&self.tint, value)?),
                };
                for (rgb, component) in rgb.iter_mut().zip(component) {
                    *rgb *= component;
                }
            }
            return Ok(rgb);
        }
        self.alternate.to_rgb(&tint(&self.tint, value)?)
    }
}

impl Indexed {
    /// The RGB colour of `index`, the largest index the table holds for any above it.
    fn color(&self, index: usize) -> [u8; 3] {
        self.rgb[index.min(self.rgb.len() - 1)]
    }
}

/// `Math.round(float)`.
fn java_round(value: f32) -> i32 {
    (value + 0.5).floor() as i32
}

/// What `PDColorSpace.create` makes of `object`, with `resources` naming colour spaces.
pub(super) fn create(
    pdf: &Pdf,
    object: &Object,
    resources: Option<&Dictionary>,
) -> Result<ColorSpace, Unwritten> {
    create_nested(pdf, object, resources, false, 0)
}

fn create_nested(
    pdf: &Pdf,
    object: &Object,
    resources: Option<&Dictionary>,
    was_default: bool,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    if depth > MAX_NESTING {
        return Err(Unported(format!(
            "colour spaces nested deeper than {MAX_NESTING}, which PDFBox follows further"
        ))
        .into());
    }
    let Some(object) = pdf.resolve(object) else {
        return Err(Unported(
            "a colour space of null, which PDFBox fails on with its description".to_owned(),
        )
        .into());
    };
    match object {
        Object::Name(name) => named(pdf, name, resources, was_default, depth),
        Object::Array(array) => from_array(pdf, array, resources, was_default, depth),
        other => match as_dict(other).filter(|dict| dict.has(b"ColorSpace")) {
            Some(dict) => {
                let inner = dict.get(b"ColorSpace").expect("the key is there");
                create_nested(pdf, inner, resources, was_default, depth + 1)
            }
            None => Err(Unported(
                "an image colour space that is neither a name nor an array, which PDFBox fails \
                 on with its description"
                    .to_owned(),
            )
            .into()),
        },
    }
}

/// `PDDeviceCMYK.INSTANCE`: its profile, which LittleCMS always converts.
fn device_cmyk() -> ColorSpace {
    let profiled =
        Profiled::new(CMYK_PROFILE, 4).expect("LittleCMS converts PDFBox's CMYK profile");
    ColorSpace::Cmyk(Box::new(profiled))
}

/// The JDK's `ColorSpace.CS_CIEXYZ`.
fn ciexyz() -> IccToRgb {
    IccToRgb::new(CIEXYZ_PROFILE).expect("LittleCMS converts the JDK's CIE XYZ profile")
}

fn named(
    pdf: &Pdf,
    name: &[u8],
    resources: Option<&Dictionary>,
    was_default: bool,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    let spaces = resources.and_then(|resources| pdf.dict(resources, b"ColorSpace"));
    let default = match name {
        b"DeviceCMYK" => Some(&b"DefaultCMYK"[..]),
        b"DeviceRGB" => Some(&b"DefaultRGB"[..]),
        b"DeviceGray" => Some(&b"DefaultGray"[..]),
        _ => None,
    };
    if let (Some(spaces), Some(default), false) = (spaces, default, was_default) {
        if let Ok(object) = spaces.get(default) {
            return create_nested(pdf, object, resources, true, depth + 1);
        }
    }
    match name {
        b"DeviceGray" => return Ok(ColorSpace::Gray),
        b"DeviceRGB" => return Ok(ColorSpace::Rgb),
        b"DeviceCMYK" => return Ok(device_cmyk()),
        b"Pattern" => return Ok(ColorSpace::Pattern),
        _ => {}
    }
    let Some(resources) = resources else {
        return Err(Unwritten::Stream(Exception::new(
            MISSING_RESOURCE,
            format!("Unknown color space: {}", name_string(name)),
        )));
    };
    match spaces.and_then(|spaces| spaces.get(name).ok()) {
        Some(object) => create_nested(pdf, object, Some(resources), was_default, depth + 1),
        None => Err(Unwritten::Stream(Exception::new(
            MISSING_RESOURCE,
            format!("Missing color space: {}", name_string(name)),
        ))),
    }
}

fn from_array(
    pdf: &Pdf,
    array: &[Object],
    resources: Option<&Dictionary>,
    was_default: bool,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    let Some(first) = array.first() else {
        return Err(thrown_io("Colorspace array is empty"));
    };
    let Some(Object::Name(kind)) = pdf.resolve(first) else {
        return Err(thrown_io(
            "First element in colorspace array must be a name",
        ));
    };
    match kind.as_slice() {
        b"ICCBased" => icc_based(pdf, array, depth),
        b"Indexed" => indexed(pdf, array, resources, depth),
        b"Separation" => separation(pdf, array, resources, depth),
        b"DeviceN" => device_n(pdf, array, resources, depth),
        b"Lab" | b"CalRGB" | b"CalGray" => cie_based(pdf, kind, array),
        b"Pattern" => Ok(ColorSpace::Pattern),
        b"DeviceCMYK" | b"DeviceRGB" | b"DeviceGray" => {
            named(pdf, kind, resources, was_default, depth)
        }
        other => Err(thrown_io(&format!(
            "Invalid color space kind: COSName{{{}}}",
            name_string(other)
        ))),
    }
}

/// The entry `index` of a colour space array, as `COSArray.getObject` gives it.
fn entry<'a>(pdf: &'a Pdf, array: &'a [Object], index: usize) -> Option<&'a Object> {
    array.get(index).and_then(|item| pdf.resolve(item))
}

/// `PDSeparation(array, resources)`.
fn separation(
    pdf: &Pdf,
    array: &[Object],
    resources: Option<&Dictionary>,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    let alternate = alternate_space(pdf, array, resources, depth)?;
    let tint = tint_transform(pdf, array)?;
    let outputs = tint.output_count();
    if outputs > 0 && outputs < alternate.components() {
        return Err(thrown_io(&format!(
            "The tint transform function has less output parameters ({outputs}) than the \
             alternate colorspace {} ({})",
            alternate.name(),
            alternate.components()
        )));
    }
    Ok(ColorSpace::Separation(Box::new(Separation {
        alternate,
        tint,
    })))
}

/// The alternate colour space of a separation or DeviceN array, its entry 2.
fn alternate_space(
    pdf: &Pdf,
    array: &[Object],
    resources: Option<&Dictionary>,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    let alternate = array.get(2).unwrap_or(&Object::Null);
    create_nested(pdf, alternate, resources, false, depth + 1)
}

/// The tint transform of a separation or DeviceN array, its entry 3.
fn tint_transform(pdf: &Pdf, array: &[Object]) -> Result<Function, Unwritten> {
    let function = array.get(3).unwrap_or(&Object::Null);
    Function::create(pdf, function).map_err(function_failure)
}

/// `PDDeviceN(array, resources)`.
fn device_n(
    pdf: &Pdf,
    array: &[Object],
    resources: Option<&Dictionary>,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    let names: Vec<Vec<u8>> = match entry(pdf, array, 1) {
        Some(Object::Array(names)) => names
            .iter()
            .map(|name| match pdf.resolve(name) {
                Some(Object::Name(name)) => Ok(name.clone()),
                _ => Err(thrown_io("a DeviceN colorant that is not a name")),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(thrown_io("a DeviceN colour space without colorant names")),
    };
    let alternate = alternate_space(pdf, array, resources, depth)?;
    let tint = tint_transform(pdf, array)?;
    let attributes = match array.len() > 4 {
        true => {
            let Some(dict) = entry(pdf, array, 4).and_then(as_dict) else {
                return Err(thrown_io(
                    "java.lang.ClassCastException: DeviceN attributes that are not a dictionary",
                ));
            };
            Some(device_n_attributes(pdf, dict, &names, resources, depth)?)
        }
        false => None,
    };
    Ok(ColorSpace::DeviceN(Box::new(DeviceN {
        colorants: names.len(),
        alternate,
        tint,
        attributes,
    })))
}

/// `initColorConversionCache`: each colorant's process component, else its separation, a
/// separation of an `NChannel` process kept as the process component.
fn device_n_attributes(
    pdf: &Pdf,
    dict: &Dictionary,
    names: &[Vec<u8>],
    resources: Option<&Dictionary>,
    depth: usize,
) -> Result<Attributes, Unwritten> {
    let n_channel = pdf.name(dict, b"Subtype") == Some(b"NChannel");
    let process = pdf.dict(dict, b"Process");
    let components: Vec<Vec<u8>> = process
        .and_then(|process| pdf.array(process, b"Components"))
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Object::Name(name) => Some(name.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let process_space = match process.and_then(|process| pdf.get(process, b"ColorSpace")) {
        Some(space) => Some(create_nested(pdf, space, None, false, depth + 1)?),
        None => None,
    };
    let spots = pdf.dict(dict, b"Colorants");
    let mut colorants = Vec::with_capacity(names.len());
    for name in names {
        let component = process.and(components.iter().position(|component| component == name));
        let spot = match spots.and_then(|spots| pdf.get(spots, name)) {
            Some(value) => {
                let space = create_nested(pdf, value, resources, false, depth + 1)?;
                if !matches!(space, ColorSpace::Separation(_)) {
                    return Err(thrown_io(
                        "java.lang.ClassCastException: a DeviceN colorant that is not a \
                         separation",
                    ));
                }
                Some(space)
            }
            None => None,
        };
        colorants.push(match (component, spot) {
            (Some(index), Some(_)) if n_channel => Colorant::Process(index),
            (_, Some(spot)) => Colorant::Spot(spot),
            (Some(index), None) => Colorant::Process(index),
            (None, None) => Colorant::Unknown,
        });
    }
    Ok(Attributes {
        colorants,
        process: process_space,
    })
}

/// `PDLab`, `PDCalRGB` and `PDCalGray` of their dictionary.
fn cie_based(pdf: &Pdf, kind: &[u8], array: &[Object]) -> Result<ColorSpace, Unwritten> {
    let Some(dict) = entry(pdf, array, 1).and_then(as_dict) else {
        return Err(thrown_io(
            "java.lang.ClassCastException: a CIE-based colour space without a dictionary",
        ));
    };
    let numbers = |key: &[u8]| -> Option<Vec<f32>> {
        pdf.array(dict, key).map(|items| {
            items
                .iter()
                .map(|item| pdf.resolve(item).and_then(float_value).unwrap_or(0.0))
                .collect()
        })
    };
    let triple = |values: Option<Vec<f32>>, default: f32| -> Result<[f32; 3], Unwritten> {
        match values {
            None => Ok([default; 3]),
            Some(values) if values.len() >= 3 => Ok([values[0], values[1], values[2]]),
            Some(_) => Err(thrown_io("java.lang.IndexOutOfBoundsException")),
        }
    };
    let white = triple(numbers(b"WhitePoint"), 1.0)?;
    Ok(match kind {
        b"Lab" => {
            let range = numbers(b"Range")
                .filter(|range| range.len() >= 4)
                .unwrap_or_else(|| vec![-100.0, 100.0, -100.0, 100.0]);
            ColorSpace::Lab(Box::new(Lab {
                white,
                a: (range[0], range[1]),
                b: (range[2], range[3]),
                xyz: ciexyz(),
            }))
        }
        b"CalRGB" => {
            let matrix = match numbers(b"Matrix") {
                None => [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                Some(matrix) if matrix.len() >= 9 => std::array::from_fn(|i| matrix[i]),
                Some(_) => return Err(thrown_io("java.lang.ArrayIndexOutOfBoundsException")),
            };
            ColorSpace::CalRgb(Box::new(CalRgb {
                white,
                gamma: triple(numbers(b"Gamma"), 1.0)?,
                matrix,
                xyz: ciexyz(),
            }))
        }
        _ => ColorSpace::CalGray(Box::new(CalGray {
            white,
            gamma: pdf.float(dict, b"Gamma", 1.0),
            xyz: ciexyz(),
        })),
    })
}

/// `PDICCBased.create`, loading the profile as `loadICCProfile` does.
fn icc_based(pdf: &Pdf, array: &[Object], depth: usize) -> Result<ColorSpace, Unwritten> {
    if array.len() < 2 {
        return Err(thrown_io(
            "ICCBased colorspace array must have two elements",
        ));
    }
    let Some(Object::Stream(stream)) = pdf.resolve(&array[1]) else {
        return Err(thrown_io(
            "ICCBased colorspace array must have a stream as second element",
        ));
    };
    let Some(components) = usize::try_from(pdf.int(&stream.dict, b"N", -1)).ok() else {
        return Err(Unported(
            "an ICC colour space without a count of components, which PDFBox fails on".to_owned(),
        )
        .into());
    };
    let lab_alternate = matches!(
        pdf.get(&stream.dict, b"Alternate"),
        Some(Object::Array(items)) if matches!(items.first().and_then(|first| pdf.resolve(first)), Some(Object::Name(name)) if name == b"Lab")
    );
    let profile = pdf.decode(stream).map_err(|Unported(reason)| {
        Unported(format!("an ICC profile stream PDFBox may read: {reason}"))
    })?;
    let falls_back = |depth| -> Result<ColorSpace, Unwritten> {
        let alternate = alternate(pdf, &stream.dict, components, depth)?;
        Ok(ColorSpace::Icc(Box::new(Icc {
            components,
            conversion: IccConversion::Alternate(alternate),
            lab_alternate,
        })))
    };
    let Some(profile_components) = accepted_components(&profile) else {
        return falls_back(depth);
    };
    if components != profile_components {
        return Err(Unported(format!(
            "an ICC colour space whose N differs from its profile's {profile_components} \
             components"
        ))
        .into());
    }
    let model = String::from_utf8_lossy(&profile[MODEL]);
    if model.trim_matches(|c: char| c <= ' ') == "sRGB" {
        return Ok(ColorSpace::Icc(Box::new(Icc {
            components,
            conversion: IccConversion::Srgb,
            lab_alternate,
        })));
    }
    let profile = display_profile(profile);
    let Some(profiled) = Profiled::new(&profile, components) else {
        return falls_back(depth);
    };
    Ok(ColorSpace::Icc(Box::new(Icc {
        components,
        conversion: IccConversion::Profile(Box::new(profiled)),
        lab_alternate,
    })))
}

/// `PDICCBased.ensureDisplayProfile`: a profile of another class than a display's whose
/// rendering intent is perceptual taken as a display's. PDFBox reads only the first byte of the
/// four-byte intent, which is zero for every intent, so it takes nearly every profile as a
/// display's (#131).
fn display_profile(mut profile: Vec<u8>) -> Vec<u8> {
    if &profile[DEVICE_CLASS] != DISPLAY_CLASS && profile[RENDERING_INTENT] == PERCEPTUAL[..] {
        profile[DEVICE_CLASS].copy_from_slice(DISPLAY_CLASS);
    }
    profile
}

/// `PDICCBased.getAlternateColorSpace`.
fn alternate(
    pdf: &Pdf,
    dict: &Dictionary,
    components: usize,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    match pdf.get(dict, b"Alternate") {
        None => match components {
            1 => Ok(ColorSpace::Gray),
            3 => Ok(ColorSpace::Rgb),
            4 => Ok(device_cmyk()),
            other => Err(thrown_io(&format!(
                "Unknown color space number of components:{other}"
            ))),
        },
        Some(alternate @ (Object::Array(_) | Object::Name(_))) => {
            let array = match alternate {
                Object::Name(_) => vec![alternate.clone()],
                Object::Array(items) => items.clone(),
                _ => unreachable!("matched above"),
            };
            from_array(pdf, &array, None, false, depth + 1)
        }
        Some(_) => Err(Unported(
            "an ICC alternate colour space that is neither an array nor a name, which PDFBox \
             fails on with its description"
                .to_owned(),
        )
        .into()),
    }
}

/// `new PDIndexed(array, resources)`: the base colour space and the RGB colours of the
/// lookup table, as `readColorTable` and `initRgbColorTable` compute them.
fn indexed(
    pdf: &Pdf,
    array: &[Object],
    resources: Option<&Dictionary>,
    depth: usize,
) -> Result<ColorSpace, Unwritten> {
    let unported =
        |what: &str| Unwritten::Unported(Unported(format!("an indexed colour space {what}")));
    let (Some(base), Some(hival), lookup) = (array.get(1), array.get(2), array.get(3)) else {
        return Err(unported(
            "of fewer than three entries, which PDFBox fails on",
        ));
    };
    let base = create_nested(pdf, base, resources, false, depth + 1)?;
    let Some(hival) = pdf.resolve(hival).and_then(int_value) else {
        return Err(unported(
            "whose highest index is not a number, which PDFBox fails on",
        ));
    };
    let lookup = match lookup.and_then(|lookup| pdf.resolve(lookup)) {
        None => Vec::new(),
        Some(Object::String(bytes, _)) => bytes.clone(),
        Some(Object::Stream(stream)) => pdf.decode(stream)?,
        Some(_) => {
            return Err(unported(
                "whose lookup table is neither a string nor a stream",
            ))
        }
    };
    let components = base.components();
    let max_index = hival
        .min(255)
        .min(lookup.len() as i32 / components as i32 - 1);
    if max_index < 0 {
        return Err(unported("without colours, which PDFBox fails on"));
    }
    let entries = max_index as usize + 1;
    let samples: Vec<u8> = lookup[..entries * components]
        .iter()
        .map(|&byte| ((f32::from(byte) / 255.0) * 255.0) as u8)
        .collect();
    let Bitmap::Rgb(rgb) = base.to_rgb_image(entries as u32, 1, samples)? else {
        unreachable!("toRGBImage gives an RGB image")
    };
    Ok(ColorSpace::Indexed(Box::new(Indexed {
        rgb: rgb.pixels().map(|pixel| pixel.0).collect(),
    })))
}

fn thrown_io(message: &str) -> Unwritten {
    Unwritten::Stream(Exception::new(IO_EXCEPTION, message.to_owned()))
}

/// `PDPattern.toRGBImage`'s `UnsupportedOperationException`, which Tika does not catch, so
/// that it fails the whole document; the port records it as the image's failure (#125).
fn unsupported() -> Unwritten {
    Unwritten::Stream(Exception::bare(UNSUPPORTED_OPERATION))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile header of the device class `class` and the rendering intent `intent`.
    fn header(class: &[u8; 4], intent: u32) -> Vec<u8> {
        let mut profile = vec![0; 128];
        profile[DEVICE_CLASS].copy_from_slice(class);
        profile[RENDERING_INTENT].copy_from_slice(&intent.to_be_bytes());
        profile
    }

    #[test]
    fn takes_only_a_perceptual_profile_for_a_displays() {
        let class = |profile: Vec<u8>| display_profile(profile)[DEVICE_CLASS].to_vec();
        assert_eq!(class(header(b"prtr", 0)), DISPLAY_CLASS);
        assert_eq!(class(header(b"scnr", 0)), DISPLAY_CLASS);
        for intent in 1..=3 {
            assert_eq!(class(header(b"prtr", intent)), b"prtr", "intent {intent}");
        }
        assert_eq!(class(header(b"mntr", 2)), DISPLAY_CLASS);
    }

    #[test]
    fn converts_through_pdfboxs_profiles() {
        let ColorSpace::Cmyk(cmyk) = device_cmyk() else {
            unreachable!("DeviceCMYK is CMYK")
        };
        let white = cmyk.to_rgb(&[0.0; 4]);
        assert!(white.iter().all(|&c| c > 0.95), "{white:?}");
        let black = cmyk.to_rgb(&[0.0, 0.0, 0.0, 1.0]);
        assert!(black.iter().all(|&c| c < 0.25), "{black:?}");
        let xyz = ciexyz();
        let d65 = xyz_to_rgb(&xyz, 0.9505, 1.0, 1.089);
        assert!(d65.iter().all(|&c| c > 0.9), "{d65:?}");
    }

    /// What PDFBox's CIE-based colour spaces give colours on the JVM, as `CieColours.java`
    /// writes it.
    const CIE_COLOURS: &str = include_str!("../../../../tests/fixtures/pdf/cie-colours.tsv");

    #[test]
    fn converts_cie_based_colours_as_pdfbox_does_on_the_jvm() {
        let spaces = [
            (
                "Lab",
                ColorSpace::Lab(Box::new(Lab {
                    white: [0.9505, 1.0, 1.089],
                    a: (-100.0, 100.0),
                    b: (-100.0, 100.0),
                    xyz: ciexyz(),
                })),
            ),
            (
                "CalRGB",
                ColorSpace::CalRgb(Box::new(CalRgb {
                    white: [1.0; 3],
                    gamma: [2.2; 3],
                    matrix: [
                        0.4124, 0.2126, 0.0193, 0.3576, 0.7152, 0.1192, 0.1805, 0.0722, 0.9505,
                    ],
                    xyz: ciexyz(),
                })),
            ),
            (
                "CalGray",
                ColorSpace::CalGray(Box::new(CalGray {
                    white: [1.0; 3],
                    gamma: 2.2,
                    xyz: ciexyz(),
                })),
            ),
        ];
        let mut checked = 0;
        for line in CIE_COLOURS.lines().filter(|line| !line.starts_with('#')) {
            let [name, components, rgb] = line.split('\t').collect::<Vec<_>>()[..] else {
                panic!("{line}");
            };
            let (_, space) = spaces.iter().find(|(space, _)| *space == name).unwrap();
            let value: Vec<f32> = components
                .split(' ')
                .map(|component| component.parse().unwrap())
                .collect();
            let expected: Vec<u32> = rgb
                .split(' ')
                .map(|bits| u32::from_str_radix(bits, 16).unwrap())
                .collect();
            let found = space.to_rgb(&value).unwrap().map(f32::to_bits);
            assert_eq!(found.to_vec(), expected, "{line}");
            checked += 1;
        }
        assert_eq!(checked, 16);
    }
}
