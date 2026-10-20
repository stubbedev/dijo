use dijo_core::Extractor;

const DOCUMENTS: [&str; 4] = [
    "../test_files/documents/simple.odt",
    "../test_files/documents/simple.doc",
    "../test_files/documents/category-level.docx",
    "../test_files/documents/table-multi-row-column-cells-actual.csv",
];

#[test]
fn test_extract_many_files_in_one_process() {
    let extractor = Extractor::new();
    let expected: Vec<String> = DOCUMENTS
        .iter()
        .map(|path| extractor.extract_file_to_string(path).unwrap().0)
        .collect();

    for _ in 0..50 {
        for (path, expected) in DOCUMENTS.iter().zip(&expected) {
            let (content, _) = extractor.extract_file_to_string(path).unwrap();
            assert_eq!(&content, expected, "extraction of {path} changed");
        }
    }
}
