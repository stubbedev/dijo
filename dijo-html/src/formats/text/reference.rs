//! TextAndCSVParser against Tika's own answers: `tests/fixtures/text/text-reference.tsv.gz`,
//! written by `just text-reference`.

use super::TextAndCsvParser;
use crate::formats::reference::{cases, difference, Compared};

const REFERENCE: &[u8] = include_bytes!("../../../tests/fixtures/text/text-reference.tsv.gz");

/// The inputs whose text the port writes differently from Tika on purpose: a character outside
/// the Basic Multilingual Plane on a read boundary, which Tika splits into two U+FFFD (#66);
/// malformed UTF-8 and UTF-16, which the port replaces as WHATWG delimits malformed sequences and
/// Java's decoders otherwise, and a declared UTF-16 starting with two byte order marks, of which
/// Tika drops both and the port only the one that sets the byte order (#186).
const DEPARTURES: [&str; 8] = [
    "synthetic/emoji-after-sniff.txt",
    "synthetic/emoji-at-4096.txt",
    "synthetic/emoji-at-8192.txt",
    "synthetic/utf16le-lone-surrogate.txt",
    "synthetic/utf8-malformed-0.txt",
    "synthetic/utf8-malformed-1.txt",
    "synthetic/utf8-malformed-2.txt",
    "declared/utf16-double-bom-declared-utf16",
];

#[test]
fn parses_as_tika_does() {
    let cases = cases(REFERENCE);
    let departed = cases
        .iter()
        .filter(|case| DEPARTURES.contains(&case.label.as_str()))
        .count();
    assert_eq!(
        departed,
        DEPARTURES.len(),
        "departures not in the reference"
    );
    let mismatches: Vec<String> = cases
        .iter()
        .filter(|case| !DEPARTURES.contains(&case.label.as_str()))
        .filter_map(|case| difference(&TextAndCsvParser, case, Compared::Everything))
        .collect();
    assert!(
        cases.len() > 200,
        "the reference has {} inputs",
        cases.len()
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} inputs differ:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}
