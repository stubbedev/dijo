//! A port of Apache Tika's JunkDetector (`tika-ml-junkdetect`, model format version 15): a
//! language-independent score of how much a text reads like real text rather than mojibake,
//! reversed text or other decoding damage. A positive score is clean, a negative one junk.
//!
//! The score combines nine features, each a z-score calibrated by the model: the mean
//! log-probability of the code point bigrams in each script's letter runs (z1), of the
//! transitions between Unicode blocks (z2) and between scripts (z4), the share of control bytes
//! (z3), of letters followed by a combining mark (z5), of decoding anomalies (z6), of code points
//! in a script (z7), the longest single-script stretch (z8), and how often the script alternates
//! (z9). The arithmetic follows Tika's, float for float, so that scores match Tika's own;
//! `tests/fixtures/junk/junk-reference.tsv` holds Tika's scores to check against.

use std::collections::HashMap;
use std::io::Read;
use std::sync::LazyLock;

use flate2::read::GzDecoder;
use unicode_normalization::UnicodeNormalization;
use unicode_script::Script;

use super::blocks;
use crate::unicode;

const MODEL: &[u8] = include_bytes!("../../models/junkdetect.bin");
const MAGIC: &[u8] = b"JUNKDET1";
const VERSION: u8 = 15;
const COMMON: &str = "COMMON";
const TOKEN_START: u32 = 0x11_0000;
const TOKEN_END: u32 = 0x11_0001;
const CASE_FOLD_PENALTY: f64 = 0.25;
const QUANT_LEVELS_16: f32 = 65535.0;
const QUANT_LEVELS_8: f32 = 255.0;

/// The detector with Tika's bundled model.
pub static DETECTOR: LazyLock<JunkDetector> = LazyLock::new(|| {
    JunkDetector::parse(MODEL).unwrap_or_else(|e| panic!("The embedded junk model is invalid: {e}"))
});

/// A model's `{mu, sigma}` calibration of one feature.
#[derive(Debug, Clone, Copy)]
struct Calibration {
    mu: f32,
    sigma: f32,
}

/// A transition table quantized to 16 bits between a minimum and a maximum log-probability.
#[derive(Debug)]
struct TransitionTable {
    min: f32,
    max: f32,
    values: Vec<u16>,
    calibration: Calibration,
}

impl TransitionTable {
    /// The calibrated mean log-probability of a sequence of states (block or script buckets)
    /// in a table of `size` states, or 0 without a transition.
    fn z(&self, states: impl Iterator<Item = usize>, size: usize) -> f32 {
        let scale = (self.max - self.min) / QUANT_LEVELS_16;
        let mut previous = None;
        let mut sum = 0f64;
        let mut count = 0u32;
        for state in states {
            if let Some(previous) = previous {
                let value = f32::from(self.values[previous * size + state]);
                sum += f64::from(self.min + value * scale);
                count += 1;
            }
            previous = Some(state);
        }
        if count == 0 {
            return 0.0;
        }
        ((sum / f64::from(count)) as f32 - self.calibration.mu) / self.calibration.sigma
    }
}

/// One script's code point bigram language model.
#[derive(Debug)]
struct BigramTables {
    backoff_alpha: f32,
    codepoints: Vec<i32>,
    keys: Vec<i32>,
    values: Vec<u8>,
    bigram_min: f32,
    bigram_max: f32,
    unigram_min: f32,
    unigram_max: f32,
    unigram_fallback: f32,
    unigrams: Vec<u8>,
    calibration: Calibration,
}

impl BigramTables {
    fn index(&self, cp: u32) -> Option<usize> {
        self.codepoints.binary_search(&(cp as i32)).ok()
    }

    fn bigram(&self, a: usize, b: usize) -> Option<f32> {
        let key = ((a as i32) << 16) | (b as i32 & 0xFFFF);
        let slot = self.keys.binary_search(&key).ok()?;
        Some(dequantize(
            self.values[slot],
            self.bigram_min,
            self.bigram_max,
        ))
    }

    fn unigram(&self, index: Option<usize>) -> f64 {
        match index {
            Some(index) => f64::from(dequantize(
                self.unigrams[index],
                self.unigram_min,
                self.unigram_max,
            )),
            None => f64::from(self.unigram_fallback),
        }
    }

    /// The log-probability of the bigram (a, b): its own, or for an uppercase pair the better of
    /// its own and its lowercase twin's less a small penalty, or else the unigrams' backoff.
    fn score_pair(&self, a: u32, b: u32) -> f64 {
        let index_a = self.index(a);
        let index_b = self.index(b);
        let direct = match (index_a, index_b) {
            (Some(ia), Some(ib)) => self.bigram(ia, ib).map(f64::from),
            _ => None,
        };
        let upper_a = char::from_u32(a).is_some_and(unicode::is_uppercase);
        let upper_b = char::from_u32(b).is_some_and(unicode::is_uppercase);
        let lower_a = char::from_u32(a).is_some_and(unicode::is_lowercase);
        let lower_b = char::from_u32(b).is_some_and(unicode::is_lowercase);
        if (upper_a || upper_b) && !(lower_a || lower_b) {
            let folded_a = if upper_a { lowercase(a) } else { a };
            let folded_b = if upper_b { lowercase(b) } else { b };
            if folded_a != a || folded_b != b {
                if let (Some(ia), Some(ib)) = (self.index(folded_a), self.index(folded_b)) {
                    if let Some(folded) = self.bigram(ia, ib) {
                        let folded = f64::from(folded) - CASE_FOLD_PENALTY;
                        return direct.map_or(folded, |direct| direct.max(folded));
                    }
                }
            }
        }
        if let Some(direct) = direct {
            return direct;
        }
        f64::from(self.backoff_alpha) * (self.unigram(index_a) + self.unigram(index_b))
    }
}

/// The model class a letter's run is scored with: the pooled pseudo-scripts, or a script, with
/// the Japanese kana scored as Han.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Class {
    Common,
    Script(Script),
}

impl Class {
    fn of(c: char) -> Self {
        match unicode::script(c) {
            script if unicode::is_pseudo_script(script) => Class::Common,
            Script::Hiragana | Script::Katakana => Class::Script(Script::Han),
            script => Class::Script(script),
        }
    }
}

/// Tika's junk detection model.
#[derive(Debug)]
pub struct JunkDetector {
    scripts: HashMap<Class, BigramTables>,
    blocks: TransitionTable,
    script_transitions: TransitionTable,
    script_buckets: HashMap<Script, usize>,
    script_bucket_count: usize,
    control: Calibration,
    letter_mark: Calibration,
    replacement: Calibration,
    alternation: Calibration,
    weights: Vec<f32>,
}

impl JunkDetector {
    /// The score of a text: positive when it reads as real text, negative as junk; `None` for an
    /// empty text, which gives no evidence either way.
    pub fn score(&self, text: &str) -> Option<f32> {
        if text.is_empty() {
            return None;
        }
        let text: String = text.nfkc().collect();
        let chars: Vec<char> = text.chars().collect();
        let mut buckets: Vec<(Class, f64, u64)> = Vec::new();
        for_each_script_bigram(&chars, |class, a, b| {
            let Some(tables) = self.scripts.get(&class) else {
                return;
            };
            let lp = tables.score_pair(a, b);
            match buckets.iter_mut().find(|(bucket, _, _)| *bucket == class) {
                Some((_, sum, count)) => {
                    *sum += lp;
                    *count += 1;
                }
                None => buckets.push((class, lp, 1)),
            }
        });
        let mut weighted = 0f64;
        let mut z1_count = 0u64;
        for (class, sum, count) in &buckets {
            let calibration = self.scripts[class].calibration;
            let mean = sum / *count as f64;
            let z = (mean - f64::from(calibration.mu)) / f64::from(calibration.sigma);
            weighted += z * *count as f64;
            z1_count += count;
        }

        let z2 = self.blocks.z(
            chars.iter().map(|&c| usize::from(blocks::id(u32::from(c)))),
            blocks::COUNT,
        );
        let z3 = control_z(text.as_bytes(), self.control);
        let z4 = self.script_transitions.z(
            chars.iter().filter_map(|&c| {
                let script = unicode::script(c);
                (!unicode::is_pseudo_script(script)).then(|| {
                    self.script_buckets
                        .get(&script)
                        .copied()
                        .unwrap_or(self.script_bucket_count - 1)
                })
            }),
            self.script_bucket_count,
        );
        let z5 = letter_adjacent_to_mark_ratio(&chars).map_or(0.0, |raw| {
            (raw as f32 - self.letter_mark.mu) / self.letter_mark.sigma
        });
        let z6 = (self.replacement.mu - replacement_ratio(&chars) as f32) / self.replacement.sigma;
        let z7 = script_density(&chars).map_or(0.0, |density| density as f32);
        let z8 = 1.0 - script_fragmentation(&chars) as f32;
        let z9 = (self.alternation.mu - script_alternation_ratio(&chars) as f32)
            / self.alternation.sigma;

        if z1_count == 0 {
            return Some(-7.0 + 4.0 * z7 + 6.0 * z8 + 4.0 * z6);
        }
        let z1 = (weighted / z1_count as f64) as f32;
        Some(self.combine(&[z1, z2, z3, z4, z5, z6, z7, z8, z9]))
    }

    /// The combiner's logit: its bias, then each weight times its feature, in order.
    fn combine(&self, features: &[f32; 9]) -> f32 {
        let (bias, weights) = self
            .weights
            .split_last()
            .expect("the model has a combiner bias");
        let mut logit = *bias;
        for (weight, feature) in weights.iter().zip(features) {
            logit += weight * feature;
        }
        logit
    }

    fn parse(gzipped: &[u8]) -> Result<Self, String> {
        let mut bytes = Vec::new();
        GzDecoder::new(gzipped)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("cannot decompress it: {e}"))?;
        let mut reader = Reader {
            bytes: &bytes,
            at: 0,
        };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err("bad magic".to_string());
        }
        let version = reader.u8()?;
        if version != VERSION {
            return Err(format!("format version {version}, not {VERSION}"));
        }
        let script_count = reader.u32()? as usize;
        let scheme = reader.u8()?;
        if scheme != blocks::SCHEME_VERSION {
            return Err(format!(
                "block scheme {scheme}, not {}",
                blocks::SCHEME_VERSION
            ));
        }
        let bucket_count = usize::from(reader.u8()?);
        let mut script_buckets = HashMap::with_capacity(bucket_count);
        for bucket in 0..bucket_count {
            let name = reader.name()?;
            if let Some(script) = unicode::java_script(&name) {
                script_buckets.insert(script, bucket);
            }
        }
        let script_transitions = reader.transitions(bucket_count * bucket_count)?;
        let blocks = reader.transitions(blocks::COUNT * blocks::COUNT)?;
        let control = reader.calibration()?;
        let letter_mark = reader.calibration()?;
        let replacement = reader.calibration()?;
        let alternation = reader.calibration()?;
        let feature_count = usize::from(reader.u8()?);
        if feature_count != 9 {
            return Err(format!("{feature_count} features, not 9"));
        }
        let weights = (0..=feature_count)
            .map(|_| reader.f32())
            .collect::<Result<Vec<_>, _>>()?;
        let mut scripts = HashMap::with_capacity(script_count);
        for _ in 0..script_count {
            let name = reader.name()?;
            let calibration = reader.calibration()?;
            let tables = reader.bigram_tables(calibration)?;
            let class = if name == COMMON {
                Some(Class::Common)
            } else {
                unicode::java_script(&name).map(Class::Script)
            };
            if let Some(class) = class {
                scripts.insert(class, tables);
            }
        }
        if reader.at != bytes.len() {
            return Err(format!("{} bytes left over", bytes.len() - reader.at));
        }
        Ok(JunkDetector {
            scripts,
            blocks,
            script_transitions,
            script_buckets,
            script_bucket_count: bucket_count,
            control,
            letter_mark,
            replacement,
            alternation,
            weights,
        })
    }
}

/// Hands each scored bigram of a text to `sink` with the class whose model scores it, as Tika's
/// `forEachScriptBigram`: letters form runs of one class, each opened by a start token and
/// closed by an end token; whitespace, NUL and decoding anomalies end a run; any other
/// non-letter inside a run joins it.
fn for_each_script_bigram(chars: &[char], mut sink: impl FnMut(Class, u32, u32)) {
    let mut run: Option<Class> = None;
    let mut previous = 0u32;
    for &c in chars {
        let cp = u32::from(c);
        if unicode::is_letter(c) {
            let class = Class::of(c);
            if run == Some(class) {
                sink(class, previous, cp);
            } else {
                if let Some(open) = run {
                    sink(open, previous, TOKEN_END);
                }
                run = Some(class);
                sink(class, TOKEN_START, cp);
            }
            previous = cp;
        } else if is_boundary(c) {
            if let Some(open) = run.take() {
                sink(open, previous, TOKEN_END);
            }
        } else if let Some(open) = run {
            sink(open, previous, cp);
            previous = cp;
        }
    }
    if let Some(open) = run {
        sink(open, previous, TOKEN_END);
    }
}

fn is_boundary(c: char) -> bool {
    c == '\0' || unicode::is_whitespace(c) || unicode::is_space_char(c) || is_anomaly(c)
}

/// A code point a correct decoding hardly yields: U+FFFD, an unexpected C0 control or DEL, a C1
/// control, or a private use character.
fn is_anomaly(c: char) -> bool {
    matches!(
        u32::from(c),
        0xFFFD
            | 0x01..=0x08
            | 0x0B
            | 0x0C
            | 0x0E..=0x1F
            | 0x7F..=0x9F
            | 0xE000..=0xF8FF
            | 0xF_0000..=0x10_FFFD
    )
}

fn lowercase(cp: u32) -> u32 {
    char::from_u32(cp).map_or(cp, |c| u32::from(unicode::lowercase(c)))
}

fn dequantize(value: u8, min: f32, max: f32) -> f32 {
    min + (f32::from(value) / QUANT_LEVELS_8) * (max - min)
}

/// The calibrated share of control bytes, negated so that higher is cleaner.
fn control_z(utf8: &[u8], calibration: Calibration) -> f32 {
    if utf8.is_empty() {
        return 0.0;
    }
    let controls = utf8
        .iter()
        .filter(|&&b| matches!(b, 0x01..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F))
        .count();
    let score = -(controls as f32) / utf8.len() as f32;
    (score - calibration.mu) / calibration.sigma
}

fn letter_adjacent_to_mark_ratio(chars: &[char]) -> Option<f64> {
    if chars.len() < 2 {
        return None;
    }
    let hits = chars
        .windows(2)
        .filter(|pair| unicode::is_letter(pair[0]) && unicode::is_mark(pair[1]))
        .count();
    Some(hits as f64 / (chars.len() - 1) as f64)
}

fn replacement_ratio(chars: &[char]) -> f64 {
    let mut candidates = 0u32;
    let mut anomalies = 0u32;
    for &c in chars {
        let anomaly = is_anomaly(c);
        if anomaly {
            anomalies += 1;
        }
        if u32::from(c) >= 0x80 || anomaly {
            candidates += 1;
        }
    }
    if candidates == 0 {
        0.0
    } else {
        f64::from(anomalies) / f64::from(candidates)
    }
}

/// The scripts of a text's code points, without the pseudo-scripts.
fn scripts(chars: &[char]) -> impl Iterator<Item = Script> + '_ {
    chars
        .iter()
        .map(|&c| unicode::script(c))
        .filter(|&script| !unicode::is_pseudo_script(script))
}

fn script_density(chars: &[char]) -> Option<f64> {
    let counted = chars.iter().filter(|&&c| c != '\u{FFFD}');
    let total = counted.clone().count();
    if total == 0 {
        return None;
    }
    let scripted = counted
        .filter(|&&c| !unicode::is_pseudo_script(unicode::script(c)))
        .count();
    Some(scripted as f64 / total as f64)
}

fn script_fragmentation(chars: &[char]) -> f64 {
    let mut total = 0u32;
    let mut longest = 0u32;
    let mut current = 0u32;
    let mut current_script = None;
    for script in scripts(chars) {
        total += 1;
        if current_script == Some(script) {
            current += 1;
        } else {
            longest = longest.max(current);
            current_script = Some(script);
            current = 1;
        }
    }
    longest = longest.max(current);
    if total == 0 {
        return 0.0;
    }
    1.0 - f64::from(longest) / f64::from(total)
}

/// Script transitions between neighbouring scripted code points, against the number expected if
/// the scripts were shuffled: high when another script is scattered in singletons.
fn script_alternation_ratio(chars: &[char]) -> f64 {
    let mut counts: Vec<(Script, u32)> = Vec::new();
    let mut total = 0u32;
    for script in scripts(chars) {
        match counts.iter_mut().find(|(seen, _)| *seen == script) {
            Some((_, count)) => *count += 1,
            None => counts.push((script, 1)),
        }
        total += 1;
    }
    if counts.len() <= 1 || total < 2 {
        return 0.0;
    }
    let squares: f64 = counts
        .iter()
        .map(|&(_, count)| {
            let share = f64::from(count) / f64::from(total);
            share * share
        })
        .sum();
    let expected = f64::from(total - 1) * (1.0 - squares);
    if expected <= 0.0 {
        return 0.0;
    }
    let transitions = scripts(chars)
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|pair| pair[0] != pair[1])
        .count();
    transitions as f64 / expected
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], String> {
        let end = self
            .at
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| format!("truncated at byte {}", self.at))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn name(&mut self) -> Result<String, String> {
        let length = usize::from(self.u16()?);
        String::from_utf8(self.take(length)?.to_vec()).map_err(|e| format!("bad name: {e}"))
    }

    fn calibration(&mut self) -> Result<Calibration, String> {
        let calibration = Calibration {
            mu: self.f32()?,
            sigma: self.f32()?,
        };
        if !calibration.sigma.is_finite() || calibration.sigma <= 0.0 {
            return Err(format!("calibration sigma {}", calibration.sigma));
        }
        Ok(calibration)
    }

    fn transitions(&mut self, size: usize) -> Result<TransitionTable, String> {
        let min = self.f32()?;
        let max = self.f32()?;
        let values = (0..size).map(|_| self.u16()).collect::<Result<_, _>>()?;
        let calibration = self.calibration()?;
        Ok(TransitionTable {
            min,
            max,
            values,
            calibration,
        })
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            if shift >= 64 {
                return Err("varint too long".to_string());
            }
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7F) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
    }

    fn bigram_tables(&mut self, calibration: Calibration) -> Result<BigramTables, String> {
        let backoff_alpha = self.f32()?;
        let codepoint_count = self.u32()? as usize;
        let codepoints = (0..codepoint_count)
            .map(|_| self.i32())
            .collect::<Result<_, _>>()?;
        let slots = self.u32()? as usize;
        let bigram_min = self.f32()?;
        let bigram_max = self.f32()?;
        let mut keys = Vec::with_capacity(slots);
        if slots > 0 {
            keys.push(self.i32()?);
            for _ in 1..slots {
                let next = i64::from(*keys.last().unwrap())
                    .checked_add(self.varint()? as i64)
                    .filter(|&next| next <= i64::from(i32::MAX))
                    .ok_or("bigram keys out of range")?;
                if next <= i64::from(*keys.last().unwrap()) {
                    return Err("bigram keys not ascending".to_string());
                }
                keys.push(next as i32);
            }
        }
        let values = self.take(slots)?.to_vec();
        let unigram_min = self.f32()?;
        let unigram_max = self.f32()?;
        let unigram_fallback = self.f32()?;
        let unigrams = self.take(codepoint_count)?.to_vec();
        Ok(BigramTables {
            backoff_alpha,
            codepoints,
            keys,
            values,
            bigram_min,
            bigram_max,
            unigram_min,
            unigram_max,
            unigram_fallback,
            unigrams,
            calibration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str = include_str!("../../tests/fixtures/junk/junk-reference.tsv");
    const TOLERANCE: f32 = 1e-5;

    fn unescape(field: &str) -> String {
        let mut text = String::with_capacity(field.len());
        let mut chars = field.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                text.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => text.push('\n'),
                Some('t') => text.push('\t'),
                Some('r') => text.push('\r'),
                Some('\\') => text.push('\\'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let cp = u32::from_str_radix(&hex, 16).expect("four hex digits");
                    text.push(char::from_u32(cp).expect("a code point"));
                }
                other => panic!("unknown escape {other:?} in {field}"),
            }
        }
        text
    }

    #[test]
    fn scores_as_tika_does() {
        let mut mismatches = Vec::new();
        let mut count = 0;
        for line in REFERENCE.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let input = unescape(fields[0]);
            let expected =
                (!fields[1].is_empty()).then(|| fields[1].parse::<f32>().expect("a float"));
            let actual = DETECTOR.score(&input);
            count += 1;
            let agrees = match (expected, actual) {
                (None, None) => true,
                (Some(expected), Some(actual)) => {
                    (expected - actual).abs() <= TOLERANCE * expected.abs().max(1.0)
                }
                _ => false,
            };
            if !agrees {
                mismatches.push(format!(
                    "{:?}: Tika {expected:?} {:?}, port {actual:?}",
                    input.chars().take(60).collect::<String>(),
                    &fields[2..]
                ));
            }
        }
        assert!(count > 10_000, "the reference has {count} inputs");
        assert!(
            mismatches.is_empty(),
            "{} of {count} scores differ from Tika's:\n{}",
            mismatches.len(),
            mismatches
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
