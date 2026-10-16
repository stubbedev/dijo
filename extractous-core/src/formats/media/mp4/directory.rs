//! The directories metadata-extractor reads an MP4 file into, as much of them as `MP4Parser`
//! reads, and the metadata `MP4Parser` writes of them once the boxes are read.

use super::Event;
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::pdf::jfloat::float_to_string;
use crate::formats::xlsx::format::decimal::Rounding;
use crate::formats::xlsx::format::decimal_format::DecimalFormat;
use crate::java::dtoa::java_double_to_string;
use crate::java::time::format_instant;
use crate::java::{self, hash_set_order};

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
    fn double_value(self) -> f64 {
        if self.numerator == 0 {
            0.0
        } else {
            self.numerator as f64 / self.denominator as f64
        }
    }

    fn is_integer(self) -> bool {
        self.denominator == 1
            || (self.denominator != 0 && self.numerator.wrapping_rem(self.denominator) == 0)
            || (self.denominator == 0 && self.numerator == 0)
    }

    fn fraction(self) -> String {
        if self.denominator < 0 {
            format!(
                "{}/{}",
                self.numerator.wrapping_neg(),
                self.denominator.wrapping_neg()
            )
        } else {
            format!("{}/{}", self.numerator, self.denominator)
        }
    }

    /// `getSimplifiedInstance`, `None` where its `GCD` never returns: where one of the two is
    /// `Long.MIN_VALUE`, which stays negative, and the other neither 0 nor it.
    fn simplified(self) -> Option<Self> {
        let (mut numerator, mut denominator) = (self.numerator, self.denominator);
        if denominator < 0 {
            numerator = numerator.wrapping_neg();
            denominator = denominator.wrapping_neg();
        }
        let (mut a, mut b) = (numerator.wrapping_abs(), denominator.wrapping_abs());
        while a != 0 && b != 0 {
            let (before_a, before_b) = (a, b);
            if a > b {
                a = a.wrapping_rem(b);
            } else {
                b = b.wrapping_rem(a);
            }
            if (a, b) == (before_a, before_b) {
                return None;
            }
        }
        let gcd = if a == 0 { b } else { a };
        Some(Self {
            numerator: numerator.wrapping_div(gcd),
            denominator: denominator.wrapping_div(gcd),
        })
    }

    /// `toSimpleString(true)`, as `Directory.getString` writes a rational; `None` where it
    /// never returns.
    pub(super) fn simple_string(self) -> Option<String> {
        if self.denominator == 0 && self.numerator != 0 {
            return Some(self.fraction());
        }
        if self.is_integer() {
            return Some((self.double_value() as i32).to_string());
        }
        let simplified = self.simplified()?;
        let decimal = java_double_to_string(simplified.double_value());
        Some(if decimal.len() < 5 {
            decimal
        } else {
            simplified.fraction()
        })
    }
}

/// `handleDurationInSeconds`: the duration a rational's string gives, as `0.0#`.
fn duration(seconds: &str) -> Option<String> {
    let format = DecimalFormat::new("0.0#", Rounding::HalfEven).expect("the pattern compiles");
    let value = match seconds.split_once('/') {
        None => java::parse_double(seconds)?,
        Some((numerator, denominator)) => {
            let numerator = java::parse_long(numerator)?;
            let denominator = java::parse_long(denominator).filter(|&value| value != 0)?;
            numerator as f64 / denominator as f64
        }
    };
    Some(format.format_f64(value))
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
    for error in hash_set_order(errors) {
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
    if let Some(duration) = movie
        .duration_seconds
        .map(|seconds| {
            seconds
                .simple_string()
                .expect("a duration whose GCD never returns is deferred")
        })
        .and_then(|seconds| duration(&seconds))
    {
        events.push(Event::Set("xmpDM:duration", duration));
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

/// `handleBrands`: the type the first entry of `typesMap`, in its iteration order, whose
/// brands hold the major brand names, else whose brands occur in the compatible brands, and
/// the major brand as the compressor of audio.
fn brands(movie: &Movie, events: &mut Vec<Event<'_>>) {
    let major = movie.major_brand.as_deref();
    let order = hash_set_order(TYPES.iter().map(|&(name, _)| name));
    let brands_of = |name: &str| {
        TYPES
            .iter()
            .find(|&&(type_name, _)| type_name == name)
            .map_or(&[][..], |&(_, brands)| brands)
    };
    let joined = movie
        .compatible_brands
        .as_ref()
        .map(|brands| brands.join(" "));
    let found = order
        .iter()
        .copied()
        .find(|&name| major.is_some_and(|major| brands_of(name).contains(&major)))
        .or_else(|| {
            let joined = joined.as_deref()?;
            order
                .iter()
                .copied()
                .find(|&name| brands_of(name).iter().any(|brand| joined.contains(brand)))
        })
        .unwrap_or(APPLICATION_MP4);
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

    fn simple(numerator: i64, denominator: i64) -> Option<String> {
        Rational {
            numerator,
            denominator,
        }
        .simple_string()
    }

    #[test]
    fn writes_rationals_as_metadata_extractor_does() {
        assert_eq!(simple(1070, 1000).as_deref(), Some("1.07"));
        assert_eq!(simple(1000, 1000).as_deref(), Some("1"));
        assert_eq!(simple(0, 0).as_deref(), Some("0"));
        assert_eq!(simple(5, 0).as_deref(), Some("5/0"));
        assert_eq!(simple(2730, 600).as_deref(), Some("4.55"));
        assert_eq!(simple(1001, 30000).as_deref(), Some("1001/30000"));
        assert_eq!(simple(i64::MIN, 1000), None);
        assert_eq!(simple(i64::MIN, 1024).as_deref(), Some("-2147483648"));
    }

    #[test]
    fn writes_durations_as_mp4_parser_does() {
        assert_eq!(duration("4.55").as_deref(), Some("4.55"));
        assert_eq!(duration("1001/30000").as_deref(), Some("0.03"));
        assert_eq!(duration("1").as_deref(), Some("1.0"));
        assert_eq!(duration("5/0"), None);
    }
}
