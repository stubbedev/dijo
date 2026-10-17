//! Tika's `OneNoteLegacyDumpStrings`: the runs of printable ASCII, then of UTF-16LE characters
//! below U+0100, of a OneNote file whose structure Tika does not read, each written when it
//! looks like words.
//!
//! The file is read in buffers of a million bytes, the last byte of each never looked at, and
//! a run still open at the end of a buffer is written there and again with what follows.

use crate::formats::xhtml::{Stop, Xhtml};
use crate::unicode::is_whitespace;

const MIN_STRING_LENGTH: usize = 8;
const ACCEPTABLE_ALPHA_TO_OTHER_CHAR_RATIO: f32 = 0.6;
const BUFFER_SIZE: usize = 1_000_000;

/// `dump`: the ASCII runs, then the UTF-16LE ones.
pub(super) fn dump(data: &[u8], xhtml: &mut Xhtml) -> Result<(), Stop> {
    dump_runs(data, xhtml, |buffer, i| {
        let byte = buffer[i];
        ((0x20..0x7f).contains(&byte)).then_some((byte, 0))
    })?;
    dump_runs(data, xhtml, |buffer, i| {
        let (low, high) = (buffer[i], buffer[i + 1]);
        (high == 0 && low >= 0x20).then_some((low, 1))
    })
}

/// The runs of the characters `character` takes at each position of a buffer, with how many
/// more bytes each takes up.
fn dump_runs(
    data: &[u8],
    xhtml: &mut Xhtml,
    character: impl Fn(&[u8], usize) -> Option<(u8, usize)>,
) -> Result<(), Stop> {
    let mut run = Vec::new();
    for buffer in data.chunks(BUFFER_SIZE) {
        let mut i = 0;
        while i + 1 < buffer.len() {
            match character(buffer, i) {
                Some((byte, skipped)) => {
                    run.push(byte);
                    i += skipped;
                }
                None => {
                    if run.len() >= MIN_STRING_LENGTH {
                        write_if_useful(&run, xhtml)?;
                    }
                    run.clear();
                }
            }
            i += 1;
        }
        if run.len() >= MIN_STRING_LENGTH {
            write_if_useful(&run, xhtml)?;
        }
    }
    Ok(())
}

/// `writeIfUseful`: a run of more than one word, enough of it letters, as a line. Tika counts a
/// character as a letter also where its index is a whitespace code point.
fn write_if_useful(run: &[u8], xhtml: &mut Xhtml) -> Result<(), Stop> {
    let text = crate::java::latin1(run);
    let mut words: Vec<&str> = text.split(' ').collect();
    while words.last().is_some_and(|word| word.is_empty()) {
        words.pop();
    }
    if words.len() <= 1 {
        return Ok(());
    }
    let alphabetic = text
        .chars()
        .enumerate()
        .filter(|&(i, c)| c.is_alphabetic() || char::from_u32(i as u32).is_some_and(is_whitespace))
        .count();
    let ratio = alphabetic as f32 / text.chars().count() as f32;
    if ratio > ACCEPTABLE_ALPHA_TO_OTHER_CHAR_RATIO {
        xhtml.characters(&text)?;
        xhtml.characters("\n")?;
    }
    Ok(())
}
