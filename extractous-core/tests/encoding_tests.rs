use extractous_core::Extractor;
use test_case::test_case;

#[test_case("mostly-ascii-utf8.txt"; "one ligature in ascii text")]
#[test_case("unnormalized.txt"; "a few non-ascii characters")]
fn utf8_text_with_few_non_ascii_characters_is_decoded_as_utf8(file_name: &str) {
    let path = format!("../test_files/documents/{file_name}");
    let expected = std::fs::read_to_string(&path).unwrap();
    let extractor = Extractor::new();

    let (text, metadata) = extractor.extract_file_to_string(&path).unwrap();
    assert_eq!(text.trim_end(), expected.trim_end());
    assert_eq!(
        metadata.get("Content-Encoding").unwrap(),
        &vec!["UTF-8".to_string()]
    );

    let bytes = std::fs::read(&path).unwrap();
    let (text, _) = extractor.extract_bytes_to_string(&bytes).unwrap();
    assert_eq!(text.trim_end(), expected.trim_end());
}
