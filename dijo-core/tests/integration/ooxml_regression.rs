use dijo_core::Extractor;
use test_case::test_case;

#[test_case("zh-math-answers.docx"; "docx yobix-ai/extractous#52 chinese math")]
#[test_case("sorvino.docx"; "docx yobix-ai/extractous#52 sorvino")]
#[test_case("issue2-v2.docx"; "docx yobix-ai/extractous#45 issue2")]
#[test_case("issue6-visio.docx"; "docx yobix-ai/extractous#40 embedded visio")]
#[test_case("issue2.xlsx"; "xlsx yobix-ai/extractous#45 issue2")]
#[test_case("workplace-safety-data.xlsx"; "xlsx yobix-ai/extractous#60")]
#[test_case("issue5.xlsx"; "xlsx yobix-ai/extractous#45 comments")]
#[test_case("issue4.pptx"; "pptx yobix-ai/extractous#45 drawing")]
#[test_case("smartart.pptx"; "pptx yobix-ai/extractous#58 smartart")]
fn test_extract_ooxml_regression(file_name: &str) {
    let path = format!("../test_files/documents/ooxml/{file_name}");
    let (content, _) = Extractor::new().extract_file_to_string(&path).unwrap();
    assert!(
        !content.trim().is_empty(),
        "no text extracted from {file_name}"
    );
}
