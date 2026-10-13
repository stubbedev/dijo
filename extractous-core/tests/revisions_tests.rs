use extractous_core::{Extractor, OfficeParserConfig};
use test_case::test_case;

#[test_case("odt"; "OpenDocument")]
#[test_case("docx"; "Word")]
#[test_case("doc"; "Word 97")]
fn tracked_deletions_are_text_only_on_request(extension: &str) {
    let path = format!("../quality/corpus/revisions.{extension}");
    let (current, _) = Extractor::new().extract_file_to_string(&path).unwrap();
    assert!(current.contains("eleven leather volumes"), "{current}");
    assert!(!current.contains("thin"), "{current}");

    let (with_deletions, _) = Extractor::new()
        .set_office_config(OfficeParserConfig::new().set_include_deleted_content(true))
        .extract_file_to_string(&path)
        .unwrap();
    assert!(with_deletions.contains("thin"), "{with_deletions}");
}
