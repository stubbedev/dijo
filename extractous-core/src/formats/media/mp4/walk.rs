//! `TikaMp4Reader`: the boxes of a file walked as metadata-extractor's `Mp4Reader` walks them,
//! under Tika's limits, by `TikaMp4BoxHandler` and the media handler a track's handler box
//! hands the rest of its media box to. An `IOException` ends the boxes it was thrown among,
//! recorded, and the walk goes on after the box that held them, from wherever it stopped.

use super::directory::{Directory, MetaTrack, Movie, Rational, Sound, Track, Video};
use super::reader::{Bytes, Read, Stream, Thrown};
use super::user_data::{self, Covers};
use super::{quick_time, sample_entries, Event};
use crate::formats::Deferral;
use crate::java;

/// `MAX_BOX_DEPTH`.
const MAX_BOX_DEPTH: u32 = 100;
/// `DEFAULT_MAX_BOX_SIZE`: the largest payload Tika reads.
const MAX_BOX_SIZE: u64 = 100 * 1024 * 1024;
/// `DateUtil.EPOCH_1_JAN_1904`, in milliseconds since 1970.
const EPOCH_1904: i64 = -2_082_844_800_000;
/// The rate, volume, reserved bytes, matrix, pre-defined bytes and next track ID after the
/// times of a movie header.
const MOVIE_HEADER_TAIL: u64 = 80;
/// The key of a timed metadata track whose single sample is the still image of a live photo.
const STILL_IMAGE_TIME_KEY: &str = "com.apple.quicktime.still-image-time";
/// `QuickTime.STILL_IMAGE_TIME`.
const STILL_IMAGE_TIME: &str = "quicktime:still-image-time";
/// The `uuid` boxes metadata-extractor hands to its Exif, IPTC and Photoshop readers.
const READER_UUIDS: [([u8; 16], &str); 3] = [
    (
        [
            0x05, 0x37, 0xcd, 0xab, 0x9d, 0x0c, 0x44, 0x31, 0xa7, 0x2a, 0xfa, 0x56, 0x1f, 0x2a,
            0x11, 0x3e,
        ],
        "Exif",
    ),
    (
        [
            0x2c, 0x4c, 0x01, 0x00, 0x85, 0x04, 0x40, 0xb9, 0xa0, 0x3e, 0x56, 0x21, 0x48, 0xd6,
            0xdf, 0xeb,
        ],
        "Photoshop",
    ),
    (
        [
            0x33, 0xc7, 0xa4, 0xd2, 0xb8, 0x1d, 0x47, 0x23, 0xa0, 0xba, 0xf1, 0xa3, 0xe0, 0x97,
            0xad, 0x38,
        ],
        "IPTC",
    ),
];

/// What ends the boxes being read: an `IOException`, which Tika records, or what the port
/// leaves to Tika.
enum Abort {
    Thrown(Thrown),
    Deferred(Deferral),
}

impl From<Thrown> for Abort {
    fn from(thrown: Thrown) -> Self {
        Self::Thrown(thrown)
    }
}

/// The handler boxes are read by: `TikaMp4BoxHandler`, or the media handler of a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handler {
    Box,
    Track(usize),
}

/// What `Mp4Context` keeps of the last media header.
#[derive(Debug, Default)]
struct Context {
    time_scale: Option<i64>,
    duration: Option<i64>,
}

/// A file's boxes read: the movie's directory, the tracks' in the order their handler boxes
/// came, and what was written while reading, in order.
#[derive(Debug)]
pub(super) struct Walked<'a> {
    pub(super) movie: Directory<Movie>,
    pub(super) tracks: Vec<Directory<Track>>,
    pub(super) events: Vec<Event<'a>>,
}

/// `TikaMp4Reader.extract`, or why the port leaves the file to Tika.
pub(super) fn walk(input: &[u8]) -> Result<Walked<'_>, Deferral> {
    let mut walk = Walk {
        stream: Stream::new(input),
        input_length: input.len() as u64,
        movie: Directory::new(Movie::default()),
        tracks: Vec::new(),
        context: Context::default(),
        events: Vec::new(),
        quick_time_keys: Vec::new(),
        empty_edit_duration: -1,
        covers: Covers::default(),
    };
    walk.boxes(None, Handler::Box, 0)?;
    Ok(Walked {
        movie: walk.movie,
        tracks: walk.tracks,
        events: walk.events,
    })
}

/// The walk under way: the file, the directories, `Mp4Context` and what
/// `TikaMp4BoxHandler` keeps.
struct Walk<'a> {
    stream: Stream<'a>,
    input_length: u64,
    movie: Directory<Movie>,
    tracks: Vec<Directory<Track>>,
    context: Context,
    events: Vec<Event<'a>>,
    quick_time_keys: Vec<String>,
    empty_edit_duration: i64,
    covers: Covers,
}

impl<'a> Walk<'a> {
    fn errors(&mut self, handler: Handler) -> &mut Vec<String> {
        match handler {
            Handler::Box => &mut self.movie.errors,
            Handler::Track(index) => &mut self.tracks[index].errors,
        }
    }

    /// `processBoxes`: the boxes up to `end`, or to the end of the file, an exception among
    /// them recorded.
    fn boxes(&mut self, end: Option<u64>, handler: Handler, depth: u32) -> Result<(), Deferral> {
        if depth > MAX_BOX_DEPTH {
            self.errors(handler).push(format!(
                "MP4 box nesting exceeds the maximum depth of {MAX_BOX_DEPTH}"
            ));
            return Ok(());
        }
        match self.read_boxes(end, handler, depth) {
            Ok(()) => Ok(()),
            Err(Abort::Thrown(thrown)) => {
                self.events.push(Event::Recorded(thrown));
                Ok(())
            }
            Err(Abort::Deferred(deferral)) => Err(deferral),
        }
    }

    fn read_boxes(
        &mut self,
        end: Option<u64>,
        mut handler: Handler,
        depth: u32,
    ) -> Result<(), Abort> {
        while end.is_none_or(|end| self.stream.position() < end) {
            let size = self.stream.u32()?;
            let kind = four_cc(self.stream.take(4)?);
            let (size, header) = if size == 1 {
                (self.stream.i64()?, 16)
            } else {
                (i64::from(size), 8)
            };
            if size > i64::from(i32::MAX) {
                self.errors(handler).push("Box size too large.".to_owned());
                break;
            }
            if size < header {
                self.errors(handler).push("Box size too small.".to_owned());
                break;
            }
            let payload_length = (size - header) as u64;
            if self.accepts_container(handler, &kind) {
                let container_end = self.stream.position() + payload_length;
                let inner = self.container(handler, &kind);
                self.boxes(Some(container_end), inner, depth + 1)
                    .map_err(Abort::Deferred)?;
                // The next box starts where the container ends, even when a box in it failed:
                // metadata-extractor reads on from wherever the failure left it (#118).
                let position = self.stream.position();
                if position < container_end && container_end <= self.input_length {
                    self.stream.skip(container_end - position)?;
                }
            } else if self.accepts_box(handler, &kind) {
                let too_large = payload_length > MAX_BOX_SIZE;
                if too_large || self.stream.position() + payload_length > self.input_length {
                    let limit = if too_large {
                        format!("maximum of {MAX_BOX_SIZE} bytes")
                    } else {
                        "input size".to_owned()
                    };
                    self.errors(handler).push(format!(
                        "MP4 box '{kind}' payload ({payload_length} bytes) exceeds the {limit}; \
                         skipping."
                    ));
                    self.stream.skip(payload_length)?;
                } else {
                    let payload = self.stream.take(payload_length)?;
                    handler = self.leaf(handler, &kind, payload, size)?;
                }
            } else {
                self.stream.skip(payload_length)?;
            }
        }
        Ok(())
    }

    fn accepts_container(&self, handler: Handler, kind: &str) -> bool {
        match handler {
            Handler::Box => matches!(kind, "edts" | "trak" | "meta" | "moov" | "mdia"),
            Handler::Track(_) => matches!(kind, "stbl" | "minf"),
        }
    }

    fn accepts_box(&self, handler: Handler, kind: &str) -> bool {
        match handler {
            Handler::Box => matches!(
                kind,
                "udta"
                    | "keys"
                    | "ilst"
                    | "elst"
                    | "ftyp"
                    | "mvhd"
                    | "hdlr"
                    | "mdhd"
                    | "tkhd"
                    | "uuid"
            ),
            Handler::Track(index) => {
                let information = match self.tracks[index].contents {
                    Track::Sound(_) => "smhd",
                    Track::Video(_) => "vmhd",
                    Track::Hint => "hmhd",
                    Track::Text => "text",
                    Track::Meta(_) => "nmhd",
                };
                kind == information || kind == "stsd" || kind == "stts"
            }
        }
    }

    /// `processBox` of a container: a track starts with no empty edits.
    fn container(&mut self, handler: Handler, kind: &str) -> Handler {
        if handler == Handler::Box && kind == "trak" {
            self.empty_edit_duration = -1;
        }
        handler
    }

    /// `processBox` of a box with its payload: the handler the boxes after it are read by.
    fn leaf(
        &mut self,
        handler: Handler,
        kind: &str,
        payload: &'a [u8],
        size: i64,
    ) -> Result<Handler, Abort> {
        let Handler::Track(index) = handler else {
            return self.movie_box(kind, payload, size);
        };
        let Self {
            tracks,
            context,
            events,
            ..
        } = self;
        match &mut tracks[index].contents {
            Track::Sound(sound) => sound_box(sound, kind, payload, context, events)?,
            Track::Video(video) => video_box(video, kind, payload, context, events)?,
            Track::Hint => {
                if kind == "hmhd" {
                    Bytes::new(payload).skip(16)?;
                }
            }
            Track::Text => {}
            Track::Meta(meta) => match kind {
                "stsd" => meta_sample_description(meta, payload, events)?,
                "stts" => meta_time_to_sample(meta, payload, events)?,
                _ => {}
            },
        }
        Ok(handler)
    }

    /// `TikaMp4BoxHandler.processBox` of a box with its payload.
    fn movie_box(&mut self, kind: &str, payload: &'a [u8], size: i64) -> Result<Handler, Abort> {
        match kind {
            "udta" => self.user_data(payload)?,
            "keys" => self.quick_time_keys = quick_time::keys(payload),
            "ilst" => quick_time::item_list(payload, &self.quick_time_keys, &mut self.events),
            "elst" => {
                if let Some(duration) = quick_time::leading_empty_edits(payload) {
                    self.empty_edit_duration = duration;
                }
            }
            "hdlr" => return Ok(self.handler_box(payload, size)?),
            "mvhd" => self.movie_header(payload)?,
            "ftyp" => self.file_type(payload, size)?,
            "mdhd" => self.media_header(payload)?,
            "tkhd" => track_header(payload)?,
            "uuid" => uuid(payload)?,
            _ => {}
        }
        Ok(Handler::Box)
    }

    /// `processUserData`: the `udta` box, and the location its `©xyz` box gives, the altitude
    /// at once and the rest in the movie's directory.
    fn user_data(&mut self, payload: &'a [u8]) -> Read<()> {
        let coordinates = user_data::read(payload, &mut self.covers, &mut self.events)?;
        let Some(location) = coordinates.as_deref().and_then(super::location::parse) else {
            return Ok(());
        };
        self.movie.contents.latitude = Some(location.latitude);
        self.movie.contents.longitude = Some(location.longitude);
        if let Some(altitude) = location.altitude {
            self.events.push(Event::Set(
                "geo:alt",
                java::dtoa::java_double_to_string(altitude),
            ));
        }
        Ok(())
    }

    /// A handler box: the media handler of the track type it names, the meta, sound and video
    /// ones Tika's own; metadata-extractor's reading of the box for the others.
    fn handler_box(&mut self, payload: &[u8], size: i64) -> Read<Handler> {
        let track = match payload.get(8..12) {
            Some(b"meta") => Track::Meta(MetaTrack {
                key_names: Vec::new(),
                sample_count: -1,
                empty_edit_duration: self.empty_edit_duration,
                movie_time_scale: self.movie.contents.time_scale.unwrap_or(0),
            }),
            Some(b"soun") => Track::Sound(Sound::default()),
            Some(b"vide") => Track::Video(Video::default()),
            _ => {
                let mut reader = Bytes::new(payload);
                reader.skip(8)?;
                let handler_type = reader.take(4)?;
                reader.skip(12)?;
                reader.null_terminated(
                    u64::try_from(size - 32).expect("a handler box read this far is 32 bytes"),
                )?;
                match handler_type {
                    b"hint" => Track::Hint,
                    b"text" => Track::Text,
                    _ => return Ok(Handler::Box),
                }
            }
        };
        self.tracks.push(Directory::new(track));
        Ok(Handler::Track(self.tracks.len() - 1))
    }

    /// `processMovieHeader`.
    fn movie_header(&mut self, payload: &[u8]) -> Read<()> {
        let mut reader = Bytes::new(payload);
        let version = reader.u8()?;
        reader.skip(3)?;
        let (creation, modification, time_scale, duration) = if version == 1 {
            (
                reader.i64()?,
                reader.i64()?,
                i64::from(reader.u32()?),
                reader.i64()?,
            )
        } else {
            (
                i64::from(reader.u32()?),
                i64::from(reader.u32()?),
                i64::from(reader.u32()?),
                i64::from(reader.u32()?),
            )
        };
        reader.skip(MOVIE_HEADER_TAIL)?;
        let movie = &mut self.movie.contents;
        movie.creation_time = Some(since_1904(creation));
        movie.modification_time = Some(since_1904(modification));
        movie.time_scale = Some(time_scale);
        movie.duration_seconds = Some(Rational {
            numerator: duration,
            denominator: time_scale,
        });
        Ok(())
    }

    /// `processFileType`: the major brand and as many compatible brands as the box size makes
    /// room for.
    fn file_type(&mut self, payload: &[u8], size: i64) -> Read<()> {
        let mut reader = Bytes::new(payload);
        let major = four_cc(reader.take(4)?);
        reader.u32()?;
        let mut compatible = Vec::new();
        let mut at = 16;
        while at < size {
            compatible.push(four_cc(reader.take(4)?));
            at += 4;
        }
        let movie = &mut self.movie.contents;
        movie.major_brand = Some(major);
        movie.compatible_brands = Some(compatible);
        Ok(())
    }

    /// `processMediaHeader`: the time scale and duration of the track, each kept as soon as it
    /// is read.
    fn media_header(&mut self, payload: &[u8]) -> Read<()> {
        let mut reader = Bytes::new(payload);
        let version = reader.u8()?;
        reader.skip(3)?;
        if version == 1 {
            reader.skip(16)?;
            self.context.time_scale = Some(i64::from(reader.i32()?));
            self.context.duration = Some(reader.i64()?);
        } else {
            reader.skip(8)?;
            self.context.time_scale = Some(i64::from(reader.u32()?));
            self.context.duration = Some(i64::from(reader.u32()?));
        }
        reader.i16()?;
        Ok(())
    }
}

/// A box type or brand, its four bytes as ISO-8859-1 characters, so `©nam` reads as it is
/// written: metadata-extractor decodes them in the platform's charset (#118).
fn four_cc(bytes: &[u8]) -> String {
    bytes.iter().copied().map(char::from).collect()
}

/// `get1Jan1904EpochDate`: seconds since 1904 as milliseconds since 1970, in Java's `long`
/// arithmetic.
fn since_1904(seconds: i64) -> i64 {
    seconds.wrapping_mul(1000).wrapping_add(EPOCH_1904)
}

/// `processTrackHeader`, of which nothing is kept but whether it reads.
fn track_header(payload: &[u8]) -> Read<()> {
    let mut reader = Bytes::new(payload);
    let version = reader.u8()?;
    reader.skip(3)?;
    reader.skip(if version == 1 { 92 } else { 80 })
}

/// A `uuid` box: those metadata-extractor reads with a reader of another format are left to
/// Tika; it keeps nothing of the others `MP4Parser` writes.
fn uuid(payload: &[u8]) -> Result<(), Abort> {
    let Some(reader) = payload.get(..16).and_then(|uuid| {
        READER_UUIDS
            .iter()
            .find(|(known, _)| known == uuid)
            .map(|&(_, reader)| reader)
    }) else {
        return Ok(());
    };
    Err(Abort::Deferred(format!(
        "a uuid box metadata-extractor reads with its {reader} reader"
    )))
}

/// `TikaMp4SoundHandler` and `Mp4SoundHandler` of a box of the sound track.
fn sound_box(
    sound: &mut Sound,
    kind: &str,
    payload: &[u8],
    context: &Context,
    events: &mut Vec<Event<'_>>,
) -> Read<()> {
    let mut reader = Bytes::new(payload);
    match kind {
        "stsd" => {
            sample_entries::walk(payload, |four_cc, start, end| {
                let entry = sample_entries::sound(four_cc, payload, start, end);
                if entry.protected {
                    events.push(Event::Set("audio:has-drm", "true".to_owned()));
                }
                push_entry(entry, "audio:fourcc", "audio:bitrate", events);
            });
            reader.skip(32)?;
            let channels = reader.u16()?;
            let sample_size = reader.i16()?;
            reader.skip(8)?;
            sound.channels = Some(i32::from(channels));
            sound.sample_size = Some(i32::from(sample_size));
        }
        "smhd" => reader.skip(8)?,
        "stts" => {
            reader.skip(4)?;
            let entries = reader.u32()?;
            reader.skip(u64::from(entries) * 8)?;
            if let Some(time_scale) = context.time_scale {
                sound.sample_rate = Some(time_scale as f64);
            }
        }
        _ => {}
    }
    Ok(())
}

/// `TikaMp4VideoHandler` and `Mp4VideoHandler` of a box of the video track.
fn video_box(
    video: &mut Video,
    kind: &str,
    payload: &[u8],
    context: &Context,
    events: &mut Vec<Event<'_>>,
) -> Read<()> {
    let mut reader = Bytes::new(payload);
    match kind {
        "stsd" => {
            sample_entries::walk(payload, |four_cc, start, end| {
                let entry = sample_entries::video(four_cc, payload, start, end);
                push_entry(entry, "video:fourcc", "video:bitrate", events);
            });
            reader.skip(40)?;
            let width = reader.u16()?;
            let height = reader.u16()?;
            reader.skip(14)?;
            let compressor = java::utf8(reader.take(32)?);
            reader.skip(4)?;
            video.width = Some(i32::from(width));
            video.height = Some(i32::from(height));
            let compressor = java::trim(&compressor);
            if !compressor.is_empty() {
                video.compressor = Some(compressor.to_owned());
            }
        }
        "vmhd" => reader.skip(12)?,
        "stts" => {
            reader.skip(4)?;
            let entries = reader.u32()?;
            let mut samples = 0f32;
            for _ in 0..entries {
                samples += reader.u32()? as f32;
                reader.skip(4)?;
            }
            if let (Some(time_scale), Some(duration)) = (context.time_scale, context.duration) {
                video.frame_rate = Some(time_scale as f32 / (duration as f32 / samples));
            }
        }
        _ => {}
    }
    Ok(())
}

/// The format and bit rate Tika sets of a sample entry.
fn push_entry(
    entry: sample_entries::SampleEntry,
    four_cc_name: &'static str,
    bit_rate_name: &'static str,
    events: &mut Vec<Event<'_>>,
) {
    if let Some(four_cc) = entry.four_cc {
        events.push(Event::Set(four_cc_name, four_cc));
    }
    if let Some(bit_rate) = entry.bit_rate {
        events.push(Event::Set(bit_rate_name, bit_rate.to_string()));
    }
}

/// `TikaMp4MetaHandler.processSampleDescription`: the key names of its `mebx` entries, up to
/// an entry that does not fit, which ends it.
fn meta_sample_description(
    meta: &mut MetaTrack,
    payload: &[u8],
    events: &mut Vec<Event<'_>>,
) -> Read<()> {
    let mut reader = Bytes::new(payload);
    reader.skip(4)?;
    let entries = reader.u32()?;
    for _ in 0..entries {
        let size = u64::from(reader.u32()?);
        let format = reader.take(4)?;
        if size < 16 || size - 8 > reader.available() {
            return Ok(());
        }
        let entry = reader.take(size - 8)?;
        if format == b"mebx" {
            quick_time::mebx_key_names(entry, &mut meta.key_names);
        }
    }
    still_image_time(meta, events);
    Ok(())
}

/// `TikaMp4MetaHandler.processTimeToSample`: the track's sample count.
fn meta_time_to_sample(
    meta: &mut MetaTrack,
    payload: &[u8],
    events: &mut Vec<Event<'_>>,
) -> Read<()> {
    let mut reader = Bytes::new(payload);
    reader.skip(4)?;
    let entries = reader.u32()?;
    let mut samples: i64 = 0;
    for _ in 0..entries {
        samples = samples.wrapping_add(i64::from(reader.u32()?));
        reader.skip(4)?;
    }
    meta.sample_count = samples;
    still_image_time(meta, events);
    Ok(())
}

/// `maybeEmit`: the time of the still image of a live photo's single-sample still image track,
/// in microseconds, from the empty edits before it.
fn still_image_time(meta: &mut MetaTrack, events: &mut Vec<Event<'_>>) {
    if meta.sample_count != 1
        || !meta
            .key_names
            .iter()
            .any(|name| name == STILL_IMAGE_TIME_KEY)
    {
        return;
    }
    if meta.empty_edit_duration <= 0 {
        events.push(Event::Set(STILL_IMAGE_TIME, "0".to_owned()));
    } else if meta.movie_time_scale > 0 && meta.empty_edit_duration <= i64::MAX / 1_000_000 {
        events.push(Event::Set(
            STILL_IMAGE_TIME,
            (meta.empty_edit_duration * 1_000_000 / meta.movie_time_scale).to_string(),
        ));
    }
    meta.key_names.clear();
}
