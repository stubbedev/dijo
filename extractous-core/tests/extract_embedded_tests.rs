use extractous::{EmbeddedExtractResult, Extractor};
use std::fs;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

fn assert_single_png(result: &EmbeddedExtractResult) {
    assert_eq!(result.documents.len(), 1);
    let image = &result.documents[0];
    assert_eq!(image.resource_name, "image1.png");
    assert_eq!(image.content_type, "image/png");
    assert!(image.is_image());
    assert!(image.content.starts_with(PNG_SIGNATURE));
}

#[test]
fn test_extract_embedded_from_file() {
    let result = Extractor::new()
        .extract_embedded_from_file("../test_files/documents/vodafone.xlsx")
        .unwrap();
    assert_single_png(&result);
}

#[test]
fn test_extract_embedded_from_bytes() {
    let bytes = fs::read("../test_files/documents/vodafone.xlsx").unwrap();
    let result = Extractor::new()
        .extract_embedded_from_bytes(&bytes)
        .unwrap();
    assert_single_png(&result);
}

#[test]
fn test_extract_embedded_from_file_without_embedded_documents() {
    let result = Extractor::new()
        .extract_embedded_from_file("../test_files/documents/simple.odt")
        .unwrap();
    assert!(result.documents.is_empty());
}
