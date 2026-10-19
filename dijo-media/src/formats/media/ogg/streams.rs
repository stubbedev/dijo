//! Vorbis, Opus and FLAC streams as vorbis-java 0.8's `VorbisFile`, `OpusFile` and `FlacFile`
//! read them for Tika: the stream's info header, its Vorbis-style comments, and for Vorbis and
//! Opus the duration `OggAudioStatistics` gives from the last granule position of its audio
//! packets. Where vorbis-java throws, so do these, but on a FLAC comment block flagged as the
//! last metadata block, which these read as comments ([`BlockHeader`], #182).
//!
//! A packet is looked at by its first bytes, which hold every field these read of an audio
//! packet or an identification header; only the comment headers, and FLAC's metadata blocks as
//! far as their lengths go, are read whole.

use super::packets::{Packet, Reader, Thrown, HEAD};
use crate::input::Pages;
use crate::java;
use crate::ordered_map::OrderedMap;

/// A little-endian `int` at `offset`, `IOUtils.getInt4` cast to `int`; past the data an
/// `ArrayIndexOutOfBoundsException`.
fn int4(data: &[u8], offset: i64) -> Result<i32, Thrown> {
    let start = usize::try_from(offset).map_err(|_| out_of_bounds())?;
    let bytes = data.get(start..start + 4).ok_or_else(out_of_bounds)?;
    Ok(i32::from_le_bytes(bytes.try_into().expect("four bytes")))
}

fn out_of_bounds() -> Thrown {
    "ArrayIndexOutOfBoundsException".to_owned()
}

fn byte(data: &[u8], index: usize) -> Result<u8, Thrown> {
    data.get(index).copied().ok_or_else(out_of_bounds)
}

/// `new String(data, offset, length, UTF_8)`: a range outside the data throws.
fn utf8(data: &[u8], offset: i64, length: i32) -> Result<String, Thrown> {
    let range = usize::try_from(offset)
        .ok()
        .zip(usize::try_from(length).ok())
        .and_then(|(start, length)| data.get(start..start.checked_add(length)?))
        .ok_or_else(|| "StringIndexOutOfBoundsException".to_owned())?;
    Ok(java::utf8(range))
}

/// A stream's Vorbis-style comments, `VorbisStyleComments`: the packet they were read from, the
/// vendor, and the comments by their normalised tags, in the order the tags first come, where
/// vorbis-java holds them in a `java.util.HashMap` (#186).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Comments {
    pub(super) data: Vec<u8>,
    pub(super) vendor: String,
    pub(super) all: OrderedMap<Vec<String>>,
}

/// `normaliseTag`: lower-cased, with only the characters from U+0020 to U+007D but `=`.
fn normalise_tag(tag: &str) -> String {
    java::to_lower_case(tag)
        .chars()
        .filter(|&character| (' '..='}').contains(&character) && character != '=')
        .collect()
}

impl Comments {
    /// `VorbisStyleComments(packet, dataBeginsAt)`, the framing bit checked where the format
    /// has one.
    fn read(data: Vec<u8>, begins_at: i64, framing_bit: bool) -> Result<Self, Thrown> {
        let vendor_length = int4(&data, begins_at)?;
        let vendor = utf8(&data, begins_at + 4, vendor_length)?;
        let mut offset = begins_at + 4 + i64::from(vendor_length);
        let count = int4(&data, offset)?;
        offset += 4;
        let mut all = OrderedMap::default();
        for _ in 0..count.max(0) {
            let length = int4(&data, offset)?;
            offset += 4;
            let comment = utf8(&data, offset, length)?;
            offset += i64::from(length);
            if let Some((tag, value)) = comment.split_once('=') {
                let tag = normalise_tag(tag);
                if all.get(&tag).is_none() {
                    all.insert(&tag, Vec::new());
                }
                all.get_mut(&tag)
                    .expect("the tag was just put")
                    .push(value.to_owned());
            }
        }
        if framing_bit {
            if let Some(&0) = usize::try_from(offset).ok().and_then(|at| data.get(at)) {
                return Err("IllegalArgumentException: Framing bit not set, invalid".to_owned());
            }
        }
        Ok(Self { data, vendor, all })
    }
}

/// The Vorbis identification header Tika reads, `VorbisInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VorbisInfo {
    pub(super) version: i32,
    pub(super) channels: i32,
    pub(super) rate: i64,
    pub(super) bitrate_upper: i32,
    pub(super) bitrate_nominal: i32,
    pub(super) bitrate_lower: i32,
}

/// Every field read from a packet's first bytes lies within the [`HEAD`] they hold: Opus's
/// channel mapping, 21 bytes and at most 127 channels; FLAC's stream info, 34 bytes after 17;
/// and Vorbis's identification header, 30 bytes.
const _: () = assert!(21 + i8::MAX as usize <= HEAD && 17 + 34 <= HEAD && 30 <= HEAD);

/// `new VorbisInfo(packet)`, of the packet's first bytes.
fn vorbis_info(data: &[u8]) -> Result<VorbisInfo, Thrown> {
    let version = int4(data, 7)?;
    if version != 0 {
        return Err(format!(
            "IllegalArgumentException: Unsupported vorbis version {version} detected"
        ));
    }
    let info = VorbisInfo {
        version,
        channels: i32::from(byte(data, 11)? as i8),
        rate: i64::from(int4(data, 12)?),
        bitrate_upper: int4(data, 16)?,
        bitrate_nominal: int4(data, 20)?,
        bitrate_lower: int4(data, 24)?,
    };
    byte(data, 28)?;
    if byte(data, 29)? == 0 {
        return Err("IllegalArgumentException: Framing bit not set, invalid".to_owned());
    }
    Ok(info)
}

/// A Vorbis header packet's type, `isVorbisSpecial`.
fn vorbis_header_type(data: &[u8]) -> Option<u8> {
    if data.len() < 16 || !matches!(data[0], 1 | 3 | 5) || &data[1..7] != b"vorbis" {
        return None;
    }
    Some(data[0])
}

/// A Vorbis stream as Tika reads it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Vorbis {
    pub(super) info: VorbisInfo,
    pub(super) comments: Comments,
    pub(super) duration: f64,
}

/// What a `create` of a header the file expects is: the header, or the class that would not
/// cast.
fn expect_header<'a, T>(
    packet: Option<Packet<'a>>,
    read: impl FnOnce(&Packet<'a>) -> Result<Option<T>, Thrown>,
) -> Result<(T, Packet<'a>), Thrown> {
    let packet = packet.ok_or_else(|| "NullPointerException".to_owned())?;
    let header = read(&packet)?.ok_or_else(|| "ClassCastException".to_owned())?;
    Ok((header, packet))
}

/// `new VorbisComments(packet)`.
fn vorbis_comments(packet: &Packet) -> Result<Comments, Thrown> {
    Comments::read(packet.data().into_owned(), 7, true)
}

/// `new VorbisFile(new OggFile(input))` and the statistics of its audio packets, the `pages` of
/// `input` released as they are read.
pub(super) fn vorbis<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Vorbis, Thrown> {
    let mut reader = Reader::new(input, pages);
    let first = loop {
        match reader.next()? {
            None => return Err("IllegalArgumentException: Supplied File is not Vorbis".to_owned()),
            Some(packet)
                if packet.beginning_of_stream
                    && packet.len() > 10
                    && vorbis_header_type(packet.head()).is_some() =>
            {
                break packet;
            }
            Some(_) => {}
        }
    };
    let sid = first.sid;
    let (info, info_packet) = expect_header(Some(first), |packet| {
        match vorbis_header_type(packet.head()) {
            Some(1) => vorbis_info(packet.head()).map(Some),
            Some(3) => vorbis_comments(packet).map(|_| None),
            _ => Ok(None),
        }
    })?;
    let (comments, comments_packet) =
        expect_header(
            reader.next_with_sid(sid)?,
            |packet| match vorbis_header_type(packet.head()) {
                Some(3) => vorbis_comments(packet).map(Some),
                Some(1) => vorbis_info(packet.head()).map(|_| None),
                _ => Ok(None),
            },
        )?;
    let (_, setup_packet) =
        expect_header(
            reader.next_with_sid(sid)?,
            |packet| match vorbis_header_type(packet.head()) {
                Some(5) => Ok(Some(())),
                Some(1) => vorbis_info(packet.head()).map(|_| None),
                Some(3) => vorbis_comments(packet).map(|_| None),
                _ => Ok(None),
            },
        )?;
    for header in [&info_packet, &comments_packet, &setup_packet] {
        header.check_overhead()?;
    }
    let last_granule = last_granule(&mut reader, sid, |packet| {
        match vorbis_header_type(packet.head()) {
            Some(1) => vorbis_info(packet.head()).map(|_| false),
            Some(3) => vorbis_comments(packet).map(|_| false),
            Some(_) => Ok(false),
            None => Ok(true),
        }
    })?;
    let duration = duration(last_granule, 0, f64::from(info.rate as i32));
    Ok(Vorbis {
        info,
        comments,
        duration,
    })
}

/// The greatest granule position of the stream's audio packets, `-1` without any, as
/// `OggAudioStatistics.calculate` reads them; `is_audio` makes a packet of what `create` makes of
/// it, failing where that throws.
fn last_granule(
    reader: &mut Reader,
    sid: i32,
    is_audio: impl Fn(&Packet) -> Result<bool, Thrown>,
) -> Result<i64, Thrown> {
    let mut last = -1;
    while let Some(packet) = reader.next_with_sid(sid)? {
        if !is_audio(&packet)? {
            continue;
        }
        packet.check_overhead()?;
        last = last.max(packet.granule_position);
    }
    Ok(last)
}

/// The duration `OggAudioStatistics` computes: the samples after the pre-skip over the rate,
/// 0 without a positive last granule position.
fn duration(last_granule: i64, pre_skip: i32, sample_rate: f64) -> f64 {
    if last_granule > 0 {
        (last_granule - i64::from(pre_skip)) as f64 / sample_rate
    } else {
        0.0
    }
}

const OPUS_HEAD: &[u8] = b"OpusHead";
const OPUS_TAGS: &[u8] = b"OpusTags";
/// `OpusAudioData.OPUS_GRANULE_RATE`.
const OPUS_GRANULE_RATE: f64 = 48000.0;

/// The Opus identification header Tika reads, `OpusInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OpusInfo {
    pub(super) major_version: i32,
    pub(super) minor_version: i32,
    pub(super) channels: i32,
    pub(super) pre_skip: i32,
    pub(super) rate: i64,
}

/// `isOpusSpecial`: a packet of twelve bytes or more that starts as a header.
fn is_opus_header(data: &[u8]) -> bool {
    data.len() >= 12 && (data.starts_with(OPUS_HEAD) || data.starts_with(OPUS_TAGS))
}

/// `new OpusInfo(packet)`.
fn opus_info(data: &[u8]) -> Result<OpusInfo, Thrown> {
    if !data.starts_with(OPUS_HEAD) {
        return Err("IllegalArgumentException: Invalid type, not a Opus Header".to_owned());
    }
    let version = byte(data, 8)? as i8;
    let major_version = i32::from(version >> 4);
    if major_version != 0 {
        return Err(format!(
            "IllegalArgumentException: Unsupported Opus version {version} at major version \
             {major_version} detected"
        ));
    }
    let channels = i32::from(byte(data, 9)? as i8);
    let pre_skip = i32::from(byte(data, 10)?) | i32::from(byte(data, 11)?) << 8;
    let rate = i64::from(int4(data, 12)?);
    byte(data, 17)?;
    if byte(data, 18)? != 0 {
        byte(data, 20)?;
        let count = usize::try_from(channels)
            .map_err(|_| format!("NegativeArraySizeException: {channels}"))?;
        if data.len() < 21 + count {
            return Err(out_of_bounds());
        }
    }
    Ok(OpusInfo {
        major_version,
        minor_version: i32::from(version & 0xF),
        channels,
        pre_skip,
        rate,
    })
}

/// `new OpusTags(packet)`.
fn opus_tags(packet: &Packet) -> Result<Comments, Thrown> {
    let comments = Comments::read(packet.data().into_owned(), OPUS_TAGS.len() as i64, false)?;
    if !comments.data.starts_with(OPUS_TAGS) {
        return Err("IllegalArgumentException: Invalid type, not a Opus Header".to_owned());
    }
    Ok(comments)
}

/// What `OpusPacketFactory.create` makes of a header packet, failing where it throws: the
/// identification header, the tags, or `None` for audio.
enum OpusPacket {
    Info(OpusInfo),
    Tags(Comments),
    Audio,
}

fn opus_packet(packet: &Packet) -> Result<OpusPacket, Thrown> {
    let head = packet.head();
    if !is_opus_header(head) {
        return Ok(OpusPacket::Audio);
    }
    Ok(match head[4] {
        b'H' => OpusPacket::Info(opus_info(head)?),
        b'T' => OpusPacket::Tags(opus_tags(packet)?),
        _ => OpusPacket::Audio,
    })
}

/// An Opus stream as Tika reads it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Opus {
    pub(super) info: OpusInfo,
    pub(super) comments: Comments,
    pub(super) duration: f64,
}

/// `new OpusFile(new OggFile(input))` and the statistics of its audio packets, the `pages` of
/// `input` released as they are read.
pub(super) fn opus<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Opus, Thrown> {
    let mut reader = Reader::new(input, pages);
    let mut first = None;
    while let Some(packet) = reader.next()? {
        if packet.beginning_of_stream && packet.len() > 10 && is_opus_header(packet.head()) {
            first = Some(packet);
            break;
        }
    }
    let first =
        first.ok_or_else(|| "IllegalArgumentException: Supplied File is not Opus".to_owned())?;
    let sid = first.sid;
    let (info, info_packet) = expect_header(Some(first), |packet| {
        Ok(match opus_packet(packet)? {
            OpusPacket::Info(info) => Some(info),
            _ => None,
        })
    })?;
    let (comments, comments_packet) = expect_header(reader.next_with_sid(sid)?, |packet| {
        Ok(match opus_packet(packet)? {
            OpusPacket::Tags(comments) => Some(comments),
            _ => None,
        })
    })?;
    info_packet.check_overhead()?;
    comments_packet.check_overhead()?;
    let last_granule = last_granule(&mut reader, sid, |packet| {
        Ok(matches!(opus_packet(packet)?, OpusPacket::Audio))
    })?;
    Ok(Opus {
        duration: duration(last_granule, info.pre_skip, OPUS_GRANULE_RATE),
        info,
        comments,
    })
}

/// The FLAC stream info Tika reads, `FlacInfo`, its sample count in Java's `int` arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FlacInfo {
    pub(super) sample_rate: i32,
    pub(super) channels: i32,
    pub(super) samples: i64,
}

/// `new FlacInfo(data, offset)`.
fn flac_info(data: &[u8], offset: usize) -> Result<FlacInfo, Thrown> {
    if data.len() < offset + 34 {
        return Err(out_of_bounds());
    }
    let next: Vec<i32> = data[offset + 10..offset + 18]
        .iter()
        .map(|&b| i32::from(b))
        .collect();
    let samples = ((next[3] & 0x0f) << 30)
        .wrapping_add(next[4] << 24)
        .wrapping_add(next[5] << 16)
        .wrapping_add(next[6] << 8)
        .wrapping_add(next[7]);
    Ok(FlacInfo {
        sample_rate: (next[0] << 12) + (next[1] << 4) + ((next[2] & 0xf0) >> 4),
        channels: ((next[2] & 0x0e) >> 1) + 1,
        samples: i64::from(samples),
    })
}

/// The type of a FLAC metadata block of stream info.
const STREAM_INFO: u8 = 0;

/// The type of a FLAC metadata block of Vorbis comments.
const VORBIS_COMMENT: u8 = 4;

/// The type of a FLAC metadata block of a picture.
pub(super) const PICTURE: u8 = 6;

/// The bit of a FLAC metadata block header's first byte that flags the last metadata block.
const LAST_BLOCK: u8 = 0x80;

/// The bytes of a FLAC metadata block's header: its type and length.
pub(super) const BLOCK_HEADER: usize = 4;

/// A FLAC metadata block's header: its type, apart from the flag of the last block, which
/// vorbis-java's `FlacMetadataBlock.create` and `FlacTags` match the type with, so that Tika's
/// path reads no stream info or comment block flagged as the last as one (#182).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockHeader {
    pub(super) kind: u8,
    pub(super) last: bool,
    pub(super) length: usize,
}

impl BlockHeader {
    /// The header `data` starts with, `None` where `data` is shorter than a header.
    pub(super) fn read(data: &[u8]) -> Option<Self> {
        let &[first, high, middle, low] = data.first_chunk::<BLOCK_HEADER>()?;
        Some(Self {
            kind: first & !LAST_BLOCK,
            last: first & LAST_BLOCK != 0,
            length: usize::from(high) << 16 | usize::from(middle) << 8 | usize::from(low),
        })
    }

    /// The bytes of the block whose header `data` starts with, its header and as many as the
    /// header gives, where `data` holds one.
    fn block_length(data: &[u8]) -> usize {
        BLOCK_HEADER + Self::read(data).map_or(0, |header| header.length)
    }
}

/// A FLAC metadata block, `FlacMetadataBlock.create`: its header and data.
struct Block<'d> {
    header: BlockHeader,
    data: &'d [u8],
}

/// `FlacMetadataBlock.create` reading from `data` at `at`.
fn block<'d>(data: &'d [u8], at: &mut usize) -> Result<Block<'d>, Thrown> {
    let rest = data
        .get(*at..)
        .filter(|rest| !rest.is_empty())
        .ok_or_else(|| "IllegalArgumentException".to_owned())?;
    let header = BlockHeader::read(rest).ok_or_else(|| "EOFException".to_owned())?;
    let body = rest
        .get(BLOCK_HEADER..BLOCK_HEADER + header.length)
        .ok_or_else(|| "EOFException".to_owned())?;
    *at += BLOCK_HEADER + header.length;
    Ok(Block { header, data: body })
}

/// `FlacTags.FlacTagsAsMetadata` of a comment block's data.
fn flac_tags_block(data: &[u8]) -> Result<Comments, Thrown> {
    let mut packet = vec![VORBIS_COMMENT, 0, 0, 0];
    packet.extend_from_slice(data);
    flac_tags(packet).map(|(comments, _)| comments)
}

/// `new FlacTags(packet)` of a packet's bytes, a comment block's header and data: the comments,
/// and whether the block is the last metadata block.
fn flac_tags(packet: Vec<u8>) -> Result<(Comments, bool), Thrown> {
    let comments = Comments::read(packet, BLOCK_HEADER as i64, false)?;
    let header = BlockHeader::read(&comments.data).ok_or_else(out_of_bounds)?;
    if header.kind != VORBIS_COMMENT {
        return Err(format!(
            "IllegalArgumentException: Invalid type {}",
            header.kind
        ));
    }
    Ok((comments, header.last))
}

/// A FLAC file as Tika reads it: native, or in Ogg with its mapping's version.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Flac {
    pub(super) info: FlacInfo,
    /// The comments, which a native file may lack.
    pub(super) comments: Option<Comments>,
    pub(super) ogg_version: Option<(u8, u8)>,
}

/// `FlacFile.open(input)`, the `pages` of `input` released as they are read.
pub(super) fn flac<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Flac, Thrown> {
    let header = input.get(..4).ok_or_else(|| "EOFException".to_owned())?;
    match header {
        b"OggS" => flac_ogg(input, pages),
        b"fLaC" => flac_native(input),
        _ => Err("IllegalArgumentException: File type not recognised".to_owned()),
    }
}

/// `new FlacNativeFile(input)`: the stream info block first, then the blocks up to the last.
fn flac_native(input: &[u8]) -> Result<Flac, Thrown> {
    let mut at = 4;
    let first = block(input, &mut at)?;
    if first.header.kind != STREAM_INFO {
        return Err("ClassCastException".to_owned());
    }
    let info = flac_info(first.data, 0)?;
    let mut comments = None;
    let mut last = first.header.last;
    while !last {
        let next = block(input, &mut at)?;
        match next.header.kind {
            STREAM_INFO => {
                flac_info(next.data, 0)?;
            }
            VORBIS_COMMENT => comments = Some(flac_tags_block(next.data)?),
            _ => {}
        }
        last = next.header.last;
    }
    Ok(Flac {
        info,
        comments,
        ogg_version: None,
    })
}

/// `new FlacOggFile(new OggFile(input))`: each metadata block read from as much of its packet
/// as the block's length takes.
fn flac_ogg<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Flac, Thrown> {
    let mut reader = Reader::new(input, pages);
    let mut first = None;
    while let Some(packet) = reader.next()? {
        let head = packet.head();
        if packet.beginning_of_stream
            && packet.len() > 10
            && head[0] == 0x7f
            && &head[1..5] == b"FLAC"
        {
            first = Some(packet);
            break;
        }
    }
    let first = first.ok_or_else(|| "NullPointerException".to_owned())?;
    let head = first.head();
    let version = (byte(head, 5)?, byte(head, 6)?);
    byte(head, 8)?;
    let info = flac_info(head, 17)?;
    let tags = reader
        .next_with_sid(first.sid)?
        .ok_or_else(|| "NullPointerException".to_owned())?;
    let (comments, mut last) = flac_tags(tags.data().into_owned())?;
    while !last {
        let Some(packet) = reader.next_with_sid(first.sid)? else {
            break;
        };
        let data = packet.prefix(BlockHeader::block_length(packet.head()));
        let mut at = 0;
        let next = block(&data, &mut at)?;
        match next.header.kind {
            STREAM_INFO => {
                flac_info(next.data, 0)?;
            }
            VORBIS_COMMENT => {
                flac_tags_block(next.data)?;
            }
            _ => {}
        }
        last = next.header.last;
    }
    Ok(Flac {
        info,
        comments: Some(comments),
        ogg_version: Some(version),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_comments_as_vorbis_java_does() {
        let mut data = b"\x03vorbis".to_vec();
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(b"ven");
        data.extend_from_slice(&2u32.to_le_bytes());
        for comment in ["TITLE=Sang", "Ti\u{1}tle=To"] {
            data.extend_from_slice(&u32::try_from(comment.len()).unwrap().to_le_bytes());
            data.extend_from_slice(comment.as_bytes());
        }
        data.push(1);
        let comments = Comments::read(data.clone(), 7, true).unwrap();
        assert_eq!(comments.vendor, "ven");
        assert_eq!(comments.all.get("title").unwrap(), &["Sang", "To"]);
        *data.last_mut().unwrap() = 0;
        assert!(Comments::read(data, 7, true).is_err());
    }

    #[test]
    fn reads_flac_stream_info_as_vorbis_java_does() {
        let mut info = vec![0u8; 34];
        info[10..18].copy_from_slice(&[0x0A, 0xC4, 0x42, 0xF0, 0x00, 0x01, 0x58, 0x88]);
        let read = flac_info(&info, 0).unwrap();
        assert_eq!(read.sample_rate, 44100);
        assert_eq!(read.channels, 2);
        assert_eq!(read.samples, 88200);
    }
}
