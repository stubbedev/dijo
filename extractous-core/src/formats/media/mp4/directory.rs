//! The directories metadata-extractor reads an MP4 file into, as much of them as `MP4Parser`
//! reads, and the metadata `MP4Parser` writes of them once the boxes are read.

use super::Event;
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::pdf::jfloat::float_to_string;
use crate::java;
use crate::java::decimal::Rounding;
use crate::java::decimal_format::DecimalFormat;
use crate::java::dtoa::java_double_to_string;
use crate::java::time::format_instant;

/// `MAX_ERROR_MESSAGES`: how many distinct directory errors Tika reports.
const MAX_ERROR_MESSAGES: usize = 100;
/// The type `MP4Parser` falls back to where no brand names one.
const APPLICATION_MP4: &str = "application/mp4";
/// `MP4Parser.typesMap`: the brands of each type it reports.
const TYPES: [(&str, &[&str]); 7] = [
    ("audio/mp4", &["M4A ", "M4B ", "F4A ", "F4B "]),
    (
        "video/3gpp",
        &[
            "3ge6", "3ge7", "3gg6", "3gp1", "3gp2", "3gp3", "3gp4", "3gp5", "3gp6", "3gs7",
        ],
    ),
    ("video/3gpp2", &["3g2a", "3g2b", "3g2c"]),
    ("video/mp4", &["mp41", "mp42"]),
    ("video/x-m4v", &["M4V ", "M4VH", "M4VP"]),
    ("video/quicktime", &[]),
    (APPLICATION_MP4, &[]),
];

/// A directory: what was read into it, and the errors the box reader added to it.
#[derive(Debug)]
pub(super) struct Directory<T> {
    pub(super) contents: T,
    pub(super) errors: Vec<String>,
}

impl<T> Directory<T> {
    pub(super) fn new(contents: T) -> Self {
        Self {
            contents,
            errors: Vec::new(),
        }
    }
}

/// `Mp4Directory` of the movie: its file type, header and location.
#[derive(Debug, Default)]
pub(super) struct Movie {
    pub(super) major_brand: Option<String>,
    pub(super) compatible_brands: Option<Vec<String>>,
    /// Milliseconds since 1970.
    pub(super) creation_time: Option<i64>,
    pub(super) modification_time: Option<i64>,
    pub(super) time_scale: Option<i64>,
    pub(super) duration_seconds: Option<Rational>,
    pub(super) latitude: Option<f64>,
    pub(super) longitude: Option<f64>,
}

/// The directory of a track's media handler.
#[derive(Debug)]
pub(super) enum Track {
    Sound(Sound),
    Video(Video),
    Hint,
    Text,
    Meta(MetaTrack),
}

/// `Mp4SoundDirectory`.
#[derive(Debug, Default)]
pub(super) struct Sound {
    pub(super) channels: Option<i32>,
    pub(super) sample_size: Option<i32>,
    pub(super) sample_rate: Option<f64>,
}

/// `Mp4VideoDirectory`.
#[derive(Debug, Default)]
pub(super) struct Video {
    pub(super) width: Option<i32>,
    pub(super) height: Option<i32>,
    pub(super) compressor: Option<String>,
    pub(super) frame_rate: Option<f32>,
}

/// What `TikaMp4MetaHandler` keeps of a timed metadata track.
#[derive(Debug)]
pub(super) struct MetaTrack {
    pub(super) key_names: Vec<String>,
    pub(super) sample_count: i64,
    pub(super) empty_edit_duration: i64,
    pub(super) movie_time_scale: i64,
}

/// metadata-extractor's `Rational`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rational {
    pub(super) numerator: i64,
    pub(super) denominator: i64,
}

impl Rational {
    /// The seconds the rational counts, none without a time scale.
    fn seconds(self) -> Option<f64> {
        (self.denominator != 0).then(|| self.numerator as f64 / self.denominator as f64)
    }
}

/// `handleDurationInSeconds`: the duration in seconds, as `0.0#`. Tika formats the rational's
/// `toSimpleString`, which truncates a whole number of seconds to an `int` and may never end
/// for a duration of `Long.MIN_VALUE` (#118).
fn duration(seconds: f64) -> String {
    DecimalFormat::new("0.0#", Rounding::HalfEven)
        .expect("the pattern compiles")
        .format_f64(seconds)
}

/// `MP4Parser`'s writing of the directories, in the order they were added: the movie's, then
/// the tracks', the type an audio-only file is, and the distinct errors the directories hold.
pub(super) fn metadata(
    movie: &Directory<Movie>,
    tracks: &[Directory<Track>],
    events: &mut Vec<Event<'_>>,
) {
    fn gather<'e>(errors: &mut Vec<&'e str>, directory_errors: &'e [String]) {
        for error in directory_errors {
            if errors.len() >= MAX_ERROR_MESSAGES {
                break;
            }
            if !errors.contains(&error.as_str()) {
                errors.push(error);
            }
        }
    }
    let mut errors = Vec::new();
    gather(&mut errors, &movie.errors);
    movie_metadata(&movie.contents, events);
    for track in tracks {
        gather(&mut errors, &track.errors);
        match &track.contents {
            Track::Sound(sound) => sound_metadata(sound, events),
            Track::Video(video) => video_metadata(video, events),
            Track::Hint | Track::Text | Track::Meta(_) => {}
        }
    }
    let has_sound = tracks
        .iter()
        .any(|track| matches!(track.contents, Track::Sound(_)));
    let has_video = tracks
        .iter()
        .any(|track| matches!(track.contents, Track::Video(_)));
    if has_sound && !has_video {
        events.push(Event::Set("Content-Type", "audio/mp4".to_owned()));
    }
    // In the order they were first met: Tika reports them in `HashSet` order (#118).
    for error in errors {
        events.push(Event::Add(EXCEPTION_WARNING, error.to_owned()));
    }
}

/// `processActualMp4Directory`.
fn movie_metadata(movie: &Movie, events: &mut Vec<Event<'_>>) {
    if let Some(created) = movie.creation_time {
        events.push(Event::Set("dcterms:created", format_instant(created)));
    }
    if let Some(modified) = movie.modification_time {
        events.push(Event::Set("dcterms:modified", format_instant(modified)));
    }
    brands(movie, events);
    if let Some(seconds) = movie.duration_seconds.and_then(Rational::seconds) {
        events.push(Event::Set("xmpDM:duration", duration(seconds)));
    }
    if let Some(latitude) = movie.latitude {
        events.push(Event::Set("geo:lat", java_double_to_string(latitude)));
    }
    if let Some(longitude) = movie.longitude {
        events.push(Event::Set("geo:long", java_double_to_string(longitude)));
    }
    if let Some(time_scale) = movie.time_scale {
        events.push(Event::Set(
            "xmpDM:audioSampleRate",
            (time_scale as i32).to_string(),
        ));
    }
}

/// `handleBrands`: the type of `typesMap`, in its order, whose brands hold the major brand,
/// else one of the compatible brands, and the major brand as the compressor of audio. Tika
/// takes the types in `HashMap` order and finds a brand anywhere in the compatible brands joined
/// by spaces, so `mp4` would match `mp41 ` and `M4A` across two brands (#118).
fn brands(movie: &Movie, events: &mut Vec<Event<'_>>) {
    let major = movie.major_brand.as_deref();
    let compatible = movie.compatible_brands.as_deref().unwrap_or_default();
    let found = TYPES
        .iter()
        .find(|(_, brands)| major.is_some_and(|major| brands.contains(&major)))
        .or_else(|| {
            TYPES.iter().find(|(_, brands)| {
                compatible
                    .iter()
                    .any(|brand| brands.contains(&brand.as_str()))
            })
        })
        .map_or(APPLICATION_MP4, |&(name, _)| name);
    if found != APPLICATION_MP4 {
        events.push(Event::Set("Content-Type", found.to_owned()));
    }
    if let Some(major) = major.filter(|major| found.starts_with("audio/") && !java::is_blank(major))
    {
        events.push(Event::Set(
            "xmpDM:audioCompressor",
            java::trim(major).to_owned(),
        ));
    }
}

/// `processMp4SoundDirectory`.
fn sound_metadata(sound: &Sound, events: &mut Vec<Event<'_>>) {
    if let Some(rate) = sound.sample_rate {
        events.push(Event::Set(
            "xmpDM:audioSampleRate",
            (rate as i32).to_string(),
        ));
    }
    if let Some(size) = sound.sample_size {
        events.push(Event::Set("audio:bits-per-sample", size.to_string()));
    }
    if let Some(channels) = sound.channels {
        events.push(Event::Set("audio:channels", channels.to_string()));
        let channel_type = match channels {
            1 => Some("Mono"),
            2 => Some("Stereo"),
            _ => None,
        };
        if let Some(channel_type) = channel_type {
            events.push(Event::Set(
                "xmpDM:audioChannelType",
                channel_type.to_owned(),
            ));
        }
    }
}

/// `processMp4VideoDirectory`.
fn video_metadata(video: &Video, events: &mut Vec<Event<'_>>) {
    if let Some(height) = video.height {
        events.push(Event::Set("tiff:ImageLength", height.to_string()));
    }
    if let Some(width) = video.width {
        events.push(Event::Set("tiff:ImageWidth", width.to_string()));
    }
    if let Some(compressor) = &video.compressor {
        events.push(Event::Set("xmpDM:videoCompressor", compressor.clone()));
    }
    if let Some(frame_rate) = video.frame_rate {
        events.push(Event::Set("video:frame-rate", float_to_string(frame_rate)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_files_by_whole_brands_in_declared_order() {
        // Tika finds a brand anywhere in the compatible brands joined by spaces, taking the
        // types in `HashMap` order (#118).
        let content_type = |major: &str, compatible: &[&str]| {
            let movie = Movie {
                major_brand: Some(major.to_owned()),
                compatible_brands: Some(compatible.iter().map(|&b| b.to_owned()).collect()),
                ..Movie::default()
            };
            let mut events = Vec::new();
            brands(&movie, &mut events);
            events.into_iter().find_map(|event| match event {
                Event::Set("Content-Type", value) => Some(value),
                _ => None,
            })
        };
        assert_eq!(content_type("isom", &["zM4A", " zzz"]), None);
        assert_eq!(
            content_type("isom", &["isom", "mp42", "M4A "]).as_deref(),
            Some("audio/mp4")
        );
        assert_eq!(
            content_type("mp42", &["M4V "]).as_deref(),
            Some("video/mp4")
        );
    }

    #[test]
    fn writes_durations_from_the_rational() {
        let seconds = |numerator, denominator| {
            Rational {
                numerator,
                denominator,
            }
            .seconds()
        };
        assert_eq!(seconds(2730, 600).map(duration).as_deref(), Some("4.55"));
        assert_eq!(seconds(1001, 30000).map(duration).as_deref(), Some("0.03"));
        assert_eq!(seconds(1000, 1000).map(duration).as_deref(), Some("1.0"));
        assert_eq!(seconds(5, 0), None);
        // Tika truncates whole seconds to an `int`, or never ends (#118).
        assert_eq!(
            seconds(i64::MIN, 1024).map(duration).as_deref(),
            Some("-9007199254740992.0")
        );
    }
}
