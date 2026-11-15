use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

const MAX_HELD_BYTES: usize = 64 * 1024;
const MAX_CONSECUTIVE_NEWLINES: u8 = 2;
const MAX_WORD_CHARS: usize = 64;
const MAX_REMEMBERED_WORDS: usize = 1 << 16;
const MIN_PREFIX_CONTINUATIONS: u8 = 2;
const SOFT_HYPHEN: char = '\u{ad}';

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
    /// Join a word hyphenated across a line break, dropping the hyphen when it was a soft one or
    /// when neither the halves nor the text before show the word is a hyphenated compound, and
    /// join a URL broken after a slash or dot.
    Hyphenation,
    /// Read an untagged PDF page in reading order (columns, then by position) rather than in
    /// the order its content stream draws the text, and a dash a PDF draws from overlapping
    /// hyphens as one.
    ReadingOrder,
    /// Leave out the bookmarks (outline) Tika appends to a PDF's text, which repeat its
    /// headings.
    Bookmarks,
    /// Leave out the targets (URIs) of a PDF's link annotations, which Tika writes after each
    /// page's text but which are not text on the page.
    Links,
}

impl Step {
    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// The set of normalisation steps applied to extracted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Steps(u16);

impl Steps {
    pub const ALL: Steps = Steps(
        Step::Invisible.bit()
            | Step::Ligatures.bit()
            | Step::SoftHyphens.bit()
            | Step::Unicode.bit()
            | Step::Whitespace.bit()
            | Step::Headers.bit()
            | Step::Hyphenation.bit()
            | Step::ReadingOrder.bit()
            | Step::Bookmarks.bit()
            | Step::Links.bit(),
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
                .then(|| Dehyphenator::new(steps.contains(Step::SoftHyphens))),
        }
    }

    /// Normalises the next chunk into `out`, holding back what may still change with the text
    /// that follows.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        if self.steps == Steps::NONE {
            out.extend_from_slice(bytes);
            return;
        }
        if let Some(rest) = self.complete_split_char(bytes) {
            let consumed = self.decode(rest);
            self.undecoded.extend_from_slice(&rest[consumed..]);
        }
        let ready = self.stable_prefix_len();
        let held = std::mem::take(&mut self.held);
        self.release(&held[..ready], out);
        self.held = held;
        self.held.drain(..ready);
    }

    /// Decodes the character split between the bytes held undecoded and the first of `bytes`,
    /// with what follows it in the next few, and returns the rest of `bytes`, or `None` where
    /// `bytes` end before it does, which keeps them undecoded with it.
    fn complete_split_char<'b>(&mut self, bytes: &'b [u8]) -> Option<&'b [u8]> {
        let held = self.undecoded.len();
        if held == 0 {
            return Some(bytes);
        }
        let mut joined = [0; 8];
        let taken = bytes.len().min(4);
        joined[..held].copy_from_slice(&self.undecoded);
        joined[held..held + taken].copy_from_slice(&bytes[..taken]);
        let consumed = self.decode(&joined[..held + taken]);
        match consumed.checked_sub(held) {
            Some(decoded) => {
                self.undecoded.clear();
                Some(&bytes[decoded..])
            }
            None => {
                self.undecoded.drain(..consumed);
                self.undecoded.extend_from_slice(bytes);
                None
            }
        }
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
            || (character == SOFT_HYPHEN
                && self.steps.contains(Step::SoftHyphens)
                && !self.drops_soft_hyphens_inside_lines())
            || (self.steps.contains(Step::Ligatures) && ligature(character).is_some())
    }

    /// How much of the held text no later character can compose with, short of soft hyphens at
    /// its end while the next character still has to tell whether they end a line.
    fn stable_prefix_len(&self) -> usize {
        if self.held.len() > MAX_HELD_BYTES {
            return self.held.len();
        }
        let drops_soft_hyphens = self.drops_soft_hyphens_inside_lines();
        let stable = if !self.steps.contains(Step::Unicode) {
            self.held.len()
        } else {
            self.held
                .char_indices()
                .rev()
                .find(|&(_, character)| {
                    is_stable_starter(character)
                        && !(drops_soft_hyphens && character == SOFT_HYPHEN)
                })
                .map_or(0, |(index, _)| index)
        };
        if drops_soft_hyphens {
            self.held[..stable].trim_end_matches(SOFT_HYPHEN).len()
        } else {
            stable
        }
    }

    /// Whether soft hyphens are dropped before the text is composed, all but those ending a
    /// line, which the dehyphenation joins with the line break.
    fn drops_soft_hyphens_inside_lines(&self) -> bool {
        self.steps.contains(Step::SoftHyphens) && self.hyphenation.is_some()
    }

    fn release(&mut self, text: &str, out: &mut Vec<u8>) {
        let text = if self.drops_soft_hyphens_inside_lines() {
            without_soft_hyphens_inside_lines(text)
        } else {
            Cow::Borrowed(text)
        };
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
        if character == SOFT_HYPHEN
            && (self.pending_space || self.newlines > 0)
            && self.steps.contains(Step::SoftHyphens)
        {
            return;
        }
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

/// Joins words hyphenated across a line break, and URLs broken across one. It holds back a
/// hyphen that ends a line after a letter or digit, a soft hyphen, or a slash or dot that ends a
/// line inside a URL, with what starts the next line, until it can tell what they join:
/// - a soft hyphen only marks where a word was broken, so it goes with the line break;
/// - a URL broken after a slash, or after a dot in its host where the next line goes on with
///   the rest of the host and a path (`https://www.` / `unaids.org/`), goes on where the next
///   line starts;
/// - a hyphen with a digit on either side (`1990-` / `1995`), or before a word that does not go
///   on in the case of the letter before it (`Sub-` / `Saharan`), cannot have split a syllable,
///   so it stays and only the line break goes;
/// - a word that goes on in that case (`extrac-` / `tion`, `AQUAEX-` / `CEL`) is joined, and
///   the hyphen stays only where the text before shows a hyphenated compound: when the word
///   already holds a hyphen (`state-of-the-` / `art`), or the joined word did not occur but the
///   hyphenated compound did, or both halves did as words of their own (`south` and `west`,
///   where hyphenation leaves fragments such as `extrac`), or the part before the hyphen began
///   hyphenated compounds with more than one other word, as articles and prefixes do (Maltese
///   `tal-membri`, `tal-familja`, then `tal-` / `ħajja`).
struct Dehyphenator {
    drop_soft_hyphens: bool,
    token: String,
    held: Held,
    words: HashSet<String>,
    compounds: HashSet<String>,
    prefixes: HashMap<String, u8>,
}

#[derive(Default)]
enum Held {
    #[default]
    Nothing,
    /// A hyphen right after a letter or digit, or a soft hyphen.
    Hyphen(Hyphen),
    /// What ended a line, the line break after it, and the spaces and the start of the next line
    /// so far.
    Break {
        end: LineEnd,
        text: String,
        word: String,
    },
}

#[derive(Clone, Copy)]
enum Hyphen {
    Hard(char),
    Soft,
}

#[derive(Clone, Copy)]
enum LineEnd {
    Hyphen(Hyphen),
    /// A slash or dot ending a URL.
    Url,
}

/// What becomes of a held line end and the start of the next line.
enum Release {
    /// Both stay as they stand.
    AsIs,
    /// The line end and the line break go; the next line goes on where the line ended.
    Join,
    /// The hyphen stays and the line break goes.
    KeepHyphen,
}

impl Dehyphenator {
    fn new(drop_soft_hyphens: bool) -> Self {
        Self {
            drop_soft_hyphens,
            token: String::new(),
            held: Held::Nothing,
            words: HashSet::new(),
            compounds: HashSet::new(),
            prefixes: HashMap::new(),
        }
    }

    fn push(&mut self, character: char, out: &mut Vec<u8>) {
        if !matches!(self.held, Held::Nothing) && self.takes_into_held(character, out) {
            return;
        }
        if is_line_break(character) && ends_broken_url(&self.token) {
            self.held = Held::Break {
                end: LineEnd::Url,
                text: character.to_string(),
                word: String::new(),
            };
            return;
        }
        if character == SOFT_HYPHEN {
            self.held = Held::Hyphen(Hyphen::Soft);
            return;
        }
        if is_hyphen(character) && self.token.chars().last().is_some_and(char::is_alphanumeric) {
            self.held = Held::Hyphen(Hyphen::Hard(character));
            return;
        }
        self.pass(character, out);
    }

    /// Takes `character` into what is held, or else releases what is held before it; whether it
    /// took it.
    fn takes_into_held(&mut self, character: char, out: &mut Vec<u8>) -> bool {
        match std::mem::take(&mut self.held) {
            Held::Nothing => false,
            Held::Hyphen(hyphen) => {
                if is_line_break(character) {
                    self.held = Held::Break {
                        end: LineEnd::Hyphen(hyphen),
                        text: character.to_string(),
                        word: String::new(),
                    };
                    return true;
                }
                self.pass_line_end(LineEnd::Hyphen(hyphen), out);
                false
            }
            Held::Break {
                end,
                mut text,
                mut word,
            } => {
                if word.is_empty() && continues_break(end, &text, character) {
                    text.push(character);
                    self.held = Held::Break { end, text, word };
                    return true;
                }
                if self.continues_word(end, &word, character) {
                    word.push(character);
                    self.held = Held::Break { end, text, word };
                    return true;
                }
                self.release_break(end, &text, &word, Some(character), out);
                false
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        match std::mem::take(&mut self.held) {
            Held::Nothing => {}
            Held::Hyphen(hyphen) => self.pass_line_end(LineEnd::Hyphen(hyphen), out),
            Held::Break { end, text, word } => self.release_break(end, &text, &word, None, out),
        }
        self.end_token();
    }

    /// Whether `character` is more of `word`, the start of the line after `end` that decides
    /// what `end` joins: the letters after a hyphen that follows a letter, the letters, digits
    /// and dots after a URL's slash or dot.
    fn continues_word(&self, end: LineEnd, word: &str, character: char) -> bool {
        let part = match end {
            LineEnd::Hyphen(Hyphen::Soft) => false,
            LineEnd::Hyphen(Hyphen::Hard(_)) => {
                character.is_alphabetic()
                    && self.token.chars().last().is_some_and(char::is_alphabetic)
            }
            LineEnd::Url => character.is_alphanumeric() || (!word.is_empty() && character == '.'),
        };
        part && word.chars().count() < MAX_WORD_CHARS
    }

    /// Writes the line end `end` held with the line break and spaces after it in `text` and the
    /// start of the next line in `word`, ended by `next`, or by the end of the text.
    fn release_break(
        &mut self,
        end: LineEnd,
        text: &str,
        word: &str,
        next: Option<char>,
        out: &mut Vec<u8>,
    ) {
        match self.release(end, word, next) {
            Release::AsIs => {
                self.pass_line_end(end, out);
                text.chars().for_each(|held| self.pass(held, out));
            }
            Release::Join => {}
            Release::KeepHyphen => self.pass_line_end(end, out),
        }
        word.chars().for_each(|held| self.pass(held, out));
    }

    /// What becomes of `end`: it stays as it stands where what starts the next line could
    /// compose with what it would be joined to, since the text was composed before it was joined.
    fn release(&self, end: LineEnd, word: &str, next: Option<char>) -> Release {
        if word
            .chars()
            .next()
            .or(next)
            .is_some_and(|first| !is_stable_starter(first))
        {
            return Release::AsIs;
        }
        self.decide(end, word, next)
    }

    fn decide(&self, end: LineEnd, word: &str, next: Option<char>) -> Release {
        let Some(next) = next else {
            return match end {
                LineEnd::Hyphen(Hyphen::Hard(_)) if !word.is_empty() => self.joined_word(word),
                _ => Release::AsIs,
            };
        };
        match end {
            LineEnd::Hyphen(Hyphen::Soft) => Release::Join,
            LineEnd::Url => {
                let goes_on = if self.token.ends_with('/') {
                    !word.is_empty()
                } else {
                    !url_has_path(&self.token)
                        && word.chars().next().is_some_and(char::is_lowercase)
                        && next == '/'
                };
                if goes_on {
                    Release::Join
                } else {
                    Release::AsIs
                }
            }
            LineEnd::Hyphen(Hyphen::Hard(_)) if word.is_empty() => {
                if next.is_alphanumeric() {
                    Release::KeepHyphen
                } else {
                    Release::AsIs
                }
            }
            LineEnd::Hyphen(Hyphen::Hard(_)) if next.is_alphabetic() => Release::AsIs,
            LineEnd::Hyphen(Hyphen::Hard(_)) => self.joined_word(word),
        }
    }

    /// How a word hyphenated after a letter joins `next`, the letters starting the next line:
    /// as one word where `next` goes on in the case of the letter before the hyphen and the text
    /// before shows no hyphenated compound, else keeping the hyphen.
    fn joined_word(&self, next: &str) -> Release {
        let before = self
            .token
            .chars()
            .last()
            .expect("a hyphen held after a letter");
        let goes_on = next.chars().all(char::is_lowercase)
            || next.chars().all(|letter| same_case(letter, before));
        if goes_on && !self.is_compound(next) {
            Release::Join
        } else {
            Release::KeepHyphen
        }
    }

    /// Whether the word before the line break and `next`, the word after it, make a hyphenated
    /// compound by what the text before shows.
    fn is_compound(&self, next: &str) -> bool {
        let before = last_word(&self.token).to_lowercase();
        let next = next.to_lowercase();
        self.token.contains(is_hyphen)
            || (!self.words.contains(&format!("{before}{next}"))
                && (self.compounds.contains(&format!("{before}-{next}"))
                    || self
                        .prefixes
                        .get(&before)
                        .is_some_and(|&continuations| continuations >= MIN_PREFIX_CONTINUATIONS)
                    || (self.words.contains(&before) && self.words.contains(&next))))
    }

    /// Writes the hyphen of `end`; a soft hyphen only where soft hyphens are kept.
    fn pass_line_end(&mut self, end: LineEnd, out: &mut Vec<u8>) {
        match end {
            LineEnd::Hyphen(Hyphen::Hard(hyphen)) => self.pass(hyphen, out),
            LineEnd::Hyphen(Hyphen::Soft) if !self.drop_soft_hyphens => self.pass(SOFT_HYPHEN, out),
            LineEnd::Hyphen(Hyphen::Soft) | LineEnd::Url => {}
        }
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
    /// Only a word or compound not remembered yet is copied: the text of a long document repeats
    /// most of its words.
    fn end_token(&mut self) {
        if self.token.is_empty() {
            return;
        }
        let token = self.token.to_lowercase();
        self.token.clear();
        let words = token
            .split(|character: char| !character.is_alphabetic() && !is_hyphen(character))
            .filter(|word| !word.is_empty());
        for word in words {
            if !word.contains(is_hyphen) {
                if self.words.len() < MAX_REMEMBERED_WORDS && !self.words.contains(word) {
                    self.words.insert(word.to_string());
                }
                continue;
            }
            let parts: Vec<&str> = word
                .split(is_hyphen)
                .filter(|part| !part.is_empty())
                .collect();
            if self.words.len() < MAX_REMEMBERED_WORDS {
                for part in &parts {
                    if !self.words.contains(*part) {
                        self.words.insert((*part).to_string());
                    }
                }
            }
            if self.compounds.len() < MAX_REMEMBERED_WORDS {
                for pair in parts.windows(2) {
                    if self.compounds.insert(format!("{}-{}", pair[0], pair[1]))
                        && self.prefixes.len() < MAX_REMEMBERED_WORDS
                    {
                        let continuations = self.prefixes.entry(pair[0].to_string()).or_default();
                        *continuations = continuations.saturating_add(1);
                    }
                }
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

/// Whether `character` is more of `text`, the line break after `end` so far: spaces, and for a
/// soft hyphen further line breaks.
fn continues_break(end: LineEnd, text: &str, character: char) -> bool {
    let space = match end {
        LineEnd::Hyphen(Hyphen::Soft) => character.is_whitespace(),
        LineEnd::Hyphen(Hyphen::Hard(_)) | LineEnd::Url => is_space(character),
    };
    space && text.len() < MAX_WORD_CHARS
}

/// `text` without the soft hyphens that do not end a line.
fn without_soft_hyphens_inside_lines(text: &str) -> Cow<'_, str> {
    if !text.contains(SOFT_HYPHEN) {
        return Cow::Borrowed(text);
    }
    let mut kept = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != SOFT_HYPHEN || characters.peek().copied().is_some_and(is_line_break) {
            kept.push(character);
        }
    }
    Cow::Owned(kept)
}

/// Whether `token` is a URL ending in a slash or dot, where it may go on on the next line.
fn ends_broken_url(token: &str) -> bool {
    token.ends_with(['/', '.']) && (token.contains("://") || token.starts_with("www."))
}

/// Whether the URL `token` has a path after its host, so that a dot ending it can end a sentence.
fn url_has_path(token: &str) -> bool {
    let host = token.split_once("://").map_or(token, |(_, rest)| rest);
    host.contains('/')
}

fn same_case(letter: char, other: char) -> bool {
    letter.is_uppercase() == other.is_uppercase() && letter.is_lowercase() == other.is_lowercase()
}

fn is_hyphen(character: char) -> bool {
    matches!(character, '-' | '\u{2010}')
}

fn push_char(character: char, out: &mut Vec<u8>) {
    let mut buffer = [0; 4];
    out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
}

/// Normalises `bytes` handed over in chunks of `size`.
pub fn normalize_in_chunks(steps: Steps, bytes: &[u8], size: usize) -> Vec<u8> {
    let mut normalizer = Normalizer::new(steps);
    let mut out = Vec::new();
    for chunk in bytes.chunks(size) {
        normalizer.push(chunk, &mut out);
    }
    normalizer.finish(&mut out);
    out
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
    fn a_soft_hyphen_starting_a_line_keeps_no_blank_line_more() {
        let once = normalize(Steps::ALL, "\u{b}\u{ad}\u{b}\u{b}\u{b}\u{b}\u{b}\u{b}");
        assert_eq!(once, "\n\n");
        assert_eq!(normalize(Steps::ALL, &once), once);
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
        assert_eq!(
            join("tal-membri tal-familja, tal-\nħajja"),
            "tal-membri tal-familja, tal-ħajja"
        );
        assert_eq!(
            join("anti-war and anti-trust, then anti-\nvirus"),
            "anti-war and anti-trust, then anti-virus"
        );
    }

    #[test]
    fn a_prefix_hyphenated_once_or_whose_word_occurred_does_not_keep_the_hyphen() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        assert_eq!(join("re-elect the re-\nport"), "re-elect the report");
        assert_eq!(
            join("talħajja, tal-membri tal-familja, tal-\nħajja"),
            "talħajja, tal-membri tal-familja, talħajja"
        );
    }

    #[test]
    fn a_hyphen_that_cannot_split_a_syllable_stays_and_the_line_break_goes() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        assert_eq!(join("anti-\nSemitic"), "anti-Semitic");
        assert_eq!(join("test of Morisky-\n  Green"), "test of Morisky-Green");
        assert_eq!(
            join("the Trivers-\nWillard theory"),
            "the Trivers-Willard theory"
        );
        assert_eq!(join("1990-\n1995"), "1990-1995");
        assert_eq!(join("COVID-\n19"), "COVID-19");
        assert_eq!(join("j.1469-\n185X"), "j.1469-185X");
        assert_eq!(join("HIV-\nPositive"), "HIV-Positive");
    }

    #[test]
    fn a_word_in_capitals_is_joined_as_one_in_lower_case_is() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        assert_eq!(
            join("funded by AQUAEX-\nCEL 0114"),
            "funded by AQUAEXCEL 0114"
        );
        assert_eq!(
            join("SOUTH and WEST: the SOUTH-\nWEST"),
            "SOUTH and WEST: the SOUTH-WEST"
        );
    }

    #[test]
    fn soft_hyphens_ending_a_line_join_the_word() {
        let join = |text| {
            normalize(
                Steps(Step::Hyphenation.bit() | Step::SoftHyphens.bit()),
                text,
            )
        };
        assert_eq!(join("expe\u{ad}\n\nrience"), "experience");
        assert_eq!(join("pro\u{ad}\n  cures"), "procures");
        assert_eq!(join("extrac\u{ad}tion"), "extraction");
        let keep = |text| normalize(only(Step::Hyphenation), text);
        assert_eq!(
            keep("pro\u{ad}\ncures extrac\u{ad}tion"),
            "procures extrac\u{ad}tion"
        );
    }

    #[test]
    fn urls_broken_after_a_slash_or_dot_are_joined() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        assert_eq!(
            join("2022. https://www.cdc.gov/hiv/pdf/risk/\nprep/cdc-hiv-prep-guidelines-2021.pdf"),
            "2022. https://www.cdc.gov/hiv/pdf/risk/prep/cdc-hiv-prep-guidelines-2021.pdf"
        );
        assert_eq!(
            join("https://www.\nunaids.org/en and https://www.state.\ngov/wp-content"),
            "https://www.unaids.org/en and https://www.state.gov/wp-content"
        );
        assert_eq!(
            join("https://doi.org/10.1007/\ns10461-021-03262-4"),
            "https://doi.org/10.1007/s10461-021-03262-4"
        );
        assert_eq!(
            join("https://example.org/uploads/PEPFAR-MER-Indicator-Reference-\nGuide-2.4.pdf"),
            "https://example.org/uploads/PEPFAR-MER-Indicator-Reference-Guide-2.4.pdf"
        );
    }

    #[test]
    fn other_hyphens_at_line_ends_stay() {
        let join = |text| normalize(only(Step::Hyphenation), text);
        for text in [
            "word -\nnext",
            "sup-\n'ported",
            "https://example.org\nnext",
            "see https://example.org/a.\nThe next",
            "https://doi.org/10.1093/eurjcn/zvac063.\n4. Next",
            "at https://example.org.\nnext. Then",
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
            let out = normalize_in_chunks(Steps::ALL, text.as_bytes(), size);
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
                let out = normalize_in_chunks(Steps::ALL, text.as_bytes(), size);
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

    /// Bytes split anywhere, inside characters and invalid sequences, decode as the whole does,
    /// with U+FFFD for each invalid sequence as a lossy conversion writes it.
    #[test]
    fn bytes_split_anywhere_decode_as_the_whole() {
        const PIECES: &[&[u8]] = &[
            b"a",
            b" ",
            b"\n",
            "\u{e9}".as_bytes(),
            "e\u{301}".as_bytes(),
            "\u{6f22}".as_bytes(),
            "\u{1f389}".as_bytes(),
            b"\xf0\x9f",
            b"\xf0\x9f\x8e",
            b"\xe2\x82",
            b"\xc3",
            b"\xff",
            b"\x80",
            b"\xed\xa0\x80",
        ];
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..500 {
            let length = (next() % 12) as usize;
            let bytes: Vec<u8> = (0..length)
                .flat_map(|_| {
                    PIECES[(next() % PIECES.len() as u64) as usize]
                        .iter()
                        .copied()
                })
                .collect();
            let lossy = String::from_utf8_lossy(&bytes);
            let whole = normalize(Steps::ALL, &lossy);
            for size in 1..=6 {
                let decoded = normalize_in_chunks(only(Step::Ligatures), &bytes, size);
                assert_eq!(decoded, lossy.as_bytes(), "{bytes:?} in chunks of {size}");
                let out = normalize_in_chunks(Steps::ALL, &bytes, size);
                assert_eq!(
                    String::from_utf8(out).unwrap(),
                    whole,
                    "{bytes:?} in chunks of {size}"
                );
            }
        }
    }
}
