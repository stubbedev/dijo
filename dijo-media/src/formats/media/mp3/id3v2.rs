//! ID3v2 tags as Tika's `ID3v2Frame` reads them and its `ID3v22Handler`, `ID3v23Handler` and
//! `ID3v24Handler` take their frames: the text, comment and picture frames Tika knows, each text
//! decoded by its encoding byte.

use super::mpeg::Stream;
use super::Comment;
use crate::formats::media::Picture;
use crate::java;
use crate::text::charset::Charset;

/// `ID3v2Frame.MAX_RECORD_SIZE`.
const MAX_RECORD_SIZE: i64 = 50_000_000;

/// What Tika throws while it reads a tag, which fails the parse.
pub(super) type Thrown = String;

/// The text encodings of an ID3v2 encoding byte, and whether each takes two bytes a character.
const ENCODINGS: [(&str, bool); 4] = [
    ("ISO-8859-1", false),
    ("UTF-16", true),
    ("UTF-16BE", true),
    ("UTF-8", false),
];

/// The ID3v2 versions Tika has a handler for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Version {
    V22,
    V23,
    V24,
}

/// An ID3v2 tag: its major version and the frames it holds.
pub(super) struct Frame {
    pub(super) major_version: Option<u8>,
    data: Vec<u8>,
}

/// `readFully`: `length` bytes, or as many as there are where short data is not fatal.
fn read_fully(stream: &mut Stream, length: i64, short_is_fatal: bool) -> Result<Vec<u8>, Thrown> {
    if length > MAX_RECORD_SIZE {
        return Err(format!(
            "IOException: Record size ({length} bytes) is larger than the allowed record size: \
             {MAX_RECORD_SIZE}"
        ));
    }
    let length =
        usize::try_from(length).map_err(|_| format!("NegativeArraySizeException: {length}"))?;
    let bytes = stream.read_up_to(length);
    if bytes.len() < length && short_is_fatal {
        return Err(format!(
            "IOException: Tried to read {length} bytes, but only {} bytes present",
            bytes.len()
        ));
    }
    Ok(bytes)
}

fn int(bytes: &[u8]) -> i32 {
    i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn seven_bits_int(bytes: &[u8]) -> i32 {
    bytes[..4]
        .iter()
        .fold(0, |value, &byte| value << 7 | i32::from(byte & 0x7F))
}

impl Frame {
    /// `createFrameIfPresent`: the tag at the start of the stream, if one starts there; what
    /// was read pushed back otherwise, the end of the stream as `0xFF`.
    pub(super) fn if_present(stream: &mut Stream) -> Result<Option<Self>, Thrown> {
        let as_pushed = |byte: Option<u8>| byte.unwrap_or(0xFF);
        let start = [stream.read(), stream.read(), stream.read()];
        if start != [Some(b'I'), Some(b'D'), Some(b'3')] {
            stream.unread(&start.map(as_pushed));
            return Ok(None);
        }
        let (major, minor) = (stream.read(), stream.read());
        if major.is_none() || minor.is_none() {
            stream.unread(&[b'I', b'D', b'3', as_pushed(major), as_pushed(minor)]);
            return Ok(None);
        }
        let flags = stream.read().map_or(-1, i32::from);
        let length = seven_bits_int(&read_fully(stream, 4, true)?);
        if flags & 0x02 == 0x02 {
            let size = int(&read_fully(stream, 4, true)?);
            read_fully(stream, i64::from(size), true)?;
        }
        let data = read_fully(stream, i64::from(length), false)?;
        Ok(Some(Self {
            major_version: major,
            data,
        }))
    }

    /// The version whose handler takes the tag, if Tika has one.
    pub(super) fn version(&self) -> Option<Version> {
        match self.major_version {
            Some(4) => Some(Version::V24),
            Some(3) => Some(Version::V23),
            Some(2) => Some(Version::V22),
            _ => None,
        }
    }

    /// `RawTagIterator` over the frames: name, size and flag lengths by version, each frame cut
    /// to the data there is.
    fn raw_tags(&self, version: Version) -> Vec<RawTag<'_>> {
        let (name_length, size_length, flag_length) = match version {
            Version::V22 => (3, 3, 0),
            Version::V23 | Version::V24 => (4, 4, 2),
        };
        let data = &self.data;
        let mut tags = Vec::new();
        let mut offset = 0;
        while offset + name_length + size_length + flag_length <= data.len() && data[offset] != 0 {
            let header_size = name_length + size_length + flag_length;
            let name = &data[offset..offset + name_length];
            let size_at = offset + name_length;
            let raw_size = if size_length == 3 {
                i32::from(data[size_at]) << 16
                    | i32::from(data[size_at + 1]) << 8
                    | i32::from(data[size_at + 2])
            } else if version == Version::V24 {
                v24_frame_size(data, offset, header_size, name_length)
            } else {
                int(&data[size_at..])
            };
            let copy_from = offset + header_size;
            let available = i32::try_from(data.len() - copy_from).unwrap_or(i32::MAX);
            let size = raw_size.min(available).max(0) as usize;
            tags.push(RawTag {
                name,
                data: &data[copy_from..copy_from + size],
            });
            offset += header_size + size;
        }
        tags
    }

    /// What the version's handler takes from the tag.
    pub(super) fn tags(&self, version: Version) -> Result<Tags, Thrown> {
        let mut tags = Tags::default();
        for tag in self.raw_tags(version) {
            let Some(field) = Field::of(version, tag.name) else {
                continue;
            };
            let text = || tag_string(tag.data);
            match field {
                Field::Title => tags.title = Some(text()),
                Field::Artist => tags.artist = Some(text()),
                Field::AlbumArtist => tags.album_artist = Some(text()),
                Field::Album => tags.album = Some(text()),
                Field::Year => tags.year = Some(text()),
                Field::RecordingTime => {
                    if tags.year.is_none() {
                        tags.year = Some(text());
                    }
                }
                Field::Composer => tags.composer = Some(text()),
                Field::Copyright => tags.copyright = Some(text()),
                Field::Comment => tags.comments.extend(id3_comment(tag.data)),
                Field::Picture => tags.pictures.extend(match version {
                    Version::V22 => v22_picture(tag.data),
                    Version::V23 | Version::V24 => picture_of(tag.data),
                }),
                Field::Track => tags.track_number = Some(text()),
                Field::Disc => tags.disc = Some(text()),
                Field::Compilation => tags.compilation = Some(text()),
                Field::Genre => tags.genre = extract_genre(&text())?,
            }
        }
        Ok(tags)
    }
}

/// The frames an ID3v2 handler takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Year,
    RecordingTime,
    Composer,
    Copyright,
    Comment,
    Picture,
    Track,
    Disc,
    Compilation,
    Genre,
}

impl Field {
    /// The field of the frame `name` to the handler of `version`.
    fn of(version: Version, name: &[u8]) -> Option<Self> {
        Some(match (version, name) {
            (Version::V22, b"TT2") | (Version::V23 | Version::V24, b"TIT2") => Self::Title,
            (Version::V22, b"TP1") | (Version::V23 | Version::V24, b"TPE1") => Self::Artist,
            (Version::V22, b"TP2") | (Version::V23 | Version::V24, b"TPE2") => Self::AlbumArtist,
            (Version::V22, b"TAL") | (Version::V23 | Version::V24, b"TALB") => Self::Album,
            (Version::V22, b"TYE") | (Version::V23 | Version::V24, b"TYER") => Self::Year,
            (Version::V24, b"TDRC") => Self::RecordingTime,
            (Version::V22, b"TCM") | (Version::V23 | Version::V24, b"TCOM") => Self::Composer,
            (Version::V22, b"TCR") | (Version::V23 | Version::V24, b"TCOP") => Self::Copyright,
            (Version::V22, b"COM") | (Version::V23 | Version::V24, b"COMM") => Self::Comment,
            (Version::V22, b"PIC") | (Version::V23 | Version::V24, b"APIC") => Self::Picture,
            (Version::V22, b"TRK") | (Version::V23 | Version::V24, b"TRCK") => Self::Track,
            (Version::V22, b"TPA") | (Version::V23 | Version::V24, b"TPOS") => Self::Disc,
            (Version::V23 | Version::V24, b"TCMP") => Self::Compilation,
            (Version::V22, b"TCO") | (Version::V23 | Version::V24, b"TCON") => Self::Genre,
            _ => return None,
        })
    }
}

/// A frame of a tag: its name and data.
struct RawTag<'a> {
    name: &'a [u8],
    data: &'a [u8],
}

/// What an ID3 handler keeps of its tag, `ID3Tags`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Tags {
    pub(super) title: Option<String>,
    pub(super) artist: Option<String>,
    pub(super) album: Option<String>,
    pub(super) year: Option<String>,
    pub(super) composer: Option<String>,
    pub(super) copyright: Option<String>,
    pub(super) genre: Option<String>,
    pub(super) track_number: Option<String>,
    pub(super) album_artist: Option<String>,
    pub(super) disc: Option<String>,
    pub(super) compilation: Option<String>,
    pub(super) comments: Vec<Comment>,
    pub(super) pictures: Vec<Picture>,
}

/// `getV24FrameSize`: the synchsafe size, unless a byte has its top bit set, or the plain
/// size alone leads to a plausible next frame.
fn v24_frame_size(data: &[u8], offset: usize, header_size: usize, name_length: usize) -> i32 {
    let at = offset + name_length;
    let plain = int(&data[at..]);
    if (data[at] | data[at + 1] | data[at + 2] | data[at + 3]) & 0x80 != 0 {
        return plain;
    }
    let synchsafe = seven_bits_int(&data[at..]);
    if synchsafe == plain {
        return synchsafe;
    }
    let next = |size: i32| i64::from(size) + (offset + header_size) as i64;
    if is_plausible_frame_start(data, next(synchsafe)) {
        return synchsafe;
    }
    if is_plausible_frame_start(data, next(plain)) {
        return plain;
    }
    synchsafe
}

/// `isPlausibleFrameStart`: the end of the data, padding, or four capitals and digits.
fn is_plausible_frame_start(data: &[u8], next: i64) -> bool {
    let Ok(next) = usize::try_from(next) else {
        return false;
    };
    if next > data.len() {
        return false;
    }
    if next == data.len() || data[next] == 0 {
        return true;
    }
    data.get(next..next + 4).is_some_and(|name| {
        name.iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

/// The text encoding an encoding byte names.
fn encoding(flag: u8) -> Option<(&'static str, bool)> {
    ENCODINGS.get(usize::from(flag)).copied()
}

/// `getTagString`: a text frame decoded by its encoding byte, or as ISO-8859-1 without one,
/// its trailing NULs dropped.
fn tag_string(data: &[u8]) -> String {
    if data.is_empty() || data == [0] {
        return String::new();
    }
    let (mut text, (charset, double_byte)) = match encoding(data[0]) {
        Some(found) => (&data[1..], found),
        None => (data, ENCODINGS[0]),
    };
    if double_byte {
        while text.len() >= 2 && text.ends_with(&[0, 0]) {
            text = &text[..text.len() - 2];
        }
    } else {
        while text.last() == Some(&0) {
            text = &text[..text.len() - 1];
        }
    }
    if text.is_empty() || (charset == "UTF-16" && text.len() == 2 && has_bom(text)) {
        return String::new();
    }
    decode(text, charset)
}

/// `decodeText`: UTF-16 without a byte order mark read in the order its zero bytes suggest.
fn decode(bytes: &[u8], charset: &str) -> String {
    let charset = if charset == "UTF-16" && !has_bom(bytes) {
        let (pairs, _) = bytes.as_chunks::<2>();
        let big = pairs.iter().filter(|pair| pair[0] == 0).count();
        let little = pairs.iter().filter(|pair| pair[1] == 0).count();
        if little > big {
            "UTF-16LE"
        } else {
            "UTF-16BE"
        }
    } else {
        charset
    };
    Charset::named(charset)
        .decode(bytes)
        .expect("the JDK decoders decode the ID3 encodings")
}

fn has_bom(bytes: &[u8]) -> bool {
    matches!(bytes, [0xFF, 0xFE, ..] | [0xFE, 0xFF, ..])
}

/// `getComment`: the language, then the description up to its terminator and the text after
/// it, or all of it as text without one.
fn id3_comment(data: &[u8]) -> Option<Comment> {
    if data.len() < 4 {
        return None;
    }
    let (charset, double_byte) = encoding(data[0])?;
    let language = java::latin1(&data[1..4]);
    let body = &data[4..];
    let mut description = None;
    let mut text_start = None;
    let mut i = 0;
    while i < body.len() {
        if double_byte && i + 1 < body.len() && body[i] == 0 && body[i + 1] == 0 {
            if i + 2 < body.len() && body[i + 2] == 0 {
                i += 1;
            }
            text_start = Some(i + 2);
            description = Some(decode(&body[..i], charset));
            break;
        }
        if !double_byte && body[i] == 0 {
            text_start = Some(i + 1);
            description = Some(decode(&body[..i], charset));
            break;
        }
        i += 1;
    }
    let text = match text_start {
        Some(start) => decode(body.get(start..).unwrap_or_default(), charset),
        None => decode(body, charset),
    };
    Some(Comment {
        language: Some(language),
        description,
        text: Some(text),
    })
}

/// `getPicture`: the MIME type up to its NUL, the picture type, then the description and data.
fn picture_of(data: &[u8]) -> Option<Picture> {
    if data.len() < 3 {
        return None;
    }
    let found = encoding(data[0])?;
    let mime_end = 1 + data[1..].iter().position(|&byte| byte == 0)?;
    if mime_end + 1 >= data.len() {
        return None;
    }
    let mime_type = Some(java::latin1(&data[1..mime_end])).filter(|mime| !mime.is_empty());
    picture_with_description(data, mime_end + 2, found, mime_type, data[mime_end + 1])
}

/// `getV22Picture`: the image format, the picture type, then the description and data.
fn v22_picture(data: &[u8]) -> Option<Picture> {
    if data.len() < 5 {
        return None;
    }
    let found = encoding(data[0])?;
    let mime_type = match &data[1..4] {
        b"PNG" => Some("image/png".to_owned()),
        b"JPG" => Some("image/jpeg".to_owned()),
        b"-->" => return None,
        _ => None,
    };
    picture_with_description(data, 5, found, mime_type, data[4])
}

/// `getPictureWithDescription`: the description up to its terminator, then the picture's data,
/// which must not be empty.
fn picture_with_description(
    data: &[u8],
    start: usize,
    (charset, double_byte): (&str, bool),
    mime_type: Option<String>,
    kind: u8,
) -> Option<Picture> {
    let end = data.len();
    let mut found = None;
    if double_byte {
        let mut i = start;
        while i + 1 < end {
            if data[i] == 0 && data[i + 1] == 0 {
                found = Some((i, i + 2));
                break;
            }
            i += 2;
        }
    } else {
        found = data[start.min(end)..]
            .iter()
            .position(|&byte| byte == 0)
            .map(|at| (start + at, start + at + 1));
    }
    let (description_end, data_start) = found?;
    if data_start >= end {
        return None;
    }
    Some(Picture {
        kind: i64::from(kind),
        mime_type,
        description: Some(decode(&data[start..description_end], charset)),
        data: data[data_start..].to_vec(),
    })
}

/// `ID3v22Handler.extractGenre`: a genre as written, the one a `(number)` names, or what comes
/// before it where the number names none; `None` for a closing parenthesis alone or before the
/// opening one, and what Java throws for a closing one without an opening one before it.
pub(super) fn extract_genre(raw: &str) -> Result<Option<String>, Thrown> {
    let open = raw.find('(');
    let close = raw.find(')');
    match (open, close) {
        (None, None) => Ok(Some(raw.to_owned())),
        (None, Some(_)) => Err("StringIndexOutOfBoundsException: begin 0, end -1".to_owned()),
        (Some(open), Some(close)) if open < close => {
            let before = java::trim(&raw[..open]).to_owned();
            let named = java::parse_int(&raw[open + 1..close])
                .and_then(|id| usize::try_from(id).ok())
                .and_then(|id| super::GENRES.get(id));
            Ok(Some(named.map_or(before, |genre| (*genre).to_owned())))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_tag_strings_as_tika_does() {
        assert_eq!(tag_string(b"\x00Abba\x00\x00"), "Abba");
        assert_eq!(tag_string(b"\x01\xFF\xFEA\x00\x00\x00"), "A");
        assert_eq!(tag_string(b"\x01A\x00B\x00"), "AB");
        assert_eq!(tag_string(b"\x03\xC3\xA6"), "\u{e6}");
        assert_eq!(tag_string(b"Plain"), "Plain");
        assert_eq!(tag_string(b"\x00"), "");
    }

    #[test]
    fn extracts_genres_as_tika_does() {
        assert_eq!(extract_genre("Rock"), Ok(Some("Rock".to_owned())));
        assert_eq!(extract_genre("(17)"), Ok(Some("Rock".to_owned())));
        assert_eq!(extract_genre("Mine (999)"), Ok(Some("Mine".to_owned())));
        assert_eq!(extract_genre(")("), Ok(None));
        assert!(extract_genre("x)").is_err());
    }

    #[test]
    fn reads_comments_and_pictures_as_tika_does() {
        let comment = id3_comment(b"\x00engdesc\x00text").unwrap();
        assert_eq!(comment.language.as_deref(), Some("eng"));
        assert_eq!(comment.description.as_deref(), Some("desc"));
        assert_eq!(comment.text.as_deref(), Some("text"));
        let picture = picture_of(b"\x00image/png\x00\x03cover\x00PNGDATA").unwrap();
        assert_eq!(picture.mime_type.as_deref(), Some("image/png"));
        assert_eq!(picture.kind, 3);
        assert_eq!(picture.description.as_deref(), Some("cover"));
        assert_eq!(picture.data, b"PNGDATA");
    }
}
