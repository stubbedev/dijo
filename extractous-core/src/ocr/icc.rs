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

/// Converts interleaved RGB samples in place from an embedded profile to sRGB, as the JDK's
/// JPEG reader does.
pub(crate) fn to_srgb(profile: &[u8], samples: &mut [u8]) -> Result<(), String> {
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
    samples.copy_from_slice(&converted);
    Ok(())
}

/// The count of components of a profile `ICC_Profile.getInstance` accepts, or none for one it
/// refuses: one `ProfileDataVerifier` finds damaged, or whose header names a class, a colour
/// space, a connection space or a rendering intent it does not know.
fn accepted_components(data: &[u8]) -> Option<usize> {
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
}
