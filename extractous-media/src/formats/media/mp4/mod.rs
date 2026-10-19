//! MP4, QuickTime and 3GPP files, as Tika's `MP4Parser` reads them through metadata-extractor:
//! the boxes walked ([`walk`]) into the directories metadata-extractor keeps ([`directory`]),
//! with what Tika's own handlers write on the way, the iTunes metadata of a `udta` box
//! ([`user_data`]), the QuickTime metadata of `keys` and `ilst` boxes ([`quick_time`]) and the
//! formats and bit rates of sample entries ([`sample_entries`]), then the directories written as
//! metadata. Every exception metadata-extractor throws is recorded, as Tika records it.

mod directory;
mod location;
mod quick_time;
mod reader;
mod sample_entries;
mod user_data;
mod walk;

use reader::Thrown;

use crate::config::ParserConfigs;
use crate::formats::embedded::{self, ResourceType, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Failure, Footprint, Opened, Parser, Prepared};

/// `MP4`: the prefix of the QuickTime metadata keys, which the file names.
const QUICK_TIME_KEY_PREFIX: &str = "mp4:";

/// What Tika does to its output while reading the boxes, and once it has read them, in order.
#[derive(Debug)]
enum Event<'a> {
    Set(&'static str, String),
    Add(&'static str, String),
    /// `Metadata.add(MP4, key, value)` of a QuickTime metadata key.
    AddPrefixed(String, String),
    Paragraph(String),
    Cover(Cover<'a>),
    /// `EmbeddedDocumentUtil.recordException`.
    Recorded(Thrown),
}

/// A cover art picture handed to the embedded document extractor.
#[derive(Debug)]
struct Cover<'a> {
    picture: &'a [u8],
    resource_type: ResourceType,
    content_type: Option<&'static str>,
}

pub struct Mp4Parser;

impl Parser for Mp4Parser {
    fn name(&self) -> &'static str {
        "extractous::MP4Parser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mp4.MP4Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "audio/mp4",
            "video/3gpp",
            "video/3gpp2",
            "video/mp4",
            "video/x-m4v",
            "video/quicktime",
            "application/mp4",
            "video/iso.segment",
            "image/x-canon-cr3",
        ]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let walked = walk::walk(input);
        let mut events = walked.events;
        directory::metadata(&walked.movie, &walked.tracks, &mut events);
        unless_different(
            Box::new(move |xhtml| write(&events, xhtml)),
            walked.difference,
        )
    }
}

/// `MP4Parser.parse` replayed: what was written while the boxes were read, then the
/// directories' metadata.
fn write(events: &[Event<'_>], xhtml: &mut Xhtml) -> Result<(), Failure> {
    for event in events {
        match event {
            Event::Set(name, value) => xhtml.metadata_mut().set(name, value.as_str()),
            Event::Add(name, value) => xhtml.metadata_mut().add(name, value.as_str()),
            Event::AddPrefixed(name, value) => {
                xhtml
                    .metadata_mut()
                    .add_prefixed(QUICK_TIME_KEY_PREFIX, name, value.as_str());
            }
            Event::Paragraph(text) => xhtml.element("p", text)?,
            Event::Cover(cover) => {
                let mut metadata = TikaMetadata::new();
                cover.resource_type.set(&mut metadata);
                if let Some(content_type) = cover.content_type {
                    metadata.set("Content-Type", content_type);
                }
                xhtml.parse_embedded_opened(
                    cover.picture,
                    &metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?;
            }
            Event::Recorded(thrown) => {
                embedded::record_exception(xhtml.metadata_mut(), &thrown.exception());
            }
        }
    }
    Ok(())
}
