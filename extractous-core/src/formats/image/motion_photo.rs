//! Tika's `MotionPhoto`: the video a motion photo appends to its image, which the image's XMP
//! declares by its length (`Container:Directory`) or its offset from the end (`MicroVideo`),
//! typed by its content and parsed as an embedded document.

use crate::formats::embedded::{ResourceType, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::Unported;
use crate::mime::{self, Origin};

const NAME: &str = "motion-photo";
const DIRECTORY: &str = "xmp-raw:Container:Directory[";
const ITEM: &str = "]/Container:Item/";
const MOTION_PHOTO: &str = "MotionPhoto";
const MAX_ITEMS: usize = 64;
const DETECTION_PREFIX: usize = 8 * 1024;
const MICRO_VIDEO_OFFSET: &str = "Camera:MicroVideoOffset";

/// The video at the end of a motion photo: where it starts, and the metadata it is parsed with.
#[derive(Debug, Clone)]
pub(crate) struct Trailer {
    pub(crate) start: usize,
    pub(crate) metadata: TikaMetadata,
}

/// What the image says of its video: its length to the end of the file and its claimed type.
struct Declaration {
    length: i64,
    mime: Option<String>,
}

/// `EmbeddedDocumentUtil.getExtensionForMediaType`: the first extension of the registered type a
/// name normalizes to, of its base type when it has parameters, or nothing.
fn extension_for(name: &str) -> &'static str {
    let Some(media_type) = mime::MediaType::parse(name) else {
        return "";
    };
    match mime::extension(&media_type) {
        "" if media_type.has_parameters() => mime::extension(&media_type.base_type()),
        extension => extension,
    }
}

fn positive_long(value: Option<&str>) -> Option<i64> {
    crate::java::parse_long(crate::java::trim(value?)).filter(|value| *value > 0)
}

fn declaration(metadata: &TikaMetadata) -> Option<Declaration> {
    if let Some(declared) = directory_declaration(metadata) {
        return Some(declared);
    }
    let offset = positive_long(metadata.get(MICRO_VIDEO_OFFSET))?;
    Some(Declaration {
        length: offset,
        mime: None,
    })
}

/// `directoryDeclaration`: the length and type of the `MotionPhoto` item of the container
/// directory, read up to the first item without a semantic.
fn directory_declaration(metadata: &TikaMetadata) -> Option<Declaration> {
    for index in 1..=MAX_ITEMS {
        let item = format!("{DIRECTORY}{index}{ITEM}");
        let semantic = metadata.get(&format!("{item}Item:Semantic"))?;
        if semantic == MOTION_PHOTO {
            let length = positive_long(metadata.get(&format!("{item}Item:Length")))?;
            return Some(Declaration {
                length,
                mime: metadata.get(&format!("{item}Item:Mime")).map(str::to_owned),
            });
        }
    }
    None
}

/// `MotionPhoto.extract` as far as deciding what to parse: the trailer the image's metadata
/// declares, if it fits in the file and its content is recognized.
pub(crate) fn trailer(metadata: &TikaMetadata, input: &[u8]) -> Result<Option<Trailer>, Unported> {
    let Some(declared) = declaration(metadata) else {
        return Ok(None);
    };
    let size = input.len() as i64;
    if declared.length >= size {
        return Ok(None);
    }
    let start = size - declared.length;
    if start <= 0 {
        return Ok(None);
    }
    let start = start as usize;
    let prefix = &input[start..(start + DETECTION_PREFIX).min(input.len())];
    let detected = mime::detect(prefix, Origin::Bytes)
        .map_err(|e| {
            Unported::new(format!(
                "a motion photo trailer Tika's detection fails on: {e}"
            ))
        })?
        .media_type;
    if detected.as_str() == "application/octet-stream" {
        return Ok(None);
    }
    if detected.kind() == "image" {
        return Err(Unported::new(
            "a motion photo whose trailer is an image, which Tika does not search for a trailer of its own",
        ));
    }
    let from_detection = declared
        .mime
        .as_deref()
        .is_some_and(|mime| !mime.starts_with(&format!("{}/", detected.kind())));
    let extension = match &declared.mime {
        None => "",
        Some(mime) => extension_for(if from_detection {
            detected.as_str()
        } else {
            mime
        }),
    };
    let mut trailer = TikaMetadata::new();
    trailer.set(RESOURCE_NAME, format!("{NAME}{extension}"));
    if from_detection && !extension.is_empty() {
        trailer.set("tk:resource-name-extension-inferred", "true");
    }
    trailer.set("Content-Type", detected.as_str());
    trailer.set("Content-Length", declared.length.to_string());
    ResourceType::Attachment.set(&mut trailer);
    Ok(Some(Trailer {
        start,
        metadata: trailer,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_the_extensions_of_types_as_tika_does() {
        assert_eq!(super::extension_for("video/mp4"), ".mp4");
        assert_eq!(super::extension_for("image/jpeg"), ".jpg");
        assert_eq!(super::extension_for("text/plain; charset=UTF-8"), ".txt");
        assert_eq!(super::extension_for("application/x-no-such-type"), "");
        assert_eq!(super::extension_for("not a type"), "");
    }
}
