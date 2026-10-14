//! A port of Apache Tika 4.1's CharSoup language detector (`tika-langdetect-charsoup`,
//! Apache-2.0): a multinomial logistic regression with INT8 weights over hashed character
//! n-grams, short-word bigrams and script counts, trained by Tika on Wikipedia and MADLAD. The
//! model's weights only fit the exact features it was trained on, so preprocessing, features and
//! scoring follow Tika's code step for step; `tests/fixtures/language/charsoup-reference.tsv`
//! holds Tika's own answers to check against.

use std::borrow::Cow;
use std::sync::LazyLock;

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::{is_nfc, UnicodeNormalization};
use unicode_script::{Script, UnicodeScript};

use extractous_core::unicode::{is_letter, lowercase};

const MODEL_BYTES: &[u8] = include_bytes!("../../models/langdetect-20260320.bin");
const MAGIC: u32 = 0x4C44_4D31;
const VERSION: u32 = 2;
const BUCKETS: usize = 32_768;
/// Trigrams, 4-grams, script blocks, L2 normalisation and short-word bigrams: the features of
/// Tika's `SaltedNgramFeatureExtractor` with word bigrams, the only ones this port computes.
const FEATURE_FLAGS: u32 = 0xE81;

/// Languages whose texts the model cannot reliably tell apart (Tika's `confusables.txt`): their
/// scores are pooled and given to the best of them.
const CONFUSABLE_GROUPS: [&[&str]; 4] = [
    &["msa", "ind"],
    &["xho", "zul"],
    &["bel", "be-x-old"],
    &["yue", "zho"],
];

const MAX_TEXT_UTF16: usize = 100_000;
const WORD_CAPACITY: usize = 256;
const MIN_NGRAM: usize = 2;
const MAX_NGRAM: usize = 4;
const MAX_SHORT_WORD: usize = 3;
const MIN_URL_TAIL: usize = 10;
const MAX_URL_TAIL: usize = 10_000;
const MAX_MAIL_PART: usize = 100;
const MASKED_LOGIT: f32 = -1e30;

const FNV_BASIS: u32 = 0x811C_9DC5;
const FNV_PRIME: u32 = 0x0100_0193;
const SALT_MID: u32 = 0x00;
const SALT_BOW: u32 = 0x01;
const SALT_EOW: u32 = 0x02;
const SALT_FULL_WORD: u32 = 0x03;
const SALT_CJK_UNIGRAM: u32 = 0x04;
const SALT_SCRIPT: u32 = 0x05;
const SALT_SCRIPT_TRANSITION: u32 = 0x06;
const SALT_WORD_BIGRAM: u32 = 0x07;
const WORD_SEPARATOR: u32 = 0xFF;

/// Tika's coarse script categories (`ScriptCategory`), numbered as Tika numbers them, since the
/// numbers are hashed into the features.
mod category {
    pub const LATIN: usize = 0;
    pub const CYRILLIC: usize = 1;
    pub const ARABIC: usize = 2;
    pub const HAN: usize = 3;
    pub const HANGUL: usize = 4;
    pub const HIRAGANA: usize = 5;
    pub const KATAKANA: usize = 6;
    pub const DEVANAGARI: usize = 7;
    pub const THAI: usize = 8;
    pub const GREEK: usize = 9;
    pub const HEBREW: usize = 10;
    pub const BENGALI: usize = 11;
    pub const GEORGIAN: usize = 12;
    pub const ARMENIAN: usize = 13;
    pub const ETHIOPIC: usize = 14;
    pub const OTHER: usize = 15;
    pub const CANADIAN_ABORIGINAL: usize = 16;
    pub const MYANMAR: usize = 17;
    pub const TIBETAN: usize = 18;
    pub const KHMER: usize = 19;
    pub const HAN_EXT_A: usize = 20;
    pub const HAN_EXT_B: usize = 21;
    pub const HAN_COMPAT: usize = 22;
    pub const BOPOMOFO: usize = 23;
    pub const COUNT: usize = 24;
}

/// How sure the detector is of its answer, as Tika grades it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// The language a text is written in, by the model's own label (ISO 639-3, or a base code with
/// a private-use orthography tag such as `be-x-old`).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub label: &'static str,
    /// The logistic of the winner's lead over the runner-up: 0.5 for a tie, towards 1 for a
    /// clear lead.
    pub score: f32,
    /// The entropy of the distribution over all languages, in bits.
    pub entropy: f32,
    pub confidence: Confidence,
    /// The script most of the text's letters are written in.
    pub script: Script,
}

/// The model's scores for a text: one logit per [`labels`] entry, with each confusable group
/// pooled into its best member, and the script most of its letters are written in.
#[derive(Debug, Clone, PartialEq)]
pub struct Scores {
    pub logits: Vec<f32>,
    pub script: Script,
}

/// The model's labels, in the order of [`Scores::logits`].
pub fn labels() -> &'static [&'static str] {
    &MODEL.labels
}

/// The model's scores for a text, or None when it has no letters.
pub fn scores(text: &str) -> Option<Scores> {
    let features = extract(&preprocess(text));
    let script = features.dominant_script?;
    let model = &*MODEL;
    Some(Scores {
        logits: model.collapse_groups(&model.logits(&features.counts)),
        script,
    })
}

/// How Tika grades logits: the winner's index, the logistic of its lead over the runner-up, the
/// entropy in bits and the confidence, or None when no logit leads.
pub fn decide(logits: &[f32]) -> Option<(usize, f32, f32, Confidence)> {
    let entropy = entropy(logits);
    let (top, top_logit, second_logit) = top_two(logits);
    let score = sigmoid(top_logit - second_logit);
    let confidence = if score > 0.9 {
        Confidence::High
    } else if score > 0.7 {
        if entropy < 2.0 {
            Confidence::Medium
        } else {
            Confidence::Low
        }
    } else if score > 0.5 {
        Confidence::Low
    } else {
        return None;
    };
    Some((top, score, entropy, confidence))
}

/// The language of a text as Tika detects it, or None when it has no letters or no language
/// leads. Unlike Tika, which answers the model's biases for a text without letters, such a text
/// has no language.
#[cfg(test)]
pub fn detect(text: &str) -> Option<Detection> {
    let Scores { logits, script } = scores(text)?;
    let (top, score, entropy, confidence) = decide(&logits)?;
    Some(Detection {
        label: MODEL.labels[top],
        score,
        entropy,
        confidence,
        script,
    })
}

static MODEL: LazyLock<Model> = LazyLock::new(|| {
    Model::parse(MODEL_BYTES)
        .unwrap_or_else(|error| panic!("the embedded CharSoup model is malformed: {error}"))
});

struct Model {
    labels: Vec<&'static str>,
    scales: Vec<f32>,
    biases: Vec<f32>,
    /// `BUCKETS` rows of one signed byte per class.
    weights: &'static [u8],
    groups: Vec<Vec<usize>>,
}

impl Model {
    fn parse(bytes: &'static [u8]) -> Result<Self, String> {
        let mut reader = Reader { bytes, at: 0 };
        let magic = reader.u32()?;
        if magic != MAGIC {
            return Err(format!("magic {magic:#010x}, expected {MAGIC:#010x}"));
        }
        let version = reader.u32()?;
        if version != VERSION {
            return Err(format!("version {version}, expected {VERSION}"));
        }
        let buckets = reader.u32()? as usize;
        if buckets != BUCKETS {
            return Err(format!("{buckets} buckets, expected {BUCKETS}"));
        }
        let classes = reader.u32()? as usize;
        let flags = reader.u32()?;
        if flags != FEATURE_FLAGS {
            return Err(format!(
                "feature flags {flags:#x}, expected {FEATURE_FLAGS:#x}"
            ));
        }
        let labels = (0..classes)
            .map(|_| {
                let length = usize::from(reader.u16()?);
                std::str::from_utf8(reader.take(length)?).map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let scales = (0..classes)
            .map(|_| reader.f32())
            .collect::<Result<_, _>>()?;
        let biases = (0..classes)
            .map(|_| reader.f32())
            .collect::<Result<_, _>>()?;
        let weights = reader.take(buckets * classes)?;
        if reader.at != bytes.len() {
            return Err(format!(
                "{} bytes after the weights",
                bytes.len() - reader.at
            ));
        }
        let groups = CONFUSABLE_GROUPS
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|label| {
                        labels
                            .iter()
                            .position(|known| known == label)
                            .ok_or_else(|| format!("no class for the confusable language {label}"))
                    })
                    .collect()
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            labels,
            scales,
            biases,
            weights,
            groups,
        })
    }

    fn logits(&self, counts: &[i32]) -> Vec<f32> {
        let classes = self.labels.len();
        let present: Vec<usize> = (0..BUCKETS).filter(|&b| counts[b] != 0).collect();
        let norm_squared: f64 = present
            .iter()
            .map(|&b| (i64::from(counts[b]) * i64::from(counts[b])) as f64)
            .sum();
        let inverse_norm = if norm_squared > 0.0 {
            (1.0 / norm_squared.sqrt()) as f32
        } else {
            1.0
        };
        let mut dots = vec![0i32; classes];
        accumulate(&mut dots, &present, counts, self.weights);
        (0..classes)
            .map(|c| self.biases[c] + self.scales[c] * dots[c] as f32 * inverse_norm)
            .collect()
    }

    /// Pools each confusable group's logits by log-sum-exp into its best member, and masks out
    /// the others.
    fn collapse_groups(&self, logits: &[f32]) -> Vec<f32> {
        let mut collapsed = logits.to_vec();
        for group in &self.groups {
            let mut best = group[0];
            let mut max = logits[best];
            for &member in group {
                if logits[member] > max {
                    max = logits[member];
                    best = member;
                }
            }
            let mut sum = 0f32;
            for &member in group {
                sum = (f64::from(sum) + f64::from(logits[member] - max).exp()) as f32;
            }
            let pooled = max + f64::from(sum).ln() as f32;
            for &member in group {
                collapsed[member] = if member == best { pooled } else { MASKED_LOGIT };
            }
        }
        collapsed
    }
}

/// Adds each present bucket's weights, times its count, to the classes' dot products, with AVX2
/// where the processor has it.
fn accumulate(dots: &mut [i32], present: &[usize], counts: &[i32], weights: &[u8]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        unsafe { accumulate_avx2(dots, present, counts, weights) };
        return;
    }
    accumulate_rows(dots, present, counts, weights);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn accumulate_avx2(dots: &mut [i32], present: &[usize], counts: &[i32], weights: &[u8]) {
    accumulate_rows(dots, present, counts, weights);
}

#[inline(always)]
fn accumulate_rows(dots: &mut [i32], present: &[usize], counts: &[i32], weights: &[u8]) {
    let classes = dots.len();
    for &bucket in present {
        let count = counts[bucket];
        let row = &weights[bucket * classes..][..classes];
        for (dot, &weight) in dots.iter_mut().zip(row) {
            *dot += i32::from(weight as i8) * count;
        }
    }
}

struct Reader {
    bytes: &'static [u8],
    at: usize,
}

impl Reader {
    fn take(&mut self, length: usize) -> Result<&'static [u8], String> {
        let end = self
            .at
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| format!("truncated at byte {}", self.at))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }
}

/// The index of the highest logit (the first one, on a tie), it, and the runner-up.
fn top_two(logits: &[f32]) -> (usize, f32, f32) {
    let mut top = 0;
    let mut top_logit = logits[0];
    let mut second_logit = f32::NEG_INFINITY;
    for (index, &logit) in logits.iter().enumerate().skip(1) {
        if logit > top_logit {
            second_logit = top_logit;
            top = index;
            top_logit = logit;
        } else if logit > second_logit {
            second_logit = logit;
        }
    }
    (top, top_logit, second_logit)
}

fn sigmoid(x: f32) -> f32 {
    (1.0 / (1.0 + (-f64::from(x)).exp())) as f32
}

fn log_sum_exp(logits: &[f32]) -> f32 {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0f32;
    for &logit in logits {
        sum = (f64::from(sum) + f64::from(logit - max).exp()) as f32;
    }
    max + f64::from(sum).ln() as f32
}

fn entropy(logits: &[f32]) -> f32 {
    let lse = log_sum_exp(logits);
    let weighted: f64 = logits
        .iter()
        .map(|&logit| f64::from(logit - lse).exp() * f64::from(logit))
        .sum();
    ((f64::from(lse) - weighted) / 2f64.ln()) as f32
}

/// Tika's preprocessing: the first 100,000 UTF-16 code units, URLs and then e-mail addresses
/// replaced by a space, NFC. The integer dot products cannot overflow on that much text: no
/// code unit yields ten features, and no weight exceeds 127.
fn preprocess(text: &str) -> Cow<'_, str> {
    let truncated = truncate_utf16(text, MAX_TEXT_UTF16);
    let text = strip_urls(truncated).map_or(Cow::Borrowed(truncated), Cow::Owned);
    let text = match strip_emails(&text) {
        Some(stripped) => Cow::Owned(stripped),
        None => text,
    };
    if is_nfc(&text) {
        text
    } else {
        Cow::Owned(text.nfc().collect())
    }
}

/// The longest prefix of whole characters within `limit` UTF-16 code units. Java cuts a
/// surrogate pair in half there, but a lone surrogate ends a word just as the end of the text
/// does, so the features are the same.
fn truncate_utf16(text: &str, limit: usize) -> &str {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        units += c.len_utf16();
        if units > limit {
            return &text[..index];
        }
    }
    text
}

fn is_url_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-_.?&~;+=/#".contains(&byte)
}

fn is_mail_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn is_mail_dot_char(byte: u8) -> bool {
    is_mail_char(byte) || byte == b'.'
}

/// The text with `https?://[-_.?&~;+=/#0-9A-Za-z]{10,10000}` replaced by a space, or None when
/// it has none. Every character the pattern spans is ASCII, so bytes stand for characters.
fn strip_urls(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut emitted = 0;
    let mut i = 0;
    while i < bytes.len() {
        let scheme = if bytes[i..].starts_with(b"http://") {
            7
        } else if bytes[i..].starts_with(b"https://") {
            8
        } else {
            0
        };
        if scheme > 0 {
            let start = i + scheme;
            let limit = bytes.len().min(start + MAX_URL_TAIL);
            let end = (start..limit)
                .find(|&k| !is_url_char(bytes[k]))
                .unwrap_or(limit);
            if end - start >= MIN_URL_TAIL {
                let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
                out.push_str(&text[emitted..i]);
                out.push(' ');
                emitted = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.map(|mut out| {
        out.push_str(&text[emitted..]);
        out
    })
}

/// The text with `[-_.0-9A-Za-z]{1,100}@[-_0-9A-Za-z]{1,100}[-_.0-9A-Za-z]{1,100}` replaced by a
/// space, or None when it has none, matched as Tika's hand-written matcher does: around each `@`, the local part runs back (not past the
/// previous match), the dotless head of the domain runs ahead greedily, and gives up one
/// character to the tail when nothing else could start it. Every character the pattern spans is
/// ASCII, so bytes stand for characters.
fn strip_emails(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut emitted = 0;
    let mut floor = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let mut start = i;
            while start > floor && i - start < MAX_MAIL_PART && is_mail_dot_char(bytes[start - 1]) {
                start -= 1;
            }
            if start < i {
                let head = i + 1;
                let head_limit = bytes.len().min(head + MAX_MAIL_PART);
                let head_end = (head..head_limit)
                    .find(|&j| !is_mail_char(bytes[j]))
                    .unwrap_or(head_limit);
                let mut end = None;
                if head_end > head {
                    if head_end < bytes.len() && is_mail_dot_char(bytes[head_end]) {
                        let tail_limit = bytes.len().min(head_end + MAX_MAIL_PART);
                        end = Some(
                            (head_end..tail_limit)
                                .find(|&k| !is_mail_dot_char(bytes[k]))
                                .unwrap_or(tail_limit),
                        );
                    } else if head_end - head >= 2 {
                        end = Some(head_end);
                    }
                }
                if let Some(end) = end {
                    let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
                    out.push_str(&text[emitted..start]);
                    out.push(' ');
                    emitted = end;
                    floor = end;
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.map(|mut out| {
        out.push_str(&text[emitted..]);
        out
    })
}

/// Characters skipped as if absent: nonspacing marks such as Arabic harakat and Hebrew niqqud,
/// the Arabic tatweel, and the zero-width (non-)joiners.
fn is_transparent(c: char) -> bool {
    matches!(c, '\u{0640}' | '\u{200C}' | '\u{200D}')
        || get_general_category(c) == GeneralCategory::NonspacingMark
}

fn script_category(c: char, script: Script) -> usize {
    let cp = u32::from(c);
    if cp < 0x80 {
        return category::LATIN;
    }
    if (0x3100..=0x312F).contains(&cp) || (0x31A0..=0x31BF).contains(&cp) {
        return category::BOPOMOFO;
    }
    match script {
        Script::Han => match cp {
            0x3400..=0x4DBF => category::HAN_EXT_A,
            0xF900..=0xFAFF => category::HAN_COMPAT,
            0x2_0000.. => category::HAN_EXT_B,
            _ => category::HAN,
        },
        Script::Latin => category::LATIN,
        Script::Cyrillic => category::CYRILLIC,
        Script::Arabic => category::ARABIC,
        Script::Hangul => category::HANGUL,
        Script::Hiragana => category::HIRAGANA,
        Script::Katakana => category::KATAKANA,
        Script::Devanagari => category::DEVANAGARI,
        Script::Thai => category::THAI,
        Script::Greek => category::GREEK,
        Script::Hebrew => category::HEBREW,
        Script::Bengali => category::BENGALI,
        Script::Georgian => category::GEORGIAN,
        Script::Armenian => category::ARMENIAN,
        Script::Ethiopic => category::ETHIOPIC,
        Script::Canadian_Aboriginal => category::CANADIAN_ABORIGINAL,
        Script::Myanmar => category::MYANMAR,
        Script::Tibetan => category::TIBETAN,
        Script::Khmer => category::KHMER,
        _ => category::OTHER,
    }
}

fn is_cjk(category: usize) -> bool {
    matches!(
        category,
        category::HAN
            | category::HAN_EXT_A
            | category::HAN_EXT_B
            | category::HAN_COMPAT
            | category::BOPOMOFO
            | category::HIRAGANA
            | category::KATAKANA
    )
}

fn same_family(a: usize, b: Option<usize>) -> bool {
    b.is_some_and(|b| a == b || (is_cjk(a) && is_cjk(b)))
}

fn fnv_byte(hash: u32, byte: u32) -> u32 {
    (hash ^ (byte & 0xFF)).wrapping_mul(FNV_PRIME)
}

fn fnv_codepoint(hash: u32, cp: u32) -> u32 {
    cp.to_le_bytes()
        .iter()
        .fold(hash, |hash, &byte| fnv_byte(hash, u32::from(byte)))
}

fn bucket(hash: u32) -> usize {
    (hash & 0x7FFF_FFFF) as usize % BUCKETS
}

struct Features {
    counts: Vec<i32>,
    /// The script of most letters, or None without letters.
    dominant_script: Option<Script>,
}

/// Counts the features of a preprocessed text into hashed buckets, as Tika's
/// `SaltedNgramFeatureExtractor` with word bigrams does: the 2-, 3- and 4-grams of every word
/// (letters of one script family, lowercased) salted by their place in the word, each CJK
/// character and adjacent pair, each short word with the word after it, and the letters and
/// changes of each script category.
fn extract(text: &str) -> Features {
    let mut extractor = Extractor {
        counts: vec![0; BUCKETS],
        word: Vec::with_capacity(WORD_CAPACITY),
        previous_word: Vec::with_capacity(WORD_CAPACITY),
    };
    let mut scripts: Vec<(Script, usize)> = Vec::new();
    let mut category_counts = [0i32; category::COUNT];
    let mut transitions = [0i32; category::COUNT * category::COUNT];
    let mut last_category: Option<usize> = None;
    let mut previous: Option<char> = None;
    let mut previous_category: Option<usize> = None;
    let mut previous_was_letter = false;
    let mut previous_was_cjk = false;

    for c in text.chars() {
        let letter = if c.is_ascii() {
            c.is_ascii_alphabetic()
                .then(|| (c.to_ascii_lowercase(), Script::Latin))
        } else if u32::from(c) >= 0x0300 && is_transparent(c) {
            continue;
        } else {
            is_letter(c).then(|| {
                let lower = lowercase(c);
                (lower, lower.script())
            })
        };
        let Some((lower, script)) = letter else {
            if previous_was_letter && !previous_was_cjk && !extractor.word.is_empty() {
                extractor.finish_word();
            }
            previous_was_letter = false;
            previous_was_cjk = false;
            previous = None;
            continue;
        };
        match scripts.iter_mut().find(|(known, _)| *known == script) {
            Some((_, count)) => *count += 1,
            None => scripts.push((script, 1)),
        }
        let category = script_category(lower, script);
        let cjk = is_cjk(category);
        category_counts[category] += 1;
        if let Some(last) = last_category.filter(|&last| last != category) {
            transitions[last * category::COUNT + category] += 1;
        }
        last_category = Some(category);

        if previous_was_letter && !same_family(category, previous_category) {
            if !previous_was_cjk && !extractor.word.is_empty() {
                extractor.finish_word();
            }
            extractor.word.clear();
        }
        if cjk {
            extractor.emit(SALT_CJK_UNIGRAM, &[lower]);
            if let Some(before) = previous.filter(|_| previous_was_cjk) {
                extractor.emit(SALT_MID, &[before, lower]);
            }
            if !previous_was_cjk && !extractor.word.is_empty() {
                extractor.finish_word();
            }
            extractor.previous_word.clear();
        } else {
            if !previous_was_letter || previous_was_cjk || !same_family(category, previous_category)
            {
                extractor.word.clear();
            }
            if extractor.word.len() < WORD_CAPACITY {
                extractor.word.push(lower);
            }
        }
        previous = Some(lower);
        previous_category = Some(category);
        previous_was_letter = true;
        previous_was_cjk = cjk;
    }
    if previous_was_letter && !previous_was_cjk && !extractor.word.is_empty() {
        extractor.finish_word();
    }

    for (category, &count) in category_counts.iter().enumerate() {
        if count > 0 {
            let hash = fnv_byte(fnv_byte(FNV_BASIS, SALT_SCRIPT), category as u32);
            extractor.counts[bucket(hash)] += count;
        }
    }
    for (index, &count) in transitions.iter().enumerate() {
        if count > 0 {
            let from = (index / category::COUNT) as u32;
            let to = (index % category::COUNT) as u32;
            let hash = fnv_byte(
                fnv_byte(fnv_byte(FNV_BASIS, SALT_SCRIPT_TRANSITION), from),
                to,
            );
            extractor.counts[bucket(hash)] += count;
        }
    }
    let dominant_script = scripts
        .iter()
        .fold(
            None::<(Script, usize)>,
            |best, &(script, count)| match best {
                Some((_, most)) if most >= count => best,
                _ => Some((script, count)),
            },
        )
        .map(|(script, _)| script);
    Features {
        counts: extractor.counts,
        dominant_script,
    }
}

struct Extractor {
    counts: Vec<i32>,
    word: Vec<char>,
    previous_word: Vec<char>,
}

impl Extractor {
    fn emit(&mut self, salt: u32, chars: &[char]) {
        emit(&mut self.counts, salt, chars);
    }

    /// Emits the finished word's n-grams and, after a short word, the pair of them, then keeps
    /// the word as the one before the next and starts the next one empty. A word of up to four
    /// letters is its own n-gram; a longer one's n-grams of each order are salted as at its
    /// beginning, middle or end. The n-grams starting where others of the same salt do are hashed
    /// on from the shorter ones' hashes.
    fn finish_word(&mut self) {
        let word = &self.word;
        let len = word.len();
        if len <= MAX_NGRAM {
            emit(&mut self.counts, SALT_FULL_WORD, word);
        }
        if len > MIN_NGRAM {
            let longest = MAX_NGRAM.min(len - 1);
            emit_prefixes(&mut self.counts, SALT_BOW, word, longest);
            for start in 1..len - MIN_NGRAM {
                emit_prefixes(
                    &mut self.counts,
                    SALT_MID,
                    &word[start..],
                    MAX_NGRAM.min(len - start - 1),
                );
            }
            for order in MIN_NGRAM..=longest {
                emit(&mut self.counts, SALT_EOW, &word[len - order..]);
            }
        }
        if (1..=MAX_SHORT_WORD).contains(&self.previous_word.len()) {
            let anchor = hash_chars(fnv_byte(FNV_BASIS, SALT_WORD_BIGRAM), &self.previous_word);
            let hash = hash_chars(fnv_byte(anchor, WORD_SEPARATOR), word);
            self.counts[bucket(hash)] += 1;
        }
        std::mem::swap(&mut self.word, &mut self.previous_word);
        self.word.clear();
    }
}

fn hash_chars(hash: u32, chars: &[char]) -> u32 {
    chars
        .iter()
        .fold(hash, |hash, &c| fnv_codepoint(hash, u32::from(c)))
}

fn emit(counts: &mut [i32], salt: u32, chars: &[char]) {
    counts[bucket(hash_chars(fnv_byte(FNV_BASIS, salt), chars))] += 1;
}

/// Emits the n-grams of `chars` from its start, of every order from the shortest to `longest`.
fn emit_prefixes(counts: &mut [i32], salt: u32, chars: &[char], longest: usize) {
    let mut hash = fnv_byte(FNV_BASIS, salt);
    for (order, &c) in (1..=longest).zip(chars) {
        hash = fnv_codepoint(hash, u32::from(c));
        if order >= MIN_NGRAM {
            counts[bucket(hash)] += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str = include_str!("../../tests/fixtures/language/charsoup-reference.tsv");

    fn unescape(field: &str) -> String {
        let mut text = String::with_capacity(field.len());
        let mut chars = field.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                text.push(c);
                continue;
            }
            text.push(match chars.next() {
                Some('n') => '\n',
                Some('t') => '\t',
                Some('r') => '\r',
                Some('\\') => '\\',
                other => panic!("unknown escape {other:?} in {field}"),
            });
        }
        text
    }

    #[test]
    fn the_embedded_model_loads() {
        assert_eq!(MODEL.labels.len(), 204);
        assert!(["dan", "nob", "nno", "swe", "isl", "fao", "fin", "sme"]
            .iter()
            .all(|label| MODEL.labels.contains(label)));
    }

    #[test]
    fn every_answer_is_the_one_tika_gives() {
        let mut checked = 0;
        for line in REFERENCE.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let [text, label, score, entropy, confidence] = fields[..] else {
                panic!("malformed reference line {line:?}");
            };
            let text = unescape(text);
            let detection = detect(&text)
                .unwrap_or_else(|| panic!("no language for {text:?}, Tika says {label}"));
            assert_eq!(detection.label, label, "{text:?}");
            let score: f32 = score.parse().unwrap();
            assert!(
                (detection.score - score).abs() <= 1e-6,
                "{text:?}: score {} against Tika's {score}",
                detection.score
            );
            let entropy: f32 = entropy.parse().unwrap();
            assert!(
                (detection.entropy - entropy).abs() <= 1e-4,
                "{text:?}: entropy {} against Tika's {entropy}",
                detection.entropy
            );
            let expected = match confidence {
                "high" => Confidence::High,
                "medium" => Confidence::Medium,
                "low" => Confidence::Low,
                other => panic!("unexpected confidence {other}"),
            };
            assert_eq!(detection.confidence, expected, "{text:?}");
            checked += 1;
        }
        assert!(checked > 4000, "only {checked} reference answers");
    }

    #[test]
    fn text_without_letters_has_no_language() {
        for text in [
            "",
            "   \n\t",
            "12345 67,89",
            "!? -- ...",
            "https://example.com/path",
        ] {
            assert_eq!(detect(text), None, "{text:?}");
        }
    }

    #[test]
    fn urls_and_mail_addresses_are_cut_as_tika_cuts_them() {
        assert_eq!(
            strip_urls("se https://example.com/a og").unwrap(),
            "se   og"
        );
        assert_eq!(strip_urls("http://kort.dk"), None);
        assert_eq!(
            strip_emails("skriv til navn@firma.dk nu").unwrap(),
            "skriv til   nu"
        );
        assert_eq!(strip_emails("a@bb").unwrap(), " ");
        assert_eq!(strip_emails("a@b"), None);
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        assert_eq!(truncate_utf16("abc", 2), "ab");
        assert_eq!(truncate_utf16("a😀b", 2), "a");
        assert_eq!(truncate_utf16("a😀b", 3), "a😀");
    }
}
