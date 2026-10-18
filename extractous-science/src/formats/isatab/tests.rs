#[cfg(feature = "tika")]
use std::path::Path;

use super::*;
use crate::formats::parity::Scratch;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_read_where_tika_reads_nothing, conformance_corpus, documents, WITHOUT_SCIENTIFIC_MODULE,
};

const INVESTIGATION_FILE: &str = "ONTOLOGY SOURCE REFERENCE\n\
    Term Source Name\t\"OBI\"\t\"UO\"\t\n\
    INVESTIGATION\n\
    Investigation Identifier\t\"BII-I-1\"\n\
    Investigation Title\t\"Gærvækst i kemostat\"\n\
    STUDY\n\
    Study Identifier\t\"BII-S-1\"\n\
    Study File Name\t\"s_study.txt\"\n\
    STUDY ASSAYS\n\
    Study Assay File Name\t\"a_assay.txt\"\n\
    STUDY\n\
    Study Identifier\t\"BII-S-2\"\n\
    Study File Name\t\"s_other.txt\"\n";

const STUDY_FILE: &str = "\"Source Name\"\t\"Characteristics[organism]\"\t\"Sample Name\"\n\
    \"culture1\"\t\"Saccharomyces cerevisiae\"\t\"C-0.07-aliquot1\"\n";

const ASSAY_FILE: &str = "\"Sample Name\"\t\"Assay Name\"\n\"C-0.07-aliquot1\"\t\"JC_A1\"\n";

#[test]
fn reads_a_study_with_its_investigation_and_assays() {
    let scratch = Scratch::new("isatab");
    scratch.write("i_investigation.txt", INVESTIGATION_FILE.as_bytes());
    scratch.write("a_assay.txt", ASSAY_FILE.as_bytes());
    let study = scratch.write("s_study.txt", STUDY_FILE.as_bytes());
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(study.to_str().unwrap())
        .unwrap();
    assert!(text.contains("INVESTIGATION BII-I-1"), "{text}");
    assert!(text.contains("STUDY BII-S-1"), "{text}");
    assert!(text.contains("Saccharomyces cerevisiae"), "{text}");
    assert!(text.contains("ASSAY a_assay.txt"), "{text}");
    assert!(text.contains("JC_A1"), "{text}");
    assert_eq!(
        metadata["isatab:Investigation Title"],
        ["Gærvækst i kemostat"]
    );
    assert_eq!(metadata["isatab:Term Source Name"], ["OBI", "UO", ""]);
    assert_eq!(metadata["isatab:Study Identifier"], ["BII-S-1"]);
    assert_eq!(metadata["isatab:Study Assay File Name"], ["a_assay.txt"]);
    assert_eq!(
        metadata["tk:parsed-by"].last().map(String::as_str),
        Some(IsaTabParser.name())
    );
}

#[test]
fn reads_an_investigation_on_its_own() {
    let scratch = Scratch::new("isatab-investigation");
    let investigation = scratch.write("i_investigation.txt", INVESTIGATION_FILE.as_bytes());
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(investigation.to_str().unwrap())
        .unwrap();
    assert!(text.contains("Gærvækst i kemostat"), "{text}");
    assert!(text.contains("s_other.txt"), "{text}");
    assert_eq!(metadata["isatab:Investigation Identifier"], ["BII-I-1"]);
    assert!(
        !metadata.contains_key("isatab:Study Identifier"),
        "{metadata:?}"
    );
}

#[test]
fn fails_on_text_after_a_closing_quote() {
    let scratch = Scratch::new("isatab-malformed");
    let assay = scratch.write("a_broken.txt", b"\"Sample Name\"x\t\"Assay Name\"\n");
    assert!(crate::Extractor::new()
        .extract_file_to_string(assay.to_str().unwrap())
        .is_err());
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn reads_tikas_test_files_which_tikas_path_leaves_empty() {
    let (parsed, deferred) = documents(&IsaTabParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 8, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(std::path::PathBuf::as_path).collect();
    assert_read_where_tika_reads_nothing(&paths, WITHOUT_SCIENTIFIC_MODULE);
}
