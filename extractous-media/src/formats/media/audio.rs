//! WAVE, AU and AIFF files, as Tika's `AudioParser` reads them through the JDK's
//! `AudioSystem.getAudioFileFormat`: the JDK 25 file readers tried in the order its service
//! loader lists them, each from the start, the first that takes the file giving its type and
//! audio format, written as metadata. A file no reader takes gets Tika's warning instead.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Footprint, Parser, Prepared};
use crate::java::JavaHashMap;

/// The warning Tika records for a file no reader takes.
const UNSUPPORTED: &str = "An UnsupportedAudioFileException was thrown.  This could mean that \
                           the underlying parser hit an EndOfFileException or that the file is \
                           unsupported. ¯\\_(ツ)_/¯";
/// `AudioSystem.NOT_SPECIFIED`.
const NOT_SPECIFIED: i32 = -1;

/// `AudioFileFormat.Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Type {
    Wave,
    Au,
    Aiff,
    Aifc,
}

impl Type {
    /// The `Content-Type` `AudioParser` sets for the type.
    fn content_type(self) -> &'static str {
        match self {
            Type::Aifc | Type::Aiff => "audio/x-aiff",
            Type::Au => "audio/basic",
            Type::Wave => "audio/vnd.wave",
        }
    }
}

/// `AudioFormat.Encoding`, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    PcmSigned,
    PcmUnsigned,
    PcmFloat,
    Ulaw,
    Alaw,
}

impl Encoding {
    fn name(self) -> &'static str {
        match self {
            Encoding::PcmSigned => "PCM_SIGNED",
            Encoding::PcmUnsigned => "PCM_UNSIGNED",
            Encoding::PcmFloat => "PCM_FLOAT",
            Encoding::Ulaw => "ULAW",
            Encoding::Alaw => "ALAW",
        }
    }
}

/// What `getAudioFileFormat` returns that Tika reads.
#[derive(Debug, Clone, PartialEq)]
struct FileFormat {
    kind: Type,
    encoding: Encoding,
    sample_rate: f32,
    sample_size_in_bits: i32,
    channels: i32,
    /// The audio format's properties, `toString` of each value, in Java's order.
    properties: Vec<(String, String)>,
}

/// Why a reader does not give a format.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rejected {
    /// `UnsupportedAudioFileException`, or an `EOFException` `SunFileReader` turns into one:
    /// the next reader is tried.
    Unsupported,
    /// A runtime exception, which fails the parse.
    Thrown(String),
}

type Read<T> = Result<T, Rejected>;

/// A JDK audio file reader's `getAudioFileFormat`.
type Reader = fn(&[u8]) -> Read<FileFormat>;

/// A `DataInputStream` over the file from its start, its skips those of Tika's
/// `SkipFullyInputStream`, which fail short of the count.
struct Data<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Data<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take<const N: usize>(&mut self) -> Read<[u8; N]> {
        let bytes = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or(Rejected::Unsupported)?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn int(&mut self) -> Read<i32> {
        Ok(i32::from_be_bytes(self.take()?))
    }

    fn unsigned_short(&mut self) -> Read<u16> {
        Ok(u16::from_be_bytes(self.take()?))
    }

    fn little_int(&mut self) -> Read<i32> {
        Ok(i32::from_le_bytes(self.take()?))
    }

    fn little_short(&mut self) -> Read<i16> {
        Ok(i16::from_le_bytes(self.take()?))
    }

    /// `skipBytes(count)` over `SkipFullyInputStream`: nothing for a count that is not
    /// positive, else all of it or an `EOFException`.
    fn skip_bytes(&mut self, count: i32) -> Read<()> {
        let Ok(count) = usize::try_from(count) else {
            return Ok(());
        };
        if self.bytes.len() - self.at < count {
            return Err(Rejected::Unsupported);
        }
        self.at += count;
        Ok(())
    }
}

/// `calculatePCMFrameSize`: the bytes of a frame, 0 where the product overflows an `int`.
fn pcm_frame_size(sample_size_in_bits: i32, channels: i32) -> i32 {
    ((sample_size_in_bits + 7) / 8)
        .checked_mul(channels)
        .unwrap_or(0)
}

const RIFF: i32 = 0x5249_4646;
const WAVE: i32 = 0x5741_5645;
const FMT: i32 = 0x666d_7420;
const DATA: i32 = 0x6461_7461;

/// `WaveFileReader`: PCM, A-law and µ-law WAVE files.
fn wave(bytes: &[u8]) -> Read<FileFormat> {
    let mut data = Data::new(bytes);
    let magic = data.int()?;
    data.int()?;
    let wave_magic = data.int()?;
    if magic != RIFF || wave_magic != WAVE {
        return Err(Rejected::Unsupported);
    }
    let mut read = 0i32;
    loop {
        let chunk = data.int()?;
        read = read.wrapping_add(4);
        if chunk == FMT {
            break;
        }
        let mut length = data.little_int()?;
        read = read.wrapping_add(4);
        if length % 2 > 0 {
            length += 1;
        }
        data.skip_bytes(length)?;
        if length > 0 {
            read = read.wrapping_add(length);
        }
    }
    let length = data.little_int()?;
    read = read.wrapping_add(4);
    let end = read.wrapping_add(length);
    let encoding = match data.little_short()? {
        1 => Encoding::PcmSigned,
        6 => Encoding::Alaw,
        7 => Encoding::Ulaw,
        _ => return Err(Rejected::Unsupported),
    };
    read = read.wrapping_add(2);
    let channels = data.little_short()?;
    read = read.wrapping_add(2);
    if channels <= 0 {
        return Err(Rejected::Unsupported);
    }
    let sample_rate = i64::from(data.little_int()?);
    data.little_int()?;
    data.little_short()?;
    let sample_size_in_bits = i32::from(data.little_short()?);
    read = read.wrapping_add(12);
    if sample_size_in_bits <= 0 {
        return Err(Rejected::Unsupported);
    }
    let encoding = if sample_size_in_bits == 8 && encoding == Encoding::PcmSigned {
        Encoding::PcmUnsigned
    } else {
        encoding
    };
    if end > read {
        data.skip_bytes(end - read)?;
    }
    loop {
        let chunk = data.int()?;
        if chunk == DATA {
            break;
        }
        let mut length = data.little_int()?;
        if length % 2 > 0 {
            length += 1;
        }
        data.skip_bytes(length)?;
    }
    data.int()?;
    Ok(FileFormat {
        kind: Type::Wave,
        encoding,
        sample_rate: sample_rate as f32,
        sample_size_in_bits,
        channels: i32::from(channels),
        properties: Vec::new(),
    })
}

/// A RIFF file as the JDK's `RIFFReader` walks it: the root chunk and its children, each
/// reading through its parent within what the parent has left.
struct Riff<'a> {
    data: &'a [u8],
    at: usize,
    /// What the root chunk has left to read.
    root: u64,
}

/// A child chunk of the root: its four-character code, size and what it has left.
struct Chunk {
    code: [u8; 4],
    size: u64,
    left: u64,
}

impl<'a> Riff<'a> {
    /// `read()` of the root: a byte of the file within the root's size.
    fn root_read(&mut self) -> Option<u8> {
        if self.root == 0 {
            return None;
        }
        let Some(&byte) = self.data.get(self.at) else {
            self.root = 0;
            return None;
        };
        self.at += 1;
        self.root -= 1;
        Some(byte)
    }

    /// `skip(n)` of the root over `SkipFullyInputStream`: up to its size, failing where the
    /// file is shorter.
    fn root_skip(&mut self, count: u64) -> Read<u64> {
        if count == 0 || self.root == 0 {
            return Ok(0);
        }
        let count = count.min(self.root);
        let count_bytes = usize::try_from(count).map_err(|_| Rejected::Unsupported)?;
        if self.data.len() - self.at < count_bytes {
            return Err(Rejected::Unsupported);
        }
        self.at += count_bytes;
        self.root -= count;
        Ok(count)
    }

    /// The `RIFFReader` of a file: its code, and its type where it is `RIFF` or `LIST`.
    fn open(data: &'a [u8]) -> Read<(Self, [u8; 4], Option<[u8; 4]>)> {
        let mut riff = Self {
            data,
            at: 0,
            root: 0xFFFF_FFFF,
        };
        let header = riff.header(Self::root_read)?;
        let Some((code, size)) = header else {
            return Ok((riff, [0; 4], None));
        };
        riff.root = size;
        let kind = if &code == b"RIFF" || &code == b"LIST" {
            Some(riff.four(Self::root_read)?)
        } else {
            None
        };
        Ok((riff, code, kind))
    }

    /// The zero bytes skipped, then a chunk's code and size; `None` at the end of the data.
    fn header(&mut self, read: fn(&mut Self) -> Option<u8>) -> Read<Option<([u8; 4], u64)>> {
        let first = loop {
            match read(self) {
                None => return Ok(None),
                Some(0) => {}
                Some(byte) => break byte,
            }
        };
        let rest = self.four_from(read, 3)?;
        let code = [first, rest[0], rest[1], rest[2]];
        let size = u32::from_le_bytes(self.four(read)?);
        Ok(Some((code, u64::from(size))))
    }

    fn four(&mut self, read: fn(&mut Self) -> Option<u8>) -> Read<[u8; 4]> {
        self.four_from(read, 4)
    }

    /// `count` bytes, at most four, or an `EOFException`.
    fn four_from(&mut self, read: fn(&mut Self) -> Option<u8>, count: usize) -> Read<[u8; 4]> {
        let mut bytes = [0; 4];
        for byte in bytes.iter_mut().take(count) {
            *byte = read(self).ok_or(Rejected::Unsupported)?;
        }
        Ok(bytes)
    }

    /// `nextChunk` while `hasNextChunk`: the next child, or `None` once the root has nothing
    /// left.
    fn next_chunk(&mut self) -> Read<Option<Chunk>> {
        if self.root == 0 {
            return Ok(None);
        }
        let header = self.header(Self::root_read)?;
        let Some((code, size)) = header else {
            return Ok(Some(Chunk {
                code: [0; 4],
                size: 0,
                left: 0,
            }));
        };
        let mut chunk = Chunk {
            code,
            size,
            left: size,
        };
        if &code == b"RIFF" || &code == b"LIST" {
            for _ in 0..4 {
                self.chunk_read(&mut chunk).ok_or(Rejected::Unsupported)?;
            }
        }
        Ok(Some(chunk))
    }

    /// `read()` of a child: a byte within its size and the root's.
    fn chunk_read(&mut self, chunk: &mut Chunk) -> Option<u8> {
        if chunk.left == 0 {
            return None;
        }
        match self.root_read() {
            Some(byte) => {
                chunk.left -= 1;
                Some(byte)
            }
            None => {
                chunk.left = 0;
                None
            }
        }
    }

    fn chunk_unsigned_short(&mut self, chunk: &mut Chunk) -> Read<u32> {
        let low = self.chunk_read(chunk);
        let high = self.chunk_read(chunk);
        match (low, high) {
            (Some(low), Some(high)) => Ok(u32::from(low) | u32::from(high) << 8),
            _ => Err(Rejected::Unsupported),
        }
    }

    fn chunk_unsigned_int(&mut self, chunk: &mut Chunk) -> Read<u64> {
        let bytes = [
            self.chunk_read(chunk),
            self.chunk_read(chunk),
            self.chunk_read(chunk),
            self.chunk_read(chunk),
        ];
        match bytes {
            [Some(a), Some(b), Some(c), Some(d)] => Ok(u64::from(u32::from_le_bytes([a, b, c, d]))),
            _ => Err(Rejected::Unsupported),
        }
    }

    fn chunk_unsigned_byte(&mut self, chunk: &mut Chunk) -> Read<u8> {
        self.chunk_read(chunk).ok_or(Rejected::Unsupported)
    }

    /// `finish` of a child: what it has left skipped, within the root.
    fn finish(&mut self, chunk: &mut Chunk) -> Read<()> {
        if chunk.left == 0 {
            return Ok(());
        }
        let mut remaining = chunk.left;
        while remaining > 0 {
            let skipped = self.root_skip(remaining)?.min(remaining);
            let skipped = if skipped == 0 {
                if self.root_read().is_none() {
                    chunk.left = 0;
                    return Ok(());
                }
                1
            } else {
                skipped
            };
            remaining -= skipped;
            chunk.left -= skipped;
        }
        Ok(())
    }
}

/// The RIFF `WAVE` readers' walk to the `fmt ` and `data` chunks: `read_format` reads the format
/// chunk, the data chunk's size ends the walk.
fn riff_wave<T>(
    bytes: &[u8],
    mut read_format: impl FnMut(&mut Riff, &mut Chunk) -> Read<T>,
) -> Read<(T, u64)> {
    let (mut riff, code, kind) = Riff::open(bytes)?;
    if &code != b"RIFF" || kind.as_ref() != Some(b"WAVE") {
        return Err(Rejected::Unsupported);
    }
    let mut format = None;
    while let Some(mut chunk) = riff.next_chunk()? {
        if &chunk.code == b"fmt " {
            format = Some(read_format(&mut riff, &mut chunk)?);
        }
        if &chunk.code == b"data" {
            let format = format.ok_or(Rejected::Unsupported)?;
            return Ok((format, chunk.size));
        }
        riff.finish(&mut chunk)?;
    }
    Err(Rejected::Unsupported)
}

/// `WaveFloatFileReader`: IEEE float WAVE files.
fn wave_float(bytes: &[u8]) -> Read<FileFormat> {
    let ((channels, sample_rate, bits), _) = riff_wave(bytes, |riff, chunk| {
        if riff.chunk_unsigned_short(chunk)? != 3 {
            return Err(Rejected::Unsupported);
        }
        let channels = riff.chunk_unsigned_short(chunk)?;
        let sample_rate = riff.chunk_unsigned_int(chunk)?;
        riff.chunk_unsigned_int(chunk)?;
        if riff.chunk_unsigned_short(chunk)? == 0 {
            return Err(Rejected::Unsupported);
        }
        let bits = riff.chunk_unsigned_short(chunk)?;
        Ok((channels, sample_rate, bits))
    })?;
    Ok(FileFormat {
        kind: Type::Wave,
        encoding: Encoding::PcmFloat,
        sample_rate: sample_rate as f32,
        sample_size_in_bits: bits as i32,
        channels: channels as i32,
        properties: Vec::new(),
    })
}

/// `WaveExtensibleFileReader.decodeChannelMask`.
fn channel_order(mask: u64) -> Option<String> {
    const NAMES: [&str; 18] = [
        "FL", "FR", "FC", "LF", "BL", "BR", "FLC", "FLR", "BC", "SL", "SR", "TC", "TFL", "TFC",
        "TFR", "TBL", "TBC", "TBR",
    ];
    let names: Vec<String> = (0..64)
        .filter(|&bit| mask & (1 << bit) != 0)
        .map(|bit| match NAMES.get(bit) {
            Some(name) => (*name).to_owned(),
            None => format!("w{}", bit + 1),
        })
        .collect();
    (!names.is_empty()).then(|| names.join(" "))
}

/// `WaveExtensibleFileReader`: WAVE files with an extensible format of PCM or float samples.
fn wave_extensible(bytes: &[u8]) -> Read<FileFormat> {
    const PCM_TAIL: [u8; 12] = [0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71];
    let (format, data_size) = riff_wave(bytes, |riff, chunk| {
        if riff.chunk_unsigned_short(chunk)? != 0xFFFE {
            return Err(Rejected::Unsupported);
        }
        let channels = riff.chunk_unsigned_short(chunk)?;
        let sample_rate = riff.chunk_unsigned_int(chunk)?;
        riff.chunk_unsigned_int(chunk)?;
        let frame_size = riff.chunk_unsigned_short(chunk)?;
        let bits = riff.chunk_unsigned_short(chunk)?;
        if riff.chunk_unsigned_short(chunk)? != 22 {
            return Err(Rejected::Unsupported);
        }
        let valid_bits = riff.chunk_unsigned_short(chunk)?;
        if valid_bits > bits {
            return Err(Rejected::Unsupported);
        }
        let mask = riff.chunk_unsigned_int(chunk)?;
        let sub_format = riff.chunk_unsigned_int(chunk)?;
        let mut tail = [0u8; 12];
        let [s1, s2] = [
            riff.chunk_unsigned_short(chunk)?,
            riff.chunk_unsigned_short(chunk)?,
        ];
        tail[..2].copy_from_slice(&u16::try_from(s1).expect("16 bits").to_le_bytes());
        tail[2..4].copy_from_slice(&u16::try_from(s2).expect("16 bits").to_le_bytes());
        for byte in &mut tail[4..] {
            *byte = riff.chunk_unsigned_byte(chunk)?;
        }
        Ok((
            channels,
            sample_rate,
            frame_size,
            bits,
            valid_bits,
            mask,
            sub_format,
            tail,
        ))
    })?;
    let (channels, sample_rate, frame_size, bits, valid_bits, mask, sub_format, tail) = format;
    let mut properties = JavaHashMap::default();
    if let Some(order) = channel_order(mask) {
        properties.insert("channelOrder", order);
    }
    if mask != 0 {
        properties.insert("channelMask", mask.to_string());
    }
    properties.insert("validBitsPerSample", valid_bits.to_string());
    let encoding = match (sub_format, tail) {
        (1, PCM_TAIL) if bits == 8 => Encoding::PcmUnsigned,
        (1, PCM_TAIL) => Encoding::PcmSigned,
        (3, PCM_TAIL) => Encoding::PcmFloat,
        _ => return Err(Rejected::Unsupported),
    };
    if frame_size == 0 {
        return Err(Rejected::Thrown(format!(
            "ArithmeticException: / by zero, the frame length of {data_size} bytes"
        )));
    }
    let properties = properties
        .copied()
        .iter()
        .map(|(name, value)| (name.to_owned(), value.clone()))
        .collect();
    Ok(FileFormat {
        kind: Type::Wave,
        encoding,
        sample_rate: sample_rate as f32,
        sample_size_in_bits: bits as i32,
        channels: channels as i32,
        properties,
    })
}

const AU_MAGIC: i32 = 0x2e73_6e64;
const AU_HEADER_SIZE: i32 = 24;

/// `AuFileReader`.
fn au(bytes: &[u8]) -> Read<FileFormat> {
    let mut data = Data::new(bytes);
    if data.int()? != AU_MAGIC {
        return Err(Rejected::Unsupported);
    }
    let header_size = data.int()?;
    if header_size < AU_HEADER_SIZE {
        return Err(Rejected::Unsupported);
    }
    data.int()?;
    let kind = data.int()?;
    let sample_rate = data.int()?;
    if sample_rate <= 0 {
        return Err(Rejected::Unsupported);
    }
    let channels = data.int()?;
    if channels <= 0 {
        return Err(Rejected::Unsupported);
    }
    let (encoding, bits) = match kind {
        1 => (Encoding::Ulaw, 8),
        27 => (Encoding::Alaw, 8),
        2 => (Encoding::PcmSigned, 8),
        3 => (Encoding::PcmSigned, 16),
        4 => (Encoding::PcmSigned, 24),
        5 => (Encoding::PcmSigned, 32),
        6 => (Encoding::PcmFloat, 32),
        7 => (Encoding::PcmFloat, 64),
        _ => return Err(Rejected::Unsupported),
    };
    data.skip_bytes(header_size - AU_HEADER_SIZE)?;
    if pcm_frame_size(bits, channels) <= 0 {
        return Err(Rejected::Unsupported);
    }
    Ok(FileFormat {
        kind: Type::Au,
        encoding,
        sample_rate: sample_rate as f32,
        sample_size_in_bits: bits,
        channels,
        properties: Vec::new(),
    })
}

const FORM: i32 = 0x464f_524d;
const AIFC: i32 = 0x4149_4643;
const COMM: i32 = 0x434f_4d4d;
const SSND: i32 = 0x5353_4e44;
const FVER: i32 = 0x4656_4552;
const AIFC_PCM: i32 = 0x4e4f_4e45;
const AIFC_ULAW: i32 = 0x756c_6177;

/// `AiffFileReader`.
fn aiff(bytes: &[u8]) -> Read<FileFormat> {
    let mut data = Data::new(bytes);
    if data.int()? != FORM {
        return Err(Rejected::Unsupported);
    }
    data.int()?;
    let aifc = data.int()? == AIFC;
    let mut format = None;
    loop {
        let name = data.int()?;
        let length = data.int()?;
        let mut read = 0i32;
        match name {
            FVER => {}
            COMM => {
                if (!aifc && length < 18) || (aifc && length < 22) {
                    return Err(Rejected::Unsupported);
                }
                let channels = i32::from(data.unsigned_short()?);
                if channels <= 0 {
                    return Err(Rejected::Unsupported);
                }
                data.int()?;
                let mut bits = i32::from(data.unsigned_short()?);
                if !(1..=32).contains(&bits) {
                    return Err(Rejected::Unsupported);
                }
                let sample_rate = ieee_extended(&mut data)? as f32;
                read += 18;
                let mut encoding = Encoding::PcmSigned;
                if aifc {
                    match data.int()? {
                        AIFC_PCM => {}
                        AIFC_ULAW => {
                            encoding = Encoding::Ulaw;
                            bits = 8;
                        }
                        _ => return Err(Rejected::Unsupported),
                    }
                    read += 4;
                }
                format = Some(FileFormat {
                    kind: if aifc { Type::Aifc } else { Type::Aiff },
                    encoding,
                    sample_rate,
                    sample_size_in_bits: bits,
                    channels,
                    properties: Vec::new(),
                });
            }
            SSND => {
                data.int()?;
                data.int()?;
                return format.ok_or(Rejected::Unsupported);
            }
            _ => {}
        }
        let skip = length.wrapping_sub(read);
        if skip > 0 {
            data.skip_bytes(skip)?;
        }
    }
}

/// `read_ieee_extended`: an 80-bit extended float as the JDK reads it, in doubles.
fn ieee_extended(data: &mut Data) -> Read<f64> {
    let exponent = i32::from(data.unsigned_short()?);
    let high = i64::from(data.unsigned_short()?) << 16 | i64::from(data.unsigned_short()?);
    let low = i64::from(data.unsigned_short()?) << 16 | i64::from(data.unsigned_short()?);
    if exponent == 0 && high == 0 && low == 0 {
        return Ok(0.0);
    }
    if exponent == 0x7FFF {
        return Ok(3.402_823_466_385_288_6e38);
    }
    let exponent = exponent - 16383 - 31;
    let value = high as f64 * power_of_two(exponent);
    Ok(value + low as f64 * power_of_two(exponent - 32))
}

/// `Math.pow(2, exponent)`, which is exact for a power of two.
fn power_of_two(exponent: i32) -> f64 {
    match exponent {
        1024.. => f64::INFINITY,
        -1022..=1023 => f64::from_bits(u64::try_from(exponent + 1023).expect("positive") << 52),
        -1074..=-1023 => f64::from_bits(1 << (exponent + 1074)),
        _ => 0.0,
    }
}

/// `AudioSystem.getAudioFileFormat`: the readers in the order the JDK's service loader lists
/// them, reversed: the extensible, float and plain WAVE readers, the MIDI reader, which takes
/// no file this parser is given but one that starts as MIDI, then AU and AIFF. `None` where
/// none takes the file.
fn file_format(bytes: &[u8]) -> Result<Option<FileFormat>, Decline<'static>> {
    if bytes.starts_with(b"MThd") {
        return Err(Decline::Deferred(
            "a file that starts as MIDI, which the JDK's MIDI audio reader may take".to_owned(),
        ));
    }
    let readers: [Reader; 5] = [wave_extensible, wave_float, wave, au, aiff];
    for reader in readers {
        match reader(bytes) {
            Ok(format) => return Ok(Some(format)),
            Err(Rejected::Unsupported) => {}
            Err(Rejected::Thrown(thrown)) => return Err(Decline::Fails(Error::ParseError(thrown))),
        }
    }
    Ok(None)
}

pub struct AudioParser;

impl Parser for AudioParser {
    fn name(&self) -> &'static str {
        "extractous::AudioParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.audio.AudioParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "audio/basic",
            "audio/vnd.wave",
            "audio/x-wav",
            "audio/x-aiff",
        ]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let format = file_format(input)?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            let Some(format) = format else {
                metadata.add(EXCEPTION_WARNING, UNSUPPORTED);
                return Ok(());
            };
            metadata.set("Content-Type", format.kind.content_type());
            if format.channels != NOT_SPECIFIED {
                metadata.set("audio:channels", format.channels.to_string());
            }
            if format.sample_rate != NOT_SPECIFIED as f32 {
                metadata.set(
                    "xmpDM:audioSampleRate",
                    (format.sample_rate as i32).to_string(),
                );
            }
            let bits = format.sample_size_in_bits;
            if bits != NOT_SPECIFIED {
                metadata.set("audio:bits-per-sample", bits.to_string());
                let sample_type = match bits {
                    8 => Some("8Int"),
                    16 => Some("16Int"),
                    32 => Some("32Int"),
                    _ => None,
                };
                if let Some(sample_type) = sample_type {
                    metadata.set("xmpDM:audioSampleType", sample_type);
                }
            }
            metadata.set("audio:encoding", format.encoding.name());
            for (name, value) in &format.properties {
                metadata.add_prefixed("audio:", name, value.clone());
            }
            Ok(())
        }))
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

    const TIKA_NAME: &str = "org.apache.tika.parser.audio.AudioParser";

    fn wav(format: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut body = b"WAVE".to_vec();
        body.extend_from_slice(extra);
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&u32::try_from(format.len()).unwrap().to_le_bytes());
        body.extend_from_slice(format);
        body.extend_from_slice(b"data\x04\x00\x00\x00\x00\x00\x00\x00");
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        file.extend_from_slice(&body);
        file
    }

    fn format(tag: u16, channels: u16, rate: u32, block: u16, bits: u16) -> Vec<u8> {
        let mut format = tag.to_le_bytes().to_vec();
        format.extend_from_slice(&channels.to_le_bytes());
        format.extend_from_slice(&rate.to_le_bytes());
        format.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        format.extend_from_slice(&block.to_le_bytes());
        format.extend_from_slice(&bits.to_le_bytes());
        format
    }

    #[test]
    fn extracts_audio_files_as_tikas_path_does() {
        let scratch = Scratch::new("audio");
        let mut extensible = format(0xFFFE, 2, 48000, 6, 24);
        extensible.extend_from_slice(&[22, 0, 20, 0]);
        extensible.extend_from_slice(&3u32.to_le_bytes());
        extensible.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38]);
        extensible.extend_from_slice(&[0x9B, 0x71]);
        let mut au = b".snd".to_vec();
        for value in [28u32, 8, 1, 8000, 1, 0] {
            au.extend_from_slice(&value.to_be_bytes());
        }
        au.extend_from_slice(&[0; 8]);
        let read = [
            scratch.write(
                "pcm8.wav",
                &wav(&format(1, 1, 22050, 1, 8), b"LIST\x02\x00\x00\x00ab"),
            ),
            scratch.write("float.wav", &wav(&format(3, 2, 44100, 8, 32), b"")),
            scratch.write("extensible.wav", &wav(&extensible, b"")),
            scratch.write("ulaw.au", &au),
            scratch.write("adpcm.wav", &wav(&format(2, 1, 8000, 1, 4), b"")),
        ];
        let (parsed, deferred) = documents(&AudioParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_audio_files_as_tikas_path_does() {
        let (parsed, deferred) = documents(&AudioParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 3, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
