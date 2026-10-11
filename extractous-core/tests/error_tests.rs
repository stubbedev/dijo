use extractous_core::{Error, Extractor};
use std::io::Read;

fn truncated_docx() -> Vec<u8> {
    let mut docx = std::fs::read("../test_files/documents/category-level.docx").unwrap();
    docx.truncate(docx.len() / 2);
    docx
}

#[test]
fn test_parse_error_carries_the_java_cause_chain() {
    let error = Extractor::new()
        .extract_bytes_to_string(&truncated_docx())
        .unwrap_err();

    assert!(matches!(error, Error::ParseError(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .contains("caused by java.io.IOException: Truncated ZIP file"),
        "{error}"
    );
}

#[test]
fn test_stream_error_carries_the_java_cause_chain() {
    let message = match Extractor::new().extract_bytes(&truncated_docx()) {
        Err(error) => error.to_string(),
        Ok((mut reader, _)) => reader
            .read_to_string(&mut String::new())
            .unwrap_err()
            .to_string(),
    };

    assert!(message.contains("Truncated ZIP file"), "{message}");
    assert!(message.contains(" <- caused by "), "{message}");
}
