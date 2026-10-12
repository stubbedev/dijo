use extractous_core::{Error, Extractor};
use std::io::Read;

const TRUNCATED_DOCX: &str = "../test_files/documents/truncated.docx";

#[test]
fn test_parse_error_carries_the_java_cause_chain() {
    let error = Extractor::new()
        .extract_file_to_string(TRUNCATED_DOCX)
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
    let message = match Extractor::new().extract_file(TRUNCATED_DOCX) {
        Err(error) => error.to_string(),
        Ok((mut reader, _)) => reader
            .read_to_string(&mut String::new())
            .unwrap_err()
            .to_string(),
    };

    assert!(message.contains("Truncated ZIP file"), "{message}");
    assert!(message.contains(" <- caused by "), "{message}");
}
