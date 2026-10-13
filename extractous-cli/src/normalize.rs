use std::collections::HashSet;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

const MAX_HELD_BYTES: usize = 64 * 1024;
const MAX_CONSECUTIVE_NEWLINES: u8 = 2;
const MAX_WORD_CHARS: usize = 64;
const MAX_REMEMBERED_WORDS: usize = 1 << 16;

/// One normalisation step applied to extracted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Step {
    /// Drop control characters and zero-width spaces; keep the joiners scripts need.
    Invisible,
    /// Expand typographic ligatures such as ﬁ and ﬂ into their letters.
    Ligatures,
    /// Drop soft hyphens.
    SoftHyphens,
    /// Compose characters to Unicode NFC.
    Unicode,
    /// Turn exotic spaces into plain ones, collapse runs of spaces, end lines with \n, drop
    /// trailing spaces and keep at most one blank line in a row.
    Whitespace,
    /// Take lines that recur at the top or bottom of most pages (running headers and footers,
    /// page numbers) out of the page text: into the page's header and footer in pages output,
    /// away in text and json output.
    Headers,
    /// Join a word hyphenated across a line break, dropping the hyphen unless the text before
    /// shows the word is a hyphenated compound.
    Hyphenation,
    /// Read an untagged PDF page in reading order (columns, then by position) rather than in
    /// the order its content stream draws the text.
    ReadingOrder,
}

impl Step {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// The set of normalisation steps applied to extracted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Steps(u8);

impl Steps {
    pub const ALL: Steps = Steps(
        Step::Invisible.bit()
            | Step::Ligatures.bit()
            | Step::SoftHyphens.bit()
            | Step::Unicode.bit()
            | Step::Whitespace.bit()
            | Step::Headers.bit()
            | Step::Hyphenation.bit()
            | Step::ReadingOrder.bit(),
    );
    pub const NONE: Steps = Steps(0);

    /// Every step except the skipped ones, or none at all for raw output.
    pub fn selected(raw: bool, skipped: &[Step]) -> Steps {
        if raw {
            return Steps::NONE;
        }
        Steps(
            skipped
                .iter()
                .fold(Steps::ALL.0, |steps, step| steps & !step.bit()),
        )
    }

    pub fn contains(self, step: Step) -> bool {
        self.0 & step.bit() != 0
    }
}

/// Normalises UTF-8 text that arrives in chunks split anywhere, even inside a character, with
/// the same result as normalising it in one piece.
pub struct Normalizer {
    steps: Steps,
    undecoded: Vec<u8>,
    held: String,
    pending_space: bool,
    newlines: u8,
    after_carriage_return: bool,
    hyphenation: Option<Dehyphenator>,
}

impl Normalizer {
    pub fn new(steps: Steps) -> Self {
        Self {
            steps,
            undecoded: Vec::new(),
            held: String::new(),
            pending_space: false,
            newlines: 0,
            after_carriage_return: false,
            hyphenation: steps
                .contains(Step::Hyphenation)
                .then(Dehyphenator::default),
        }
    }

    /// Normalises the next chunk into `out`, holding back what may still change with the text
    /// that follows.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        if self.steps == Steps::NONE {
            out.extend_from_slice(bytes);
            return;
        }
        if self.undecoded.is_empty() {
            let consumed = self.decode(bytes);
            self.undecoded.extend_from_slice(&bytes[consumed..]);
        } else {
            let mut undecoded = std::mem::take(&mut self.undecoded);
            undecoded.extend_from_slice(bytes);
            let consumed = self.decode(&undecoded);
            undecoded.drain(..consumed);
            self.undecoded = undecoded;
        }
        let rest = self.held.split_off(self.stable_prefix_len());
        let ready = std::mem::replace(&mut self.held, rest);
        self.release(&ready, out);
    }

    /// Normalises everything still held into `out`, at the end of the text.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.steps == Steps::NONE {
            return;
        }
        let rest = std::mem::take(&mut self.undecoded);
        self.hold(&String::from_utf8_lossy(&rest));
        let ready = std::mem::take(&mut self.held);
        self.release(&ready, out);
        self.pending_space = false;
        if let Some(hyphenation) = &mut self.hyphenation {
            hyphenation.finish(out);
        }
    }

    /// Holds every complete character at the start of `bytes`, with U+FFFD for each invalid
    /// sequence, and returns how many bytes that consumed; the rest is an incomplete character.
    fn decode(&mut self, bytes: &[u8]) -> usize {
        let mut consumed = 0;
        loop {
            match std::str::from_utf8(&bytes[consumed..]) {
                Ok(valid) => {
                    self.hold(valid);
                    return bytes.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    self.hold(
                        std::str::from_utf8(&bytes[consumed..valid_end]).expect("validated prefix"),
                    );
                    match error.error_len() {
                        Some(invalid) => {
                            self.hold(char::REPLACEMENT_CHARACTER.encode_utf8(&mut [0; 4]));
                            consumed = valid_end + invalid;
                        }
                        None => return valid_end,
                    }
                }
            }
        }
    }

    /// Holds `text` with the characters the per-character steps drop or replace handled, copying
    /// the runs between them whole.
    fn hold(&mut self, mut text: &str) {
        while let Some((index, character)) = text
            .char_indices()
            .find(|&(_, character)| self.alters(character))
        {
            self.held.push_str(&text[..index]);
            if let Some(letters) = ligature(character) {
                self.held.push_str(letters);
            }
            text = &text[index + character.len_utf8()..];
        }
        self.held.push_str(text);
    }

    fn alters(&self, character: char) -> bool {
        (self.steps.contains(Step::Invisible) && is_invisible(character))
            || (self.steps.contains(Step::SoftHyphens) && character == '\u{ad}')
            || (self.steps.contains(Step::Ligatures) && ligature(character).is_some())
    }

    /// How much of the held text no later character can compose with.
    fn stable_prefix_len(&self) -> usize {
        if !self.steps.contains(Step::Unicode) || self.held.len() > MAX_HELD_BYTES {
            return self.held.len();
        }
        self.held
            .char_indices()
            .rev()
            .find(|&(_, character)| is_stable_starter(character))
            .map_or(0, |(index, _)| index)
    }

    fn release(&mut self, text: &str, out: &mut Vec<u8>) {
        if self.steps.contains(Step::Unicode) && is_nfc_quick(text.chars()) != IsNormalized::Yes {
            text.nfc().for_each(|character| self.emit(character, out));
        } else {
            text.chars().for_each(|character| self.emit(character, out));
        }
    }

    fn emit(&mut self, character: char, out: &mut Vec<u8>) {
        if self.steps.contains(Step::Whitespace) {
            self.collapse(character, out);
        } else {
            self.write(character, out);
        }
    }

    fn write(&mut self, character: char, out: &mut Vec<u8>) {
        match &mut self.hyphenation {
            Some(hyphenation) => hyphenation.push(character, out),
            None => push_char(character, out),
        }
    }

    fn collapse(&mut self, character: char, out: &mut Vec<u8>) {
        let after_carriage_return = std::mem::take(&mut self.after_carriage_return);
        if is_line_break(character) {
            if character == '\n' && after_carriage_return {
                return;
            }
            self.after_carriage_return = character == '\r';
            self.pending_space = false;
            if self.newlines < MAX_CONSECUTIVE_NEWLINES {
                self.write('\n', out);
            }
            self.newlines = self.newlines.saturating_add(1);
        } else if is_space(character) {
            self.pending_space = true;
        } else {
            if std::mem::take(&mut self.pending_space) {
                self.write(' ', out);
            }
            self.newlines = 0;
            self.write(character, out);
        }
    }
}

/// Joins words hyphenated across a line break. It holds back a hyphen that ends a line, and the
/// word that starts the next, until that word ends. Whether the hyphen stays is decided from the
/// text before: it stays when the word already holds a hyphen (`state-of-the-` / `art`), or when
/// the joined word did not occur but the hyphenated compound did, or both halves did as words of
/// their own (`south` and `west`, where hyphenation leaves fragments such as `extrac`).
#[derive(Default)]
struct Dehyphenator {
    token: String,
    held: Held,
    words: HashSet<String>,
    compounds: HashSet<String>,
}

#[derive(Default)]
enum Held {
    #[default]
    Nothing,
    /// A hyphen right after a letter.
    Hyphen(char),
    /// A hyphen, the line break after it, and the spaces and letters of the next line so far.
    Break {
        hyphen: char,
        text: String,
        word: String,
    },
}

impl Dehyphenator {
    fn push(&mut self, character: char, out: &mut Vec<u8>) {
        match std::mem::take(&mut self.held) {
            Held::Nothing => {}
            Held::Hyphen(hyphen) if is_line_break(character) => {
                self.held = Held::Break {
                    hyphen,
                    text: character.to_string(),
                    word: String::new(),
                };
                return;
            }
            Held::Hyphen(hyphen) => self.pass(hyphen, out),
            Held::Break {
                hyphen,
                mut text,
                mut word,
            } => {
                let starts_lowercase = word.chars().next().is_some_and(char::is_lowercase);
                if character.is_alphabetic()
                    && (!word.is_empty() || character.is_lowercase())
                    && word.chars().count() < MAX_WORD_CHARS
                {
                    word.push(character);
                    self.held = Held::Break { hyphen, text, word };
                    return;
                }
                if word.is_empty() && is_space(character) {
                    text.push(character);
                    self.held = Held::Break { hyphen, text, word };
                    return;
                }
                if starts_lowercase && !character.is_alphabetic() {
                    self.join(hyphen, &word, out);
                } else {
                    self.pass(hyphen, out);
                    text.chars()
                        .chain(word.chars())
                        .for_each(|held| self.pass(held, out));
                }
            }
        }
        if is_hyphen(character) && self.token.chars().last().is_some_and(char::is_alphabetic) {
            self.held = Held::Hyphen(character);
            return;
        }
        self.pass(character, out);
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        match std::mem::take(&mut self.held) {
            Held::Nothing => {}
            Held::Hyphen(hyphen) => self.pass(hyphen, out),
            Held::Break { hyphen, text, word } => {
                if word.chars().next().is_some_and(char::is_lowercase) {
                    self.join(hyphen, &word, out);
                } else {
                    self.pass(hyphen, out);
                    text.chars()
                        .chain(word.chars())
                        .for_each(|held| self.pass(held, out));
                }
            }
        }
        self.end_token();
    }

    /// Joins the word before the line break with `next`, the word after it.
    fn join(&mut self, hyphen: char, next: &str, out: &mut Vec<u8>) {
        let before = last_word(&self.token).to_lowercase();
        let next_lowercase = next.to_lowercase();
        let compound = self.token.contains(is_hyphen)
            || (!self.words.contains(&format!("{before}{next_lowercase}"))
                && (self
                    .compounds
                    .contains(&format!("{before}-{next_lowercase}"))
                    || (self.words.contains(&before) && self.words.contains(&next_lowercase))));
        if compound {
            self.pass(hyphen, out);
        }
        next.chars().for_each(|character| self.pass(character, out));
    }

    fn pass(&mut self, character: char, out: &mut Vec<u8>) {
        if character.is_whitespace() {
            self.end_token();
        } else if self.token.len() < MAX_WORD_CHARS * 4 {
            self.token.push(character);
        }
        push_char(character, out);
    }

    /// Remembers the words and hyphenated compounds of the token that just ended.
    fn end_token(&mut self) {
        let token = std::mem::take(&mut self.token).to_lowercase();
        let words: Vec<&str> = token
            .split(|character: char| !character.is_alphabetic() && !is_hyphen(character))
            .filter(|word| !word.is_empty())
            .collect();
        for word in words {
            let parts: Vec<&str> = word
                .split(is_hyphen)
                .filter(|part| !part.is_empty())
                .collect();
            if self.words.len() < MAX_REMEMBERED_WORDS {
                self.words.extend(parts.iter().map(|part| part.to_string()));
            }
            if self.compounds.len() < MAX_REMEMBERED_WORDS {
                self.compounds.extend(
                    parts
                        .windows(2)
                        .map(|pair| format!("{}-{}", pair[0], pair[1])),
                );
            }
        }
    }
}

fn last_word(token: &str) -> &str {
    let start = token
        .char_indices()
        .rev()
        .find(|&(_, character)| !character.is_alphabetic())
        .map_or(0, |(index, character)| index + character.len_utf8());
    &token[start..]
}

fn is_hyphen(character: char) -> bool {
    matches!(character, '-' | '\u{2010}')
}

fn push_char(character: char, out: &mut Vec<u8>) {
    let mut buffer = [0; 4];
    out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
}

/// Normalises a whole piece of text.
pub fn normalize(steps: Steps, text: &str) -> String {
    let mut normalizer = Normalizer::new(steps);
    let mut out = Vec::with_capacity(text.len());
    normalizer.push(text.as_bytes(), &mut out);
    normalizer.finish(&mut out);
    String::from_utf8(out).expect("normalisation produces UTF-8")
}

fn is_stable_starter(character: char) -> bool {
    canonical_combining_class(character) == 0
        && is_nfc_quick(std::iter::once(character)) == IsNormalized::Yes
}

fn is_invisible(character: char) -> bool {
    matches!(
        character,
        '\u{0}'..='\u{8}'
            | '\u{e}'..='\u{1f}'
            | '\u{7f}'..='\u{84}'
            | '\u{86}'..='\u{9f}'
            | '\u{200b}'
            | '\u{2060}'
            | '\u{feff}'
    )
}

fn is_line_break(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

fn is_space(character: char) -> bool {
    matches!(
        character,
        ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

fn ligature(character: char) -> Option<&'static str> {
    match character {
        '\u{fb00}' => Some("ff"),
        '\u{fb01}' => Some("fi"),
        '\u{fb02}' => Some("fl"),
        '\u{fb03}' => Some("ffi"),
        '\u{fb04}' => Some("ffl"),
        '\u{fb05}' | '\u{fb06}' => Some("st"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(step: Step) -> Steps {
        Steps(step.bit())
    }

    #[test]
    fn ligatures_expand_into_letters() {
        assert_eq!(
            normalize(
                only(Step::Ligatures),
                "\u{fb01}nd the o\u{fb03}ce \u{fb02}oor"
            ),
            "find the office floor"
        );
    }

    #[test]
    fn decomposed_characters_compose() {
        assert_eq!(normalize(only(Step::Unicode), "cafe\u{301}"), "café");
        assert_eq!(normalize(only(Step::Unicode), "\u{fb01}"), "\u{fb01}");
    }

    #[test]
    fn soft_hyphens_and_invisible_characters_are_dropped() {
        assert_eq!(
            normalize(only(Step::SoftHyphens), "extrac\u{ad}tion"),
            "extraction"
        );
        assert_eq!(
            normalize(
                only(Step::Invisible),
                "a\u{200b}b\u{feff}c\u{0}d\u{7}e\u{9b}f\u{200d}g\u{200c}h\tj\nk"
            ),
            "abcdef\u{200d}g\u{200c}h\tj\nk"
        );
    }

    #[test]
    fn whitespace_collapses_but_keeps_paragraphs_and_tabs() {
        assert_eq!(
            normalize(
                only(Step::Whitespace),
                "a \u{a0}\u{2009} b  \r\nc\r\rd\n\n\n\n e\tf\u{3000}g   "
            ),
            "a b\nc\n\nd\n\n e\tf g"
        );
    }

    #[test]
    fn words_hyphenated_across_lines_are_joined() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        assert_eq!(join("the extrac-\ntion of text"), "the extraction of text");
        assert_eq!(join("in-\n  dented"), "indented");
        assert_eq!(join("con-\ntinue-\nd"), "continued");
        assert_eq!(join("ends with hyphen-\nation"), "ends with hyphenation");
    }

    #[test]
    fn hyphenated_compounds_keep_their_hyphen() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        assert_eq!(
            join("a well-known fact, well-\nknown to all"),
            "a well-known fact, well-known to all"
        );
        assert_eq!(join("state-of-the-\nart"), "state-of-the-art");
        assert_eq!(
            join("re-entry and reentry, then re-\nentry"),
            "re-entry and reentry, then reentry"
        );
        assert_eq!(
            join("south and west: the south-\nwest"),
            "south and west: the south-west"
        );
        assert_eq!(
            join("some thing or something, some-\nthing"),
            "some thing or something, something"
        );
    }

    #[test]
    fn other_hyphens_at_line_ends_stay() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        for text in [
            "anti-\nSemitic",
            "1990-\n1995",
            "word -\nnext",
            "end-\n\nnext paragraph",
            "list:\n- item",
            "trailing-",
            "trailing-\n",
        ] {
            assert_eq!(join(text), text);
        }
    }

    #[test]
    fn nothing_changes_without_steps() {
        let text = "\u{fb01} cafe\u{301}  \u{200b}\r\n";
        assert_eq!(normalize(Steps::NONE, text), text);
    }

    #[test]
    fn steps_can_be_skipped() {
        assert_eq!(Steps::selected(true, &[]), Steps::NONE);
        assert_eq!(Steps::selected(false, &[]), Steps::ALL);
        let steps = Steps::selected(false, &[Step::Ligatures, Step::Whitespace]);
        assert!(!steps.contains(Step::Ligatures) && !steps.contains(Step::Whitespace));
        assert!(steps.contains(Step::Unicode) && steps.contains(Step::Invisible));
    }

    #[test]
    fn chunk_boundaries_do_not_change_the_result() {
        let text = "Re\u{301}sume\u{301} \u{fb01}nal  \u{a0} o\u{fb03}ce\r\n\r\n\r\n\
                    한국어 \u{1100}\u{1161}\u{11a8} e\u{301}\u{323} 漢字\u{200b}  tab\t\u{ad}end ";
        let whole = normalize(Steps::ALL, text);
        assert_eq!(whole, "Résumé final office\n\n한국어 각 ẹ́ 漢字 tab\tend");
        for size in 1..=7 {
            let mut normalizer = Normalizer::new(Steps::ALL);
            let mut out = Vec::new();
            for chunk in text.as_bytes().chunks(size) {
                normalizer.push(chunk, &mut out);
            }
            normalizer.finish(&mut out);
            assert_eq!(String::from_utf8(out).unwrap(), whole, "chunks of {size}");
        }
    }

    const ALPHABET: &[char] = &[
        'a', 'e', 'Z', '1', ' ', ' ', '\t', '\n', '\r', '\u{a0}', '\u{3000}', '\u{2028}',
        '\u{301}', '\u{323}', '\u{308}', '\u{1100}', '\u{1161}', '\u{11a8}', '\u{fb01}',
        '\u{fb03}', '\u{ad}', '\u{200b}', '\u{200d}', '\u{feff}', '\u{0}', '\u{85}', 'é', '漢',
        '🎉', '\u{0b47}', '\u{0b3e}', '-', '-', 'q', 'Q', '\u{2010}',
    ];

    fn random_texts() -> impl Iterator<Item = String> {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..500).map(move |_| {
            let length = (next() % 40) as usize;
            (0..length)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect()
        })
    }

    #[test]
    fn any_chunking_of_random_text_gives_the_same_result() {
        for text in random_texts() {
            let whole = normalize(Steps::ALL, &text);
            for size in [1, 2, 3, 5] {
                let mut normalizer = Normalizer::new(Steps::ALL);
                let mut out = Vec::new();
                for chunk in text.as_bytes().chunks(size) {
                    normalizer.push(chunk, &mut out);
                }
                normalizer.finish(&mut out);
                assert_eq!(
                    String::from_utf8(out).unwrap(),
                    whole,
                    "{text:?} in chunks of {size}"
                );
            }
        }
    }

    #[test]
    fn normalising_twice_changes_nothing() {
        for text in random_texts() {
            let once = normalize(Steps::ALL, &text);
            assert_eq!(normalize(Steps::ALL, &once), once, "{text:?}");
        }
    }

    #[test]
    fn invalid_utf8_becomes_replacement_characters() {
        let mut normalizer = Normalizer::new(Steps::ALL);
        let mut out = Vec::new();
        normalizer.push(b"a\xffb\xe2\x82", &mut out);
        normalizer.finish(&mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "a\u{fffd}b\u{fffd}");
    }
}
