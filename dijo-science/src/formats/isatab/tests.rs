use super::*;
use crate::formats::testing::Scratch;

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

/// The metadata of the study file `study` of the investigation `investigation`, read from a
/// directory of their own.
fn study_metadata(scratch: &str, investigation: &str, study: &str) -> crate::extracted::Metadata {
    let scratch = Scratch::new(scratch);
    scratch.write("i_investigation.txt", investigation.as_bytes());
    let study = scratch.write(study, STUDY_FILE.as_bytes());
    let (_, metadata) = crate::Extractor::new()
        .extract_file_to_string(study.to_str().unwrap())
        .unwrap();
    metadata
}

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

/// A study has only its own fields, none an earlier study of the investigation has (#183).
#[test]
fn gives_a_study_none_of_an_earlier_studys_fields() {
    let investigation = "INVESTIGATION\n\
        Investigation Identifier\t\"BII-I-1\"\n\
        STUDY\n\
        Study Identifier\t\"BII-S-1\"\n\
        Study Description\t\"Gær i kemostat\"\n\
        Study File Name\t\"s_first.txt\"\n\
        STUDY\n\
        Study Identifier\t\"BII-S-2\"\n\
        Study File Name\t\"s_second.txt\"\n";
    let metadata = study_metadata("isatab-own-fields", investigation, "s_second.txt");
    assert_eq!(metadata["isatab:Study Identifier"], ["BII-S-2"]);
    assert_eq!(metadata["isatab:Study File Name"], ["s_second.txt"]);
    assert!(
        !metadata.contains_key("isatab:Study Description"),
        "{metadata:?}"
    );
}

/// A study field with no value is read as empty, and the rest of the study as ever (#183).
#[test]
fn reads_a_study_field_with_no_value_as_empty() {
    let investigation = "INVESTIGATION\n\
        Investigation Identifier\t\"BII-I-1\"\n\
        STUDY\n\
        Study Identifier\t\"BII-S-1\"\n\
        Study Description\n\
        Study File Name\t\"s_study.txt\"\n";
    let metadata = study_metadata("isatab-empty-field", investigation, "s_study.txt");
    assert_eq!(metadata["isatab:Study Description"], [""]);
    assert_eq!(metadata["isatab:Study Identifier"], ["BII-S-1"]);
    assert_eq!(metadata["isatab:Investigation Identifier"], ["BII-I-1"]);
}

/// An assay too large to read whole is read a piece at a time, every row written.
#[test]
fn reads_an_assay_too_large_to_read_whole() {
    let row = |index: u64| format!("\"C-{index}-aliquot1\"\t\"JC_A{index}\"\n").into_bytes();
    let count = dijo_core::large::bodies_past_limit(row(0).len());
    let file = dijo_core::large::LargeFile::new(
        "a_large.txt",
        b"\"Sample Name\"\t\"Assay Name\"\n",
        count,
        row,
        b"",
    );
    let mut rows = 0;
    file.extract_lines(IsaTabParser.name(), |line| {
        let last_cell = line.rsplit('\t').next().unwrap_or_default();
        if last_cell.starts_with("JC_A") {
            assert_eq!(last_cell, format!("JC_A{rows}"));
            rows += 1;
        }
    });
    assert_eq!(rows, count);
}
