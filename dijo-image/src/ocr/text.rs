//! Our Java `OcrText`: corrections to text tesseract read that need more than the character it
//! read.
//!
//! - Romanian writes s and t with a comma below (ș, ț), Turkish and Gagauz with a cedilla
//!   (ş, ţ), and tesseract cannot tell the two marks apart: it reads Romanian scans with
//!   cedillas. A reading that also holds an ă, which only Romanian of these writes, gets the
//!   commas.
//! - Tesseract reads the Greek mu as the micro sign, which Unicode deems the same letter: one
//!   beside a Greek letter becomes mu.
//! - Tesseract reads Lithuanian's preposition į ("into") as j, which is no Lithuanian word. In a
//!   reading that holds both ė and ų, which together only Lithuanian writes, a j standing alone
//!   becomes į.

use unicode_script::Script;

use crate::unicode::{is_letter, is_mark, script};

const MICRO_SIGN: char = '\u{B5}';
const MU: char = '\u{3BC}';

/// The text, corrected.
pub(crate) fn corrected(text: &str) -> String {
    lithuanian_into(&greek_mu(&romanian_commas(text)))
}

fn romanian_commas(text: &str) -> String {
    if !text.contains(['ă', 'Ă']) {
        return text.to_owned();
    }
    text.chars()
        .map(|c| match c {
            'ş' => 'ș',
            'Ş' => 'Ș',
            'ţ' => 'ț',
            'Ţ' => 'Ț',
            other => other,
        })
        .collect()
}

fn greek_mu(text: &str) -> String {
    if !text.contains(MICRO_SIGN) {
        return text.to_owned();
    }
    let characters: Vec<char> = text.chars().collect();
    let is_greek = |index: Option<usize>| {
        index
            .and_then(|index| characters.get(index))
            .is_some_and(|&c| is_letter(c) && script(c) == Script::Greek)
    };
    characters
        .iter()
        .enumerate()
        .map(|(index, &c)| {
            if c == MICRO_SIGN && (is_greek(index.checked_sub(1)) || is_greek(Some(index + 1))) {
                MU
            } else {
                c
            }
        })
        .collect()
}

fn lithuanian_into(text: &str) -> String {
    if !text.contains('ė') || !text.contains('ų') {
        return text.to_owned();
    }
    let characters: Vec<char> = text.chars().collect();
    let word_like = |index: Option<usize>| {
        index
            .and_then(|index| characters.get(index))
            .is_some_and(|&c| is_letter(c) || is_mark(c))
    };
    characters
        .iter()
        .enumerate()
        .map(|(index, &c)| {
            if c == 'j' && !word_like(index.checked_sub(1)) && !word_like(Some(index + 1)) {
                'į'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrects_as_ocr_text_does() {
        assert_eq!(corrected("ţară şi"), "țară și");
        assert_eq!(corrected("ţara şi"), "ţara şi");
        assert_eq!(corrected("5 µm, µα"), "5 µm, μα");
        assert_eq!(corrected("ėjo j namus ų j"), "ėjo į namus ų į");
        assert_eq!(corrected("jo j namus"), "jo j namus");
    }
}
