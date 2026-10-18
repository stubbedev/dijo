//! Tika's `OneNoteLegacyDumpStrings`: the runs of printable ASCII, then of UTF-16LE characters
//! below U+0100, of a OneNote file whose structure Tika does not read, each written when it
//! looks like words: enough of it letters or whitespace.
//!
//! The file is read whole, where Tika reads it in buffers of a million bytes, never looks at
//! the last byte of each, writes a run open at the end of one there and again with what
//! follows, and counts a character as a letter where its index, not the character, is
//! whitespace (#122).

use crate::formats::xhtml::{Stop, Xhtml};
use crate::unicode::is_whitespace;

const MIN_STRING_LENGTH: usize = 8;
const ACCEPTABLE_ALPHA_TO_OTHER_CHAR_RATIO: f32 = 0.6;

/// `dump`: the ASCII runs, then the UTF-16LE ones, each that looks like words a line.
pub(super) fn dump(data: &[u8], xhtml: &mut Xhtml) -> Result<(), Stop> {
    for line in useful_runs(data) {
        xhtml.characters(&line)?;
        xhtml.characters("\n")?;
    }
    Ok(())
}

/// The runs of the file that look like words: the ASCII ones, then the UTF-16LE ones.
fn useful_runs(data: &[u8]) -> Vec<String> {
    let mut lines = runs(data, |i| {
        let byte = data[i];
        ((0x20..0x7f).contains(&byte)).then_some((byte, 0))
    });
    lines.extend(runs(data, |i| {
        let (low, high) = (data[i], *data.get(i + 1)?);
        (high == 0 && low >= 0x20).then_some((low, 1))
    }));
    lines
}

/// The useful runs of the characters `character` takes at each position of the file, with how
/// many more bytes each takes up.
fn runs(data: &[u8], character: impl Fn(usize) -> Option<(u8, usize)>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut run = Vec::new();
    let mut i = 0;
    while i < data.len() {
        match character(i) {
            Some((byte, skipped)) => {
                run.push(byte);
                i += skipped;
            }
            None => {
                lines.extend(useful(&run));
                run.clear();
            }
        }
        i += 1;
    }
    lines.extend(useful(&run));
    lines
}

/// `writeIfUseful`: a run long enough, of more than one word, enough of it letters or
/// whitespace.
fn useful(run: &[u8]) -> Option<String> {
    if run.len() < MIN_STRING_LENGTH {
        return None;
    }
    let text = crate::java::latin1(run);
    let words = text
        .split(' ')
        .rev()
        .skip_while(|word| word.is_empty())
        .count();
    if words <= 1 {
        return None;
    }
    let alphabetic = text
        .chars()
        .filter(|&c| c.is_alphabetic() || is_whitespace(c))
        .count();
    let ratio = alphabetic as f32 / text.chars().count() as f32;
    (ratio > ACCEPTABLE_ALPHA_TO_OTHER_CHAR_RATIO).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
