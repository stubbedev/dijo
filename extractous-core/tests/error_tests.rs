use extractous_core::{Error, Extractor};
use std::io::Read;

/// A MATLAB file of its header alone, which only Tika's path reads, and on which Tika's
/// `MatParser` throws.
fn header_only_matlab() -> Vec<u8> {
    [b"MATLAB 5.0 MAT-file".as_slice(), &[b' '; 109]].concat()
}

#[test]
fn test_parse_error_carries_the_java_cause_chain() {
    let error = Extractor::new()
        .extract_bytes_to_string(&header_only_matlab())
        .unwrap_err();

    assert!(matches!(error, Error::ParseError(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .contains(" <- caused by java.lang.ArrayIndexOutOfBoundsException"),
        "{error}"
    );
}

#[test]
fn test_stream_error_carries_the_java_cause_chain() {
    let message = match Extractor::new().extract_bytes(&header_only_matlab()) {
        Err(error) => error.to_string(),
        Ok((mut reader, _)) => reader
            .read_to_string(&mut String::new())
            .unwrap_err()
            .to_string(),
    };

    assert!(message.contains("TikaException"), "{message}");
    assert!(message.contains(" <- caused by "), "{message}");
}
