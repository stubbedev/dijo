use extractous_core::{Extractor, Isolate};
use std::io::Read;

const DOCUMENT: &str = "../test_files/documents/category-level.docx";

#[test]
fn test_extractors_in_separate_isolates_agree() {
    let default = Extractor::new().extract_file_to_string(DOCUMENT).unwrap().0;
    let separate = Extractor::new()
        .set_isolate(Isolate::new().unwrap())
        .extract_file_to_string(DOCUMENT)
        .unwrap()
        .0;
    assert_eq!(separate, default);
}

#[test]
fn test_isolates_can_be_created_and_dropped_repeatedly() {
    for document in [
        DOCUMENT,
        "../test_files/documents/science-exploration-1p.pptx",
    ] {
        for _ in 0..3 {
            let (mut stream, _) = Extractor::new()
                .set_isolate(Isolate::new().unwrap())
                .extract_file(document)
                .unwrap();
            let mut text = String::new();
            stream.read_to_string(&mut text).unwrap();
            assert!(!text.trim().is_empty());
        }
    }
}

#[test]
fn test_a_thread_can_stay_attached_to_its_isolate() {
    let isolate = Isolate::new().unwrap();
    std::thread::spawn(move || {
        isolate.keep_current_thread_attached().unwrap();
        let extractor = Extractor::new().set_isolate(isolate);
        for _ in 0..3 {
            assert!(extractor
                .extract_file_to_string(DOCUMENT)
                .unwrap()
                .0
                .contains("Call me Ishmael"));
        }
    })
    .join()
    .unwrap();
}
