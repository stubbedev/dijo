//! Standard MIDI files, as Tika's `MidiParser` reads them through the JDK's
//! `StandardMidiFileReader`: the number of tracks and the division type as metadata, and each
//! track as a paragraph of its text meta events, in the order the JDK's `Track` keeps its events,
//! by tick. A file the reader rejects as invalid MIDI writes nothing; one it cannot read to the end
//! of a track fails.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::{Decline, Parser, Prepared};

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

/// What `MidiParser` reads of a sequence: its division and the text events of each track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Sequence {
    division: Division,
    tracks: Vec<Vec<String>>,
}

/// `StandardMidiFileReader.getSequence` from a stream that skips with `skipping`.
pub(super) fn sequence(bytes: &[u8], skipping: Skipping) -> Result<Sequence, Unread> {
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
    let mut read = Vec::new();
    for _ in 0..tracks.max(0) {
        let Some(track) = next_track(&mut data, skipping)? else {
            break;
        };
        read.push(read_track(track)?);
    }
    Ok(Sequence {
        division,
        tracks: read,
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

/// `SMFParser.readTrack`: the text meta events of a track in the order `Track.add` keeps
/// them, by tick, an event after those of the same tick.
fn read_track(bytes: &[u8]) -> Result<Vec<String>, Unread> {
    let mut track = TrackData { bytes, at: 0 };
    let mut events: Vec<(i64, Option<String>)> = Vec::new();
    let mut tick: i64 = 0;
    let mut running_status = 0u8;
    while !track.finished() {
        tick = tick.wrapping_add(track.var_int()?);
        let first = track.unsigned()?;
        let (status, has_data1) = if first >= 0x80 {
            if first & 0xF0 != 0xF0 {
                running_status = first;
            }
            (first, false)
        } else {
            (running_status, true)
        };
        let text = match status & 0xF0 {
            0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => {
                if !has_data1 {
                    track.unsigned()?;
                }
                track.unsigned()?;
                None
            }
            0xC0 | 0xD0 => {
                if !has_data1 {
                    track.unsigned()?;
                }
                None
            }
            0xF0 => match status {
                0xF0 | 0xF7 => {
                    let length = track.var_int()?;
                    track.message(length)?;
                    None
                }
                0xFF => {
                    let kind = track.unsigned()?;
                    let length = track.var_int()?;
                    let data = track.message(length)?;
                    if kind >= 128 {
                        return Err(Unread::Invalid);
                    }
                    if kind == END_OF_TRACK {
                        break;
                    }
                    (1..=15)
                        .contains(&kind)
                        .then(|| data.iter().map(|&byte| char::from(byte)).collect())
                }
                _ => return Err(Unread::Invalid),
            },
            _ => return Err(Unread::Invalid),
        };
        let at = events
            .iter()
            .rposition(|(earlier, _)| *earlier <= tick)
            .map_or(0, |index| index + 1);
        events.insert(at, (tick, text));
    }
    Ok(events.into_iter().filter_map(|(_, text)| text).collect())
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

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let sequence = match sequence(input, Skipping::Available) {
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
            for track in &sequence.tracks {
                xhtml.start_element("p")?;
                for text in track {
                    xhtml.characters(text)?;
                }
                xhtml.end_element("p")?;
            }
            Ok(())
        }))
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

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

    #[test]
    fn extracts_midi_files_as_tikas_path_does() {
        let scratch = Scratch::new("midi");
        let text = b"\x00\xFF\x03\x05Titel\x10\x90\x3C\x40\x05\x3E\x40\x00\xFF\x01\x04late\
                     \x00\xFF\x05\x03ord\x00\xF0\x02\x01\xF7\x00\xFF\x2F\x00";
        let read = [
            scratch.write(
                "song.mid",
                &midi([0x01, 0xE0], &[text, b"\x00\xFF\x2F\x00"]),
            ),
            scratch.write(
                "smpte.mid",
                &midi([0xE7, 0x28], &[b"\x00\xFF\x01\x01x\x00\xFF\x2F\x00"]),
            ),
            scratch.write(
                "meta.mid",
                &midi([0x00, 0x60], &[b"\x00\xFF\x90\x00\x00\xFF\x2F\x00"]),
            ),
        ];
        let failing = [scratch.write(
            "cut.mid",
            &midi([0x00, 0x60], &[b"\x00\xFF\x01\x01x\x00\x90"]),
        )];
        let (parsed, deferred) = documents(&MidiParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_midi_files_as_tikas_path_does() {
        let (parsed, deferred) = documents(&MidiParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 1, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
