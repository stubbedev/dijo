//! JSoupParser against Tika's own answers: `tests/fixtures/html/html-reference.tsv.gz`, written by
//! `just html-reference`, whose added column is the digest of the tree jsoup builds. Where the
//! parser builds the same tree, it writes what Tika writes; where jsoup departs from the WHATWG
//! algorithm (#84), the trees differ, the metadata alone is compared, and [`super::trees`] checks
//! the parser's trees against html5lib's.

use super::trees::tree;
use super::{read, JSoupParser};
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::reference::{cases, difference, digest, Compared};
use crate::formats::Parser;

const REFERENCE: &[u8] = include_bytes!("../../../tests/fixtures/html/html-reference.tsv.gz");

/// The inputs the parser leaves to Tika: a data URI image and an iframe's `srcdoc`, which Tika
/// parses as embedded documents.
const DEFERRED: [&str; 2] = ["synthetic/data-uri.html", "synthetic/srcdoc.html"];

/// The inputs whose metadata the parser gives differently from Tika on purpose: a `meta` title
/// after the `title` element, which Tika records as `html:dc:title`, is a further `dc:title`, as
/// is one before it, which Tika drops (#86). `handler`'s tests check what the port gives.
const DEPARTURES: [&str; 2] = [
    "synthetic/metadata.html",
    "tika/tika-parsers-standard/tika-parsers-standard-modules/tika-parser-html-module/src/test/resources/test-documents/testHTML_metadata_two_titles.html",
];

/// How many inputs the parser builds another tree from than jsoup, by the WHATWG algorithm (#84):
/// 195 of html5lib's documents, and nearly every other input, whose white space after `</body>`
/// is the body's. Their metadata, read from the head, is still Tika's; the corpus tests compare
/// the text of the documents among them with what the algorithm changes in it left out.
const WHATWG_TREES: usize = 233;

#[test]
fn parses_as_tika_does() {
    let cases = cases(REFERENCE);
    let mut mismatches = Vec::new();
    let mut deferred = Vec::new();
    let mut departed = Vec::new();
    let mut whatwg = 0;
    for case in &cases {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", case.content_type.clone());
        if JSoupParser
            .defers(&case.input, &metadata, &ParserConfigs::default())
            .is_some()
        {
            deferred.push(case.label.as_str());
            continue;
        }
        let document = read(&case.input, Some(&case.content_type))
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.label));
        let compared = if digest(&tree(&document.dom)) == case.extra[0] {
            Compared::Everything
        } else {
            whatwg += 1;
            Compared::Metadata
        };
        let difference = difference(&JSoupParser, case, compared);
        if DEPARTURES.contains(&case.label.as_str()) {
            departed.push(case.label.as_str());
            if difference.is_none() {
                mismatches.push(format!("{}: no longer departs", case.label));
            }
        } else if let Some(difference) = difference {
            mismatches.push(difference);
        }
    }
    assert_eq!(
        whatwg, WHATWG_TREES,
        "inputs whose tree departs from jsoup's"
    );
    departed.sort_unstable();
    assert_eq!(departed, DEPARTURES, "departures not in the reference");
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
