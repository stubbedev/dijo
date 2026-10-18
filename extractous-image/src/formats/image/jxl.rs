//! JPEG XL images, read with jxl-oxide: the size, bit depth and orientation of the codestream's
//! header, and the Exif and XMP of the container's boxes, Brotli-compressed or not. Tika's
//! `JXLParser` only scans the file for an XMP packet and writes no document (#132); the native
//! parser writes the document of an image, as for every other image format, which the OCR
//! enricher then reads.

use jxl_oxide::{AuxBoxData, JxlImage};

use super::extractor::{self, Target};
use super::metadata_extractor::{exif, Metadata};
use super::{dates_zone, fails, Decline, Read};
use crate::formats::embedded::record_exception;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp;

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
/// XMP, then its Exif, then its header, which wins over what the Exif says of the image. Exif
/// that cannot be read is recorded as a warning.
pub(super) fn extract_metadata(
    input: &[u8],
    mut metadata: TikaMetadata,
) -> Result<Read, Decline<'static>> {
    let image = JxlImage::builder()
        .read(input)
        .map_err(|e| fails(format!("Can't read the JPEG XL image: {e}")))?;
    let boxes = image.aux_boxes();
    if let AuxBoxData::Data(packet) = boxes.first_xml() {
        xmp::extract(packet, &mut metadata, xmp::as_is)?;
    }
    let mut prefix_route_names = 0;
    match boxes.first_exif() {
        Ok(AuxBoxData::Data(raw)) => {
            let mut directories = Metadata::default();
            let header = i64::from(raw.tiff_header_offset());
            match exif::read_tiff(raw.payload(), header, &mut directories) {
                Ok(()) => {}
                Err(exif::TiffError::Bounds(bounds)) => unreadable_exif(&mut metadata, &bounds.0),
                Err(exif::TiffError::Processing(message)) => {
                    unreadable_exif(&mut metadata, &message)
                }
                Err(exif::TiffError::Unported(unported)) => return Err(unported.into()),
            }
            let mut target = Target::new(&mut metadata);
            extractor::handle(&directories, &mut target, dates_zone())?;
            prefix_route_names = target.prefix_route_names();
        }
        Ok(AuxBoxData::Decoding | AuxBoxData::NotFound) => {}
        Err(error) => unreadable_exif(&mut metadata, &error.to_string()),
    }
    let header = image.image_header();
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

/// Records why the Exif box could not be read.
fn unreadable_exif(metadata: &mut TikaMetadata, reason: &str) {
    record_exception(
        metadata,
        &format!("The Exif box of the JPEG XL image cannot be read: {reason}"),
    );
}
