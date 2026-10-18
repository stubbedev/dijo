#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_read_where_tika_reads_nothing, conformance_corpus, documents, WITHOUT_SCIENTIFIC_MODULE,
};

const TIKAS_DOCUMENTS: &str =
    "tika-parsers-extended/tika-parser-scientific-module/src/test/resources/test-documents";

fn extract(name: &str) -> (String, crate::Metadata) {
    let path = crate::formats::parity::conformance_corpus()
        .join(TIKAS_DOCUMENTS)
        .join(name);
    crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap()
}

/// Tika's HDF4 test file, a sea surface temperature map, has its global attributes as metadata
/// and its two variables listed with their dimensions and attributes.
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_tikas_hdf4_file() {
    let (text, metadata) = extract("test.hdf");
    assert_eq!(metadata["hdf:Pass"], ["Ascending"]);
    assert_eq!(metadata["hdf:Start year"], ["1987"]);
    assert_eq!(
        metadata[FILE_TYPE_DESCRIPTION],
        [Version::Four.description()]
    );
    assert!(text.contains("Longitude = 720"), "{text}");
    assert!(
        text.contains("byte Sea Surface Temperature(Longitude=720, Latitude=360)"),
        "{text}"
    );
    assert!(text.contains("Unit = \"Degree in Celsius\""), "{text}");
    assert!(text.contains("scale_factor = 0.150000005"), "{text}");
}

/// Tika's HDF-EOS 5 test file has its groups' attributes as metadata and its datasets listed
/// by their paths, with their attributes.
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_tikas_hdf5_file() {
    let (text, metadata) = extract("test.he5");
    assert!(metadata.contains_key("hdf:HDFEOSVersion"), "{metadata:?}");
    assert_eq!(
        metadata[FILE_TYPE_DESCRIPTION],
        [Version::Five.description()]
    );
    assert!(
        text.contains("HDFEOS/SWATHS/O3 column-MLS/Data Fields/L2gpValue"),
        "{text}"
    );
    assert!(text.contains("UniqueFieldDefinition = "), "{text}");
}

#[test]
fn fails_on_a_file_without_a_superblock() {
    let mut bytes = hdf5::SIGNATURE.to_vec();
    bytes.extend_from_slice(&[9; 40]);
    let configs = ParserConfigs::default();
    let prepared = HdfParser.prepare(&bytes, &TikaMetadata::new(), &configs);
    assert!(matches!(prepared, Err(Decline::Fails(_))));
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_tikas_test_files_which_tikas_path_leaves_empty() {
    let (parsed, deferred) = documents(&HdfParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 2, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_read_where_tika_reads_nothing(&paths, &[WITHOUT_SCIENTIFIC_MODULE]);
}
