use extractous::{EmbeddedExtractResult, Extractor};
use std::fs;

mod test_utils;

const XLSX: &str = "../test_files/documents/vodafone.xlsx";
const XLSX_CONTENT_TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

fn assert_single_png(result: &EmbeddedExtractResult) {
    assert_eq!(result.documents.len(), 1);
    let image = &result.documents[0];
    assert_eq!(image.resource_name, "image1.png");
    assert_eq!(image.content_type, "image/png");
    assert!(image.is_image());
    assert!(image.content.starts_with(PNG_SIGNATURE));
    assert_eq!(
        result.metadata.get("Content-Type"),
        Some(&vec![XLSX_CONTENT_TYPE.to_string()])
    );
}

#[test]
fn test_extract_embedded_from_file() {
    let result = Extractor::new().extract_embedded_from_file(XLSX).unwrap();
    assert_single_png(&result);
}

#[test]
fn test_extract_embedded_from_bytes() {
    let bytes = fs::read(XLSX).unwrap();
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

#[test]
fn test_extract_embedded_from_url() {
    let address = test_utils::serve(fs::read(XLSX).unwrap(), XLSX_CONTENT_TYPE);
    let result = Extractor::new()
        .extract_embedded_from_url(&format!("http://{address}/vodafone.xlsx"))
        .unwrap();
    assert_single_png(&result);
}
