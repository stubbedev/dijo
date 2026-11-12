//! Matroska and WebM, which Tika reads with no parser: the segment's information and its global
//! tags as metadata (title, date, writing application, duration, and artist, genre and the like),
//! the codecs, picture size, sampling rate and languages of its tracks, the cues of its text
//! subtitle tracks (`S_TEXT/UTF8`, `S_TEXT/ASS`, `S_TEXT/SSA`, `S_TEXT/WEBVTT` and WebM's
//! `D_WEBVTT` kinds), track by track, each cue a paragraph in the order of its time, read as
//! [`subtitle`](super::subtitle) reads the cues of a subtitle file; and its attachments, fonts and
//! cover images, handed to the embedded document extractor.
//!
//! The EBML ([`ebml`]) is read in place, a cluster at a time: of the blocks ([`block`]) only
//! those of text subtitle tracks are read, their pages released as the clusters are passed; the
//! cues read are held, under the extraction's memory budget, until their track is written.

mod block;
mod ebml;

use std::collections::{BTreeMap, BTreeSet};

use block::Encoding;
use ebml::{Children, Element, Id, Malformed, Unsized};

use crate::config::ParserConfigs;
use crate::formats::embedded::{self, ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::subtitle;
use crate::formats::xhtml::{within_budget, Xhtml};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging};
use crate::java;
use crate::java::dtoa::java_double_to_string;
use crate::java::time::format_instant;

/// The IDs of the elements read, as the Matroska specification (RFC 9559) names them.
mod ids {
    use super::Id;

    pub(super) const EBML: Id = 0x1A45_DFA3;
    pub(super) const DOC_TYPE: Id = 0x4282;
    pub(super) const SEGMENT: Id = 0x1853_8067;
    pub(super) const VOID: Id = 0xEC;
    pub(super) const CRC_32: Id = 0xBF;

    pub(super) const INFO: Id = 0x1549_A966;
    pub(super) const TIMESTAMP_SCALE: Id = 0x2A_D7B1;
    pub(super) const DURATION: Id = 0x4489;
    pub(super) const DATE_UTC: Id = 0x4461;
    pub(super) const TITLE: Id = 0x7BA9;
    pub(super) const WRITING_APP: Id = 0x5741;

    pub(super) const TRACKS: Id = 0x1654_AE6B;
    pub(super) const TRACK_ENTRY: Id = 0xAE;
    pub(super) const TRACK_NUMBER: Id = 0xD7;
    pub(super) const TRACK_TYPE: Id = 0x83;
    pub(super) const CODEC_ID: Id = 0x86;
    pub(super) const LANGUAGE: Id = 0x22_B59C;
    pub(super) const LANGUAGE_BCP47: Id = 0x22_B59D;
    pub(super) const VIDEO: Id = 0xE0;
    pub(super) const PIXEL_WIDTH: Id = 0xB0;
    pub(super) const PIXEL_HEIGHT: Id = 0xBA;
    pub(super) const AUDIO: Id = 0xE1;
    pub(super) const SAMPLING_FREQUENCY: Id = 0xB5;
    pub(super) const CHANNELS: Id = 0x9F;
    pub(super) const CONTENT_ENCODINGS: Id = 0x6D80;
    pub(super) const CONTENT_ENCODING: Id = 0x6240;
    pub(super) const CONTENT_ENCODING_ORDER: Id = 0x5031;
    pub(super) const CONTENT_ENCODING_SCOPE: Id = 0x5032;
    pub(super) const CONTENT_ENCODING_TYPE: Id = 0x5033;
    pub(super) const CONTENT_COMPRESSION: Id = 0x5034;
    pub(super) const CONTENT_COMP_ALGO: Id = 0x4254;
    pub(super) const CONTENT_COMP_SETTINGS: Id = 0x4255;

    pub(super) const CLUSTER: Id = 0x1F43_B675;
    pub(super) const CLUSTER_TIMESTAMP: Id = 0xE7;
    pub(super) const SILENT_TRACKS: Id = 0x5854;
    pub(super) const POSITION: Id = 0xA7;
    pub(super) const PREV_SIZE: Id = 0xAB;
    pub(super) const SIMPLE_BLOCK: Id = 0xA3;
    pub(super) const BLOCK_GROUP: Id = 0xA0;
    pub(super) const BLOCK: Id = 0xA1;
    pub(super) const ENCRYPTED_BLOCK: Id = 0xAF;

    pub(super) const ATTACHMENTS: Id = 0x1941_A469;
    pub(super) const ATTACHED_FILE: Id = 0x61A7;
    pub(super) const FILE_DESCRIPTION: Id = 0x467E;
    pub(super) const FILE_NAME: Id = 0x466E;
    pub(super) const FILE_MEDIA_TYPE: Id = 0x4660;
    pub(super) const FILE_DATA: Id = 0x465C;

    pub(super) const TAGS: Id = 0x1254_C367;
    pub(super) const TAG: Id = 0x7373;
    pub(super) const TARGETS: Id = 0x63C0;
    pub(super) const TARGET_TYPE_VALUE: Id = 0x68CA;
    pub(super) const TAG_TRACK_UID: Id = 0x63C5;
    pub(super) const TAG_EDITION_UID: Id = 0x63C9;
    pub(super) const TAG_CHAPTER_UID: Id = 0x63C4;
    pub(super) const TAG_ATTACHMENT_UID: Id = 0x63C6;
    pub(super) const SIMPLE_TAG: Id = 0x67C8;
    pub(super) const TAG_NAME: Id = 0x45A3;
    pub(super) const TAG_STRING: Id = 0x4487;
}

/// The children a cluster of unknown size may hold, before the first element that ends it.
const CLUSTER_CHILDREN: &[Id] = &[
    ids::CLUSTER_TIMESTAMP,
    ids::SILENT_TRACKS,
    ids::POSITION,
    ids::PREV_SIZE,
    ids::SIMPLE_BLOCK,
    ids::BLOCK_GROUP,
    ids::ENCRYPTED_BLOCK,
    ids::VOID,
    ids::CRC_32,
];

/// The document types of the EBML header the parser reads.
const DOC_TYPES: [&str; 2] = ["matroska", "webm"];
/// `TimestampScale`'s default: a tick of a millisecond, in nanoseconds.
const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;
/// 2001-01-01T00:00:00Z, from which `DateUTC` counts nanoseconds, in milliseconds since 1970.
const MATROSKA_EPOCH_MILLIS: i64 = 978_307_200_000;
/// `TrackType` values.
const VIDEO_TRACK: u64 = 1;
const AUDIO_TRACK: u64 = 2;
const SUBTITLE_TRACK: u64 = 17;
/// `Language`'s default.
const DEFAULT_LANGUAGE: &str = "eng";
/// The language code of no language in particular.
const UNDETERMINED_LANGUAGE: &str = "und";
/// The `TargetTypeValue` of a tag of the whole movie or album, its default.
const MOVIE_TARGET: u64 = 50;
/// `ContentEncodingScope`'s bit for every frame of the track; its default.
const SCOPE_FRAMES: u64 = 1;
/// The fields of a SubStation Alpha block before its text: `ReadOrder`, `Layer`, `Style`,
/// `Name`, `MarginL`, `MarginR`, `MarginV` and `Effect`.
const SUBSTATION_BLOCK_TEXT_FIELD: usize = 8;

/// The global tags read as metadata, by their names.
const TAG_KEYS: [(&str, &str); 9] = [
    ("ARTIST", "xmpDM:artist"),
    ("COMMENT", "xmpDM:logComment"),
    ("COMPOSER", "xmpDM:composer"),
    ("COPYRIGHT", "xmpDM:copyright"),
    ("DATE_RELEASED", "xmpDM:releaseDate"),
    ("DESCRIPTION", "dc:description"),
    ("GENRE", "xmpDM:genre"),
    ("KEYWORDS", "dc:subject"),
    ("SUMMARY", "dc:description"),
];
/// The global tag of the title, read where the segment's information has none.
const TITLE_TAG: &str = "TITLE";
/// The global tag of the encoder, read where the segment's information names no writing
/// application.
const ENCODER_TAG: &str = "ENCODER";

pub struct MatroskaParser;

impl Parser for MatroskaParser {
    fn name(&self) -> &'static str {
        "dijo::MatroskaParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::MatroskaParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/x-matroska",
            "audio/x-matroska",
            "video/webm",
            "video/x-matroska",
        ]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| write(input, pages, xhtml)))
    }
}

impl From<Malformed> for Failure {
    fn from(Malformed(reason): Malformed) -> Self {
        Failure::Malformed(reason)
    }
}

/// How the text of a subtitle track's blocks is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextCodec {
    /// SubRip's text, `S_TEXT/UTF8` or `S_TEXT/ASCII`.
    Plain,
    /// A SubStation Alpha dialogue without its timing, `S_TEXT/ASS` or `S_TEXT/SSA`.
    SubStation,
    /// A WebVTT cue's text.
    WebVtt,
}

impl TextCodec {
    /// The codec a `CodecID` names, if it is one of text.
    fn of(codec_id: &str) -> Option<Self> {
        match codec_id {
            "S_TEXT/UTF8" | "S_TEXT/ASCII" => Some(Self::Plain),
            "S_TEXT/ASS" | "S_TEXT/SSA" | "S_ASS" | "S_SSA" => Some(Self::SubStation),
            "S_TEXT/WEBVTT"
            | "D_WEBVTT/SUBTITLES"
            | "D_WEBVTT/CAPTIONS"
            | "D_WEBVTT/DESCRIPTIONS" => Some(Self::WebVtt),
            _ => None,
        }
    }

    /// The cue a decoded frame holds, if it holds any text.
    fn cue(self, frame: &[u8]) -> Option<String> {
        let text = java::utf8(frame);
        let cue = match self {
            Self::Plain | Self::WebVtt => subtitle::timed_cue(&text),
            Self::SubStation => subtitle::substation_cue(
                text.splitn(SUBSTATION_BLOCK_TEXT_FIELD + 1, ',')
                    .nth(SUBSTATION_BLOCK_TEXT_FIELD)?,
            ),
        };
        (!cue.is_empty()).then_some(cue)
    }
}

/// What a track is, with what the metadata takes of it.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Video {
        width: Option<u64>,
        height: Option<u64>,
    },
    Audio {
        sampling_frequency: Option<f64>,
        channels: Option<u64>,
    },
    /// A subtitle track of text, its frames encoded as `encodings` say, in the order they are
    /// undone, or a reason they cannot be.
    Text {
        codec: TextCodec,
        encodings: Result<Vec<Encoding>, &'static str>,
    },
    Other,
}

/// A `TrackEntry`.
#[derive(Debug, Clone, PartialEq)]
struct Track {
    number: u64,
    kind: Kind,
    codec_id: Option<String>,
    language: String,
}

impl Track {
    /// Its language, unless it is undetermined.
    fn known_language(&self) -> Option<&str> {
        (self.language != UNDETERMINED_LANGUAGE).then_some(self.language.as_str())
    }
}

/// The segment's `Info`.
#[derive(Debug, Default, Clone, PartialEq)]
struct Info {
    title: Option<String>,
    writing_app: Option<String>,
    created_millis: Option<i64>,
    duration_seconds: Option<f64>,
}

/// An `AttachedFile`, its data in place.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attachment<'a> {
    name: Option<String>,
    media_type: Option<String>,
    description: Option<String>,
    data: &'a [u8],
}

/// A `Segment` read but for its clusters.
#[derive(Debug)]
struct Segment<'a> {
    element: Element,
    info: Info,
    tracks: Vec<Track>,
    /// The global tags, by name, in their order.
    tags: Vec<(String, String)>,
    attachments: Vec<Attachment<'a>>,
}

/// A cue of a subtitle track, at its time in the segment's ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cue {
    time: i64,
    text: String,
}

/// Writes the document `input`, whose pages are `pages`: its metadata, the cues of each text
/// subtitle track of each segment and its attachments.
fn write<'a>(input: &'a [u8], pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let paging = Paging::new(pages);
    let segments = segments(input, &paging)?;
    let metadata = xhtml.metadata_mut();
    let mut languages = BTreeSet::new();
    for segment in &segments {
        set_metadata(segment, &mut languages, metadata);
    }
    for segment in &segments {
        for (track, cues) in cues(input, segment, &paging)? {
            write_track(track, &cues, xhtml)?;
        }
    }
    for attachment in segments.iter().flat_map(|segment| &segment.attachments) {
        embed(attachment, pages, xhtml)?;
    }
    Ok(())
}

/// Where a top-level element of unknown size ends: a segment at the end of the file.
fn top_level_unsized(id: Id) -> Option<Unsized> {
    (id == ids::SEGMENT).then_some(Unsized::ToParentEnd)
}

/// Where a child of a segment of unknown size ends: a cluster before the first element it
/// cannot hold.
fn segment_unsized(id: Id) -> Option<Unsized> {
    (id == ids::CLUSTER).then_some(Unsized::BeforeOther(CLUSTER_CHILDREN))
}

/// The segments of `input`, each after an EBML header of a Matroska or WebM document, read but
/// for their clusters.
fn segments<'a>(input: &'a [u8], paging: &Paging<'_>) -> Result<Vec<Segment<'a>>, Malformed> {
    let mut top = Children::new(input, 0, input.len(), top_level_unsized);
    let mut segments = Vec::new();
    let mut headed = false;
    while let Some(element) = top.next()? {
        match element.id {
            ids::EBML => {
                doc_type(input, &element)?;
                headed = true;
            }
            ids::SEGMENT if headed => {
                segments.push(segment(input, element, paging)?);
                continue;
            }
            ids::SEGMENT => {
                return Err(Malformed(format!(
                    "the Segment at {} comes before any EBML header",
                    element.start
                )))
            }
            _ => {}
        }
        paging.read(element.length());
    }
    if segments.is_empty() {
        return Err(Malformed("the document has no Segment".to_owned()));
    }
    Ok(segments)
}

/// Checks that the EBML header `header` names a Matroska or WebM document.
fn doc_type(input: &[u8], header: &Element) -> Result<(), Malformed> {
    let mut children = Children::of(input, header);
    while let Some(child) = children.next()? {
        if child.id == ids::DOC_TYPE {
            let doc_type = ebml::text(&child, input);
            if DOC_TYPES.contains(&doc_type.as_str()) {
                return Ok(());
            }
            return Err(Malformed(format!(
                "the EBML header at {} names the document type {doc_type:?}, neither Matroska \
                 nor WebM",
                header.start
            )));
        }
    }
    Err(Malformed(format!(
        "the EBML header at {} names no document type",
        header.start
    )))
}

/// The segment `element` read but for its clusters.
fn segment<'a>(
    input: &'a [u8],
    element: Element,
    paging: &Paging<'_>,
) -> Result<Segment<'a>, Malformed> {
    let mut segment = Segment {
        element,
        info: Info::default(),
        tracks: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
    };
    let mut children = Children::new(input, element.body_start, element.end, segment_unsized);
    while let Some(child) = children.next()? {
        match child.id {
            ids::INFO => segment.info = info(input, &child)?,
            ids::TRACKS => segment.tracks.extend(tracks(input, &child)?),
            ids::TAGS => segment.tags.extend(global_tags(input, &child)?),
            ids::ATTACHMENTS => segment.attachments.extend(attachments(input, &child)?),
            _ => {}
        }
        paging.read(child.length());
    }
    Ok(segment)
}

/// The segment's `Info`.
fn info(input: &[u8], element: &Element) -> Result<Info, Malformed> {
    let mut info = Info::default();
    let mut scale = DEFAULT_TIMESTAMP_SCALE;
    let mut duration = None;
    let mut children = Children::of(input, element);
    while let Some(child) = children.next()? {
        match child.id {
            ids::TITLE => info.title = Some(ebml::text(&child, input)),
            ids::WRITING_APP => info.writing_app = Some(ebml::text(&child, input)),
            ids::DATE_UTC => {
                let nanos = ebml::signed(&child, input)?;
                info.created_millis = Some(MATROSKA_EPOCH_MILLIS + nanos.div_euclid(1_000_000));
            }
            ids::TIMESTAMP_SCALE => scale = ebml::unsigned(&child, input)?,
            ids::DURATION => duration = Some(ebml::float(&child, input)?),
            _ => {}
        }
    }
    info.duration_seconds = duration
        .map(|ticks| ticks * scale as f64 / 1e9)
        .filter(|seconds| seconds.is_finite());
    Ok(info)
}

/// The `TrackEntry` elements of `Tracks`.
fn tracks(input: &[u8], element: &Element) -> Result<Vec<Track>, Malformed> {
    let mut tracks = Vec::new();
    let mut children = Children::of(input, element);
    while let Some(child) = children.next()? {
        if child.id == ids::TRACK_ENTRY {
            tracks.push(track(input, &child)?);
        }
    }
    Ok(tracks)
}

/// A `TrackEntry`.
fn track(input: &[u8], element: &Element) -> Result<Track, Malformed> {
    let mut number = None;
    let mut track_type = None;
    let mut codec_id = None;
    let mut language = None;
    let mut language_bcp47 = None;
    let mut video = None;
    let mut audio = None;
    let mut encodings = Ok(Vec::new());
    let mut children = Children::of(input, element);
    while let Some(child) = children.next()? {
        match child.id {
            ids::TRACK_NUMBER => number = Some(ebml::unsigned(&child, input)?),
            ids::TRACK_TYPE => track_type = Some(ebml::unsigned(&child, input)?),
            ids::CODEC_ID => codec_id = Some(ebml::text(&child, input)),
            ids::LANGUAGE => language = Some(ebml::text(&child, input)),
            ids::LANGUAGE_BCP47 => language_bcp47 = Some(ebml::text(&child, input)),
            ids::VIDEO => video = Some(child),
            ids::AUDIO => audio = Some(child),
            ids::CONTENT_ENCODINGS => encodings = content_encodings(input, &child)?,
            _ => {}
        }
    }
    let number = number.ok_or_else(|| {
        Malformed(format!(
            "the TrackEntry at {} has no TrackNumber",
            element.start
        ))
    })?;
    let kind = match track_type {
        Some(VIDEO_TRACK) => video_kind(input, video)?,
        Some(AUDIO_TRACK) => audio_kind(input, audio)?,
        Some(SUBTITLE_TRACK) => match codec_id.as_deref().and_then(TextCodec::of) {
            Some(codec) => Kind::Text { codec, encodings },
            None => Kind::Other,
        },
        _ => Kind::Other,
    };
    Ok(Track {
        number,
        kind,
        codec_id,
        language: language_bcp47
            .or(language)
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_owned()),
    })
}

/// A video track's picture size, from its `Video` element, if it has one.
fn video_kind(input: &[u8], video: Option<Element>) -> Result<Kind, Malformed> {
    let (mut width, mut height) = (None, None);
    if let Some(video) = video {
        let mut children = Children::of(input, &video);
        while let Some(child) = children.next()? {
            match child.id {
                ids::PIXEL_WIDTH => width = Some(ebml::unsigned(&child, input)?),
                ids::PIXEL_HEIGHT => height = Some(ebml::unsigned(&child, input)?),
                _ => {}
            }
        }
    }
    Ok(Kind::Video { width, height })
}

/// An audio track's sampling rate and channels, from its `Audio` element, if it has one.
fn audio_kind(input: &[u8], audio: Option<Element>) -> Result<Kind, Malformed> {
    let (mut sampling_frequency, mut channels) = (None, None);
    if let Some(audio) = audio {
        let mut children = Children::of(input, &audio);
        while let Some(child) = children.next()? {
            match child.id {
                ids::SAMPLING_FREQUENCY => sampling_frequency = Some(ebml::float(&child, input)?),
                ids::CHANNELS => channels = Some(ebml::unsigned(&child, input)?),
                _ => {}
            }
        }
    }
    Ok(Kind::Audio {
        sampling_frequency,
        channels,
    })
}

/// The encodings of a track's frames, from its `ContentEncodings`, in the order they are undone:
/// from the highest `ContentEncodingOrder` down. Those only of the track's private data are left
/// out; an encryption, or a compression other than zlib and header stripping, is why the frames
/// cannot be decoded.
fn content_encodings(
    input: &[u8],
    element: &Element,
) -> Result<Result<Vec<Encoding>, &'static str>, Malformed> {
    let mut ordered = Vec::new();
    let mut children = Children::of(input, element);
    while let Some(child) = children.next()? {
        if child.id != ids::CONTENT_ENCODING {
            continue;
        }
        let mut order = 0;
        let mut scope = SCOPE_FRAMES;
        let mut encoding_type = 0;
        let mut compression = None;
        let mut fields = Children::of(input, &child);
        while let Some(field) = fields.next()? {
            match field.id {
                ids::CONTENT_ENCODING_ORDER => order = ebml::unsigned(&field, input)?,
                ids::CONTENT_ENCODING_SCOPE => scope = ebml::unsigned(&field, input)?,
                ids::CONTENT_ENCODING_TYPE => encoding_type = ebml::unsigned(&field, input)?,
                ids::CONTENT_COMPRESSION => compression = Some(field),
                _ => {}
            }
        }
        if scope & SCOPE_FRAMES == 0 {
            continue;
        }
        if encoding_type != 0 {
            return Ok(Err("encrypted"));
        }
        let encoding = match compression {
            Some(compression) => match content_compression(input, &compression)? {
                Ok(encoding) => encoding,
                Err(what) => return Ok(Err(what)),
            },
            None => Encoding::Zlib,
        };
        ordered.push((order, encoding));
    }
    ordered.sort_by_key(|&(order, _)| std::cmp::Reverse(order));
    Ok(Ok(ordered
        .into_iter()
        .map(|(_, encoding)| encoding)
        .collect()))
}

/// A `ContentCompression`: zlib, its default, or header stripping, else what other compression
/// it is.
fn content_compression(
    input: &[u8],
    element: &Element,
) -> Result<Result<Encoding, &'static str>, Malformed> {
    let mut algorithm = 0;
    let mut settings = Vec::new();
    let mut children = Children::of(input, element);
    while let Some(child) = children.next()? {
        match child.id {
            ids::CONTENT_COMP_ALGO => algorithm = ebml::unsigned(&child, input)?,
            ids::CONTENT_COMP_SETTINGS => settings = child.body(input).to_vec(),
            _ => {}
        }
    }
    Ok(match algorithm {
        0 => Ok(Encoding::Zlib),
        1 => Err("compressed with bzlib"),
        2 => Err("compressed with LZO"),
        3 => Ok(Encoding::HeaderStripping(settings)),
        _ => Err("compressed with an algorithm Matroska does not define"),
    })
}

/// The tags of `Tags` that apply to the whole segment: those of the movie, which target no
/// track, edition, chapter or attachment, each `SimpleTag` of them by its name, its nested
/// ones left out.
fn global_tags(input: &[u8], element: &Element) -> Result<Vec<(String, String)>, Malformed> {
    let mut tags = Vec::new();
    let mut children = Children::of(input, element);
    while let Some(tag) = children.next()? {
        if tag.id != ids::TAG {
            continue;
        }
        let mut global = true;
        let mut simple = Vec::new();
        let mut fields = Children::of(input, &tag);
        while let Some(field) = fields.next()? {
            match field.id {
                ids::TARGETS => global = targets_movie(input, &field)?,
                ids::SIMPLE_TAG => simple.extend(simple_tag(input, &field)?),
                _ => {}
            }
        }
        if global {
            tags.extend(simple);
        }
    }
    Ok(tags)
}

/// Whether `Targets` targets the whole movie.
fn targets_movie(input: &[u8], element: &Element) -> Result<bool, Malformed> {
    let mut children = Children::of(input, element);
    let mut target_type = MOVIE_TARGET;
    while let Some(child) = children.next()? {
        match child.id {
            ids::TARGET_TYPE_VALUE => target_type = ebml::unsigned(&child, input)?,
            ids::TAG_TRACK_UID
            | ids::TAG_EDITION_UID
            | ids::TAG_CHAPTER_UID
            | ids::TAG_ATTACHMENT_UID
                if ebml::unsigned(&child, input)? != 0 =>
            {
                return Ok(false);
            }
            _ => {}
        }
    }
    Ok(target_type == MOVIE_TARGET)
}

/// A `SimpleTag`'s name and string value, if it has both.
fn simple_tag(input: &[u8], element: &Element) -> Result<Option<(String, String)>, Malformed> {
    let (mut name, mut value) = (None, None);
    let mut children = Children::of(input, element);
    while let Some(child) = children.next()? {
        match child.id {
            ids::TAG_NAME => name = Some(ebml::text(&child, input)),
            ids::TAG_STRING => value = Some(ebml::text(&child, input)),
            _ => {}
        }
    }
    Ok(name.zip(value))
}

/// The `AttachedFile` elements of `Attachments`.
fn attachments<'a>(input: &'a [u8], element: &Element) -> Result<Vec<Attachment<'a>>, Malformed> {
    let mut attachments = Vec::new();
    let mut children = Children::of(input, element);
    while let Some(file) = children.next()? {
        if file.id != ids::ATTACHED_FILE {
            continue;
        }
        let mut attachment = Attachment {
            name: None,
            media_type: None,
            description: None,
            data: &[],
        };
        let mut data = None;
        let mut fields = Children::of(input, &file);
        while let Some(field) = fields.next()? {
            match field.id {
                ids::FILE_NAME => attachment.name = Some(ebml::text(&field, input)),
                ids::FILE_MEDIA_TYPE => attachment.media_type = Some(ebml::text(&field, input)),
                ids::FILE_DESCRIPTION => attachment.description = Some(ebml::text(&field, input)),
                ids::FILE_DATA => data = Some(field.body(input)),
                _ => {}
            }
        }
        attachment.data = data.ok_or_else(|| {
            Malformed(format!(
                "the AttachedFile at {} has no FileData",
                file.start
            ))
        })?;
        attachments.push(attachment);
    }
    Ok(attachments)
}

/// Sets the metadata `segment` gives, and adds the languages of its tracks not among
/// `languages`, the languages added so far.
fn set_metadata(
    segment: &Segment<'_>,
    languages: &mut BTreeSet<String>,
    metadata: &mut TikaMetadata,
) {
    let tag = |name: &str| {
        segment
            .tags
            .iter()
            .find(|(tag, _)| tag == name)
            .map(|(_, value)| value.as_str())
    };
    let info = &segment.info;
    if let Some(title) = info.title.as_deref().or_else(|| tag(TITLE_TAG)) {
        metadata.set("dc:title", title);
    }
    if let Some(created) = info.created_millis {
        metadata.set("dcterms:created", format_instant(created));
    }
    if let Some(tool) = info.writing_app.as_deref().or_else(|| tag(ENCODER_TAG)) {
        metadata.set("xmp:CreatorTool", tool);
    }
    if let Some(seconds) = info.duration_seconds {
        metadata.set("xmpDM:duration", java_double_to_string(seconds));
    }
    for (name, value) in &segment.tags {
        if let Some((_, key)) = TAG_KEYS.iter().find(|(tag, _)| tag == name) {
            metadata.add(key, value.as_str());
        }
    }
    let first = |video: bool| {
        segment.tracks.iter().find(|track| match track.kind {
            Kind::Video { .. } => video,
            Kind::Audio { .. } => !video,
            Kind::Text { .. } | Kind::Other => false,
        })
    };
    if let Some(track) = first(true) {
        if let Some(codec) = &track.codec_id {
            metadata.set("xmpDM:videoCompressor", codec.as_str());
        }
        if let Kind::Video { width, height } = track.kind {
            if let Some(width) = width {
                metadata.set("tiff:ImageWidth", width.to_string());
            }
            if let Some(height) = height {
                metadata.set("tiff:ImageLength", height.to_string());
            }
        }
    }
    if let Some(track) = first(false) {
        if let Some(codec) = &track.codec_id {
            metadata.set("xmpDM:audioCompressor", codec.as_str());
        }
        if let Kind::Audio {
            sampling_frequency,
            channels,
        } = track.kind
        {
            if let Some(rate) = sampling_frequency {
                metadata.set("xmpDM:audioSampleRate", (rate as i64).to_string());
            }
            if let Some(channels) = channels {
                metadata.set("audio:channels", channels.to_string());
            }
        }
    }
    for track in &segment.tracks {
        if let Some(language) = track.known_language() {
            if languages.insert(language.to_owned()) {
                metadata.add("dc:language", language);
            }
        }
        if let Kind::Text {
            encodings: Err(what),
            ..
        } = &track.kind
        {
            metadata.add(
                embedded::EXCEPTION_WARNING,
                format!(
                    "the subtitles of track {} are not read: their frames are {what}, which dijo \
                     does not decode",
                    track.number
                ),
            );
        }
    }
}

/// A text subtitle track whose cues are read, and those read so far.
struct Reading<'s> {
    track: &'s Track,
    codec: TextCodec,
    encodings: &'s [Encoding],
    cues: Vec<Cue>,
}

/// The cues of each text subtitle track of `segment` whose frames are decoded, in the order of
/// the tracks, each track's in the order of their times, cues of the same time in the order of
/// the file. Only the blocks of those tracks are read, the cues held under the memory budget.
fn cues<'s>(
    input: &[u8],
    segment: &'s Segment<'_>,
    paging: &Paging<'_>,
) -> Result<Vec<(&'s Track, Vec<Cue>)>, Failure> {
    let mut read = BTreeMap::new();
    for track in &segment.tracks {
        if let Kind::Text {
            codec,
            encodings: Ok(encodings),
        } = &track.kind
        {
            read.entry(track.number).or_insert(Reading {
                track,
                codec: *codec,
                encodings,
                cues: Vec::new(),
            });
        }
    }
    if read.is_empty() {
        return Ok(Vec::new());
    }
    let element = &segment.element;
    let mut children = Children::new(input, element.body_start, element.end, segment_unsized);
    while let Some(cluster) = children.next()? {
        if cluster.id == ids::CLUSTER {
            cluster_cues(input, &cluster, &mut read)?;
        }
        paging.read(cluster.length());
    }
    let mut tracks = Vec::with_capacity(read.len());
    for track in &segment.tracks {
        if let Some(mut reading) = read.remove(&track.number) {
            reading.cues.sort_by_key(|cue| cue.time);
            tracks.push((reading.track, reading.cues));
        }
    }
    Ok(tracks)
}

/// Adds to the tracks `read` the cues of the blocks of `cluster` that belong to them.
fn cluster_cues(
    input: &[u8],
    cluster: &Element,
    read: &mut BTreeMap<u64, Reading<'_>>,
) -> Result<(), Failure> {
    let mut time = None;
    let mut children = Children::of(input, cluster);
    while let Some(child) = children.next()? {
        let block = match child.id {
            ids::CLUSTER_TIMESTAMP => {
                time = Some(ebml::unsigned(&child, input)?);
                continue;
            }
            ids::SIMPLE_BLOCK => child,
            ids::BLOCK_GROUP => match group_block(input, &child)? {
                Some(block) => block,
                None => continue,
            },
            _ => continue,
        };
        let bytes = block.body(input);
        let (header, flags) = block::header(bytes)?;
        let Some(reading) = read.get_mut(&header.track) else {
            continue;
        };
        let cluster_time = time.ok_or_else(|| {
            Malformed(format!(
                "the block at {} comes before its Cluster's Timestamp",
                block.start
            ))
        })?;
        let time = i64::try_from(cluster_time)
            .ok()
            .and_then(|time| time.checked_add(i64::from(header.relative_time)))
            .ok_or_else(|| {
                Malformed(format!(
                    "the block at {} has a time past what a timestamp holds",
                    block.start
                ))
            })?;
        for frame in block::frames(bytes, flags)? {
            let decoded =
                block::decode(frame, reading.encodings).map_err(|Malformed(reason)| {
                    Malformed(format!(
                        "a frame of track {} in the block at {}: {reason}",
                        reading.track.number, block.start
                    ))
                })?;
            if let Some(text) = reading.codec.cue(&decoded) {
                reading.cues.push(Cue { time, text });
                within_budget()?;
            }
        }
    }
    Ok(())
}

/// The `Block` of a `BlockGroup`, if it has one.
fn group_block(input: &[u8], group: &Element) -> Result<Option<Element>, Malformed> {
    let mut fields = Children::of(input, group);
    while let Some(field) = fields.next()? {
        if field.id == ids::BLOCK {
            return Ok(Some(field));
        }
    }
    Ok(None)
}

/// Writes the cues of a subtitle track, each a paragraph, in a division of the track's language.
fn write_track(track: &Track, cues: &[Cue], xhtml: &mut Xhtml) -> Result<(), Failure> {
    if cues.is_empty() {
        return Ok(());
    }
    let mut attributes = vec![("class", "subtitles")];
    if let Some(language) = track.known_language() {
        attributes.push(("lang", language));
    }
    xhtml.start_element_with("div", &attributes)?;
    for cue in cues {
        xhtml.element("p", &cue.text)?;
    }
    xhtml.end_element("div")?;
    Ok(())
}

/// Hands `attachment` to the embedded document extractor, with its name, media type and
/// description.
fn embed(attachment: &Attachment<'_>, pages: Pages<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut metadata = TikaMetadata::new();
    ResourceType::Attachment.set(&mut metadata);
    if let Some(name) = attachment.name.as_deref().filter(|name| !name.is_empty()) {
        metadata.set(RESOURCE_NAME, name);
    }
    if let Some(media_type) = attachment
        .media_type
        .as_deref()
        .filter(|kind| !kind.is_empty())
    {
        metadata.set("Content-Type", media_type);
    }
    if let Some(description) = attachment
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
    {
        metadata.set("dc:description", description);
    }
    xhtml.parse_embedded_input(
        &Input::Within(attachment.data, pages),
        &metadata,
        Wrapping::PackageEntry,
    )
}

#[cfg(test)]
mod tests {
    use super::ebml::tests::{element, unsigned_element, unsized_element};
    use super::*;
    use crate::compress::zlib::tests::zlib;

    fn header(doc_type: &[u8]) -> Vec<u8> {
        element(ids::EBML, &element(ids::DOC_TYPE, doc_type))
    }

    fn joined(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn text_element(id: Id, text: &str) -> Vec<u8> {
        element(id, text.as_bytes())
    }

    fn track(number: u64, track_type: u64, codec: &str, more: &[Vec<u8>]) -> Vec<u8> {
        let mut fields = vec![
            unsigned_element(ids::TRACK_NUMBER, number),
            unsigned_element(ids::TRACK_TYPE, track_type),
            text_element(ids::CODEC_ID, codec),
        ];
        fields.extend_from_slice(more);
        element(ids::TRACK_ENTRY, &joined(&fields))
    }

    fn encoding(encoding_type: u64, compression: &[Vec<u8>]) -> Vec<u8> {
        element(
            ids::CONTENT_ENCODINGS,
            &element(
                ids::CONTENT_ENCODING,
                &joined(&[
                    unsigned_element(ids::CONTENT_ENCODING_TYPE, encoding_type),
                    element(ids::CONTENT_COMPRESSION, &joined(compression)),
                ]),
            ),
        )
    }

    /// A block of `track` at `time` relative to its cluster, with `flags`, then `frames`.
    fn block(track: u8, time: i16, flags: u8, frames: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x80 | track];
        bytes.extend_from_slice(&time.to_be_bytes());
        bytes.push(flags);
        bytes.extend_from_slice(frames);
        bytes
    }

    fn simple_block(track: u8, time: i16, frame: &[u8]) -> Vec<u8> {
        element(ids::SIMPLE_BLOCK, &block(track, time, 0, frame))
    }

    fn language(code: &str) -> Vec<u8> {
        text_element(ids::LANGUAGE, code)
    }

    /// A Matroska file of a video track and four subtitle tracks: SubRip compressed with zlib,
    /// SubStation Alpha, WebVTT with a stripped header and encrypted SubRip; their cues across
    /// two clusters, the first of unknown size, out of the order of their times; global and
    /// track tags; and an attachment.
    fn movie() -> Vec<u8> {
        let info = element(
            ids::INFO,
            &joined(&[
                unsigned_element(ids::TIMESTAMP_SCALE, 1_000_000),
                element(ids::DURATION, &8000f64.to_be_bytes()),
                element(ids::DATE_UTC, &86_400_000_000_000i64.to_be_bytes()),
                text_element(ids::TITLE, "Ledger"),
                text_element(ids::WRITING_APP, "dijo test"),
            ]),
        );
        let tracks = element(
            ids::TRACKS,
            &joined(&[
                track(
                    1,
                    VIDEO_TRACK,
                    "V_VP9",
                    &[
                        language("und"),
                        element(
                            ids::VIDEO,
                            &joined(&[
                                unsigned_element(ids::PIXEL_WIDTH, 32),
                                unsigned_element(ids::PIXEL_HEIGHT, 16),
                            ]),
                        ),
                    ],
                ),
                track(
                    2,
                    SUBTITLE_TRACK,
                    "S_TEXT/UTF8",
                    &[
                        language("eng"),
                        encoding(0, &[unsigned_element(ids::CONTENT_COMP_ALGO, 0)]),
                    ],
                ),
                track(3, SUBTITLE_TRACK, "S_TEXT/ASS", &[language("dan")]),
                track(
                    4,
                    SUBTITLE_TRACK,
                    "S_TEXT/WEBVTT",
                    &[encoding(
                        0,
                        &[
                            unsigned_element(ids::CONTENT_COMP_ALGO, 3),
                            text_element(ids::CONTENT_COMP_SETTINGS, "<v Keeper>the "),
                        ],
                    )],
                ),
                track(5, SUBTITLE_TRACK, "S_TEXT/UTF8", &[encoding(1, &[])]),
            ]),
        );
        let first_cluster = unsized_element(
            ids::CLUSTER,
            &joined(&[
                unsigned_element(ids::CLUSTER_TIMESTAMP, 1000),
                simple_block(1, 0, &[0; 64]),
                simple_block(2, 4000, &zlib(b"<i>the keeper climbed</i>")),
                element(
                    ids::BLOCK_GROUP,
                    &joined(&[
                        element(ids::BLOCK, &block(2, 0, 0, &zlib(b"Every evening"))),
                        unsigned_element(0x9B, 1000),
                    ]),
                ),
                simple_block(3, 0, b"0,0,Default,,0,0,0,,{\\i1}One, {\\i0}\\Ntwo"),
                simple_block(4, 0, b"keeper\r\nat dusk"),
                simple_block(5, 0, b"secret"),
            ]),
        );
        let mut laced = vec![zlib(b"first").len() as u8];
        laced.extend(zlib(b"first"));
        laced.extend(zlib(b"second"));
        let second_cluster = element(
            ids::CLUSTER,
            &joined(&[
                unsigned_element(ids::CLUSTER_TIMESTAMP, 3000),
                element(
                    ids::SIMPLE_BLOCK,
                    &block(2, 0, 0b0010, &[&[1], &laced[..]].concat()),
                ),
                simple_block(1, 0, &[0; 64]),
            ]),
        );
        let tags = element(
            ids::TAGS,
            &joined(&[
                element(
                    ids::TAG,
                    &joined(&[
                        element(ids::TARGETS, b""),
                        element(
                            ids::SIMPLE_TAG,
                            &joined(&[
                                text_element(ids::TAG_NAME, "ARTIST"),
                                text_element(ids::TAG_STRING, "Ada"),
                            ]),
                        ),
                    ]),
                ),
                element(
                    ids::TAG,
                    &joined(&[
                        element(ids::TARGETS, &unsigned_element(ids::TAG_TRACK_UID, 7)),
                        element(
                            ids::SIMPLE_TAG,
                            &joined(&[
                                text_element(ids::TAG_NAME, "ARTIST"),
                                text_element(ids::TAG_STRING, "Not the movie's"),
                            ]),
                        ),
                    ]),
                ),
            ]),
        );
        let attachments = element(
            ids::ATTACHMENTS,
            &element(
                ids::ATTACHED_FILE,
                &joined(&[
                    text_element(ids::FILE_NAME, "notes.txt"),
                    text_element(ids::FILE_MEDIA_TYPE, "text/plain"),
                    text_element(ids::FILE_DATA, "Attached notes of the keeper"),
                ]),
            ),
        );
        joined(&[
            header(b"matroska"),
            unsized_element(
                ids::SEGMENT,
                &joined(&[
                    info,
                    tracks,
                    attachments,
                    first_cluster,
                    second_cluster,
                    tags,
                ]),
            ),
        ])
    }

    #[test]
    fn reads_cues_track_by_track_in_the_order_of_their_times() {
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&movie())
            .unwrap();
        let paragraphs: Vec<&str> = text
            .split('\n')
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(
            paragraphs,
            [
                "Every evening",
                "first",
                "second",
                "the keeper climbed",
                "One,",
                "two",
                "the keeper",
                "at dusk",
                "notes.txt",
                "Attached notes of the keeper",
            ],
            "{text}"
        );
        assert!(!text.contains("secret"), "{text}");
        assert_eq!(
            metadata["tk:parsed-by"].last().unwrap(),
            "dijo::MatroskaParser"
        );
        assert_eq!(metadata["Content-Type"], ["application/x-matroska"]);
        assert_eq!(metadata["dc:title"], ["Ledger"]);
        assert_eq!(metadata["dcterms:created"], ["2001-01-02T00:00:00Z"]);
        assert_eq!(metadata["xmp:CreatorTool"], ["dijo test"]);
        assert_eq!(metadata["xmpDM:duration"], ["8.0"]);
        assert_eq!(metadata["xmpDM:artist"], ["Ada"]);
        assert_eq!(metadata["xmpDM:videoCompressor"], ["V_VP9"]);
        assert_eq!(metadata["tiff:ImageWidth"], ["32"]);
        assert_eq!(metadata["tiff:ImageLength"], ["16"]);
        assert_eq!(metadata["dc:language"], ["eng", "dan"]);
        assert!(
            metadata[embedded::EXCEPTION_WARNING]
                .iter()
                .any(|warning| warning.contains("track 5") && warning.contains("encrypted")),
            "{metadata:?}"
        );
    }

    #[test]
    fn reads_webm_and_takes_the_title_from_the_tags_without_one() {
        let bytes = joined(&[
            header(b"webm"),
            element(
                ids::SEGMENT,
                &joined(&[
                    element(
                        ids::TRACKS,
                        &track(
                            1,
                            SUBTITLE_TRACK,
                            "D_WEBVTT/SUBTITLES",
                            &[text_element(ids::LANGUAGE_BCP47, "fr-CA")],
                        ),
                    ),
                    element(
                        ids::CLUSTER,
                        &joined(&[
                            unsigned_element(ids::CLUSTER_TIMESTAMP, 0),
                            simple_block(1, 0, b"Bonsoir"),
                        ]),
                    ),
                    element(
                        ids::TAGS,
                        &element(
                            ids::TAG,
                            &element(
                                ids::SIMPLE_TAG,
                                &joined(&[
                                    text_element(ids::TAG_NAME, "TITLE"),
                                    text_element(ids::TAG_STRING, "Le phare"),
                                ]),
                            ),
                        ),
                    ),
                ]),
            ),
        ]);
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(&bytes)
            .unwrap();
        assert!(
            xml.contains(r#"<div class="subtitles" lang="fr-CA">"#)
                && xml.contains("<p>Bonsoir</p>"),
            "{xml}"
        );
        assert_eq!(metadata["Content-Type"], ["video/webm"]);
        assert_eq!(metadata["dc:title"], ["Le phare"]);
        assert_eq!(metadata["dc:language"], ["fr-CA"]);
    }

    #[test]
    fn fails_on_malformed_ebml() {
        let truncated = movie();
        let read = |bytes: &[u8]| segments(bytes, &Paging::new(Pages::IN_MEMORY)).map(|_| ());
        assert!(read(&truncated[..truncated.len() / 2]).is_err());
        assert!(read(&header(b"other"))
            .unwrap_err()
            .0
            .contains("neither Matroska nor WebM"));
        assert!(read(&header(b"webm")).unwrap_err().0.contains("no Segment"));
        let early = joined(&[
            header(b"webm"),
            element(
                ids::SEGMENT,
                &joined(&[
                    element(ids::TRACKS, &track(1, SUBTITLE_TRACK, "S_TEXT/UTF8", &[])),
                    element(ids::CLUSTER, &simple_block(1, 0, b"x")),
                ]),
            ),
        ]);
        let paging = Paging::new(Pages::IN_MEMORY);
        let segment = &segments(&early, &paging).unwrap()[0];
        let Err(Failure::Malformed(reason)) = cues(&early, segment, &paging) else {
            panic!("a block before its cluster's timestamp is read");
        };
        assert!(
            reason.contains("before its Cluster's Timestamp"),
            "{reason}"
        );
    }

    /// A file too large to read whole is read a cluster at a time, in bounded memory, only the
    /// blocks of its subtitle track read.
    #[test]
    fn reads_files_too_large_to_read_whole() {
        let frame = vec![0u8; 1 << 20];
        let head = joined(&[header(b"matroska"), {
            let mut segment = unsized_element(ids::SEGMENT, b"");
            segment.extend(element(
                ids::TRACKS,
                &joined(&[
                    track(1, VIDEO_TRACK, "V_VP9", &[]),
                    track(2, SUBTITLE_TRACK, "S_TEXT/UTF8", &[]),
                ]),
            ));
            segment
        }]);
        let cluster = |index: u64| {
            unsized_element(
                ids::CLUSTER,
                &joined(&[
                    unsigned_element(ids::CLUSTER_TIMESTAMP, index * 1000),
                    simple_block(1, 0, &frame),
                    simple_block(2, 0, format!("Cue {index}").as_bytes()),
                ]),
            )
        };
        let count = dijo_core::large::bodies_past_limit(cluster(0).len());
        let file = dijo_core::large::LargeFile::new("large.mkv", &head, count, cluster, b"");
        file.extract_holding(
            MatroskaParser.name(),
            &["Cue 0", &format!("Cue {}", count - 1)],
        );
    }
}
