//! MP3 files as Tika's `Mp3Parser` reads them: the ID3v2 tags at the start ([`id3v2`]), the
//! MPEG audio frames after them for the duration, bit rate, channels and sample rate
//! ([`mpeg`]), and the ID3v1 tag and Lyrics3 v2 lyrics in the file's last 10,368 bytes; the
//! tags' text as metadata and paragraphs, their pictures as embedded documents.

mod id3v2;
mod mpeg;

use id3v2::{Frame, Tags, Thrown, Version};
use mpeg::{AudioFrame, Frames, Stream};

use super::{extract_pictures, set_nullable, NumberAndTotal, Picture};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;
use crate::java::dtoa::java_double_to_string;

/// The bytes at the end of the file `Mp3Parser`'s `TailStream` keeps for the ID3v1 tag and the
/// lyrics.
const TAIL: usize = 10240 + 128;
const ID3V1_LENGTH: usize = 128;

/// The ID3v1 genres, `ID3Tags.GENRES`.
const GENRES: [&str; 127] = [
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychadelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
    "Folk",
    "Folk-Rock",
    "National Folk",
    "Swing",
    "Fast Fusion",
    "Bebob",
    "Latin",
    "Revival",
    "Celtic",
    "Bluegrass",
    "Avantgarde",
    "Gothic Rock",
    "Progressive Rock",
    "Psychedelic Rock",
    "Symphonic Rock",
    "Slow Rock",
    "Big Band",
    "Chorus",
    "Easy Listening",
    "Acoustic",
    "Humour",
    "Speech",
    "Chanson",
    "Opera",
    "Chamber Music",
    "Sonata",
    "Symphony",
    "Booty Bass",
    "Primus",
    "Porn Groove",
    "Satire",
    "Slow Jam",
    "Club",
    "Tango",
    "Samba",
    "Folklore",
    "Ballad",
    "Power Ballad",
    "Rhythmic Soul",
    "Freestyle",
    "Duet",
    "Punk Rock",
    "Drum Solo",
    "A capella",
    "Euro-House",
    "Dance Hall",
    "",
];

/// A comment of a tag, `ID3Comment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Comment {
    language: Option<String>,
    description: Option<String>,
    text: Option<String>,
}

impl Comment {
    /// The paragraph `Mp3Parser` writes of the comment: the language, the description and the
    /// text, as far as the comment has them.
    fn written(&self) -> String {
        let mut written = String::new();
        if let Some(language) = &self.language {
            written.push_str(language);
            written.push_str(" - ");
        }
        if let Some(description) = &self.description {
            written.push_str(description);
            if self.text.is_some() {
                written.push('\n');
            }
        }
        if let Some(text) = &self.text {
            written.push_str(text);
        }
        written
    }
}

/// `ID3v1Handler`: the tag in the last 128 bytes, if they hold one.
fn id3v1(tag: &[u8]) -> Option<Tags> {
    if tag.len() != ID3V1_LENGTH || !tag.starts_with(b"TAG") {
        return None;
    }
    let text = |start: usize, end: usize| {
        let end = start + tag[start..end].iter().take_while(|&&b| b != 0).count();
        let is_space = |byte: u8| byte as i8 <= b' ' as i8;
        let mut end = end;
        while start < end && is_space(tag[end - 1]) {
            end -= 1;
        }
        let mut start = start;
        while start < end && is_space(tag[start]) {
            start += 1;
        }
        tag[start..end]
            .iter()
            .map(|&byte| char::from(byte))
            .collect::<String>()
    };
    let genre = GENRES[usize::from(tag[127]).min(GENRES.len() - 1)];
    Some(Tags {
        title: Some(text(3, 33)),
        artist: Some(text(33, 63)),
        album: Some(text(63, 93)),
        year: Some(text(93, 97)),
        comments: vec![Comment {
            language: None,
            description: None,
            text: Some(text(97, 127)),
        }],
        genre: Some(genre.to_owned()),
        track_number: (tag[125] == 0 && tag[126] != 0).then(|| tag[126].to_string()),
        ..Tags::default()
    })
}

/// `LyricsHandler`: the ID3v1 tag of the last 128 bytes of `tail` and the `LYR` field of the
/// Lyrics3 v2 block before it, or what Java throws on a block it cannot read.
fn lyrics(tail: &[u8]) -> Result<(Option<Tags>, Option<String>), Thrown> {
    if tail.len() < ID3V1_LENGTH {
        return Ok((None, None));
    }
    let v1 = id3v1(&tail[tail.len() - ID3V1_LENGTH..]);
    if tail.len() < 137 {
        return Ok((v1, None));
    }
    let mut look_at = tail.len() - 9;
    if v1.is_some() {
        look_at -= ID3V1_LENGTH;
    }
    if &tail[look_at..look_at + 9] != b"LYRICS200" {
        return Ok((v1, None));
    }
    let out_of_bounds = || "StringIndexOutOfBoundsException".to_owned();
    let size_field = tail
        .get(look_at.checked_sub(6).ok_or_else(out_of_bounds)?..look_at)
        .ok_or_else(out_of_bounds)?;
    let size_text = java::utf8(size_field);
    let length = java::parse_int(&size_text)
        .ok_or_else(|| format!("NumberFormatException: For input string: \"{size_text}\""))?;
    let start = i64::try_from(look_at).expect("in memory") - i64::from(length) + 5;
    let count = i64::from(length) - 11;
    let (Ok(start), Ok(count)) = (usize::try_from(start), usize::try_from(count)) else {
        return Err(out_of_bounds());
    };
    let block: Vec<char> = tail
        .get(start..start + count)
        .ok_or_else(out_of_bounds)?
        .iter()
        .map(|&byte| {
            if byte.is_ascii() {
                char::from(byte)
            } else {
                char::REPLACEMENT_CHARACTER
            }
        })
        .collect();
    let mut text = None;
    let mut position = 0;
    while position + 8 < block.len() {
        let name: String = block[position..position + 3].iter().collect();
        let size: String = block[position + 3..position + 8].iter().collect();
        let size = java::parse_int(&size)
            .ok_or_else(|| format!("NumberFormatException: For input string: \"{size}\""))?;
        let Some(size) = usize::try_from(size)
            .ok()
            .filter(|&size| size >= 1 && size <= block.len())
        else {
            break;
        };
        let start = position + 8;
        let end = start + size;
        if name == "LYR" {
            text = Some(
                block
                    .get(start..end)
                    .ok_or_else(out_of_bounds)?
                    .iter()
                    .collect(),
            );
        }
        position = end;
    }
    Ok((v1, text))
}

/// What `getAllTagHandlers` reads of a file.
struct TagsAndAudio {
    tags: Vec<Tags>,
    audio: Option<AudioFrame>,
    lyrics: Option<String>,
    /// Milliseconds, as a `float`.
    duration: f32,
    average_bit_rate: i32,
    variable_bit_rate: bool,
}

/// `getAllTagHandlers`: the ID3v2 tags at the start, the frames after them, and the ID3v1 tag
/// and lyrics at the end; or what Java throws.
fn read(input: &[u8]) -> Result<TagsAndAudio, Thrown> {
    let mut stream = Stream::new(input);
    let mut handlers: [Option<Tags>; 3] = [None, None, None];
    while let Some(frame) = Frame::if_present(&mut stream)? {
        if let Some(version) = frame.version() {
            let slot = match version {
                Version::V24 => 0,
                Version::V23 => 1,
                Version::V22 => 2,
            };
            handlers[slot] = Some(frame.tags(version)?);
        }
    }
    let mut frames = Frames::new(&mut stream);
    let mut frame = frames.next_frame();
    let mut first_audio = None;
    let mut duration = 0f32;
    let mut bit_rate_sum: i64 = 0;
    let mut frame_count: i64 = 0;
    let mut baseline = -1;
    let mut variable_bit_rate = false;
    let mut first_frame = true;
    while let Some(current) = frame {
        duration += current.duration;
        if !(first_frame && frames.is_metadata_frame()) {
            bit_rate_sum += i64::from(current.bit_rate);
            frame_count += 1;
            if baseline < 0 {
                baseline = current.bit_rate;
            } else if current.bit_rate != baseline {
                variable_bit_rate = true;
            }
        }
        first_audio.get_or_insert(current);
        first_frame = false;
        frame = if frames.skip_frame() {
            frames.next_frame()
        } else {
            None
        };
    }
    let tail = &input[input.len().saturating_sub(TAIL)..];
    let (v1, lyrics) = lyrics(tail)?;
    let tags = handlers.into_iter().chain([v1]).flatten().collect();
    let average_bit_rate = if frame_count > 0 {
        (bit_rate_sum / frame_count) as i32
    } else {
        0
    };
    Ok(TagsAndAudio {
        tags,
        audio: first_audio,
        lyrics,
        duration,
        average_bit_rate,
        variable_bit_rate: frame_count > 0 && variable_bit_rate,
    })
}

/// `CompositeTagHandler`: the first value any tag has.
fn first<'t>(tags: &'t [Tags], field: impl Fn(&'t Tags) -> &'t Option<String>) -> Option<&'t str> {
    tags.iter().find_map(|tag| field(tag).as_deref())
}

pub(crate) struct Mp3Parser;

impl Parser for Mp3Parser {
    fn name(&self) -> &'static str {
        "extractous::Mp3Parser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mp3.Mp3Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["audio/mpeg"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let read = read(input).map_err(|thrown| Declined::Fails(Error::ParseError(thrown)))?;
        Ok(Box::new(move |xhtml| write(&read, xhtml)))
    }
}

/// `Mp3Parser.parse` once the file is read.
fn write(read: &TagsAndAudio, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let metadata = xhtml.metadata_mut();
    metadata.set("Content-Type", "audio/mpeg");
    metadata.set("xmpDM:audioCompressor", "MP3");
    if read.duration > 0.0 {
        let seconds = read.duration / 1000.0;
        metadata.set("xmpDM:duration", java_double_to_string(f64::from(seconds)));
    }
    if read.average_bit_rate > 0 {
        metadata.set("audio:bitrate", read.average_bit_rate.to_string());
        metadata.set(
            "audio:is-variable-bitrate",
            read.variable_bit_rate.to_string(),
        );
    }
    if let Some(audio) = &read.audio {
        metadata.set("audio:channels", audio.channels.to_string());
        metadata.set("mp3:version", audio.version());
        metadata.set("xmpDM:audioSampleRate", audio.sample_rate.to_string());
        let channel_type = match audio.channels {
            1 => Some("Mono"),
            2 => Some("Stereo"),
            5 => Some("5.1"),
            7 => Some("7.1"),
            _ => None,
        };
        if let Some(channel_type) = channel_type {
            metadata.set("xmpDM:audioChannelType", channel_type);
        }
    }
    let tags = &read.tags;
    let mut comments = Vec::new();
    if !tags.is_empty() {
        let title = first(tags, |tag| &tag.title);
        let artist = first(tags, |tag| &tag.artist);
        let album = first(tags, |tag| &tag.album);
        let year = first(tags, |tag| &tag.year);
        let genre = first(tags, |tag| &tag.genre);
        set_nullable(metadata, "dc:title", title);
        set_nullable(metadata, "dc:creator", artist);
        set_nullable(metadata, "xmpDM:artist", artist);
        set_nullable(
            metadata,
            "xmpDM:albumArtist",
            first(tags, |tag| &tag.album_artist),
        );
        set_nullable(metadata, "xmpDM:composer", first(tags, |tag| &tag.composer));
        set_nullable(
            metadata,
            "xmpDM:copyright",
            first(tags, |tag| &tag.copyright),
        );
        set_nullable(metadata, "xmpDM:album", album);
        set_nullable(
            metadata,
            "xmpDM:compilation",
            first(tags, |tag| &tag.compilation),
        );
        set_nullable(metadata, "xmpDM:releaseDate", year);
        set_nullable(metadata, "xmpDM:genre", genre);
        let tag_comments = tags
            .iter()
            .map(|tag| &tag.comments)
            .find(|comments| !comments.is_empty());
        for comment in tag_comments.into_iter().flatten() {
            let written = comment.written();
            metadata.add("xmpDM:logComment", written.clone());
            comments.push(written);
        }
        let mut position = album.unwrap_or_default().to_owned();
        if let Some(track) = first(tags, |tag| &tag.track_number) {
            position.push_str(&format!(", track {track}"));
            metadata.set("audio:raw-track-number", track);
            if let Some(numbers) = NumberAndTotal::parse(track) {
                if let Some(number) = numbers.number {
                    metadata.set("xmpDM:trackNumber", number.to_string());
                }
                if let Some(total) = numbers.total {
                    metadata.set("audio:track-count", total.to_string());
                }
            }
        }
        if let Some(disc) = first(tags, |tag| &tag.disc) {
            position.push_str(&format!(", disc {disc}"));
            metadata.set("audio:raw-disc-number", disc);
            if let Some(numbers) = NumberAndTotal::parse(disc) {
                if let Some(number) = numbers.number {
                    metadata.set("xmpDM:discNumber", number.to_string());
                }
                if let Some(total) = numbers.total {
                    metadata.set("audio:disc-count", total.to_string());
                }
            }
        }
        xhtml.element("h1", title.unwrap_or_default())?;
        xhtml.element("p", artist.unwrap_or_default())?;
        xhtml.element("p", &position)?;
        xhtml.element("p", year.unwrap_or_default())?;
        xhtml.element("p", genre.unwrap_or_default())?;
    }
    for comment in &comments {
        xhtml.element("p", comment)?;
    }
    if let Some(lyrics) = read.lyrics.as_deref().filter(|lyrics| !lyrics.is_empty()) {
        xhtml.start_element_with("p", &[("class", "lyrics")])?;
        xhtml.characters(lyrics)?;
        xhtml.end_element("p")?;
    }
    let pictures: Vec<Picture> = tags
        .iter()
        .flat_map(|tag| tag.pictures.iter().cloned())
        .collect();
    extract_pictures(&pictures, xhtml)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

    const TIKA_NAME: &str = "org.apache.tika.parser.mp3.Mp3Parser";

    #[test]
    fn reads_id3v1_tags_as_tika_does() {
        let mut tag = vec![0u8; 128];
        tag[..3].copy_from_slice(b"TAG");
        tag[3..9].copy_from_slice(b" Song\xE9");
        tag[126] = 7;
        tag[127] = 200;
        let read = id3v1(&tag).unwrap();
        assert_eq!(read.title.as_deref(), Some("Song"));
        assert_eq!(read.track_number.as_deref(), Some("7"));
        assert_eq!(read.genre.as_deref(), Some(""));
    }

    /// An ID3v2.3 frame.
    fn frame(name: &str, data: &[u8]) -> Vec<u8> {
        let mut frame = name.as_bytes().to_vec();
        frame.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(data);
        frame
    }

    /// A file with an ID3v2.3 tag, three MPEG frames, the first a Xing frame, Lyrics3 v2 lyrics
    /// and an ID3v1 tag.
    fn tagged_mp3() -> Vec<u8> {
        let frames = [
            frame("TIT2", b"\x00Sang om havet"),
            frame("TPE1", b"\x01\xFF\xFEK\x00\xF8\x00r\x00\x00\x00"),
            frame("TALB", b"\x03Kyst\xC3\xA6r"),
            frame("TRCK", b"\x003/12"),
            frame("TPOS", b"\x001"),
            frame("TCON", b"\x00(17)"),
            frame("COMM", b"\x00danbeskrivelse\x00en kommentar"),
        ]
        .concat();
        let size = u32::try_from(frames.len()).unwrap();
        let synchsafe = [21, 14, 7, 0].map(|shift| u8::try_from(size >> shift & 0x7F).unwrap());
        let mut mp3 = b"ID3\x03\x00\x00".to_vec();
        mp3.extend_from_slice(&synchsafe);
        mp3.extend_from_slice(&frames);
        for index in 0..3 {
            let mut audio = vec![0u8; 417];
            audio[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
            if index == 0 {
                audio[36..40].copy_from_slice(b"Xing");
            }
            mp3.extend_from_slice(&audio);
        }
        let fields = b"IND0000210LYR00022B\xF8lgerne sl\xE5r mod land";
        let mut lyrics = b"LYRICSBEGIN".to_vec();
        lyrics.extend_from_slice(fields);
        let length = lyrics.len();
        mp3.extend_from_slice(&lyrics);
        mp3.extend_from_slice(format!("{length:06}LYRICS200").as_bytes());
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..7].copy_from_slice(b"Navn");
        v1[127] = 17;
        mp3.extend_from_slice(&v1);
        mp3
    }

    #[test]
    fn extracts_mp3s_as_tikas_path_does() {
        let scratch = Scratch::new("mp3");
        let tagged = tagged_mp3();
        let read = [
            scratch.write("tagged.mp3", &tagged),
            scratch.write("frames.mp3", &tagged[tagged.len() - 1500..]),
        ];
        let (parsed, deferred) = documents(&Mp3Parser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_mp3s_as_tikas_path_does() {
        let (parsed, deferred) = documents(&Mp3Parser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 27, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
