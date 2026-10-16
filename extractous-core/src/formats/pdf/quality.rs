//! Our Java `TextLayerQuality`: how bad the text a page's text layer yields is, from glyphs that
//! map to no character, characters no real text holds, symbols where letters should be, words
//! mixing alphabets, Latin letters with too few vowels, and Arabic words broken into fragments.
//! Each signal is scaled so that 1 means garbage; the page's badness is the worst of them.

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::UnicodeNormalization;
use unicode_script::Script;

use crate::unicode::{is_letter, is_mark, is_space_char, is_whitespace, script};

/// A text layer this bad or worse may read worse than OCR.
pub(super) const DOUBTFUL: f64 = 0.5;

const INVALID_SHARE: f64 = 0.3;
const SYMBOL_SHARE: f64 = 0.5;
const MIXED_SCRIPT_SHARE: f64 = 0.3;
const NATURAL_VOWEL_SHARE: f64 = 0.32;
const GARBAGE_VOWEL_SHARE: f64 = 0.16;
const ONE_LETTER_ARABIC_SHARE: f64 = 0.3;
/// A text layer this bad or worse is garbage, which OCR text replaces.
pub(super) const GARBAGE: f64 = 1.0;
const MIN_SAMPLE_CHARACTERS: u32 = 20;
const MIN_SAMPLE_WORDS: u32 = 10;
const MIN_SAMPLE_LATIN_LETTERS: u32 = 50;
const MIN_SAMPLE_ARABIC_WORDS: u32 = 20;
const SYMBOL_PUNCTUATION: &str = "#%&*@\\_";
const VOWELS: &str = "aeiouyAEIOUY";

/// `TextLayerQuality.Assessment`: the badness, and the signal that decided it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Assessment {
    pub(super) badness: f64,
    pub(super) fault: &'static str,
}

impl Assessment {
    fn worst(self, badness: f64, fault: &'static str) -> Self {
        if badness > self.badness {
            Self { badness, fault }
        } else {
            self
        }
    }
}

#[derive(Debug, Default)]
struct Counts {
    visible: u32,
    invalid: u32,
    letters: u32,
    digits: u32,
    symbols: u32,
    alphabet_words: u32,
    mixed_script_words: u32,
    latin_letters: u32,
    latin_vowels: u32,
    arabic_words: u32,
    one_letter_arabic_words: u32,
}

/// `TextLayerQuality.assess`: the page's `text`, from `glyphs` of which `unmapped` map to no
/// character, against the unmapped glyphs that make a page garbage, a share of its glyphs below
/// 1 and a count from 1.
pub(super) fn assess(text: &str, glyphs: i32, unmapped: i32, unmapped_limit: f32) -> Assessment {
    let counts = count(text);
    if counts.visible == 0 {
        return Assessment {
            badness: GARBAGE,
            fault: "no-text",
        };
    }
    let limit = f64::from(unmapped_limit);
    let unmapped_badness = if unmapped_limit < 1.0 {
        f64::from(unmapped) / f64::from(glyphs.max(1)) / limit
    } else {
        f64::from(unmapped) / limit
    };
    let mut assessment = Assessment {
        badness: f64::from(counts.invalid) / f64::from(counts.visible) / INVALID_SHARE,
        fault: "invalid-characters",
    };
    if unmapped_badness >= GARBAGE {
        assessment = assessment.worst(unmapped_badness, "unmapped-glyphs");
    }
    let letters_digits_symbols = counts.letters + counts.digits + counts.symbols;
    if letters_digits_symbols >= MIN_SAMPLE_CHARACTERS {
        assessment = assessment.worst(
            f64::from(counts.symbols) / f64::from(letters_digits_symbols) / SYMBOL_SHARE,
            "symbols",
        );
    }
    if counts.alphabet_words >= MIN_SAMPLE_WORDS {
        assessment = assessment.worst(
            f64::from(counts.mixed_script_words)
                / f64::from(counts.alphabet_words)
                / MIXED_SCRIPT_SHARE,
            "mixed-scripts",
        );
    }
    if counts.latin_letters >= MIN_SAMPLE_LATIN_LETTERS {
        let vowel_share = f64::from(counts.latin_vowels) / f64::from(counts.latin_letters);
        assessment = assessment.worst(
            (NATURAL_VOWEL_SHARE - vowel_share) / (NATURAL_VOWEL_SHARE - GARBAGE_VOWEL_SHARE),
            "too-few-vowels",
        );
    }
    if counts.arabic_words >= MIN_SAMPLE_ARABIC_WORDS {
        assessment = assessment.worst(
            f64::from(counts.one_letter_arabic_words)
                / f64::from(counts.arabic_words)
                / ONE_LETTER_ARABIC_SHARE,
            "fragmented-words",
        );
    }
    assessment
}

fn count(text: &str) -> Counts {
    let mut counts = Counts::default();
    let mut word = String::new();
    for c in text.chars() {
        if is_word_character(c) {
            word.push(c);
        } else {
            count_word(&word, &mut counts);
            word.clear();
        }
        if is_whitespace(c) || is_space_char(c) {
            continue;
        }
        counts.visible += 1;
        if is_invalid(c) {
            counts.invalid += 1;
        } else if is_word_character(c) {
            counts.letters += 1;
            if script(c) == Script::Latin {
                counts.latin_letters += 1;
                if is_vowel(c) {
                    counts.latin_vowels += 1;
                }
            }
        } else if get_general_category(c) == GeneralCategory::DecimalNumber {
            counts.digits += 1;
        } else if is_symbol(c) {
            counts.symbols += 1;
        }
    }
    count_word(&word, &mut counts);
    counts
}

fn is_word_character(c: char) -> bool {
    is_letter(c) || is_mark(c)
}

fn is_invalid(c: char) -> bool {
    c == '\u{FFFD}'
        || matches!(
            get_general_category(c),
            GeneralCategory::PrivateUse
                | GeneralCategory::Unassigned
                | GeneralCategory::Surrogate
                | GeneralCategory::Control
        )
}

fn is_symbol(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::MathSymbol
            | GeneralCategory::CurrencySymbol
            | GeneralCategory::ModifierSymbol
            | GeneralCategory::OtherSymbol
    ) || SYMBOL_PUNCTUATION.contains(c)
}

fn count_word(word: &str, counts: &mut Counts) {
    if word.is_empty() {
        return;
    }
    let mut alphabets: Vec<Script> = Vec::new();
    let mut letters = 0;
    let mut arabic = false;
    for c in word.chars().filter(|&c| is_letter(c)) {
        letters += 1;
        let script = script(c);
        if matches!(script, Script::Latin | Script::Greek | Script::Cyrillic)
            && !alphabets.contains(&script)
        {
            alphabets.push(script);
        }
        arabic |= script == Script::Arabic;
    }
    if arabic {
        counts.arabic_words += 1;
        if letters == 1 {
            counts.one_letter_arabic_words += 1;
        }
    }
    if letters < 2 || alphabets.is_empty() {
        return;
    }
    counts.alphabet_words += 1;
    if alphabets.len() > 1 {
        counts.mixed_script_words += 1;
    }
}

/// Whether a Latin letter is a vowel, or one with marks on it.
fn is_vowel(c: char) -> bool {
    let base = if c.is_ascii() {
        Some(c)
    } else {
        std::iter::once(c).nfd().next()
    };
    base.is_some_and(|base| VOWELS.contains(base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judges_text_layers_as_our_java_does() {
        let clean = "Det var en gang en konge, som havde tre døtre og et stort slot ved havet.";
        let assessment = assess(clean, 70, 0, 10.0);
        assert!(assessment.badness < DOUBTFUL, "{assessment:?}");
        assert_eq!(assess(" \n\t", 3, 0, 10.0).fault, "no-text");
        let unmapped = assess(clean, 70, 30, 10.0);
        assert_eq!((unmapped.badness, unmapped.fault), (3.0, "unmapped-glyphs"));
        let consonants = "bcdfg hjklm npqrs tvwxz bcdfg hjklm npqrs tvwxz bcdfg hjklm npqrs";
        assert_eq!(assess(consonants, 60, 0, 10.0).fault, "too-few-vowels");
        let symbols = "#%&*@ #%&*@ #%&*@ abc def ghi jkl";
        assert_eq!(assess(symbols, 30, 0, 10.0).fault, "symbols");
        assert_eq!(
            assess("\u{E000}\u{E001} ok", 4, 0, 10.0).fault,
            "invalid-characters"
        );
        assert!(!is_vowel('ø') && is_vowel('é') && is_vowel('Å'));
    }
}
