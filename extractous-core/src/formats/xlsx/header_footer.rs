//! A sheet's header or footer as extractous's Tika writes it: POI's `HeaderFooterHelper` splits
//! it into its left, centre and right sections at the `&L`, `&C` and `&R` codes,
//! `ExcelExtractor._extractHeaderFooter` joins them with tabs and ends them with a line break,
//! and our substitution of `extractHeaderFooter` strips the other codes with HSSF's
//! `HeaderFooter.stripFields`, then the white space at either end.

use std::sync::LazyLock;

use regex::Regex;

use crate::formats::xlsx::format::DOT;
use crate::unicode::is_whitespace;

/// The fixed codes `HeaderFooter.MarkupTag` lists, in its order.
const FIELDS: [&str; 15] = [
    "&A", "&D", "&F", "&Z", "&P", "&T", "&N", "&G", "&B", "&I", "&S", "&Y", "&X", "&U", "&E",
];

/// The codes with arguments `stripFields` removes, in its order: font sizes, font names and
/// colours.
static ARGUMENT_FIELDS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        "&[0-9]+".to_owned(),
        format!("&\"{DOT}*?,{DOT}*?\""),
        "&K[0-9A-F]{6}".to_owned(),
        "&K[0-9]{2}[+][0-9]{3}".to_owned(),
    ]
    .map(|pattern| Regex::new(&pattern).expect("a valid pattern"))
});

const LEFT: &str = "&L";
const CENTER: &str = "&C";
const RIGHT: &str = "&R";

/// `HeaderFooterHelper.getParts`: the left, centre and right sections, each from its code to
/// the next code after it, found from the end.
fn sections(text: &str) -> [&str; 3] {
    let mut sections = [""; 3];
    let mut text = text;
    loop {
        let left = text.find(LEFT);
        let center = text.find(CENTER);
        let right = text.find(RIGHT);
        let position = |found: Option<usize>| found.map_or(-1, |at| at as isize);
        let (l, c, r) = (position(left), position(center), position(right));
        if l < 0 && c < 0 && r < 0 {
            return sections;
        }
        let (index, at) = if r > c && r > l {
            (2, r)
        } else if c > r && c > l {
            (1, c)
        } else {
            (0, l)
        };
        let at = at as usize;
        sections[index] = &text[at + 2..];
        text = &text[..at];
    }
}

/// What the substituted `extractHeaderFooter` writes in a paragraph for a header or footer:
/// nothing where only codes and white space remain.
pub(crate) fn extract(text: &str) -> String {
    let joined = join_sections(text);
    strip_fields(&joined).trim_matches(is_whitespace).to_owned()
}

/// `HeaderFooter.stripFields`: the fixed codes removed until none is left, then those with
/// arguments, then `&&` made `&`.
fn strip_fields(text: &str) -> String {
    let mut text = text.to_owned();
    for field in FIELDS {
        while let Some(at) = text.find(field) {
            text.replace_range(at..at + field.len(), "");
        }
    }
    for pattern in ARGUMENT_FIELDS.iter() {
        text = pattern.replace_all(&text, "").into_owned();
    }
    text.replace("&&", "&")
}

/// `_extractHeaderFooter(new HeaderFooterFromString(text))`: the sections joined by tabs where
/// text precedes them, and a line break after any text.
fn join_sections(text: &str) -> String {
    let [left, center, right] = sections(text);
    let mut extracted = String::from(left);
    for section in [center, right] {
        if !extracted.is_empty() {
            extracted.push('\t');
        }
        extracted.push_str(section);
    }
    if !extracted.is_empty() {
        extracted.push('\n');
    }
    extracted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_sections_as_poi_does() {
        assert_eq!(join_sections("&CPage &P"), "Page &P\t\n");
        assert_eq!(
            join_sections("&LLeft&CCentre&RRight"),
            "Left\tCentre\tRight\n"
        );
        assert_eq!(join_sections("&RRight&LLeft"), "Left\t\tRight\n");
        assert_eq!(join_sections("Plain"), "");
        assert_eq!(join_sections(""), "");
    }

    #[test]
    fn strips_fields_as_extractous_does() {
        assert_eq!(extract("&C&12&A"), "");
        assert_eq!(extract("&C&12Page &P"), "Page");
        assert_eq!(extract("&L&\"Arial,Bold\"Q&&A &KFF0000red"), "Q& red");
        assert_eq!(extract("&C&&AA"), "");
    }
}
