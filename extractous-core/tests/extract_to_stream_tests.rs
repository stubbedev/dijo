use extractous_core::{CharSet, Extractor, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig};
use std::fs;
use std::io::Read;
use test_case::test_case;
use textdistance::nstr::cosine;

// Declarers the shared test_utils code as module in this integration test
mod test_utils;

#[test_case("2022_Q3_AAPL.pdf", 0.9; "Test PDF file")]
#[test_case("science-exploration-1p.pptx", 0.9; "Test PPTX file")]
#[test_case("simple.odt", 0.8; "Test ODT file")]
#[test_case("table-multi-row-column-cells-actual.csv", 0.8; "Test CSV file")]
#[test_case("vodafone.xlsx", 0.4; "Test XLSX file")]
#[test_case("category-level.docx", 0.9; "Test DOCX file")]
#[test_case("simple.doc", 0.9; "Test DOC file")]
#[test_case("simple.pptx", 0.9; "Test another PPTX file")]
#[test_case("table-multi-row-column-cells.png", -1.0; "Test PNG file")]
#[test_case("winter-sports.epub", 0.9; "Test EPUB file")]
#[test_case("bug_16.docx", 0.9; "Test bug16 DOCX file")]
//#[test_case("eng-ocr.pdf", 0.9; "Test eng-ocr PDF file")]
fn test_extract_bytes_to_stream(file_name: &str, target_dist: f64) {
    let extractor = Extractor::new();

    let bytes = fs::read(format!("../test_files/documents/{}", file_name)).unwrap();
    let (mut stream, _) = extractor.extract_bytes(&bytes).unwrap();

    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).unwrap();
    let extracted = String::from_utf8_lossy(&buffer);
    // The metadata returned with the stream is what is known before the parse (#67); the
    // parse's own comes once the stream is read.
    let metadata = stream.metadata().unwrap();

    // read expected string
    let expected =
        fs::read_to_string(format!("../test_files/expected_result/{}.txt", file_name)).unwrap();

    let dist = cosine(expected.trim(), extracted.trim());
    assert!(
        dist > target_dist,
        "Cosine similarity is less than {} for file: {}, dist: {}",
        target_dist,
        file_name,
        dist
    );
    println!("{}: {}", file_name, dist);

    // Metadata checking
    let expected_metadata = test_utils::parse_metadata_file(&format!(
        "../test_files/expected_result/{}.metadata.json",
        file_name
    ));
    let percent_similarity =
        test_utils::calculate_similarity_percent(&expected_metadata, &metadata);
    assert!(
        percent_similarity > target_dist,
        "The metadata similarity is lower than expected. Current {}% | filename: {}",
        percent_similarity,
        file_name
    );
}

#[test]
fn test_extract_bytes_to_stream_ara_ocr_png() {
    let extractor = Extractor::new()
        .set_ocr_config(TesseractOcrConfig::new().set_language("ara"))
        .set_pdf_config(PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::NO_OCR));

    // extract file with extractor
    let bytes = fs::read("../test_files/documents/ara-ocr.png").unwrap();
    let (mut stream, _metadata) = extractor.extract_bytes(&bytes).unwrap();

    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).unwrap();
    let extracted = String::from_utf8_lossy(&buffer);

    println!("{}", extracted);

    // read expected string
    let expected = fs::read_to_string("../test_files/expected_result/ara-ocr.png.txt").unwrap();

    let dist = cosine(&expected, &extracted);
    assert!(
        dist > 0.9,
        "Cosine similarity is less than 0.9 for file: ara-ocr.png, dist: {}",
        dist
    );
    println!("ara-ocr.png: {}", dist);
}

#[test]
fn test_extract_bytes_to_stream_does_not_read_the_caller_buffer_after_returning() {
    let text = "the quick brown fox jumps over the lazy dog\n".repeat(200_000);
    let mut bytes = text.clone().into_bytes();

    let (mut stream, _) = Extractor::new().extract_bytes(&bytes).unwrap();
    bytes.fill(0);
    drop(bytes);
    let mut extracted = String::new();
    stream.read_to_string(&mut extracted).unwrap();

    assert_eq!(extracted.trim(), text.trim());
}

#[test]
fn test_extract_file_to_stream_ocr_only_deu_ocr_pdf() {
    let extractor = Extractor::new()
        .set_ocr_config(TesseractOcrConfig::new().set_language("deu"))
        .set_pdf_config(PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::OCR_ONLY));

    let (mut stream, _) = extractor
        .extract_file("../test_files/documents/deu-ocr.pdf")
        .unwrap();
    let mut extracted = String::new();
    stream.read_to_string(&mut extracted).unwrap();

    let expected = fs::read_to_string("../test_files/expected_result/deu-ocr.pdf.txt").unwrap();
    let dist = cosine(&expected, &extracted);
    assert!(dist > 0.9, "Cosine similarity for deu-ocr.pdf is {dist}");
}

#[test]
fn test_extract_file_to_stream_honours_the_encoding() {
    let path = "../test_files/documents/category-level.docx";
    let mut utf8 = String::new();
    Extractor::new()
        .extract_file(path)
        .unwrap()
        .0
        .read_to_string(&mut utf8)
        .unwrap();

    let mut utf16 = Vec::new();
    Extractor::new()
        .set_encoding(CharSet::UTF_16BE)
        .extract_file(path)
        .unwrap()
        .0
        .read_to_end(&mut utf16)
        .unwrap();
    let (pairs, rest) = utf16.as_chunks::<2>();
    assert!(rest.is_empty(), "odd number of UTF-16 bytes");
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_be_bytes(*pair)).collect();

    assert_eq!(String::from_utf16(&units).unwrap(), utf8);
}

#[test]
fn test_many_unread_streams_do_not_block_each_other() {
    let text = "every stream keeps its own parse running\n".repeat(50_000);
    let extractor = Extractor::new();
    let mut streams: Vec<_> = (0..16)
        .map(|_| extractor.extract_bytes(text.as_bytes()).unwrap().0)
        .collect();

    while let Some(mut stream) = streams.pop() {
        let mut extracted = String::new();
        stream.read_to_string(&mut extracted).unwrap();
        assert_eq!(extracted.trim(), text.trim());
    }
}

#[test]
fn test_streams_from_many_threads() {
    let path = "../test_files/documents/category-level.docx";
    let mut expected = String::new();
    Extractor::new()
        .extract_file(path)
        .unwrap()
        .0
        .read_to_string(&mut expected)
        .unwrap();

    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let extractor = Extractor::new();
                for _ in 0..10 {
                    let mut extracted = String::new();
                    extractor
                        .extract_file(path)
                        .unwrap()
                        .0
                        .read_to_string(&mut extracted)
                        .unwrap();
                    assert_eq!(extracted, expected);
                }
            });
        }
    });
}

#[test]
fn test_stream_metadata_is_final_after_reading_to_the_end() {
    let (mut stream, initial) = Extractor::new()
        .extract_file("../test_files/documents/2022_Q3_AAPL.pdf")
        .unwrap();
    assert!(matches!(
        stream.metadata(),
        Err(extractous_core::Error::StreamNotFinished)
    ));
    stream.read_to_end(&mut Vec::new()).unwrap();

    let metadata = stream.metadata().unwrap();
    assert_eq!(metadata.get("xmpTPg:NPages"), Some(&vec!["28".to_string()]));
    assert!(metadata.len() > initial.len());
}

#[test]
fn test_skip_ocr_extracts_no_text_from_an_image() {
    let (content, _) = Extractor::new()
        .set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true))
        .extract_file_to_string("../test_files/documents/ara-ocr.png")
        .unwrap();
    assert!(content.trim().is_empty(), "{content}");
}

#[test]
fn test_a_timeout_ends_a_slow_stream() {
    let started = std::time::Instant::now();
    let (mut stream, _) = Extractor::new()
        .set_ocr_config(TesseractOcrConfig::new().set_language("ara"))
        .set_timeout(std::time::Duration::from_millis(300))
        .extract_file("../test_files/documents/ara-ocr.png")
        .unwrap();
    let error = stream.read_to_end(&mut Vec::new()).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let inner = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<extractous_core::Error>());
    assert!(
        matches!(inner, Some(extractous_core::Error::Timeout)),
        "{error:?}"
    );
}
