//! Audio and video files, as Tika's audio and video module reads their metadata: MP3 ([`mp3`]),
//! MP4, QuickTime and 3GPP ([`mp4`]), Flash video ([`flv`]), WAVE, AU and AIFF ([`audio`]), MIDI
//! ([`midi`]), and Ogg Vorbis, Opus and FLAC ([`ogg`]). What its parsers share lives here: `CoverArt`, which hands a file's
//! pictures to the embedded document extractor, and `NumberAndTotal`, which reads a track or
//! disc number.

mod audio;
mod flv;
mod midi;
mod mp3;
mod mp4;
mod ogg;

pub use audio::AudioParser;
pub use flv::FlvParser;
pub use midi::MidiParser;
pub use mp3::Mp3Parser;
pub use mp4::Mp4Parser;
pub use ogg::{FlacParser, OpusParser, VorbisParser};

use crate::formats::embedded::{ResourceType, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::Failure;
use crate::java;

/// The ID3 picture types, `ID3Tags.PICTURE_TYPES`.
const PICTURE_TYPES: [&str; 21] = [
    "Other",
    "32x32 pixels 'file icon' (PNG only)",
    "Other file icon",
    "Cover (front)",
    "Cover (back)",
    "Leaflet page",
    "Media (e.g. label side of CD)",
    "Lead artist/lead performer/soloist",
    "Artist/performer",
    "Conductor",
    "Band/Orchestra",
    "Composer",
    "Lyricist/text writer",
    "Recording Location",
    "During recording",
    "During performance",
    "Movie/video screen capture",
    "A bright coloured fish",
    "Illustration",
    "Band/artist logotype",
    "Publisher/Studio logotype",
];
const FRONT_COVER: i64 = 3;
const OTHER: i64 = 0;

/// A picture of a file, `CoverArt.Picture`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Picture {
    pub(super) kind: i64,
    pub(super) mime_type: Option<String>,
    pub(super) description: Option<String>,
    pub(super) data: Vec<u8>,
}

impl Picture {
    /// `normalizedType`: the type, or `-1` past the known ones.
    fn normalized_type(&self) -> i64 {
        if self.kind >= PICTURE_TYPES.len() as i64 {
            -1
        } else {
            self.kind
        }
    }
}

/// `CoverArt.thumbnailIndex`: the front cover, else the first picture of type `Other` or less,
/// else the first.
fn thumbnail_index(pictures: &[Picture]) -> Option<usize> {
    if pictures.is_empty() {
        return None;
    }
    let types: Vec<i64> = pictures.iter().map(Picture::normalized_type).collect();
    types
        .iter()
        .position(|&kind| kind == FRONT_COVER)
        .or_else(|| types.iter().position(|&kind| kind <= OTHER))
        .or(Some(0))
}

/// `CoverArt.extractPictures`: each picture handed to the embedded document extractor, the
/// thumbnail as a thumbnail and the others inline, with its type, title and description.
pub(super) fn extract_pictures(pictures: &[Picture], xhtml: &mut Xhtml) -> Result<(), Failure> {
    let thumbnail = thumbnail_index(pictures);
    for (index, picture) in pictures.iter().enumerate() {
        let mut metadata = TikaMetadata::new();
        if Some(index) == thumbnail {
            ResourceType::Thumbnail.set(&mut metadata);
        } else {
            ResourceType::Inline.set(&mut metadata);
        }
        if let Some(mime_type) = picture.mime_type.as_deref().filter(|t| !t.is_empty()) {
            metadata.set("Content-Type", mime_type);
        }
        if let Some(description) = picture.description.as_deref().filter(|d| !d.is_empty()) {
            metadata.set("dc:title", description);
        }
        if let Some(name) = usize::try_from(picture.kind)
            .ok()
            .and_then(|kind| PICTURE_TYPES.get(kind))
        {
            metadata.set("dc:description", *name);
        }
        xhtml.parse_embedded(&picture.data, &metadata, Wrapping::PackageEntry)?;
    }
    Ok(())
}

/// A track or disc number and the total it counts to, `NumberAndTotal`: each a positive
/// integer where there is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NumberAndTotal {
    pub(super) number: Option<i32>,
    pub(super) total: Option<i32>,
}

impl NumberAndTotal {
    /// `NumberAndTotal.parse`: `n` or `n/total`; `None` where neither part is a positive
    /// integer.
    pub(super) fn parse(text: &str) -> Option<Self> {
        let positive = |part: &str| java::parse_int(java::trim(part)).filter(|&value| value > 0);
        let (number, total) = match text.split_once('/') {
            Some((number, total)) => (positive(number), positive(total)),
            None => (positive(text), None),
        };
        (number.is_some() || total.is_some()).then_some(Self { number, total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(kind: i64) -> Picture {
        Picture {
            kind,
            mime_type: None,
            description: None,
            data: Vec::new(),
        }
    }

    #[test]
    fn picks_the_thumbnail_as_cover_art_does() {
        assert_eq!(thumbnail_index(&[]), None);
        assert_eq!(thumbnail_index(&[picture(4), picture(3)]), Some(1));
        assert_eq!(
            thumbnail_index(&[picture(4), picture(99), picture(0)]),
            Some(1)
        );
        assert_eq!(thumbnail_index(&[picture(4), picture(5)]), Some(0));
    }

    #[test]
    fn reads_numbers_and_totals_as_tika_does() {
        assert_eq!(
            NumberAndTotal::parse(" 3/12"),
            Some(NumberAndTotal {
                number: Some(3),
                total: Some(12)
            })
        );
        assert_eq!(
            NumberAndTotal::parse("0/7"),
            Some(NumberAndTotal {
                number: None,
                total: Some(7)
            })
        );
        assert_eq!(NumberAndTotal::parse("x"), None);
    }
}
