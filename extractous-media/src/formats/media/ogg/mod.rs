//! Ogg Vorbis, Opus and FLAC files, as Tika's `VorbisParser`, `OpusParser` and `FlacParser`
//! read what vorbis-java reads of them ([`streams`]): the stream info, the Vorbis comments as
//! metadata and paragraphs, their `metadata_block_picture` pictures (and a native FLAC file's
//! picture blocks) as embedded documents, and the duration.

mod packets;
mod streams;

use std::collections::HashSet;

use streams::{BlockHeader, Comments};

use super::{extract_pictures, NumberAndTotal, Picture};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::set_nullable;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::java;
use crate::java::decimal::Rounding;
use crate::java::decimal_format::DecimalFormat;
use crate::text::charset::Charset;

const METADATA_BLOCK_PICTURE: &str = "metadata_block_picture";

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

fn thrown(thrown: String) -> Decline<'static> {
    Decline::Fails(Error::ParseError(thrown))
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
        let vorbis = streams::vorbis(input, pages).map_err(thrown)?;
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
        let opus = streams::opus(input, pages).map_err(thrown)?;
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
    while let Some(header) = BlockHeader::read(&input[at..]) {
        at += streams::BLOCK_HEADER;
        let Some(block) = input.get(at..at + header.length) else {
            break;
        };
        at += header.length;
        if header.kind == streams::PICTURE {
            pictures.extend(picture_block(block));
        }
        if header.last {
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
        let flac = streams::flac(input, pages).map_err(thrown)?;
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

    use super::*;
    use extractous_core::large::{bodies_past_limit, LargeFile};

    /// Vorbis comments with a vendor, after `header`.
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

    fn flac_block(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut block = vec![kind];
        block.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes()[1..]);
        block.extend_from_slice(data);
        block
    }

    /// An Ogg page of one stream, of `data` cut by the `lacing` values.
    fn laced_page(flags: u8, granule: u64, sequence: u32, lacing: &[u8], data: &[u8]) -> Vec<u8> {
        let mut page = b"OggS\x00".to_vec();
        page.push(flags);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&0x1234u32.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&[0; 4]);
        page.push(u8::try_from(lacing.len()).unwrap());
        page.extend_from_slice(lacing);
        page.extend_from_slice(data);
        page
    }

    /// An Ogg page of one stream holding `packets`, each under 255 bytes.
    fn page(flags: u8, granule: u64, sequence: u32, packets: &[&[u8]]) -> Vec<u8> {
        let lacing: Vec<u8> = packets
            .iter()
            .map(|packet| u8::try_from(packet.len()).unwrap())
            .collect();
        laced_page(flags, granule, sequence, &lacing, &packets.concat())
    }

    /// An Opus identification header of a stereo stream with a pre-skip of 312 samples.
    fn opus_head() -> Vec<u8> {
        let mut head = b"OpusHead\x01\x02".to_vec();
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&48000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]);
        head
    }

    /// A FLAC stream info block's data: 44.1 kHz stereo of `samples` samples.
    fn stream_info(samples: u32) -> Vec<u8> {
        let mut info = vec![0u8; 34];
        info[10..14].copy_from_slice(&[0x0A, 0xC4, 0x42, 0xF0]);
        info[14..18].copy_from_slice(&samples.to_be_bytes());
        info
    }

    /// A file too large to read whole of `headers`, the pages of a stream's headers, then one
    /// audio packet over every page after them, ending on one of granule position `granule`:
    /// an audio packet as large as the file, which is never joined.
    fn long_ogg(name: &str, headers: &[u8], granule: u64) -> LargeFile {
        let fragment = vec![0u8; 255 * 255];
        let continued = laced_page(0, u64::MAX, 2, &[255; 255], &fragment);
        let count = bodies_past_limit(continued.len());
        let last = laced_page(4, granule, 3, &[1], &[0]);
        LargeFile::new(name, headers, count, |_| continued.clone(), &last)
    }

    /// The pages of a Vorbis stream's headers, then the audio after them, as large as the file,
    /// are read in place, their pages released as they are read, in memory that does not grow
    /// with the file.
    #[test]
    fn extracts_vorbis_files_too_large_to_read_whole_in_bounded_memory() {
        let mut info = b"\x01vorbis".to_vec();
        info.extend_from_slice(&0u32.to_le_bytes());
        info.push(2);
        info.extend_from_slice(&44100u32.to_le_bytes());
        info.extend_from_slice(&[0; 4]);
        info.extend_from_slice(&128_000u32.to_le_bytes());
        info.extend_from_slice(&[0; 4]);
        info.extend_from_slice(&[0xB8, 1]);
        let mut tags = comments(b"\x03vorbis", &["TITLE=Lang sang"]);
        tags.push(1);
        let setup = [b"\x05vorbis".as_slice(), &[0; 16]].concat();
        let headers = [page(2, 0, 0, &[&info]), page(0, 0, 1, &[&tags, &setup])].concat();
        let file = long_ogg("long.ogg", &headers, 44100 * 3600);
        let (text, metadata) = file.extract(VorbisParser.name());
        assert!(text.contains("Lang sang"), "{text}");
        assert_eq!(metadata["audio:bitrate"], ["128000"]);
        assert_eq!(metadata["xmpDM:duration"], ["3600.0"]);
    }

    /// The same of an Opus stream.
    #[test]
    fn extracts_opus_files_too_large_to_read_whole_in_bounded_memory() {
        let tags = comments(b"OpusTags", &["ALBUM=Album"]);
        let headers = [page(2, 0, 0, &[&opus_head()]), page(0, 0, 1, &[&tags])].concat();
        let file = long_ogg("long.opus", &headers, 312 + 48000 * 3600);
        let (text, metadata) = file.extract(OpusParser.name());
        assert!(text.contains("Album"), "{text}");
        assert_eq!(metadata["xmpDM:duration"], ["3600.0"]);
    }

    /// A native FLAC file is read as far as its metadata blocks, in place, whatever the audio
    /// after them.
    #[test]
    fn extracts_flac_files_too_large_to_read_whole_in_bounded_memory() {
        let mut flac = b"fLaC".to_vec();
        flac.extend(flac_block(0, &stream_info(44100 * 3600)));
        flac.extend(flac_block(4, &comments(b"", &["TITLE=Lang sang"])));
        flac.extend(flac_block(0x81, &[0; 1024]));
        let frames = vec![0u8; 1 << 20];
        let count = bodies_past_limit(frames.len());
        let file = LargeFile::new("long.flac", &flac, count, |_| frames.clone(), b"");
        let (text, metadata) = file.extract(FlacParser.name());
        assert!(text.contains("Lang sang"), "{text}");
        assert_eq!(metadata["xmpDM:duration"], ["3600.0"]);
    }

    /// A FLAC frame's sync code and header, which read as a metadata block header give a type of
    /// 127 and a length past the file.
    const FLAC_FRAME: &[u8] = &[0xFF, 0xF8, 0x69, 0x08, 0x00, 0x00];

    /// A native FLAC file and an Ogg FLAC file, each with a comment block, titled `title`, that
    /// is flagged as the last metadata block, then audio.
    fn flac_files_with_last_comments(title: &str) -> [Vec<u8>; 2] {
        let info = stream_info(44100);
        let tags = flac_block(0x84, &comments(b"", &[&format!("TITLE={title}")]));
        let mut native = b"fLaC".to_vec();
        native.extend(flac_block(0, &info));
        native.extend_from_slice(&tags);
        native.extend_from_slice(FLAC_FRAME);
        let mut head = b"\x7fFLAC\x01\x00\x00\x01fLaC".to_vec();
        head.extend(flac_block(0, &info));
        let mut ogg = page(2, 0, 0, &[&head]);
        ogg.extend(page(0, 0, 1, &[&tags]));
        ogg.extend(page(4, 44100, 2, &[FLAC_FRAME]));
        [native, ogg]
    }

    /// A comment block flagged as the last metadata block is read as comments, and nothing after
    /// it as a metadata block, where vorbis-java matches the block's type with the flag and so
    /// reads no comments (#182).
    #[test]
    fn reads_flac_comments_flagged_as_the_last_metadata_block() {
        for (file, ogg_version) in flac_files_with_last_comments("Sidste blok")
            .iter()
            .zip([None, Some((1, 0))])
        {
            let flac = streams::flac(file, Pages::IN_MEMORY).unwrap();
            assert_eq!(flac.ogg_version, ogg_version);
            assert_eq!(flac.info.sample_rate, 44100);
            let comments = flac.comments.expect("the comment block");
            assert_eq!(comments.all.get("title").unwrap(), &["Sidste blok"]);
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
