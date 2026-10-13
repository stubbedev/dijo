//! The parsers of untrusted input and the properties each keeps whatever its input, for the
//! coverage-guided fuzz targets in `fuzz/`. Each function panics when its property fails.

use clap::ValueEnum;
use extractous_core::Metadata;

use crate::normalize::{normalize_in_chunks, Step, Steps};
use crate::schema;
use crate::server::query_pairs;
use crate::structure::{split, Unit};

/// Tika's XHTML, which a document shapes, split into pages and into elements: splitting may
/// fail on malformed markup but never panics, and yields at most a part per byte.
pub fn split_xhtml(xhtml: &[u8]) {
    for unit in [Unit::Pages, Unit::Elements] {
        let mut parts = 0;
        let emitted = split(xhtml, unit, &mut |_| {
            parts += 1;
            Ok(())
        });
        assert!(
            emitted.is_ok(),
            "only emitting a part can fail the outer result"
        );
        assert!(
            parts <= xhtml.len() + 1,
            "{parts} parts from {} bytes",
            xhtml.len()
        );
    }
}

/// Text normalised by the steps the bits of `skipped` do not leave out: handed over in chunks
/// of `chunk` bytes it comes out as it does whole, and with every step, normalising it again
/// changes nothing.
pub fn normalize(skipped: u16, chunk: u8, text: &[u8]) {
    let skipped: Vec<Step> = Step::value_variants()
        .iter()
        .enumerate()
        .filter(|(index, _)| skipped & (1 << index) != 0)
        .map(|(_, step)| *step)
        .collect();
    let steps = Steps::selected(false, &skipped);
    let whole = normalize_in_chunks(steps, text, text.len().max(1));
    assert_eq!(
        normalize_in_chunks(steps, text, usize::from(chunk).max(1)),
        whole,
        "chunks of {chunk}"
    );
    if steps == Steps::ALL {
        assert_eq!(
            normalize_in_chunks(steps, &whole, whole.len().max(1)),
            whole,
            "normalised twice"
        );
    }
}

/// A request's query string: decoding it never panics, and yields no more text than it holds.
pub fn decode_query(query: &str) {
    let decoded: usize = query_pairs(Some(query))
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum();
    assert!(
        decoded <= query.len(),
        "{decoded} bytes from {}",
        query.len()
    );
}

/// Metadata as a document states it, normalised into the fields of json output: never panics.
pub fn normalize_metadata(metadata: &Metadata) {
    schema::normalize(metadata);
}
