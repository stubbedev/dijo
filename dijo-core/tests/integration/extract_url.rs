use std::io::Read;
use std::time::{Duration, Instant};

use dijo_core::{Error, Extractor};

use crate::test_utils;

const DOCUMENT: &str = "../test_files/documents/simple.odt";
const DOCUMENT_TYPE: &str = "application/vnd.oasis.opendocument.text";

fn serve_document() -> String {
    let address = test_utils::serve(std::fs::read(DOCUMENT).unwrap(), DOCUMENT_TYPE);
    format!("http://{address}/simple.odt")
}

#[test]
fn test_extract_http_url_to_string() {
    let extractor = Extractor::new();
    let (expected, _) = extractor.extract_file_to_string(DOCUMENT).unwrap();

    let (content, _) = extractor.extract_url_to_string(&serve_document()).unwrap();

    assert_eq!(content, expected);
}

#[test]
fn test_extract_http_url_streams_as_the_file_does() {
    let extractor = Extractor::new();
    let (mut file, _) = extractor.extract_file(DOCUMENT).unwrap();
    let mut expected = String::new();
    file.read_to_string(&mut expected).unwrap();

    let (mut fetched, metadata) = extractor.extract_url(&serve_document()).unwrap();
    let mut content = String::new();
    fetched.read_to_string(&mut content).unwrap();

    assert_eq!(content, expected);
    assert_eq!(metadata["tk:resource-name"], ["simple.odt"]);
}

#[test]
fn test_detect_http_url() {
    let detected = Extractor::new().detect_url(&serve_document()).unwrap();

    assert_eq!(detected.as_str(), DOCUMENT_TYPE);
}

#[test]
fn test_extract_file_url_as_the_file() {
    let extractor = Extractor::new();
    let (expected, expected_metadata) = extractor.extract_file_to_string(DOCUMENT).unwrap();

    let (content, metadata) = extractor
        .extract_url_to_string(&test_utils::file_url(DOCUMENT))
        .unwrap();

    assert_eq!(content, expected);
    assert_eq!(
        metadata["tk:resource-name"],
        expected_metadata["tk:resource-name"]
    );
}

#[test]
fn test_a_missing_document_is_an_io_error() {
    let address = test_utils::answer("404 Not Found", b"gone".to_vec(), "text/plain");

    let error = Extractor::new()
        .extract_url_to_string(&format!("http://{address}/missing.pdf"))
        .unwrap_err();

    assert!(matches!(error, Error::IoError(_)), "{error:?}");
    assert!(error.to_string().contains("404"), "{error}");
}

#[test]
fn test_an_unreachable_server_is_an_io_error() {
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();

    let error = Extractor::new()
        .extract_url_to_string(&format!("http://{address}/a.pdf"))
        .unwrap_err();

    assert!(matches!(error, Error::IoError(_)), "{error:?}");
}

#[test]
fn test_a_server_that_does_not_answer_times_the_extraction_out() {
    let address = test_utils::silent();
    let started = Instant::now();

    let result = Extractor::new()
        .set_timeout(Duration::from_millis(500))
        .extract_url(&format!("http://{address}/a.pdf"));

    assert!(matches!(result, Err(Error::Timeout)), "{:?}", result.err());
    assert!(started.elapsed() < Duration::from_secs(10));
}
