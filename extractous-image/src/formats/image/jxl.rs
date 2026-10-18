//! JPEG XL images, read in place ([`crate::jxl`]): the size, bit depth and orientation of the
//! codestream's header, and the Exif and XMP of the container's boxes, Brotli-compressed or not.
//! Tika's `JXLParser` only scans the file for an XMP packet and writes no document (#132); the
//! native parser writes the document of an image, as for every other image format, which the
//! OCR enricher then reads.

use super::extractor::{self, Target};
use super::metadata_extractor::{exif, Metadata};
use super::{dates_zone, fails, Decline, Read};
use crate::formats::embedded::record_exception;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp;
use crate::jxl::Container;

/// For the parity tests: Tika's result of a JPEG XL image, from the native one. It has no
/// document, nor the OCR enricher's reading of it, and only the metadata Tika's scan for an XMP
/// packet finds, without what the header and the boxes add (#132).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn as_tika_writes_no_jpeg_xl_document(
    text: &mut String,
    metadata: &mut crate::formats::parity::ComparedMetadata,
    tika: &crate::formats::parity::ComparedMetadata,
) {
    let [(_, ocr)] = super::TIKA_DELEGATES;
    text.clear();
    metadata.retain(|(name, _)| tika.iter().any(|(tika_name, _)| tika_name == name));
    for (_, values) in metadata
        .iter_mut()
        .filter(|(name, _)| name.starts_with("tk:parsed-by"))
    {
        values.retain(|value| value != ocr);
    }
}

/// The metadata of the JPEG XL image `input` into `metadata`, which is then the image's: its
/// XMP, then its Exif, then its header, which wins over what the Exif says of the image. An Exif
/// or XML box that cannot be read is recorded as a warning.
pub(super) fn extract_metadata(
    input: &[u8],
    mut metadata: TikaMetadata,
) -> Result<Read, Decline<'static>> {
    let unreadable_image = |e: String| fails(format!("Can't read the JPEG XL image: {e}"));
    let container = Container::read(input).map_err(unreadable_image)?;
    let header = container.header().map_err(unreadable_image)?;
    if let Some(xml) = container.xml() {
        match xml.content() {
            Ok(packet) => xmp::extract(&packet, &mut metadata, xmp::as_is)?,
            Err(reason) => unreadable_box(&mut metadata, "XML", &reason),
        }
    }
    let mut prefix_route_names = 0;
    if let Some(exif) = container.exif() {
        let content = exif.content();
        match content
            .as_deref()
            .map_err(String::clone)
            .and_then(tiff_of_exif)
        {
            Ok((tiff, offset)) => {
                let mut directories = Metadata::default();
                match exif::read_tiff(tiff, offset, &mut directories) {
                    Ok(()) => {}
                    Err(exif::TiffError::Bounds(bounds)) => {
                        unreadable_box(&mut metadata, "Exif", &bounds.0)
                    }
                    Err(exif::TiffError::Processing(message)) => {
                        unreadable_box(&mut metadata, "Exif", &message)
                    }
                    Err(exif::TiffError::Unported(unported)) => return Err(unported.into()),
                }
                let mut target = Target::new(&mut metadata);
                extractor::handle(&directories, &mut target, dates_zone())?;
                prefix_route_names = target.prefix_route_names();
            }
            Err(reason) => unreadable_box(&mut metadata, "Exif", &reason),
        }
    }
    let size = &header.size;
    xmp::set(
        &mut metadata,
        &extractor::BITS_PER_SAMPLE,
        &header.metadata.bit_depth.bits_per_sample().to_string(),
    );
    xmp::set(
        &mut metadata,
        &extractor::IMAGE_LENGTH,
        &size.height.to_string(),
    );
    xmp::set(
        &mut metadata,
        &extractor::IMAGE_WIDTH,
        &size.width.to_string(),
    );
    xmp::set(
        &mut metadata,
        &extractor::ORIENTATION,
        &header.metadata.orientation.to_string(),
    );
    Ok(Read {
        metadata,
        prefix_route_names,
        reads_file: false,
    })
}

/// The payload of an Exif box's `content` after the offset of its TIFF header the box starts
/// with, and that offset.
fn tiff_of_exif(content: &[u8]) -> Result<(&[u8], i64), String> {
    let Some((offset, payload)) = content.split_first_chunk::<4>() else {
        return Err(format!(
            "it holds {} bytes, too few for the offset of its TIFF header",
            content.len()
        ));
    };
    let offset = u32::from_be_bytes(*offset);
    if offset as usize >= payload.len() {
        return Err(format!(
            "its TIFF header is at {offset}, past the {} bytes it holds",
            payload.len()
        ));
    }
    Ok((payload, i64::from(offset)))
}

/// Records why the box of `kind` could not be read.
fn unreadable_box(metadata: &mut TikaMetadata, kind: &str, reason: &str) {
    record_exception(
        metadata,
        &format!("The {kind} box of the JPEG XL image cannot be read: {reason}"),
    );
}
