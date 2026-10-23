//! The paragraph styles of a document, as Tika's `XWPFStylesShim` reads their names and
//! `WordExtractor.buildParagraphTagAndStyle` turns a name into an element and a class.

use std::collections::HashMap;

use super::W_NAMESPACE;
use crate::formats::xml::{attribute, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java;

/// The names of the styles, by id.
#[derive(Debug, Default)]
pub(super) struct Styles {
    names: HashMap<String, String>,
    current: Option<String>,
}

impl Styles {
    pub(super) fn name(&self, id: &str) -> Option<&str> {
        self.names.get(id).map(String::as_str)
    }
}

impl SaxHandler for Styles {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if namespace != W_NAMESPACE {
            return Ok(());
        }
        if local_name == "style" {
            self.current = attribute(attributes, W_NAMESPACE, "styleId").map(str::to_owned);
        } else if local_name == "name" {
            if let (Some(id), Some(value)) =
                (&self.current, attribute(attributes, W_NAMESPACE, "val"))
            {
                self.names.insert(id.clone(), value.to_owned());
            }
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace == W_NAMESPACE && local_name == "style" {
            self.current = None;
        }
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// The element a paragraph is written as, and its class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TagAndStyle {
    pub(super) tag: &'static str,
    pub(super) class: Option<String>,
}

impl TagAndStyle {
    fn plain(tag: &'static str, class: Option<&str>) -> Self {
        Self {
            tag,
            class: class.map(str::to_owned),
        }
    }
}

const HEADINGS: [&str; 7] = ["h0", "h1", "h2", "h3", "h4", "h5", "h6"];

/// `WordExtractor.buildParagraphTagAndStyle(styleName, false)`.
pub(super) fn paragraph_tag_and_style(style: &str) -> TagAndStyle {
    if crate::java::utf16_length(style) < 2 {
        return TagAndStyle::plain("p", None);
    }
    match style {
        "Default" | "Normal" => return TagAndStyle::plain("p", None),
        "heading" | "Heading" => return TagAndStyle::plain("h1", None),
        "Title" => return TagAndStyle::plain("h1", Some("title")),
        "Subtitle" => return TagAndStyle::plain("h2", Some("subtitle")),
        "HTML Preformatted" => return TagAndStyle::plain("pre", None),
        _ => {}
    }
    if style.starts_with("heading") || style.starts_with("Heading") {
        let level = style
            .chars()
            .next_back()
            .filter(|last| last.len_utf16() == 1)
            .and_then(|last| java::parse_int(last.encode_utf8(&mut [0; 4])))
            .and_then(|level| usize::try_from(level).ok())
            .unwrap_or(1);
        return TagAndStyle {
            tag: HEADINGS[level.min(6)],
            class: None,
        };
    }
    let class = style.replace(' ', "_");
    let mut characters = class.chars();
    let first = characters.next().map_or(String::new(), |first| {
        if first.len_utf16() == 1 {
            first.to_lowercase().collect()
        } else {
            first.to_string()
        }
    });
    TagAndStyle {
        tag: "p",
        class: Some(format!("{first}{}", characters.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_style_names_into_elements_as_tika_does() {
        assert_eq!(
            paragraph_tag_and_style("heading 3"),
            TagAndStyle::plain("h3", None)
        );
        assert_eq!(
            paragraph_tag_and_style("Heading9"),
            TagAndStyle::plain("h6", None)
        );
        assert_eq!(
            paragraph_tag_and_style("Heading0"),
            TagAndStyle::plain("h0", None)
        );
        assert_eq!(
            paragraph_tag_and_style("Heading X"),
            TagAndStyle::plain("h1", None)
        );
        assert_eq!(
            paragraph_tag_and_style("Title"),
            TagAndStyle::plain("h1", Some("title"))
        );
        assert_eq!(
            paragraph_tag_and_style("List Paragraph"),
            TagAndStyle::plain("p", Some("list_Paragraph"))
        );
        assert_eq!(paragraph_tag_and_style("X"), TagAndStyle::plain("p", None));
        assert_eq!(
            paragraph_tag_and_style("Überschrift"),
            TagAndStyle::plain("p", Some("überschrift"))
        );
    }

    #[test]
    fn names_heading_levels_as_formatting_them_did() {
        for last in [
            '0', '1', '5', '6', '7', '9', '\u{663}', '\u{ff18}', 'X', '-', '+',
        ] {
            let level = java::parse_int(last.encode_utf8(&mut [0; 4])).unwrap_or(1);
            assert_eq!(
                paragraph_tag_and_style(&format!("Heading{last}")).tag,
                format!("h{}", level.min(6))
            );
        }
    }
}
