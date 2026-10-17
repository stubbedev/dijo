//! Ogg Vorbis, Opus and FLAC files, as Tika's `VorbisParser`, `OpusParser` and `FlacParser`
//! read what vorbis-java reads of them ([`streams`]): the stream info, the Vorbis comments as
//! metadata and paragraphs, their `metadata_block_picture` pictures (and a native FLAC file's
//! picture blocks) as embedded documents, and the duration.

mod packets;
mod streams;

use std::collections::HashSet;

use streams::Comments;

use super::{extract_pictures, NumberAndTotal, Picture};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::set_nullable;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;
use crate::java::decimal::Rounding;
use crate::java::decimal_format::DecimalFormat;
use crate::text::charset::Charset;

const METADATA_BLOCK_PICTURE: &str = "metadata_block_picture";
const PICTURE_BLOCK_TYPE: u8 = 6;

/// A stream's comments as `OggAudioParser.fields` reads them: by lower-cased name, in the order
/// of the comment packet where `rawFields` reads it, else in the order of vorbis-java's map.
type Fields = Vec<(String, Vec<String>)>;

fn fields(comments: &Comments) -> Fields {
    if let Some(fields) = raw_fields(comments) {
        return fields;
    }
    let mut fields: Fields = Vec::new();
    for (name, values) in comments.all.iter() {
        push_all(
            &mut fields,
            java::to_lower_case(name),
            values.iter().cloned(),
        );
    }
    fields
}

/// `computeIfAbsent(name, …).addAll(values)` on a `LinkedHashMap`.
fn push_all(fields: &mut Fields, name: String, values: impl Iterator<Item = String>) {
    match fields.iter_mut().find(|(existing, _)| *existing == name) {
        Some((_, existing)) => existing.extend(values),
        None => fields.push((name, values.collect())),
    }
}

/// `rawFields`: the comments read again from their packet past the vendor string, which must
/// stand after a header of 0, 4, 7 or 8 bytes; `None` where they do not read.
fn raw_fields(comments: &Comments) -> Option<Fields> {
    let data = &comments.data;
    let vendor = comments.vendor.as_bytes();
    let int = |at: usize| -> Option<i32> {
        data.get(at..at + 4)
            .map(|bytes| i32::from_le_bytes(bytes.try_into().expect("four bytes")))
    };
    let offset = [0usize, 4, 7, 8].into_iter().find_map(|header| {
        let end = header + 4 + vendor.len();
        (end <= data.len()
            && int(header) == i32::try_from(vendor.len()).ok()
            && &data[header + 4..end] == vendor)
            .then_some(end)
    })?;
    let count = int(offset)?;
    if count < 0 {
        return None;
    }
    let mut at = offset + 4;
    let mut fields: Fields = Vec::new();
    for _ in 0..count {
        let length = usize::try_from(int(at)?).ok()?;
        at += 4;
        let bytes = data.get(at..at.checked_add(length)?)?;
        at += length;
        let comment = java::utf8(bytes);
        let Some((name, value)) = comment.split_once('=') else {
            continue;
        };
        push_all(
            &mut fields,
            java::to_lower_case(name),
            std::iter::once(value.to_owned()),
        );
    }
    Some(fields)
}

/// `all(fields, name)`.
fn all<'f>(fields: &'f Fields, name: &str) -> &'f [String] {
    fields
        .iter()
        .find(|(existing, _)| existing == name)
        .map_or(&[], |(_, values)| values.as_slice())
}

/// `first(fields, name)`.
fn first<'f>(fields: &'f Fields, name: &str) -> Option<&'f str> {
    all(fields, name).first().map(String::as_str)
}

/// `firstPositiveInteger`: the first value of the names that reads as a positive `int`.
fn first_positive_integer(fields: &Fields, names: [&str; 2]) -> Option<i32> {
    names.iter().find_map(|name| {
        all(fields, name)
            .iter()
            .find_map(|value| java::parse_int(java::trim(value)).filter(|&value| value > 0))
    })
}

/// `extractChannelInfo`.
fn set_channel_type(metadata: &mut TikaMetadata, channels: i32) {
    let channel_type = match channels {
        1 => "Mono",
        2 => "Stereo",
        5 => "5.1",
        7 => "7.1",
        _ => return,
    };
    metadata.set("xmpDM:audioChannelType", channel_type);
}

/// Sets a track or disc number and its total from `NumberAndTotal`.
fn set_numbers(metadata: &mut TikaMetadata, value: &str, number_name: &str, total_name: &str) {
    if let Some(numbers) = NumberAndTotal::parse(value) {
        if let Some(number) = numbers.number {
            metadata.set(number_name, number.to_string());
        }
        if let Some(total) = numbers.total {
            metadata.set(total_name, total.to_string());
        }
    }
}

/// `extractComments`: the comments as metadata and paragraphs; the pictures they hold.
fn extract_comments(comments: &Comments, xhtml: &mut Xhtml) -> Result<Vec<Picture>, Failure> {
    let fields = fields(comments);
    let title = first(&fields, "title");
    let artist = first(&fields, "artist");
    let album = first(&fields, "album");
    let track = first(&fields, "tracknumber");
    let date = first(&fields, "date");
    let genre = first(&fields, "genre");
    let metadata = xhtml.metadata_mut();
    set_nullable(metadata, "dc:title", title);
    set_nullable(metadata, "dc:creator", artist);
    set_nullable(metadata, "xmpDM:artist", artist);
    set_nullable(metadata, "xmpDM:album", album);
    set_nullable(metadata, "xmpDM:genre", genre);
    set_nullable(metadata, "xmpDM:releaseDate", date);
    metadata.set("xmp:CreatorTool", comments.vendor.clone());
    metadata.set("vorbis:vendor", comments.vendor.clone());
    if let Some(copyright) = first(&fields, "copyright") {
        metadata.set("xmpDM:copyright", copyright);
    }
    for comment in all(&fields, "comment") {
        metadata.add("xmpDM:logComment", comment.clone());
    }
    let done: HashSet<&str> = [
        "title",
        "artist",
        "album",
        "genre",
        "date",
        "tracknumber",
        "vendor",
        "comment",
        METADATA_BLOCK_PICTURE,
    ]
    .into_iter()
    .collect();
    for (name, values) in &fields {
        if !done.contains(name.as_str()) {
            for value in values {
                metadata.add_prefixed("vorbis:", name, value.clone());
            }
        }
    }
    xhtml.element("h1", title.unwrap_or_default())?;
    xhtml.element("p", artist.unwrap_or_default())?;
    if let Some(track) = track {
        let album = album.unwrap_or("null");
        xhtml.element("p", &format!("{album}, track {track}"))?;
        let metadata = xhtml.metadata_mut();
        metadata.set("audio:raw-track-number", track);
        set_numbers(metadata, track, "xmpDM:trackNumber", "audio:track-count");
    } else {
        xhtml.element("p", album.unwrap_or_default())?;
    }
    let metadata = xhtml.metadata_mut();
    for disc in all(&fields, "discnumber") {
        metadata.set("audio:raw-disc-number", disc.clone());
        set_numbers(metadata, disc, "xmpDM:discNumber", "audio:disc-count");
    }
    if let Some(total) = first_positive_integer(&fields, ["tracktotal", "totaltracks"]) {
        metadata.set("audio:track-count", total.to_string());
    }
    if let Some(total) = first_positive_integer(&fields, ["disctotal", "totaldiscs"]) {
        metadata.set("audio:disc-count", total.to_string());
    }
    xhtml.element("p", date.unwrap_or_default())?;
    for comment in all(&fields, "comment") {
        xhtml.element("p", comment)?;
    }
    xhtml.element("p", genre.unwrap_or_default())?;
    Ok(all(&fields, METADATA_BLOCK_PICTURE)
        .iter()
        .filter_map(|block| java::base64_mime_decode(block))
        .filter_map(|block| picture_block(&block))
        .collect())
}

/// A big-endian `ByteBuffer` over a picture block: reading past its end underflows.
struct Buffer<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Buffer<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let taken = self.bytes.get(self.at..self.at.checked_add(count)?)?;
        self.at += count;
        Some(taken)
    }

    fn int(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    /// `getPrefixedString`: a string of the length before it, which must lie in the buffer.
    fn prefixed(&mut self, charset: &str) -> Option<String> {
        let length = usize::try_from(self.int()?).ok()?;
        Some(
            Charset::named(charset)
                .decode(self.take(length)?)
                .expect("the JDK decoders decode ISO-8859-1 and UTF-8"),
        )
    }
}

/// `PictureBlock.parse`: a FLAC picture block, `None` where it does not read.
fn picture_block(block: &[u8]) -> Option<Picture> {
    let mut buffer = Buffer {
        bytes: block,
        at: 0,
    };
    let kind = buffer.int()?;
    let mime_type = buffer.prefixed("ISO-8859-1").filter(|mime| mime != "-->")?;
    let description = buffer.prefixed("UTF-8")?;
    buffer.take(16)?;
    let length = usize::try_from(buffer.int()?)
        .ok()
        .filter(|&length| length > 0)?;
    let data = buffer.take(length)?;
    Some(Picture {
        kind: i64::from(kind),
        mime_type: Some(mime_type),
        description: Some(description),
        data: data.to_vec(),
    })
}

/// `extractDuration`: a positive duration as `0.0#` metadata and as a paragraph of minutes and
/// seconds, with the hours before them where there are any.
fn extract_duration(duration: f64, xhtml: &mut Xhtml) -> Result<(), Failure> {
    if duration <= 0.0 {
        return Ok(());
    }
    let format =
        DecimalFormat::new("0.0#", Rounding::HalfEven).expect("the duration pattern compiles");
    xhtml
        .metadata_mut()
        .add("xmpDM:duration", format.format_f64(duration));
    let total = duration as i64;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    let text = if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    };
    Ok(xhtml.element("p", &text)?)
}

fn thrown(thrown: String) -> Declined {
    Declined::Fails(Error::ParseError(thrown))
}

pub struct VorbisParser;

impl Parser for VorbisParser {
    fn name(&self) -> &'static str {
        "extractous::VorbisParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.ogg.VorbisParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["audio/vorbis"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let vorbis = streams::vorbis(input).map_err(thrown)?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "audio/vorbis");
            metadata.set("xmpDM:audioCompressor", "Vorbis");
            let info = vorbis.info;
            metadata.set("xmpDM:audioSampleRate", (info.rate as i32).to_string());
            metadata.add("ogg:codec-version", format!("Vorbis {}", info.version));
            let (upper, nominal, lower) =
                (info.bitrate_upper, info.bitrate_nominal, info.bitrate_lower);
            if nominal > 0 || upper > 0 || lower > 0 {
                if nominal > 0 {
                    metadata.set("audio:bitrate", nominal.to_string());
                } else if upper > 0 && upper == lower {
                    metadata.set("audio:bitrate", upper.to_string());
                }
                let fixed = (nominal > 0 && upper == nominal && lower == nominal)
                    || (nominal <= 0 && upper > 0 && upper == lower);
                metadata.set("audio:is-variable-bitrate", (!fixed).to_string());
            }
            set_channel_type(metadata, info.channels);
            let pictures = extract_comments(&vorbis.comments, xhtml)?;
            extract_pictures(&pictures, xhtml)?;
            extract_duration(vorbis.duration, xhtml)
        }))
    }
}

pub struct OpusParser;

impl Parser for OpusParser {
    fn name(&self) -> &'static str {
        "extractous::OpusParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.ogg.OpusParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["audio/opus"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let opus = streams::opus(input).map_err(thrown)?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "audio/opus");
            metadata.set("xmpDM:audioCompressor", "Opus");
            let info = opus.info;
            metadata.set("xmpDM:audioSampleRate", (info.rate as i32).to_string());
            metadata.add(
                "ogg:codec-version",
                format!("Opus {}.{}", info.major_version, info.minor_version),
            );
            set_channel_type(metadata, info.channels);
            let pictures = extract_comments(&opus.comments, xhtml)?;
            extract_pictures(&pictures, xhtml)?;
            extract_duration(opus.duration, xhtml)
        }))
    }
}

pub struct FlacParser;

/// `FlacParser.readNativePictures`: the picture blocks of a native FLAC file's metadata, up to
/// the last block or where the file ends.
fn native_pictures(input: &[u8]) -> Vec<Picture> {
    let mut pictures = Vec::new();
    if !input.starts_with(b"fLaC") {
        return pictures;
    }
    let mut at = 4;
    while let Some(header) = input.get(at..at + 4) {
        at += 4;
        let last = header[0] & 0x80 != 0;
        let length =
            usize::from(header[1]) << 16 | usize::from(header[2]) << 8 | usize::from(header[3]);
        let Some(block) = input.get(at..at + length) else {
            break;
        };
        at += length;
        if header[0] & 0x7F == PICTURE_BLOCK_TYPE {
            pictures.extend(picture_block(block));
        }
        if last {
            break;
        }
    }
    pictures
}

impl Parser for FlacParser {
    fn name(&self) -> &'static str {
        "extractous::FlacParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.ogg.FlacParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["audio/x-flac", "audio/x-oggflac"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let flac = streams::flac(input).map_err(thrown)?;
        let Some(comments) = flac.comments.clone() else {
            return Err(thrown(
                "NullPointerException: a FLAC file without comments".to_owned(),
            ));
        };
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("xmpDM:audioCompressor", "FLAC");
            let info = flac.info;
            metadata.set("xmpDM:audioSampleRate", info.sample_rate.to_string());
            set_channel_type(metadata, info.channels);
            match flac.ogg_version {
                Some((major, minor)) => {
                    metadata.add("ogg:codec-version", format!("Flac {major}.{minor}"));
                    metadata.set("Content-Type", "audio/x-oggflac");
                }
                None => metadata.set("Content-Type", "audio/x-flac"),
            }
            let mut pictures = extract_comments(&comments, xhtml)?;
            if flac.ogg_version.is_none() {
                pictures.extend(native_pictures(input));
            }
            extract_pictures(&pictures, xhtml)?;
            if info.samples > 0 && info.sample_rate > 0 {
                extract_duration(info.samples as f64 / f64::from(info.sample_rate), xhtml)?;
            }
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};

    /// Vorbis comments with a vendor, after `header`.
    #[cfg(feature = "tika")]
    fn comments(header: &[u8], entries: &[&str]) -> Vec<u8> {
        let mut data = header.to_vec();
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(b"test");
        data.extend_from_slice(&u32::try_from(entries.len()).unwrap().to_le_bytes());
        for entry in entries {
            data.extend_from_slice(&u32::try_from(entry.len()).unwrap().to_le_bytes());
            data.extend_from_slice(entry.as_bytes());
        }
        data
    }

    /// A FLAC picture block of text, which the extractor parses without OCR.
    #[cfg(feature = "tika")]
    fn picture(kind: i32, text: &str) -> Vec<u8> {
        let mut block = kind.to_be_bytes().to_vec();
        block.extend_from_slice(&10i32.to_be_bytes());
        block.extend_from_slice(b"text/plain");
        block.extend_from_slice(&4i32.to_be_bytes());
        block.extend_from_slice(b"note");
        block.extend_from_slice(&[0; 16]);
        block.extend_from_slice(&i32::try_from(text.len()).unwrap().to_be_bytes());
        block.extend_from_slice(text.as_bytes());
        block
    }

    #[cfg(feature = "tika")]
    fn flac_block(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut block = vec![kind];
        block.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes()[1..]);
        block.extend_from_slice(data);
        block
    }

    /// An Ogg page of one stream holding `packets`, each under 255 bytes.
    #[cfg(feature = "tika")]
    fn page(flags: u8, granule: u64, sequence: u32, packets: &[&[u8]]) -> Vec<u8> {
        let mut page = b"OggS\x00".to_vec();
        page.push(flags);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&0x1234u32.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&[0; 4]);
        page.push(u8::try_from(packets.len()).unwrap());
        for packet in packets {
            page.push(u8::try_from(packet.len()).unwrap());
        }
        for packet in packets {
            page.extend_from_slice(packet);
        }
        page
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_flac_and_opus_files_as_tikas_path_does() {
        let scratch = crate::formats::parity::Scratch::new("ogg");
        let mut info = vec![0u8; 34];
        info[10..18].copy_from_slice(&[0x0A, 0xC4, 0x42, 0xF0, 0x00, 0x04, 0xEB, 0xA0]);
        let tags = comments(
            b"",
            &[
                "TITLE=Bølger",
                "Artist=Kunstner",
                "TRACKNUMBER=2/9",
                "comment=en kommentar",
                "DISCNUMBER=1",
                "Label=Mærke",
                "noequals",
            ],
        );
        let mut flac = b"fLaC".to_vec();
        flac.extend(flac_block(0, &info));
        flac.extend(flac_block(4, &tags));
        flac.extend(flac_block(6, &picture(3, "front cover")));
        flac.extend(flac_block(0x86, &picture(4, "back cover")));
        let mut head = b"OpusHead\x01\x02".to_vec();
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&48000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]);
        let opus_tags = comments(b"OpusTags", &["ALBUM=Album", "DATE=2024"]);
        let mut opus = page(2, 0, 0, &[&head]);
        opus.extend(page(0, 0, 1, &[&opus_tags]));
        opus.extend(page(0, 48312, 2, &[b"\xFC\x00", b"\xFC\x00"]));
        opus.extend(page(4, 96312, 3, &[b"\xFC\x00"]));
        let read = [
            scratch.write("tagged.flac", &flac),
            scratch.write("tagged.opus", &opus),
        ];
        for (parser, tika_name, path) in [
            (
                &FlacParser as &dyn Parser,
                "org.apache.tika.parser.ogg.FlacParser",
                &read[0],
            ),
            (
                &OpusParser,
                "org.apache.tika.parser.ogg.OpusParser",
                &read[1],
            ),
        ] {
            let (parsed, deferred) = documents(parser, &[scratch.directory()]);
            assert!(deferred.is_empty(), "{deferred:?}");
            assert_eq!(parsed, std::slice::from_ref(path));
            assert_same_as_tika(&[path.as_path()], tika_name);
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_ogg_files_as_tikas_path_does() {
        for (parser, tika_name, count) in [
            (
                &VorbisParser as &dyn Parser,
                "org.apache.tika.parser.ogg.VorbisParser",
                4,
            ),
            (&OpusParser, "org.apache.tika.parser.ogg.OpusParser", 1),
            (&FlacParser, "org.apache.tika.parser.ogg.FlacParser", 5),
        ] {
            let (parsed, deferred) = documents(parser, &[&conformance_corpus()]);
            assert!(deferred.is_empty(), "{deferred:?}");
            assert_eq!(parsed.len(), count, "{parsed:?}");
            let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
            assert_same_as_tika(&paths, tika_name);
        }
    }

    #[test]
    fn formats_durations_as_tika_does() {
        let format = DecimalFormat::new("0.0#", Rounding::HalfEven).unwrap();
        assert_eq!(format.format_f64(2.0), "2.0");
        assert_eq!(format.format_f64(12.345_678), "12.35");
        assert_eq!(format.format_f64(0.125), "0.12");
    }

    #[test]
    fn reads_picture_blocks_as_tika_does() {
        let mut block = 3i32.to_be_bytes().to_vec();
        block.extend_from_slice(&9i32.to_be_bytes());
        block.extend_from_slice(b"image/png");
        block.extend_from_slice(&5i32.to_be_bytes());
        block.extend_from_slice(b"cover");
        block.extend_from_slice(&[0; 16]);
        block.extend_from_slice(&3i32.to_be_bytes());
        block.extend_from_slice(b"PNG");
        let picture = picture_block(&block).unwrap();
        assert_eq!(picture.kind, 3);
        assert_eq!(picture.data, b"PNG");
        assert!(picture_block(&block[..block.len() - 1]).is_none());
    }
}
