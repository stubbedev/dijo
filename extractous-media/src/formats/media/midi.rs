//! Standard MIDI files, as Tika's `MidiParser` reads them through the JDK's
//! `StandardMidiFileReader`: the number of tracks and the division type as metadata, and each
//! track as a paragraph of its text meta events, in the order the JDK's `Track` keeps its events,
//! by tick. A file the reader rejects as invalid MIDI writes nothing; one it cannot read to the end
//! of a track fails.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};

const MTHD: i32 = 0x4d54_6864;
const MTRK: i32 = 0x4d54_726b;
const END_OF_TRACK: u8 = 0x2F;
const TIKA_NAME: &str = "org.apache.tika.parser.audio.MidiParser";
/// The message of the `EOFException`s `SMFParser` throws.
const INVALID_FILE: &str = "invalid MIDI file";

/// How the stream the JDK reads a file from skips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Skipping {
    /// As many bytes of a count as there are: the `TikaInputStream` `MidiParser` reads from.
    Available,
    /// All of a count or an `EOFException`, and a negative count, which only a skip of the
    /// header's remainder asks for, an `IllegalArgumentException`: the `SkipFullyInputStream`
    /// `AudioParser` reads from.
    Fully,
}

/// Why the JDK gives no sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Unread {
    /// `InvalidMidiDataException`.
    Invalid,
    /// An `EOFException`, with its message, if it has one.
    EndOfFile(Option<String>),
    /// The `IllegalArgumentException` commons-io's `IOUtils.skip` throws on the negative
    /// remainder of a header shorter than six bytes, under [`Skipping::Fully`].
    NegativeSkip(i32),
}

impl Unread {
    /// What the JDK throws where it gives no sequence, which fails the parse: `None` for an
    /// invalid file.
    pub(super) fn thrown(self) -> Option<Thrown> {
        match self {
            Unread::Invalid => None,
            Unread::EndOfFile(None) => Some(Thrown::Io(Exception::bare("java.io.EOFException"))),
            Unread::EndOfFile(Some(message)) => {
                Some(Thrown::Io(Exception::new("java.io.EOFException", message)))
            }
            Unread::NegativeSkip(count) => Some(negative_skip(count)),
        }
    }
}

/// The `IllegalArgumentException` commons-io's `IOUtils.skip` throws on a negative `count`.
pub(super) fn negative_skip(count: i32) -> Thrown {
    Thrown::Runtime(Exception::new(
        "java.lang.IllegalArgumentException",
        format!("Skip count must be non-negative, actual: {count}"),
    ))
}

fn invalid_file() -> Unread {
    Unread::EndOfFile(Some(INVALID_FILE.to_owned()))
}

/// A `DataInputStream` over the file.
struct Data<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Data<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Unread> {
        let bytes = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or(Unread::EndOfFile(None))?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn int(&mut self) -> Result<i32, Unread> {
        Ok(i32::from_be_bytes(self.take()?))
    }

    fn short(&mut self) -> Result<i16, Unread> {
        Ok(i16::from_be_bytes(self.take()?))
    }

    /// `skipBytes` with `skipping`: nothing of a count that is not positive, else as many bytes
    /// of `count` as there are, or all of them or an `EOFException`.
    fn skip_bytes(&mut self, count: i32, skipping: Skipping) -> Result<i32, Unread> {
        if count <= 0 {
            return Ok(0);
        }
        let available = i32::try_from(self.bytes.len() - self.at).unwrap_or(i32::MAX);
        let skipped = match skipping {
            Skipping::Available => count.min(available),
            Skipping::Fully if count > available => {
                return Err(Unread::EndOfFile(Some(format!(
                    "Bytes to skip: {count} actual: {available}"
                ))))
            }
            Skipping::Fully => count,
        };
        self.at += usize::try_from(skipped).expect("not negative");
        Ok(skipped)
    }
}

/// The division type `Sequence` names by its constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Division {
    Ppq,
    Smpte24,
    Smpte25,
    Smpte30,
    Smpte30Drop,
}

impl Division {
    fn name(self) -> &'static str {
        match self {
            Division::Ppq => "PPQ",
            Division::Smpte24 => "SMPTE_24",
            Division::Smpte25 => "SMPTE_25",
            Division::Smpte30 => "SMPTE_30",
            Division::Smpte30Drop => "SMPTE_30DROP",
        }
    }
}

/// What `MidiParser` reads of a sequence: its division and its tracks, each read for its text
/// events again as it is written.
pub(super) struct Sequence<'a> {
    division: Division,
    tracks: Vec<Track<'a>>,
    pages: Pages<'a>,
}

impl Sequence<'_> {
    /// Writes each track as a paragraph of its text events, releasing the pages read as it goes.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let paging = Paging::new(self.pages);
        for track in &self.tracks {
            xhtml.start_element("p")?;
            for text in track.texts() {
                xhtml.characters(&text)?;
            }
            xhtml.end_element("p")?;
            paging.read(track.data.len());
        }
        Ok(())
    }
}

/// A track's data, which reads to its end, and the order of its text events.
struct Track<'a> {
    data: &'a [u8],
    order: Order,
}

/// The order in which `Track.add` keeps a track's text events, by tick, each after those of
/// the same tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// The order of the data: the tick of no text event is less than that of one before it.
    AsRead,
    /// Sorted: a text event has a tick less than one before it, which only a delta of more than
    /// 63 bits, wrapping, gives.
    ByTick,
}

impl Track<'_> {
    /// The text of its text events, in the order `Track.add` keeps them.
    fn texts(&self) -> Box<dyn Iterator<Item = String> + '_> {
        let texts = Texts::of(self.data)
            .map(|text| text.expect("the track read to its end when the sequence was read"));
        match self.order {
            Order::AsRead => Box::new(texts.map(|text| text.characters())),
            Order::ByTick => {
                let mut sorted: Vec<Text> = Vec::new();
                for text in texts {
                    let at = sorted
                        .iter()
                        .rposition(|earlier| earlier.tick <= text.tick)
                        .map_or(0, |index| index + 1);
                    sorted.insert(at, text);
                }
                Box::new(sorted.into_iter().map(|text| text.characters()))
            }
        }
    }
}

/// `StandardMidiFileReader.getSequence` from a stream that skips with `skipping`, each track
/// read to its end, keeping none of its events, and the `pages` of `bytes` released as they are.
pub(super) fn sequence<'a>(
    bytes: &'a [u8],
    pages: Pages<'a>,
    skipping: Skipping,
) -> Result<Sequence<'a>, Unread> {
    let mut data = Data { bytes, at: 0 };
    if data.int()? != MTHD {
        return Err(Unread::Invalid);
    }
    let remaining = data.int()?.wrapping_sub(6);
    let kind = data.short()?;
    let tracks = data.short()?;
    let timing = data.short()?;
    let division = if timing > 0 {
        Division::Ppq
    } else {
        match -i32::from(timing >> 8) {
            24 => Division::Smpte24,
            25 => Division::Smpte25,
            29 => Division::Smpte30Drop,
            30 => Division::Smpte30,
            _ => return Err(Unread::Invalid),
        }
    };
    if remaining < 0 && skipping == Skipping::Fully {
        return Err(Unread::NegativeSkip(remaining));
    }
    data.skip_bytes(remaining, skipping)?;
    if kind != 0 && kind != 1 {
        return Err(Unread::Invalid);
    }
    let paging = Paging::new(pages);
    let mut read = Vec::new();
    for _ in 0..tracks.max(0) {
        let start = data.at;
        let Some(track) = next_track(&mut data, skipping)? else {
            break;
        };
        read.push(Track {
            data: track,
            order: order(track)?,
        });
        paging.read(data.at - start);
    }
    Ok(Sequence {
        division,
        tracks: read,
        pages,
    })
}

/// `SMFParser.nextTrack`: the data of the next `MTrk` chunk, other chunks skipped with
/// `skipping`; `None` where a skip or the track's data falls short.
fn next_track<'a>(data: &mut Data<'a>, skipping: Skipping) -> Result<Option<&'a [u8]>, Unread> {
    let mut length = 0;
    loop {
        if data.skip_bytes(length, skipping)? != length {
            return Ok(None);
        }
        let magic = data.int().map_err(|_| invalid_file())?;
        length = data.int().map_err(|_| invalid_file())?;
        if magic == MTRK {
            break;
        }
    }
    let Ok(length) = usize::try_from(length) else {
        return Ok(None);
    };
    let Some(track) = data.bytes.get(data.at..data.at + length) else {
        return Ok(None);
    };
    data.at += length;
    Ok(Some(track))
}

/// A track's data as `SMFParser` reads it: past its end is an `ArrayIndexOutOfBoundsException`,
/// which it rethrows as an `EOFException`.
struct TrackData<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> TrackData<'a> {
    fn finished(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn unsigned(&mut self) -> Result<u8, Unread> {
        let byte = *self.bytes.get(self.at).ok_or_else(invalid_file)?;
        self.at += 1;
        Ok(byte)
    }

    /// `readVarInt`, its `long` arithmetic wrapping.
    fn var_int(&mut self) -> Result<i64, Unread> {
        let mut value: i64 = 0;
        loop {
            let current = self.unsigned()?;
            value = value
                .wrapping_shl(7)
                .wrapping_add(i64::from(current & 0x7F));
            if current & 0x80 == 0 {
                return Ok(value);
            }
        }
    }

    /// A message's data of the length its `(int)` cast gives, which must lie within the rest of
    /// the track.
    fn message(&mut self, length: i64) -> Result<&'a [u8], Unread> {
        let length = usize::try_from(length as i32).map_err(|_| Unread::Invalid)?;
        if length > self.bytes.len() - self.at {
            return Err(Unread::Invalid);
        }
        let data = &self.bytes[self.at..self.at + length];
        self.at += length;
        Ok(data)
    }
}

/// A text meta event of a track: its tick and its text's bytes.
struct Text<'a> {
    tick: i64,
    bytes: &'a [u8],
}

impl Text<'_> {
    /// The text, each byte a character, as `MidiParser` makes it.
    fn characters(&self) -> String {
        self.bytes.iter().map(|&byte| char::from(byte)).collect()
    }
}

/// The text meta events of a track's data as `SMFParser.readTrack` reads its events, up to the
/// end of the data or the end-of-track event; or why the track does not read, which ends them.
struct Texts<'a> {
    track: TrackData<'a>,
    tick: i64,
    running_status: u8,
    ended: bool,
}

impl<'a> Texts<'a> {
    fn of(bytes: &'a [u8]) -> Self {
        Self {
            track: TrackData { bytes, at: 0 },
            tick: 0,
            running_status: 0,
            ended: false,
        }
    }

    /// The next text event, `None` at the end of the track.
    fn next_text(&mut self) -> Result<Option<Text<'a>>, Unread> {
        let track = &mut self.track;
        while !track.finished() {
            self.tick = self.tick.wrapping_add(track.var_int()?);
            let first = track.unsigned()?;
            let (status, has_data1) = if first >= 0x80 {
                if first & 0xF0 != 0xF0 {
                    self.running_status = first;
                }
                (first, false)
            } else {
                (self.running_status, true)
            };
            match status & 0xF0 {
                0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => {
                    if !has_data1 {
                        track.unsigned()?;
                    }
                    track.unsigned()?;
                }
                0xC0 | 0xD0 => {
                    if !has_data1 {
                        track.unsigned()?;
                    }
                }
                0xF0 => match status {
                    0xF0 | 0xF7 => {
                        let length = track.var_int()?;
                        track.message(length)?;
                    }
                    0xFF => {
                        let kind = track.unsigned()?;
                        let length = track.var_int()?;
                        let bytes = track.message(length)?;
                        if kind >= 128 {
                            return Err(Unread::Invalid);
                        }
                        if kind == END_OF_TRACK {
                            return Ok(None);
                        }
                        if (1..=15).contains(&kind) {
                            return Ok(Some(Text {
                                tick: self.tick,
                                bytes,
                            }));
                        }
                    }
                    _ => return Err(Unread::Invalid),
                },
                _ => return Err(Unread::Invalid),
            }
        }
        Ok(None)
    }
}

impl<'a> Iterator for Texts<'a> {
    type Item = Result<Text<'a>, Unread>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let text = self.next_text();
        self.ended = !matches!(text, Ok(Some(_)));
        text.transpose()
    }
}

/// `SMFParser.readTrack` of a track's data, keeping none of its events: the order of its text
/// events, or why it does not read.
fn order(bytes: &[u8]) -> Result<Order, Unread> {
    let mut order = Order::AsRead;
    let mut last = i64::MIN;
    for text in Texts::of(bytes) {
        let tick = text?.tick;
        if tick < last {
            order = Order::ByTick;
        }
        last = tick;
    }
    Ok(order)
}

pub struct MidiParser;

impl Parser for MidiParser {
    fn name(&self) -> &'static str {
        "extractous::MidiParser"
    }
    fn tika_name(&self) -> &'static str {
        TIKA_NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-midi", "audio/midi"]
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
        let sequence = match sequence(input, pages, Skipping::Available) {
            Ok(sequence) => Some(sequence),
            Err(unread) => match unread.thrown() {
                None => None,
                Some(thrown) => return Err(Decline::Fails(thrown.into_error(TIKA_NAME))),
            },
        };
        Ok(Box::new(move |xhtml| {
            xhtml.metadata_mut().set("Content-Type", "audio/midi");
            let Some(sequence) = &sequence else {
                return Ok(());
            };
            let metadata = xhtml.metadata_mut();
            metadata.set("midi:tracks", sequence.tracks.len().to_string());
            metadata.set("midi:patches", "0");
            metadata.set("midi:division-type", sequence.division.name());
            sequence.write(xhtml)
        }))
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use extractous_core::large::{bodies_past_limit, LargeFile};

    fn midi(timing: [u8; 2], tracks: &[&[u8]]) -> Vec<u8> {
        let mut file = b"MThd\x00\x00\x00\x06\x00\x01".to_vec();
        file.extend_from_slice(&u16::try_from(tracks.len()).unwrap().to_be_bytes());
        file.extend_from_slice(&timing);
        for track in tracks {
            file.extend_from_slice(b"MTrk");
            file.extend_from_slice(&u32::try_from(track.len()).unwrap().to_be_bytes());
            file.extend_from_slice(track);
        }
        file
    }

    /// Text events come in the order of their ticks, as `Track.add` keeps them, also where a
    /// delta wraps the tick back.
    #[test]
    fn orders_text_events_by_tick() {
        let back_one = [[0xFF; 9].as_slice(), &[0x7F]].concat();
        let data = [
            b"\x05\xFF\x01\x01a\x00\xFF\x01\x01b".as_slice(),
            &back_one,
            b"\xFF\x01\x01c\x00\xFF\x2F\x00",
        ]
        .concat();
        let track = Track {
            order: order(&data).unwrap(),
            data: &data,
        };
        assert_eq!(track.order, Order::ByTick);
        assert_eq!(track.texts().collect::<Vec<_>>(), ["c", "a", "b"]);
        let in_order = b"\x00\xFF\x01\x01a\x05\x90\x3C\x40\x00\xFF\x01\x01b";
        assert_eq!(order(in_order).unwrap(), Order::AsRead);
    }

    /// A file too large to read whole is read a track at a time, in place, its pages released
    /// as they are read, keeping none of its events: its text is read again as it is written.
    #[test]
    fn extracts_midi_files_too_large_to_read_whole_in_bounded_memory() {
        let mut events = b"\x00\xFF\x05\x04vers".to_vec();
        for _ in 0..1000 {
            events.extend_from_slice(b"\x00\x90\x3C\x40");
        }
        let end = b"\x00\xFF\x2F\x00";
        let count = bodies_past_limit(events.len());
        let length = u32::try_from(count * events.len() as u64 + end.len() as u64).unwrap();
        let mut head = midi([0x01, 0xE0], &[]);
        head[11] = 1;
        head.extend_from_slice(b"MTrk");
        head.extend_from_slice(&length.to_be_bytes());
        let file = LargeFile::new("long.mid", &head, count, |_| events.clone(), end);
        let (text, metadata) = file.extract(MidiParser.name());
        assert_eq!(metadata["midi:tracks"], ["1"]);
        assert_eq!(text.trim(), "vers".repeat(usize::try_from(count).unwrap()));
    }
}
