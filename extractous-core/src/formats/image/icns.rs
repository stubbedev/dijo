//! Apple icon images, as Tika's `ICNSParser` reads them: the kinds of the icons and masks the
//! file holds, by their type codes, and an empty document.

use super::fails;
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};

const SIGNATURE: [u8; 4] = *b"icns";
/// `MAX_IMAGE_LENGTH_BYTES`.
const MAX_IMAGE_LENGTH: i32 = 10_485_760;
const CONTENT_TYPE: &str = "image/icns";

/// An icon kind of `ICNSType`.
struct IconType {
    code: [u8; 4],
    width: u32,
    height: u32,
    bits_per_pixel: u32,
    has_mask: bool,
    retina: bool,
}

const fn icon(
    code: &[u8; 4],
    width: u32,
    height: u32,
    bits_per_pixel: u32,
    has_mask: bool,
    retina: bool,
) -> IconType {
    IconType {
        code: *code,
        width,
        height,
        bits_per_pixel,
        has_mask,
        retina,
    }
}

/// `ICNSType.allImageTypes`.
const ICON_TYPES: [IconType; 32] = [
    icon(b"ICON", 32, 32, 1, false, false),
    icon(b"icm#", 16, 12, 1, true, false),
    icon(b"icm4", 16, 12, 4, false, false),
    icon(b"icm8", 16, 12, 8, false, false),
    icon(b"ics#", 16, 16, 1, true, false),
    icon(b"ics4", 16, 16, 4, false, false),
    icon(b"ics8", 16, 16, 8, false, false),
    icon(b"is32", 16, 16, 24, false, false),
    icon(b"ICN#", 32, 32, 1, true, false),
    icon(b"icl4", 32, 32, 4, false, false),
    icon(b"icl8", 32, 32, 8, false, false),
    icon(b"il32", 32, 32, 24, false, false),
    icon(b"ich#", 48, 48, 1, true, false),
    icon(b"ich4", 48, 48, 4, false, false),
    icon(b"ich8", 48, 48, 8, false, false),
    icon(b"ih32", 48, 48, 24, false, false),
    icon(b"it32", 128, 128, 24, false, false),
    icon(b"s8mk", 16, 16, 8, true, false),
    icon(b"l8mk", 32, 32, 8, true, false),
    icon(b"h8mk", 48, 48, 8, true, false),
    icon(b"t8mk", 128, 128, 8, true, false),
    icon(b"icp4", 16, 16, 0, false, false),
    icon(b"icp5", 32, 32, 0, false, false),
    icon(b"icp6", 64, 64, 0, false, false),
    icon(b"icp7", 128, 128, 0, false, false),
    icon(b"ic08", 256, 256, 0, false, false),
    icon(b"ic09", 512, 512, 0, false, false),
    icon(b"ic10", 1024, 1024, 0, false, true),
    icon(b"ic11", 16, 16, 0, false, true),
    icon(b"ic12", 32, 32, 0, false, true),
    icon(b"ic13", 128, 128, 0, false, true),
    icon(b"ic14", 256, 256, 0, false, true),
];

/// `IOUtils.read` of up to four bytes at `at` into `header`, which keeps what it held past
/// those the input still has.
fn read_into(input: &[u8], at: usize, header: &mut [u8; 4]) {
    for (slot, &byte) in header.iter_mut().zip(input.get(at..).unwrap_or_default()) {
        *slot = byte;
    }
}

/// `ICNSParser.parse` as far as the document: the metadata it sets.
fn read(input: &[u8], metadata: &mut TikaMetadata) -> Result<(), Declined> {
    let mut header = [0; 4];
    read_into(input, 0, &mut header);
    if header != SIGNATURE {
        return Err(fails("ICNS magic signature invalid".to_owned()));
    }
    read_into(input, 4, &mut header);
    let length = i32::from_be_bytes(header).wrapping_sub(8);
    if length > MAX_IMAGE_LENGTH {
        return Err(fails(format!(
            "tried to allocate {length} bytes, but the limit is {MAX_IMAGE_LENGTH}"
        )));
    }
    let size =
        usize::try_from(length).map_err(|_| fails("image length must be >= 0".to_owned()))?;
    let file = input.get(8..8 + size).ok_or_else(|| {
        fails(format!(
            "the ICNS file ends before the {size} bytes it declares"
        ))
    })?;
    let mut icons = Vec::new();
    let mut masks = Vec::new();
    let mut offset: i32 = 0;
    while offset < length - 8 {
        let at = usize::try_from(offset).map_err(|_| {
            fails(format!(
                "Index {offset} out of bounds for length {length} in the ICNS file"
            ))
        })?;
        let code: [u8; 4] = file[at..at + 4].try_into().expect("four bytes");
        let Some(kind) = ICON_TYPES.iter().find(|kind| kind.code == code) else {
            break;
        };
        if kind.has_mask {
            masks.push(kind);
        } else {
            icons.push(kind);
        }
        let icon_length = i32::from_be_bytes(file[at + 4..at + 8].try_into().expect("four bytes"));
        if icon_length <= 0 {
            break;
        }
        offset = offset.wrapping_add(icon_length);
    }
    metadata.set("Content-Type", CONTENT_TYPE);
    if !icons.is_empty() {
        let details: Vec<String> = icons
            .iter()
            .map(|icon| {
                let bits = if icon.bits_per_pixel != 0 {
                    format!("{} bpp", icon.bits_per_pixel)
                } else {
                    "JPEG 2000 or PNG format".to_owned()
                };
                let retina = if icon.retina { "@2X" } else { "" };
                format!("{}x{}{retina} ({bits})", icon.height, icon.width)
            })
            .collect();
        metadata.set("Icon count", icons.len().to_string());
        metadata.set("Icon details", details.join(", "));
    }
    if !masks.is_empty() {
        let details: Vec<String> = masks
            .iter()
            .map(|mask| {
                format!(
                    "{}x{} ({} bpp)",
                    mask.height, mask.width, mask.bits_per_pixel
                )
            })
            .collect();
        metadata.set("Masked icon count", masks.len().to_string());
        metadata.set("Masked icon details", details.join(", "));
    }
    Ok(())
}

/// Tika's `ICNSParser`.
pub(crate) struct IcnsParser;

impl Parser for IcnsParser {
    fn name(&self) -> &'static str {
        "extractous::ICNSParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.ICNSParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[CONTENT_TYPE]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let mut read_metadata = metadata.clone();
        read(input, &mut read_metadata)?;
        Ok(Box::new(move |xhtml| {
            *xhtml.metadata_mut() = read_metadata;
            Ok(())
        }))
    }
}
