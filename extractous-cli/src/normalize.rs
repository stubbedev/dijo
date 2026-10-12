use clap::ValueEnum;
use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

const MAX_HELD_BYTES: usize = 64 * 1024;
const MAX_CONSECUTIVE_NEWLINES: u8 = 2;

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
}

impl Step {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// The set of normalisation steps applied to extracted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steps(u8);

impl Steps {
    pub const ALL: Steps = Steps(
        Step::Invisible.bit()
            | Step::Ligatures.bit()
            | Step::SoftHyphens.bit()
            | Step::Unicode.bit()
            | Step::Whitespace.bit(),
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

    fn contains(self, step: Step) -> bool {
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
        }
    }

    /// Normalises the next chunk into `out`, holding back what may still change with the text
    /// that follows.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        if self.steps == Steps::NONE {
            out.extend_from_slice(bytes);
            return;
        }
        self.undecoded.extend_from_slice(bytes);
        let mut decoded = String::new();
        let consumed = decode_utf8_prefix(&self.undecoded, &mut decoded);
        self.undecoded.drain(..consumed);
        self.hold(&decoded);
        let stable = self.stable_prefix_len();
        let ready: String = self.held.drain(..stable).collect();
        self.release(&ready, out);
    }

    /// Normalises everything still held into `out`, at the end of the text.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.steps == Steps::NONE {
            return;
        }
        let rest = String::from_utf8_lossy(&self.undecoded).into_owned();
        self.undecoded.clear();
        self.hold(&rest);
        let ready = std::mem::take(&mut self.held);
        self.release(&ready, out);
        self.pending_space = false;
    }

    fn hold(&mut self, text: &str) {
        for character in text.chars() {
            if self.steps.contains(Step::Invisible) && is_invisible(character) {
                continue;
            }
            if self.steps.contains(Step::SoftHyphens) && character == '\u{ad}' {
                continue;
            }
            match ligature(character).filter(|_| self.steps.contains(Step::Ligatures)) {
                Some(letters) => self.held.push_str(letters),
                None => self.held.push(character),
            }
        }
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
        if self.steps.contains(Step::Unicode) {
            text.nfc().for_each(|character| self.emit(character, out));
        } else {
            text.chars().for_each(|character| self.emit(character, out));
        }
    }

    fn emit(&mut self, character: char, out: &mut Vec<u8>) {
        if self.steps.contains(Step::Whitespace) {
            self.collapse(character, out);
        } else {
            push_char(character, out);
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
                out.push(b'\n');
            }
            self.newlines = self.newlines.saturating_add(1);
        } else if is_space(character) {
            self.pending_space = true;
        } else {
            if std::mem::take(&mut self.pending_space) {
                out.push(b' ');
            }
            self.newlines = 0;
            push_char(character, out);
        }
    }
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

/// Decodes the longest prefix of `bytes` that does not end inside a character, replacing invalid
/// sequences with U+FFFD, and returns how many bytes it consumed.
fn decode_utf8_prefix(bytes: &[u8], decoded: &mut String) -> usize {
    let mut consumed = 0;
    loop {
        match std::str::from_utf8(&bytes[consumed..]) {
            Ok(valid) => {
                decoded.push_str(valid);
                return bytes.len();
            }
            Err(error) => {
                let valid_end = consumed + error.valid_up_to();
                decoded.push_str(
                    std::str::from_utf8(&bytes[consumed..valid_end]).expect("validated prefix"),
                );
                match error.error_len() {
                    Some(invalid) => {
                        decoded.push(char::REPLACEMENT_CHARACTER);
                        consumed = valid_end + invalid;
                    }
                    None => return valid_end,
                }
            }
        }
    }
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
        '🎉', '\u{0b47}', '\u{0b3e}',
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
