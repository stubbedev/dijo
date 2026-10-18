//! Tika's `OggDetector`: an Ogg file named by the streams it carries, read page by page as
//! vorbis-java's `OggPacketReader` reads them.

use std::collections::{HashMap, HashSet};

use crate::mime::media_type::MediaType;

use super::Thrown;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    General,
    Audio,
    Video,
    Metadata,
}

/// An `OggStreamType`: its media type and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StreamType {
    mimetype: &'static str,
    kind: Kind,
}

const UNKNOWN: StreamType = StreamType {
    mimetype: "application/octet-stream",
    kind: Kind::General,
};
const SKELETON: StreamType = StreamType {
    mimetype: "application/annodex",
    kind: Kind::Metadata,
};
const KATE: StreamType = StreamType {
    mimetype: "application/kate",
    kind: Kind::Metadata,
};

/// `OggStreamIdentifier.identifyType` for a stream's first packet.
fn identify(data: &[u8]) -> StreamType {
    if data.len() <= 10 {
        return UNKNOWN;
    }
    let starts = |magic: &[u8]| data.starts_with(magic);
    let typed =
        |prefix: &[u8], word: &[u8]| prefix.contains(&data[0]) && data[1..].starts_with(word);
    let (mimetype, kind) = if is_skeleton(data) || starts(b"Annodex\0") {
        return SKELETON;
    } else if starts(b"CMML\0\0\0\0") {
        ("text/x-cmml", Kind::Metadata)
    } else if starts(b"\x80kate\0\0\0") {
        return KATE;
    } else if data.len() >= 16 && typed(&[1, 3, 5], b"vorbis") {
        ("audio/vorbis", Kind::Audio)
    } else if data.len() >= 72 && starts(b"Speex   ") {
        ("audio/speex", Kind::Audio)
    } else if data.len() >= 12 && (starts(b"OpusHead") || starts(b"OpusTags")) {
        ("audio/opus", Kind::Audio)
    } else if typed(&[0x7F], b"FLAC") {
        ("audio/x-oggflac", Kind::Audio)
    } else if starts(b"PCM     ") {
        ("audio/x-oggpcm", Kind::Audio)
    } else if data.len() >= 16 && typed(&[0x80, 0x81, 0x82], b"theora") {
        ("video/theora", Kind::Video)
    } else if starts(b"\x80daala\0\0") {
        ("video/daala", Kind::Video)
    } else if starts(b"BBCD") {
        ("video/x-dirac", Kind::Video)
    } else if starts(b"video") {
        ("video/x-ogm", Kind::Video)
    } else if starts(b"UVS ") {
        ("video/x-ogguvs", Kind::Video)
    } else if starts(b"\x01YUV") {
        ("video/x-oggyuv", Kind::Video)
    } else if starts(b"\x01GBP") {
        ("video/x-oggrgb", Kind::Video)
    } else {
        return UNKNOWN;
    };
    StreamType { mimetype, kind }
}

fn is_skeleton(data: &[u8]) -> bool {
    data.len() >= 52 && (data.starts_with(b"fishead\0") || data.starts_with(b"fisbone\0"))
}

/// `SkeletonPacketFactory.create`: what parsing a Skeleton stream's packet throws.
fn skeleton_packet(data: &[u8]) -> Result<(), Thrown> {
    if !is_skeleton(data) {
        return Ok(());
    }
    match data[3] {
        b'h' => {
            let major = u16::from_le_bytes([data[8], data[9]]);
            if !(3..=4).contains(&major) {
                return Err(Thrown::Runtime);
            }
            let utc_present = data[44] != 0 || data[45] != 0;
            let needed = if major == 4 {
                80
            } else if utc_present {
                64
            } else {
                52
            };
            if data.len() < needed {
                return Err(Thrown::Runtime);
            }
            Ok(())
        }
        b'b' => {
            if u32::from_le_bytes([data[8], data[9], data[10], data[11]]) != 44 {
                return Err(Thrown::Runtime);
            }
            let headers = String::from_utf8_lossy(&data[52..]);
            if !headers.contains("Content-Type") {
                return Err(Thrown::Runtime);
            }
            let all_split = headers
                .split(['\r', '\n'])
                .filter(|line| !line.is_empty())
                .all(|line| line.contains(": "));
            if all_split {
                Ok(())
            } else {
                Err(Thrown::Runtime)
            }
        }
        _ => Ok(()),
    }
}

/// A packet as `OggPacketReader` returns it, of which only the first [`HEAD`] bytes are kept,
/// unless it is a Skeleton `fisbone` packet, which is read whole: a packet may run over all of
/// a file's pages.
struct Packet {
    sid: i32,
    data: Vec<u8>,
    beginning: bool,
}

/// The bytes kept of a packet: more than any packet but a `fisbone` is read for.
const HEAD: usize = 255;
const FISBONE: &[u8] = b"fisbone\0";

/// Appends `fragment` to the bytes kept of `packet`.
fn append(packet: &mut Vec<u8>, fragment: &[u8]) {
    let head = HEAD.saturating_sub(packet.len()).min(fragment.len());
    packet.extend_from_slice(&fragment[..head]);
    if packet.starts_with(FISBONE) {
        packet.extend_from_slice(&fragment[head..]);
    }
}

/// `OggPacketReader.getNextPacket` over the whole input: `each` of the packets until the input
/// ends or a page is cut short.
fn packets(data: &[u8], mut each: impl FnMut(Packet)) -> Result<(), Thrown> {
    let mut position = 0;
    let mut left_over: Option<Vec<u8>> = None;
    loop {
        let mut searched = 0usize;
        let mut matched = 0usize;
        loop {
            if searched >= 65_536 {
                return Err(Thrown::Io);
            }
            let Some(&byte) = data.get(position) else {
                return Ok(());
            };
            position += 1;
            matched = match (matched, byte) {
                (0, b'O') => 1,
                (1, b'g') => 2,
                (2, b'g') => 3,
                (3, b'S') => 4,
                _ => 0,
            };
            if matched == 4 {
                break;
            }
            searched += 1;
        }
        match data.get(position) {
            None => return Ok(()),
            Some(0) => {}
            Some(_) => return Err(Thrown::Runtime),
        }
        let Some(header) = data.get(position..position + 23) else {
            return Ok(());
        };
        let flags = header[1];
        let sid = i32::from_le_bytes([header[10], header[11], header[12], header[13]]);
        let segments = usize::from(header[22]);
        position += 23;
        let Some(lacing) = data.get(position..position + segments) else {
            return Ok(());
        };
        position += segments;
        let size: usize = lacing.iter().map(|&l| usize::from(l)).sum();
        let Some(body) = data.get(position..position + size) else {
            return Ok(());
        };
        position += size;
        let beginning_page = flags & 0x02 != 0;
        let mut lv = 0;
        let mut offset = 0;
        let mut first = true;
        while lv < segments || (first && segments == 0) {
            let mut packet_lvs = 0;
            let mut packet_size = 0;
            let mut continues = false;
            for (i, &value) in lacing.iter().enumerate().skip(lv) {
                packet_size += usize::from(value);
                packet_lvs += 1;
                if value < 255 {
                    break;
                }
                if i == segments - 1 {
                    continues = true;
                }
            }
            let mut packet = left_over.take().unwrap_or_default();
            append(&mut packet, &body[offset..offset + packet_size]);
            if continues {
                left_over = Some(packet);
            } else {
                each(Packet {
                    sid,
                    data: packet,
                    beginning: beginning_page && lv == 0,
                });
            }
            lv += packet_lvs;
            offset += packet_size;
            if lv == 0 {
                lv = 1;
            }
            first = false;
        }
    }
}

const OGG_GENERAL: &str = "application/ogg";

fn to_media_type(kind: StreamType) -> MediaType {
    if kind == UNKNOWN {
        MediaType::named(OGG_GENERAL)
    } else {
        MediaType::named(kind.mimetype)
    }
}

/// The Ogg type, `None` for a document that does not start with `OggS`, or what Tika throws.
pub(crate) fn detect(data: &[u8]) -> Result<Option<MediaType>, Thrown> {
    if !data.starts_with(b"OggS") {
        return Ok(None);
    }
    let mut total = 0;
    let mut streams: HashMap<StreamType, usize> = HashMap::new();
    let mut skeletons: HashSet<i32> = HashSet::new();
    let mut thrown = None;
    packets(data, |packet| {
        if thrown.is_some() {
            return;
        }
        if packet.beginning {
            total += 1;
            let kind = identify(&packet.data);
            if kind == SKELETON {
                if let Err(error) = skeleton_packet(&packet.data) {
                    thrown = Some(error);
                    return;
                }
                skeletons.insert(packet.sid);
            }
            *streams.entry(kind).or_insert(0) += 1;
        } else if skeletons.contains(&packet.sid) {
            thrown = skeleton_packet(&packet.data).err();
        }
    })?;
    if let Some(thrown) = thrown {
        return Err(thrown);
    }
    let only = |filter: &dyn Fn(&StreamType) -> bool| {
        streams
            .keys()
            .copied()
            .filter(|k| filter(k))
            .collect::<Vec<_>>()
    };
    if total == 1 {
        return Ok(Some(to_media_type(
            *streams.keys().next().expect("one stream"),
        )));
    }
    let non_metadata: usize = streams
        .iter()
        .filter(|(k, _)| k.kind != Kind::Metadata)
        .map(|(_, n)| n)
        .sum();
    if non_metadata == 0 {
        return Ok(Some(MediaType::named(OGG_GENERAL)));
    }
    if non_metadata == 1 {
        let kind = only(&|k| k.kind != Kind::Metadata)[0];
        return Ok(Some(to_media_type(kind)));
    }
    let count = |kind: Kind| -> usize {
        streams
            .iter()
            .filter(|(k, _)| k.kind == kind)
            .map(|(_, n)| n)
            .sum()
    };
    let (video, audio) = (count(Kind::Video), count(Kind::Audio));
    let video_types = only(&|k| k.kind == Kind::Video);
    let audio_types = only(&|k| k.kind == Kind::Audio);
    let found = if video == 1 {
        to_media_type(video_types[0])
    } else if video == 0 && audio > 1 {
        if audio_types.len() == 1 {
            to_media_type(audio_types[0])
        } else {
            MediaType::named("audio/ogg")
        }
    } else if video > 1 {
        if video_types.len() == 1 {
            to_media_type(video_types[0])
        } else {
            MediaType::named("video/ogg")
        }
    } else if streams.contains_key(&KATE) {
        to_media_type(KATE)
    } else {
        MediaType::named(OGG_GENERAL)
    };
    Ok(Some(found))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laced_page(sid: u32, flags: u8, lacing: &[u8], body: &[u8]) -> Vec<u8> {
        let mut page = b"OggS\0".to_vec();
        page.push(flags);
        page.extend([0u8; 8]);
        page.extend(sid.to_le_bytes());
        page.extend([0u8; 8]);
        page.push(lacing.len() as u8);
        page.extend(lacing);
        page.extend(body);
        page
    }

    fn page(sid: u32, flags: u8, packets: &[&[u8]]) -> Vec<u8> {
        let mut lacing = Vec::new();
        for packet in packets {
            let mut left = packet.len();
            while left >= 255 {
                lacing.push(255);
                left -= 255;
            }
            lacing.push(left as u8);
        }
        laced_page(sid, flags, &lacing, &packets.concat())
    }

    fn vorbis_header() -> Vec<u8> {
        let mut header = b"\x01vorbis".to_vec();
        header.extend([0u8; 23]);
        header
    }

    #[test]
    fn names_a_single_stream() {
        let data = page(1, 0x02, &[&vorbis_header()]);
        assert_eq!(detect(&data).unwrap().unwrap().as_str(), "audio/vorbis");
    }

    #[test]
    fn names_mixed_streams() {
        let mut data = page(1, 0x02, &[&vorbis_header()]);
        let mut theora = b"\x80theora".to_vec();
        theora.extend([0u8; 40]);
        data.extend(page(2, 0x02, &[&theora]));
        assert_eq!(detect(&data).unwrap().unwrap().as_str(), "video/theora");
    }

    /// A packet that runs over pages is kept by its first bytes, as many as it is named by.
    #[test]
    fn names_a_stream_by_the_start_of_a_packet_over_pages() {
        let mut header = vorbis_header();
        header.resize(HEAD * 4, 0);
        let mut data = laced_page(1, 0x02, &[255], &header[..HEAD]);
        data.extend(page(1, 0x02, &[&header[HEAD..]]));
        assert_eq!(detect(&data).unwrap().unwrap().as_str(), "audio/vorbis");
        let mut packet = Vec::new();
        append(&mut packet, &header);
        assert_eq!(packet, header[..HEAD]);
        let fisbone = [FISBONE, &[0; HEAD * 2]].concat();
        let mut packet = Vec::new();
        append(&mut packet, &fisbone[..HEAD]);
        append(&mut packet, &fisbone[HEAD..]);
        assert_eq!(packet, fisbone);
    }

    #[test]
    fn ignores_what_is_not_ogg() {
        assert!(detect(b"RIFF").unwrap().is_none());
        assert_eq!(detect(b"OggS").unwrap().unwrap().as_str(), OGG_GENERAL);
    }
}
