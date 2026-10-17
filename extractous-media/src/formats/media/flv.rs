//! Flash video, as Tika's `FLVParser` reads it: the header's audio and video flags, and the
//! entries of each `onMetaData` script tag's AMF0 map as `flv:` metadata, each value as Java's
//! `toString` writes it. A document is left to Tika where a value holds an AMF date, which Java
//! writes with its default time zone's name.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::dtoa::java_double_to_string;
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
    Undefined,
}

impl Amf {
    /// `toString` of the value.
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
            Amf::Undefined => "UNDEFINED".to_owned(),
        }
    }
}

/// Why a read stops: what Java throws, which fails the parse, or a value the port does not
/// write.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stop {
    Thrown(String),
    Unported(&'static str),
}

fn end_of_file() -> Stop {
    Stop::Thrown("EOFException".to_owned())
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
            return Err(Stop::Thrown(format!(
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
                    return Err(Stop::Thrown(format!(
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
            11 => return Err(Stop::Unported("an AMF date")),
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
                return Err(Stop::Thrown(format!(
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
            return Err(Stop::Thrown(format!(
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

/// The header flags and the metadata entries of `input`, or why the parse stops: what Java
/// throws before the document starts, and what it throws after it, with the entries read
/// before.
struct Flv {
    flags: u8,
    entries: Vec<(String, String)>,
    thrown: Option<String>,
}

/// `FLVParser.parse` up to the output.
fn read(input: &[u8]) -> Result<Flv, Stop> {
    let mut data = Input {
        bytes: input,
        at: 0,
    };
    if data.take(3).ok() != Some(b"FLV".as_slice()) {
        return Err(Stop::Thrown("FLV signature not detected".to_owned()));
    }
    let version = data.u8()?;
    if version != 1 {
        return Err(Stop::Thrown(format!("Unpexpected FLV version: {version}")));
    }
    let flags = data.u8()?;
    let length = data.u32()?;
    if length != 9 {
        return Err(Stop::Thrown(format!(
            "Unpexpected FLV header length: {length}"
        )));
    }
    let previous = data.u32()?;
    if previous != 0 {
        return Err(Stop::Thrown(format!(
            "Unpexpected FLV first previous block size: {previous}"
        )));
    }
    let mut entries = Vec::new();
    let thrown = match tags(&mut data, &mut entries) {
        Ok(()) => None,
        Err(Stop::Thrown(thrown)) => Some(thrown),
        Err(unported) => return Err(unported),
    };
    Ok(Flv {
        flags,
        entries,
        thrown,
    })
}

/// The tags after the header: each script tag's second AMF value, where it is a map, read for
/// its entries.
fn tags(data: &mut Input, entries: &mut Vec<(String, String)>) -> Result<(), Stop> {
    while let Some(kind) = data.read() {
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
                        entries.push((key.to_owned(), value.text()));
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

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let flv = read(input).map_err(|stop| match stop {
            Stop::Thrown(thrown) => Declined::Fails(Error::ParseError(thrown)),
            Stop::Unported(what) => Declined::Deferred(format!(
                "{what}, which Java writes with its default time zone's name"
            )),
        })?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "video/x-flv");
            metadata.set("flv:hasVideo", (flv.flags & MASK_VIDEO != 0).to_string());
            metadata.set("flv:hasAudio", (flv.flags & MASK_AUDIO != 0).to_string());
            for (key, value) in &flv.entries {
                metadata.add_prefixed(PREFIX, key, value.clone());
            }
            match &flv.thrown {
                Some(thrown) => Err(Failure::Malformed(thrown.clone())),
                None => Ok(()),
            }
        }))
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

    const TIKA_NAME: &str = "org.apache.tika.parser.video.FLVParser";

    fn amf_string(text: &str) -> Vec<u8> {
        let mut bytes = u16::try_from(text.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes
    }

    fn flv() -> Vec<u8> {
        let mut script = vec![2];
        script.extend(amf_string("onMetaData"));
        script.extend([8, 0, 0, 0, 4]);
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
        let mut file = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00".to_vec();
        let length = u32::try_from(script.len()).unwrap();
        file.push(TYPE_METADATA);
        file.extend(&length.to_be_bytes()[1..]);
        file.extend([0; 7]);
        file.extend(&script);
        file.extend((length + 11).to_be_bytes());
        file
    }

    #[test]
    fn extracts_flash_videos_as_tikas_path_does() {
        let scratch = Scratch::new("flv");
        let read = [scratch.write("video.flv", &flv())];
        let (parsed, deferred) = documents(&FlvParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

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
