use dijo_core::Extractor;
use test_case::test_case;

#[test_case("2022_Q3_AAPL.pdf", "application/pdf")]
#[test_case(
    "category-level.docx",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
)]
#[test_case(
    "vodafone.xlsx",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
)]
#[test_case(
    "simple.pptx",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation"
)]
#[test_case("simple.doc", "application/msword")]
#[test_case("simple.odt", "application/vnd.oasis.opendocument.text")]
#[test_case("winter-sports.epub", "application/epub+zip")]
#[test_case("ara-ocr.png", "image/png")]
fn test_detect_file_and_bytes(file_name: &str, expected: &str) {
    let path = format!("../test_files/documents/{file_name}");
    let extractor = Extractor::new();

    assert_eq!(extractor.detect_file(&path).unwrap().as_str(), expected);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(extractor.detect_bytes(&bytes).unwrap().as_str(), expected);
}

#[test]
fn test_detect_ignores_a_misleading_extension() {
    let scratch = std::env::temp_dir().join(format!("dijo-detect-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let disguised = scratch.join("report.txt");
    std::fs::copy("../test_files/documents/2022_Q3_AAPL.pdf", &disguised).unwrap();

    let detected = Extractor::new()
        .detect_file(disguised.to_str().unwrap())
        .unwrap();
    std::fs::remove_dir_all(&scratch).unwrap();

    assert_eq!(detected.as_str(), "application/pdf");
}

#[test]
fn test_detect_missing_file_is_an_io_error() {
    let error = Extractor::new()
        .detect_file("../test_files/documents/does-not-exist.pdf")
        .unwrap_err();
    assert!(matches!(error, dijo_core::Error::IoError(_)), "{error:?}");
}
