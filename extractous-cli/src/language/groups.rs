//! Models that tell apart the languages of a group the CharSoup model confuses, such as Danish,
//! Norwegian and Swedish, trained on Wikipedia by `just language-groups`: a multinomial logistic
//! regression per group over each word of a text, lowercased, and the character 1- to 5-grams of
//! its words, hashed into buckets and L2-normalised, with weights quantised to a signed byte.

use std::sync::LazyLock;

use unicode_normalization::UnicodeNormalization;
use unicode_script::Script;

use extractous_core::unicode;

const MODEL_BYTES: &[u8] = include_bytes!("../../models/language-groups.bin");
const MAGIC: u32 = 0x4C47_5250;
const VERSION: u32 = 3;
const BUCKETS: usize = 1 << 18;
const MAX_NGRAM: usize = 5;
const WORD_JOINERS: [char; 3] = ['\'', '\u{2019}', '-'];
const WORD_TAG: u8 = b'w';
const NGRAM_TAG: u8 = b'c';
const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01B3;
/// How much of a text, from its start, the models read: a few paragraphs settle which language
/// of a group it is.
const MAX_TEXT_CHARS: usize = 8 * 1024;

/// Languages of one script that are told apart by their own model.
pub struct Group {
    pub script: Script,
    pub labels: Vec<&'static str>,
    /// Whether CharSoup scores each label in this script; it knows some languages only in
    /// another script (Serbian) or not at all (Bosnian).
    pub scored: Vec<bool>,
    biases: Vec<f32>,
    scales: Vec<f32>,
    /// `BUCKETS` rows of one signed byte per label.
    weights: &'static [u8],
}

impl Group {
    /// Each label's logit for the start of the text, or None when it has no words.
    pub fn logits(&self, text: &str) -> Option<Vec<f32>> {
        let features = Features::of(start(text, MAX_TEXT_CHARS))?;
        let classes = self.labels.len();
        let mut dots = vec![0f32; classes];
        for &(bucket, value) in &features.values {
            let row = &self.weights[bucket * classes..][..classes];
            for (dot, &weight) in dots.iter_mut().zip(row) {
                *dot += f32::from(weight as i8) * value;
            }
        }
        Some(
            (0..classes)
                .map(|class| self.biases[class] + self.scales[class] * dots[class])
                .collect(),
        )
    }
}

/// The longest prefix of whole characters within `limit` characters.
fn start(text: &str, limit: usize) -> &str {
    text.char_indices()
        .nth(limit)
        .map_or(text, |(index, _)| &text[..index])
}

/// The group a language of a script belongs to, if any.
pub fn group_of(script: Script, label: &str) -> Option<&'static Group> {
    GROUPS
        .iter()
        .find(|group| group.script == script && group.labels.contains(&label))
}

static GROUPS: LazyLock<Vec<Group>> = LazyLock::new(|| {
    parse(MODEL_BYTES)
        .unwrap_or_else(|error| panic!("the embedded language group model is malformed: {error}"))
});

fn parse(bytes: &'static [u8]) -> Result<Vec<Group>, String> {
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
    let count = reader.u32()? as usize;
    let mut groups = Vec::with_capacity(count);
    for _ in 0..count {
        let name = reader.string()?;
        let script_name = reader.string()?;
        let script = Script::from_full_name(script_name)
            .ok_or_else(|| format!("group {name} has the unknown script {script_name}"))?;
        let classes = reader.u32()? as usize;
        let mut labels = Vec::with_capacity(classes);
        let mut scored = Vec::with_capacity(classes);
        let mut biases = Vec::with_capacity(classes);
        let mut scales = Vec::with_capacity(classes);
        for _ in 0..classes {
            labels.push(reader.string()?);
            scored.push(match reader.take(1)?[0] {
                0 => false,
                1 => true,
                flag => return Err(format!("group {name} has the flag {flag} for a label")),
            });
            biases.push(reader.f32()?);
            scales.push(reader.f32()?);
        }
        let weights = reader.take(BUCKETS * classes)?;
        groups.push(Group {
            script,
            labels,
            scored,
            biases,
            scales,
            weights,
        });
    }
    if reader.at != bytes.len() {
        return Err(format!(
            "{} bytes after the groups",
            bytes.len() - reader.at
        ));
    }
    Ok(groups)
}

/// A text's features: each bucket that its words and their character n-grams fall into, with
/// its count divided by the L2 norm of all counts.
struct Features {
    values: Vec<(usize, f32)>,
}

impl Features {
    fn of(text: &str) -> Option<Self> {
        let mut buckets = buckets(text);
        if buckets.is_empty() {
            return None;
        }
        buckets.sort_unstable();
        let mut counts: Vec<(usize, f32)> = Vec::new();
        for bucket in buckets {
            match counts.last_mut() {
                Some((last, count)) if *last == bucket => *count += 1.0,
                _ => counts.push((bucket, 1.0)),
            }
        }
        let norm = counts
            .iter()
            .map(|&(_, count)| count * count)
            .sum::<f32>()
            .sqrt();
        for (_, count) in &mut counts {
            *count /= norm;
        }
        Some(Self { values: counts })
    }
}

/// The buckets of a text's features: each word, lowercased, and each 1- to 5-gram of its
/// characters with a space before and after it. A word is a run of letters and marks (after
/// NFC), which apostrophes and hyphens may join.
fn buckets(text: &str) -> Vec<usize> {
    let mut buckets = Vec::new();
    let mut padded = Vec::new();
    for word in words(text) {
        buckets.push(bucket(fnv(WORD_TAG, word.chars())));
        padded.clear();
        padded.push(' ');
        padded.extend(word.chars());
        padded.push(' ');
        for start in 0..padded.len() {
            let mut hash = fnv(NGRAM_TAG, std::iter::empty());
            for &character in padded[start..].iter().take(MAX_NGRAM) {
                hash = fnv_extend(hash, character);
                buckets.push(bucket(hash));
            }
        }
    }
    buckets
}

fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    for character in text.nfc() {
        if is_word_character(character) || (!word.is_empty() && WORD_JOINERS.contains(&character)) {
            word.push(character);
        } else if !word.is_empty() {
            push_word(&mut words, &word);
            word.clear();
        }
    }
    push_word(&mut words, &word);
    words
}

fn push_word(words: &mut Vec<String>, word: &str) {
    let trimmed = word.trim_matches(WORD_JOINERS.as_slice());
    if !trimmed.is_empty() {
        words.push(trimmed.to_lowercase());
    }
}

/// A character a word is made of: a letter, or a mark on one.
fn is_word_character(character: char) -> bool {
    unicode::is_letter(character) || unicode::is_mark(character)
}

/// FNV-1a over a tag byte and the UTF-8 of the characters.
fn fnv(tag: u8, characters: impl Iterator<Item = char>) -> u64 {
    characters.fold(
        (FNV_OFFSET ^ u64::from(tag)).wrapping_mul(FNV_PRIME),
        fnv_extend,
    )
}

fn fnv_extend(hash: u64, character: char) -> u64 {
    let mut buffer = [0; 4];
    character
        .encode_utf8(&mut buffer)
        .bytes()
        .fold(hash, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
        })
}

fn bucket(hash: u64) -> usize {
    (hash % BUCKETS as u64) as usize
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

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn string(&mut self) -> Result<&'static str, String> {
        let length = usize::from(u16::from_be_bytes(self.take(2)?.try_into().unwrap()));
        std::str::from_utf8(self.take(length)?).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    const SOURCES: &str = "models/language-groups.tsv";
    const MODEL: &str = "models/language-groups.bin";
    const EPOCHS: usize = 3;
    const LEARNING_RATE: f32 = 0.5;
    const ADAGRAD_EPSILON: f32 = 1e-6;
    const SEED: u64 = 20_261_014;
    const QUANTISED_MAX: f32 = 127.0;

    #[test]
    fn words_are_lowercased_letter_runs_joined_by_apostrophes_and_hyphens() {
        assert_eq!(
            words("Tal-ħajja, l’homme -- 'quoted' ÆBLE og 12 æbler."),
            ["tal-ħajja", "l’homme", "quoted", "æble", "og", "æbler"]
        );
        assert!(words("12, 34 -- !").is_empty());
    }

    #[test]
    fn every_group_parses_and_scores() {
        assert!(!GROUPS.is_empty());
        for group in GROUPS.iter() {
            let logits = group.logits("text").unwrap();
            assert_eq!(logits.len(), group.labels.len(), "{:?}", group.labels);
            assert!(group.logits("12 34").is_none());
        }
    }

    /// A group of the sources: its name, script and members.
    struct Source {
        name: String,
        script: String,
        members: Vec<Member>,
    }

    /// A language of a group, and whether CharSoup scores it in the group's script.
    struct Member {
        label: String,
        scored: bool,
    }

    /// The groups of the sources, in the order they first appear.
    fn sources() -> Vec<Source> {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources: Vec<Source> = Vec::new();
        for line in fs::read_to_string(manifest.join(SOURCES)).unwrap().lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let scored = match fields[6] {
                "scored" => true,
                "unscored" => false,
                other => panic!("{SOURCES}: unknown CharSoup column {other:?} in {line:?}"),
            };
            let member = Member {
                label: fields[2].to_string(),
                scored,
            };
            match sources
                .iter_mut()
                .find(|source| source.name == fields[0] && source.script == fields[1])
            {
                Some(source) => source.members.push(member),
                None => sources.push(Source {
                    name: fields[0].to_string(),
                    script: fields[1].to_string(),
                    members: vec![member],
                }),
            }
        }
        sources
    }

    /// A deterministic shuffle (SplitMix64 and Fisher-Yates), so that training is reproducible.
    fn shuffle<T>(items: &mut [T], seed: u64) {
        let mut state = seed;
        for index in (1..items.len()).rev() {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut mixed = state;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            mixed ^= mixed >> 31;
            items.swap(index, (mixed % (index as u64 + 1)) as usize);
        }
    }

    /// Fits a multinomial logistic regression with AdaGrad to the labelled texts and returns
    /// its weights (`BUCKETS` rows of one per class) and biases.
    fn fit(texts: &[(usize, String)], classes: usize) -> (Vec<f32>, Vec<f32>) {
        let mut weights = vec![0f32; BUCKETS * classes];
        let mut weight_squares = vec![0f32; BUCKETS * classes];
        let mut biases = vec![0f32; classes];
        let mut bias_squares = vec![0f32; classes];
        let mut order: Vec<usize> = (0..texts.len()).collect();
        let mut gradient = vec![0f32; classes];
        for epoch in 0..EPOCHS {
            shuffle(&mut order, SEED + epoch as u64);
            for &index in &order {
                let (class, text) = &texts[index];
                let Some(features) = Features::of(text) else {
                    continue;
                };
                let mut logits = biases.clone();
                for &(bucket, value) in &features.values {
                    for (logit, weight) in logits
                        .iter_mut()
                        .zip(&weights[bucket * classes..][..classes])
                    {
                        *logit += weight * value;
                    }
                }
                let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let sum: f32 = logits.iter().map(|logit| (logit - max).exp()).sum();
                for (target, (slot, logit)) in gradient.iter_mut().zip(&logits).enumerate() {
                    *slot = (logit - max).exp() / sum - f32::from(u8::from(target == *class));
                }
                for (target, &step) in gradient.iter().enumerate() {
                    bias_squares[target] += step * step;
                    biases[target] -=
                        LEARNING_RATE * step / (bias_squares[target] + ADAGRAD_EPSILON).sqrt();
                }
                for &(bucket, value) in &features.values {
                    for (target, &step) in gradient.iter().enumerate() {
                        let slot = bucket * classes + target;
                        let update = step * value;
                        weight_squares[slot] += update * update;
                        weights[slot] -= LEARNING_RATE * update
                            / (weight_squares[slot] + ADAGRAD_EPSILON).sqrt();
                    }
                }
            }
        }
        (weights, biases)
    }

    /// Trains the model from the sentences `language-groups.py` sampled into
    /// LANGUAGE_GROUPS_SENTENCES (`<group>/<label>.txt`), for the groups of the sources, and
    /// writes it to models/language-groups.bin.
    #[test]
    #[ignore = "trains the model; `just language-groups` runs it"]
    fn train() {
        let sentences = PathBuf::from(
            std::env::var_os("LANGUAGE_GROUPS_SENTENCES")
                .expect("LANGUAGE_GROUPS_SENTENCES names the sampled sentences"),
        );
        let groups = sources();
        let mut model = Vec::new();
        model.extend_from_slice(&MAGIC.to_be_bytes());
        model.extend_from_slice(&VERSION.to_be_bytes());
        model.extend_from_slice(&(BUCKETS as u32).to_be_bytes());
        model.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for Source {
            name,
            script,
            members,
        } in &groups
        {
            let mut texts = Vec::new();
            for (class, Member { label, .. }) in members.iter().enumerate() {
                let path = sentences.join(name).join(format!("{label}.txt"));
                let text = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("Failed to read {}: {error}", path.display()));
                texts.extend(text.lines().map(|line| (class, line.to_string())));
            }
            let classes = members.len();
            let (weights, biases) = fit(&texts, classes);
            write_string(&mut model, name);
            write_string(&mut model, script);
            model.extend_from_slice(&(classes as u32).to_be_bytes());
            let mut scales = vec![0f32; classes];
            for (class, Member { label, scored }) in members.iter().enumerate() {
                let largest = (0..BUCKETS)
                    .map(|bucket| weights[bucket * classes + class].abs())
                    .fold(0f32, f32::max);
                scales[class] = largest.max(f32::MIN_POSITIVE) / QUANTISED_MAX;
                write_string(&mut model, label);
                model.push(u8::from(*scored));
                model.extend_from_slice(&biases[class].to_be_bytes());
                model.extend_from_slice(&scales[class].to_be_bytes());
            }
            model.extend(weights.iter().enumerate().map(|(slot, weight)| {
                (weight / scales[slot % classes])
                    .round()
                    .clamp(-QUANTISED_MAX, QUANTISED_MAX) as i8 as u8
            }));
            eprintln!("{name}: {} sentences of {classes} languages", texts.len());
        }
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODEL);
        let partial = target.with_extension("partial");
        fs::File::create(&partial)
            .and_then(|mut file| file.write_all(&model).and_then(|()| file.sync_all()))
            .unwrap_or_else(|error| panic!("Failed to write {}: {error}", partial.display()));
        fs::rename(&partial, &target)
            .unwrap_or_else(|error| panic!("Failed to replace {}: {error}", target.display()));
    }

    fn write_string(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&(text.len() as u16).to_be_bytes());
        out.extend_from_slice(text.as_bytes());
    }
}
