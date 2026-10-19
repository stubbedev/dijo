//! A sheet's header or footer as dijo's Tika writes it: POI's `HeaderFooterHelper` splits
//! it into its left, centre and right sections at the `&L`, `&C` and `&R` codes,
//! `ExcelExtractor._extractHeaderFooter` joins them with tabs and ends them with a line break,
//! and our substitution of `extractHeaderFooter` strips the other codes as HSSF's
//! `HeaderFooter.stripFields` does, then the white space at either end. Unlike POI, an escaped
//! `&&` is never read as the start of a code (#94).

use std::sync::LazyLock;

use regex::Regex;

use crate::formats::data_format::DOT;
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

/// Where `code` first stands in `text` as a code, not after the `&` of an escaped `&&`.
fn find_code(text: &str, code: &str) -> Option<usize> {
    let mut at = 0;
    while let Some(found) = text[at..].find('&') {
        let found = at + found;
        if text[found..].starts_with("&&") {
            at = found + 2;
        } else if text[found..].starts_with(code) {
            return Some(found);
        } else {
            at = found + 1;
        }
    }
    None
}

/// `HeaderFooterHelper.getParts`: the left, centre and right sections, each from its code to
/// the next code after it, found from the end. An escaped `&&` followed by `L`, `C` or `R` is
/// text, where POI takes it for a code.
pub fn sections(text: &str) -> [&str; 3] {
    let mut sections = [""; 3];
    let mut text = text;
    loop {
        let left = find_code(text, LEFT);
        let center = find_code(text, CENTER);
        let right = find_code(text, RIGHT);
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
pub fn extract(text: &str) -> String {
    let joined = join_sections(text);
    strip_fields(&joined).trim_matches(is_whitespace).to_owned()
}

/// The codes `HeaderFooter.stripFields` removes, read in one pass from the start: an escaped
/// `&&` is `&`, and the fixed codes and those with arguments are removed. `stripFields` removes
/// the codes before it unescapes, so the `&A` of `Q&&A` is taken for a code (#94).
fn strip_fields(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        stripped.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with("&&") {
            stripped.push('&');
            rest = &rest[2..];
            continue;
        }
        let code = FIELDS
            .iter()
            .find(|field| rest.starts_with(**field))
            .map(|field| field.len())
            .or_else(|| {
                ARGUMENT_FIELDS
                    .iter()
                    .filter_map(|pattern| pattern.find(rest))
                    .find(|found| found.start() == 0)
                    .map(|found| found.end())
            });
        match code {
            Some(len) => rest = &rest[len..],
            None => {
                stripped.push('&');
                rest = &rest[1..];
            }
        }
    }
    stripped.push_str(rest);
    stripped
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
    fn strips_fields_and_unescapes_ampersands() {
        assert_eq!(extract("&C&12&A"), "");
        assert_eq!(extract("&C&12Page &P"), "Page");
        assert_eq!(extract("&L&\"Arial,Bold\"Q&&A &KFF0000red"), "Q&A red");
        assert_eq!(extract("&C&&AA"), "&AA");
        assert_eq!(extract("&CQ&&&A"), "Q&");
        assert_eq!(extract("&CQ&&Rules"), "Q&Rules");
    }
}
