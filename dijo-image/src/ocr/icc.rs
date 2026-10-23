//! The colour conversion the JDK's JPEG reader applies to the RGB samples of a JPEG with an
//! embedded ICC profile: a `ColorConvertOp` from the profile to the JDK's own sRGB profile, run by
//! LittleCMS, the CMM the JDK carries, of the same version (2.19), perceptually unless the
//! profile is a printer's. A profile `ICC_Profile` refuses, or one LittleCMS cannot make sRGB
//! into (`ICC_ColorSpace.fromRGB`), is ignored, as the reader ignores it; one that is accepted but
//! cannot convert three samples fails the reading, as it fails there.

use std::ffi::c_void;

use lcms2_sys as lcms;

/// The data of the JDK's `ICC_Profile.getInstance(ColorSpace.CS_sRGB)`, written by
/// `just ocr-image-reference`.
const JDK_SRGB: &[u8] = include_bytes!("../../models/jdk-srgb.icc");

/// `ProfileDataVerifier` and `ICC_Profile`'s header layout.
const HEADER_SIZE: usize = 128;
const TOC_OFFSET: usize = HEADER_SIZE + 4;
const TOC_RECORD_SIZE: usize = 12;
const MAX_TAG_COUNT: i32 = 100;
const PROFILE_FILE_SIGNATURE: i32 = 0x6163_7370;
const ABSOLUTE_COLORIMETRIC: u16 = 3;

const CLASS_OUTPUT: &[u8; 4] = b"prtr";
const CLASS_LINK: &[u8; 4] = b"link";
const PROFILE_CLASSES: [&[u8; 4]; 7] = [
    b"scnr",
    b"mntr",
    CLASS_OUTPUT,
    CLASS_LINK,
    b"spac",
    b"abst",
    b"nmcl",
];
const PCS_SPACES: [&[u8; 4]; 2] = [b"XYZ ", b"Lab "];
/// The colour spaces `ICC_Profile` knows, with their counts of components.
const COLOR_SPACES: [(&[u8; 4], usize); 25] = [
    (b"XYZ ", 3),
    (b"Lab ", 3),
    (b"Luv ", 3),
    (b"YCbr", 3),
    (b"Yxy ", 3),
    (b"RGB ", 3),
    (b"GRAY", 1),
    (b"HSV ", 3),
    (b"HLS ", 3),
    (b"CMYK", 4),
    (b"CMY ", 3),
    (b"2CLR", 2),
    (b"3CLR", 3),
    (b"4CLR", 4),
    (b"5CLR", 5),
    (b"6CLR", 6),
    (b"7CLR", 7),
    (b"8CLR", 8),
    (b"9CLR", 9),
    (b"ACLR", 10),
    (b"BCLR", 11),
    (b"CCLR", 12),
    (b"DCLR", 13),
    (b"ECLR", 14),
    (b"FCLR", 15),
];
const MAX_COMPONENTS: usize = {
    let mut max = 0;
    let mut i = 0;
    while i < COLOR_SPACES.len() {
        if COLOR_SPACES[i].1 > max {
            max = COLOR_SPACES[i].1;
        }
        i += 1;
    }
    max
};

/// Converts interleaved RGB samples in place from an embedded profile to sRGB, as the JDK's
/// JPEG reader does.
pub(crate) fn to_srgb(profile: &[u8], samples: &mut Vec<u8>) -> Result<(), String> {
    let Some(components) = accepted_components(profile) else {
        return Ok(());
    };
    let Some(embedded) = Profile::open(profile) else {
        return Ok(());
    };
    let srgb = Profile::open(JDK_SRGB).expect("the JDK's sRGB profile opens");
    let from_srgb = Transform::new(
        &srgb,
        format(3, 2),
        &embedded,
        format(components, 2),
        lcms::Intent::Perceptual,
    );
    if from_srgb.is_none() {
        return Ok(());
    }
    if components != 3 {
        return Err(format!(
            "an embedded ICC profile of {components} components for 3 samples a pixel"
        ));
    }
    let intent = if profile[12..16] == *CLASS_OUTPUT {
        lcms::Intent::RelativeColorimetric
    } else {
        lcms::Intent::Perceptual
    };
    let transform = Transform::new(&embedded, format(3, 1), &srgb, format(3, 1), intent)
        .ok_or("LittleCMS cannot convert the embedded ICC profile to sRGB")?;
    let pixels = u32::try_from(samples.len() / 3).map_err(|_| "too many pixels to convert")?;
    let mut converted = vec![0u8; samples.len()];
    unsafe {
        lcms::cmsDoTransform(
            transform.0,
            samples.as_ptr().cast::<c_void>(),
            converted.as_mut_ptr().cast::<c_void>(),
            pixels,
        );
    }
    *samples = converted;
    Ok(())
}

/// A `ColorConvertOp` from a profile to the JDK's sRGB, as `ICCBIFilter` makes it for an image:
/// colorimetrically from a printer's profile, perceptually from any other.
pub struct ToSrgb {
    transform: Transform,
    components: usize,
}

impl ToSrgb {
    /// The conversion from the profile `profile` of `components` components, or none when
    /// LittleCMS cannot make one.
    pub fn new(profile: &[u8], components: usize) -> Option<Self> {
        let embedded = Profile::open(profile)?;
        let srgb = Profile::open(JDK_SRGB).expect("the JDK's sRGB profile opens");
        let intent = if profile[12..16] == *CLASS_OUTPUT {
            lcms::Intent::RelativeColorimetric
        } else {
            lcms::Intent::Perceptual
        };
        let transform = Transform::new(
            &embedded,
            format(components, 1),
            &srgb,
            format(3, 1),
            intent,
        )?;
        Some(Self {
            transform,
            components,
        })
    }

    /// The sRGB samples of interleaved 8-bit samples of the profile, three a pixel.
    pub fn convert(&self, samples: &[u8]) -> Vec<u8> {
        let pixels = samples.len() / self.components;
        let mut converted = vec![0u8; pixels * 3];
        for (from, to) in samples
            .chunks(self.components * CONVERT_CHUNK)
            .zip(converted.chunks_mut(3 * CONVERT_CHUNK))
        {
            let count = u32::try_from(from.len() / self.components).expect("a chunk of pixels");
            unsafe {
                lcms::cmsDoTransform(
                    self.transform.0,
                    from.as_ptr().cast::<c_void>(),
                    to.as_mut_ptr().cast::<c_void>(),
                    count,
                );
            }
        }
        converted
    }
}

/// How many pixels one call to LittleCMS converts.
const CONVERT_CHUNK: usize = 1 << 16;

/// `ICC_ColorSpace.toRGB`: one colour of a profile's colour space converted to sRGB, each
/// component scaled from the colour space's range to 16 bits and run through a LittleCMS
/// transform to the JDK's sRGB profile with the perceptual intent `ColorTransform.Any` asks
/// for. Java2D's drawing of an image of such a colour space converts each pixel so too.
pub struct IccToRgb {
    transform: Transform,
    minimum: Vec<f32>,
    maximum: Vec<f32>,
}

impl IccToRgb {
    /// The conversion from the profile `profile`, or none where `ICC_Profile` refuses it or
    /// LittleCMS cannot make one.
    pub fn new(profile: &[u8]) -> Option<Self> {
        let components = accepted_components(profile)?;
        let (minimum, maximum) = component_ranges(&profile[16..20], components);
        let embedded = Profile::open(profile)?;
        let srgb = Profile::open(JDK_SRGB).expect("the JDK's sRGB profile opens");
        let transform = Transform::new(
            &embedded,
            format(components, 2),
            &srgb,
            format(3, 2),
            lcms::Intent::Perceptual,
        )?;
        Some(Self {
            transform,
            minimum,
            maximum,
        })
    }

    /// `getMinValue` and `getMaxValue` of each component.
    pub fn ranges(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.minimum
            .iter()
            .copied()
            .zip(self.maximum.iter().copied())
    }

    /// The sRGB colour of `value`, each component from 0 to 1.
    pub fn to_rgb(&self, value: &[f32]) -> [f32; 3] {
        let mut input = [0u16; MAX_COMPONENTS];
        let ranges = self.minimum.iter().zip(&self.maximum);
        for (i, (slot, (&min, &max))) in input.iter_mut().zip(ranges).enumerate() {
            let scaled =
                (value.get(i).copied().unwrap_or(0.0) - min) * (65535.0 / (max - min)) + 0.5;
            *slot = scaled as i32 as u16;
        }
        let mut output = [0u16; 3];
        unsafe {
            lcms::cmsDoTransform(
                self.transform.0,
                input.as_ptr().cast::<c_void>(),
                output.as_mut_ptr().cast::<c_void>(),
                1,
            );
        }
        output.map(|component| f32::from(component) / 65535.0)
    }
}

/// `ColorModel.getGray8TosRGB8LUT`: the sRGB level of each 8-bit grey level of a grey profile,
/// the blue of an 8-bit perceptual transform, as Java2D draws a grey image of that profile;
/// none where LittleCMS cannot make the transform.
pub fn gray_to_srgb_levels(profile: &[u8]) -> Option<[u8; 256]> {
    let embedded = Profile::open(profile)?;
    let srgb = Profile::open(JDK_SRGB).expect("the JDK's sRGB profile opens");
    let transform = Transform::new(
        &embedded,
        format(1, 1),
        &srgb,
        format(3, 1),
        lcms::Intent::Perceptual,
    )?;
    let levels: [u8; 256] = std::array::from_fn(|level| level as u8);
    let mut converted = [0u8; 256 * 3];
    unsafe {
        lcms::cmsDoTransform(
            transform.0,
            levels.as_ptr().cast::<c_void>(),
            converted.as_mut_ptr().cast::<c_void>(),
            256,
        );
    }
    Some(std::array::from_fn(|level| converted[level * 3 + 2]))
}

/// `ICC_ColorSpace`'s range of each of the `components` components of a colour space whose
/// header names `space`: L* from 0 to 100 and a* and b* from -128 to 127 for Lab, almost 2 for
/// XYZ, 0 to 1 otherwise.
fn component_ranges(space: &[u8], components: usize) -> (Vec<f32>, Vec<f32>) {
    match space {
        b"Lab " => (vec![0.0, -128.0, -128.0], vec![100.0, 127.0, 127.0]),
        b"XYZ " => (vec![0.0; 3], vec![1.0 + 32767.0 / 32768.0; 3]),
        _ => (vec![0.0; components], vec![1.0; components]),
    }
}

/// The count of components of a profile `ICC_Profile.getInstance` accepts, or none for one it
/// refuses: one `ProfileDataVerifier` finds damaged, or whose header names a class, a colour
/// space, a connection space or a rendering intent it does not know.
pub fn accepted_components(data: &[u8]) -> Option<usize> {
    if data.len() < TOC_OFFSET {
        return None;
    }
    let int = |at: usize| i32::from_be_bytes(data[at..at + 4].try_into().expect("four bytes"));
    let size = int(0);
    let tag_count = int(HEADER_SIZE);
    if !(0..=MAX_TAG_COUNT).contains(&tag_count) {
        return None;
    }
    let tags_end = TOC_OFFSET as i64 + i64::from(tag_count) * TOC_RECORD_SIZE as i64;
    if i64::from(size) < tags_end || size as usize > data.len() || size < 0 {
        return None;
    }
    if int(36) != PROFILE_FILE_SIGNATURE {
        return None;
    }
    for tag in 0..tag_count as usize {
        let record = TOC_OFFSET + tag * TOC_RECORD_SIZE;
        let (offset, length) = (int(record + 4), int(record + 8));
        if offset < TOC_OFFSET as i32 || offset > size {
            return None;
        }
        if length < 0 || length > i32::MAX - offset || length + offset > size {
            return None;
        }
    }
    let signature = |at: usize| -> &[u8; 4] { data[at..at + 4].try_into().expect("four bytes") };
    let class = signature(12);
    if !PROFILE_CLASSES.contains(&class) {
        return None;
    }
    let components = COLOR_SPACES
        .iter()
        .find(|(space, _)| *space == signature(16))
        .map(|&(_, components)| components)?;
    let pcs = signature(20);
    let pcs_known = if class == CLASS_LINK {
        COLOR_SPACES.iter().any(|(space, _)| *space == pcs)
    } else {
        PCS_SPACES.contains(&pcs)
    };
    if !pcs_known {
        return None;
    }
    let intent = u16::from_be_bytes([data[66], data[67]]);
    (intent <= ABSOLUTE_COLORIMETRIC).then_some(components)
}

/// `CHANNELS_SH(channels) | BYTES_SH(bytes)`, the formats the JDK hands LittleCMS.
fn format(channels: usize, bytes: u32) -> lcms::PixelFormat {
    lcms::PixelFormat(((channels as u32) << 3) | bytes)
}

/// A profile LittleCMS opened, closed when dropped.
struct Profile(lcms::HPROFILE);

impl Profile {
    fn open(data: &[u8]) -> Option<Self> {
        let size = u32::try_from(data.len()).ok()?;
        let handle = unsafe { lcms::cmsOpenProfileFromMem(data.as_ptr().cast::<c_void>(), size) };
        (!handle.is_null()).then_some(Self(handle))
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        unsafe { lcms::cmsCloseProfile(self.0) };
    }
}

/// A transform LittleCMS made, deleted when dropped.
struct Transform(lcms::HTRANSFORM);

impl Transform {
    fn new(
        input: &Profile,
        input_format: lcms::PixelFormat,
        output: &Profile,
        output_format: lcms::PixelFormat,
        intent: lcms::Intent,
    ) -> Option<Self> {
        let handle = unsafe {
            lcms::cmsCreateTransform(input.0, input_format, output.0, output_format, intent, 0)
        };
        (!handle.is_null()).then_some(Self(handle))
    }
}

impl Drop for Transform {
    fn drop(&mut self) {
        unsafe { lcms::cmsDeleteTransform(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_jdks_srgb_profile() {
        assert_eq!(accepted_components(JDK_SRGB), Some(3));
    }

    fn to_rgb_through_a_vec(conversion: &IccToRgb, value: &[f32]) -> [f32; 3] {
        let input: Vec<u16> = conversion
            .minimum
            .iter()
            .zip(&conversion.maximum)
            .enumerate()
            .map(|(i, (&min, &max))| {
                let scaled =
                    (value.get(i).copied().unwrap_or(0.0) - min) * (65535.0 / (max - min)) + 0.5;
                scaled as i32 as u16
            })
            .collect();
        let mut output = [0u16; 3];
        unsafe {
            lcms::cmsDoTransform(
                conversion.transform.0,
                input.as_ptr().cast::<c_void>(),
                output.as_mut_ptr().cast::<c_void>(),
                1,
            );
        }
        output.map(|component| f32::from(component) / 65535.0)
    }

    #[test]
    fn converts_a_colour_as_through_a_vec() {
        assert_eq!(MAX_COMPONENTS, 15);
        let conversion = IccToRgb::new(JDK_SRGB).expect("the JDK's sRGB profile converts");
        let values: [&[f32]; 9] = [
            &[],
            &[0.5],
            &[0.0, 0.0, 0.0],
            &[1.0, 1.0, 1.0],
            &[0.25, 0.5, 0.75],
            &[0.1, 0.9, 0.3, 0.7],
            &[-0.5, 1.5, 0.5],
            &[2.0, -3.0, 0.999],
            &[f32::NAN, 0.2, 0.4],
        ];
        for value in values {
            let found = conversion.to_rgb(value).map(f32::to_bits);
            let expected = to_rgb_through_a_vec(&conversion, value).map(f32::to_bits);
            assert_eq!(found, expected, "{value:?}");
        }
    }

    #[test]
    fn converts_samples_to_srgb_as_through_a_copy() {
        let original: Vec<u8> = (0..=255u8).chain([7, 11]).collect();
        let mut converted = original.clone();
        to_srgb(JDK_SRGB, &mut converted).expect("the JDK's sRGB profile converts");
        let (embedded, srgb) = (
            Profile::open(JDK_SRGB).unwrap(),
            Profile::open(JDK_SRGB).unwrap(),
        );
        let intent = lcms::Intent::Perceptual;
        let transform =
            Transform::new(&embedded, format(3, 1), &srgb, format(3, 1), intent).unwrap();
        let mut copied = original;
        let mut buffer = vec![0u8; copied.len()];
        unsafe {
            lcms::cmsDoTransform(
                transform.0,
                copied.as_ptr().cast::<c_void>(),
                buffer.as_mut_ptr().cast::<c_void>(),
                u32::try_from(copied.len() / 3).unwrap(),
            );
        }
        copied.copy_from_slice(&buffer);
        assert_eq!(converted, copied);
    }
}
