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
//! - Early modern print sets s as the long s (ſ), which tesseract's models read as f: `fu cafa`
//!   for *su casa*. On a page where at least [`MIN_LONG_S_WORDS`] words are not in the language's
//!   dictionary as read but are with some of their f read as s, each becomes that word.

use std::ops::Range;

use unicode_script::Script;

use crate::unicode::{is_letter, is_mark, script};

const MICRO_SIGN: char = '\u{B5}';
const MU: char = '\u{3BC}';
const LONG_S: char = '\u{17F}';
/// How many words of a page must be words of its language only with long s read as s for the
/// page to be taken for early modern print. A modern page has at most one, a word its dictionary
/// lacks (`wafted`, which reads as `wasted`); a page of early modern print has dozens.
const MIN_LONG_S_WORDS: usize = 3;
/// The most f of a word tried as long s, so that a word takes at most fifteen lookups.
const MAX_LONG_S: usize = 4;

/// The text, corrected.
pub(crate) fn corrected(text: &str) -> String {
    long_s_as_s(&lithuanian_into(&greek_mu(&romanian_commas(text))))
}

fn long_s_as_s(text: &str) -> String {
    text.replace(LONG_S, "s")
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

/// The text with each word that `is_word` holds only with some of its f read as long s written
/// with s, the fewest such f first and only when one word results, if at least
/// [`MIN_LONG_S_WORDS`] words are; else the text as it is.
pub(crate) fn with_long_s(text: &str, is_word: &dyn Fn(&str) -> bool) -> String {
    let restored: Vec<(Range<usize>, String)> = word_ranges(text)
        .filter_map(|range| long_s_word(&text[range.clone()], is_word).map(|word| (range, word)))
        .collect();
    if restored.len() < MIN_LONG_S_WORDS {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (range, word) in restored {
        out.push_str(&text[copied..range.start]);
        out.push_str(&word);
        copied = range.end;
    }
    out.push_str(&text[copied..]);
    out
}

pub(crate) fn lexicality(text: &str, is_word: &dyn Fn(&str) -> bool) -> f64 {
    let (words, known) = word_ranges(text).fold((0u64, 0u64), |(words, known), range| {
        (words + 1, known + u64::from(is_word(&text[range])))
    });
    if words == 0 {
        return 0.0;
    }
    known as f64 / words as f64
}

fn word_ranges(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut start = None;
    text.char_indices()
        .chain([(text.len(), ' ')])
        .filter_map(
            move |(index, c)| match (start, is_letter(c) || is_mark(c)) {
                (None, true) => {
                    start = Some(index);
                    None
                }
                (Some(from), false) => {
                    start = None;
                    Some(from..index)
                }
                _ => None,
            },
        )
}

/// The word `is_word` holds with the fewest of `word`'s f read as s, when `word` is no word as it
/// is and exactly one such word is.
fn long_s_word(word: &str, is_word: &dyn Fn(&str) -> bool) -> Option<String> {
    let at: Vec<usize> = word.match_indices('f').map(|(index, _)| index).collect();
    if at.is_empty() || at.len() > MAX_LONG_S || is_word(word) {
        return None;
    }
    let read = |chosen: u32| {
        let mut bytes = word.as_bytes().to_vec();
        for (bit, &index) in at.iter().enumerate() {
            if chosen >> bit & 1 == 1 {
                bytes[index] = b's';
            }
        }
        String::from_utf8(bytes).expect("f and s are both one byte")
    };
    for count in 1..=at.len() as u32 {
        let mut words = (1..1u32 << at.len())
            .filter(|chosen| chosen.count_ones() == count)
            .map(read)
            .filter(|candidate| is_word(candidate));
        if let Some(found) = words.next() {
            return words.next().is_none().then_some(found);
        }
    }
    None
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
        assert_eq!(
            corrected("Geſchichte ſeiner Zeit"),
            "Geschichte seiner Zeit"
        );
    }

    #[test]
    fn lexicality_is_the_share_of_words_the_dictionary_holds() {
        assert_eq!(lexicality("su casa, efta es", &spanish), 0.75);
        assert_eq!(lexicality("su casa es esta", &spanish), 1.0);
        assert_eq!(lexicality("12 345, ...", &spanish), 0.0);
    }

    fn spanish(word: &str) -> bool {
        [
            "su", "casa", "es", "esta", "nuestro", "fuego", "sus", "este", "Famoso", "sossiego",
            "fase", "safe",
        ]
        .contains(&word)
    }

    #[test]
    fn early_modern_print_has_its_long_s_restored() {
        assert_eq!(
            with_long_s("En fu cafa, efta es nueftra; Famofo fueGo.", &spanish),
            "En su casa, esta es nueftra; Famoso fueGo."
        );
        assert_eq!(
            with_long_s("cafa efte nueftro foffiego", &spanish),
            "casa este nuestro sossiego"
        );
    }

    #[test]
    fn a_page_with_few_such_words_is_left_as_read() {
        assert_eq!(
            with_long_s("La cafa y el fuego.", &spanish),
            "La cafa y el fuego."
        );
    }

    #[test]
    fn a_word_that_reads_as_two_words_is_left_as_read() {
        assert_eq!(long_s_word("fafe", &spanish), None);
        assert_eq!(long_s_word("fuego", &spanish), None);
        assert_eq!(long_s_word("Fu", &spanish), None);
        assert_eq!(long_s_word("cafa", &spanish).as_deref(), Some("casa"));
    }
}
