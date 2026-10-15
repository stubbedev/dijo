//! TextAndCSVParser against Tika's own answers: `tests/fixtures/text/text-reference.tsv.gz`,
//! written by `just text-reference`.

use super::TextAndCsvParser;
use crate::formats::reference::{cases, difference};

const REFERENCE: &[u8] = include_bytes!("../../../tests/fixtures/text/text-reference.tsv.gz");

#[test]
fn parses_as_tika_does() {
    let cases = cases(REFERENCE);
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|case| difference(&TextAndCsvParser, case))
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
