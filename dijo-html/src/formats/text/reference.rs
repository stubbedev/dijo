//! TextAndCSVParser against Tika's own answers: `tests/fixtures/text/text-reference.tsv.gz`,
//! written by `just text-reference`.

use super::TextAndCsvParser;
use crate::formats::reference::{cases, difference, Compared};

const REFERENCE: &[u8] = include_bytes!("../../../tests/fixtures/text/text-reference.tsv.gz");

/// The inputs whose text the port writes differently from Tika on purpose: a character outside
/// the Basic Multilingual Plane on a read boundary, which Tika splits into two U+FFFD (#66).
const DEPARTURES: [&str; 3] = [
    "synthetic/emoji-after-sniff.txt",
    "synthetic/emoji-at-4096.txt",
    "synthetic/emoji-at-8192.txt",
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
