//! JSoupParser against Tika's own answers: `tests/fixtures/html/html-reference.tsv.gz`, written by
//! `just html-reference`, whose added column is the digest of the tree jsoup builds.

use super::trees::tree;
use super::{read, JSoupParser};
use crate::formats::metadata::TikaMetadata;
use crate::formats::reference::{cases, difference, digest};
use crate::formats::Parser;

const REFERENCE: &[u8] = include_bytes!("../../../tests/fixtures/html/html-reference.tsv.gz");

/// The inputs the parser leaves to Tika: a data URI image and an iframe's `srcdoc`, which Tika
/// parses as embedded documents.
const DEFERRED: [&str; 2] = ["synthetic/data-uri.html", "synthetic/srcdoc.html"];

#[test]
fn parses_as_tika_does() {
    let cases = cases(REFERENCE);
    let mut mismatches = Vec::new();
    let mut deferred = Vec::new();
    for case in &cases {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", case.content_type.clone());
        if JSoupParser.defers(&case.input, &metadata).is_some() {
            deferred.push(case.label.as_str());
            continue;
        }
        let document = read(&case.input, Some(&case.content_type))
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.label));
        let tree = digest(&tree(&document.dom));
        if tree != case.extra[0] {
            mismatches.push(format!(
                "{}: tree {}, port {tree}",
                case.label, case.extra[0]
            ));
            continue;
        }
        if let Some(difference) = difference(&JSoupParser, case) {
            mismatches.push(difference);
        }
    }
    let tika_deferred: Vec<&str> = deferred
        .iter()
        .copied()
        .filter(|label| label.starts_with("tika/"))
        .collect();
    let others: Vec<&str> = deferred
        .iter()
        .copied()
        .filter(|label| !label.starts_with("tika/"))
        .collect();
    assert!(
        cases.len() > 1600,
        "the reference has {} inputs",
        cases.len()
    );
    assert_eq!(others, DEFERRED, "deferred inputs");
    assert_eq!(tika_deferred.len(), 4, "deferred: {tika_deferred:?}");
    assert!(
        mismatches.is_empty(),
        "{} of {} inputs differ:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}
