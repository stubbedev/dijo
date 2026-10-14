//! A port of Tika's `MojibusterEncodingDetector`: structural proofs (UTF-32, UTF-16 by its
//! byte-order specialist, UTF-8 grammar, ISO-2022 escapes) first, then the byte-bigram
//! classifier for everything else, with the East Asian picks whose bytes do not decode vetoed
//! and the Western Latin look-alikes of windows-1252 rewritten to it.

use super::jdk::Charset;
use super::models::{NAIVE_BAYES, UTF16};
use super::strip::strip_tags_and_entities;
use super::structural::{self, Utf8Verdict};
use super::{Candidate, Kind};
use crate::unicode;

const STRUCTURAL_CONFIDENCE: f32 = 0.95;
const FALLBACK_CONFIDENCE: f32 = 0.1;
const ISO2022_MIN_CJK: usize = 4;
const ISO2022_MAX_REPLACEMENT_RATE: f64 = 0.05;
const CJK_FAILURE_VETO: f64 = 0.025;
const UTF8_MALFORMED_TOLERANCE: f64 = 0.0001;
const UTF8_MAX_TOLERATED_ERRORS: usize = 1;
const MIN_TOLERATED_UTF8_SEQUENCES: usize = 30;
const MIN_TAGS_TO_STRIP: usize = 1;
const MIN_ENTITIES_TO_STRIP: usize = 3;
const CJK_MIN_HIGH_BYTES: usize = 30;
pub(super) const WINDOWS_1252: &str = "windows-1252";
const WESTERN_LATIN: [&str; 3] = ["x-MacRoman", "IBM850", "ISO-8859-15"];

/// Mojibuster's candidates for a probe, best first; only the best when it is a structural
/// proof.
pub(super) fn detect(probe: &[u8], content_type: Option<&str>) -> Vec<Candidate> {
    if probe.len() < 2 {
        return fallback();
    }
    if probe.iter().all(|&b| b != 0 && b < 0x80) {
        return match iso2022_verified(probe) {
            Some(charset) => vec![Candidate::new(
                charset,
                STRUCTURAL_CONFIDENCE,
                Kind::Structural,
            )],
            None => fallback(),
        };
    }
    let mut pool = Vec::new();
    let wide = structural::wide(probe);
    if let Some(utf32) = wide.utf32 {
        pool.push(Candidate::new(
            Charset::named(utf32),
            STRUCTURAL_CONFIDENCE,
            Kind::Structural,
        ));
    }
    if structural::has_two_byte_column_asymmetry_evidence(probe) {
        if let Some(utf16) = UTF16.utf16(probe) {
            let invalid = (utf16.name() == "UTF-16LE" && wide.invalid_utf16_le)
                || (utf16.name() == "UTF-16BE" && wide.invalid_utf16_be);
            if !invalid {
                return vec![Candidate::new(
                    utf16,
                    STRUCTURAL_CONFIDENCE,
                    Kind::Structural,
                )];
            }
        }
    }
    let utf8 = structural::utf8_stats(probe);
    let verdict = utf8.verdict();
    let max_tolerated =
        UTF8_MAX_TOLERATED_ERRORS.max((probe.len() as f64 * UTF8_MALFORMED_TOLERANCE) as usize);
    let tolerated = verdict == Utf8Verdict::Not && utf8.errors > 0 && utf8.errors <= max_tolerated;
    if verdict == Utf8Verdict::Likely
        || (tolerated && utf8.sequences >= MIN_TOLERATED_UTF8_SEQUENCES)
    {
        pool.push(Candidate::new(
            Charset::named("UTF-8"),
            STRUCTURAL_CONFIDENCE,
            Kind::Structural,
        ));
    }
    let stripped;
    let nb_input = match strip_markup(probe, content_type) {
        Some(text) => {
            stripped = text;
            stripped.as_slice()
        }
        None => probe,
    };
    for ranked in NAIVE_BAYES.rank(nb_input) {
        let name = ranked.charset.name();
        if name == "UTF-8" && verdict == Utf8Verdict::Not && !tolerated {
            continue;
        }
        if is_cjk_family(name)
            && stripped_failure_rate(nb_input, ranked.charset)
                .is_some_and(|rate| rate >= CJK_FAILURE_VETO)
        {
            continue;
        }
        pool.push(Candidate::new(
            ranked.charset,
            ranked.confidence,
            Kind::Statistical,
        ));
    }
    let ranked = latin_sibling_fallback(probe, sort_and_dedup(pool));
    match ranked.first() {
        None => fallback(),
        Some(top) if top.kind == Kind::Structural => vec![*top],
        Some(_) => ranked,
    }
}

fn fallback() -> Vec<Candidate> {
    vec![Candidate::new(
        Charset::named(WINDOWS_1252),
        FALLBACK_CONFIDENCE,
        Kind::Statistical,
    )]
}

/// The ISO-2022 charset of a 7-bit probe, when its escapes designate one and it decodes to East
/// Asian text rather than a stray escape in ASCII.
fn iso2022_verified(probe: &[u8]) -> Option<Charset> {
    let charset = Charset::named(structural::iso2022(probe)?);
    let decoded = charset.decode(probe)?;
    let mut cjk = 0;
    let mut replacements = 0;
    for c in decoded.chars() {
        if c == '\u{FFFD}' {
            replacements += 1;
        } else if is_cjk_char(c) {
            cjk += 1;
        }
    }
    let units = decoded.encode_utf16().count();
    (cjk >= ISO2022_MIN_CJK && replacements as f64 <= units as f64 * ISO2022_MAX_REPLACEMENT_RATE)
        .then_some(charset)
}

fn is_cjk_char(c: char) -> bool {
    matches!(u32::from(c),
        0x3040..=0x30FF | 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xAC00..=0xD7A3 | 0xFF66..=0xFF9F
        | 0x3000..=0x303F)
}

/// The probe without markup, when it may be markup (its content type says HTML or XML, or there
/// is none) and stripping found tags or a few entities.
fn strip_markup(probe: &[u8], content_type: Option<&str>) -> Option<Vec<u8>> {
    if let Some(content_type) = content_type {
        let lower = content_type.to_lowercase();
        if !lower.contains("html") && !lower.contains("xml") {
            return None;
        }
    }
    let (stripped, found) = strip_tags_and_entities(probe);
    (found.tags >= MIN_TAGS_TO_STRIP || found.entities >= MIN_ENTITIES_TO_STRIP).then_some(stripped)
}

/// Sorts by kind (structural, declarative, statistical) then confidence, keeping each charset's
/// first.
fn sort_and_dedup(mut pool: Vec<Candidate>) -> Vec<Candidate> {
    pool.sort_by(|a, b| {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then(b.confidence.total_cmp(&a.confidence))
    });
    let mut seen = Vec::with_capacity(pool.len());
    pool.retain(|candidate| {
        if seen.contains(&candidate.charset) {
            false
        } else {
            seen.push(candidate.charset);
            true
        }
    });
    pool
}

/// Rewrites a Western Latin look-alike at the top to windows-1252 when windows-1252 decodes at
/// least as many cased letters from the high bytes.
fn latin_sibling_fallback(probe: &[u8], mut ranked: Vec<Candidate>) -> Vec<Candidate> {
    let Some(top) = ranked.first().copied() else {
        return ranked;
    };
    let name = top.charset.name();
    if name == WINDOWS_1252 || !WESTERN_LATIN.contains(&name) {
        return ranked;
    }
    let windows = Charset::named(WINDOWS_1252);
    if cased_high_letters(probe, windows) < cased_high_letters(probe, top.charset) {
        return ranked;
    }
    ranked[0] = Candidate::new(windows, top.confidence, top.kind);
    ranked
}

/// The cased letters above ASCII a charset decodes the bytes to, ordinal indicators aside.
pub(super) fn cased_high_letters(bytes: &[u8], charset: Charset) -> usize {
    charset.decode(bytes).map_or(0, |text| {
        text.chars()
            .filter(|&c| {
                u32::from(c) >= 0x80
                    && !matches!(c, '\u{AA}' | '\u{BA}' | '\u{207F}')
                    && unicode::is_cased_letter(c)
            })
            .count()
    })
}

pub(super) fn high_bytes(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b >= 0x80).count()
}

/// Whether a charset is one of the East Asian multi-byte charsets the decode check applies to.
fn is_cjk_family(name: &str) -> bool {
    let name = name.to_lowercase();
    !name.contains("2022")
        && ["gb", "big5", "euc", "shift", "jis", "949"]
            .iter()
            .any(|part| name.contains(part))
}

/// The share of high bytes outside embedded UTF-8 that an East Asian charset's vendor superset
/// fails to decode: 1 for a probe that is all UTF-8, and `Some(-1)` when there are too few to
/// tell; `None` for a charset this port does not decode.
fn stripped_failure_rate(bytes: &[u8], charset: Charset) -> Option<f64> {
    let superset = superset(charset);
    let mut i = 0;
    let mut failures = 0usize;
    let mut high = 0usize;
    let mut utf8_sequences = 0usize;
    while i < bytes.len() {
        if bytes[i] < 0x80 {
            i += 1;
            continue;
        }
        let utf8 = structural::utf8_sequence_length(bytes, i);
        if utf8 > 0 {
            utf8_sequences += 1;
            i += utf8;
            continue;
        }
        high += 1;
        match superset.decode_first(&bytes[i..bytes.len().min(i + 4)])? {
            Err(()) => {
                failures += 1;
                i += 1;
            }
            Ok(consumed) => i += consumed.max(1),
        }
    }
    if high < CJK_MIN_HIGH_BYTES {
        return Some(if high == 0 && utf8_sequences >= CJK_MIN_HIGH_BYTES {
            1.0
        } else {
            -1.0
        });
    }
    Some(failures as f64 / high as f64)
}

/// The vendor superset a detected East Asian charset is decoded with (`CharsetSupersets`).
pub(super) fn superset(charset: Charset) -> Charset {
    match charset.name() {
        "EUC-KR" => Charset::named("x-windows-949"),
        "Big5" => Charset::named("Big5-HKSCS"),
        "GB2312" | "GBK" => Charset::named("GB18030"),
        "Shift_JIS" => Charset::named("windows-31j"),
        "EUC-JP" => Charset::named("x-eucJP-Open"),
        _ => charset,
    }
}
