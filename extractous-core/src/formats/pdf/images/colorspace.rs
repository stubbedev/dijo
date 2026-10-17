//! The colour spaces of images, as PDFBox's `PDColorSpace.create` makes them and their
//! `toRGBImage` converts a raster of an image's samples to the image Java writes: grey and RGB,
//! ICC profiles, which the JDK converts to sRGB with LittleCMS, and palettes over them. The
//! colour spaces whose conversion the port does not do leave the document to Tika.

use lopdf::{Dictionary, Object};

use super::super::cos::{as_dict, int_value, name_string, Pdf};
use super::super::Unported;
use super::{Thrown, Unwritten};
use crate::ocr::bitmap::Bitmap;
use crate::ocr::icc::{accepted_components, ToSrgb};

/// How deep colour spaces may name colour spaces before the port stops following them.
const MAX_NESTING: usize = 8;
const MISSING_RESOURCE: &str = "org.apache.pdfbox.pdmodel.MissingResourceException";
const IO_EXCEPTION: &str = "java.io.IOException";
/// `ICC_Profile.icHdrModel`: where a profile's header names the device model.
const MODEL: std::ops::Range<usize> = 52..59;
/// `ICC_Profile.icHdrDeviceClass`.
const DEVICE_CLASS: std::ops::Range<usize> = 12..16;
/// `ICC_Profile.icHdrRenderingIntent`.
const RENDERING_INTENT: std::ops::Range<usize> = 64..68;
/// `ICC_Profile.icPerceptual`, as the header holds it.
const PERCEPTUAL: [u8; 4] = [0; 4];
const DISPLAY_CLASS: &[u8; 4] = b"mntr";

/// A colour space PDFBox reads an image's samples in.
pub(super) enum ColorSpace {
    /// `PDDeviceGray.INSTANCE`.
    Gray,
    /// `PDDeviceRGB.INSTANCE`.
    Rgb,
    /// `PDICCBased`.
    Icc(Box<Icc>),
    /// `PDIndexed`.
    Indexed(Box<Indexed>),
}

/// `PDICCBased`: its count of components, from its stream's `N`, and how its samples become
/// sRGB.
pub(super) struct Icc {
    components: usize,
    conversion: IccConversion,
}

enum IccConversion {
    /// A profile of the sRGB model, for which PDFBox takes the JDK's own sRGB profile: the
    /// samples are sRGB.
    Srgb,
    /// Another profile, which `ColorConvertOp` converts with LittleCMS.
    Profile(ToSrgb),
    /// The alternate colour space PDFBox falls back to for a profile the JDK refuses.
    Alternate(ColorSpace),
}

/// `PDIndexed`: the RGB colour of each index up to the largest the lookup table holds.
pub(super) struct Indexed {
    rgb: Vec<[u8; 3]>,
}

impl ColorSpace {
    /// `getName`.
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Gray => "DeviceGray",
            Self::Rgb => "DeviceRGB",
            Self::Icc(_) => "ICCBased",
            Self::Indexed(_) => "Indexed",
        }
    }

    /// `getNumberOfComponents`.
    pub(super) fn components(&self) -> usize {
        match self {
            Self::Gray | Self::Indexed(_) => 1,
            Self::Rgb => 3,
            Self::Icc(icc) => icc.components,
        }
    }

    /// `getDefaultDecode`: every component from 0 to 1, a palette's index from 0 to its
    /// largest value.
    pub(super) fn default_decode(&self, bits_per_component: i32) -> Vec<f32> {
        match self {
            Self::Indexed(_) => vec![0.0, 2f32.powi(bits_per_component) - 1.0],
            Self::Icc(icc) => match &icc.conversion {
                IccConversion::Alternate(alternate) => alternate.default_decode(bits_per_component),
                _ => [0.0, 1.0].repeat(icc.components),
            },
            Self::Gray | Self::Rgb => [0.0, 1.0].repeat(self.components()),
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
            Self::Icc(icc) => match &icc.conversion {
                IccConversion::Alternate(alternate) => {
                    return alternate.to_rgb_image(width, height, samples)
                }
                IccConversion::Srgb => samples,
                IccConversion::Profile(conversion) => {
                    if width == 1 || height == 1 {
                        return Err(Unported(
                            "an image of one row or column in an ICC colour space, which \
                             PDFBox converts by Java2D's drawing rather than LittleCMS"
                                .to_owned(),
                        )
                        .into());
                    }
                    conversion.convert(&samples)
                }
            },
        };
        Ok(Bitmap::Rgb(
            image::RgbImage::from_raw(width, height, rgb)
                .expect("a raster holds three samples a pixel"),
        ))
    }

    /// `toRGB` of one colour, each component from 0 to 1.
    pub(super) fn to_rgb(&self, value: &[f32]) -> Result<[f32; 3], Unported> {
        match self {
            Self::Gray => Ok([value[0]; 3]),
            Self::Rgb => Ok([value[0], value[1], value[2]]),
            Self::Indexed(indexed) => {
                let index = (java_round(value[0]).max(0) as usize).min(indexed.rgb.len() - 1);
                Ok(indexed.rgb[index].map(|component| f32::from(component) / 255.0))
            }
            Self::Icc(icc) => match &icc.conversion {
                IccConversion::Srgb => Ok([value[0], value[1], value[2]]),
                IccConversion::Alternate(alternate) => alternate.to_rgb(value),
                IccConversion::Profile(_) => Err(Unported(
                    "a matte colour in an ICC colour space, which the JDK converts in floating \
                     point"
                        .to_owned(),
                )),
            },
        }
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
        b"DeviceCMYK" => return Err(unported_kind("DeviceCMYK")),
        b"Pattern" => return Err(unported_kind("Pattern")),
        _ => {}
    }
    let Some(resources) = resources else {
        return Err(Unwritten::Stream(Thrown::new(
            MISSING_RESOURCE,
            format!("Unknown color space: {}", name_string(name)),
        )));
    };
    match spaces.and_then(|spaces| spaces.get(name).ok()) {
        Some(object) => create_nested(pdf, object, Some(resources), was_default, depth + 1),
        None => Err(Unwritten::Stream(Thrown::new(
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
        b"DeviceCMYK" | b"DeviceRGB" | b"DeviceGray" => {
            named(pdf, kind, resources, was_default, depth)
        }
        b"CalGray" | b"CalRGB" | b"DeviceN" | b"Separation" | b"Lab" | b"Pattern" => {
            Err(unported_kind(&name_string(kind)))
        }
        other => Err(thrown_io(&format!(
            "Invalid color space kind: COSName{{{}}}",
            name_string(other)
        ))),
    }
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
    let profile = pdf.decode(stream).map_err(|Unported(reason)| {
        Unported(format!("an ICC profile stream PDFBox may read: {reason}"))
    })?;
    let Some(profile_components) = accepted_components(&profile) else {
        let alternate = alternate(pdf, &stream.dict, components, depth)?;
        return Ok(ColorSpace::Icc(Box::new(Icc {
            components,
            conversion: IccConversion::Alternate(alternate),
        })));
    };
    if components != profile_components {
        return Err(Unported(format!(
            "an ICC colour space whose N differs from its profile's {profile_components} \
             components"
        ))
        .into());
    }
    if !matches!(&profile[16..20], b"RGB " | b"GRAY") {
        return Err(Unported(format!(
            "an ICC profile of the colour space {}, whose ranges and conversion the port does \
             not do",
            String::from_utf8_lossy(&profile[16..20])
        ))
        .into());
    }
    let model = String::from_utf8_lossy(&profile[MODEL]);
    if model.trim_matches(|c: char| c <= ' ') == "sRGB" {
        return Ok(ColorSpace::Icc(Box::new(Icc {
            components,
            conversion: IccConversion::Srgb,
        })));
    }
    let profile = display_profile(profile);
    let conversion = ToSrgb::new(&profile, components).ok_or_else(|| {
        Unported(
            "an ICC profile LittleCMS cannot convert, which PDFBox replaces by its alternate \
             colour space or fails on"
                .to_owned(),
        )
    })?;
    Ok(ColorSpace::Icc(Box::new(Icc {
        components,
        conversion: IccConversion::Profile(conversion),
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
            4 => Err(unported_kind("DeviceCMYK")),
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
    let rgb = match &base {
        ColorSpace::Indexed(_) => {
            return Err(unported(
                "over another indexed colour space, which the port does not look up",
            ))
        }
        ColorSpace::Icc(icc) if matches!(icc.conversion, IccConversion::Profile(_)) => {
            return Err(unported(
                "over an ICC profile, whose colours Java2D's drawing converts",
            ))
        }
        _ => base.to_rgb_image(entries as u32, 1, samples)?,
    };
    let Bitmap::Rgb(rgb) = rgb else {
        unreachable!("toRGBImage gives an RGB image")
    };
    Ok(ColorSpace::Indexed(Box::new(Indexed {
        rgb: rgb.pixels().map(|pixel| pixel.0).collect(),
    })))
}

fn thrown_io(message: &str) -> Unwritten {
    Unwritten::Stream(Thrown::new(IO_EXCEPTION, message.to_owned()))
}

fn unported_kind(kind: &str) -> Unwritten {
    Unwritten::Unported(Unported(format!(
        "an image in the colour space {kind}, whose conversion the port does not do"
    )))
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
}
