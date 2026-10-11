use extractous_core::Extractor;

mod test_utils;

const DOCUMENT: &str = "../test_files/documents/simple.odt";

#[test]
fn test_extract_http_url_to_string() {
    let extractor = Extractor::new();
    let (expected, _) = extractor.extract_file_to_string(DOCUMENT).unwrap();
    let address = test_utils::serve(
        std::fs::read(DOCUMENT).unwrap(),
        "application/vnd.oasis.opendocument.text",
    );

    let (content, _) = extractor
        .extract_url_to_string(&format!("http://{address}/simple.odt"))
        .unwrap();

    assert_eq!(content, expected);
}
