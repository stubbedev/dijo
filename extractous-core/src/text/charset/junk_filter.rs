//! A port of Tika's `JunkFilterEncodingDetector`, which arbitrates when the detectors disagree:
//! each candidate charset decodes the probe, markup is stripped and entities expanded, and the
//! JunkDetector's score picks the decoding that reads most like text, with Tika's guards around
//! it: a declaration stands when it decodes as another candidate does, unless a structural proof
//! contradicts it; a Chinese, Japanese or Korean pick yields to a Western one its letters read
//! clearly better in; an IBM or Mac pick yields to windows-1252 when that decodes clearly more
//! cased letters; and a declaration wins outright when the statistics had nothing confident to
//! say.

use std::collections::HashMap;

use super::jdk::Charset;
use super::mojibuster::{cased_high_letters, high_bytes, superset, WINDOWS_1252};
use super::strip::strip_tags;
use super::structural::has_wide_utf8_sequence;
use super::{Kind, Report};
use crate::text::junk::DETECTOR;
use crate::unicode;

const NO_INFO_CONFIDENCE: f32 = 0.1;
const ALWAYS_KEEP_TOP: usize = 2;
const MIN_TAIL_CONFIDENCE: f32 = 0.5;
const FAMILY_DIFF_MARGIN: f64 = 2.0;
const LATIN_GATE_MIN_HIGH_BYTES: usize = 16;
const LATIN_GATE_MARGIN_FLOOR: f64 = 6.0;
const LATIN_GATE_MARGIN_FRACTION: f64 = 0.20;
const LATIN_SBCS: [&str; 27] = [
    "windows-1252",
    "windows-1250",
    "windows-1254",
    "windows-1257",
    "windows-1258",
    "ISO-8859-1",
    "ISO-8859-2",
    "ISO-8859-3",
    "ISO-8859-4",
    "ISO-8859-9",
    "ISO-8859-10",
    "ISO-8859-13",
    "ISO-8859-14",
    "ISO-8859-15",
    "ISO-8859-16",
    "IBM437",
    "IBM850",
    "IBM852",
    "IBM858",
    "IBM860",
    "IBM861",
    "IBM863",
    "IBM865",
    "x-MacRoman",
    "x-MacCentralEurope",
    "x-MacRomania",
    "x-MacIceland",
];

/// The charset the arbitration picks among the detectors' reports for a probe, or `None` when
/// it abstains and the first report stands.
pub(super) fn arbitrate(reports: &[Report], probe: &[u8]) -> Option<Charset> {
    let unique = unique_charsets(reports);
    if unique.len() <= 1 || probe.is_empty() {
        return None;
    }
    let bytes = &probe[bom_length(probe)..];
    let mut candidates: Vec<(Charset, String)> = Vec::new();
    let mut cleaned_by_raw: HashMap<String, String> = HashMap::new();
    for charset in band(reports, &unique) {
        let Some(raw) = superset(charset).decode(bytes) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let cleaned = cleaned_by_raw
            .entry(raw)
            .or_insert_with_key(|raw| clean(raw))
            .clone();
        if !cleaned.is_empty() {
            candidates.push((charset, cleaned));
        }
    }
    if candidates.len() <= 1 {
        return None;
    }
    if let Some(declared) = declared_with_equivalent_decode(reports, &candidates) {
        return Some(
            match conflicting_structural(reports, &candidates, declared) {
                Some(structural) if overrides_declared(structural, declared, bytes) => structural,
                _ => declared,
            },
        );
    }
    if candidates.iter().all(|(_, text)| *text == candidates[0].1) {
        return None;
    }

    let mut scores_by_text: HashMap<&str, f64> = HashMap::new();
    let mut scores: Vec<(Charset, f64)> = Vec::with_capacity(candidates.len());
    let mut champion = None;
    let mut champion_score = f64::NEG_INFINITY;
    for (charset, text) in &candidates {
        let score = *scores_by_text
            .entry(text)
            .or_insert_with(|| DETECTOR.score(text).map_or(f64::NEG_INFINITY, f64::from));
        scores.push((*charset, score));
        if score > champion_score {
            champion_score = score;
            champion = Some(*charset);
        }
    }
    let mut champion = champion.unwrap_or(candidates[0].0);

    if is_cjk(champion) {
        let mut best_cjk = f64::NEG_INFINITY;
        let mut best_other = f64::NEG_INFINITY;
        let mut diffs_by_text: HashMap<&str, f64> = HashMap::new();
        for (charset, text) in &candidates {
            let diff = *diffs_by_text.entry(text).or_insert_with(|| {
                let letters: String = text
                    .chars()
                    .filter(|&c| u32::from(c) >= 0x80 && unicode::is_letter(c))
                    .collect();
                DETECTOR
                    .score(&letters)
                    .map_or(f64::NEG_INFINITY, f64::from)
            });
            let best = if is_cjk(*charset) {
                &mut best_cjk
            } else {
                &mut best_other
            };
            *best = best.max(diff);
        }
        if best_other > best_cjk + FAMILY_DIFF_MARGIN {
            if let Some(western) = best_in_family(&scores, false) {
                champion = western;
            }
        }
    }
    champion = latin_letter_gate(bytes, champion, &candidates);
    if let Some(anchor) = best_anchor(reports, &scores) {
        if anchor != champion && !has_confident_non_declarative(reports) {
            return Some(anchor);
        }
    }
    Some(champion)
}

fn unique_charsets(reports: &[Report]) -> Vec<Charset> {
    let mut unique = Vec::new();
    for candidate in reports.iter().flat_map(|report| &report.candidates) {
        if !unique.contains(&candidate.charset) {
            unique.push(candidate.charset);
        }
    }
    unique
}

/// The charsets worth decoding: every declaration and structural proof, and the statistical
/// candidates among the two most confident or confident enough.
fn band(reports: &[Report], unique: &[Charset]) -> Vec<Charset> {
    let mut anchors = Vec::new();
    let mut statistical = Vec::new();
    for candidate in reports.iter().flat_map(|report| &report.candidates) {
        match candidate.kind {
            Kind::Declarative | Kind::Structural => anchors.push(candidate.charset),
            Kind::Statistical => statistical.push(*candidate),
        }
    }
    statistical.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let kept: Vec<Charset> = statistical
        .iter()
        .enumerate()
        .filter(|&(i, candidate)| {
            i < ALWAYS_KEEP_TOP || candidate.confidence >= MIN_TAIL_CONFIDENCE
        })
        .map(|(_, candidate)| candidate.charset)
        .collect();
    unique
        .iter()
        .copied()
        .filter(|charset| anchors.contains(charset) || kept.contains(charset))
        .collect()
}

fn declared_with_equivalent_decode(
    reports: &[Report],
    candidates: &[(Charset, String)],
) -> Option<Charset> {
    reports
        .iter()
        .flat_map(|report| &report.candidates)
        .filter(|candidate| candidate.kind == Kind::Declarative)
        .find_map(|declared| {
            let (_, text) = candidates
                .iter()
                .find(|(charset, _)| *charset == declared.charset)?;
            candidates
                .iter()
                .any(|(charset, other)| *charset != declared.charset && other == text)
                .then_some(declared.charset)
        })
}

fn conflicting_structural(
    reports: &[Report],
    candidates: &[(Charset, String)],
    declared: Charset,
) -> Option<Charset> {
    reports
        .iter()
        .flat_map(|report| &report.candidates)
        .filter(|candidate| candidate.kind == Kind::Structural)
        .map(|candidate| candidate.charset)
        .find(|&charset| {
            charset != declared
                && candidates
                    .iter()
                    .any(|(candidate, _)| *candidate == charset)
        })
}

/// Whether a structural proof outranks a declaration: always, but UTF-8 against an East Asian
/// declaration only with a three- or four-byte sequence to show for it.
fn overrides_declared(structural: Charset, declared: Charset, bytes: &[u8]) -> bool {
    if structural.name() == "UTF-8" && is_cjk(declared) {
        has_wide_utf8_sequence(bytes)
    } else {
        true
    }
}

fn is_cjk(charset: Charset) -> bool {
    let name = charset.name().to_lowercase();
    ["gb", "big5", "euc", "shift", "jis", "2022", "949"]
        .iter()
        .any(|part| name.contains(part))
}

fn best_in_family(scores: &[(Charset, f64)], cjk: bool) -> Option<Charset> {
    let mut best = None;
    let mut best_score = f64::NEG_INFINITY;
    for &(charset, score) in scores {
        if is_cjk(charset) == cjk && score > best_score {
            best_score = score;
            best = Some(charset);
        }
    }
    best
}

fn latin_letter_gate(bytes: &[u8], champion: Charset, candidates: &[(Charset, String)]) -> Charset {
    let name = champion.name();
    if name == WINDOWS_1252 || !LATIN_SBCS.contains(&name) {
        return champion;
    }
    let Some(windows) = candidates
        .iter()
        .map(|(charset, _)| *charset)
        .find(|charset| charset.name() == WINDOWS_1252)
    else {
        return champion;
    };
    let high = high_bytes(bytes);
    if high < LATIN_GATE_MIN_HIGH_BYTES {
        return champion;
    }
    let margin = LATIN_GATE_MARGIN_FLOOR.max(LATIN_GATE_MARGIN_FRACTION * high as f64);
    if cased_high_letters(bytes, windows) as f64
        > cased_high_letters(bytes, champion) as f64 + margin
    {
        windows
    } else {
        champion
    }
}

fn best_anchor(reports: &[Report], scores: &[(Charset, f64)]) -> Option<Charset> {
    let mut best = None;
    let mut best_score = f64::NEG_INFINITY;
    for candidate in reports.iter().flat_map(|report| &report.candidates) {
        if candidate.kind == Kind::Statistical {
            continue;
        }
        let Some(&(_, score)) = scores
            .iter()
            .find(|(charset, _)| *charset == candidate.charset)
        else {
            continue;
        };
        if score > best_score {
            best_score = score;
            best = Some(candidate.charset);
        }
    }
    best
}

fn has_confident_non_declarative(reports: &[Report]) -> bool {
    reports
        .iter()
        .flat_map(|report| &report.candidates)
        .any(|candidate| match candidate.kind {
            Kind::Structural => true,
            Kind::Statistical => candidate.confidence > NO_INFO_CONFIDENCE,
            Kind::Declarative => false,
        })
}

/// The length of a byte order mark the probe starts with.
fn bom_length(bytes: &[u8]) -> usize {
    match bytes {
        [0x00, 0x00, 0xFE, 0xFF, ..] | [0xFF, 0xFE, 0x00, 0x00, ..] => 4,
        [0xEF, 0xBB, 0xBF, ..] => 3,
        [0xFE, 0xFF, ..] | [0xFF, 0xFE, ..] => 2,
        _ => 0,
    }
}

/// A decoding as the junk model reads it (`HtmlContentCleaner`): tags stripped when there are
/// any, then character references and a few named entities expanded.
fn clean(text: &str) -> String {
    let (stripped, found) = strip_tags(text.as_bytes());
    let tagless = if found.tags > 0 && found.length > 0 {
        String::from_utf8_lossy(&stripped).into_owned()
    } else {
        text.to_string()
    };
    let decimal = replace_references(&tagless, false, 7);
    let hexadecimal = replace_references(&decimal, true, 6);
    replace_named_entities(&hexadecimal)
}

/// Replaces each character reference `&#` (with `x` or `X` when `hex`), 1 to `max_digits`
/// digits and `;` by its character, in one left-to-right pass as Java's `Matcher.replaceAll`
/// does, leaving references beyond U+10FFFF as they are.
fn replace_references(text: &str, hex: bool, max_digits: usize) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(found) = text[i..].find("&#") {
        let start = i + found;
        let mut digits_start = start + 2;
        if hex {
            if !matches!(bytes.get(digits_start), Some(b'x' | b'X')) {
                i = start + 1;
                continue;
            }
            digits_start += 1;
        }
        let digits = bytes[digits_start..]
            .iter()
            .take_while(|&&b| {
                if hex {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit()
                }
            })
            .count();
        if !(1..=max_digits).contains(&digits) || bytes.get(digits_start + digits) != Some(&b';') {
            i = start + 1;
            continue;
        }
        let end = digits_start + digits + 1;
        let radix = if hex { 16 } else { 10 };
        let value = u32::from_str_radix(&text[digits_start..digits_start + digits], radix).ok();
        if let Some(c) = value.filter(|&v| v <= 0x10_FFFF).map(java_char) {
            out.push_str(&text[copied..start]);
            out.push(c);
            copied = end;
        }
        i = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// The character Java's `Character.toChars` gives a code point, with a lone surrogate, which a
/// Rust string cannot hold, as U+FFFD.
fn java_char(cp: u32) -> char {
    char::from_u32(cp).unwrap_or('\u{FFFD}')
}

fn replace_named_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let named = ["amp", "lt", "gt", "quot", "apos", "nbsp", "copy", "reg"]
            .iter()
            .find(|name| after.starts_with(*name) && after[name.len()..].starts_with(';'));
        match named {
            Some(&name) => {
                out.push_str(match name {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    "nbsp" => " ",
                    "copy" => "\u{A9}",
                    _ => "\u{AE}",
                });
                rest = &after[name.len() + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}
