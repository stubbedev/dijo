//! Tika's `OneNoteLegacyDumpStrings`: the runs of printable ASCII, then of UTF-16LE characters
//! below U+0100, of a OneNote file whose structure Tika does not read, each written when it
//! looks like words: enough of it letters or whitespace.
//!
//! The file is scanned whole, where Tika reads it in buffers of a million bytes, never looks at
//! the last byte of each, writes a run open at the end of one there and again with what
//! follows, and counts a character as a letter where its index, not the character, is
//! whitespace (#122). A run is counted as it is scanned and written from the file once it ends,
//! so that nothing proportional to the file is held.

use crate::formats::xhtml::{Stop, Xhtml};
use crate::input::{Pages, Paging};
use crate::unicode::is_whitespace;

const MIN_STRING_LENGTH: usize = 8;
const ACCEPTABLE_ALPHA_TO_OTHER_CHAR_RATIO: f32 = 0.6;
/// The bytes scanned between two counts of the pages read.
const BLOCK: usize = 1 << 20;
/// The characters of a run written at a time.
const PIECE: usize = 1 << 16;

/// How the characters of a run are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Width {
    /// A byte each, printable ASCII.
    Ascii,
    /// Two bytes each, UTF-16LE below U+0100 but the controls.
    Utf16,
}

impl Width {
    /// The character of this width at `i` of `data`, if one is there.
    fn character(self, data: &[u8], i: usize) -> Option<u8> {
        let low = data[i];
        match self {
            Width::Ascii => (0x20..0x7f).contains(&low).then_some(low),
            Width::Utf16 => (*data.get(i + 1)? == 0 && low >= 0x20).then_some(low),
        }
    }

    fn bytes(self) -> usize {
        match self {
            Width::Ascii => 1,
            Width::Utf16 => 2,
        }
    }
}

/// A run of characters of one width in the file, by where it starts and how many it holds,
/// with what `writeIfUseful` counts of it: its spaces, the words its spaces part, and the
/// characters that are letters or whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    width: Width,
    start: usize,
    length: usize,
    spaces: usize,
    words: usize,
    alphabetic: usize,
}

impl Run {
    fn new(width: Width, start: usize) -> Self {
        Run {
            width,
            start,
            length: 0,
            spaces: 0,
            words: 0,
            alphabetic: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        let character = char::from(byte);
        if byte == b' ' {
            self.spaces += 1;
        } else {
            self.words = self.spaces + 1;
        }
        if character.is_alphabetic() || is_whitespace(character) {
            self.alphabetic += 1;
        }
        self.length += 1;
    }

    /// `writeIfUseful`: long enough, of more than one word, enough of it letters or
    /// whitespace.
    fn is_useful(&self) -> bool {
        self.length >= MIN_STRING_LENGTH
            && self.words > 1
            && self.alphabetic as f32 / self.length as f32 > ACCEPTABLE_ALPHA_TO_OTHER_CHAR_RATIO
    }

    /// Its characters read from `data` as Latin-1, [`PIECE`] at a time.
    fn pieces<'d>(&self, data: &'d [u8]) -> impl Iterator<Item = String> + 'd {
        let step = self.width.bytes();
        let end = self.start + (self.length - 1) * step + 1;
        data[self.start..end]
            .chunks(PIECE * step)
            .map(move |piece| {
                piece
                    .iter()
                    .step_by(step)
                    .map(|&byte| char::from(byte))
                    .collect()
            })
    }
}

/// `dump`: the ASCII runs, then the UTF-16LE ones, each that looks like words a line, read from
/// `data`, whose pages are `pages`.
pub(super) fn dump(data: &[u8], pages: Pages, xhtml: &mut Xhtml) -> Result<(), Stop> {
    let paging = Paging::new(pages);
    for width in [Width::Ascii, Width::Utf16] {
        each_useful_run(data, width, &paging, |run| {
            for piece in run.pieces(data) {
                xhtml.characters(&piece)?;
            }
            xhtml.characters("\n")
        })?;
        paging.release();
    }
    Ok(())
}

/// Hands `each` the useful runs of characters of `width` in `data`, in order, counting the bytes
/// scanned read on `paging`.
fn each_useful_run<E>(
    data: &[u8],
    width: Width,
    paging: &Paging,
    mut each: impl FnMut(&Run) -> Result<(), E>,
) -> Result<(), E> {
    let mut run = Run::new(width, 0);
    let mut i = 0;
    let mut counted = 0;
    while i < data.len() {
        if i - counted >= BLOCK {
            paging.read(i - counted);
            counted = i;
        }
        match width.character(data, i) {
            Some(byte) => {
                run.push(byte);
                i += width.bytes() - 1;
            }
            None => {
                if run.is_useful() {
                    each(&run)?;
                }
                run = Run::new(width, i + 1);
            }
        }
        i += 1;
    }
    paging.read(data.len().saturating_sub(counted));
    if run.is_useful() {
        each(&run)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use extractous_core::large::LargeFile;

    use super::*;

    /// The runs of `data` that look like words: the ASCII ones, then the UTF-16LE ones.
    fn useful_runs(data: &[u8]) -> Vec<String> {
        let paging = Paging::new(Pages::IN_MEMORY);
        let mut lines = Vec::new();
        for width in [Width::Ascii, Width::Utf16] {
            each_useful_run(data, width, &paging, |run| {
                lines.push(run.pieces(data).collect::<String>());
                Ok::<_, ()>(())
            })
            .unwrap();
        }
        lines
    }

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn dumps_each_run_once_to_the_files_last_byte() {
        let mut data = vec![0u8; 999_995];
        data.extend(b"Havnen ligger stille");
        assert_eq!(useful_runs(&data), ["Havnen ligger stille"]);
    }

    #[test]
    fn counts_whitespace_as_wordlike() {
        let mut data = b"\x01ab 12, 34 cd\x01".to_vec();
        data.extend(utf16("Monday, August 08, 2016"));
        assert_eq!(useful_runs(&data), ["Monday, August 08, 2016"]);
        assert!(useful_runs(b"1234 5678 90").is_empty());
        assert!(useful_runs(b"single-word").is_empty());
        assert!(useful_runs(b"   trailing-word").len() == 1);
        assert!(useful_runs(b"leading-word     ").is_empty());
    }

    #[test]
    fn writes_a_run_longer_than_a_piece_whole() {
        let run = "ord og ".repeat(PIECE);
        let mut data = vec![0u8];
        data.extend(utf16(&run));
        assert_eq!(useful_runs(&data), [run]);
    }

    /// A OneNote section whose root file node list lies past its end, so that Tika dumps
    /// its runs, followed by runs past what is read whole: the dump scans them in bounded
    /// memory, each written as it ends, to the last.
    #[test]
    fn dumps_a_section_too_large_to_read_whole_run_by_run() {
        let mut head = std::fs::read("../test_files/documents/onenote/section.one").unwrap();
        head.truncate(1024);
        head[172..180].copy_from_slice(&(i64::MAX as u64).to_le_bytes());
        let line = |index: u64| format!("Havnen ligger stille nummer {index:010}\0").into_bytes();
        let count = extractous_core::large::bodies_past_limit(line(0).len());
        let file = LargeFile::new("large.one", &head, count, line, b"the very last words\0");
        let mut last = String::new();
        let mut lines = 0u64;
        let metadata = file.extract_lines("extractous::OneNoteParser", |text| {
            if text.starts_with("Havnen") {
                lines += 1;
            }
            last = text.to_owned();
        });
        assert_eq!(lines, count);
        assert_eq!(last, "the very last words");
        assert!(metadata["tk:exception:warn"]
            .iter()
            .any(|warning| warning.contains("legacy text dump")));
    }
}
