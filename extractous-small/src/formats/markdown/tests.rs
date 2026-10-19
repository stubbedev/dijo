use super::*;

#[test]
fn reads_titles_and_tables_as_commonmark_java_does() {
    assert_eq!(separator_columns("|:-|-:|---|"), Some(3));
    assert_eq!(separator_columns("--- | ---"), Some(2));
    assert_eq!(separator_columns("---"), None);
    assert_eq!(separator_columns("|| --"), None);
    assert_eq!(row_cells("| a | b |"), 2);
    assert_eq!(row_cells("a | b |  "), 3);
    assert_eq!(row_cells(r"| a \| b |"), 1);
    assert_eq!(lenient_table("a | b\n- | - | -\n"), Some(2));
    assert_eq!(lenient_table("a | b | c\n- | -\n"), None);
    assert!(parse("[a](b \"\") [c](d) [e](f\"\")").1.is_some());
    let (items, difference) = parse("[a](b \"\") [c](d)");
    assert_eq!(difference, None);
    let titles: Vec<Option<String>> = items
        .into_iter()
        .filter_map(|item| match item {
            Item::Link { title, .. } => Some(title),
            _ => None,
        })
        .collect();
    assert_eq!(titles, [Some(String::new()), None]);
}

/// A link to a data URI of text without a media type, which Tika's path fails on.
const UNTYPED_DATA_URI: &str = "[tekst](data:,hej)\n";

#[test]
fn embeds_text_of_data_uris_without_a_media_type() {
    let scratch = crate::formats::testing::Scratch::new("markdown-untyped");
    let path = scratch.write("untyped.md", UNTYPED_DATA_URI.as_bytes());
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    assert!(
        xml.contains("<div class=\"package-entry\">") && xml.contains("hej"),
        "{xml}"
    );
}

#[test]
fn embeds_the_documents_of_data_uris() {
    let markdown = "[a](data:text/plain;base64,SGVq) <b title=\"data:text/plain,Hallo\">b</b>\n";
    let scratch = crate::formats::testing::Scratch::new("markdown-data");
    let path = scratch.write("data.md", markdown.as_bytes());
    let (text, _) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    assert!(text.contains("Hej\n"), "{text:?}");
    assert!(text.contains("Hallo\n"), "{text:?}");
}
