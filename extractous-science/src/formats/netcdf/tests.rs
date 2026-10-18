#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
use crate::formats::bytes::Unread;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_read_where_tika_reads_nothing, conformance_corpus, documents, WITHOUT_SCIENTIFIC_MODULE,
};

fn push_name(bytes: &mut Vec<u8>, name: &str) {
    bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
    bytes.extend_from_slice(name.as_bytes());
    bytes.resize(bytes.len().next_multiple_of(4), 0);
}

fn push_text_attribute(bytes: &mut Vec<u8>, name: &str, value: &str) {
    push_name(bytes, name);
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    bytes.resize(bytes.len().next_multiple_of(4), 0);
}

/// A classic file of `records` records, or of a count left unknown, with a record dimension
/// `time` and a dimension `lat` of 3, a title, conventions and a version, and a variable `tas`
/// over both in kelvin, whose data follows the header.
fn classic(records: Option<u32>) -> Vec<u8> {
    let mut bytes = b"CDF\x01".to_vec();
    bytes.extend_from_slice(&records.unwrap_or(u32::MAX).to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0x0A, 0, 0, 0, 2]);
    push_name(&mut bytes, "time");
    bytes.extend_from_slice(&0u32.to_be_bytes());
    push_name(&mut bytes, "lat");
    bytes.extend_from_slice(&3u32.to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0x0C, 0, 0, 0, 3]);
    push_text_attribute(&mut bytes, "title", "Bølger i Øresund");
    push_text_attribute(&mut bytes, "Conventions", "CF-1.0\0\0");
    push_name(&mut bytes, "version");
    bytes.extend_from_slice(&6u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&1.5f64.to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0x0B, 0, 0, 0, 1]);
    push_name(&mut bytes, "tas");
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0x0C, 0, 0, 0, 2]);
    push_text_attribute(&mut bytes, "units", "K");
    push_name(&mut bytes, "valid_range");
    bytes.extend_from_slice(&5u32.to_be_bytes());
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&150f32.to_be_bytes());
    bytes.extend_from_slice(&350.5f32.to_be_bytes());
    bytes.extend_from_slice(&5u32.to_be_bytes());
    bytes.extend_from_slice(&12u32.to_be_bytes());
    let begin = bytes.len() as u32 + 4;
    bytes.extend_from_slice(&begin.to_be_bytes());
    bytes.resize(bytes.len() + 2 * 12, 0);
    bytes
}

#[test]
fn reads_the_header_of_a_classic_file() {
    let (text, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&classic(Some(2)))
        .unwrap();
    assert!(text.contains("dimensions"), "{text}");
    assert!(text.contains("time = 2"), "{text}");
    assert!(text.contains("lat = 3"), "{text}");
    assert!(text.contains("float tas(time=2, lat=3)"), "{text}");
    assert!(text.contains("units = \"K\""), "{text}");
    assert!(text.contains("valid_range = 150.0f, 350.5f"), "{text}");
    assert_eq!(metadata["dc:title"], ["Bølger i Øresund"]);
    assert_eq!(metadata["cf:Conventions"], ["CF-1.0"]);
    assert_eq!(metadata["netcdf:version"], ["1.5"]);
    assert_eq!(metadata[FILE_TYPE_DESCRIPTION], [DESCRIPTION]);
    assert_eq!(
        metadata["tk:parsed-by"].last().map(String::as_str),
        Some("extractous::NetCDFParser")
    );
}

#[test]
fn counts_the_records_of_a_file_being_written() {
    let header = Header::read(Bytes::new(&classic(None))).unwrap();
    assert_eq!(header.dimensions[0].length, 2);
    assert_eq!(
        header.variables[0].declaration(),
        "float tas(time=2, lat=3)"
    );
}

#[test]
fn fails_on_a_header_cut_short() {
    let whole = classic(Some(2));
    let Err(Unread::Malformed(reason)) = Header::read(Bytes::new(&whole[..60])) else {
        panic!("a cut header is read");
    };
    assert!(reason.contains("the NetCDF header"), "{reason}");
}

/// Tika's test file, a climate model's output, lists its dimensions and variables.
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_tikas_test_file() {
    let path = crate::formats::parity::conformance_corpus().join(
        "tika-parsers-extended/tika-parser-scientific-module/src/test/resources/\
         test-documents/sresa1b_ncar_ccsm3_0_run1_200001.nc",
    );
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    assert!(text.contains("lat = 128"), "{text}");
    assert!(
        text.contains("float tas(time=1, lat=128, lon=256)"),
        "{text}"
    );
    assert_eq!(metadata["dc:title"], ["model output prepared for IPCC AR4"]);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_tikas_test_files_which_tikas_path_leaves_empty() {
    let (parsed, deferred) = documents(&NetCdfParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_read_where_tika_reads_nothing(&paths, WITHOUT_SCIENTIFIC_MODULE);
}
