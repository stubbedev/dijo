#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

#[cfg(feature = "tika")]
use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};

/// Tika's `testSAS.sas7bdat`: a 32-bit, little-endian data set of two columns and eleven rows on
/// one page.
const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/sas/testSAS.sas7bdat");

fn extract(bytes: &[u8]) -> (String, crate::Metadata) {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(bytes)
        .unwrap()
}

#[test]
fn reads_data_sets_as_parso_does() {
    let (xml, metadata) = extract(FIXTURE);
    assert!(xml.contains("<h1>TESTING</h1>"), "{xml}");
    assert!(
        xml.contains("<th title=\"recnum\">recnum</th>\t<th title=\"label\">label</th>"),
        "{xml}"
    );
    assert!(
        xml.contains("<td>10</td>\t<td>This is row           10 of           10</td>"),
        "{xml}"
    );
    assert_eq!(xml.matches("<tr>").count(), 12, "{xml}");
    assert_eq!(metadata["database:column-name"], ["recnum", "label"]);
    assert_eq!(metadata["database:row-count"], ["11"]);
    assert_eq!(metadata["dcterms:created"], ["2017-01-30T07:31:47Z"]);
    assert_eq!(metadata["extended-properties:AppVersion"], ["9.0301M2"]);
    assert_eq!(metadata["machine:endian"], ["Little"]);
    assert_eq!(metadata["Content-Encoding"], ["windows-1252"]);
}

/// The fixture cut at several lengths, with a page length beyond parso's limit and a negative
/// one, which parso throws on, and with more rows than it holds.
#[cfg(feature = "tika")]
fn variants(scratch: &Scratch) -> Vec<PathBuf> {
    let mut written = Vec::new();
    for length in [300, 1024, 1500, 5000, 9000, 13_000, 17_000] {
        written.push(scratch.write(&format!("cut-{length}.sas7bdat"), &FIXTURE[..length]));
    }
    let mut page_length = FIXTURE.to_vec();
    page_length[200..204].copy_from_slice(&20_000_000i32.to_le_bytes());
    written.push(scratch.write("page-limit.sas7bdat", &page_length));
    page_length[200..204].copy_from_slice(&(-1i32).to_le_bytes());
    written.push(scratch.write("negative-page.sas7bdat", &page_length));
    let mut row_count = FIXTURE.to_vec();
    let pages = 1024;
    for at in (pages..row_count.len() - 4).step_by(4) {
        if row_count[at..at + 4] == 11i32.to_le_bytes() {
            row_count[at..at + 4].copy_from_slice(&14i32.to_le_bytes());
            break;
        }
    }
    written.push(scratch.write("row-count.sas7bdat", &row_count));
    written
}

#[cfg(feature = "tika")]
#[test]
fn reads_damaged_data_sets_as_tikas_path_does() {
    let scratch = Scratch::new("sas");
    let whole = scratch.write("whole.sas7bdat", FIXTURE);
    let damaged = variants(&scratch);
    let (parsed, deferred) = documents(&Sas7bdatParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    let (failing, read): (Vec<&Path>, Vec<&Path>) =
        parsed.iter().map(PathBuf::as_path).partition(|path| {
            crate::Extractor::new()
                .extract_file_to_string(path.to_str().unwrap())
                .is_err()
        });
    assert!(read.contains(&whole.as_path()), "{read:?}");
    assert!(!failing.is_empty(), "{read:?}");
    assert_eq!(read.len() + failing.len(), damaged.len() + 1);
    assert_same_as_tika(&read, Sas7bdatParser.tika_name());
    assert_fails_as_tika(&failing);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_tikas_test_data_sets_as_tikas_path_does() {
    let (parsed, deferred) = documents(&Sas7bdatParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 2, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, Sas7bdatParser.tika_name());
}
