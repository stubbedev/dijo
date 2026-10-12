use extractous_core::{EmbeddedExtractResult, Extractor};
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

fn spool(name: &str, byte_budget: u64) -> extractous_core::EmbeddedSpool {
    let dir = std::env::temp_dir().join(format!("extractous-spool-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    extractous_core::EmbeddedSpool { dir, byte_budget }
}

#[test]
fn test_embedded_documents_are_spooled_instead_of_inlined() {
    let spool = spool("zip", u64::MAX);
    let (mut stream, _) = Extractor::new()
        .extract_file_spooling_embedded("../test_files/documents/partially-damaged.zip", &spool)
        .unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut stream, &mut text).unwrap();
    assert!(
        !text.contains("This part of the archive extracts fine."),
        "{text}"
    );

    let embedded = stream.embedded().unwrap();
    let names: Vec<&str> = embedded
        .iter()
        .map(|document| document.name.as_str())
        .collect();
    assert_eq!(names, ["readable.txt", "truncated.docx"]);
    assert!(embedded.iter().all(|document| !document.truncated));
    assert_eq!(
        fs::read_to_string(&embedded[0].path).unwrap(),
        "This part of the archive extracts fine.\n"
    );
    assert_eq!(embedded[0].size, 40);
    fs::remove_dir_all(&spool.dir).unwrap();
}

#[test]
fn test_spooling_stops_at_the_byte_budget() {
    let spool = spool("budget", 10);
    let (mut stream, _) = Extractor::new()
        .extract_file_spooling_embedded("../test_files/documents/partially-damaged.zip", &spool)
        .unwrap();
    std::io::Read::read_to_end(&mut stream, &mut Vec::new()).unwrap();
    let embedded = stream.embedded().unwrap();
    assert_eq!(embedded[0].size, 10);
    assert!(embedded[0].truncated);
    assert_eq!(embedded[1].size, 0);
    assert!(embedded[1].truncated);
    fs::remove_dir_all(&spool.dir).unwrap();
}
