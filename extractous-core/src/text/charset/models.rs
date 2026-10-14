//! Ports of Mojibuster's two statistical models: the UTF-16 byte-order specialist
//! (`Utf16SpecialistEncodingDetector`, a linear model over the byte ranges of even and odd
//! columns) and the byte-bigram Naive Bayes classifier (`NaiveBayesBigramEncodingDetector`)
//! that ranks the other charsets. Both read Tika 4.1's model files, embedded unchanged.

use std::sync::LazyLock;

use super::jdk::Charset;

const UTF16_MODEL: &[u8] = include_bytes!("../../../models/utf16-specialist.bin");
const NB_MODEL: &[u8] = include_bytes!("../../../models/nb-bigram.bin");

pub(super) static UTF16: LazyLock<LinearModel> = LazyLock::new(|| {
    LinearModel::parse(UTF16_MODEL)
        .unwrap_or_else(|e| panic!("The embedded UTF-16 specialist model is invalid: {e}"))
});

pub(super) static NAIVE_BAYES: LazyLock<NaiveBayes> = LazyLock::new(|| {
    NaiveBayes::parse(NB_MODEL)
        .unwrap_or_else(|e| panic!("The embedded byte-bigram model is invalid: {e}"))
});

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let slice = self
            .bytes
            .get(self.at..self.at + length)
            .ok_or_else(|| format!("truncated at byte {}", self.at))?;
        self.at += length;
        Ok(slice)
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

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn label(&mut self) -> Result<String, String> {
        let length = usize::from(self.u16()?);
        String::from_utf8(self.take(length)?.to_vec()).map_err(|e| e.to_string())
    }

    fn end(&self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(format!("{} bytes left over", self.bytes.len() - self.at))
        }
    }
}

/// The JDK charset a model label stands for.
fn charset_of_label(label: &str) -> Charset {
    Charset::named(match label {
        "UTF-16-LE" => "UTF-16LE",
        "UTF-16-BE" => "UTF-16BE",
        "UTF-32-LE" => "UTF-32LE",
        "UTF-32-BE" => "UTF-32BE",
        "IBM420-ltr" | "IBM420-rtl" => "IBM420",
        "IBM424-ltr" | "IBM424-rtl" => "IBM424",
        "x-mac-cyrillic" => "x-MacCyrillic",
        "windows-874" => "x-windows-874",
        other => other,
    })
}

/// Tika's `LinearModel` (format `LDM1`): per-class weights, quantized to a byte with a per-class
/// scale, over feature counts.
pub(super) struct LinearModel {
    buckets: usize,
    charsets: Vec<Charset>,
    scales: Vec<f32>,
    biases: Vec<f32>,
    weights: Vec<i8>,
}

impl LinearModel {
    const MIN_LOGIT_MARGIN: f32 = 1.0;
    const MIN_PROBE_BYTES: usize = 8;
    const MAX_PROBE_BYTES: usize = 512;

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(4)? != b"LDM1" {
            return Err("bad magic".to_string());
        }
        let version = reader.u32()?;
        if version != 1 && version != 2 {
            return Err(format!("version {version}"));
        }
        let buckets = reader.u32()? as usize;
        let classes = reader.u32()? as usize;
        if buckets != 12 || classes != 2 {
            return Err(format!(
                "{buckets} buckets and {classes} classes, not 12 and 2"
            ));
        }
        let charsets = (0..classes)
            .map(|_| reader.label().map(|label| charset_of_label(&label)))
            .collect::<Result<_, _>>()?;
        let scales = (0..classes)
            .map(|_| reader.f32())
            .collect::<Result<_, _>>()?;
        let biases = (0..classes)
            .map(|_| reader.f32())
            .collect::<Result<_, _>>()?;
        if version == 2 && reader.u8()? != 0 {
            reader.take(classes * 8)?;
        }
        let weights = reader
            .take(buckets * classes)?
            .iter()
            .map(|&w| w as i8)
            .collect();
        reader.end()?;
        Ok(LinearModel {
            buckets,
            charsets,
            scales,
            biases,
            weights,
        })
    }

    /// The UTF-16 byte order a probe reads in, when the model is sure of it by a clear margin.
    pub(super) fn utf16(&self, probe: &[u8]) -> Option<Charset> {
        if probe.len() < Self::MIN_PROBE_BYTES {
            return None;
        }
        let probe = &probe[..probe.len().min(Self::MAX_PROBE_BYTES)];
        let mut features = vec![0i32; self.buckets];
        for (i, &b) in probe.iter().enumerate() {
            features[byte_range(b) * 2 + (i & 1)] += 1;
        }
        let logits = self.logits(&features);
        let (winner, loser) = if logits[1] > logits[0] {
            (1, 0)
        } else {
            (0, 1)
        };
        (logits[winner] - logits[loser] >= Self::MIN_LOGIT_MARGIN).then(|| self.charsets[winner])
    }

    fn logits(&self, features: &[i32]) -> Vec<f32> {
        let present: Vec<usize> = (0..self.buckets).filter(|&b| features[b] != 0).collect();
        let clip = 1.5f32 * (present.len() as f64).sqrt() as f32;
        let classes = self.charsets.len();
        (0..classes)
            .map(|c| {
                let mut dot = 0f32;
                for &b in &present {
                    let weight = self.scales[c]
                        * f32::from(self.weights[b * classes + c])
                        * features[b] as f32;
                    dot += weight.min(clip).max(-clip);
                }
                self.biases[c] + dot
            })
            .collect()
    }
}

/// The range of a byte the UTF-16 features count: NUL, other control, printable ASCII (with
/// tab, line feed and carriage return), DEL, C1, and the rest.
fn byte_range(b: u8) -> usize {
    match b {
        0x00 => 0,
        0x09 | 0x0A | 0x0D => 2,
        0x01..=0x1F => 1,
        0x20..=0x7E => 2,
        0x7F => 3,
        0x80..=0x9F => 4,
        _ => 5,
    }
}

/// The groups of related charsets the bigram classifier caps contributions across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cohort {
    Latin,
    Cjk,
    Cyrillic,
    Greek,
    Hebrew,
    Arabic,
    Thai,
    Ebcdic,
    Utf,
}

const COHORTS: usize = 9;

fn cohort(label: &str) -> Option<Cohort> {
    Some(match label {
        "windows-1252" | "windows-1250" | "windows-1254" | "windows-1257" | "windows-1258"
        | "ISO-8859-2" | "ISO-8859-3" | "ISO-8859-16" | "x-MacRoman" | "IBM850" | "IBM852" => {
            Cohort::Latin
        }
        "Big5-HKSCS" | "EUC-JP" | "GB18030" | "Shift_JIS" | "x-EUC-TW" | "x-windows-949" => {
            Cohort::Cjk
        }
        "windows-1251" | "KOI8-R" | "KOI8-U" | "IBM855" | "IBM866" | "x-mac-cyrillic" => {
            Cohort::Cyrillic
        }
        "windows-1253" => Cohort::Greek,
        "windows-1255" => Cohort::Hebrew,
        "windows-1256" => Cohort::Arabic,
        "windows-874" => Cohort::Thai,
        "IBM1047" | "IBM500" | "IBM420-ltr" | "IBM420-rtl" | "IBM424-ltr" | "IBM424-rtl" => {
            Cohort::Ebcdic
        }
        "UTF-8" => Cohort::Utf,
        _ => return None,
    })
}

/// A charset a statistical model ranks, with its confidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ranked {
    pub charset: Charset,
    pub confidence: f32,
}

/// Tika's byte-bigram Naive Bayes classifier (format `NBB3`).
pub(super) struct NaiveBayes {
    charsets: Vec<Charset>,
    cohorts: Vec<Cohort>,
    log_p: Vec<i8>,
    idf: Vec<i8>,
    dequant: Vec<f64>,
    gb18030: Option<usize>,
}

impl NaiveBayes {
    const BIGRAMS: usize = 1 << 16;
    const MAX_PROBE_BYTES: usize = 16 * 1024;
    const TOP_K: usize = 5;
    const MARGIN_PER_BIGRAM: f64 = 0.20;
    const CAP_PER_BIGRAM: f64 = 10.0;
    const MIN_DISTINCT_FOR_CAP: usize = 30;
    const MIN_DIVERSITY_RATIO: f64 = 0.02;
    const MIN_BIGRAMS_FOR_DIVERSITY_GATE: usize = 100;

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(4)? != b"NBB3" || reader.u32()? != 3 {
            return Err("not an NBB3 model of version 3".to_string());
        }
        let classes = reader.u32()? as usize;
        let idf_scale = reader.f32()?;
        let idf: Vec<i8> = reader
            .take(Self::BIGRAMS)?
            .iter()
            .map(|&b| b as i8)
            .collect();
        let mut log_p = vec![0i8; Self::BIGRAMS * classes];
        let mut charsets = Vec::with_capacity(classes);
        let mut cohorts = Vec::with_capacity(classes);
        let mut dequant = Vec::with_capacity(classes);
        let mut gb18030 = None;
        for c in 0..classes {
            let label = reader.label()?;
            cohorts.push(cohort(&label).ok_or_else(|| format!("{label} has no cohort"))?);
            if gb18030.is_none() && label.eq_ignore_ascii_case("GB18030") {
                gb18030 = Some(c);
            }
            charsets.push(charset_of_label(&label));
            let scale = reader.f32()?;
            dequant.push(f64::from(scale) * f64::from(idf_scale));
            let unseen = reader.u8()? as i8;
            for bigram in 0..Self::BIGRAMS {
                log_p[bigram * classes + c] = unseen;
            }
            let vocabulary = reader.u32()?;
            let mut bigram = 0usize;
            for _ in 0..vocabulary {
                let mut delta = 0usize;
                let mut shift = 0;
                loop {
                    if shift >= 35 {
                        return Err("bigram delta too long".to_string());
                    }
                    let b = reader.u8()?;
                    delta |= usize::from(b & 0x7F) << shift;
                    shift += 7;
                    if b & 0x80 == 0 {
                        break;
                    }
                }
                bigram += delta;
                if bigram >= Self::BIGRAMS {
                    return Err(format!("bigram {bigram} out of range"));
                }
                log_p[bigram * classes + c] = reader.u8()? as i8;
            }
        }
        reader.end()?;
        if cohorts.iter().all(|&cohort| cohort == cohorts[0]) {
            return Err("the model spans a single cohort".to_string());
        }
        Ok(NaiveBayes {
            charsets,
            cohorts,
            log_p,
            idf,
            dequant,
            gb18030,
        })
    }

    /// The charsets whose score is within a margin of the best one's, best first: the best at
    /// confidence 1, the others less the further they are behind.
    pub(super) fn rank(&self, probe: &[u8]) -> Vec<Ranked> {
        let Some((scores, scored)) = self.scores(probe) else {
            return Vec::new();
        };
        if scored == 0 {
            return Vec::new();
        }
        let margin = Self::MARGIN_PER_BIGRAM * scored as f64;
        let k = Self::TOP_K.min(self.charsets.len());
        let mut top: Vec<(usize, f64)> = Vec::with_capacity(k + 1);
        for (c, &score) in scores.iter().enumerate() {
            if top.len() == k && score <= top[k - 1].1 {
                continue;
            }
            let position = top
                .iter()
                .position(|&(_, value)| value < score)
                .unwrap_or(top.len());
            top.insert(position, (c, score));
            top.truncate(k);
        }
        let Some(&(_, best)) = top.first() else {
            return Vec::new();
        };
        let mut ranked = Vec::with_capacity(k);
        for (i, &(c, score)) in top.iter().enumerate() {
            let gap = best - score;
            if i > 0 && gap >= margin {
                break;
            }
            let confidence = if i == 0 {
                1.0
            } else {
                (1.0 - gap / margin).max(0.0)
            };
            ranked.push(Ranked {
                charset: self.charsets[c],
                confidence: confidence as f32,
            });
        }
        ranked
    }

    fn scores(&self, probe: &[u8]) -> Option<(Vec<f64>, usize)> {
        if probe.len() < 2 {
            return None;
        }
        let probe = &probe[..probe.len().min(Self::MAX_PROBE_BYTES)];
        let mut counts = BigramCounts::new();
        let mut scored = 0usize;
        for pair in probe.windows(2) {
            if is_whitespace(pair[0]) && is_whitespace(pair[1]) {
                continue;
            }
            let bigram = (usize::from(pair[0]) << 8) | usize::from(pair[1]);
            if self.idf[bigram] == 0 {
                continue;
            }
            scored += 1;
            counts.increment(bigram as i32);
        }
        let distinct = counts.size;
        if scored >= Self::MIN_BIGRAMS_FOR_DIVERSITY_GATE
            && (distinct as f64) / (scored as f64) < Self::MIN_DIVERSITY_RATIO
        {
            return None;
        }
        let cap = distinct >= Self::MIN_DISTINCT_FOR_CAP;
        let classes = self.charsets.len();
        let mut scores = vec![0f64; classes];
        let mut contributions = vec![0f64; classes];
        for (bigram, count) in counts.entries() {
            let bigram = bigram as usize;
            let weight = f64::from(self.idf[bigram]);
            let tf = if count > 1 {
                1.0 + f64::from(count).ln()
            } else {
                f64::from(count)
            };
            let count_times_idf = tf * weight;
            let base = bigram * classes;
            let skip_gb18030 = self.gb18030.is_some()
                && (is_offending_ascii(bigram >> 8) || is_offending_ascii(bigram & 0xFF));
            let skipped = |c: usize| skip_gb18030 && Some(c) == self.gb18030;
            if !cap {
                for (c, score) in scores.iter_mut().enumerate() {
                    if !skipped(c) {
                        *score +=
                            f64::from(self.log_p[base + c]) * count_times_idf * self.dequant[c];
                    }
                }
                continue;
            }
            let mut best_per_cohort = [f64::NEG_INFINITY; COHORTS];
            let mut top_class = 0;
            let mut max = f64::NEG_INFINITY;
            for (c, slot) in contributions.iter_mut().enumerate() {
                let contribution =
                    f64::from(self.log_p[base + c]) * count_times_idf * self.dequant[c];
                *slot = contribution;
                if contribution > max {
                    max = contribution;
                    top_class = c;
                }
                let cohort = self.cohorts[c] as usize;
                if contribution > best_per_cohort[cohort] {
                    best_per_cohort[cohort] = contribution;
                }
            }
            let top_cohort = self.cohorts[top_class] as usize;
            let best_cross_cohort = best_per_cohort
                .iter()
                .enumerate()
                .filter(|&(k, _)| k != top_cohort)
                .map(|(_, &value)| value)
                .fold(
                    f64::NEG_INFINITY,
                    |best, value| if value > best { value } else { best },
                );
            let cap_value = best_cross_cohort + Self::CAP_PER_BIGRAM;
            let clip = max > cap_value;
            for c in 0..classes {
                if skipped(c) {
                    continue;
                }
                let mut value = contributions[c];
                if clip && value > cap_value {
                    value = cap_value;
                }
                scores[c] += value;
            }
        }
        Some((scores, scored))
    }
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, 0x09..=0x0D | 0x20)
}

fn is_offending_ascii(b: usize) -> bool {
    matches!(b as u8, b'{' | b'"' | b'&' | b'<' | b'>')
}

/// The bigram counts of a probe, in Tika's open-addressing table, whose slot order is the order
/// the scores sum in, so that they come out as Tika's do to the last bit.
struct BigramCounts {
    keys: Vec<i32>,
    counts: Vec<u32>,
    mask: usize,
    shift: u32,
    threshold: usize,
    size: usize,
}

impl BigramCounts {
    const INITIAL_CAPACITY: usize = 1024;
    const HASH_MULTIPLIER: i32 = 0x9E37_79B9_u32 as i32;

    fn new() -> Self {
        let mut counts = BigramCounts {
            keys: Vec::new(),
            counts: Vec::new(),
            mask: 0,
            shift: 0,
            threshold: 0,
            size: 0,
        };
        counts.allocate(Self::INITIAL_CAPACITY);
        counts
    }

    fn allocate(&mut self, capacity: usize) {
        self.keys = vec![-1; capacity];
        self.counts = vec![0; capacity];
        self.mask = capacity - 1;
        self.shift = (self.mask as u32).leading_zeros();
        self.threshold = capacity >> 1;
    }

    fn slot(&self, key: i32) -> usize {
        (key.wrapping_mul(Self::HASH_MULTIPLIER) as u32 >> self.shift) as usize
    }

    fn increment(&mut self, key: i32) {
        let mut slot = self.slot(key);
        while self.keys[slot] != -1 && self.keys[slot] != key {
            slot = (slot + 1) & self.mask;
        }
        if self.keys[slot] == -1 {
            self.keys[slot] = key;
            self.counts[slot] = 1;
            self.size += 1;
            if self.size > self.threshold {
                self.resize();
            }
        } else {
            self.counts[slot] += 1;
        }
    }

    fn resize(&mut self) {
        let keys = std::mem::take(&mut self.keys);
        let counts = std::mem::take(&mut self.counts);
        self.allocate(keys.len() << 1);
        for (key, count) in keys.into_iter().zip(counts) {
            if key == -1 {
                continue;
            }
            let mut slot = self.slot(key);
            while self.keys[slot] != -1 {
                slot = (slot + 1) & self.mask;
            }
            self.keys[slot] = key;
            self.counts[slot] = count;
        }
    }

    fn entries(&self) -> impl Iterator<Item = (i32, u32)> + '_ {
        self.keys
            .iter()
            .zip(&self.counts)
            .filter(|&(&key, _)| key != -1)
            .map(|(&key, &count)| (key, count))
    }
}
