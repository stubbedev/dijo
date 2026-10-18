//! Flash video, as Tika's `FLVParser` reads it: the header's audio and video flags, and the
//! entries of each `onMetaData` script tag's AMF0 map as `flv:` metadata, each value as Java's
//! `toString` writes it. Java writes an AMF date in its default time zone, named; the port
//! writes it in UTC, which makes the reading differ.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::java::dtoa::java_double_to_string;
use crate::java::time;
use crate::java::{self, JavaHashMap};

const TYPE_METADATA: u8 = 0x12;
const MASK_AUDIO: u8 = 1;
const MASK_VIDEO: u8 = 4;
const MAX_AMF_DEPTH: usize = 64;
const MAX_AMF_ELEMENTS: u64 = 100_000;
const PREFIX: &str = "flv:";

/// An AMF0 value, as `readAMFData` returns it.
#[derive(Debug, Clone, PartialEq)]
enum Amf {
    Number(f64),
    Boolean(bool),
    String(String),
    Map(JavaHashMap<Option<Amf>>),
    List(Vec<Option<Amf>>),
    /// A `Date`, in milliseconds since 1970.
    Date(i64),
    Undefined,
}

impl Amf {
    /// `toString` of the value, a date written in UTC.
    fn text(&self) -> String {
        let text =
            |value: &Option<Amf>| value.as_ref().map_or_else(|| "null".to_owned(), Amf::text);
        match self {
            Amf::Number(number) => java_double_to_string(*number),
            Amf::Boolean(value) => value.to_string(),
            Amf::String(value) => value.clone(),
            Amf::Map(map) => {
                let entries: Vec<String> = map
                    .iter()
                    .map(|(key, value)| format!("{key}={}", text(value)))
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
            Amf::List(items) => {
                let items: Vec<String> = items.iter().map(text).collect();
                format!("[{}]", items.join(", "))
            }
            Amf::Date(millis) => time::utc_date_to_string(*millis),
            Amf::Undefined => "UNDEFINED".to_owned(),
        }
    }

    /// Whether the value is or holds a date.
    fn dated(&self) -> bool {
        let dated = |value: &Option<Amf>| value.as_ref().is_some_and(Amf::dated);
        match self {
            Amf::Date(_) => true,
            Amf::Map(map) => map.iter().any(|(_, value)| dated(value)),
            Amf::List(items) => items.iter().any(dated),
            Amf::Number(_) | Amf::Boolean(_) | Amf::String(_) | Amf::Undefined => false,
        }
    }
}

/// What Java throws, which stops the read and fails the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stop(String);

fn end_of_file() -> Stop {
    Stop("EOFException".to_owned())
}

/// A `DataInputStream` over bytes.
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Input<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Stop> {
        let taken = self
            .bytes
            .get(self.at..self.at + count)
            .ok_or_else(end_of_file)?;
        self.at += count;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, Stop> {
        Ok(self.take(1)?[0])
    }

    /// `read()`: the next byte, or `None` at the end.
    fn read(&mut self) -> Option<u8> {
        self.u8().ok()
    }

    fn u16(&mut self) -> Result<u16, Stop> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u24(&mut self) -> Result<u32, Stop> {
        let bytes = self.take(3)?;
        Ok(u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]))
    }

    fn u32(&mut self) -> Result<u32, Stop> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// `readAMFData`.
    fn amf(&mut self, kind: Option<u8>, depth: usize) -> Result<Option<Amf>, Stop> {
        if depth > MAX_AMF_DEPTH {
            return Err(Stop(format!(
                "IOException: AMF nesting exceeds the maximum depth of {MAX_AMF_DEPTH}"
            )));
        }
        let kind = match kind {
            Some(kind) => kind,
            None => self.u8()?,
        };
        Ok(Some(match kind {
            0 => Amf::Number(f64::from_bits(u64::from_be_bytes(
                self.take(8)?.try_into().expect("eight bytes"),
            ))),
            1 => Amf::Boolean(self.u8()? == 1),
            2 => Amf::String(self.string()?),
            3 => self.object(depth)?,
            8 => self.ecma_array(depth)?,
            10 => {
                let count = u64::from(self.u32()?);
                if count > MAX_AMF_ELEMENTS {
                    return Err(Stop(format!(
                        "IOException: AMF array count {count} exceeds the maximum of \
                         {MAX_AMF_ELEMENTS}"
                    )));
                }
                let mut items = Vec::new();
                for _ in 0..count {
                    items.push(self.amf(None, depth + 1)?);
                }
                Amf::List(items)
            }
            11 => {
                let millis = f64::from_bits(u64::from_be_bytes(
                    self.take(8)?.try_into().expect("eight bytes"),
                )) as i64;
                self.u16()?;
                Amf::Date(millis)
            }
            13 => Amf::Undefined,
            _ => return Ok(None),
        }))
    }

    /// `readAMFString`.
    fn string(&mut self) -> Result<String, Stop> {
        let size = usize::from(self.u16()?);
        let bytes = self.take(size)?;
        Ok(java::utf8(bytes))
    }

    /// `readAMFObject`: entries up to the end marker, the marker's type read as `read()`
    /// reads it, the end of the data as `-1`.
    fn object(&mut self, depth: usize) -> Result<Amf, Stop> {
        let mut map = JavaHashMap::default();
        let mut index: u64 = 0;
        loop {
            let key = self.string()?;
            let kind = self.read();
            if kind == Some(9) {
                break;
            }
            if index >= MAX_AMF_ELEMENTS {
                return Err(Stop(format!(
                    "IOException: AMF object exceeds the maximum of {MAX_AMF_ELEMENTS} entries"
                )));
            }
            let value = self.typed(kind, depth)?;
            map.insert(&key, value);
            index += 1;
        }
        Ok(Amf::Map(map))
    }

    /// `readAMFEcmaArray`.
    fn ecma_array(&mut self, depth: usize) -> Result<Amf, Stop> {
        let size = u64::from(self.u32()?);
        if size > MAX_AMF_ELEMENTS {
            return Err(Stop(format!(
                "IOException: AMF array size {size} exceeds the maximum of {MAX_AMF_ELEMENTS}"
            )));
        }
        let mut map = JavaHashMap::default();
        for _ in 0..size {
            let key = self.string()?;
            let kind = self.read();
            let value = self.typed(kind, depth)?;
            map.insert(&key, value);
        }
        Ok(Amf::Map(map))
    }

    /// `readAMFData(input, dataType, depth + 1)` of a type `read()` returned: the end of the data,
    /// `-1`, makes it read the type again, which then fails.
    fn typed(&mut self, kind: Option<u8>, depth: usize) -> Result<Option<Amf>, Stop> {
        match kind {
            Some(kind) => self.amf(Some(kind), depth + 1),
            None => self.amf(None, depth + 1),
        }
    }
}

/// The header flags and the metadata entries of `input`, whether an entry holds a date, or why
/// the parse stops: what Java throws before the document starts, and what it throws after it,
/// with the entries read before.
struct Flv {
    flags: u8,
    entries: Entries,
    thrown: Option<String>,
}

/// The metadata entries read, and whether a value of one holds a date.
#[derive(Default)]
struct Entries {
    written: Vec<(String, String)>,
    dated: bool,
}

/// `FLVParser.parse` up to the output, the `pages` of `input` released as its tags are read.
fn read<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Flv, Stop> {
    let mut data = Input {
        bytes: input,
        at: 0,
    };
    if data.take(3).ok() != Some(b"FLV".as_slice()) {
        return Err(Stop("FLV signature not detected".to_owned()));
    }
    let version = data.u8()?;
    if version != 1 {
        return Err(Stop(format!("Unpexpected FLV version: {version}")));
    }
    let flags = data.u8()?;
    let length = data.u32()?;
    if length != 9 {
        return Err(Stop(format!("Unpexpected FLV header length: {length}")));
    }
    let previous = data.u32()?;
    if previous != 0 {
        return Err(Stop(format!(
            "Unpexpected FLV first previous block size: {previous}"
        )));
    }
    let mut entries = Entries::default();
    let thrown = tags(&mut data, &mut entries, &Paging::new(pages))
        .err()
        .map(|Stop(thrown)| thrown);
    Ok(Flv {
        flags,
        entries,
        thrown,
    })
}

/// The tags after the header: each script tag's second AMF value, where it is a map, read for
/// its entries; each tag counted read once it is.
fn tags(data: &mut Input, entries: &mut Entries, paging: &Paging) -> Result<(), Stop> {
    let mut counted = data.at;
    while let Some(kind) = data.read() {
        paging.read(data.at - counted);
        counted = data.at;
        let length = data.u24()?;
        data.u32()?;
        data.u24()?;
        let length_bytes = usize::try_from(length).expect("24 bits fit");
        if kind == TYPE_METADATA {
            let available = data.bytes.len() - data.at;
            let body = data.take(length_bytes.min(available))?;
            let mut padded = body.to_vec();
            padded.resize(length_bytes, 0);
            let mut script = Input {
                bytes: &padded,
                at: 0,
            };
            let mut value = None;
            for _ in 0..2 {
                value = script.amf(None, 0)?;
            }
            if let Some(Amf::Map(map)) = value {
                for (key, value) in map.iter() {
                    if let Some(value) = value {
                        entries.dated |= value.dated();
                        entries.written.push((key.to_owned(), value.text()));
                    }
                }
            }
        } else {
            data.take(length_bytes)?;
        }
        let previous = data.u32()?;
        if previous != length + 11 {
            break;
        }
    }
    Ok(())
}

pub struct FlvParser;

impl Parser for FlvParser {
    fn name(&self) -> &'static str {
        "extractous::FLVParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.video.FLVParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["video/x-flv"]
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
        let flv =
            read(input, pages).map_err(|Stop(thrown)| Decline::Fails(Error::ParseError(thrown)))?;
        let difference = flv.entries.dated.then(|| {
            Difference::new(
                "an AMF date, which Java writes in its default time zone, named, and the port in \
                 UTC",
            )
        });
        let reading = Box::new(move |xhtml: &mut Xhtml<'_>| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "video/x-flv");
            metadata.set("flv:hasVideo", (flv.flags & MASK_VIDEO != 0).to_string());
            metadata.set("flv:hasAudio", (flv.flags & MASK_AUDIO != 0).to_string());
            for (key, value) in &flv.entries.written {
                metadata.add_prefixed(PREFIX, key, value.clone());
            }
            match &flv.thrown {
                Some(thrown) => Err(Failure::Malformed(thrown.clone())),
                None => Ok(()),
            }
        });
        unless_different(reading, difference)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_same_as_tika, conformance_corpus, differing_reading, documents, Scratch,
    };
    use extractous_core::large::{bodies_past_limit, LargeFile};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.video.FLVParser";

    /// The header of a file of audio and video.
    const HEADER: &[u8] = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00";
    const TYPE_VIDEO: u8 = 9;

    fn amf_string(text: &str) -> Vec<u8> {
        let mut bytes = u16::try_from(text.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes
    }

    /// A tag of `kind` holding `body`, with the size of the tag after it.
    fn tag(kind: u8, body: &[u8]) -> Vec<u8> {
        let length = u32::try_from(body.len()).unwrap();
        let mut tag = vec![kind];
        tag.extend(&length.to_be_bytes()[1..]);
        tag.extend([0; 7]);
        tag.extend(body);
        tag.extend((length + 11).to_be_bytes());
        tag
    }

    /// An AMF date of `millis` since 1970, with the offset of the zone it names in minutes.
    #[cfg(feature = "tika")]
    fn date(millis: f64, offset: i16) -> Vec<u8> {
        let mut date = vec![11];
        date.extend(millis.to_be_bytes());
        date.extend(offset.to_be_bytes());
        date
    }

    /// A video whose `onMetaData` map has the entries of an encoder's, then `extra`, each a key
    /// and its AMF value.
    fn flv(extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut script = vec![2];
        script.extend(amf_string("onMetaData"));
        script.push(8);
        script.extend((4 + u32::try_from(extra.len()).unwrap()).to_be_bytes());
        for (key, value) in extra {
            script.extend(amf_string(key));
            script.extend(value);
        }
        script.extend(amf_string("duration"));
        script.push(0);
        script.extend(12.5f64.to_be_bytes());
        script.extend(amf_string("stereo"));
        script.extend([1, 1]);
        script.extend(amf_string("encoder"));
        script.push(2);
        script.extend(amf_string("Lavf"));
        script.extend(amf_string("keyframes"));
        script.push(3);
        script.extend(amf_string("times"));
        script.extend([10, 0, 0, 0, 2, 0]);
        script.extend(0f64.to_be_bytes());
        script.push(13);
        script.extend(amf_string(""));
        script.push(9);
        [HEADER, &tag(TYPE_METADATA, &script)].concat()
    }

    /// A file too large to read whole is read a tag at a time, in place, its pages released as
    /// they are read, in memory that does not grow with the file.
    #[test]
    fn extracts_flash_videos_too_large_to_read_whole_in_bounded_memory() {
        let video = tag(TYPE_VIDEO, &[0x17; 1 << 16]);
        let count = bodies_past_limit(video.len());
        let file = LargeFile::new("long.flv", &flv(&[]), count, |_| video.clone(), b"");
        let (_, metadata) = file.extract(FlvParser.name());
        assert_eq!(metadata["flv:hasVideo"], ["true"]);
        assert_eq!(metadata["flv:duration"], ["12.5"]);
        assert_eq!(metadata["flv:encoder"], ["Lavf"]);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_flash_videos_as_tikas_path_does() {
        let scratch = Scratch::new("flv");
        let read = [scratch.write("video.flv", &flv(&[]))];
        let (parsed, deferred) = documents(&FlvParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

    /// Dates are written in UTC, as Tika's path writes them where Java's default time zone is
    /// UTC, and the reading differs from Tika's in other zones.
    #[cfg(feature = "tika")]
    #[test]
    fn writes_flash_video_dates_in_utc() {
        let scratch = Scratch::new("flv-dates");
        let mut dates = vec![10, 0, 0, 0, 5];
        for millis in [
            f64::NAN,
            1e300,
            -1e15,
            -12_219_292_800_001.0,
            951_782_400_123.0,
        ] {
            dates.extend(date(millis, 0));
        }
        let path = scratch.write(
            "dated.flv",
            &flv(&[
                ("creationdate", date(1_791_836_869_000.0, -120)),
                ("dates", dates),
            ]),
        );
        let (parsed, deferred) = documents(&FlvParser, &[scratch.directory()]);
        assert!(parsed.is_empty(), "{parsed:?}");
        assert_eq!(deferred, std::slice::from_ref(&path));
        let (Difference(difference), xhtml) = differing_reading(&FlvParser, &path);
        assert!(difference.starts_with("an AMF date"), "{difference}");
        for meta in [
            r#"<meta name="flv:creationdate" content="Mon Oct 12 20:27:49 UTC 2026" />"#,
            r#"<meta name="flv:dates" content="[Thu Jan 01 00:00:00 UTC 1970, Sun Aug 17 07:12:55 UTC 292278994, Tue Nov 16 22:13:20 UTC 29720, Thu Oct 04 23:59:59 UTC 1582, Tue Feb 29 00:00:00 UTC 2000]" />"#,
            r#"<meta name="flv:duration" content="12.5" />"#,
        ] {
            assert!(xhtml.contains(meta), "{meta} in {xhtml}");
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_flash_videos_as_tikas_path_does() {
        let (parsed, deferred) = documents(&FlvParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 1, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
